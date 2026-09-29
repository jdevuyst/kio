//! Implementation of `kio dep <subcommand>`.
//!
//! `kio dep` is a subcommand container for managing the current
//! package's declared dependencies (`<local>.dep.kio` files). Two
//! children today:
//!
//! - `kio dep fetch [--force] [<name>...]` — materialize the package's
//!   dependencies, without a build: fetch/clone every git dependency into
//!   the per-user cache, resolve + lock any not-yet-locked git
//!   dependency, check out the (locked or just-resolved) commit, and
//!   re-root each dependency's modules under the consumer tree. An
//!   existing `<local>.lock.kio` is **honored** — a locked git
//!   dependency uses its locked commit, with no re-resolution — so fetch
//!   is reproducible. A dependency already materialized at the lock's
//!   intent (its re-rooted tree on disk matches byte-for-byte) is a no-op
//!   reported `up to date`, so the redundant re-fetch a build/check would
//!   repeat is skipped; `--force` bypasses the skip and re-materializes
//!   every selected dependency unconditionally. With one or more `<name>`
//!   arguments, only the named dependencies are materialized. `path`
//!   dependencies materialize as usual (they carry no lock).
//! - `kio dep update [--allow-breaking] [<name>...]` — re-pin **and**
//!   re-materialize: for each git dependency, re-resolve its `ref` to the
//!   commit it designates **now** and rewrite the `<local>.lock.kio`
//!   (commit + fresh contract digest), **ignoring** the lock's old commit.
//!   Before rewriting an A → B move it runs the contract-compatibility
//!   honesty gate: the dependency's contract surface at the old commit is
//!   compared against the new commit's, and a breaking change to a
//!   **sealed** dependency contract is a dependency error that leaves the
//!   lock unchanged (unless `--allow-breaking` downgrades it to a warning
//!   and proceeds); a breaking change to an **unsealed** contract is a
//!   warning. It then re-materializes every selected dependency's tree
//!   from its current source; a `path` dependency, having no lock, is
//!   re-materialized from its on-disk modules. With no name, every
//!   dependency is updated; with names, only the named ones. The command
//!   reports each git dependency's `old → new` commit move (or that it was
//!   unchanged).
//!
//! `fetch` and `update` are the only commands that materialize:
//! `kio build` / `kio check` / `kio test` consume the re-rooted trees
//! as ordinary source, so the analysis pipeline stays
//! dependency-agnostic. They run from a package
//! root (the directory holding the `<local>.dep.kio` files); a directory
//! that declares no dependency is a 0-exit no-op with a note.

use std::collections::BTreeSet;
use std::path::Path;

use crate::exit_code::ExitCode;
use crate::package_collection::{self, DeclaredDependency};

const HELP_DEP_TEMPLATE: &str = "\
Usage: kio dep <subcommand>

Manage the current package's declared dependencies (`<local>.dep.kio`).

Subcommands:
  fetch [--force]     Materialize the package's dependencies: fetch git
        [<name>...]    dependencies into the per-user cache, resolve + lock
                      any unlocked git dependency, check out the commit,
                      and re-root each dependency's modules. An existing
                      lockfile is honored (a locked git dependency uses
                      its locked commit; no re-resolution). A dependency
                      already materialized at the lock's intent is a no-op
                      (reported `up to date`); `--force` re-materializes
                      unconditionally. With names, only the named
                      dependencies.
  update [<name>...]  Re-pin git dependencies (re-resolve each `ref` to
                      the commit it points at now, rewrite the
                      `<local>.lock.kio`) and re-materialize every
                      dependency, path included. Reports each `old -> new`
                      commit. Runs the contract-compatibility honesty
                      gate; a breaking change to a sealed dependency
                      contract is an error unless `--allow-breaking` is
                      given. With names, only the named dependencies.
  clean [<name>...]   Remove the materialized dependency trees (the
                      generated `<local>/` modules). The `.dep.kio`
                      declarations and `.lock.kio` pins are left in place.
                      With names, only the named dependencies' trees.

Options:
  --force            (fetch) re-materialize even dependencies already up
                     to date, instead of skipping them.
  --allow-breaking   (update) downgrade a sealed-contract break to a
                     warning and proceed, instead of erroring.
  -h, --help         Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on success / no-op (the
package declares no dependency, or no named dependency matched after a
note); 2 on CLI usage error (missing / unknown subcommand, unknown
flag); 30 on dependency error (a fetch / clone / ref-resolution
failure, a stale lockfile, an unknown named dependency, a blocked
sealed-contract break on `update`); 1 on internal error.

See {base}/specs/cli.md#kio-dep-subcommand for full command behavior.";

const HELP_FETCH_TEMPLATE: &str = "\
Usage: kio dep fetch [--force] [<name>...]

Materialize the current package's dependencies without building.

Materializes each dependency: fetches each git dependency into the
per-user cache, resolves + writes a `<local>.lock.kio` for any
not-yet-locked git dependency, checks out the (locked or just-resolved)
commit, and re-roots each dependency's modules under the consumer tree.
An existing lockfile is honored — a locked git dependency uses its
locked commit, with no re-resolution — so fetch is reproducible.
`path` dependencies materialize as usual.

A dependency whose re-rooted tree on disk already matches the lock's
intent (every module byte-identical, nothing stale) is skipped as a
no-op, reported `` `<name>` (git|path): up to date ``. Pass `--force`
to bypass the skip and re-materialize every selected dependency
unconditionally (an explicit refresh / drift check).

With one or more <name> arguments, only the named dependencies (by
their `<local>.dep.kio` stem) are materialized; an unknown name is a
dependency error. Must be run from a package root.

