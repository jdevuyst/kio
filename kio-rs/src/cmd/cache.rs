//! Implementation of `kio cache <subcommand>`.
//!
//! `kio cache` is a subcommand container for Kio-semantic on-disk
//! cache management. Three children today:
//!
//! - `kio cache clear` — clear the contents of the cache directory
//!   declared by the package's `build { cache "<path>"; }` block in
//!   the package file. The intent is to give CI (and developers) a
//!   single, name-it-by command for restoring a fresh cache state
//!   without `rm -rf`'ing into implementation-specific paths.
//! - `kio cache path` — print the absolute path of the cache directory
//!   to stdout. Useful when external tooling (a CI step, an editor
//!   integration, a profiler) needs to read or measure cache contents
//!   without re-deriving the resolution rule.
//! - `kio cache gc` — garbage-collect stale Kio-semantic cache entries
//!   according to the cache root's recorded access metadata.
//!
//! The cache directory holds every Kio-semantic on-disk cache `kio`
//! ships (package-check, typed-module, enriched-IR,
//! emit/artifact, equiv, and Kiodoc snippet results). The rlib cache lives at
//! `<cache>/rlib/` under the same directory but is content-addressed
//! by `rustc` inputs — it is *not* gated by `--no-cache` and is
//! intentionally preserved by `kio cache clear` (it can be the
//! biggest by far, and re-populating it is expensive). The
//! per-user git-clone fetch cache at `$KIO_CACHE_HOME` is a
//! separate concept and is unaffected.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ast::{BuildBlock, BuildBlockCache};
use crate::exit_code::ExitCode;
use crate::pass::parser::parse_package_file;
use crate::path_display::DisplayPath;

const HELP_CACHE_TEMPLATE: &str = "\
Usage: kio cache <subcommand>

Manage the Kio-semantic on-disk caches for the current package.

Subcommands:
  clear         Clear the contents of the cache directory declared
                by the package's `build { cache \"<path>\"; }`
                block. Preserves the rlib cache subdirectory (which
                is content-addressed by rustc inputs and stays
                valid across Kio compiler edits).
  path          Print the absolute path of the cache directory to
                stdout.
  gc            Garbage-collect stale Kio-semantic cache entries.

Options:
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on success; 2 on CLI
usage error (missing / unknown subcommand).

See {base}/specs/cli.md#kio-cache-subcommand for full command behavior.";

const HELP_CLEAR_TEMPLATE: &str = "\
Usage: kio cache clear

Clear the Kio-semantic on-disk caches for the current package.

Reads `<name>.pkg.kio` at the current directory, resolves the
`build { cache \"<path>\"; }` directive, and removes the contents of
that directory. Preserves the rlib cache subdirectory and the cache
root's `.gitignore`. A build block that declares `cache ();` (no
cache), or a package file with no `build { ... }` block, exits 0
with `no cache configured`.

Must be run from a package root (one `<name>.pkg.kio` at the
current directory).

Options:
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on success / no-op /
disabled cache; 2 on CLI usage error (no package file, positional
args, unknown flag); 1 on internal error (unreadable package file,
filesystem failure).

See {base}/specs/cli.md#kio-cache-clear for full command behavior.";

const HELP_PATH_TEMPLATE: &str = "\
Usage: kio cache path

Print the absolute path of the package's cache directory to stdout.

Reads `<name>.pkg.kio` at the current directory and resolves
its `build { cache \"<path>\"; }` directive. Relative paths print
joined to the package root; absolute paths print verbatim. The path is
the same directory `kio cache clear` operates on; the directory need
not exist yet (a fresh checkout never built will see the would-be
path).

Must be run from a package root (one `<name>.pkg.kio` at the
current directory). A package file with no `build { ... }` block,
or a build block that declares `cache ()`, exits with the
build-error category.

Options:
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on success; 40 (build
error) when no `<name>.pkg.kio` is at the package root, the
package file has no `build { ... }` block, or the block declares
`cache ()`; 2 on CLI usage error (positional args, unknown flag);
1 on internal error.

See {base}/specs/cli.md#kio-cache-path for full command behavior.";

