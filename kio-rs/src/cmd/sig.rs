//! Implementation of `kio sig` — the package contract-changelog tool.
//!
//! `kio sig` records a package's interface as a versioned, diffable
//! `*.sig.kio` changelog and classifies whether a change is
//! backward-compatible *before* it ships. The subcommands:
//!
//! - `kio sig` — bare, no subcommand: a **non-mutating** status summary
//!   (the same rendering as `kio sig status`). Writes nothing.
//! - `kio sig stage` — record a compatible delta into the draft;
//!   **error** if it breaks the last sealed contract.
//! - `kio sig stage --force` — record a break vs the sealed contract.
//! - `kio sig commit` — seal the draft + increment `v(N)`; records
//!   nothing; **errors** on any unrecorded delta.
//! - `kio sig uncommit --force` — pop the most-recently-sealed version
//!   back into the open draft (tip-only; refused without `--force`).
//! - `kio sig status` — the CI gate; exit `0` / `80` / `81` / `82`
//!   (see [`status_exit_code`] for the precedence).
//! - `kio sig log` — pretty-print the changelog; `--breaking` /
//!   `--since <version>` filter the displayed versions.
//! - `kio sig compact <version>` — collapse additive pre-`<version>`
//!   history into a synthesized boundary block.
//!
//! `--stdout` on the write-path commands prints the recomputed
//! changelog to stdout instead of writing the `*.sig.kio` file.
//!
//! Package scope: with no package-path argument, `kio sig` fans out over
//! every package discovered in the cwd subtree (each its own
//! `*.sig.kio` + independent generation, per-package report / write,
//! no cross-package rollback); explicit package-path arguments scope a
//! subset — mirroring `kio build`.

use crate::ast::{SigVersion, SignatureFile, Surface};
use crate::cmd::package_fanout::CapturedOutput;
use crate::exit_code::ExitCode;
use crate::package_collection;
use crate::pass::resolve::Package;
use crate::path_display::DisplayPath;
use crate::sig::{self, ContractSnapshot, RecordedSurface};
use std::fs;
use std::path::{Path, PathBuf};

const HELP_TEMPLATE: &str = "\
Usage: kio sig [<package-path>...]
       kio sig stage [--force | --stdout] [<package-path>...]
       kio sig commit [-m <message>] [--stdout] [<package-path>...]
       kio sig uncommit [--force] [--stdout] [<package-path>...]
       kio sig status [<package-path>...]
       kio sig log [--breaking] [--since <version>] [<package-path>...]
       kio sig compact <version> [--stdout] [<package-path>...]

Record and gate a package's versioned contract changelog (`*.sig.kio`).

Bare `kio sig` (no subcommand) prints a non-mutating status summary and
writes nothing. `stage` records a backward-compatible delta into the
current draft version, erroring if the live surface breaks the last
sealed contract; `stage --force` records a break instead. `commit` seals
the draft and increments the version (recording nothing; it errors on
any unrecorded delta). `uncommit --force` pops the most-recently-sealed
version back into the draft (tip-only; refused without `--force` since it
rewrites a sealed contract). `status` is the CI gate. `log` pretty-prints
the changelog (`--breaking` / `--since` filter the displayed versions).
`compact <version>` collapses additive pre-`<version>` history.

With no <package-path>, every package in the current directory's subtree
is processed independently. A <package-path> (a directory or a
`*.pkg.kio` file) scopes a subset.

Options:
  --force       on `kio sig stage`, record a break vs the sealed contract
                into the draft; on `kio sig uncommit`, proceed with
                popping a sealed version back into the draft.
  -m <message>, --message <message>
                (`kio sig commit` only) record a changelog message on the
                sealed version block. Repeat `-m` for multiple lines.
  --breaking    (`kio sig log` only) show only versions with a breaking
                section, and only their breaking entries.
  --since <version>
                (`kio sig log` only) show only versions after <version>.
  --stdout      (write-path commands) print the recomputed changelog to
                stdout instead of writing the `*.sig.kio` file.
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): `kio sig status` reports 0
(clean), 80 (breaks the last sealed contract, unrecorded), 81 (stale-
but-compatible drift), or 82 (a recorded break, not yet sealed). The
other subcommands exit 0 on success, 40 on a recording / build error,
or the matching `1x` category for a package that doesn't typecheck.

See {base}/specs/cli.md#kio-sig-subcommand for full command behavior.";

pub fn run(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "{}",
            HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }

    // The subcommand is the first non-flag positional that isn't a
    // package path. `stage`, `commit`, `status`, `log`, `compact` are
    // reserved keywords; a bare `kio sig` (no subcommand) is a
    // non-mutating status summary.
    let mut subcommand = SigCommand::Status;
    let mut stdout = false;
    let mut force = false;
    let mut message: Option<String> = None;
    let mut log_breaking = false;
    let mut log_since: Option<u32> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut compact_version: Option<u32> = None;
    let mut i = 0;
    let mut parsed_subcommand = false;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            // `--force` is a flag, parsed independently of the
            // subcommand; it is honored only on `kio sig stage` (Record),
            // and rejected with a Usage error on any other subcommand
            // (see the post-loop guard) so `status --force` never
            // silently runs a mutating forced record.
            "--force" => force = true,
            "--stdout" => stdout = true,
            // `-m` / `--message` carries the changelog message recorded on
            // the version block `kio sig commit` seals. The value is the
            // next token; multiple `-m` flags join with newlines (each
            // becomes its own `///` line, like `git commit -m … -m …`).
            "-m" | "--message" => {
                i += 1;
                let Some(value) = args.get(i) else {
                    eprintln!("error: `{arg}` requires a message argument");
                    return ExitCode::Usage;
                };
                match &mut message {
                    Some(existing) => {
                        existing.push('\n');
                        existing.push_str(value);
                    }
                    None => message = Some(value.clone()),
                }
            }
            // `--breaking` / `--since N` are `kio sig log` display filters
            // (validated against the subcommand in the post-loop guard).
            "--breaking" => log_breaking = true,
            "--since" => {
                i += 1;
                let Some(value) = args.get(i) else {
                    eprintln!("error: `--since` requires a <version> argument");
                    return ExitCode::Usage;
                };
                match value.parse::<u32>() {
                    Ok(v) => log_since = Some(v),
                    Err(_) => {
                        eprintln!(
                            "error: `--since` <version> must be a non-negative integer, got `{value}`"
                        );
                        return ExitCode::Usage;
                    }
                }
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown flag for `kio sig`: {other}");
                return ExitCode::Usage;
            }
            "stage" if !parsed_subcommand => {
                subcommand = SigCommand::Record { force: false };
                parsed_subcommand = true;
            }
            "commit" if !parsed_subcommand => {
                subcommand = SigCommand::Commit;
                parsed_subcommand = true;
            }
            "uncommit" if !parsed_subcommand => {
                subcommand = SigCommand::Uncommit { force: false };
                parsed_subcommand = true;
            }
            "status" if !parsed_subcommand => {
                subcommand = SigCommand::Status;
                parsed_subcommand = true;
            }
            "log" if !parsed_subcommand => {
                subcommand = SigCommand::Log;
                parsed_subcommand = true;
            }
            "compact" if !parsed_subcommand => {
                parsed_subcommand = true;
                // The next non-flag token is the collapse-before version.
                i += 1;
                let Some(ver_arg) = args.get(i) else {
                    eprintln!("error: `kio sig compact` requires a <version> argument");
                    return ExitCode::Usage;
                };
                match ver_arg.parse::<u32>() {
                    Ok(v) if v >= 1 => compact_version = Some(v),
                    _ => {
                        eprintln!(
                            "error: `kio sig compact` <version> must be a positive integer, got `{ver_arg}`"
                        );
                        return ExitCode::Usage;
                    }
                }
                subcommand = SigCommand::Compact;
            }
            // Any other positional is a package selector.
            other => positionals.push(other.to_owned()),
        }
        i += 1;
    }

    // `--force` is honored only on `kio sig stage` (Record) and
    // `kio sig uncommit`. On any other subcommand it is a usage error —
    // without this guard a `status --force` / `log --force` would
    // silently turn a read-only command into a mutating one.
    if force {
        match subcommand {
            SigCommand::Record { .. } => subcommand = SigCommand::Record { force: true },
            SigCommand::Uncommit { .. } => subcommand = SigCommand::Uncommit { force: true },
            _ => {
                eprintln!(
                    "error: `--force` is only valid on `kio sig stage` (record a break) or `kio sig uncommit` (rewrite a sealed contract)"
                );
                return ExitCode::Usage;
            }
        }
    }

    // `--stdout` only applies to the write-path commands.
    if matches!(subcommand, SigCommand::Status | SigCommand::Log) && stdout {
        eprintln!("error: `--stdout` is only valid on `kio sig stage` / `commit` / `compact`");
        return ExitCode::Usage;
    }

    // `-m` / `--message` records the changelog message on the sealed
    // version block, so it only applies to `kio sig commit`.
    if message.is_some() && !matches!(subcommand, SigCommand::Commit) {
        eprintln!("error: `-m` / `--message` is only valid on `kio sig commit`");
        return ExitCode::Usage;
    }

    // `--breaking` / `--since` are read-only display filters for
    // `kio sig log`.
    if (log_breaking || log_since.is_some()) && !matches!(subcommand, SigCommand::Log) {
        eprintln!("error: `--breaking` / `--since` are only valid on `kio sig log`");
        return ExitCode::Usage;
    }

    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return ExitCode::Internal;
        }
    };

    let package_dirs = match resolve_package_dirs(&cwd, &positionals) {
        Ok(dirs) => dirs,
        Err(code) => return code,
    };

    let log_filter = LogFilter {
        breaking: log_breaking,
        since: log_since,
    };

    // Each package is independent — own `*.sig.kio`, own typecheck, no
    // cross-package rollback — so the fan-out runs them in parallel
    // (`cmd::package_fanout`), buffering each package's output and
    // replaying it in input order so diagnostics stay deterministic.
    // `combine_exit` is order-independent (worst-severity for status,
    // first-failure otherwise), so the overall code does not depend on
    // which worker finished first.
    crate::cmd::package_fanout::run(
        &package_dirs,
        "kio sig",
        |dir, cap| {
            run_one(
                dir,
                &subcommand,
                stdout,
                compact_version,
                message.as_deref(),
                &log_filter,
                cap,
            )
        },
        |acc, next| combine_exit(acc, next, &subcommand),
    )
}

/// The read-only display filters for `kio sig log`.
#[derive(Default)]
struct LogFilter {
    /// `--breaking`: show only versions with a `breaking` section, and
    /// only their breaking entries (the `nonbreaking` section is dropped).
    breaking: bool,
    /// `--since N`: show only versions strictly greater than `N`.
    since: Option<u32>,
}

impl LogFilter {
    /// Whether any filter is active. With none, `kio sig log` prints the
    /// on-disk changelog verbatim (preserving its exact bytes).
    fn is_active(&self) -> bool {
        self.breaking || self.since.is_some()
    }
}

/// Which `kio sig` subcommand to run for each package.
enum SigCommand {
    Record { force: bool },
    Commit,
    Uncommit { force: bool },
    Status,
    Log,
    Compact,
}

/// Combine a per-package result into the running overall exit code.
/// For `status` the more-severe `8x` code wins (CI wants the worst);
/// for the write commands the first failure wins.
fn combine_exit(acc: ExitCode, next: ExitCode, command: &SigCommand) -> ExitCode {
    if matches!(command, SigCommand::Status) {
        // Severity order: 80 > 82 > 81 > 0; a non-sig failure (e.g. a
        // typecheck code) outranks all (it short-circuits before the
        // sig comparison anyway, so it can only appear alone per
        // package, but keep it dominant).
        return more_severe_status(acc, next);
    }
    if acc != ExitCode::Success { acc } else { next }
}

fn status_rank(code: ExitCode) -> u8 {
    match code {
        ExitCode::Success => 0,
        ExitCode::SigStale => 1,
        ExitCode::SigUnsealedBreak => 2,
        ExitCode::SigIncompatible => 3,
        // A typecheck / build code is the most severe (a broken package).
        _ => 4,
    }
}

fn more_severe_status(a: ExitCode, b: ExitCode) -> ExitCode {
    if status_rank(b) > status_rank(a) {
        b
    } else {
        a
    }
}

/// Resolve the package directories to process. Mirrors `kio build`:
/// with no selector, discover every package in the cwd subtree; with
/// selectors, resolve each to its holding directory.
fn resolve_package_dirs(cwd: &Path, selectors: &[String]) -> Result<Vec<PathBuf>, ExitCode> {
    if selectors.is_empty() {
        let roots = match package_collection::discover_package_roots(cwd) {
            Ok(roots) => roots,
            Err(package_collection::WalkError::MultiplePackageFiles { root, paths }) => {
                eprintln!(
                    "error: multiple `*.pkg.kio` files at the package root {}; expected exactly one per package directory",
                    DisplayPath(&root)
                );
                for p in paths {
                    eprintln!("  {}", DisplayPath(&p));
                }
                return Err(ExitCode::Build);
            }
            Err(e) => {
                eprintln!(
                    "error: walking source tree: {}",
                    e.into_located().error.diag().1
                );
                return Err(ExitCode::Internal);
            }
        };
        if roots.is_empty() {
            eprintln!(
                "error: no `<name>.pkg.kio` in {} or its subdirectories — `kio sig` requires a package file at a package root",
                DisplayPath(cwd)
            );
            return Err(ExitCode::Build);
        }
        return Ok(roots.into_iter().map(|r| r.dir).collect());
    }

    let mut dirs = Vec::with_capacity(selectors.len());
    for sel in selectors {
        let path = Path::new(sel);
        let dir = if path.is_dir() {
            path.to_path_buf()
        } else if path.is_file() {
            // Only a `*.pkg.kio` file is a valid file selector; any
            // other existing file (a `notes.txt`, a module `.kio`) is an
            // input error, not a silent accept.
            let is_pkg = path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(crate::file_kind::is_package_file);
            if !is_pkg {
                eprintln!(
                    "error: package selector `{sel}` is not an existing directory or `*.pkg.kio` file"
                );
                return Err(ExitCode::Build);
            }
            match path.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
                _ => cwd.to_path_buf(),
            }
        } else {
            eprintln!(
                "error: package selector `{sel}` is not an existing directory or `*.pkg.kio` file"
            );
            return Err(ExitCode::Build);
        };
        dirs.push(dir);
    }
    Ok(dedup_dirs(dirs))
}

/// Canonicalize + deduplicate resolved package directories, preserving
/// first-seen order. Overlapping / duplicate selectors (`./pkg ./pkg`,
/// or `./pkg ./pkg/foo.pkg.kio`) collapse to one entry, so a fan-out
/// command (`commit` / `compact`) mutates each package exactly once.
fn dedup_dirs(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(dirs.len());
    for dir in dirs {
        // Canonicalize for identity; fall back to the raw path when the
        // dir can't be canonicalized (it was already validated to exist,
        // so this is defensive).
        let key = fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if seen.insert(key) {
            out.push(dir);
        }
    }
    out
}