Options:
  --force       Re-materialize even dependencies already up to date.
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on success / no-op; 2 on
CLI usage error (unknown flag); 30 on dependency error (fetch / clone /
ref-resolution failure, stale lockfile, unknown named dependency); 1 on
internal error.

See {base}/specs/cli.md#kio-dep-fetch---force-name for full command behavior.";

const HELP_UPDATE_TEMPLATE: &str = "\
Usage: kio dep update [--allow-breaking] [<name>...]

Re-pin the current package's git dependencies.

For each git dependency, re-resolves its `ref` to the commit it points
at now and rewrites the `<local>.lock.kio`, ignoring the commit the
lockfile pinned before. Use this to advance a floating ref (a branch or
tag) to its current commit. It then re-materializes every dependency's
tree from its current source; a `path` dependency, which has no lock, is
re-materialized from its on-disk modules.

Before re-pinning an A -> B move, the dependency's contract surface at
the old commit is compared against the new commit's. A breaking change
(a dropped / narrowed export, or an added host requirement) to a
*sealed* dependency contract is a dependency error and the lock is left
unchanged, unless `--allow-breaking` downgrades it to a warning and
proceeds. A breaking change to an *unsealed* contract is a warning, and
the re-pin proceeds. A compatible move re-pins silently.

With no name, every git dependency is re-pinned; with one or more
<name> arguments, only the named dependencies (by their
`<local>.dep.kio` stem). An unknown name is a dependency error. Each
dependency's resulting `old -> new` commit move (or `unchanged`) is
reported. Must be run from a package root.

Options:
  --allow-breaking   Downgrade a sealed-contract break to a warning and
                     proceed with the re-pin, instead of erroring.
  -h, --help         Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on success / no-op; 2 on
CLI usage error (unknown flag); 30 on dependency error (fetch / clone /
ref-resolution failure, unknown named dependency, a blocked
sealed-contract break); 1 on internal error.

See {base}/specs/cli.md#kio-dep-update---allow-breaking-name for full command behavior.";

const HELP_CLEAN_TEMPLATE: &str = "\
Usage: kio dep clean [<name>...]

Remove the current package's materialized dependency trees.

Deletes the re-rooted `<local>/...` module tree each dependency was
materialized into. The `<local>.dep.kio` declarations and any
`<local>.lock.kio` pins are left untouched. The trees are the committed
materialized closure, so removing them dirties the working tree;
re-materialize with `kio dep fetch` (or restore them with `git`).

With one or more <name> arguments, only the named dependencies' trees
are removed; an unknown name is a dependency error. Must be run from a
package root.

Options:
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on success / no-op; 2 on
CLI usage error (unknown flag); 30 on dependency error (unknown named
dependency); 1 on internal error (a tree could not be removed).

See {base}/specs/cli.md#kio-dep-clean-name for full command behavior.";

/// Dispatch for `kio dep <subcommand>`. The container alone is a usage
/// error — same shape as `kio cache` / `kio doc` with no subcommand.
pub fn run(args: &[String]) -> ExitCode {
    if let Some(first) = args.first() {
        match first.as_str() {
            "-h" | "--help" => {
                print_help(HELP_DEP_TEMPLATE);
                return ExitCode::Success;
            }
            "fetch" => return run_fetch(&args[1..]),
            "update" => return run_update(&args[1..]),
            "clean" => return run_clean(&args[1..]),
            other => {
                eprintln!("error: unknown subcommand: kio dep {other}");
                eprintln!();
                print_help_err(HELP_DEP_TEMPLATE);
                return ExitCode::Usage;
            }
        }
    }
    eprintln!("error: `kio dep` requires a subcommand");
    eprintln!();
    print_help_err(HELP_DEP_TEMPLATE);
    ExitCode::Usage
}

/// `kio dep fetch [--force] [<name>...]`.
fn run_fetch(args: &[String]) -> ExitCode {
    let (names, force) = match parse_fetch_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return ExitCode::Success, // --help
        Err(code) => return code,
    };
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return ExitCode::Internal;
        }
    };
    fetch_at(&cwd, &names, force)
}

/// `kio dep update [--allow-breaking] [<name>...]`.
fn run_update(args: &[String]) -> ExitCode {
    let (names, allow_breaking) = match parse_update_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return ExitCode::Success, // --help
        Err(code) => return code,
    };
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return ExitCode::Internal;
        }
    };
    update_at(&cwd, &names, allow_breaking)
}

/// Materialize the package rooted at `root`. When `names` is empty every
/// declared dependency is materialized; otherwise only the named ones.
/// When `force` is false, a dependency already materialized at the lock's
/// intent is a no-op reported as `up to date`; `force` re-materializes
/// every selected dependency unconditionally. Pulled out so tests can drive
/// the command against a temp package without changing the process `cwd`.
pub fn fetch_at(root: &Path, names: &[String], force: bool) -> ExitCode {
    let declared = match package_collection::read_dependency_files(root) {
        Ok(d) => d,
        Err(diag) => return report_dep_error(&diag),
    };
    if declared.is_empty() {
        println!("no dependencies declared");
        return ExitCode::Success;
    }

    let selected = match select(&declared, names) {
        Ok(set) => set,
        Err(code) => return code,
    };
    let filter = (!names.is_empty()).then(|| selected.clone());

    let outcomes =
        match package_collection::materialize_dependencies_filtered(root, filter.as_ref(), force) {
            Ok(outcomes) => outcomes,
            Err(diag) => return report_dep_error(&diag),
        };

    // Report each in-scope dependency, in declared order, tagged with its
    // source kind and whether it was (re)fetched or already current.
    for d in &declared {
        if names.is_empty() || selected.contains(&d.dependency.name) {
            let name = &d.dependency.name;
            match outcomes.get(name) {
                Some(package_collection::MaterializeOutcome::UpToDate) => {
                    println!("`{name}` ({}): up to date", source_kind(d));
                }
                _ => println!("fetched `{name}` ({})", source_kind(d)),
            }
        }
    }
    ExitCode::Success
}