const HELP_GC_TEMPLATE: &str = "\
Usage: kio cache gc

Garbage-collect stale Kio-semantic cache entries for the current
package.

Reads `<name>.pkg.kio` at the current directory, resolves the
`build { cache \"<path>\"; }` directive, and removes semantic cache
entries whose recorded access time is outside the retained window for
their cache family. Preserves the rlib cache subdirectory and the
cache root's `.gitignore`. A build block that declares `cache ();`
(no cache), or a package file with no `build { ... }` block, exits 0
with `no cache configured`.

Must be run from a package root (one `<name>.pkg.kio` at the
current directory).

Options:
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on success / no-op /
disabled cache; 2 on CLI usage error (no package file, positional
args, unknown flag); 1 on internal error (unreadable package file,
filesystem failure).

See {base}/specs/cli.md#kio-cache-gc for full command behavior.";

/// Dispatch for `kio cache <subcommand>`. The container alone is a
/// usage error — same shape as `kio doc` with no subcommand.
pub fn run(args: &[String]) -> ExitCode {
    if let Some(first) = args.first() {
        match first.as_str() {
            "-h" | "--help" => {
                println!(
                    "{}",
                    HELP_CACHE_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
                );
                return ExitCode::Success;
            }
            "clear" => return run_clear(&args[1..]),
            "path" => return run_path(&args[1..]),
            "gc" => return run_gc(&args[1..]),
            other => {
                eprintln!("error: unknown subcommand: kio cache {other}");
                eprintln!();
                eprintln!(
                    "{}",
                    HELP_CACHE_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
                );
                return ExitCode::Usage;
            }
        }
    }
    eprintln!("error: `kio cache` requires a subcommand");
    eprintln!();
    eprintln!(
        "{}",
        HELP_CACHE_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
    );
    ExitCode::Usage
}

/// `kio cache path`. Reads the package's package file, resolves the
/// `build { cache "<path>"; }` directive, and prints the directory
/// path to stdout. A build block declaring `cache ();` (or a
/// package file with no build block) exits at the build-error tier
/// rather than printing nothing — external consumers (CI scripts,
/// profilers) want a loud signal that the package opted out, not an
/// empty string they might misinterpret as the cwd.
fn run_path(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "{}",
            HELP_PATH_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }
    if !args.is_empty() {
        eprintln!("error: `kio cache path` does not accept arguments");
        return ExitCode::Usage;
    }
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return ExitCode::Internal;
        }
    };
    path_at(&cwd)
}

/// Print the cache directory for the package rooted at
/// `package_root`. Pulled out so tests can drive the command against
/// a temp package without changing the process `cwd`.
pub fn path_at(package_root: &Path) -> ExitCode {
    // Locate + parse the package file and read its build block.
    // Missing-block / missing-file surface at the build-error tier
    // (40), matching `kio build`'s shape.
    let (build_path, build_block) = match load_build_block(package_root) {
        Ok(parts) => parts,
        Err(e) => return e.report_build(package_root),
    };
    let build_block = match build_block {
        Some(b) => b,
        None => {
            eprintln!(
                "error: {} has no `build {{ … }}` block — no cache directory configured",
                DisplayPath(&build_path)
            );
            return ExitCode::Build;
        }
    };
    match &build_block.cache {
        BuildBlockCache::Disabled { .. } => {
            eprintln!(
                "error: {} declares `cache ();` — no cache directory configured",
                DisplayPath(&build_path)
            );
            ExitCode::Build
        }
        BuildBlockCache::Path { path, .. } => {
            let resolved = absolutize(package_root, path);
            println!("{}", DisplayPath(&resolved));
            ExitCode::Success
        }
    }
}

/// `kio cache clear`. Reads the package's package file, resolves the
/// `build { cache "<path>"; }` directive, and clears the directory's
/// contents (preserving the rlib subdirectory and the cache root's
/// `.gitignore`).
fn run_clear(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "{}",
            HELP_CLEAR_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }
    if !args.is_empty() {
        eprintln!("error: `kio cache clear` does not accept arguments");
        return ExitCode::Usage;
    }
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return ExitCode::Internal;
        }
    };
    clear_at(&cwd)
}