/// Process one package directory under the chosen subcommand.
fn run_one(
    pkg_dir: &Path,
    command: &SigCommand,
    stdout: bool,
    compact_version: Option<u32>,
    message: Option<&str>,
    log_filter: &LogFilter,
    cap: &mut CapturedOutput,
) -> ExitCode {
    // Find the package file + name first; every subcommand needs it.
    let (package_name, _pkg_file) = match find_package(pkg_dir, cap) {
        Ok(found) => found,
        Err(code) => return code,
    };

    // `log` reads only the on-disk changelog (no typecheck needed).
    if let SigCommand::Log = command {
        return run_log(pkg_dir, &package_name, log_filter, cap);
    }

    // Read + parse the on-disk changelog (if any). Header-coherence
    // (#3): the `signature <pkg>` header must equal the package name.
    let existing = match load_sig_file(pkg_dir, &package_name, cap) {
        Ok(file) => file,
        Err(code) => return code,
    };

    if let SigCommand::Compact = command {
        let version = compact_version.expect("compact parsed its version");
        return run_compact(
            pkg_dir,
            &package_name,
            existing.as_ref(),
            version,
            stdout,
            cap,
        );
    }

    // The remaining commands need the typechecked package. The `1x`
    // typecheck cascade short-circuits here, before any sig comparison.
    let package = match typecheck_package(pkg_dir, cap) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let live = ContractSnapshot::from_package(&package);
    let recorded = RecordedSurface::from_package(&package);

    let plan = match sig::compute_draft(&package_name, existing.as_ref(), &recorded, &live) {
        Ok(plan) => plan,
        Err(e) => {
            cap_errln!(
                cap,
                "error: replaying the sealed history of `{package_name}`: {}",
                e.diag().1
            );
            if let Some(help) = e.diagnostic().help() {
                cap_errln!(cap, "help: {help}");
            }
            return ExitCode::Build;
        }
    };

    match command {
        SigCommand::Record { force } => run_record(
            pkg_dir,
            &package_name,
            existing.as_ref(),
            &plan,
            *force,
            stdout,
            cap,
        ),
        SigCommand::Commit => run_commit(
            pkg_dir,
            &package_name,
            existing.as_ref(),
            &plan,
            stdout,
            message,
            cap,
        ),
        SigCommand::Uncommit { force } => run_uncommit(
            pkg_dir,
            &package_name,
            existing.as_ref(),
            &recorded,
            &live,
            *force,
            stdout,
            cap,
        ),
        SigCommand::Status => run_status(&package_name, existing.as_ref(), &plan, cap),
        SigCommand::Log | SigCommand::Compact => {
            unreachable!("log / compact handled before typecheck")
        }
    }
}

/// `kio sig stage` / `kio sig stage --force` — record the live delta
/// into the draft.
fn run_record(
    pkg_dir: &Path,
    package_name: &str,
    existing: Option<&SignatureFile<Surface>>,
    plan: &sig::DraftPlan,
    force: bool,
    stdout: bool,
    cap: &mut CapturedOutput,
) -> ExitCode {
    // Intra-draft net-out advisory: an item that the previous on-disk
    // draft recorded as an `add` but the freshly-recomputed draft drops
    // entirely (it is in neither the sealed baseline nor the live
    // surface) was added then removed within the open draft before
    // sealing. The recompute correctly nets it out; this names it so the
    // churn is not silent. Derived by diffing the on-disk draft against
    // the recomputed one (presentational), not a persisted op-log.
    warn_intra_draft_net_out(existing, &plan.recomputed, cap);

    if plan.report.is_empty() {
        // Nothing changed vs the sealed contract; the draft already
        // describes the live surface (or there is nothing to record).
        // Still (re)write so a stale on-disk draft is reconciled.
        if !drafts_equal(existing, &plan.recomputed) {
            return write_or_print(
                pkg_dir,
                package_name,
                &plan.recomputed,
                stdout,
                "recorded",
                cap,
            );
        }
        cap_outln!(
            cap,
            "kio sig: `{package_name}` is up to date — no contract change to record"
        );
        return ExitCode::Success;
    }

    if plan.report.is_breaking() && !force {
        cap_errln!(
            cap,
            "error: recording the contract of `{package_name}` would break the last sealed contract:"
        );
        for change in plan.report.breaking() {
            cap_errln!(cap, "  - {}", change.detail);
        }
        cap_errln!(
            cap,
            "re-run with `kio sig stage --force` to record the break (it stays unsealed-pending until `kio sig commit`)"
        );
        return ExitCode::Build;
    }

    write_or_print(
        pkg_dir,
        package_name,
        &plan.recomputed,
        stdout,
        "recorded",
        cap,
    )
}

/// `kio sig commit` — seal the draft + increment the version. Records
/// nothing; errors on any unrecorded delta.
fn run_commit(
    pkg_dir: &Path,
    package_name: &str,
    existing: Option<&SignatureFile<Surface>>,
    plan: &sig::DraftPlan,
    stdout: bool,
    message: Option<&str>,
    cap: &mut CapturedOutput,
) -> ExitCode {
    // The worktree must be fully reconciled: the on-disk draft must
    // already record the live delta. Else `commit` would silently seal a
    // surface that differs from what the draft claims.
    if !drafts_equal(existing, &plan.recomputed) {
        cap_errln!(
            cap,
            "error: cannot `kio sig commit` `{package_name}` — the draft has unrecorded changes:"
        );
        for change in plan.report.changes.iter() {
            cap_errln!(cap, "  - {}", change.detail);
        }
        cap_errln!(
            cap,
            "run `kio sig stage` (or `kio sig stage --force` for a break) to record them first"
        );
        return ExitCode::Build;
    }

    let current_version = existing.map(|f| f.version).unwrap_or(1);
    // `commit` seals exactly the recorded draft. An empty draft records
    // nothing, so there is nothing to seal: minting a new generation
    // would leave a phantom version with no block (a non-contiguous
    // changelog). Reject it as a no-op. (The draft is reconciled at this
    // point, so the recomputed file is authoritative: no block at the
    // current version means the draft is empty.)
    if draft_block_for(&plan.recomputed, current_version).is_none() {
        cap_outln!(
            cap,
            "kio sig: `{package_name}` has nothing to seal — no recorded change at v({current_version})"
        );
        return ExitCode::Success;
    }

    // Sealing increments the header generation and opens the next
    // (empty) draft. The existing versions — including the now-sealed
    // top version — are carried verbatim; only the header increments.
    let mut versions = existing.map(|f| f.versions.clone()).unwrap_or_default();
    // A `-m "…"` message is recorded as the now-sealed top version's
    // changelog doc-comment. It replaces any message already on the
    // block (so re-committing with a new `-m` overwrites). A multi-line
    // message (one `\n`-separated string built from one or more `-m`
    // flags) becomes one `///` line per line.
    if let Some(text) = message
        && let Some(block) = versions.iter_mut().find(|v| v.version == current_version)
    {
        block.doc = Some(crate::ast::DocComment {
            lines: text.split('\n').map(|l| l.to_owned()).collect(),
            span: crate::span::Span::new(0, 0),
        });
    }
    let sealed = SignatureFile {
        pkg: package_name.to_owned(),
        version: current_version + 1,
        versions,
        meta: crate::ast::Meta::new(crate::span::Span::new(0, 0)),
    };
    write_or_print(pkg_dir, package_name, &sealed, stdout, "sealed", cap)
}

/// `kio sig uncommit` — pop the most-recently-sealed version block back
/// into the open draft. Tip-only: it touches only the last sealed
/// generation, never an older one. Refused by default (it rewrites a
/// sealed contract); `--force` proceeds.
///
/// Mechanics: the last sealed version is `current_version - 1`. Drop it
/// (and the current open draft, which is recomputed) and step the header
/// back to `current_version - 1`, then recompute the new open draft from
/// `diff(v(N-2), live)` via [`sig::compute_draft`]. The popped block's
/// changes reappear as the open draft (the normal recompute), and no
/// older history is disturbed.
// Each parameter is an independent input the uncommit needs (the sealed
// baseline, the live + recorded surfaces, the `--force`/`--stdout`
// flags, and the captured-output sink); they don't cluster into a
// meaningful sub-record.
#[allow(clippy::too_many_arguments)]
fn run_uncommit(
    pkg_dir: &Path,
    package_name: &str,
    existing: Option<&SignatureFile<Surface>>,
    recorded: &RecordedSurface,
    live: &ContractSnapshot,
    force: bool,
    stdout: bool,
    cap: &mut CapturedOutput,
) -> ExitCode {
    let current_version = existing.map(|f| f.version).unwrap_or(1);
    // The last sealed generation is `current_version - 1`. With the
    // header still at v(1) (no `kio sig commit` has run), there is
    // nothing sealed to pop.
    if current_version <= 1 {
        cap_errln!(
            cap,
            "error: `{package_name}` has no sealed version to uncommit — the changelog is still on its first (unsealed) draft"
        );
        return ExitCode::Build;
    }

    // Refuse by default: uncommit rewrites a contract that was already
    // sealed (and may have shipped). Prefer a forward fix; `--force`
    // proceeds.
    if !force {
        let sealed = current_version - 1;
        cap_errln!(
            cap,
            "error: `kio sig uncommit` would rewrite the sealed contract v({sealed}) of `{package_name}`"
        );
        cap_errln!(
            cap,
            "a sealed version may already have shipped — prefer a forward fix (a new version recording the correction)"
        );
        cap_errln!(
            cap,
            "re-run with `kio sig uncommit --force` to pop v({sealed}) back into the draft"
        );
        return ExitCode::Build;
    }

    let file = existing.expect("current_version > 1 implies an on-disk changelog");

    // Synthesize the post-pop changelog: header steps back to the last
    // sealed generation (it becomes the open draft again), keeping only
    // the strictly-older sealed history. `compute_draft` then recomputes
    // that generation's open-draft block from `diff(v(N-2), live)`, so
    // the popped block's changes return as the open draft.
    let popped = current_version - 1;
    let truncated = SignatureFile {
        pkg: package_name.to_owned(),
        version: popped,
        versions: file
            .versions
            .iter()
            .filter(|v| v.version < popped)
            .cloned()
            .collect(),
        meta: crate::ast::Meta::new(crate::span::Span::new(0, 0)),
    };

    let plan = match sig::compute_draft(package_name, Some(&truncated), recorded, live) {
        Ok(plan) => plan,
        Err(e) => {
            cap_errln!(
                cap,
                "error: replaying the sealed history of `{package_name}` after uncommit: {}",
                e.diag().1
            );
            return ExitCode::Build;
        }
    };

    write_or_print(
        pkg_dir,
        package_name,
        &plan.recomputed,
        stdout,
        "uncommitted",
        cap,
    )
}

/// `kio sig status` — the CI gate. Maps the draft plan + on-disk draft
/// to the `8x` exit code per the settled precedence.
fn run_status(
    package_name: &str,
    existing: Option<&SignatureFile<Surface>>,
    plan: &sig::DraftPlan,
    cap: &mut CapturedOutput,
) -> ExitCode {
    let code = status_exit_code(existing, plan);
    match code {
        ExitCode::Success if existing.is_none() => {
            // Point-of-intent discoverability: a clean status on a
            // package that has never recorded a contract means there is
            // simply nothing to gate. Print a friendly, single-line note
            // pointing at the first step rather than a bare "is clean".
            cap_outln!(
                cap,
                "kio sig: `{package_name}` has no compatibility changelog yet — `kio sig commit` seals the first version"
            );
        }
        ExitCode::Success => {
            cap_outln!(cap, "kio sig: `{package_name}` is clean");
        }
        ExitCode::SigIncompatible => {
            cap_errln!(
                cap,
                "kio sig status: `{package_name}` breaks the last sealed contract (unrecorded):"
            );
            for change in plan.report.breaking() {
                cap_errln!(cap, "  - {}", change.detail);
            }
        }
        ExitCode::SigUnsealedBreak => {
            cap_errln!(
                cap,
                "kio sig status: `{package_name}` has a recorded break that is not yet sealed — run `kio sig commit`"
            );
        }
        ExitCode::SigStale => {
            cap_errln!(
                cap,
                "kio sig status: `{package_name}` has an unrecorded compatible change — run `kio sig stage`"
            );
            for change in plan.report.compatible() {
                cap_errln!(cap, "  - {}", change.detail);
            }
        }
        _ => {}
    }
    code
}

/// The `kio sig status` exit code, per the settled precedence
/// (`80 > 82 > 81 > 0`):
///
/// 1. breaks the sealed contract **and** the break is unrecorded ⇒ **80**;
/// 2. else a break **is** recorded but unsealed ⇒ **82**;
/// 3. else an unrecorded compatible delta ⇒ **81**;
/// 4. else ⇒ **0**.
///
/// The 80-vs-82 discriminator keys on whether the **breaking** portion
/// specifically is recorded, not the whole draft: a fully-recorded break
/// plus a *later, still-unrecorded compatible* add must report 82
/// (recorded-break-pending outranks 81 stale-but-compatible), not 80. So
/// the 80 gate is `is_breaking() && !break_recorded`, where
/// `break_recorded` compares only the on-disk `breaking` change-set
/// against the recomputed one. The 82/81/0 branches still use
/// whole-draft equality (`recorded`): an unsealed break is "pending" only
/// while the draft is otherwise reconciled, and any other unrecorded
/// drift is 81.
fn status_exit_code(existing: Option<&SignatureFile<Surface>>, plan: &sig::DraftPlan) -> ExitCode {
    let recorded = drafts_equal(existing, &plan.recomputed);
    let break_recorded = breaking_recorded(existing, &plan.recomputed);
    let recomputed_breaking = draft_block_for(&plan.recomputed, plan.recomputed.version)
        .map(|v| v.breaking.is_some())
        .unwrap_or(false);

    if plan.report.is_breaking() && !break_recorded {
        // A breaking change vs the sealed contract is not recorded in
        // the draft's `breaking` section.
        ExitCode::SigIncompatible
    } else if recomputed_breaking && break_recorded {
        // The break is recorded (its `breaking` section matches), still
        // current, and not sealed (the header version still names the
        // draft). This outranks a residual unrecorded compatible drift.
        ExitCode::SigUnsealedBreak
    } else if !recorded {
        // A compatible drift the draft does not yet record (the break,
        // if any, is recorded; only compatible changes remain).
        ExitCode::SigStale
    } else {
        ExitCode::Success
    }
}

/// `kio sig log` — pretty-print the changelog. The file *is* the
/// history, so with no filter the rendering replays the on-disk bytes
/// verbatim. With a `--breaking` / `--since` filter, the changelog is
/// parsed, filtered to the matching versions / sections, and re-emitted
/// through the canonical emitter.
fn run_log(
    pkg_dir: &Path,
    package_name: &str,
    filter: &LogFilter,
    cap: &mut CapturedOutput,
) -> ExitCode {
    let path = sig_path(pkg_dir, package_name);
    let source = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            cap_outln!(
                cap,
                "kio sig: `{package_name}` has no `*.sig.kio` changelog yet"
            );
            return ExitCode::Success;
        }
        Err(e) => {
            cap_errln!(cap, "error: cannot read `{}`: {e}", DisplayPath(&path));
            return ExitCode::Internal;
        }
    };

    if !filter.is_active() {
        // No filter: replay the on-disk bytes verbatim, preserving the
        // file's exact formatting.
        cap.stdout.push_str(&source);
        return ExitCode::Success;
    }

    // A filter is active: parse + filter + re-emit. A parse failure is
    // surfaced like the write-path commands' parse error.
    let file = match crate::pass::parser::parse_signature_file(&source, Some(package_name)) {
        Ok(file) => file,
        Err(e) => {
            crate::cmd::check::render_error(&path, &source, &e, &mut cap.stderr);
            return ExitCode::Parse;
        }
    };
    let filtered = filter_changelog(&file, filter);
    cap.stdout.push_str(&sig::emit_signature_file(&filtered));
    ExitCode::Success
}