/// Parse `kio dep fetch`'s arguments: the positional `<name>...` plus the
/// `--force` flag (which re-materializes every selected dependency even
/// when it is already current). Handles `-h` / `--help` (printing the help
/// and returning `Ok(None)` for the caller to exit 0) and rejects any other
/// unknown flag. Returns `Ok(Some((names, force)))` on success.
fn parse_fetch_args(args: &[String]) -> Result<Option<(Vec<String>, bool)>, ExitCode> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help(HELP_FETCH_TEMPLATE);
        return Ok(None);
    }
    let mut names = Vec::new();
    let mut force = false;
    for arg in args {
        match arg.as_str() {
            "--force" => force = true,
            _ if arg.starts_with('-') => {
                eprintln!("error: unknown flag: {arg}");
                eprintln!();
                print_help_err(HELP_FETCH_TEMPLATE);
                return Err(ExitCode::Usage);
            }
            _ => names.push(arg.clone()),
        }
    }
    Ok(Some((names, force)))
}

/// Re-pin the git dependencies of the package rooted at `root`. When
/// `names` is empty every git dependency is re-pinned; otherwise only the
/// named ones. `allow_breaking` downgrades a sealed-contract break from a
/// blocking error to a warning (the `--allow-breaking` flag). Pulled out
/// so tests can drive the command against a temp package without changing
/// the process `cwd`.
pub fn update_at(root: &Path, names: &[String], allow_breaking: bool) -> ExitCode {
    let declared = match package_collection::read_dependency_files(root) {
        Ok(d) => d,
        Err(diag) => return report_dep_error(&diag),
    };
    if declared.is_empty() {
        println!("no dependencies declared");
        return ExitCode::Success;
    }

    let selected = match select(&declared, names) {
        Ok(set) => set,
        Err(code) => return code,
    };

    let filter = (!names.is_empty()).then(|| selected.clone());

    // Re-pin atomically. Each git dependency's `ref` is re-resolved and run
    // through the honesty gate, and its new lock is *staged* — computed but
    // not written. The whole batch is committed only after every selected
    // dependency has passed its gate, so a blocked sealed break (which
    // returns early here) leaves no lock advanced and therefore no committed
    // tree stale against an advanced lock. Without this, a
    // lock written the moment its own gate passed could outrun a later
    // dependency's block, leaving the consumer in a partial, inconsistent
    // state (exit 30 with some locks moved but their trees un-rematerialized).
    let mut updates: Vec<(&str, crate::git_dep::LockUpdate)> = Vec::new();
    for d in &declared {
        if !names.is_empty() && !selected.contains(&d.dependency.name) {
            continue;
        }
        if let crate::ast::SourceOrigin::Git(source) = &d.dependency.source.origin {
            let update = match crate::git_dep::update_git_lock(
                root,
                &d.dep_file_path,
                &d.dependency.name,
                source,
                allow_breaking,
            ) {
                Ok(u) => u,
                // A blocked break (or any re-pin error) aborts before any
                // lock is written: every dependency stays on its old commit
                // with its committed tree intact.
                Err(diag) => return report_dep_error(&diag),
            };
            updates.push((&d.dependency.name, update));
        }
    }

    // Every gate passed: commit the staged locks, then materialize. A write
    // failure here is rare (an I/O error), but it too precedes the
    // materialization, so the trees still reflect a consistent set of locks.
    for (_name, update) in &updates {
        if let Err(diag) = update.staged_lock.commit() {
            return report_dep_error(&diag);
        }
    }

    // Materialize every selected dependency's tree so it reflects the
    // current source — for `git`, the commit just re-pinned above; for
    // `path`, its current on-disk modules. `update` re-pins to "now", so it
    // forces a re-materialization (bypassing the up-to-date skip): the
    // re-rooted trees are committed (a consumer ships its dependency's
    // materialized closure), and `update` rewrites them in place to the
    // current source.
    if let Err(diag) =
        package_collection::materialize_dependencies_filtered(root, filter.as_ref(), true)
    {
        return report_dep_error(&diag);
    }

    // Report only after the locks and trees are both committed, so the
    // printed outcome matches disk.
    for (name, update) in &updates {
        report_update(name, update);
    }
    for d in &declared {
        if !names.is_empty() && !selected.contains(&d.dependency.name) {
            continue;
        }
        if matches!(
            d.dependency.source.origin,
            crate::ast::SourceOrigin::Path { .. }
        ) {
            println!("`{}` (path): materialized", d.dependency.name);
        }
    }
    ExitCode::Success
}

/// `kio dep clean [<name>...]`.
fn run_clean(args: &[String]) -> ExitCode {
    let names = match parse_names(args, HELP_CLEAN_TEMPLATE) {
        Ok(Some(names)) => names,
        Ok(None) => return ExitCode::Success, // --help
        Err(code) => return code,
    };
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return ExitCode::Internal;
        }
    };
    clean_at(&cwd, &names)
}