fn run_gc(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "{}",
            HELP_GC_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }
    if !args.is_empty() {
        eprintln!("error: `kio cache gc` does not accept arguments");
        return ExitCode::Usage;
    }
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return ExitCode::Internal;
        }
    };
    gc_at(&cwd)
}

pub fn gc_at(package_root: &Path) -> ExitCode {
    let (_build_path, build_block) = match load_build_block(package_root) {
        Ok(parts) => parts,
        Err(e) => return e.report_usage(package_root),
    };

    let cache_path = match build_block.as_ref().map(|b| &b.cache) {
        None | Some(BuildBlockCache::Disabled { .. }) => {
            println!("no cache configured");
            return ExitCode::Success;
        }
        Some(BuildBlockCache::Path { path, .. }) => absolutize(package_root, path),
    };

    if !cache_path.exists() {
        println!("cache at {} was already clean", DisplayPath(&cache_path));
        return ExitCode::Success;
    }

    match crate::cache::gc::run_explicit(&cache_path) {
        Ok(summary) if summary.total_removed() == 0 => {
            println!("cache at {} was already clean", DisplayPath(&cache_path));
            ExitCode::Success
        }
        Ok(summary) => {
            println!(
                "garbage-collected {} entries from {}",
                summary.total_removed(),
                DisplayPath(&cache_path)
            );
            ExitCode::Success
        }
        Err(e) => {
            eprintln!(
                "error: garbage-collecting {}: {e}",
                DisplayPath(&cache_path)
            );
            ExitCode::Internal
        }
    }
}

/// Clear the cache for the package rooted at `package_root`. Pulled
/// out so tests can drive the command against a temp package without
/// changing the process `cwd`.
pub fn clear_at(package_root: &Path) -> ExitCode {
    // Locate + parse the package file and read its build block. A
    // missing package file, multiple package files, or a parse
    // error surface at the usage tier here — `kio cache clear` doesn't
    // run the build pipeline, so it doesn't surface the build-error
    // category (40) the way `kio build` does.
    let (_build_path, build_block) = match load_build_block(package_root) {
        Ok(parts) => parts,
        Err(e) => return e.report_usage(package_root),
    };

    // A package file with no build block, or one whose build block
    // declares `cache ();`, has no cache directory — a no-op success.
    let cache_path = match build_block.as_ref().map(|b| &b.cache) {
        None | Some(BuildBlockCache::Disabled { .. }) => {
            println!("no cache configured");
            return ExitCode::Success;
        }
        Some(BuildBlockCache::Path { path, .. }) => absolutize(package_root, path),
    };

    // The cache directory may not exist yet — first run on a fresh
    // checkout, or a package that's never been built. Treat that as
    // "already empty" rather than an error.
    if !cache_path.exists() {
        println!("cache at {} was already empty", DisplayPath(&cache_path));
        return ExitCode::Success;
    }

    match clear_cache_contents(&cache_path) {
        Ok(0) => {
            println!("cache at {} was already empty", DisplayPath(&cache_path));
            ExitCode::Success
        }
        Ok(n) => {
            println!("cleared {n} entries from {}", DisplayPath(&cache_path));
            ExitCode::Success
        }
        Err(e) => {
            eprintln!("error: clearing {}: {e}", DisplayPath(&cache_path));
            ExitCode::Internal
        }
    }
}