/// Apply the `kio sig log` display filters to a parsed changelog,
/// returning a new `SignatureFile` carrying only the matching versions /
/// sections. `--since N` drops versions `<= N`; `--breaking` drops
/// versions with no `breaking` section and, for the survivors, drops the
/// `nonbreaking` section so only breaking entries show. The header
/// generation is preserved (it is bookkeeping, not a displayed version).
fn filter_changelog(file: &SignatureFile<Surface>, filter: &LogFilter) -> SignatureFile<Surface> {
    let versions = file
        .versions
        .iter()
        .filter(|v| match filter.since {
            Some(n) => v.version > n,
            None => true,
        })
        .filter(|v| !filter.breaking || v.breaking.is_some())
        .map(|v| {
            if filter.breaking {
                // Show only the breaking entries. A recursive member's
                // declaration lives in the version-leading `with` block, so
                // retain its complete current/previous epoch (including an
                // unchanged split peer) and the type context that epoch uses.
                let required_context =
                    breaking_context_names(file, v.version).unwrap_or_else(|_| {
                        v.breaking
                            .as_ref()
                            .map(sig_change_set_names)
                            .unwrap_or_default()
                    });
                SigVersion {
                    with: filter_sig_context(&v.with, &required_context),
                    nonbreaking: None,
                    ..v.clone()
                }
            } else {
                v.clone()
            }
        })
        .collect();
    SignatureFile {
        pkg: file.pkg.clone(),
        version: file.version,
        versions,
        meta: crate::ast::Meta::new(crate::span::Span::new(0, 0)),
    }
}

/// Retain the declaration context needed to understand exact operation
/// references that survived a display filter. Recursive groups are atomic:
/// naming one member keeps the complete group and its imports, while unrelated
/// groups disappear with the operations they supported.
fn filter_sig_context(
    sections: &[crate::ast::SigModuleSection<Surface>],
    required: &std::collections::BTreeSet<(String, String)>,
) -> Vec<crate::ast::SigModuleSection<Surface>> {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    // Treat each ordinary declaration as one context unit and every rec group
    // as one atomic unit. First index all declared names; then close the units
    // containing surviving operation targets over version-local type
    // references. This keeps a second group only when the selected group's
    // declarations genuinely depend on it.
    let mut units = Vec::<(usize, usize, Vec<(String, String)>)>::new();
    let mut owner = BTreeMap::<(String, String), usize>::new();
    for (section_index, section) in sections.iter().enumerate() {
        let module = module_path_str(&section.path);
        for (item_index, item) in section.items.iter().enumerate() {
            let declarations = sig_context_item_names(item)
                .into_iter()
                .map(|name| (module.clone(), name))
                .collect::<Vec<_>>();
            let unit_index = units.len();
            for declaration in &declarations {
                owner.insert(declaration.clone(), unit_index);
            }
            units.push((section_index, item_index, declarations));
        }
    }

    let mut dependencies = vec![BTreeSet::new(); units.len()];
    for (unit_index, (section_index, item_index, _)) in units.iter().enumerate() {
        let section = &sections[*section_index];
        let item = &section.items[*item_index];
        for dependency in sig_context_item_dependencies(item, section) {
            if let Some(dependency_unit) = owner.get(&dependency)
                && *dependency_unit != unit_index
            {
                dependencies[unit_index].insert(*dependency_unit);
            }
        }
    }

    let mut selected = BTreeSet::new();
    let mut queue = VecDeque::new();
    for name in required {
        if let Some(unit) = owner.get(name)
            && selected.insert(*unit)
        {
            queue.push_back(*unit);
        }
    }
    while let Some(unit) = queue.pop_front() {
        for dependency in &dependencies[unit] {
            if selected.insert(*dependency) {
                queue.push_back(*dependency);
            }
        }
    }
    let selected_positions = selected
        .iter()
        .map(|unit| (units[*unit].0, units[*unit].1))
        .collect::<BTreeSet<_>>();

    sections
        .iter()
        .enumerate()
        .filter_map(|(section_index, section)| {
            let items = section
                .items
                .iter()
                .enumerate()
                .filter(|(item_index, _)| {
                    selected_positions.contains(&(section_index, *item_index))
                })
                .map(|(_, item)| item.clone())
                .collect::<Vec<_>>();
            (!items.is_empty()).then(|| crate::ast::SigModuleSection {
                leading_trivia: section.leading_trivia.clone(),
                trailing_trivia: section.trailing_trivia.clone(),
                path: section.path.clone(),
                // The canonical emitter removes imports unused by the retained
                // declarations, so preserving the source list here loses no
                // exact meaning and does not leak unrelated imports to output.
                imports: section.imports.clone(),
                items,
                span: section.span,
            })
        })
        .collect()
}

fn sig_context_item_names(item: &crate::ast::SigItem<Surface>) -> Vec<String> {
    use crate::ast::{SigItem, TypeRecMember};
    match item {
        SigItem::TypeRecGroup(group) => group
            .members
            .iter()
            .filter_map(|member| match member {
                TypeRecMember::TypeAlias(alias) => Some(alias.name.clone()),
                TypeRecMember::Newtype(newtype) => Some(newtype.name.clone()),
                TypeRecMember::Labels(labels, _) => labels.type_alias_name.clone(),
            })
            .collect(),
        other => vec![sig_item_leaf(other).to_owned()],
    }
}