/// Remove the materialized `<local>/` module tree of each selected
/// dependency from the working directory. The `<local>.dep.kio` declaration
/// and any `<local>.lock.kio` pin are left in place. The tree is the
/// committed materialized closure, so removing it dirties the working tree;
/// `kio dep fetch` (or `git restore`) regenerates it. Pulled out so tests
/// can drive it against a temp package without changing the process `cwd`.
pub fn clean_at(root: &Path, names: &[String]) -> ExitCode {
    let declared = match package_collection::read_dependency_files(root) {
        Ok(d) => d,
        Err(diag) => return report_dep_error(&diag),
    };
    if declared.is_empty() {
        println!("no dependencies declared");
        return ExitCode::Success;
    }

    let selected = match select(&declared, names) {
        Ok(set) => set,
        Err(code) => return code,
    };

    for d in &declared {
        if !names.is_empty() && !selected.contains(&d.dependency.name) {
            continue;
        }
        let dir = root.join(&d.dependency.name);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => println!("cleaned `{}`", d.dependency.name),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!("`{}`: nothing to clean", d.dependency.name);
            }
            Err(e) => {
                eprintln!(
                    "error: cannot remove materialized directory `{}`: {e}",
                    dir.display()
                );
                return ExitCode::Internal;
            }
        }
    }
    ExitCode::Success
}

/// Print the `old -> new` (or `unchanged`) line for one re-pinned git
/// dependency, abbreviating commits to the first 12 hex chars (enough to
/// identify a commit, the convention `git log --oneline` uses). When the
/// re-pin adopted a breaking contract change — an unsealed break, or a
/// sealed break overridden by `--allow-breaking` — a warning naming the
/// breaking changes is printed to stderr first.
fn report_update(name: &str, update: &crate::git_dep::LockUpdate) {
    if let Some(gate) = &update.gate
        && gate.breaking
    {
        let qualifier = if gate.sealed {
            "sealed contract (adopted with --allow-breaking)"
        } else {
            "unsealed contract"
        };
        eprintln!("warning: `{name}` (git): re-pin adopts a breaking change to its {qualifier}:");
        for reason in &gate.reasons {
            eprintln!("  - {reason}");
        }
    }
    if update.is_unchanged() {
        println!("`{name}` (git): unchanged ({})", short(&update.new_commit));
    } else {
        match &update.old_commit {
            Some(old) => println!(
                "`{name}` (git): {} -> {}",
                short(old),
                short(&update.new_commit)
            ),
            None => println!(
                "`{name}` (git): pinned {} (first lock)",
                short(&update.new_commit)
            ),
        }
    }
}

/// The 12-char abbreviation of a 40-char commit SHA for display. A
/// non-SHA-shaped string (shouldn't happen — the resolver validates) is
/// shown verbatim so the output never silently truncates something
/// unexpected.
fn short(commit: &str) -> &str {
    if commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        &commit[..12]
    } else {
        commit
    }
}

/// The source-kind tag (`git` / `path`) for a declared dependency, for
/// the `fetched` report line.
fn source_kind(d: &DeclaredDependency) -> &'static str {
    match &d.dependency.source.origin {
        crate::ast::SourceOrigin::Path { .. } => "path",
        crate::ast::SourceOrigin::Git(_) => "git",
    }
}

/// Resolve the user-named dependency set against the declared ones. An
/// empty `names` selects everything (the caller distinguishes "all" from
/// "named" by `names.is_empty()`), so the returned set is the full set in
/// that case. A named dependency that matches no declaration is a
/// dependency error naming the unknown name and the available ones.
fn select(declared: &[DeclaredDependency], names: &[String]) -> Result<BTreeSet<String>, ExitCode> {
    let available: BTreeSet<String> = declared.iter().map(|d| d.dependency.name.clone()).collect();
    if names.is_empty() {
        return Ok(available);
    }
    let mut selected = BTreeSet::new();
    for name in names {
        if !available.contains(name) {
            eprintln!(
                "error: no dependency named `{name}` in this package; declared: {}",
                available
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Err(ExitCode::Dep);
        }
        selected.insert(name.clone());
    }
    Ok(selected)
}

/// Parse the positional `<name>...` arguments shared by `fetch` /
/// `update`, handling `-h` / `--help` (printing `help` and returning
/// `Ok(None)` for the caller to exit 0) and rejecting unknown flags.
/// Returns `Ok(Some(names))` with the positional dependency names.
fn parse_names(args: &[String], help: &str) -> Result<Option<Vec<String>>, ExitCode> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help(help);
        return Ok(None);
    }
    let mut names = Vec::new();
    for arg in args {
        if arg.starts_with('-') {
            eprintln!("error: unknown flag: {arg}");
            eprintln!();
            print_help_err(help);
            return Err(ExitCode::Usage);
        }
        names.push(arg.clone());
    }
    Ok(Some(names))
}

/// Parse `kio dep update`'s arguments: the positional `<name>...` plus the
/// `--allow-breaking` flag (which downgrades a sealed-contract break from a
/// blocking error to a warning). Handles `-h` / `--help` (returning
/// `Ok(None)`) and rejects any other unknown flag. Returns `Ok(Some((names,
/// allow_breaking)))` on success.
fn parse_update_args(args: &[String]) -> Result<Option<(Vec<String>, bool)>, ExitCode> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help(HELP_UPDATE_TEMPLATE);
        return Ok(None);
    }
    let mut names = Vec::new();
    let mut allow_breaking = false;
    for arg in args {
        match arg.as_str() {
            "--allow-breaking" => allow_breaking = true,
            _ if arg.starts_with('-') => {
                eprintln!("error: unknown flag: {arg}");
                eprintln!();
                print_help_err(HELP_UPDATE_TEMPLATE);
                return Err(ExitCode::Usage);
            }
            _ => names.push(arg.clone()),
        }
    }
    Ok(Some((names, allow_breaking)))
}