/// Walk one level of `<cache>` and remove every entry except the
/// rlib cache (`rlib/`) and the cache root's `.gitignore`. Returns
/// the count of removed entries.
///
/// We deliberately do **not** `remove_dir_all` the cache root
/// itself: the operator may have set it up with specific
/// permissions, ACLs, or as a symlink; the on-first-write
/// `.gitignore` would have to be regenerated; and the rlib cache
/// (which is *not* a Kio-semantic cache and is unaffected by
/// `--no-cache`) sits under the same root and must survive.
fn clear_cache_contents(cache_root: &Path) -> std::io::Result<usize> {
    let mut removed = 0usize;
    for entry in fs::read_dir(cache_root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Preserve:
        // - `rlib/` — content-addressed by rustc inputs, expensive
        //   to rebuild, *not* gated by `--no-cache`.
        // - `.gitignore` — written on first cache-root creation so
        //   the cache contents stay out of `git status`. Surviving
        //   the clear means we don't have to recreate it on next
        //   write; idempotent.
        if name_str == "rlib" || name_str == ".gitignore" {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() && !file_type.is_symlink() {
            fs::remove_dir_all(&path)?;
        } else {
            // Files, symlinks (including symlinks to directories) —
            // `remove_file` unlinks the link itself rather than
            // following it.
            fs::remove_file(&path)?;
        }
        removed += 1;
    }
    Ok(removed)
}

// =========================================================================
// Package-file discovery + build-block extraction
// =========================================================================

/// Failure modes from locating + parsing the package file. Each caller
/// maps these to the exit-code tier it wants via
/// [`Self::report_build`] / [`Self::report_usage`].
enum LocateError {
    Missing,
    Multiple(Vec<PathBuf>),
    Io(std::io::Error),
    Read(PathBuf, std::io::Error),
    Parse(PathBuf, String),
}

impl LocateError {
    /// Print a diagnostic and return the exit code for callers that
    /// treat a missing / unreadable / malformed package file as a
    /// build-error-tier failure (`kio cache path`, matching
    /// `kio build`).
    fn report_build(&self, package_root: &Path) -> ExitCode {
        self.report(package_root, ExitCode::Build, "`kio cache path` requires")
    }

    /// Print a diagnostic and return the exit code for callers that
    /// treat the same conditions as a usage-tier failure
    /// (`kio cache clear`).
    fn report_usage(&self, package_root: &Path) -> ExitCode {
        self.report(package_root, ExitCode::Usage, "run `kio cache clear` from")
    }

    fn report(&self, package_root: &Path, missing_tier: ExitCode, missing_hint: &str) -> ExitCode {
        match self {
            LocateError::Missing => {
                eprintln!(
                    "error: no `<name>.pkg.kio` in {} — {missing_hint} a package directory; run `kio init` to scaffold one (see specs/package.md § Package file)",
                    DisplayPath(package_root)
                );
                missing_tier
            }
            LocateError::Multiple(paths) => {
                eprintln!(
                    "error: multiple `*.pkg.kio` files at the package root; expected exactly one"
                );
                for p in paths {
                    eprintln!("  {}", DisplayPath(p));
                }
                missing_tier
            }
            LocateError::Io(e) => {
                eprintln!("error: reading {}: {e}", DisplayPath(package_root));
                ExitCode::Internal
            }
            LocateError::Read(path, e) => {
                eprintln!("error: cannot read {}: {e}", DisplayPath(path));
                ExitCode::Internal
            }
            LocateError::Parse(path, message) => {
                eprintln!("error: cannot parse {}: {message}", DisplayPath(path));
                missing_tier
            }
        }
    }
}

/// Locate the package's `<name>.pkg.kio` at `package_root`,
/// parse it, and return its path plus the parsed `build { ... }`
/// block (`None` when the file declares none). Subdirs are not
/// searched — the package file lives at the package root per
/// `specs/package.md` § Package file.
///
/// `kio cache` parses the package file standalone rather than
/// running the full build pipeline: it only needs the build block's
/// `cache` field, not a typecheck.
fn load_build_block(package_root: &Path) -> Result<(PathBuf, Option<BuildBlock>), LocateError> {
    let mut hits = Vec::new();
    for entry in fs::read_dir(package_root).map_err(LocateError::Io)? {
        let entry = entry.map_err(LocateError::Io)?;
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if crate::file_kind::is_package_file(name) {
            hits.push(path);
        }
    }
    let path = match hits.len() {
        0 => return Err(LocateError::Missing),
        1 => hits.into_iter().next().unwrap(),
        _ => {
            hits.sort();
            return Err(LocateError::Multiple(hits));
        }
    };
    let stem = path
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(crate::file_kind::package_stem)
        .map(|s| s.to_owned());
    let source = fs::read_to_string(&path).map_err(|e| LocateError::Read(path.clone(), e))?;
    let package = parse_package_file(&source, stem.as_deref()).map_err(|err| {
        let (_, message) = err.diag();
        LocateError::Parse(path.clone(), message.to_string())
    })?;
    Ok((path, package.build))
}

/// Resolve a possibly-relative `path` (from the build block's
/// `cache` field) against `package_root`. Absolute paths pass
/// through unchanged. Mirrors `enriched_cache::absolutize`.
fn absolutize(package_root: &Path, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        package_root.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("kio-cache-clear-unit-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Drop helper — recursively remove a temp tree even if the test
    /// panics.
    struct TempTree(PathBuf);
    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Write a `<name>.pkg.kio` at `root` carrying a `build { ... }`
    /// block with the given body (the `cache` / `target` lines).
    fn write_pkg(root: &Path, build_body: &str) {
        let src = format!("package pkg;\n\nbuild {{\n{build_body}}}\n");
        fs::write(root.join("pkg.pkg.kio"), src).unwrap();
    }

    /// `cache ();` — clear exits 0 with the "no cache configured"
    /// message; no filesystem state is touched.
    #[test]
    fn disabled_cache_succeeds_with_no_cache_configured_message() {
        let root = TempTree(temp_dir());
        write_pkg(&root.0, "  cache ();\n  target js { out \"out/\"; }\n");
        assert_eq!(clear_at(&root.0), ExitCode::Success);
    }

    /// A package file with no `build { ... }` block — clear exits 0
    /// with the "no cache configured" message.
    #[test]
    fn no_build_block_succeeds_with_no_cache_configured_message() {
        let root = TempTree(temp_dir());
        fs::write(root.0.join("pkg.pkg.kio"), "package pkg;\n").unwrap();
        assert_eq!(clear_at(&root.0), ExitCode::Success);
    }

    /// Cache directory absent: clear exits 0 with the
    /// "already empty" message. Same shape as a build that never
    /// ran.
    #[test]
    fn missing_cache_dir_succeeds() {
        let root = TempTree(temp_dir());
        write_pkg(
            &root.0,
            "  cache \".cache/\";\n  target js { out \"out/\"; }\n",
        );
        assert_eq!(clear_at(&root.0), ExitCode::Success);
    }

    /// Empty cache directory: clear exits 0 with the "already empty"
    /// message; the directory stays.
    #[test]
    fn empty_cache_dir_succeeds_and_preserves_dir() {
        let root = TempTree(temp_dir());
        write_pkg(
            &root.0,
            "  cache \".cache/\";\n  target js { out \"out/\"; }\n",
        );
        let cache = root.0.join(".cache");
        fs::create_dir_all(&cache).unwrap();
        assert_eq!(clear_at(&root.0), ExitCode::Success);
        assert!(cache.is_dir(), "cache dir should survive an empty clear");
    }

    /// Populated cache directory: contents are removed; the rlib
    /// subdir and the cache root's `.gitignore` survive.
    #[test]
    fn populated_cache_dir_is_cleared_preserving_rlib_and_gitignore() {
        let root = TempTree(temp_dir());
        write_pkg(
            &root.0,
            "  cache \".cache/\";\n  target js { out \"out/\"; }\n",
        );
        let cache = root.0.join(".cache");
        fs::create_dir_all(cache.join("enriched-ir")).unwrap();
        fs::write(cache.join("enriched-ir/entry.bin"), b"stale").unwrap();
        fs::create_dir_all(cache.join("doc")).unwrap();
        fs::write(cache.join("doc/entry.bin"), b"stale").unwrap();
        fs::create_dir_all(cache.join("rlib/v1")).unwrap();
        fs::write(cache.join("rlib/v1/keep.bin"), b"keepme").unwrap();
        fs::write(cache.join(".gitignore"), "*\n!.gitignore\n").unwrap();

        assert_eq!(clear_at(&root.0), ExitCode::Success);

        assert!(
            !cache.join("enriched-ir").exists(),
            "enriched-ir/ should be cleared"
        );
        assert!(!cache.join("doc").exists(), "doc/ should be cleared");
        assert!(cache.join("rlib").is_dir(), "rlib/ must be preserved");
        assert!(
            cache.join("rlib/v1/keep.bin").is_file(),
            "rlib/ contents must be preserved"
        );
        assert!(
            cache.join(".gitignore").is_file(),
            ".gitignore must be preserved"
        );
    }

    /// Package file absent: clear exits with the usage code (2)
    /// and a message pointing at the package-directory requirement.
    #[test]
    fn missing_package_file_exits_usage() {
        let root = TempTree(temp_dir());
        // No package file in the dir.
        assert_eq!(clear_at(&root.0), ExitCode::Usage);
    }

    /// `kio cache` with no child subcommand exits with the usage
    /// code.
    #[test]
    fn cache_with_no_subcommand_exits_usage() {
        assert_eq!(run(&[]), ExitCode::Usage);
    }

    /// `kio cache <unknown>` exits with the usage code.
    #[test]
    fn cache_with_unknown_subcommand_exits_usage() {
        assert_eq!(run(&["bogus".to_owned()]), ExitCode::Usage);
    }

    /// `kio cache --help` and `kio cache -h` both succeed.
    #[test]
    fn cache_help_succeeds() {
        assert_eq!(run(&["--help".to_owned()]), ExitCode::Success);
        assert_eq!(run(&["-h".to_owned()]), ExitCode::Success);
    }

    /// `kio cache clear --help` succeeds. The flag is recognized
    /// before the no-arguments check.
    #[test]
    fn cache_clear_help_succeeds() {
        assert_eq!(run_clear(&["--help".to_owned()]), ExitCode::Success);
        assert_eq!(run_clear(&["-h".to_owned()]), ExitCode::Success);
    }

    /// `kio cache clear <extra-arg>` exits with the usage code.
    #[test]
    fn cache_clear_with_positional_args_exits_usage() {
        assert_eq!(run_clear(&["unexpected".to_owned()]), ExitCode::Usage);
    }

    /// `kio cache path` on a package whose build block declares a
    /// `cache "<path>";` directive exits 0 and prints the absolute
    /// path. The directory need not exist (a never-built package
    /// still has a well-defined would-be path).
    #[test]
    fn path_succeeds_for_configured_cache() {
        let root = TempTree(temp_dir());
        write_pkg(
            &root.0,
            "  cache \".cache/\";\n  target js { out \"out/\"; }\n",
        );
        assert_eq!(path_at(&root.0), ExitCode::Success);
    }

    /// `kio cache path` against `cache ();` exits with the build-
    /// error category — printing nothing would be ambiguous, the
    /// loud signal lets a CI script branch on the exit code.
    #[test]
    fn path_exits_build_for_disabled_cache() {
        let root = TempTree(temp_dir());
        write_pkg(&root.0, "  cache ();\n  target js { out \"out/\"; }\n");
        assert_eq!(path_at(&root.0), ExitCode::Build);
    }

    /// `kio cache path` against a package file with no
    /// `build { ... }` block exits at the build-error tier (40) —
    /// there is no cache directory to name.
    #[test]
    fn path_exits_build_for_no_build_block() {
        let root = TempTree(temp_dir());
        fs::write(root.0.join("pkg.pkg.kio"), "package pkg;\n").unwrap();
        assert_eq!(path_at(&root.0), ExitCode::Build);
    }

    /// `kio cache path` without a package file at the package root
    /// exits at the build-error tier (40), matching `kio build`'s
    /// missing-marker shape.
    #[test]
    fn path_exits_build_for_missing_package_file() {
        let root = TempTree(temp_dir());
        assert_eq!(path_at(&root.0), ExitCode::Build);
    }

    /// `kio cache path --help` and `-h` both succeed.
    #[test]
    fn cache_path_help_succeeds() {
        assert_eq!(run_path(&["--help".to_owned()]), ExitCode::Success);
        assert_eq!(run_path(&["-h".to_owned()]), ExitCode::Success);
    }

    /// `kio cache path <extra-arg>` exits with the usage code.
    #[test]
    fn cache_path_with_positional_args_exits_usage() {
        assert_eq!(run_path(&["unexpected".to_owned()]), ExitCode::Usage);
    }
}