fn sig_context_item_dependencies(
    item: &crate::ast::SigItem<Surface>,
    section: &crate::ast::SigModuleSection<Surface>,
) -> std::collections::BTreeSet<(String, String)> {
    use crate::ast::{HostFnParam, SigItem, TypeRecMember};

    let mut heads = Vec::new();
    let collect_fn = |function: &crate::ast::HostFn<Surface>, heads: &mut Vec<Vec<String>>| {
        let bound = function
            .params
            .iter()
            .filter_map(|param| match param {
                HostFnParam::Type(param) => Some(param.name.clone()),
                HostFnParam::Value(_) => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        for param in &function.params {
            if let HostFnParam::Value(param) = param {
                collect_sig_context_type_heads(&param.ty, &bound, heads);
            }
        }
        collect_sig_context_type_heads(&function.ret, &bound, heads);
    };
    match item {
        SigItem::HostType(_) => {}
        SigItem::HostFn(function) => collect_fn(function, &mut heads),
        SigItem::ExportFn(export) => collect_fn(&export.function, &mut heads),
        SigItem::TypeAlias(alias) => {
            let bound = alias
                .type_params
                .iter()
                .map(|param| param.name.clone())
                .collect();
            collect_sig_context_type_heads(&alias.body, &bound, &mut heads);
        }
        SigItem::Newtype(newtype) => {
            let bound = newtype
                .type_params
                .iter()
                .chain(&newtype.existential_params)
                .map(|param| param.name.clone())
                .collect();
            collect_sig_context_type_heads(&newtype.payload, &bound, &mut heads);
        }
        SigItem::TypeRecGroup(group) => {
            for member in &group.members {
                match member {
                    TypeRecMember::TypeAlias(alias) => {
                        let bound = alias
                            .type_params
                            .iter()
                            .map(|param| param.name.clone())
                            .collect();
                        collect_sig_context_type_heads(&alias.body, &bound, &mut heads);
                    }
                    TypeRecMember::Newtype(newtype) => {
                        let bound = newtype
                            .type_params
                            .iter()
                            .chain(&newtype.existential_params)
                            .map(|param| param.name.clone())
                            .collect();
                        collect_sig_context_type_heads(&newtype.payload, &bound, &mut heads);
                    }
                    TypeRecMember::Labels(_, _) => {}
                }
            }
        }
    }

    let module = module_path_str(&section.path);
    heads
        .into_iter()
        .filter_map(|head| resolve_sig_context_head(&module, &section.imports, &head))
        .collect()
}

fn collect_sig_context_type_heads(
    ty: &crate::ast::Type<Surface>,
    bound: &std::collections::BTreeSet<String>,
    out: &mut Vec<Vec<String>>,
) {
    use crate::ast::Type;
    match ty {
        Type::Path { segments, args, .. } => {
            let head = segments
                .iter()
                .map(|segment| segment.name.clone())
                .collect::<Vec<_>>();
            if !(head.len() == 1 && bound.contains(&head[0])) {
                out.push(head);
            }
            for argument in args {
                collect_sig_context_type_heads(argument, bound, out);
            }
        }
        Type::Function { param, ret, .. } => {
            collect_sig_context_type_heads(param, bound, out);
            collect_sig_context_type_heads(ret, bound, out);
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            collect_sig_context_type_heads(left, bound, out);
            collect_sig_context_type_heads(right, bound, out);
        }
        Type::Forall { param, body, .. } => {
            let mut bound = bound.clone();
            bound.insert(param.name.clone());
            collect_sig_context_type_heads(body, &bound, out);
        }
        Type::Goal { args, .. } => {
            for argument in args {
                collect_sig_context_type_heads(argument, bound, out);
            }
        }
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } | Type::LabelSugar { .. } => {}
    }
}

fn resolve_sig_context_head(
    module: &str,
    imports: &[crate::ast::Import],
    head: &[String],
) -> Option<(String, String)> {
    use crate::ast::{ImportItem, ImportKind};
    let (first, tail) = head.split_first()?;
    if tail.is_empty() {
        for usage in imports {
            if let ImportKind::Selective { items, from } = &usage.kind
                && items
                    .iter()
                    .any(|item| matches!(item, ImportItem::Name { name, .. } if name == first))
            {
                return Some((module_path_str(from), first.clone()));
            }
        }
        return Some((module.to_owned(), first.clone()));
    }
    for usage in imports {
        if let ImportKind::Qualified { path, alias, .. } = &usage.kind
            && alias == first
        {
            let mut resolved = path
                .segments
                .iter()
                .map(|segment| segment.name.clone())
                .collect::<Vec<_>>();
            resolved.extend_from_slice(tail);
            let leaf = resolved.pop()?;
            return Some((resolved.join("/"), leaf));
        }
    }
    None
}

/// `kio sig compact <version>` — collapse additive pre-`<version>`
/// history into a synthesized boundary block, then carry versions
/// `>= version` through canonical emission without changing their
/// operations or generations. The collapsed prefix must contain first
/// introductions only. A `modify`, `remove`, re-add, or other history
/// that cannot be expressed by one add-only boundary is rejected before
/// the changelog write begins. The cut cannot pass the current header,
/// so an open draft always remains in the suffix.
fn run_compact(
    pkg_dir: &Path,
    package_name: &str,
    existing: Option<&SignatureFile<Surface>>,
    version: u32,
    stdout: bool,
    cap: &mut CapturedOutput,
) -> ExitCode {
    let Some(file) = existing else {
        cap_errln!(
            cap,
            "error: `{package_name}` has no `*.sig.kio` changelog to compact"
        );
        return ExitCode::Build;
    };
    let max_version = file.versions.iter().map(|v| v.version).max().unwrap_or(0);
    let max_cut = max_version.saturating_add(1).min(file.version);
    if version < 1 || version > max_cut {
        cap_errln!(
            cap,
            "error: `kio sig compact {version}` is out of range — the cut must be within v(1)..=v({max_cut}) and cannot exceed `{package_name}`'s current header v({})",
            file.version
        );
        return ExitCode::Build;
    }
    let compacted_text = match compact_file(file, package_name, version) {
        Ok(text) => text,
        Err(e) => {
            cap_errln!(
                cap,
                "error: cannot compact `{package_name}` history before v({version}): {}",
                e.diag().1
            );
            return ExitCode::Build;
        }
    };
    write_text_or_print(
        pkg_dir,
        package_name,
        &compacted_text,
        stdout,
        "compacted",
        cap,
    )
}

/// Pure compact computation: collapse `file`'s pre-`version` history into
/// a synthesized boundary block, carrying versions `>= version` through
/// canonical emission without changing their operations or generations.
/// The exact returned text has already been parsed and replayed to prove
/// that it reproduces the complete interface and retained-removal state,
/// including every suffix removal's original generation. Separated from
/// `run_compact` so the replay-equivalence invariant is unit-testable
/// without disk I/O.
fn compact_file(
    file: &SignatureFile<Surface>,
    package_name: &str,
    version: u32,
) -> Result<String, sig::ReplayError> {
    let max_version = file.versions.iter().map(|v| v.version).max().unwrap_or(0);
    let max_cut = max_version.saturating_add(1).min(file.version);
    if version < 1 || version > max_cut {
        return Err(crate::error::Error::bridge(
            file.meta.span,
            format!(
                "compact cut v({version}) is outside v(1)..=v({max_cut}); a cut cannot exceed the current header v({})",
                file.version
            ),
        ));
    }
    validate_compact_prefix(file, version)?;

    // Replay the operation-additive prefix to recover its effective
    // interface. First-introduction-only eligibility proves there is no
    // retained removal to flatten and no later incarnation to choose
    // between.
    let through = version.saturating_sub(1);
    let replayed = sig::replay_through(file, through)?;

    // Materialize the surviving interface as a single synthesized
    // add-only boundary at version `through` (the last collapsed
    // version). Each surviving suffix block keeps the same version,
    // operation, message, declaration, and removal generation, while
    // the canonical emitter may normalize its presentation.
    let boundary = if through > 0 {
        synthesize_boundary_block(through, &replayed)
    } else {
        None
    };

    let has_prefix = file.versions.iter().any(|block| block.version < version);
    if has_prefix && boundary.is_none() {
        let span = file
            .versions
            .iter()
            .find(|block| block.version < version)
            .map_or(file.meta.span, |block| block.span);
        return Err(crate::error::Error::bridge(
            span,
            "the pre-cut history has no representable compact boundary; compact leaves no implicit version gap",
        ));
    }

    let mut versions: Vec<SigVersion<Surface>> = Vec::new();
    if let Some(block) = boundary {
        versions.push(block);
    }
    for v in &file.versions {
        if v.version >= version {
            versions.push(v.clone());
        }
    }
    let compacted = SignatureFile {
        pkg: package_name.to_owned(),
        version: file.version,
        versions,
        meta: crate::ast::Meta::new(crate::span::Span::new(0, 0)),
    };

    // Full replay equality preserves the resulting interface and retained
    // history. The independent last-sealed comparison prevents a rewrite
    // that reaches the same tip by moving sealed entries into the open
    // draft, which would change `kio sig status`.
    let original_replay = normalized_replay_state(&sig::replay(file)?);
    let sealed_version = file.version.saturating_sub(1);
    let original_sealed_replay =
        normalized_replay_state(&sig::replay_through(file, sealed_version)?);
    let compacted_replay = sig::replay(&compacted)?;
    if original_replay != normalized_replay_state(&compacted_replay) {
        return Err(crate::error::Error::bridge(
            file.meta.span,
            "the synthesized compact boundary does not preserve the complete replay state",
        ));
    }
    let compacted_sealed_replay = sig::replay_through(&compacted, sealed_version)?;
    if original_sealed_replay != normalized_replay_state(&compacted_sealed_replay) {
        return Err(crate::error::Error::bridge(
            file.meta.span,
            "the synthesized compact boundary does not preserve the last sealed replay state",
        ));
    }

    // Validate the exact canonical text the caller will print or write,
    // not merely the pre-render AST. Parsing and replaying that text
    // catches both an inadmissible later boundary and any semantic change
    // introduced by canonical ordering before stdout or atomic replace.
    let rendered = sig::emit_signature_file(&compacted);
    let reparsed = crate::pass::parser::parse_signature_file(&rendered, Some(package_name))?;
    let rendered_replay = sig::replay(&reparsed)?;
    if original_replay != normalized_replay_state(&rendered_replay) {
        return Err(crate::error::Error::bridge(
            file.meta.span,
            "the rendered compact changelog does not preserve the complete replay state",
        ));
    }
    let rendered_sealed_replay = sig::replay_through(&reparsed, sealed_version)?;
    if original_sealed_replay != normalized_replay_state(&rendered_sealed_replay) {
        return Err(crate::error::Error::bridge(
            file.meta.span,
            "the rendered compact changelog does not preserve the last sealed replay state",
        ));
    }

    Ok(rendered)
}

/// Reject any pre-cut history that is not an operation-additive sequence
/// of first introductions. The single compact boundary has no syntax for
/// a historical `modify`, `remove`, or same-name later incarnation, and
/// silently flattening one would change its frozen origin or removal
/// generation.
fn validate_compact_prefix(
    file: &SignatureFile<Surface>,
    version: u32,
) -> Result<(), sig::ReplayError> {
    use crate::ast::SigItem;
    use crate::sig::ContractSide;
    use std::collections::BTreeSet;

    let mut introduced = BTreeSet::new();
    for block in file.versions.iter().filter(|block| block.version < version) {
        let context_sides = sig_context_sides(block)?;
        for (set, expected_side, partition) in [
            (block.breaking.as_ref(), ContractSide::Env, "breaking"),
            (
                block.nonbreaking.as_ref(),
                ContractSide::Export,
                "nonbreaking",
            ),
        ] {
            let Some(set) = set else {
                continue;
            };
            if let Some(span) = set
                .modify
                .first()
                .map(|section| section.span)
                .or_else(|| set.modify_refs.first().map(|reference| reference.span))
            {
                return Err(crate::error::Error::bridge(
                    span,
                    format!(
                        "pre-cut v({}) contains `modify`; compact accepts only first-introduction `add` history",
                        block.version
                    ),
                ));
            }
            if let Some(span) = set
                .remove
                .first()
                .map(|section| section.span)
                .or_else(|| set.remove_refs.first().map(|reference| reference.span))
            {
                return Err(crate::error::Error::bridge(
                    span,
                    format!(
                        "pre-cut v({}) contains `remove`; compact preserves removals only at their original generations in the surviving suffix",
                        block.version
                    ),
                ));
            }
            if set.add.is_empty() && set.add_refs.is_empty() {
                return Err(crate::error::Error::bridge(
                    set.span,
                    format!(
                        "pre-cut v({}) has no additive introduction to materialize at the compact boundary",
                        block.version
                    ),
                ));
            }

            for section in &set.add {
                let module = module_path_string(&section.path);
                for item in &section.items {
                    let (name, side) = match item {
                        SigItem::HostType(item) => (item.name.as_str(), ContractSide::Env),
                        SigItem::HostFn(item) => (item.name.as_str(), ContractSide::Env),
                        SigItem::TypeAlias(item) => (item.name.as_str(), ContractSide::Export),
                        SigItem::Newtype(item) => (item.name.as_str(), ContractSide::Export),
                        SigItem::ExportFn(item) => {
                            (item.function.name.as_str(), ContractSide::Export)
                        }
                        SigItem::TypeRecGroup(_) => {
                            return Err(crate::error::Error::bridge(
                                section.span,
                                "recursive type groups belong in a version-leading `with` block",
                            ));
                        }
                    };
                    if side != expected_side {
                        return Err(crate::error::Error::bridge(
                            section.span,
                            format!(
                                "pre-cut v({}) places `{module}.{name}` in `{partition}`, which is not its computed add verdict",
                                block.version
                            ),
                        ));
                    }
                    if !introduced.insert((module.clone(), name.to_owned())) {
                        return Err(crate::error::Error::bridge(
                            section.span,
                            format!(
                                "pre-cut v({}) re-adds `{module}.{name}`; compact accepts each qualified name's first introduction only",
                                block.version
                            ),
                        ));
                    }
                }
            }
            for reference in &set.add_refs {
                let module = module_path_str(&reference.path);
                let key = (module.clone(), reference.name.clone());
                let Some(side) = context_sides.get(&key).copied() else {
                    return Err(crate::error::Error::bridge(
                        reference.span,
                        format!(
                            "pre-cut v({}) references `{module}.{}` outside that version's `with` block",
                            block.version, reference.name
                        ),
                    ));
                };
                if side != expected_side {
                    return Err(crate::error::Error::bridge(
                        reference.span,
                        format!(
                            "pre-cut v({}) places `{module}.{}` in `{partition}`, which is not its computed add verdict",
                            block.version, reference.name
                        ),
                    ));
                }
                if !introduced.insert(key) {
                    return Err(crate::error::Error::bridge(
                        reference.span,
                        format!(
                            "pre-cut v({}) re-adds `{module}.{}`; compact accepts each qualified name's first introduction only",
                            block.version, reference.name
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Exact declaration sides supplied by one version's `with` block. The map
/// flattens recursive groups because operations name their members, never the
/// group container.
fn sig_context_sides(
    block: &SigVersion<Surface>,
) -> Result<std::collections::BTreeMap<(String, String), sig::ContractSide>, sig::ReplayError> {
    use crate::ast::{SigItem, TypeRecMember};
    let mut sides = std::collections::BTreeMap::new();
    for section in &block.with {
        let module = module_path_str(&section.path);
        for item in &section.items {
            let declarations = match item {
                SigItem::HostType(host) => vec![(host.name.clone(), sig::ContractSide::Env)],
                SigItem::HostFn(host) => vec![(host.name.clone(), sig::ContractSide::Env)],
                SigItem::TypeAlias(alias) => {
                    vec![(alias.name.clone(), sig::ContractSide::Export)]
                }
                SigItem::Newtype(newtype) => {
                    vec![(newtype.name.clone(), sig::ContractSide::Export)]
                }
                SigItem::ExportFn(export) => {
                    vec![(export.function.name.clone(), sig::ContractSide::Export)]
                }
                SigItem::TypeRecGroup(group) => group
                    .members
                    .iter()
                    .filter_map(|member| match member {
                        TypeRecMember::TypeAlias(alias) => {
                            Some((alias.name.clone(), sig::ContractSide::Export))
                        }
                        TypeRecMember::Newtype(newtype) => {
                            Some((newtype.name.clone(), sig::ContractSide::Export))
                        }
                        TypeRecMember::Labels(_, _) => None,
                    })
                    .collect(),
            };
            for (name, side) in declarations {
                if sides.insert((module.clone(), name.clone()), side).is_some() {
                    return Err(crate::error::Error::bridge(
                        section.span,
                        format!(
                            "signature version context declares `{module}.{name}` more than once"
                        ),
                    ));
                }
            }
        }
    }
    Ok(sides)
}

/// Build the synthesized add-only compact-boundary block.
///
/// Every surviving live item is re-materialized as an `add` (env →
/// `breaking.add`, export → `nonbreaking.add`). Each item keeps its own
/// frozen declaration and origin `import` clauses in a separate module
/// section. Keeping origins separate avoids merging two historical
/// import environments that happen to use the same local head for
/// different modules.
fn synthesize_boundary_block(
    version: u32,
    replayed: &sig::ReplayedInterface,
) -> Option<SigVersion<Surface>> {
    use crate::ast::{SigChangeSet, SigItemRef, SigModuleSection};
    use crate::sig::ContractSide;
    let mut nonbreaking_add = Vec::new();
    let mut breaking_add = Vec::new();
    let mut nonbreaking_add_refs = Vec::new();
    let mut breaking_add_refs = Vec::new();
    let mut recursive_contexts = std::collections::BTreeMap::new();
    for item in &replayed.live_frozen {
        if let Some(context) = &item.recursive_context {
            recursive_contexts
                .entry(recursive_context_key(context))
                .or_insert_with(|| context.as_ref().clone());
            let reference = SigItemRef {
                leading_trivia: Vec::new(),
                path: module_path_of(&item.entry.name.module_path),
                name: item.entry.name.leaf.clone(),
                span: crate::span::Span::new(0, 0),
            };
            match item.entry.side {
                ContractSide::Env => breaking_add_refs.push(reference),
                ContractSide::Export => nonbreaking_add_refs.push(reference),
            }
            continue;
        }
        let section = SigModuleSection {
            leading_trivia: Vec::new(),
            trailing_trivia: Vec::new(),
            path: module_path_of(&item.entry.name.module_path),
            imports: item.imports.clone(),
            items: vec![item.frozen.clone()],
            span: crate::span::Span::new(0, 0),
        };
        match item.entry.side {
            ContractSide::Env => breaking_add.push(section),
            ContractSide::Export => nonbreaking_add.push(section),
        }
    }

    let breaking = if breaking_add.is_empty() && breaking_add_refs.is_empty() {
        None
    } else {
        Some(SigChangeSet {
            leading_trivia: Vec::new(),
            trailing_trivia: Vec::new(),
            add_leading_trivia: Vec::new(),
            add_trailing_trivia: Vec::new(),
            modify_leading_trivia: Vec::new(),
            modify_trailing_trivia: Vec::new(),
            remove_leading_trivia: Vec::new(),
            remove_trailing_trivia: Vec::new(),
            add: breaking_add,
            add_refs: breaking_add_refs,
            modify: Vec::new(),
            modify_refs: Vec::new(),
            remove: Vec::new(),
            remove_refs: Vec::new(),
            span: crate::span::Span::new(0, 0),
        })
    };
    let nonbreaking = if nonbreaking_add.is_empty() && nonbreaking_add_refs.is_empty() {
        None
    } else {
        Some(SigChangeSet {
            leading_trivia: Vec::new(),
            trailing_trivia: Vec::new(),
            add_leading_trivia: Vec::new(),
            add_trailing_trivia: Vec::new(),
            modify_leading_trivia: Vec::new(),
            modify_trailing_trivia: Vec::new(),
            remove_leading_trivia: Vec::new(),
            remove_trailing_trivia: Vec::new(),
            add: nonbreaking_add,
            add_refs: nonbreaking_add_refs,
            modify: Vec::new(),
            modify_refs: Vec::new(),
            remove: Vec::new(),
            remove_refs: Vec::new(),
            span: crate::span::Span::new(0, 0),
        })
    };

    if breaking.is_none() && nonbreaking.is_none() {
        return None;
    }
    Some(SigVersion {
        leading_trivia: Vec::new(),
        trailing_trivia: Vec::new(),
        with_leading_trivia: Vec::new(),
        with_trailing_trivia: Vec::new(),
        // The synthesized boundary collapses additive history; it carries
        // no changelog message.
        doc: None,
        version,
        with: recursive_contexts.into_values().collect(),
        breaking,
        nonbreaking,
        span: crate::span::Span::new(0, 0),
    })
}

fn recursive_context_key(section: &crate::ast::SigModuleSection<Surface>) -> (String, Vec<String>) {
    let mut names = Vec::new();
    for item in &section.items {
        match item {
            crate::ast::SigItem::TypeRecGroup(group) => {
                for member in &group.members {
                    names.push(match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => alias.name.clone(),
                        crate::ast::TypeRecMember::Newtype(newtype) => newtype.name.clone(),
                        crate::ast::TypeRecMember::Labels(labels, _) => labels
                            .type_alias_name
                            .clone()
                            .unwrap_or_else(|| "<anonymous labels>".to_owned()),
                    });
                }
            }
            other => names.push(sig_item_leaf(other).to_owned()),
        }
    }
    names.sort();
    (module_path_string(&section.path), names)
}

/// Replay state in a span-, presentation-, and retained-item-order
/// independent form. Replay currently emits removed items in deterministic
/// qualified-name/side order, but that presentation order has no contract
/// meaning. Each retained item is therefore compared by its semantic identity,
/// canonical frozen declaration plus origin imports, and removal generation.
#[derive(Debug, PartialEq, Eq)]
struct NormalizedReplayState {
    current: ContractSnapshot,
    live_frozen: Vec<NormalizedFrozenItem>,
    removed: Vec<NormalizedFrozenItem>,
}

#[derive(Debug, PartialEq, Eq)]
struct NormalizedFrozenItem {
    entry: sig::ContractEntry,
    frozen_with_imports: String,
    frozen_type_closure: Option<Vec<(sig::QualifiedName, String)>>,
    removed_at_version: u32,
}

fn normalized_replay_state(replayed: &sig::ReplayedInterface) -> NormalizedReplayState {
    NormalizedReplayState {
        current: replayed.current.clone(),
        live_frozen: normalized_frozen_items(&replayed.live_frozen),
        removed: normalized_frozen_items(&replayed.removed),
    }
}

fn normalized_frozen_items(items: &[sig::RemovedItem]) -> Vec<NormalizedFrozenItem> {
    let mut normalized = items
        .iter()
        .map(|item| NormalizedFrozenItem {
            entry: item.entry.clone(),
            frozen_with_imports: normalized_frozen_text(item),
            frozen_type_closure: item
                .frozen_type_closure
                .as_ref()
                .map(normalized_frozen_type_closure),
            removed_at_version: item.removed_at_version,
        })
        .collect::<Vec<_>>();
    normalized.sort_by(|left, right| {
        left.entry
            .name
            .cmp(&right.entry.name)
            .then_with(|| {
                contract_side_rank(left.entry.side).cmp(&contract_side_rank(right.entry.side))
            })
            .then_with(|| left.frozen_with_imports.cmp(&right.frozen_with_imports))
            .then_with(|| left.frozen_type_closure.cmp(&right.frozen_type_closure))
            .then_with(|| left.removed_at_version.cmp(&right.removed_at_version))
    });
    normalized
}

/// Canonical, span-insensitive form of one host-function root's frozen
/// nominal closure. `None` remains distinct from `Some([])`: the former is
/// a non-host-function item, while the latter is a host function that reaches
/// no nominal declarations. Each declaration is emitted separately with the
/// `import` clauses from its own historical origin, so equal local spellings do
/// not erase which declaration generation they resolved against.
fn normalized_frozen_type_closure(
    closure: &sig::FrozenTypeClosure,
) -> Vec<(sig::QualifiedName, String)> {
    let mut declarations = closure
        .declarations
        .iter()
        .map(|(name, declaration)| {
            (
                name.clone(),
                normalized_frozen_type_declaration_text(name, declaration),
            )
        })
        .collect::<Vec<_>>();
    declarations.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    declarations
}

/// Render one declaration as a one-item signature file. The signature
/// emitter is the canonicalizer for declaration presentation, import order,
/// and spans; keeping the item isolated prevents another closure member's
/// imports from changing its meaning during normalization.
fn normalized_frozen_type_declaration_text(
    name: &sig::QualifiedName,
    declaration: &sig::FrozenTypeDeclaration,
) -> String {
    use crate::ast::{SigChangeSet, SigItem, SigModuleSection};

    let (item, side) = match &declaration.declaration {
        sig::FrozenTypeItem::HostType(item) => {
            (SigItem::HostType(item.clone()), sig::ContractSide::Env)
        }
        sig::FrozenTypeItem::TypeAlias(item) => {
            (SigItem::TypeAlias(item.clone()), sig::ContractSide::Export)
        }
        sig::FrozenTypeItem::Newtype(item) => (
            SigItem::Newtype((**item).clone()),
            sig::ContractSide::Export,
        ),
    };
    let add = vec![SigModuleSection {
        leading_trivia: Vec::new(),
        trailing_trivia: Vec::new(),
        path: module_path_of(&name.module_path),
        imports: declaration.imports.clone(),
        items: vec![item],
        span: crate::span::Span::new(0, 0),
    }];
    let changes = SigChangeSet {
        leading_trivia: Vec::new(),
        trailing_trivia: Vec::new(),
        add_leading_trivia: Vec::new(),
        add_trailing_trivia: Vec::new(),
        modify_leading_trivia: Vec::new(),
        modify_trailing_trivia: Vec::new(),
        remove_leading_trivia: Vec::new(),
        remove_trailing_trivia: Vec::new(),
        add,
        add_refs: Vec::new(),
        modify: Vec::new(),
        modify_refs: Vec::new(),
        remove: Vec::new(),
        remove_refs: Vec::new(),
        span: crate::span::Span::new(0, 0),
    };
    let (breaking, nonbreaking) = match side {
        sig::ContractSide::Env => (Some(changes), None),
        sig::ContractSide::Export => (None, Some(changes)),
    };
    sig::emit_signature_comparison(&SignatureFile {
        pkg: "normalized".to_owned(),
        version: 1,
        versions: vec![SigVersion {
            leading_trivia: Vec::new(),
            trailing_trivia: Vec::new(),
            with_leading_trivia: Vec::new(),
            with_trailing_trivia: Vec::new(),
            doc: None,
            version: 1,
            with: Vec::new(),
            breaking,
            nonbreaking,
            span: crate::span::Span::new(0, 0),
        }],
        meta: crate::ast::Meta::new(crate::span::Span::new(0, 0)),
    })
}

fn normalized_frozen_text(item: &sig::RemovedItem) -> String {
    let mut one = sig::ReplayedInterface::default();
    one.live_frozen.push(item.clone());
    let boundary = synthesize_boundary_block(1, &one)
        .expect("one retained item always synthesizes one compact boundary");
    sig::emit_signature_comparison(&SignatureFile {
        pkg: "normalized".to_owned(),
        version: 1,
        versions: vec![boundary],
        meta: crate::ast::Meta::new(crate::span::Span::new(0, 0)),
    })
}

fn contract_side_rank(side: sig::ContractSide) -> u8 {
    match side {
        sig::ContractSide::Env => 0,
        sig::ContractSide::Export => 1,
    }
}

fn module_path_string(path: &crate::ast::ModulePath) -> String {
    path.segments
        .iter()
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

fn module_path_of(path: &str) -> crate::ast::ModulePath {
    let segments = path
        .split('/')
        .map(|s| crate::ast::PathSegment::synth(s, crate::span::Span::new(0, 0)))
        .collect();
    crate::ast::ModulePath {
        segments,
        span: crate::span::Span::new(0, 0),
    }
}

// =========================================================================
// Shared helpers
// =========================================================================

/// Whether the on-disk file's draft block equals the recomputed draft
/// block (the draft already records the live delta).
///
/// The comparison is **span- and order-insensitive**: the on-disk block
/// is parsed (carrying real source spans, and possibly hand-edited /
/// merge-reordered module sections + items) while the recomputed block
/// is synthesized (zero spans, canonical order). The emitter
/// canonicalizes section / item / `import` / remove order (see
/// [`sig::emit_signature_comparison`]) and ignores spans / trivia, so
/// rendering both through it gives a faithful semantic equality.
fn drafts_equal(
    existing: Option<&SignatureFile<Surface>>,
    recomputed: &SignatureFile<Surface>,
) -> bool {
    let version = recomputed.version;
    // The header version must agree (a `commit` changes it, so a sealed
    // file is never "equal" to a draft recomputed at the old version).
    if existing.map(|f| f.version).unwrap_or(version) != version {
        return false;
    }
    let on_disk = existing
        .and_then(|f| draft_block_for(f, version))
        .map(emit_draft_block);
    let computed = draft_block_for(recomputed, version).map(emit_draft_block);
    on_disk == computed
}

/// Whether the on-disk draft's **breaking** change-set and its required
/// version-local declaration context equal the recomputed draft's breaking
/// projection — i.e. every breaking change vs the sealed contract is already
/// acknowledged in the draft.
///
/// A recursive operation contains only an exact name reference; its actual
/// declaration lives in `with`. Comparing the operation set alone would
/// therefore mistake a stale or hand-edited recursive declaration for a
/// recorded break. Conversely, comparing the complete `with` block would let
/// an unrelated later compatible recursive change turn an already-recorded
/// break from status 82 back into 80. `breaking_context_names` closes the
/// breaking targets over their previous and current recursive epochs, and
/// `filter_sig_context` retains that projection plus its type dependencies.
/// The result remains span/order-insensitive through canonical emission, like
/// [`drafts_equal`].
fn breaking_recorded(
    existing: Option<&SignatureFile<Surface>>,
    recomputed: &SignatureFile<Surface>,
) -> bool {
    let version = recomputed.version;
    if existing.map(|f| f.version).unwrap_or(version) != version {
        return false;
    }
    let recomputed_block = draft_block_for(recomputed, version);
    let required_context = match recomputed_block.and_then(|block| block.breaking.as_ref()) {
        Some(_) => match breaking_context_names(recomputed, version) {
            Ok(required) => required,
            Err(_) => return false,
        },
        None => std::collections::BTreeSet::new(),
    };
    let on_disk = existing
        .and_then(|file| draft_block_for(file, version))
        .and_then(|block| emit_breaking_block(block, &required_context));
    let computed = recomputed_block.and_then(|block| emit_breaking_block(block, &required_context));
    on_disk == computed
}

/// Exact declaration names whose version-local context belongs to the
/// breaking projection. A member affected by a split or merge brings along
/// every peer from both its last sealed recursive epoch and its new epoch;
/// this includes unchanged peers whose only job in the version is to replace
/// or clear stale group context.
fn breaking_context_names(
    file: &SignatureFile<Surface>,
    version: u32,
) -> Result<std::collections::BTreeSet<(String, String)>, sig::ReplayError> {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    let Some(block) = draft_block_for(file, version) else {
        return Ok(BTreeSet::new());
    };
    let Some(breaking) = &block.breaking else {
        return Ok(BTreeSet::new());
    };

    let mut required = sig_change_set_names(breaking);
    let mut queue = required.iter().cloned().collect::<VecDeque<_>>();

    let current_groups = recursive_context_index(&block.with);
    let previous_groups = if version > 1 {
        let replayed = sig::replay_through(file, version - 1)?;
        let mut groups = Vec::new();
        let mut identities = BTreeMap::new();
        for item in &replayed.live_frozen {
            let Some(context) = &item.recursive_context else {
                continue;
            };
            let identity = std::sync::Arc::as_ptr(context) as usize;
            identities.entry(identity).or_insert_with(|| {
                let group = groups.len();
                groups.push(sig_context_section_names(context));
                group
            });
        }
        index_context_groups(groups)
    } else {
        RecursiveSigContextIndex::default()
    };

    let mut expanded_current = BTreeSet::new();
    let mut expanded_previous = BTreeSet::new();
    while let Some(name) = queue.pop_front() {
        for (index, expanded) in [
            (&current_groups, &mut expanded_current),
            (&previous_groups, &mut expanded_previous),
        ] {
            let Some(group) = index.by_name.get(&name).copied() else {
                continue;
            };
            if !expanded.insert(group) {
                continue;
            }
            for member in &index.groups[group] {
                if required.insert(member.clone()) {
                    queue.push_back(member.clone());
                }
            }
        }
    }
    Ok(required)
}

#[derive(Default)]
struct RecursiveSigContextIndex {
    by_name: std::collections::BTreeMap<(String, String), usize>,
    groups: Vec<std::collections::BTreeSet<(String, String)>>,
}

fn recursive_context_index(
    sections: &[crate::ast::SigModuleSection<Surface>],
) -> RecursiveSigContextIndex {
    use crate::ast::SigItem;

    let groups = sections
        .iter()
        .flat_map(|section| {
            let module = module_path_str(&section.path);
            section.items.iter().filter_map(move |item| match item {
                SigItem::TypeRecGroup(_)
                | SigItem::Newtype(crate::ast::Newtype {
                    rec_span: Some(_), ..
                }) => Some(
                    sig_context_item_names(item)
                        .into_iter()
                        .map(|name| (module.clone(), name))
                        .collect(),
                ),
                _ => None,
            })
        })
        .collect();
    index_context_groups(groups)
}

fn index_context_groups(
    groups: Vec<std::collections::BTreeSet<(String, String)>>,
) -> RecursiveSigContextIndex {
    let mut by_name = std::collections::BTreeMap::new();
    for (group, members) in groups.iter().enumerate() {
        for member in members {
            by_name.insert(member.clone(), group);
        }
    }
    RecursiveSigContextIndex { by_name, groups }
}

fn sig_context_section_names(
    section: &crate::ast::SigModuleSection<Surface>,
) -> std::collections::BTreeSet<(String, String)> {
    let module = module_path_str(&section.path);
    section
        .items
        .iter()
        .flat_map(sig_context_item_names)
        .map(|name| (module.clone(), name))
        .collect()
}

fn sig_change_set_names(
    set: &crate::ast::SigChangeSet<Surface>,
) -> std::collections::BTreeSet<(String, String)> {
    let mut names = set
        .add_refs
        .iter()
        .chain(&set.modify_refs)
        .chain(&set.remove_refs)
        .map(|reference| (module_path_str(&reference.path), reference.name.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    for section in set.add.iter().chain(&set.modify) {
        let module = module_path_str(&section.path);
        names.extend(
            section
                .items
                .iter()
                .flat_map(sig_context_item_names)
                .map(|name| (module.clone(), name)),
        );
    }
    for remove in &set.remove {
        let module = module_path_str(&remove.path);
        names.extend(
            remove
                .names
                .iter()
                .map(|name| (module.clone(), name.name.clone())),
        );
    }
    names
}

/// Emit a build-time advisory warning when a package's `*.sig.kio`
/// changelog is present **and** the live surface has an **unrecorded
/// breaking** delta against the last sealed contract — the same `80`
/// condition [`status_exit_code`] gates on.
///
/// Called from the `kio check` / `kio build` paths. It is **advisory**:
/// it only writes to stderr and never changes the caller's exit code.
/// It is **silent** when there is no sig file (open-world: a package with
/// no changelog builds identically), when the drift is only compatible,
/// when a break is already recorded (the author acknowledged it), and
/// when the surface is clean.
///
/// `package` is the typechecked root package; `pkg_dir` is its package
/// root (where the `<pkg>.sig.kio` lives). Any I/O / parse / replay
/// hiccup degrades to silence — this is an advisory, not a gate, so it
/// must never break a build that would otherwise succeed.
pub fn warn_if_unrecorded_breaking(package: &Package<crate::ast::Prime>, pkg_dir: &Path) {
    if let Some(lines) = unrecorded_breaking_advisory(package, pkg_dir) {
        for line in lines {
            eprintln!("{line}");
        }
    }
}

/// As [`warn_if_unrecorded_breaking`], but writes the advisory into
/// `buf` (a captured stderr buffer) rather than printing. Used by the
/// `kio build` multi-package fan-out so the advisory replays in input
/// order alongside the package's other diagnostics
/// (`cmd::package_fanout`).
pub fn warn_if_unrecorded_breaking_buffered(
    package: &Package<crate::ast::Prime>,
    pkg_dir: &Path,
    buf: &mut String,
) {
    use std::fmt::Write as _;
    if let Some(lines) = unrecorded_breaking_advisory(package, pkg_dir) {
        for line in lines {
            let _ = writeln!(buf, "{line}");
        }
    }
}

/// Compute the unrecorded-breaking advisory lines, or `None` when the
/// package has no changelog, the drift is compatible, or the break is
/// already recorded (open-world: silent). Shared by the printing and
/// buffered entry points so they never diverge.
fn unrecorded_breaking_advisory(
    package: &Package<crate::ast::Prime>,
    pkg_dir: &Path,
) -> Option<Vec<String>> {
    let package_name = package.package_file().map(|f| f.package_name.clone())?;

    // No changelog ⇒ nothing to gate (open-world: silent).
    let path = sig_path(pkg_dir, &package_name);
    let source = fs::read_to_string(&path).ok()?;
    let existing = crate::pass::parser::parse_signature_file(&source, Some(&package_name)).ok()?;

    let live = ContractSnapshot::from_package(package);
    let recorded = RecordedSurface::from_package(package);
    let plan = sig::compute_draft(&package_name, Some(&existing), &recorded, &live).ok()?;

    // The advisory fires on the same condition as `kio sig status`'s 80
    // (an unrecorded incompatibility): a breaking delta vs the sealed
    // contract that the draft does not already record. A recorded break
    // (82) or a merely-compatible drift (81) does not warn here.
    let break_recorded = breaking_recorded(Some(&existing), &plan.recomputed);
    if !plan.report.is_breaking() || break_recorded {
        return None;
    }

    let mut lines = vec![format!(
        "warning: `{package_name}` breaks its last recorded contract — the live surface drops or narrows a sealed export, or adds a host requirement:"
    )];
    for change in plan.report.breaking() {
        lines.push(format!("  - {}", change.detail));
    }
    lines.push(
        "run `kio sig status` to review, or `kio sig stage --force` to record the break in the changelog"
            .to_owned(),
    );
    Some(lines)
}

/// Warn (on stderr) for every item the previous on-disk draft recorded
/// as an `add` that the freshly-recomputed draft drops entirely — an
/// item added then removed within the open (unsealed) draft before
/// sealing. The net-out itself is correct (the recompute is
/// `diff(sealed, live)`); this advisory just names the churn so it is
/// not silent. It is purely presentational — derived by diffing the two
/// drafts, never from a persisted op-log.
///
/// "Drops entirely" means the name appears in the on-disk draft's `add`
/// sections but is absent from *every* section of the recomputed draft
/// (add / modify / remove). A name that became a `remove` is a sealed
/// item the live surface dropped (the deprecation-carry obligation), not
/// an intra-draft net-out, so it does not warn.
fn warn_intra_draft_net_out(
    existing: Option<&SignatureFile<Surface>>,
    recomputed: &SignatureFile<Surface>,
    cap: &mut CapturedOutput,
) {
    let version = recomputed.version;
    // Only compare within the same open-draft generation. A header
    // mismatch (a just-sealed file) means there is no shared draft to
    // diff.
    if existing.map(|f| f.version).unwrap_or(version) != version {
        return;
    }
    let Some(on_disk) = existing.and_then(|f| draft_block_for(f, version)) else {
        return;
    };

    let previously_added = added_names(on_disk);
    if previously_added.is_empty() {
        return;
    }
    let still_present = match draft_block_for(recomputed, version) {
        Some(block) => all_names(block),
        None => std::collections::BTreeSet::new(),
    };

    let mut netted_out: Vec<&(String, String)> = previously_added
        .iter()
        .filter(|name| !still_present.contains(*name))
        .collect();
    netted_out.sort();
    for (module, leaf) in netted_out {
        cap_errln!(
            cap,
            "warning: `{module}.{leaf}` was added then removed within v({version}) before sealing — the draft records no change for it"
        );
    }
}

/// The `(module_path, leaf)` set of every item in a draft block's `add`
/// sections (across both partitions).
fn added_names(block: &SigVersion<Surface>) -> std::collections::BTreeSet<(String, String)> {
    let mut out = std::collections::BTreeSet::new();
    for set in [block.breaking.as_ref(), block.nonbreaking.as_ref()]
        .into_iter()
        .flatten()
    {
        collect_section_item_names(&set.add, &mut out);
        collect_ref_names(&set.add_refs, &mut out);
    }
    out
}

/// The `(module_path, leaf)` set of every name a draft block mentions in
/// any section — `add`, `modify`, or `remove`, across both partitions.
fn all_names(block: &SigVersion<Surface>) -> std::collections::BTreeSet<(String, String)> {
    let mut out = std::collections::BTreeSet::new();
    for set in [block.breaking.as_ref(), block.nonbreaking.as_ref()]
        .into_iter()
        .flatten()
    {
        collect_section_item_names(&set.add, &mut out);
        collect_section_item_names(&set.modify, &mut out);
        collect_ref_names(&set.add_refs, &mut out);
        collect_ref_names(&set.modify_refs, &mut out);
        for remove in &set.remove {
            let module = module_path_str(&remove.path);
            for name in &remove.names {
                out.insert((module.clone(), name.name.clone()));
            }
        }
        collect_ref_names(&set.remove_refs, &mut out);
    }
    out
}

/// Collect the `(module_path, leaf)` of every item declared in a list of
/// `add` / `modify` module sections.
fn collect_section_item_names(
    sections: &[crate::ast::SigModuleSection<Surface>],
    out: &mut std::collections::BTreeSet<(String, String)>,
) {
    for section in sections {
        let module = module_path_str(&section.path);
        for item in &section.items {
            match item {
                crate::ast::SigItem::TypeRecGroup(group) => {
                    for member in &group.members {
                        let leaf = match member {
                            crate::ast::TypeRecMember::TypeAlias(alias) => &alias.name,
                            crate::ast::TypeRecMember::Newtype(newtype) => &newtype.name,
                            crate::ast::TypeRecMember::Labels(labels, _) => labels
                                .type_alias_name
                                .as_deref()
                                .unwrap_or("<anonymous labels>"),
                        };
                        out.insert((module.clone(), leaf.to_owned()));
                    }
                }
                _ => {
                    out.insert((module.clone(), sig_item_leaf(item).to_owned()));
                }
            }
        }
    }
}

fn collect_ref_names(
    refs: &[crate::ast::SigItemRef],
    out: &mut std::collections::BTreeSet<(String, String)>,
) {
    for reference in refs {
        out.insert((module_path_str(&reference.path), reference.name.clone()));
    }
}

/// The declared name of a [`crate::ast::SigItem`].
fn sig_item_leaf(item: &crate::ast::SigItem<Surface>) -> &str {
    use crate::ast::SigItem;
    match item {
        SigItem::HostType(h) => &h.name,
        SigItem::HostFn(h) => &h.name,
        SigItem::ExportFn(export) => &export.function.name,
        SigItem::TypeAlias(a) => &a.name,
        SigItem::Newtype(n) => &n.name,
        SigItem::TypeRecGroup(group) => group
            .members
            .first()
            .map(|member| match member {
                crate::ast::TypeRecMember::TypeAlias(alias) => alias.name.as_str(),
                crate::ast::TypeRecMember::Newtype(newtype) => newtype.name.as_str(),
                crate::ast::TypeRecMember::Labels(labels, _) => labels
                    .type_alias_name
                    .as_deref()
                    .unwrap_or("<anonymous labels>"),
            })
            .unwrap_or(""),
    }
}

fn module_path_str(p: &crate::ast::ModulePath) -> String {
    p.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// Render a single draft version block to canonical text (span- and
/// trivia-insensitive), for the `drafts_equal` comparison.
fn emit_draft_block(block: &SigVersion<Surface>) -> String {
    let file = SignatureFile {
        pkg: String::new(),
        version: block.version,
        versions: vec![block.clone()],
        meta: crate::ast::Meta::new(crate::span::Span::new(0, 0)),
    };
    sig::emit_signature_comparison(&file)
}

/// Render the breaking partition and just its required `with` context to
/// canonical text for [`breaking_recorded`].
fn emit_breaking_block(
    block: &SigVersion<Surface>,
    required_context: &std::collections::BTreeSet<(String, String)>,
) -> Option<String> {
    let breaking = block.breaking.clone()?;
    let block = SigVersion {
        leading_trivia: block.leading_trivia.clone(),
        trailing_trivia: block.trailing_trivia.clone(),
        with_leading_trivia: block.with_leading_trivia.clone(),
        with_trailing_trivia: block.with_trailing_trivia.clone(),
        doc: None,
        version: 0,
        with: filter_sig_context(&block.with, required_context),
        breaking: Some(breaking),
        nonbreaking: None,
        span: crate::span::Span::new(0, 0),
    };
    Some(emit_draft_block(&block))
}

/// The version block for `version` in `file`, if present.
fn draft_block_for(file: &SignatureFile<Surface>, version: u32) -> Option<&SigVersion<Surface>> {
    file.versions.iter().find(|v| v.version == version)
}

/// Write the recomputed changelog to the package's `*.sig.kio`, or print
/// it to stdout under `--stdout`. `verb` names the action for the
/// success message.
fn write_or_print(
    pkg_dir: &Path,
    package_name: &str,
    file: &SignatureFile<Surface>,
    stdout: bool,
    verb: &str,
    cap: &mut CapturedOutput,
) -> ExitCode {
    let text = sig::emit_signature_file(file);
    write_text_or_print(pkg_dir, package_name, &text, stdout, verb, cap)
}

/// Print or atomically write already-rendered changelog text. Compact
/// uses this path so the exact text it parsed and replay-validated is the
/// text that reaches stdout or disk.
fn write_text_or_print(
    pkg_dir: &Path,
    package_name: &str,
    text: &str,
    stdout: bool,
    verb: &str,
    cap: &mut CapturedOutput,
) -> ExitCode {
    if stdout {
        cap.stdout.push_str(text);
        return ExitCode::Success;
    }
    let path = sig_path(pkg_dir, package_name);
    // The `*.sig.kio` changelog is the committed, no-GC, not-recomputable
    // record of every removed item's frozen signature — a torn write is
    // silent loss of history. Write through the fsync-before-rename
    // atomic helper so a mid-write crash never truncates it.
    match crate::cmd::atomic_write::write_atomic(&path, text.as_bytes()) {
        Ok(()) => {
            cap_outln!(
                cap,
                "kio sig: {verb} contract for `{package_name}` to {}",
                DisplayPath(&path)
            );
            ExitCode::Success
        }
        Err(e) => {
            cap_errln!(cap, "error: cannot write `{}`: {e}", DisplayPath(&path));
            ExitCode::Internal
        }
    }
}

/// The `<pkg>.sig.kio` path in `pkg_dir`.
fn sig_path(pkg_dir: &Path, package_name: &str) -> PathBuf {
    pkg_dir.join(format!("{package_name}.sig.kio"))
}

/// Find the package file in `pkg_dir` and return `(package_name,
/// package_file_path)`.
fn find_package(pkg_dir: &Path, cap: &mut CapturedOutput) -> Result<(String, PathBuf), ExitCode> {
    let mut hits = Vec::new();
    let entries = match fs::read_dir(pkg_dir) {
        Ok(e) => e,
        Err(e) => {
            cap_errln!(cap, "error: cannot read {}: {e}", DisplayPath(pkg_dir));
            return Err(ExitCode::Internal);
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if crate::file_kind::is_package_file(name) {
            hits.push(path);
        }
    }
    match hits.len() {
        0 => {
            cap_errln!(
                cap,
                "error: no `<name>.pkg.kio` in {} — `kio sig` requires a package file at the package root",
                DisplayPath(pkg_dir)
            );
            Err(ExitCode::Build)
        }
        1 => {
            let path = hits.into_iter().next().unwrap();
            let stem = path
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(crate::file_kind::package_stem)
                .unwrap_or("pkg")
                .to_owned();
            Ok((stem, path))
        }
        _ => {
            hits.sort();
            cap_errln!(
                cap,
                "error: multiple `*.pkg.kio` files in {}; expected exactly one",
                DisplayPath(pkg_dir)
            );
            for p in hits {
                cap_errln!(cap, "  {}", DisplayPath(&p));
            }
            Err(ExitCode::Build)
        }
    }
}

/// Load + parse the package's on-disk `*.sig.kio`, or `None` when the
/// file does not exist. The `signature <pkg>` header must match
/// `package_name` (reconciliation #3 — package-name coherence, mirroring
/// the parser's file-stem coherence and module-name coherence).
fn load_sig_file(
    pkg_dir: &Path,
    package_name: &str,
    cap: &mut CapturedOutput,
) -> Result<Option<SignatureFile<Surface>>, ExitCode> {
    let path = sig_path(pkg_dir, package_name);
    let source = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            cap_errln!(cap, "error: cannot read `{}`: {e}", DisplayPath(&path));
            return Err(ExitCode::Internal);
        }
    };
    // The parser's stem check (`Some(package_name)`) is exactly the
    // package-name coherence — the file is named `<package_name>.sig.kio`
    // and the header `signature <pkg>` must equal that stem, which is the
    // package name.
    match crate::pass::parser::parse_signature_file(&source, Some(package_name)) {
        Ok(file) => Ok(Some(file)),
        Err(e) => {
            crate::cmd::check::render_error(&path, &source, &e, &mut cap.stderr);
            Err(ExitCode::Parse)
        }
    }
}

/// Typecheck the package rooted at `pkg_dir`, returning its
/// `Package<Prime>`. The `1x` typecheck cascade short-circuits here,
/// before any sig comparison runs.
fn typecheck_package(
    pkg_dir: &Path,
    cap: &mut CapturedOutput,
) -> Result<Package<crate::ast::Prime>, ExitCode> {
    let workspace =
        crate::cmd::check::compile_workspace_at_buffered(pkg_dir, false, false, &mut cap.stderr)?;
    // `skip_ok = false` always materializes the root.
    workspace.root_package.ok_or(ExitCode::Internal)
}

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::pass::parser::parse_signature_file;

    fn parse(src: &str) -> SignatureFile<Surface> {
        parse_signature_file(src, None).unwrap_or_else(|e| panic!("parse {src:?}: {e:?}"))
    }

    #[test]
    fn signature_comparisons_ignore_comments_but_preserve_contract_changes() {
        let source = |note: &str, result: &str| {
            format!(
                "// {note} header\n\
             signature app v(1);\n\
             /// {note} version\n\
             v(1) {{ // {note} partition\n\
               nonbreaking {{ // {note} operation\n\
                 add {{ // {note} module\n\
                   module api {{\n\
                     /// {note} type\n\
                     pub newtype Token : . {{\n\
                       // {note} constructor\n\
                       pub constructor make;\n\
                       // {note} projector\n\
                       pub projector take\n\
                     }};\n\
                     type Pair[A] = A & A;\n\
                     type Twice = Pair(\n\
                       // {note} argument\n\
                       Token\n\
                     );\n\
                     // {note} function\n\
                     host fn consume(value: Token) -> {result}\n\
                     // {note} closer\n\
                   }}\n\
                 }}\n\
               }}\n\
             }}\n\
             // {note} end\n"
            )
        };
        let left = parse(&source("left", "."));
        let right = parse(&source("right", "."));
        let changed = parse(&source("right", "Token"));
        assert_ne!(
            sig::emit_signature_file(&left),
            sig::emit_signature_file(&right)
        );
        assert!(drafts_equal(Some(&left), &right));
        assert!(!drafts_equal(Some(&left), &changed));
        let normalize = |file| normalized_replay_state(&sig::replay(file).unwrap());
        assert_eq!(normalize(&left), normalize(&right));
        assert_ne!(normalize(&left), normalize(&changed));
        // Comparison rendering never erases presentation on the source AST.
        assert!(sig::emit_signature_file(&left).contains("left argument"));
    }

    fn replay_src(src: &str) -> sig::ReplayedInterface {
        sig::replay(&parse(src)).unwrap_or_else(|e| panic!("replay {src:?}: {e:?}"))
    }

    fn frozen_item<'a>(
        items: &'a [sig::RemovedItem],
        module_path: &str,
        leaf: &str,
    ) -> &'a sig::RemovedItem {
        items
            .iter()
            .find(|item| item.entry.name.module_path == module_path && item.entry.name.leaf == leaf)
            .unwrap_or_else(|| panic!("missing frozen item `{module_path}.{leaf}`"))
    }

    fn normalized_live_closure(
        src: &str,
        module_path: &str,
        leaf: &str,
    ) -> Option<Vec<(QualifiedName, String)>> {
        let replayed = replay_src(src);
        frozen_item(&replayed.live_frozen, module_path, leaf)
            .frozen_type_closure
            .as_ref()
            .map(normalized_frozen_type_closure)
    }

    // -- status_exit_code precedence ------------------------------------
    //
    // `status_exit_code` is the CI gate's verdict (80 > 82 > 81 > 0). It
    // takes the on-disk changelog and a recomputed draft plan; the four
    // branches are driven below directly, without standing up a package
    // on disk, by pairing a synthetic `CompatReport` with hand-built
    // on-disk + recomputed `SignatureFile`s. A `*.sig.kio` source is
    // never order- or span-sensitive here because `drafts_equal` /
    // `breaking_recorded` route both sides through the canonicalizing
    // emitter.

    use crate::sig::{
        Change as SigChange, ChangePlacement, CompatReport, DraftPlan, QualifiedName, Verdict,
    };

    /// A one-change report (`module.leaf`) with the given verdict +
    /// placement, enough to flip the `is_breaking()` / non-empty gates
    /// `status_exit_code` keys on.
    fn report(verdict: Verdict, placement: ChangePlacement) -> CompatReport {
        CompatReport {
            changes: vec![SigChange {
                name: QualifiedName::new("api", "serve"),
                placement,
                verdict,
                detail: "synthetic".to_owned(),
            }],
        }
    }

    fn plan(report: CompatReport, recomputed: SignatureFile<Surface>) -> DraftPlan {
        DraftPlan { report, recomputed }
    }

    /// A sealed baseline whose v(1) records the export `api.serve`, with
    /// the header open at v(2) and no v(2) draft block.
    fn sealed_v1() -> SignatureFile<Surface> {
        parse(
            "\
signature app v(2);

v(1) {
  nonbreaking {
    add {
      module api {
        pub fn serve() -> .;
      }
    }
  }
}
",
        )
    }

    /// `sealed_v1` plus an open v(2) draft recording the break: `serve`
    /// removed (breaking) and `other` added (compatible).
    fn recorded_break() -> SignatureFile<Surface> {
        parse(
            "\
signature app v(2);

v(1) {
  nonbreaking {
    add {
      module api {
        pub fn serve() -> .;
      }
    }
  }
}

v(2) {
  breaking {
    remove {
      module api {
        serve;
      }
    }
  };
  nonbreaking {
    add {
      module api {
        pub fn other() -> .;
      }
    }
  }
}
",
        )
    }

    /// `sealed_v1` plus an open v(2) draft recording only a compatible
    /// add (`other`), no breaking section.
    fn recorded_compatible() -> SignatureFile<Surface> {
        parse(
            "\
signature app v(2);

v(1) {
  nonbreaking {
    add {
      module api {
        pub fn serve() -> .;
      }
    }
  }
}

v(2) {
  nonbreaking {
    add {
      module api {
        pub fn other() -> .;
      }
    }
  }
}
",
        )
    }

    /// Branch 1 — a breaking delta vs the sealed contract that the
    /// on-disk draft does NOT record (its `breaking` section differs from
    /// the recomputed one) reports 80.
    #[test]
    fn status_exit_code_unrecorded_break_is_80() {
        let existing = sealed_v1();
        let recomputed = recorded_break();
        let p = plan(
            report(Verdict::Breaking, ChangePlacement::Removed),
            recomputed,
        );
        assert_eq!(
            status_exit_code(Some(&existing), &p),
            ExitCode::SigIncompatible
        );
    }

    /// Branch 2 — the break IS recorded (the on-disk and recomputed
    /// `breaking` sections match) and unsealed reports 82, outranking any
    /// residual compatible drift.
    #[test]
    fn status_exit_code_recorded_unsealed_break_is_82() {
        let existing = recorded_break();
        let recomputed = recorded_break();
        let p = plan(
            report(Verdict::Breaking, ChangePlacement::Removed),
            recomputed,
        );
        assert_eq!(
            status_exit_code(Some(&existing), &p),
            ExitCode::SigUnsealedBreak
        );
    }

    /// Recursive operations carry only exact names, so equal operation sets
    /// do not record a break when the declaration in `with` is stale.
    #[test]
    fn status_exit_code_stale_breaking_recursive_context_is_80() {
        let existing = parse(
            "\
signature app v(1);

v(1) {
  with {
    module api {
      rec {
        pub type A = B;
        pub newtype B : A { pub constructor mk_b; pub projector un_b; };
      }
    }
  };
  breaking { add { api.A; api.B; } }
}
",
        );
        let recomputed = parse(
            "\
signature app v(1);

v(1) {
  with {
    module api {
      rec {
        pub type A = B | B;
        pub newtype B : A { pub constructor mk_b; pub projector un_b; };
      }
    }
  };
  breaking { add { api.A; api.B; } }
}
",
        );
        let p = plan(
            report(Verdict::Breaking, ChangePlacement::Added),
            recomputed,
        );
        assert_eq!(
            status_exit_code(Some(&existing), &p),
            ExitCode::SigIncompatible
        );
    }

    /// Compatible recursive work elsewhere in the same draft does not make
    /// an already-recorded breaking group look unrecorded.
    #[test]
    fn status_exit_code_ignores_unrelated_compatible_recursive_context() {
        let existing = parse(
            "\
signature app v(1);

v(1) {
  with {
    module api {
      rec {
        pub type A = B;
        pub newtype B : A { pub constructor mk_b; pub projector un_b; };
      };
      rec {
        pub type C = D;
        pub newtype D : C { pub constructor mk_d; pub projector un_d; };
      }
    }
  };
  breaking { add { api.A; api.B; } };
  nonbreaking { add { api.C; api.D; } }
}
",
        );
        let recomputed = parse(
            "\
signature app v(1);

v(1) {
  with {
    module api {
      rec {
        pub type A = B;
        pub newtype B : A { pub constructor mk_b; pub projector un_b; };
      };
      rec {
        pub type C = D | D;
        pub newtype D : C { pub constructor mk_d; pub projector un_d; };
      }
    }
  };
  breaking { add { api.A; api.B; } };
  nonbreaking { add { api.C; api.D; } }
}
",
        );
        let p = plan(
            report(Verdict::Breaking, ChangePlacement::Added),
            recomputed,
        );
        assert_eq!(
            status_exit_code(Some(&existing), &p),
            ExitCode::SigUnsealedBreak
        );
    }

    /// When a breaking member leaves a sealed recursive group, the unchanged
    /// peers' replacement context epoch is part of recording that break even
    /// though those peers have no operation entries of their own.
    #[test]
    fn status_exit_code_requires_context_for_unchanged_split_peers() {
        let existing = parse(
            "\
signature app v(2);

v(1) {
  with {
    module api {
      rec {
        pub type A = B | C;
        pub newtype B : A { pub constructor mk_b; pub projector un_b; };
        pub newtype C : A { pub constructor mk_c; pub projector un_c; };
      }
    }
  };
  nonbreaking { add { api.A; api.B; api.C; } }
}

v(2) {
  breaking {
    modify {
      module api {
        pub newtype C : . { pub constructor mk_c; pub projector un_c; };
      }
    }
  }
}
",
        );
        let recomputed = parse(
            "\
signature app v(2);

v(1) {
  with {
    module api {
      rec {
        pub type A = B | C;
        pub newtype B : A { pub constructor mk_b; pub projector un_b; };
        pub newtype C : A { pub constructor mk_c; pub projector un_c; };
      }
    }
  };
  nonbreaking { add { api.A; api.B; api.C; } }
}

v(2) {
  with {
    module api {
      rec {
        pub type A = B | C;
        pub newtype B : A { pub constructor mk_b; pub projector un_b; };
      }
    }
  };
  breaking {
    modify {
      module api {
        pub newtype C : . { pub constructor mk_c; pub projector un_c; };
      }
    }
  }
}
",
        );
        let p = plan(
            report(Verdict::Breaking, ChangePlacement::Modified),
            recomputed,
        );
        assert_eq!(
            status_exit_code(Some(&existing), &p),
            ExitCode::SigIncompatible
        );
    }

    /// Branch 3 — an unrecorded *compatible* drift (the on-disk draft
    /// lacks the recomputed compatible add, but there is no breaking
    /// change) reports 81.
    #[test]
    fn status_exit_code_unrecorded_compatible_is_81() {
        let existing = sealed_v1();
        let recomputed = recorded_compatible();
        let p = plan(
            report(Verdict::Compatible, ChangePlacement::Added),
            recomputed,
        );
        assert_eq!(status_exit_code(Some(&existing), &p), ExitCode::SigStale);
    }

    /// Branch 4 — a fully-reconciled draft (the on-disk draft equals the
    /// recomputed one and no breaking section is open) reports 0.
    #[test]
    fn status_exit_code_reconciled_is_0() {
        let existing = recorded_compatible();
        let recomputed = recorded_compatible();
        // Report empty: the recorded compatible add nets against the
        // sealed baseline, so nothing is pending.
        let p = plan(CompatReport::default(), recomputed);
        assert_eq!(status_exit_code(Some(&existing), &p), ExitCode::Success);
    }

    // -- run() arg / flag validation ------------------------------------
    //
    // The Usage-error arms of `run()` all return before any package
    // resolution / filesystem access (the dispatch loop and the post-loop
    // flag guards), so calling `run` with the offending argv exercises
    // the arg contract without standing up a package on disk.

    #[test]
    fn run_compact_missing_version_is_usage() {
        assert_eq!(run(&["compact".to_owned()]), ExitCode::Usage);
    }

    #[test]
    fn run_compact_non_integer_version_is_usage() {
        assert_eq!(
            run(&["compact".to_owned(), "x".to_owned()]),
            ExitCode::Usage
        );
    }

    #[test]
    fn run_compact_zero_version_is_usage() {
        assert_eq!(
            run(&["compact".to_owned(), "0".to_owned()]),
            ExitCode::Usage
        );
    }

    #[test]
    fn run_stdout_on_status_is_usage() {
        assert_eq!(
            run(&["status".to_owned(), "--stdout".to_owned()]),
            ExitCode::Usage
        );
    }

    #[test]
    fn run_stdout_on_log_is_usage() {
        assert_eq!(
            run(&["log".to_owned(), "--stdout".to_owned()]),
            ExitCode::Usage
        );
    }

    #[test]
    fn run_unknown_flag_is_usage() {
        assert_eq!(run(&["--frobnicate".to_owned()]), ExitCode::Usage);
    }

    /// An explicit package-path positional scopes resolution to that
    /// package's directory: a `*.pkg.kio` file selector resolves to its
    /// parent dir, and a directory selector resolves to itself.
    #[test]
    fn resolve_package_dirs_scopes_to_explicit_selector() {
        let tmp = std::env::temp_dir().join(format!(
            "kio-sig-selector-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let pkg_dir = tmp.join("pkg");
        std::fs::create_dir_all(&pkg_dir).expect("mkdir pkg");
        let pkg_file = pkg_dir.join("app.pkg.kio");
        std::fs::write(&pkg_file, "package app;\n").expect("write pkg file");

        // A `*.pkg.kio` file selector resolves to its holding directory.
        let by_file = resolve_package_dirs(&tmp, &[pkg_file.to_string_lossy().into_owned()])
            .expect("by file");
        assert_eq!(by_file.len(), 1, "one selector -> one package dir");
        assert_eq!(
            fs::canonicalize(&by_file[0]).unwrap(),
            fs::canonicalize(&pkg_dir).unwrap(),
            "the file selector scopes to its parent package dir"
        );

        // A directory selector resolves to itself.
        let by_dir =
            resolve_package_dirs(&tmp, &[pkg_dir.to_string_lossy().into_owned()]).expect("by dir");
        assert_eq!(by_dir.len(), 1);
        assert_eq!(
            fs::canonicalize(&by_dir[0]).unwrap(),
            fs::canonicalize(&pkg_dir).unwrap()
        );

        // A non-`*.pkg.kio` existing file is an input error, not a silent
        // accept.
        let note = tmp.join("notes.txt");
        std::fs::write(&note, "notes\n").expect("write note");
        assert_eq!(
            resolve_package_dirs(&tmp, &[note.to_string_lossy().into_owned()]),
            Err(ExitCode::Build)
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn status_rejects_malformed_sealed_history_before_compatibility() {
        let tmp = std::env::temp_dir().join(format!(
            "kio-sig-invalid-history-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).expect("create package directory");
        std::fs::write(
            tmp.join("app.pkg.kio"),
            "package app;\n\nbridge {\n  api;\n}\n",
        )
        .expect("write package file");
        std::fs::write(
            tmp.join("api.kio"),
            "module api;\n\npub fn live() -> . { () }\n",
        )
        .expect("write live module");
        std::fs::write(
            tmp.join("app.sig.kio"),
            r#"signature app v(2);

v(1) {
  with {
    module api {
      pub rec newtype Bad : (Bad -> .) { pub constructor mk; pub projector un; };
    }
  };
  nonbreaking { add { api.Bad; } }
}
"#,
        )
        .expect("write malformed sealed history");

        let mut output = CapturedOutput::new();
        let code = run_one(
            &tmp,
            &SigCommand::Status,
            false,
            None,
            None,
            &LogFilter::default(),
            &mut output,
        );
        assert_eq!(code, ExitCode::Build, "{}", output.stderr);
        assert!(
            output.stderr.contains("replaying the sealed history"),
            "{}",
            output.stderr
        );
        assert!(
            output.stderr.contains("strictly positive"),
            "{}",
            output.stderr
        );
        assert!(
            !output.stderr.contains("breaks the last sealed contract"),
            "{}",
            output.stderr
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The compact replay-equivalence invariant: a compacted additive
    /// prefix reproduces the complete normalized replay state, while
    /// suffix removals stay at their original generation. The two `app`
    /// function pairs deliberately resolve the same local `Token` head
    /// through different origin imports; merging those import
    /// environments in the boundary would change a frozen signature.
    /// The two removals are written in noncanonical order so validating
    /// only the pre-render AST would miss the exact output's reordered suffix
    /// text.
    #[test]
    fn compact_preserves_full_replay_and_suffix_removal_generation() {
        let src = "\
signature app v(3);

v(1) {
  breaking {
    add {
      module app {
        import left(Token);
        host fn boot_left(x: Token) -> Token;
      };
      module left {
        host type Token role(i32);
      }
    }
  };
  nonbreaking {
    add {
      module app {
        import left(Token);
        pub fn from_left(x: Token) -> Token;
      }
    }
  }
}

v(2) {
  breaking {
    add {
      module right {
        host type Token role(i32);
      };
      module app {
        import right(Token);
        host fn boot_right(x: Token) -> Token;
      }
    }
  };
  nonbreaking {
    add {
      module app {
        import right(Token);
        pub fn from_right(x: Token) -> Token;
      }
    }
  }
}

/// Retire both old boot hooks without changing their introductions.
v(3) {
  nonbreaking {
    remove {
      module app {
        boot_right;
        boot_left;
      }
    }
  }
}
";
        let file = parse(src);
        let source_remove = &file.versions[2]
            .nonbreaking
            .as_ref()
            .expect("v(3) nonbreaking changes")
            .remove[0];
        assert_eq!(
            source_remove
                .names
                .iter()
                .map(|name| name.name.as_str())
                .collect::<Vec<_>>(),
            vec!["boot_right", "boot_left"],
            "the source removal order is intentionally noncanonical"
        );
        let pre = sig::replay(&file).expect("replay before compact");

        // Collapse v(1)..v(2) into a v(2) boundary. The original v(3)
        // suffix operations stay at v(3), while canonical emission sorts
        // their presentation. `compact_file` returns the exact text it
        // already parsed and replay-validated.
        let text = compact_file(&file, "app", 3).expect("compact");
        let left_remove = text.find("\n      app.boot_left;\n").expect("left removal");
        let right_remove = text
            .find("\n      app.boot_right\n")
            .expect("right removal");
        assert!(
            left_remove < right_remove,
            "canonical output must reorder the noncanonical suffix:\n{text}"
        );
        let compacted = parse_signature_file(&text, Some("app")).unwrap_or_else(|e| {
            panic!("validated compact text must reparse:\n{text}\nerror: {e:?}")
        });
        assert_eq!(compacted.versions.len(), 2);
        assert_eq!(compacted.versions[0].version, 2);
        assert_eq!(compacted.versions[1].version, 3);
        assert_eq!(
            compacted.versions[1].doc.as_ref().map(|doc| &doc.lines),
            file.versions[2].doc.as_ref().map(|doc| &doc.lines),
            "canonical emission keeps the suffix message content"
        );

        let post = sig::replay(&compacted).expect("replay exact compact output");
        assert_eq!(
            post.removed
                .iter()
                .map(|item| item.entry.name.leaf.as_str())
                .collect::<Vec<_>>(),
            vec!["boot_left", "boot_right"],
            "the exact rendered artifact is the replayed artifact"
        );
        assert_eq!(
            normalized_replay_state(&pre),
            normalized_replay_state(&post),
            "the exact rendered artifact must preserve current state, every frozen declaration and origin import, and every removal generation\ncompacted text:\n{text}"
        );
        let removed = post
            .removed
            .iter()
            .filter(|item| item.entry.name.module_path == "app")
            .collect::<Vec<_>>();
        assert_eq!(removed.len(), 2);
        assert!(
            removed.iter().all(|item| item.removed_at_version == 3),
            "both suffix removals keep generation v(3)"
        );
    }

    #[test]
    fn compact_preserves_recursive_context_as_one_later_boundary_group() {
        let file = parse(
            "\
signature app v(3);

v(1) {
  with {
    module api {
      rec {
        pub type A = B;
        pub newtype B : A { pub constructor mk_b; pub projector un_b; };
      }
    }
  };
  nonbreaking { add { api.A; api.B; } }
}

v(2) {
  nonbreaking {
    add { module api { pub fn use_b(value: B) -> A; } }
  }
}
",
        );
        let before = normalized_replay_state(&sig::replay(&file).expect("replay before compact"));

        let text = compact_file(&file, "app", 3).expect("recursive additive history compacts");
        assert!(text.contains("v(2) {\n  with {"), "{text}");
        assert_eq!(text.matches("      rec {").count(), 1, "{text}");
        assert!(text.contains("api.A;"), "{text}");
        assert!(text.contains("api.B;"), "{text}");

        let compacted = parse_signature_file(&text, Some("app")).unwrap_or_else(|error| {
            panic!("recursive compact text must reparse: {error:?}\n{text}")
        });
        let after = normalized_replay_state(
            &sig::replay(&compacted).expect("replay recursive compact boundary"),
        );
        assert_eq!(before, after, "{text}");
    }

    #[test]
    fn compact_keeps_same_module_recursive_origin_imports_separate() {
        let file = parse(
            "\
signature app v(3);

v(1) {
  with {
    module api {
      import left as dep;
      pub rec newtype Left : dep.Token | Left {
        pub constructor mk_left;
        pub projector un_left;
      };
    }
  };
  breaking { add { module left { host type Token; } } };
  nonbreaking { add { api.Left; } }
}

v(2) {
  with {
    module api {
      import right as dep;
      pub rec newtype Right : dep.Token | Right {
        pub constructor mk_right;
        pub projector un_right;
      };
    }
  };
  breaking { add { module right { host type Token; } } };
  nonbreaking { add { api.Right; } }
}
",
        );
        let before = normalized_replay_state(&sig::replay(&file).expect("replay before compact"));

        let text = compact_file(&file, "app", 3)
            .expect("distinct recursive origin imports must survive compaction");
        assert_eq!(text.matches("    module api {").count(), 2, "{text}");
        assert_eq!(
            text.matches("      import left as dep;").count(),
            1,
            "{text}"
        );
        assert_eq!(
            text.matches("      import right as dep;").count(),
            1,
            "{text}"
        );
        assert_eq!(text.matches("pub rec newtype Left").count(), 1, "{text}");
        assert_eq!(text.matches("pub rec newtype Right").count(), 1, "{text}");

        let compacted = parse_signature_file(&text, Some("app")).unwrap_or_else(|error| {
            panic!("separated recursive origins must reparse: {error:?}\n{text}")
        });
        let after = normalized_replay_state(
            &sig::replay(&compacted).expect("replay separated recursive origins"),
        );
        assert_eq!(before, after, "{text}");
    }

    #[test]
    fn compact_preserves_recursive_singleton_context_at_the_boundary() {
        let file = parse(
            "\
signature app v(3);

v(1) {
  with {
    module api {
      pub rec newtype List[A] : . | (A & List(A)) {
        pub constructor mk_list;
        pub projector un_list;
      };
    }
  };
  nonbreaking { add { api.List; } }
}

v(2) {
  nonbreaking {
    add { module api { pub fn empty[A]() -> List(A); } }
  }
}
",
        );
        let before = normalized_replay_state(&sig::replay(&file).expect("replay before compact"));

        let text = compact_file(&file, "app", 3).expect("recursive singleton history compacts");
        assert!(text.contains("v(2) {\n  with {"), "{text}");
        assert_eq!(text.matches("rec newtype List").count(), 1, "{text}");
        assert!(text.contains("api.List;"), "{text}");

        let compacted = parse_signature_file(&text, Some("app")).unwrap_or_else(|error| {
            panic!("recursive singleton compact text must reparse: {error:?}\n{text}")
        });
        let after = normalized_replay_state(
            &sig::replay(&compacted).expect("replay recursive singleton compact boundary"),
        );
        assert_eq!(before, after, "{text}");
    }

    #[test]
    fn breaking_log_filter_keeps_only_groups_supporting_surviving_refs() {
        let file = parse(
            "\
signature app v(2);

v(1) {
  nonbreaking { add { module api { pub fn seed() -> .; } } }
}

v(2) {
  with {
    module api {
      rec {
        pub type A = B | C;
        pub newtype B : A { pub constructor mk_b; pub projector un_b; };
      };
      rec {
        pub type C = D;
        pub newtype D : C { pub constructor mk_d; pub projector un_d; };
      };
      rec {
        pub type E = F;
        pub newtype F : E { pub constructor mk_f; pub projector un_f; };
      }
    }
  };
  breaking { modify { api.A; } };
  nonbreaking { modify { api.E; } }
}
",
        );
        let filtered = filter_changelog(
            &file,
            &LogFilter {
                breaking: true,
                since: None,
            },
        );
        assert_eq!(filtered.versions.len(), 1);
        let text = sig::emit_signature_file(&filtered);
        assert!(text.contains("pub type A = B | C;"), "{text}");
        assert!(text.contains("pub newtype B : A"), "{text}");
        assert!(text.contains("pub type C = D;"), "{text}");
        assert!(text.contains("pub newtype D : C"), "{text}");
        assert!(!text.contains("pub type E = F;"), "{text}");
        assert!(!text.contains("pub newtype F : E"), "{text}");
        assert!(text.contains("\n      api.A\n"), "{text}");
        assert!(!text.contains("api.E"), "{text}");
    }

    #[test]
    fn breaking_log_filter_keeps_unchanged_peers_from_a_split_epoch() {
        let file = parse(
            "\
signature app v(2);

v(1) {
  with {
    module api {
      rec {
        pub type A = B | C;
        pub newtype B : A { pub constructor mk_b; pub projector un_b; };
        pub newtype C : A { pub constructor mk_c; pub projector un_c; };
      }
    }
  };
  nonbreaking { add { api.A; api.B; api.C; } }
}

v(2) {
  with {
    module api {
      rec {
        pub type A = B | C;
        pub newtype B : A { pub constructor mk_b; pub projector un_b; };
      }
    }
  };
  breaking {
    modify {
      module api {
        pub newtype C : . { pub constructor mk_c; pub projector un_c; };
      }
    }
  }
}
",
        );
        let filtered = filter_changelog(
            &file,
            &LogFilter {
                breaking: true,
                since: None,
            },
        );
        let text = sig::emit_signature_file(&filtered);
        assert_eq!(filtered.versions.len(), 1, "{text}");
        assert!(text.contains("pub type A = B | C;"), "{text}");
        assert!(text.contains("pub newtype B : A"), "{text}");
        assert!(text.contains("pub newtype C : ."), "{text}");
    }

    /// Compact validates the historical origin before attempting to collapse
    /// it with a later declaration that happens to satisfy the missing name.
    #[test]
    fn compact_rejects_missing_historical_type_before_future_rebinding() {
        let file = parse(
            "\
signature app v(3);

v(1) {
  breaking {
    add {
      module api {
        import api as local;
        host fn consume(x: local.Later) -> .;
      }
    }
  }
}

v(2) {
  breaking {
    add {
      module api {
        host type Later;
      }
    }
  }
}
",
        );
        let message = compact_file(&file, "app", 3)
            .expect_err("malformed sealed history must fail before compaction")
            .diag()
            .1
            .to_owned();
        assert!(
            message.contains("does not resolve to a declaration in this signature version"),
            "got: {message}"
        );
    }

    /// Closure equality ignores source spans, module-section order, and the
    /// presentation order of declaration-local imports. The normalized list
    /// remains qualified-name sorted even when the source is not.
    #[test]
    fn frozen_closure_normalization_ignores_spans_and_import_order() {
        let left = replay_src(
            "\
signature app v(3);

v(1) {
  nonbreaking {
    add {
      module right {
        host type Right;
      };
      module left {
        host type Left;
      };
      module types {
        import left(Left);
        import right(Right);
        type Pair = Left & Right;
      };
      module api {
        import types(Pair);
        host fn consume(x: Pair) -> .;
      }
    }
  }
}

v(2) {
  nonbreaking {
    remove {
      module api {
        consume;
      }
    }
  }
}
",
        );
        let right = replay_src(
            "\
signature app v(2);


v(1) {
  nonbreaking {
    add {
      module api {
        import types(Pair);
        host fn consume(x: Pair) -> .;
      };
      module types {
        import right(Right);
        import left(Left);
        type Pair = Left & Right;
      };
      module left {
        host type Left;
      };
      module right {
        host type Right;
      }
    }
  }
}

v(2) {
  nonbreaking {
    remove {
      module api {
        consume;
      }
    }
  }
}
",
        );

        let left_normalized = normalized_frozen_items(&left.removed);
        let right_normalized = normalized_frozen_items(&right.removed);
        assert_eq!(left_normalized, right_normalized);
        let closure = left_normalized[0]
            .frozen_type_closure
            .as_ref()
            .expect("retained host root closure");
        assert_eq!(
            closure
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>(),
            vec![
                QualifiedName::new("left", "Left"),
                QualifiedName::new("right", "Right"),
                QualifiedName::new("types", "Pair"),
            ],
            "closure normalization is deterministically qualified-name sorted"
        );
    }

    /// The same retained host-function spelling can close over a different
    /// historical nominal graph when one declaration-local import changes.
    /// Canonicalization must retain that semantic difference.
    #[test]
    fn frozen_closure_normalization_distinguishes_changed_local_import() {
        let with_alias_import = |origin: &str| {
            replay_src(&format!(
                "\
signature app v(2);

v(1) {{
  nonbreaking {{
    add {{
      module left {{
        host type Raw;
      }};
      module right {{
        host type Raw;
      }};
      module types {{
        import {origin}(Raw);
        type Wrapped = Raw;
      }};
      module api {{
        import left as left_types;
        import right as right_types;
        import types(Wrapped);
        host fn consume(wrapped: Wrapped, left_raw: left_types.Raw, right_raw: right_types.Raw) -> .;
      }}
    }}
  }}
}}

v(2) {{
  nonbreaking {{
    remove {{
      module api {{
        consume;
      }}
    }}
  }}
}}
"
            ))
        };
        let left = normalized_frozen_items(&with_alias_import("left").removed);
        let right = normalized_frozen_items(&with_alias_import("right").removed);

        assert_eq!(left.len(), 1);
        assert_eq!(right.len(), 1);
        assert_eq!(left[0].entry, right[0].entry);
        assert_eq!(left[0].frozen_with_imports, right[0].frozen_with_imports);
        assert_eq!(left[0].removed_at_version, right[0].removed_at_version);
        let left_closure = left[0]
            .frozen_type_closure
            .as_ref()
            .expect("left retained host closure");
        let right_closure = right[0]
            .frozen_type_closure
            .as_ref()
            .expect("right retained host closure");
        let left_names = left_closure
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let right_names = right_closure
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        assert_eq!(left_names, right_names);
        assert_eq!(
            left_names,
            vec![
                QualifiedName::new("left", "Raw"),
                QualifiedName::new("right", "Raw"),
                QualifiedName::new("types", "Wrapped"),
            ],
            "both roots independently reach both Raw declarations"
        );
        let differing_declarations = left_closure
            .iter()
            .zip(right_closure)
            .filter_map(|((left_name, left_text), (right_name, right_text))| {
                assert_eq!(left_name, right_name);
                (left_text != right_text).then_some(left_name.clone())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            differing_declarations,
            vec![QualifiedName::new("types", "Wrapped")],
            "only Wrapped's declaration-local frozen import changes"
        );
        let left_wrapped = &left_closure
            .iter()
            .find(|(name, _)| name == &QualifiedName::new("types", "Wrapped"))
            .expect("left Wrapped declaration")
            .1;
        let right_wrapped = &right_closure
            .iter()
            .find(|(name, _)| name == &QualifiedName::new("types", "Wrapped"))
            .expect("right Wrapped declaration")
            .1;
        assert!(left_wrapped.contains("import left(Raw);"));
        assert!(right_wrapped.contains("import right(Raw);"));
        assert_ne!(left[0].frozen_type_closure, right[0].frozen_type_closure);
        assert_ne!(
            left, right,
            "retained-item equality includes each root's frozen type closure"
        );
    }

    /// A closure member's qualified name alone is not its identity: the
    /// historical declaration kind and body are part of the canonical form.
    #[test]
    fn frozen_closure_normalization_distinguishes_all_type_declaration_kinds() {
        let host_type = normalized_live_closure(
            "\
signature app v(1);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Token;
        host fn consume(x: Token) -> .;
      }
    }
  }
}
",
            "api",
            "consume",
        )
        .expect("host root closure");
        let type_alias = normalized_live_closure(
            "\
signature app v(1);
v(1) {
  nonbreaking {
    add {
      module api {
        type Token = .;
        host fn consume(x: Token) -> .;
      }
    }
  }
}
",
            "api",
            "consume",
        )
        .expect("host root closure");
        let newtype = normalized_live_closure(
            "\
signature app v(1);
v(1) {
  nonbreaking {
    add {
      module api {
        newtype Token : . { pub constructor make_token; pub projector un_token; };
        host fn consume(x: Token) -> .;
      }
    }
  }
}
",
            "api",
            "consume",
        )
        .expect("host root closure");

        for closure in [&host_type, &type_alias, &newtype] {
            assert_eq!(closure.len(), 1);
            assert_eq!(closure[0].0, QualifiedName::new("api", "Token"));
        }
        assert_ne!(host_type, type_alias);
        assert_ne!(host_type, newtype);
        assert_ne!(type_alias, newtype);
    }

    /// A cut after the current header would fold the open compatible
    /// draft into a boundary at that same open generation. That moves a
    /// sealed host requirement into the draft's `breaking` partition and
    /// turns a reconciled staged state from exit 0 into exit 82.
    #[test]
    fn compact_rejects_cut_past_header_before_staged_state_becomes_break() {
        let file = parse(
            "\
signature app v(2);

v(1) {
  breaking {
    add {
      module api {
        host fn need() -> .;
      }
    }
  }
}

v(2) {
  nonbreaking {
    add {
      module api {
        pub fn provide() -> .;
      }
    }
  }
}
",
        );

        let clean_plan = plan(
            report(Verdict::Compatible, ChangePlacement::Added),
            file.clone(),
        );
        assert_eq!(
            status_exit_code(Some(&file), &clean_plan),
            ExitCode::Success,
            "the compatible staged export is fully reconciled"
        );

        // Reconstruct the former header+1 result to pin the failure's
        // cause: both live items became a v(2) boundary, leaving the
        // sealed v(1) baseline empty and marking `need` as an open break.
        let replayed = sig::replay_through(&file, file.version).expect("replay through draft");
        let boundary = synthesize_boundary_block(file.version, &replayed)
            .expect("the live interface produces a boundary");
        let formerly_compacted = SignatureFile {
            pkg: "app".to_owned(),
            version: file.version,
            versions: vec![boundary],
            meta: crate::ast::Meta::new(crate::span::Span::new(0, 0)),
        };
        assert_ne!(
            normalized_replay_state(
                &sig::replay_through(&file, file.version - 1)
                    .expect("sealed replay before compaction")
            ),
            normalized_replay_state(
                &sig::replay_through(&formerly_compacted, file.version - 1)
                    .expect("sealed replay after former compaction")
            ),
            "the former header+1 cut changed the sealed baseline"
        );
        let misclassified_plan = plan(
            report(Verdict::Breaking, ChangePlacement::Added),
            formerly_compacted.clone(),
        );
        assert_eq!(
            status_exit_code(Some(&formerly_compacted), &misclassified_plan),
            ExitCode::SigUnsealedBreak,
            "the former result would report a recorded, unsealed break"
        );

        let message = compact_file(&file, "app", file.version + 1)
            .expect_err("a compact cut cannot pass the current header")
            .diag()
            .1
            .to_owned();
        assert!(
            message.contains("cannot exceed the current header v(2)"),
            "got: {message}"
        );
    }

    /// A modify or remove before the cut is causally rejected instead of
    /// being flattened into a new add/remove pair at the boundary.
    #[test]
    fn compact_rejects_pre_cut_modify_and_remove() {
        let modified = parse(
            "\
signature app v(3);

v(1) {
  nonbreaking {
    add {
      module api {
        pub fn provide(x: .) -> .;
      }
    }
  }
}

v(2) {
  breaking {
    modify {
      module api {
        pub fn provide(x: ., y: .) -> .;
      }
    }
  }
}
",
        );
        let message = compact_file(&modified, "app", 3)
            .expect_err("pre-cut modify must fail")
            .diag()
            .1
            .to_owned();
        assert!(
            message.contains("pre-cut v(2) contains `modify`"),
            "got: {message}"
        );

        let removed = parse(
            "\
signature app v(3);

v(1) {
  nonbreaking {
    add {
      module api {
        pub fn provide() -> .;
      }
    }
  }
}

v(2) {
  breaking {
    remove {
      module api {
        provide;
      }
    }
  }
}
",
        );
        let message = compact_file(&removed, "app", 3)
            .expect_err("pre-cut remove must fail")
            .diag()
            .1
            .to_owned();
        assert!(
            message.contains("pre-cut v(2) contains `remove`"),
            "got: {message}"
        );
    }

    /// Re-adding one qualified name is a later incarnation, not an
    /// additive first introduction. Replay can choose the last
    /// declaration, but one compact boundary cannot preserve that
    /// history, so eligibility fails closed.
    #[test]
    fn compact_rejects_pre_cut_re_add() {
        let file = parse(
            "\
signature app v(3);

v(1) {
  nonbreaking {
    add {
      module api {
        pub fn provide() -> .;
      }
    }
  }
}

v(2) {
  nonbreaking {
    add {
      module api {
        pub fn provide(x: .) -> .;
      }
    }
  }
}
",
        );
        let message = compact_file(&file, "app", 3)
            .expect_err("pre-cut re-add must fail")
            .diag()
            .1
            .to_owned();
        assert!(
            message.contains("pre-cut v(2) re-adds `api.provide`"),
            "got: {message}"
        );
    }
}