/// Render a dependency-resolution diagnostic to stderr and return its
/// exit code, the same path the implicit materialization step uses.
fn report_dep_error(diag: &crate::pass::resolve::LocatedError) -> ExitCode {
    let source = std::fs::read_to_string(&diag.file_path).unwrap_or_default();
    crate::cmd::check::eprint_error(&diag.file_path, &source, &diag.error);
    diag.error.exit_code()
}

fn print_help(template: &str) {
    println!("{}", template.replace("{base}", crate::KIO_DOCS_BASE_URL));
}

fn print_help_err(template: &str) {
    eprintln!("{}", template.replace("{base}", crate::KIO_DOCS_BASE_URL));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn dep_with_no_subcommand_exits_usage() {
        assert_eq!(run(&[]), ExitCode::Usage);
    }

    #[test]
    fn dep_with_unknown_subcommand_exits_usage() {
        assert_eq!(run(&argv(&["bogus"])), ExitCode::Usage);
    }

    #[test]
    fn dep_help_succeeds() {
        assert_eq!(run(&argv(&["-h"])), ExitCode::Success);
        assert_eq!(run(&argv(&["--help"])), ExitCode::Success);
    }

    #[test]
    fn fetch_and_update_help_succeed() {
        assert_eq!(run(&argv(&["fetch", "--help"])), ExitCode::Success);
        assert_eq!(run(&argv(&["fetch", "-h"])), ExitCode::Success);
        assert_eq!(run(&argv(&["update", "--help"])), ExitCode::Success);
        assert_eq!(run(&argv(&["update", "-h"])), ExitCode::Success);
    }

    /// An unknown flag on `fetch` / `update` is a usage error, caught
    /// before any filesystem read (so it needs no package on disk).
    #[test]
    fn unknown_flag_exits_usage() {
        assert_eq!(run_fetch(&argv(&["--bogus"])), ExitCode::Usage);
        assert_eq!(run_update(&argv(&["--bogus"])), ExitCode::Usage);
    }

    /// A package with no `*.dep.kio` files: fetch / update both exit 0
    /// with the no-dependencies note, touching nothing.
    #[test]
    fn no_dependencies_is_noop_success() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("app.pkg.kio"), "package app;\n").expect("write pkg");
        assert_eq!(fetch_at(dir.path(), &[], false), ExitCode::Success);
        assert_eq!(update_at(dir.path(), &[], false), ExitCode::Success);
    }

    /// A named dependency that matches no declaration is a dependency
    /// error (exit 30) for both fetch and update.
    #[test]
    fn unknown_named_dependency_is_dep_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("app.pkg.kio"), "package app;\n").expect("write pkg");
        // Declare a local `path` dependency so the dep set is non-empty.
        std::fs::create_dir_all(dir.path().join("lib")).expect("mk lib");
        std::fs::write(dir.path().join("lib/lib.pkg.kio"), "package lib;\n").expect("write lib");
        std::fs::write(
            dir.path().join("lib.dep.kio"),
            "dependency lib;\n\nsource {\n  path \"lib/lib.pkg.kio\";\n}\n",
        )
        .expect("write dep");
        assert_eq!(
            fetch_at(dir.path(), &["nope".to_owned()], false),
            ExitCode::Dep
        );
        assert_eq!(
            update_at(dir.path(), &["nope".to_owned()], false),
            ExitCode::Dep
        );
        assert_eq!(clean_at(dir.path(), &["nope".to_owned()]), ExitCode::Dep);
    }

    /// `update` materializes a `path` dependency (re-rooting its modules
    /// under the consumer tree) and, having no lock, writes no lockfile.
    #[test]
    fn update_path_dependency_materializes() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("app.pkg.kio"), "package app;\n").expect("write pkg");
        std::fs::create_dir_all(dir.path().join("src/greet")).expect("mk dep dir");
        std::fs::write(
            dir.path().join("src/greet/greet.pkg.kio"),
            "package greet;\n",
        )
        .expect("write dep pkg");
        std::fs::write(dir.path().join("src/greet/hello.kio"), "module hello;\n")
            .expect("write dep mod");
        std::fs::write(
            dir.path().join("greet.dep.kio"),
            "dependency greet;\n\nsource {\n  path \"src/greet/greet.pkg.kio\";\n}\n",
        )
        .expect("write dep");
        assert_eq!(update_at(dir.path(), &[], false), ExitCode::Success);
        // The path dependency's module is re-rooted under its local name.
        assert!(dir.path().join("greet/hello.kio").exists());
        // A path dependency carries no lockfile.
        assert!(!dir.path().join("greet.lock.kio").exists());
    }

    /// `clean` removes a materialized `path` dependency's re-rooted tree
    /// while leaving the declaration in place; re-running `fetch` restores
    /// it, and a second `clean` is a success no-op.
    #[test]
    fn clean_removes_materialized_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("app.pkg.kio"), "package app;\n").expect("write pkg");
        std::fs::create_dir_all(dir.path().join("src/greet")).expect("mk dep dir");
        std::fs::write(
            dir.path().join("src/greet/greet.pkg.kio"),
            "package greet;\n",
        )
        .expect("write dep pkg");
        std::fs::write(dir.path().join("src/greet/hello.kio"), "module hello;\n")
            .expect("write dep mod");
        std::fs::write(
            dir.path().join("greet.dep.kio"),
            "dependency greet;\n\nsource {\n  path \"src/greet/greet.pkg.kio\";\n}\n",
        )
        .expect("write dep");

        assert_eq!(fetch_at(dir.path(), &[], false), ExitCode::Success);
        assert!(dir.path().join("greet/hello.kio").exists());

        // clean removes the materialized tree but leaves the declaration.
        assert_eq!(clean_at(dir.path(), &[]), ExitCode::Success);
        assert!(!dir.path().join("greet").exists());
        assert!(dir.path().join("greet.dep.kio").exists());

        // A second clean has nothing to remove and still succeeds.
        assert_eq!(clean_at(dir.path(), &[]), ExitCode::Success);

        // fetch re-materializes the removed tree.
        assert_eq!(fetch_at(dir.path(), &[], false), ExitCode::Success);
        assert!(dir.path().join("greet/hello.kio").exists());
    }

    /// `rehost` rebinds a re-rooted module's host items to a provider with
    /// a **local** rewrite: each `host type` becomes a transparent alias to
    /// the provider's type and each `host fn` a forwarding wrapper. Because
    /// the rewritten items stay exported, a module that imports them across
    /// modules keeps its import unchanged — no importer redirect.
    #[test]
    fn fetch_rehost_rebinds_locally() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("app.pkg.kio"), "package app;\n").expect("write pkg");
        std::fs::create_dir_all(dir.path().join("src/lib")).expect("mk dep");
        std::fs::write(
            dir.path().join("src/lib/lib.pkg.kio"),
            "package lib;\n\nbridge {\n  greet;\n  types;\n}\n",
        )
        .expect("write dep pkg");
        std::fs::write(
            dir.path().join("src/lib/types.kio"),
            "module types;\n\nhost type Str role(str);\n",
        )
        .expect("write types");
        std::fs::write(
            dir.path().join("src/lib/greet.kio"),
            "module greet;\n\nimport types(Str);\n\nhost fn shout(p0: Str) -> .;\n\n\
             pub fn greet(s: Str) -> . { shout(s) }\n",
        )
        .expect("write greet");
        std::fs::write(
            dir.path().join("lib.dep.kio"),
            "dependency lib;\n\nsource {\n  path \"src/lib/lib.pkg.kio\";\n}\n\n\
             rehost lib/greet to provfn;\nrehost lib/types to provtypes;\n",
        )
        .expect("write dep");

        assert_eq!(fetch_at(dir.path(), &[], false), ExitCode::Success);

        // The host type is rebound as a transparent alias to the provider.
        let types = std::fs::read_to_string(dir.path().join("lib/types.kio")).expect("read types");
        assert!(
            types.contains("import provtypes as _rehost_hahcgphghehjhagfhd;"),
            "types: {types}"
        );
        assert!(
            types.contains("pub type Str = _rehost_hahcgphghehjhagfhd.Str;"),
            "types: {types}"
        );

        let greet = std::fs::read_to_string(dir.path().join("lib/greet.kio")).expect("read greet");
        // The host fn is rebound as a forwarding wrapper to its provider.
        assert!(
            greet.contains("import provfn as _rehost_hahcgphggggo;"),
            "greet: {greet}"
        );
        assert!(
            greet.contains("pub fn shout(p0: Str) -> . { _rehost_hahcgphggggo.shout(p0) }"),
            "greet: {greet}"
        );
        // The cross-module `import types(Str)` stays put — the alias is
        // re-exported from `lib/types`, so no importer redirect is needed.
        assert!(greet.contains("import lib/types(Str);"), "greet: {greet}");
    }

    #[cfg(feature = "surface")]
    #[test]
    fn fetch_rehost_avoids_recursive_member_provider_alias() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("app.pkg.kio"),
            "package app;\n\nbridge {\n  provider;\n}\n",
        )
        .expect("write pkg");
        std::fs::write(
            dir.path().join("provider.kio"),
            "module provider;\n\nhost fn loop[S][R](step: S -> S | R, state: S) -> R;\n",
        )
        .expect("write provider");
        std::fs::create_dir_all(dir.path().join("src/lib")).expect("mk dep");
        std::fs::write(
            dir.path().join("src/lib/lib.pkg.kio"),
            "package lib;\n\nbridge {\n  api;\n}\n",
        )
        .expect("write dep pkg");
        std::fs::write(
            dir.path().join("src/lib/api.kio"),
            "module api;\n\nhost fn loop[S][R](step: S -> S | R, state: S) -> R;\n\n\
             rec(loop) fn _rehost_hahcgphggjgegfhc(_value: .) -> . { () }\n",
        )
        .expect("write api");
        std::fs::write(
            dir.path().join("lib.dep.kio"),
            "dependency lib;\n\nsource {\n  path \"src/lib/lib.pkg.kio\";\n}\n\n\
             rehost lib/api to provider;\n",
        )
        .expect("write dep");

        assert_eq!(fetch_at(dir.path(), &[], false), ExitCode::Success);
        let api = std::fs::read_to_string(dir.path().join("lib/api.kio")).expect("read api");
        assert!(
            api.contains("import provider as _rehost_hahcgphggjgegfhc_n2;"),
            "api: {api}"
        );
        assert!(
            api.contains("_rehost_hahcgphggjgegfhc_n2.loop("),
            "api: {api}"
        );
        crate::cmd::check::analyze_workspace_at(dir.path(), false)
            .expect("rehosted package with fresh provider alias analyzes");
    }

    /// `retype` drops a re-rooted module's matched `newtype`s, imports the
    /// same names from the consumer's counterpart, and redirects *other*
    /// modules' imports of those newtypes — the `newtype` analogue of
    /// `rehost`. A congruent counterpart must exist on the consumer side.
    #[test]
    fn fetch_retype_remaps_and_redirects() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("app.pkg.kio"),
            "package app;\n\nbridge {\n  local;\n}\n",
        )
        .expect("write pkg");
        // The consumer holds the congruent counterpart newtype.
        std::fs::write(
            dir.path().join("local.kio"),
            "module local;\n\npub newtype Tag : . { pub constructor mk; pub projector un; };\n",
        )
        .expect("write local");
        std::fs::create_dir_all(dir.path().join("src/lib")).expect("mk dep");
        std::fs::write(
            dir.path().join("src/lib/lib.pkg.kio"),
            "package lib;\n\nbridge {\n  store;\n  relay;\n}\n",
        )
        .expect("write dep pkg");
        std::fs::write(
            dir.path().join("src/lib/store.kio"),
            "module store;\n\npub newtype Tag : . { pub constructor mk; pub projector un; };\n\n\
             pub fn make_tag() -> Tag { Tag.mk(()) }\n",
        )
        .expect("write store");
        std::fs::write(
            dir.path().join("src/lib/relay.kio"),
            "module relay;\n\nimport store(Tag, make_tag);\n\n\
             pub fn relay_tag() -> Tag { make_tag() }\n",
        )
        .expect("write relay");
        std::fs::write(
            dir.path().join("lib.dep.kio"),
            "dependency lib;\n\nsource {\n  path \"src/lib/lib.pkg.kio\";\n}\n\n\
             retype lib/store to local;\n",
        )
        .expect("write dep");

        assert_eq!(fetch_at(dir.path(), &[], false), ExitCode::Success);
        let store = std::fs::read_to_string(dir.path().join("lib/store.kio")).expect("read store");
        // The original-slot alias preserves both the public type and its members.
        assert!(
            store.contains("import local as _rehost_gmgpgdgbgm;"),
            "store: {store}"
        );
        assert!(!store.contains("import local(Tag);"), "store: {store}");
        assert!(
            store.contains("pub type Tag = _rehost_gmgpgdgbgm.Tag;"),
            "store: {store}"
        );
        assert!(
            !store.contains("newtype Tag"),
            "store still declares Tag: {store}"
        );
        let relay = std::fs::read_to_string(dir.path().join("lib/relay.kio")).expect("read relay");
        // `relay`'s `import lib/store(Tag)` is redirected to the
        // counterpart; `make_tag` (a non-retyped fn) keeps its `from`.
        assert!(relay.contains("import local(Tag);"), "relay: {relay}");
        assert!(
            relay.contains("import lib/store(make_tag);"),
            "relay: {relay}"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn fetch_retype_avoids_function_and_literal_provider_aliases() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("app.pkg.kio"),
            "package app;\n\nbridge {\n  core/store;\n}\n",
        )
        .expect("write pkg");
        std::fs::create_dir_all(dir.path().join("core")).expect("mk core");
        std::fs::write(
            dir.path().join("core/store.kio"),
            "module core/store;\n\npub newtype Tag : . { pub constructor mk; pub projector un; };\n",
        )
        .expect("write counterpart");
        std::fs::create_dir_all(dir.path().join("src/lib")).expect("mk dep");
        std::fs::write(
            dir.path().join("src/lib/lib.pkg.kio"),
            "package lib;\n\nbridge {\n  store;\n}\n",
        )
        .expect("write dep pkg");
        std::fs::write(
            dir.path().join("src/lib/store.kio"),
            "module store;\n\npub newtype Tag : . { pub constructor mk; pub projector un; };\n\n\
             fn _rehost_gdgphcgfcphdhegphcgf() -> . { () }\n\n\
             literal _rehost_gdgphcgfcphdhegphcgf_n2 = 2;\n",
        )
        .expect("write store");
        std::fs::write(
            dir.path().join("lib.dep.kio"),
            "dependency lib;\n\nsource {\n  path \"src/lib/lib.pkg.kio\";\n}\n\n\
             retype lib/store to core/store;\n",
        )
        .expect("write dep");

        assert_eq!(fetch_at(dir.path(), &[], false), ExitCode::Success);
        let store = std::fs::read_to_string(dir.path().join("lib/store.kio")).expect("read store");
        assert!(
            store.contains("import core/store as _rehost_gdgphcgfcphdhegphcgf_n3;"),
            "store: {store}"
        );
        assert!(
            store.contains("pub type Tag = _rehost_gdgphcgfcphdhegphcgf_n3.Tag;"),
            "store: {store}"
        );
        crate::cmd::check::analyze_workspace_at(dir.path(), false)
            .expect("retyped package with fresh provider alias analyzes");
    }

    /// A `retype` whose `to` module has no same-named, congruent
    /// counterpart is a dependency error at materialization, not a
    /// downstream typecheck failure.
    #[test]
    fn fetch_retype_incongruent_counterpart_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("app.pkg.kio"),
            "package app;\n\nbridge {\n  local;\n}\n",
        )
        .expect("write pkg");
        // Counterpart `Tag` has an incongruent payload (`!` vs the dep's `.`).
        std::fs::write(
            dir.path().join("local.kio"),
            "module local;\n\npub newtype Tag : ! { pub constructor mk; pub projector un; };\n",
        )
        .expect("write local");
        std::fs::create_dir_all(dir.path().join("src/lib")).expect("mk dep");
        std::fs::write(
            dir.path().join("src/lib/lib.pkg.kio"),
            "package lib;\n\nbridge {\n  store;\n}\n",
        )
        .expect("write dep pkg");
        std::fs::write(
            dir.path().join("src/lib/store.kio"),
            "module store;\n\npub newtype Tag : . { pub constructor mk; pub projector un; };\n",
        )
        .expect("write store");
        std::fs::write(
            dir.path().join("lib.dep.kio"),
            "dependency lib;\n\nsource {\n  path \"src/lib/lib.pkg.kio\";\n}\n\n\
             retype lib/store to local;\n",
        )
        .expect("write dep");

        assert_ne!(fetch_at(dir.path(), &[], false), ExitCode::Success);
    }

    #[test]
    fn short_abbreviates_sha_and_passes_through_other() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(short(sha), "0123456789ab");
        assert_eq!(short("main"), "main");
    }

    use crate::package_collection::MaterializeOutcome;

    /// Assemble a consumer package at `dir` with a single local `path`
    /// dependency `greetlib` (one module `greet.kio`), the natural shape a
    /// user would write: the dependency package lives in a `vendor/`
    /// subtree, separate from the `greetlib/` directory its modules
    /// materialize into. The dependency is unmaterialized until the first
    /// fetch.
    fn write_path_dependency_consumer(dir: &Path) {
        std::fs::write(dir.join("app.pkg.kio"), "package app;\n").expect("write pkg");
        std::fs::create_dir_all(dir.join("vendor/greetlib")).expect("mk vendor");
        std::fs::write(
            dir.join("vendor/greetlib/greetlib.pkg.kio"),
            "package greetlib;\n",
        )
        .expect("write dep pkg");
        std::fs::write(dir.join("vendor/greetlib/greet.kio"), "module greet;\n")
            .expect("write dep mod");
        std::fs::write(
            dir.join("greetlib.dep.kio"),
            "dependency greetlib;\n\nsource {\n  path \"vendor/greetlib/greetlib.pkg.kio\";\n}\n",
        )
        .expect("write dep");
    }

    fn fetch_once(dir: &Path, force: bool) -> MaterializeOutcome {
        let outcomes = package_collection::materialize_dependencies_filtered(dir, None, force)
            .expect("materialize");
        *outcomes.get("greetlib").expect("greetlib materialized")
    }

    /// The first fetch materializes the dependency (`Fetched`); a second
    /// fetch with nothing changed is the skip-if-already-materialized no-op
    /// (`UpToDate`). This is the redundant-fetch case the skip exists to
    /// elide.
    #[test]
    fn fetch_when_already_materialized_is_up_to_date() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_path_dependency_consumer(dir.path());

        assert_eq!(
            fetch_once(dir.path(), false),
            MaterializeOutcome::Fetched,
            "first fetch materializes"
        );
        // The re-rooted module is on disk under the `greetlib/` prefix.
        assert!(dir.path().join("greetlib/greet.kio").exists());

        assert_eq!(
            fetch_once(dir.path(), false),
            MaterializeOutcome::UpToDate,
            "second fetch with nothing changed is a no-op"
        );
    }

    /// When the materialized tree drifts from the source — here a stale
    /// extra module left behind, and a hand-deleted current module — the
    /// next fetch is no longer a no-op: it re-materializes (`Fetched`),
    /// pruning the stale file and rewriting the missing one. The skip must
    /// not mask a partial / drifted tree.
    #[test]
    fn fetch_when_stale_refetches() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_path_dependency_consumer(dir.path());
        assert_eq!(fetch_once(dir.path(), false), MaterializeOutcome::Fetched);

        // A stale module the source no longer has must trigger a re-fetch
        // (the full path prunes it).
        std::fs::write(
            dir.path().join("greetlib/stale.kio"),
            "module greetlib/stale;\n",
        )
        .expect("write stale");
        assert_eq!(
            fetch_once(dir.path(), false),
            MaterializeOutcome::Fetched,
            "an extra stale module is not up to date"
        );
        assert!(
            !dir.path().join("greetlib/stale.kio").exists(),
            "the stale module was pruned by the re-fetch"
        );
        // Now current again.
        assert_eq!(fetch_once(dir.path(), false), MaterializeOutcome::UpToDate);

        // A hand-deleted current module must also trigger a re-fetch.
        std::fs::remove_file(dir.path().join("greetlib/greet.kio")).expect("rm greet");
        assert_eq!(
            fetch_once(dir.path(), false),
            MaterializeOutcome::Fetched,
            "a missing module is not up to date"
        );
        assert!(dir.path().join("greetlib/greet.kio").exists(), "rewritten");
    }

    /// `--force` re-materializes unconditionally even when the tree is
    /// already current — the drift-check refresh path.
    #[test]
    fn fetch_force_always_refetches() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_path_dependency_consumer(dir.path());
        assert_eq!(fetch_once(dir.path(), false), MaterializeOutcome::Fetched);
        // Up to date without force...
        assert_eq!(fetch_once(dir.path(), false), MaterializeOutcome::UpToDate);
        // ...but force re-materializes regardless.
        assert_eq!(
            fetch_once(dir.path(), true),
            MaterializeOutcome::Fetched,
            "--force bypasses the up-to-date skip"
        );
    }

    /// The CLI entry point reports `up to date` (not `fetched`) on the
    /// second run and exits 0 either way.
    #[test]
    fn fetch_at_reports_up_to_date_on_second_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_path_dependency_consumer(dir.path());
        assert_eq!(fetch_at(dir.path(), &[], false), ExitCode::Success);
        assert_eq!(fetch_at(dir.path(), &[], false), ExitCode::Success);
        assert_eq!(fetch_at(dir.path(), &[], true), ExitCode::Success);
    }
}
