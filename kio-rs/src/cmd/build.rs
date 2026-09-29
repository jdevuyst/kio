//! Implementation of `kio build`.
//!
//! Pipeline:
//!
//! 1. Confirm a package marker — exactly one `<name>.pkg.kio` at
//!    the package root — **before** the source walk, so invoking
//!    `kio build` outside a package fails loud rather than recursing
//!    into stray nested `*.kio` files.
//! 2. Run the same checks `kio check` runs (parse → use → name-res →
//!    type). If any fail, return their category exit code without
//!    writing output. The package file (with its `build { ... }`
//!    block) is parsed as part of this walk.
//! 3. Read the `build { ... }` block off the already-parsed root
//!    package file. A package whose package file has no build
//!    block is a build error (exit 40), per `specs/exit-codes.md`.
//! 4. Resolve the user-supplied target ids (or all of them if none were
//!    given) against the block's `target <id> { ... }` blocks. Unknown
//!    ids are a build error.
//! 5. Dispatch each selected target to its backend. Backends are
//!    recognized by bare target ids such as `js`, `python`, `java`,
//!    `rust`, and `kio-prime`; other ids are rejected.
//!
//! The "target id selects backend" convention is a placeholder for what
//! the spec leaves open ("hand the target block's key/value map to the
//! corresponding backend" — without saying how the backend is
//! identified). It will tighten once a `backend` key (or equivalent) is
//! settled in the spec.

// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(â¦)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
use crate::cmd::package_fanout::CapturedOutput;
use crate::path_display::DisplayPath;
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::ast::{BuildBlock, TargetBlock};
use crate::backends;
use crate::backends::kio_prime;
use crate::build_target::BuildTarget;
use crate::cache::keys::{
    ArtifactInputFingerprint, ArtifactTargetProfileFingerprint, CacheTarget, EmitInputFingerprint,
    EmitTargetProfileFingerprint,
};
use crate::cmd::build_timing;
use crate::cmd::check;
use crate::exit_code::ExitCode;
use crate::pass::resolve::Package;

#[cfg(test)]
mod cache_work_counters {
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub(super) struct Counts {
        pub(super) artifact_keys: usize,
        pub(super) artifact_renders: usize,
        pub(super) prime_emit_serializations: usize,
        pub(super) prime_emit_keys: usize,
    }

    thread_local! {
        static COUNTS: Cell<Counts> = const { Cell::new(Counts {
            artifact_keys: 0,
            artifact_renders: 0,
            prime_emit_serializations: 0,
            prime_emit_keys: 0,
        }) };
    }

    #[cfg(feature = "surface")]
    pub(super) fn reset() {
        COUNTS.set(Counts::default());
    }

    #[cfg(feature = "surface")]
    pub(super) fn snapshot() -> Counts {
        COUNTS.get()
    }

    pub(super) fn record_artifact_key() {
        COUNTS.with(|cell| {
            let mut counts = cell.get();
            counts.artifact_keys += 1;
            cell.set(counts);
        });
    }

    pub(super) fn record_artifact_render() {
        COUNTS.with(|cell| {
            let mut counts = cell.get();
            counts.artifact_renders += 1;
            cell.set(counts);
        });
    }

    pub(super) fn record_prime_emit_serialization() {
        COUNTS.with(|cell| {
            let mut counts = cell.get();
            counts.prime_emit_serializations += 1;
            cell.set(counts);
        });
    }

    pub(super) fn record_prime_emit_key() {
        COUNTS.with(|cell| {
            let mut counts = cell.get();
            counts.prime_emit_keys += 1;
            cell.set(counts);
        });
    }
}

const HELP_TEMPLATE: &str = "\
Usage: kio build [--skip-unsupported-targets] [<target-id>...] [<package-path>...]

Transpile package(s) to one or more compilation targets.

Phase 1 runs the same checks `kio check` runs. Phase 2 reads the
`build { ... }` block from each package's <name>.pkg.kio and
dispatches to the backend(s). With no <target-id>, every
target block in the build block is built; the
`--skip-unsupported-targets` flag tunes that default mode.

Package scope: with no <package-path>, every package discovered in the
current directory's subtree is built independently. A <package-path>
positional (a directory or a `*.pkg.kio` file) scopes the build to the
named package(s); a bare identifier positional is a <target-id>.

Options:
  --skip-unsupported-targets
                   Treat targets in the build block whose backend this
                   build of kio doesn't recognize as no-ops (warn on
                   stderr, exit 0 if every target that did run
                   succeeded). Only applies to implicit selection (no
                   positional <target-id>); explicit ids whose backend
                   is unsupported still error.
  -h, --help       Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on success; 40 (build
error) for a missing package file, a package file with no
`build { ... }` block, unknown target id, key validation, codegen, or
runtime-audit failures; the matching `1x` category code for Phase-1
parse / use / type errors.

See {base}/specs/cli.md#kio-build---skip-unsupported-targets-target-id-package-path for full command behavior.";

pub fn run(args: &[String], prime_only: bool) -> ExitCode {
    let build_total_start = Instant::now();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "{}",
            HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }
    let mut skip_unsupported = false;
    let mut target_ids: Vec<String> = Vec::new();
    let mut package_selectors: Vec<String> = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--skip-unsupported-targets" => skip_unsupported = true,
            other if other.starts_with("--") => {
                eprintln!("error: unknown flag for `kio build`: {other}");
                return ExitCode::Usage;
            }
            // A positional that contains a path separator or carries the
            // `*.pkg.kio` suffix is a package selector; a bare identifier
            // (`js`, `rust`, `kio-prime`) is a target id. The shape-based
            // rule is deterministic — it does not depend on cwd
            // filesystem state, so a cwd-sibling directory named `rust`
            // never hijacks the bare-identifier-is-a-target-id rule.
            other if crate::package_collection::is_package_selector_arg(other) => {
                package_selectors.push(other.to_owned())
            }
            other => target_ids.push(other.to_owned()),
        }
    }

    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return ExitCode::Internal;
        }
    };

    // Resolve the set of package roots to build. With no explicit
    // package selector, discover every package in the cwd subtree and
    // fan out over all of them; with selectors, build exactly the
    // named packages. `kio build` is no longer single-root-package:
    // it builds every discovered package independently (no
    // cross-package rollback — each package's success/failure is
    // its own), mirroring `kio sig`.
    let package_dirs = match resolve_package_dirs(&cwd, &package_selectors) {
        Ok(dirs) => dirs,
        Err(code) => return code,
    };

    // Build each discovered package, rooted at its own directory.
    // Packages are independent — own workspace, own typecheck, own
    // codegen, disjoint `out/` — so the fan-out runs them in parallel
    // (`cmd::package_fanout`), buffering each package's diagnostics and
    // replaying them in input order. A later package still builds after
    // an earlier one fails; the overall exit code is the first failure
    // (build-error tier) so CI sees a non-zero result. First-failure is
    // order-independent, so the chosen code does not depend on which
    // package's worker finished first.
    crate::cmd::package_fanout::run(
        &package_dirs,
        "kio build",
        |pkg_dir, cap| {
            build_one_package(
                pkg_dir,
                &target_ids,
                skip_unsupported,
                prime_only,
                build_total_start,
                cap,
            )
        },
        |acc, next| {
            if acc != ExitCode::Success { acc } else { next }
        },
    )
}

/// Resolve the package directories to build. With no selector, discover
/// every package in `cwd`'s subtree; with selectors, resolve each to
/// the directory holding its `*.pkg.kio`. Prints a diagnostic and
/// returns the matching exit code on failure.
fn resolve_package_dirs(cwd: &Path, selectors: &[String]) -> Result<Vec<PathBuf>, ExitCode> {
    if selectors.is_empty() {
        let roots = match crate::package_collection::discover_package_roots(cwd) {
            Ok(roots) => roots,
            Err(crate::package_collection::WalkError::MultiplePackageFiles { root, paths }) => {
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
                "error: no `<name>.pkg.kio` in {} or its subdirectories — `kio build` requires a package file at a package root; run `kio init` to scaffold one (see specs/package.md § Package File)",
                DisplayPath(cwd)
            );
            return Err(ExitCode::Build);
        }
        return Ok(roots.into_iter().map(|r| r.dir).collect());
    }

    // Explicit selectors: each names a package directory or a
    // `.pkg.kio` file. Resolve to the holding directory and confirm
    // a package marker is present there.
    let mut dirs = Vec::with_capacity(selectors.len());
    for sel in selectors {
        let path = Path::new(sel);
        let dir = if path.is_dir() {
            path.to_path_buf()
        } else if path.is_file() {
            // Only a `*.pkg.kio` file is a valid file selector; any other
            // existing file is an input error, not a silent accept.
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
        match find_package_file(&dir) {
            Ok(_) => dirs.push(dir),
            Err(PackageFileError::Missing) => {
                eprintln!(
                    "error: no `<name>.pkg.kio` in {} (selected by `{sel}`)",
                    DisplayPath(&dir)
                );
                return Err(ExitCode::Build);
            }
            Err(PackageFileError::Multiple(paths)) => {
                eprintln!(
                    "error: multiple `*.pkg.kio` files in {} (selected by `{sel}`); expected exactly one",
                    DisplayPath(&dir)
                );
                for p in paths {
                    eprintln!("  {}", DisplayPath(&p));
                }
                return Err(ExitCode::Build);
            }
            Err(PackageFileError::Io(e)) => {
                eprintln!("error: walking source tree: {e}");
                return Err(ExitCode::Internal);
            }
        }
    }
    // Canonicalize + dedup so overlapping / duplicate selectors
    // (`./pkg ./pkg`, or `./pkg ./pkg/foo.pkg.kio`) build each package
    // exactly once, preserving first-seen order.
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut deduped = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let key = fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if seen.insert(key) {
            deduped.push(dir);
        }
    }
    Ok(deduped)
}

/// Build one package rooted at `pkg_dir`. Runs the same pipeline the
/// single-package path used to run inline, but rooted at an explicit
/// directory so the multi-package fan-out can build each discovered
/// package independently.
fn build_one_package(
    pkg_dir: &Path,
    target_ids: &[String],
    skip_unsupported: bool,
    prime_only: bool,
    build_total_start: Instant,
    cap: &mut CapturedOutput,
) -> ExitCode {
    // Phase 0: confirm the package marker **before** any source walk.
    match find_package_file(pkg_dir) {
        Ok(_) => {}
        Err(PackageFileError::Missing) => {
            cap_errln!(
                cap,
                "error: no `<name>.pkg.kio` in {} — `kio build` requires a package file at the package root; run `kio init` to scaffold one (see specs/package.md § Package File)",
                DisplayPath(pkg_dir)
            );
            return ExitCode::Build;
        }
        Err(PackageFileError::Multiple(paths)) => {
            cap_errln!(
                cap,
                "error: multiple `*.pkg.kio` files at the package root; expected exactly one"
            );
            for p in paths {
                cap_errln!(cap, "  {}", DisplayPath(&p));
            }
            return ExitCode::Build;
        }
        Err(PackageFileError::Io(e)) => {
            cap_errln!(cap, "error: walking source tree: {e}");
            return ExitCode::Internal;
        }
    };

    // Dependencies are materialized at `kio dep fetch`/`update` and committed,
    // so the build consumes their re-rooted `.kio` modules as ordinary source
    // — it does not re-materialize on every build.

    // Phase 1: typecheck. Reuses the full `kio check` pipeline so a
    // package that doesn't typecheck never produces build output. The
    // returned workspace is at the `Prime` phase: elaborator-position
    // elaborations (`into!` / `onto!` / `match!`) are already
    // substituted into the AST, so the backend can emit the strict
    // Kio'-shaped tree without consulting a side table.
    //
    // `skip_ok = false`: `kio build` consumes every package's typed
    // form for codegen, so it never skips re-typechecking via the
    // package-check cache.
    let typecheck_start = Instant::now();
    let mut workspace =
        match check::compile_workspace_at_buffered(pkg_dir, prime_only, false, &mut cap.stderr) {
            Ok(w) => w,
            Err(code) => return code,
        };
    let typecheck_ms = build_timing::elapsed_ms(typecheck_start);

    // Scope the build to the bridge: emit and the host interface are the
    // contract surface (the bridge-reachable closure), not every module
    // that happens to be on disk. A dependency materializes its whole
    // tree, but modules the consumer's bridge never reaches (a library's
    // demo `main`, unused helpers) must not contribute emitted code or
    // host requirements — otherwise their out-of-contract host calls
    // surface as missing host items on the static backends. Done once
    // here, before the enriched-IR cache and the shared routed package,
    // so every target and both caches see the pruned set. `kio check`
    // (above) still type-checks every module; only `kio build` prunes.
    if let Some(root) = workspace.root_package.as_mut() {
        root.prune_to_bridge_reachable();
    }

    // Phase 3: read the `build { ... }` block off the already-parsed
    // root package file. The block was parsed (and its grammar
    // validated) as part of the Phase-1 source walk, so there is no
    // separate filesystem lookup or parse here. A package whose
    // package file declares no build block has no build outputs —
    // a build error (exit 40), per spec.
    let root_pkg = workspace
        .root_package
        .as_ref()
        .expect("kio build passes skip_ok = false, so the root is always typechecked");

    // Advisory: if a `*.sig.kio` changelog is present and the live
    // surface breaks the last sealed contract (unrecorded), warn on
    // stderr. This never changes the build's exit code — a package with
    // no sig builds identically (open-world).
    #[cfg(feature = "surface")]
    crate::cmd::sig::warn_if_unrecorded_breaking_buffered(root_pkg, pkg_dir, &mut cap.stderr);

    let build_block = match root_pkg
        .package_file()
        .and_then(|d| d.package_file.build.as_ref())
    {
        Some(b) => b,
        None => {
            cap_errln!(
                cap,
                "error: the package's package file has no `build {{ … }}` block — \
                 `kio build` requires one to name the compilation targets (see \
                 specs/package.md § Build target files)"
            );
            return ExitCode::Build;
        }
    };

    // Phase 3b: enforce target-id uniqueness (spec: "unique within the block").
    if let Err(msg) = check_unique_ids(build_block) {
        cap_errln!(cap, "error: {msg}");
        return ExitCode::Build;
    }

    // Phase 4: resolve target ids. `explicit` records whether the user
    // named ids on the CLI — `--skip-unsupported-targets` only applies
    // to implicit selection.
    let explicit = !target_ids.is_empty();
    let selected: Vec<&TargetBlock> = if target_ids.is_empty() {
        build_block.targets.iter().collect()
    } else {
        let mut out = Vec::with_capacity(target_ids.len());
        for id in target_ids {
            match build_block.targets.iter().find(|t| t.id == *id) {
                Some(t) => out.push(t),
                None => {
                    cap_errln!(cap, "error: unknown target id `{id}` in the build block");
                    return ExitCode::Build;
                }
            }
        }
        out
    };

    // Phase 4b: resolve the enriched-IR cache from the build block's
    // `cache` field. `cache "<path>";` activates every on-disk
    // cache `kio` ships, sub-namespaced under that directory;
    // `cache ();` opts the package out entirely (every cache
    // lookup misses, every store is a no-op). The
    // enriched-IR cache then sits between structural recovery +
    // the optimization catalog and per-backend lowering, so a
    // package whose typed `Module<Prime>` hasn't changed skips
    // the recovery + optimization walk per module.
    //
    // Gated on [`cache::policy::caches_enabled`]: when the operator
    // hasn't opted in to the Kio-semantic caches, fall back to the
    // disabled variant regardless of what the build block declares.
    let enriched_cache = if crate::cache::policy::caches_enabled() {
        crate::cache::enriched::resolve_from_cache_field(pkg_dir, &build_block.cache)
    } else {
        crate::cache::enriched::EnrichedCache::disabled()
    };
    let emit_cache = if crate::cache::policy::caches_enabled() {
        crate::cache::emit::resolve_from_cache_field(pkg_dir, &build_block.cache)
    } else {
        crate::cache::emit::EmitCache::disabled()
    };
    let artifact_cache = if crate::cache::policy::caches_enabled() {
        crate::cache::artifact::resolve_from_cache_field(pkg_dir, &build_block.cache)
    } else {
        crate::cache::artifact::ArtifactCache::disabled()
    };
    let root_label = build_timing::package_label(root_pkg);

    // The backend-agnostic recover → optimize → route → annotate prefix is
    // shared across the JS and Rust backends and computed once (lazily, on
    // the first backend that needs it). See `SharedRoutedPackage`. Each
    // backend that recovers consumes this shared `Package<Routed>` instead
    // of recomputing the route + capability passes per target.
    let routed = SharedRoutedPackage::new(root_pkg, &enriched_cache, &root_label);

    // Compute the shared routed package on this thread, up front,
    // whenever more than one target is selected — before the rayon
    // per-target dispatch below fans out. The computation is itself
    // rayon-parallel (recover / route / capability passes), and
    // `SharedRoutedPackage`'s `OnceLock::get_or_init` parks whichever
    // backend loses the initialization race — i.e. it parks a rayon
    // worker. Triggering that init from *inside* the parallel dispatch
    // deadlocks the pool under load: the parked workers shrink the pool
    // while the initializer still needs it for its own parallel passes,
    // and rayon cannot steal work from a parked worker (observed as
    // every thread wedged in `futex_wait`, no child processes, no
    // progress). Forcing it here, with the whole pool free, means every
    // backend then reads an already-initialized value and never blocks.
    //
    // The guard is `len > 1`, not a per-backend "does this backend read
    // the routed package?" test, on purpose: target ids are unique and
    // `kio-prime` (which emits from `Package<Prime>` directly) is the
    // only backend that does not read it, so any two or more targets
    // always include at least one consumer — the up-front computation is
    // never wasted — and the rule needs no maintenance when a backend is
    // added. A lone target cannot race itself, so a warm single-target
    // build whose artifact cache hits still computes nothing.
    if selected.len() > 1 {
        routed.get();
    }

    // Phase 5: dispatch. Targets emit in parallel via rayon — codegen
    // for one target is independent of any other, so the build
    // orchestrator fans the outer per-target loop across cores (per-item
    // parallelism within each target still applies). Results are
    // collected in input order so diagnostics and the chosen exit code
    // stay deterministic regardless of execution order.
    let dispatch_start = Instant::now();
    let results: Vec<Result<(), BackendError>> = crate::maybe_par_iter!(selected)
        .map(|target| {
            dispatch_target(
                pkg_dir,
                target,
                &workspace,
                &routed,
                &emit_cache,
                &artifact_cache,
            )
        })
        .collect();
    let dispatch_ms = build_timing::elapsed_ms(dispatch_start);
    build_timing::log_workspace(
        &root_label,
        selected.len(),
        typecheck_ms,
        dispatch_ms,
        build_timing::elapsed_ms(build_total_start),
    );
    for (target, result) in selected.iter().zip(results) {
        match result {
            Ok(()) => {}
            Err(BackendError::UnknownBackend(msg)) => {
                if skip_unsupported && !explicit {
                    cap_errln!(
                        cap,
                        "warning: skipping target '{}': no backend in this kio build",
                        target.id
                    );
                    continue;
                }
                cap_errln!(cap, "error: target `{}`: {msg}", target.id);
                return ExitCode::Build;
            }
            Err(BackendError::Build(msg)) => {
                cap_errln!(cap, "error: target `{}`: {msg}", target.id);
                return ExitCode::Build;
            }
        }
    }

    ExitCode::Success
}

// =========================================================================
// Package-marker discovery
// =========================================================================

enum PackageFileError {
    Missing,
    Multiple(Vec<PathBuf>),
    Io(std::io::Error),
}

/// Find the package's `<name>.pkg.kio` at the cwd root — the
/// package marker `kio build` checks before walking the source tree.
/// Subdirectories are not searched — the package file lives at the
/// package root per spec.
fn find_package_file(cwd: &Path) -> Result<PathBuf, PackageFileError> {
    let mut hits = Vec::new();
    for entry in fs::read_dir(cwd).map_err(PackageFileError::Io)? {
        let entry = entry.map_err(PackageFileError::Io)?;
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if crate::file_kind::is_package_file(name) {
            hits.push(path);
        }
    }
    match hits.len() {
        0 => Err(PackageFileError::Missing),
        1 => Ok(hits.into_iter().next().unwrap()),
        _ => {
            hits.sort();
            Err(PackageFileError::Multiple(hits))
        }
    }
}

fn check_unique_ids(bf: &BuildBlock) -> Result<(), String> {
    for (i, t) in bf.targets.iter().enumerate() {
        if let Some(prev) = bf.targets[..i].iter().find(|p| p.id == t.id) {
            let _ = prev;
            return Err(format!(
                "duplicate target id `{}` in the build block (each id must be unique)",
                t.id
            ));
        }
    }
    Ok(())
}

// =========================================================================
// Backend dispatch
// =========================================================================

/// Two failure modes from a backend. `Build` is a user-visible
/// configuration / source issue (unknown key, malformed `out`, …) and
/// maps to exit code 40. `UnknownBackend` is the specific case where
/// the target id maps to no backend in this build of kio; the dispatch
/// loop in `run()` may treat it as a skip under
/// `--skip-unsupported-targets`, and otherwise surfaces it as a build
/// error.
enum BackendError {
    Build(String),
    UnknownBackend(String),
}

impl From<String> for BackendError {
    fn from(s: String) -> Self {
        BackendError::Build(s)
    }
}

/// The backend-agnostic recover → optimize → route → annotate prefix,
/// computed **once** per build and shared by the JS and Rust backends.
///
/// Both backends consume an identical `Package<Routed>` derived from the
/// root `Package<Prime>`: `recover_and_optimize_package_cached` (enriched-
/// cache-backed) → `recover_to_low::lower` → `annotate_escapes` →
/// `annotate_lifetime`. Only the recover+optimize step is on-disk-cached;
/// the route + capability passes are not, so before this hoist each
/// target re-ran them — and because the per-target dispatch loop is
/// rayon-parallel, that was duplicated *concurrent* CPU.
///
/// Computing it here once realizes the enriched-cache module's contract
/// that recovery is paid "once per module per package-edit, not once per
/// (module, target) pair" — for the route + capability passes too.
///
/// Laziness via [`std::sync::OnceLock`] is load-bearing for the builds
/// that need *nothing*: an artifact-cache **hit** in the sole consuming
/// backend returns before it ever asks for the routed package, so a warm
/// single-target build computes nothing, and the kio-prime backend never
/// asks (it consumes `Package<Prime>` directly). When more than one
/// target is selected, the orchestrator forces the computation once up
/// front, before the rayon per-target dispatch, rather than letting a
/// backend trigger `get_or_init` mid-dispatch: the init is itself
/// rayon-parallel and `get_or_init` parks the losing backend's rayon
/// worker, which deadlocks the pool under load. See the dispatch phase
/// for the full argument. Either way the value is shared and the result
/// is byte-identical to each backend recomputing — only wall-clock moves.
struct SharedRoutedPackage<'a> {
    package: &'a Package<crate::ast::Prime>,
    enriched_cache: &'a crate::cache::enriched::EnrichedCache,
    package_label: &'a str,
    routed: std::sync::OnceLock<Package<crate::ast::Routed>>,
}

impl<'a> SharedRoutedPackage<'a> {
    fn new(
        package: &'a Package<crate::ast::Prime>,
        enriched_cache: &'a crate::cache::enriched::EnrichedCache,
        package_label: &'a str,
    ) -> Self {
        Self {
            package,
            enriched_cache,
            package_label,
            routed: std::sync::OnceLock::new(),
        }
    }

    /// Borrow the shared `Package<Routed>`, computing it on first call.
    fn get(&self) -> &Package<crate::ast::Routed> {
        self.routed.get_or_init(|| {
            let prime_shape_start = Instant::now();
            build_timing::log_shared_package_shape(self.package_label, "prime", self.package);
            let prime_shape_ms = build_timing::elapsed_ms(prime_shape_start);

            let enriched_start = Instant::now();
            let recovered = crate::cache::enriched::recover_and_optimize_package_cached(
                self.package,
                self.enriched_cache,
            );
            let enriched_ms = build_timing::elapsed_ms(enriched_start);
            let enriched_shape_start = Instant::now();
            build_timing::log_shared_package_shape(self.package_label, "enriched", &recovered);
            let enriched_shape_ms = build_timing::elapsed_ms(enriched_shape_start);

            // Resolution lowering: `Package<Enriched>` → `Package<Routed>`,
            // carrying pre-classified call / value-position routing on each
            // node. JS (session 10) and Rust (session 11) both consume the
            // Routed phase directly; no unlower round-trip.
            let route_start = Instant::now();
            let routed = crate::pass::recover_to_low::lower(&recovered);
            // Capability annotation: each pass stamps one position-specific
            // annotation on the Routed AST. `annotate_escapes` writes
            // `captured_from`, which `annotate_lifetime` reads to derive
            // `Lifetime`. JS ignores both annotations; the Rust backend reads
            // only the derived `Lifetime`.
            let routed = crate::pass::capabilities::annotate_escapes(routed);
            let routed = crate::pass::capabilities::annotate_lifetime(routed);
            let route_ms = build_timing::elapsed_ms(route_start);
            let routed_shape_start = Instant::now();
            build_timing::log_shared_package_shape(self.package_label, "routed", &routed);
            let routed_shape_ms = build_timing::elapsed_ms(routed_shape_start);

            build_timing::log_shared_prefix(
                self.package_label,
                &[
                    ("prime_shape", prime_shape_ms),
                    ("enriched", enriched_ms),
                    ("enriched_shape", enriched_shape_ms),
                    ("route", route_ms),
                    ("routed_shape", routed_shape_ms),
                ],
            );
            routed
        })
    }
}

/// Hand a target block off to the backend named by its bare target id.
fn dispatch_target(
    cwd: &Path,
    target: &TargetBlock,
    workspace: &check::TypedPackageCollection,
    routed: &SharedRoutedPackage,
    emit_cache: &crate::cache::emit::EmitCache,
    artifact_cache: &crate::cache::artifact::ArtifactCache,
) -> Result<(), BackendError> {
    let root = workspace
        .root_package
        .as_ref()
        .expect("kio build passes skip_ok = false, so the root is always typechecked");
    match BuildTarget::from_id(&target.id) {
        Some(BuildTarget::Js) => {
            js_backend(cwd, target, workspace, routed, emit_cache, artifact_cache)
        }
        Some(BuildTarget::Ts) => {
            ts_backend(cwd, target, workspace, routed, emit_cache, artifact_cache)
        }
        Some(BuildTarget::Go) => go_backend(cwd, target, workspace, routed, artifact_cache),
        Some(BuildTarget::Python) => python_backend(cwd, target, workspace, routed, artifact_cache),
        Some(BuildTarget::Java) => java_backend(cwd, target, workspace, routed, artifact_cache),
        Some(BuildTarget::Swift) => swift_backend(cwd, target, workspace, routed, artifact_cache),
        Some(BuildTarget::Haskell) => {
            haskell_backend(cwd, target, workspace, routed, artifact_cache)
        }
        Some(BuildTarget::KioPrime) => {
            kio_prime_backend(cwd, target, root, emit_cache, artifact_cache)
        }
        Some(BuildTarget::Rust) => rust_backend(cwd, target, workspace, routed, artifact_cache),
        None => Err(BackendError::UnknownBackend(format!(
            "unknown backend `{}` (recognized: {})",
            target.id,
            BuildTarget::ALL
                .iter()
                .map(|target| format!("`{}`", target.id()))
                .collect::<Vec<_>>()
                .join(", "),
        ))),
    }
}

/// JS backend. Validates target keys, creates the output directory,
/// and emits a single `<ns>.js` ES module exporting the branded
/// `create<Handle>(host)` factory. The namespace (the artifact stem)
/// defaults to the kio package name; the `namespace` key overrides it.
/// Every Kio module gets inlined into the factory body as a
/// closure-scoped IIFE; nothing escapes to `globalThis`. See
/// `specs/backends/js.md` § Output layout.
fn js_backend(
    cwd: &Path,
    target: &TargetBlock,
    workspace: &check::TypedPackageCollection,
    routed: &SharedRoutedPackage,
    emit_cache: &crate::cache::emit::EmitCache,
    artifact_cache: &crate::cache::artifact::ArtifactCache,
) -> Result<(), BackendError> {
    let build_start = Instant::now();
    let package = workspace
        .root_package
        .as_ref()
        .expect("kio build passes skip_ok = false, so the root is always typechecked");
    let package_label = build_timing::package_label(package);
    let mut out_dir: Option<&str> = None;
    let mut namespace: Option<&str> = None;
    for entry in &target.entries {
        match BuildTarget::Js.key(&entry.key) {
            Some("out") => {
                if out_dir.is_some() {
                    return Err(BackendError::Build("duplicate key `out`".to_owned()));
                }
                out_dir = Some(entry.value.as_str());
            }
            Some("namespace") => {
                if namespace.is_some() {
                    return Err(BackendError::Build("duplicate key `namespace`".to_owned()));
                }
                backends::namespace::validate_js_namespace(&entry.value)
                    .map_err(BackendError::Build)?;
                namespace = Some(entry.value.as_str());
            }
            _ => {
                let other = &entry.key;
                return Err(BackendError::Build(format!(
                    "unknown key `{other}` for the `js` backend \
                     (recognized: `out`, `namespace`)"
                )));
            }
        }
    }
    let out =
        out_dir.ok_or_else(|| BackendError::Build("missing required key `out`".to_owned()))?;
    let out_path = Path::new(out);
    if out_path.is_absolute() {
        return Err(BackendError::Build(format!(
            "`out` must be a path relative to the package root, got `{out}`"
        )));
    }
    let target_dir = cwd.join(out_path);
    // The sig is a declared build input; folding its bytes into the
    // cache key keeps a sealed sig change from serving a stale artifact.
    // JS is removal-tolerant (a removed host item is simply absent from
    // the emitted module — no re-emit), so the JS output is unchanged by
    // a sig edit; the key still folds the bytes for uniformity (a
    // spurious-but-sound miss). See `append_package_fingerprint`.
    let sig_content = read_sig_content(
        cwd,
        &package
            .package_file()
            .expect("kio build validates the root package file before backend dispatch")
            .package_name,
    )?;
    let artifact_key = artifact_key_for_workspace_if_enabled(
        artifact_cache,
        "js",
        target,
        workspace,
        sig_content.as_deref(),
    );
    let artifact_restore_start = Instant::now();
    let artifact_hit = restore_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_restore_ms = build_timing::elapsed_ms(artifact_restore_start);
    if artifact_hit {
        build_timing::log_target(
            &package_label,
            target,
            "js",
            true,
            &[("artifact_restore", artifact_restore_ms)],
            build_timing::elapsed_ms(build_start),
        );
        return Ok(());
    }
    let prepare_start = Instant::now();
    prepare_target_dir(&target_dir)?;
    let prepare_ms = build_timing::elapsed_ms(prepare_start);

    let pkg_name = package
        .package_file()
        .expect("kio build validates the root package file before backend dispatch")
        .package_name
        .clone();
    let ns: String = namespace
        .map(|s| s.to_owned())
        .unwrap_or_else(|| backends::namespace::default_js_namespace(&pkg_name));

    // The recover → optimize → route → annotate prefix is backend-agnostic
    // and computed once for the whole build (see `SharedRoutedPackage`); on
    // the first backend that misses the artifact cache it runs, and every
    // other recovering backend shares the same `Package<Routed>`. The JS
    // backend consumes `Module<Routed>` directly (session 10 migrated
    // `backends::js::emit` onto the Routed phase); no unlower round-trip is needed.
    let recovered_routed = routed.get();

    // A package build emits one factory module for the local package.
    let emit_start = Instant::now();
    let js = backends::js::lower_package_to_factory_module_cached(
        recovered_routed,
        &ns,
        Some(emit_cache),
    )
    .map_err(|e| {
        BackendError::Build(format!(
            "cannot emit factory module for package `{pkg_name}`: {}",
            e.message
        ))
    })?;
    let emit_ms = build_timing::elapsed_ms(emit_start);
    let out_file = target_dir.join(format!("{ns}.js"));
    let write_start = Instant::now();
    fs::write(&out_file, js).map_err(|e| {
        BackendError::Build(format!("cannot write `{}`: {e}", DisplayPath(&out_file)))
    })?;
    let write_ms = build_timing::elapsed_ms(write_start);

    let artifact_store_start = Instant::now();
    store_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_store_ms = build_timing::elapsed_ms(artifact_store_start);
    build_timing::log_target(
        &package_label,
        target,
        "js",
        false,
        &[
            ("artifact_restore", artifact_restore_ms),
            ("prepare", prepare_ms),
            ("emit", emit_ms),
            ("write", write_ms),
            ("artifact_store", artifact_store_ms),
        ],
        build_timing::elapsed_ms(build_start),
    );
    Ok(())
}

/// TypeScript backend. A **pure-skin** target: it writes `<ns>.js` by
/// reusing the JS lowering (`backends::js::lower_package_to_factory_module`)
/// with the same resolved namespace, so the runtime artifact is
/// **byte-identical** to `target=js`, plus a generated `<ns>.d.ts`
/// sidecar — the typed FFI skin (`backends::ts::lower_package_to_dts`).
/// The namespace (the shared artifact stem) defaults to the kio package
/// name; the `namespace` key overrides it. No separate body emitter: the
/// body *is* the JS backend's, and the `.d.ts` is type annotations for it
/// (per `specs/backends/ts.md`).
fn ts_backend(
    cwd: &Path,
    target: &TargetBlock,
    workspace: &check::TypedPackageCollection,
    routed: &SharedRoutedPackage,
    emit_cache: &crate::cache::emit::EmitCache,
    artifact_cache: &crate::cache::artifact::ArtifactCache,
) -> Result<(), BackendError> {
    let build_start = Instant::now();
    let package = workspace
        .root_package
        .as_ref()
        .expect("kio build passes skip_ok = false, so the root is always typechecked");
    let package_label = build_timing::package_label(package);
    let mut out_dir: Option<&str> = None;
    let mut namespace: Option<&str> = None;
    for entry in &target.entries {
        match BuildTarget::Ts.key(&entry.key) {
            Some("out") => {
                if out_dir.is_some() {
                    return Err(BackendError::Build("duplicate key `out`".to_owned()));
                }
                out_dir = Some(entry.value.as_str());
            }
            Some("namespace") => {
                if namespace.is_some() {
                    return Err(BackendError::Build("duplicate key `namespace`".to_owned()));
                }
                backends::namespace::validate_js_namespace(&entry.value)
                    .map_err(BackendError::Build)?;
                namespace = Some(entry.value.as_str());
            }
            _ => {
                let other = &entry.key;
                return Err(BackendError::Build(format!(
                    "unknown key `{other}` for the `ts` backend \
                     (recognized: `out`, `namespace`)"
                )));
            }
        }
    }
    let out =
        out_dir.ok_or_else(|| BackendError::Build("missing required key `out`".to_owned()))?;
    let out_path = Path::new(out);
    if out_path.is_absolute() {
        return Err(BackendError::Build(format!(
            "`out` must be a path relative to the package root, got `{out}`"
        )));
    }
    let target_dir = cwd.join(out_path);
    // The sig is a declared build input. TS reuses the removal-tolerant JS
    // runtime verbatim; only the `.d.ts` reads sealed history, retaining
    // optional deprecated host properties and their exact deprecated type
    // dependencies.
    let sig_content = read_sig_content(
        cwd,
        &package
            .package_file()
            .expect("kio build validates the root package file before backend dispatch")
            .package_name,
    )?;
    let artifact_key = artifact_key_for_workspace_if_enabled(
        artifact_cache,
        "ts",
        target,
        workspace,
        sig_content.as_deref(),
    );
    let artifact_restore_start = Instant::now();
    let artifact_hit = restore_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_restore_ms = build_timing::elapsed_ms(artifact_restore_start);
    if artifact_hit {
        build_timing::log_target(
            &package_label,
            target,
            "ts",
            true,
            &[("artifact_restore", artifact_restore_ms)],
            build_timing::elapsed_ms(build_start),
        );
        return Ok(());
    }
    let prepare_start = Instant::now();
    prepare_target_dir(&target_dir)?;
    let prepare_ms = build_timing::elapsed_ms(prepare_start);

    let pkg_name = package
        .package_file()
        .expect("kio build validates the root package file before backend dispatch")
        .package_name
        .clone();
    let ns: String = namespace
        .map(|s| s.to_owned())
        .unwrap_or_else(|| backends::namespace::default_js_namespace(&pkg_name));

    let recovered_routed = routed.get();

    let sig = match &sig_content {
        Some(content) => Some(replay_sig_for_build(content, &pkg_name)?),
        None => None,
    };

    // The `.js` is the JS backend's, byte-identical: the *same* emitter
    // entry point, same shared `Package<Routed>`, same emit cache, same
    // resolved namespace (so the branded factory bytes match). The
    // `.d.ts` is the only TS-specific artifact. It renders the live FFI skin
    // plus the optional deprecated source facade prepared from sealed history,
    // all branded off the same `ns`.
    let emit_start = Instant::now();
    let js = backends::js::lower_package_to_factory_module_cached(
        recovered_routed,
        &ns,
        Some(emit_cache),
    )
    .map_err(|e| {
        BackendError::Build(format!(
            "cannot emit factory module for package `{pkg_name}`: {}",
            e.message
        ))
    })?;
    let dts =
        backends::ts::lower_package_to_dts_with_signature(recovered_routed, &ns, sig.as_ref())
            .map_err(|e| {
                BackendError::Build(format!(
                    "cannot emit `.d.ts` skin for package `{pkg_name}`: {}",
                    e.message
                ))
            })?;
    let emit_ms = build_timing::elapsed_ms(emit_start);

    let write_start = Instant::now();
    let js_file = target_dir.join(format!("{ns}.js"));
    fs::write(&js_file, js).map_err(|e| {
        BackendError::Build(format!("cannot write `{}`: {e}", DisplayPath(&js_file)))
    })?;
    let dts_file = target_dir.join(format!("{ns}.d.ts"));
    fs::write(&dts_file, dts).map_err(|e| {
        BackendError::Build(format!("cannot write `{}`: {e}", DisplayPath(&dts_file)))
    })?;
    let write_ms = build_timing::elapsed_ms(write_start);

    let artifact_store_start = Instant::now();
    store_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_store_ms = build_timing::elapsed_ms(artifact_store_start);
    build_timing::log_target(
        &package_label,
        target,
        "ts",
        false,
        &[
            ("artifact_restore", artifact_restore_ms),
            ("prepare", prepare_ms),
            ("emit", emit_ms),
            ("write", write_ms),
            ("artifact_store", artifact_store_ms),
        ],
        build_timing::elapsed_ms(build_start),
    );
    Ok(())
}

/// Go backend. Validates target keys, creates the output directory, and
/// emits a self-contained multi-file Go package (`pkg.go`, `host.go`,
/// `shapes.go`, `ffi.go`, `kio_runtime.go`, all declaring the package's
/// namespace as their `package` clause) exposing a branded
/// `Create<Handle>(host)` factory. The namespace defaults to the kio
/// package name; the `namespace` key overrides it. Every emitted file
/// lives flat at the top of the output directory (a Go package is one
/// directory). See `specs/backends/go.md` § Output layout.
fn go_backend(
    cwd: &Path,
    target: &TargetBlock,
    workspace: &check::TypedPackageCollection,
    routed: &SharedRoutedPackage,
    artifact_cache: &crate::cache::artifact::ArtifactCache,
) -> Result<(), BackendError> {
    let build_start = Instant::now();
    let package = workspace
        .root_package
        .as_ref()
        .expect("kio build passes skip_ok = false, so the root is always typechecked");
    let package_label = build_timing::package_label(package);
    let mut out_dir: Option<&str> = None;
    let mut namespace: Option<&str> = None;
    for entry in &target.entries {
        match BuildTarget::Go.key(&entry.key) {
            Some("out") => {
                if out_dir.is_some() {
                    return Err(BackendError::Build("duplicate key `out`".to_owned()));
                }
                out_dir = Some(entry.value.as_str());
            }
            Some("namespace") => {
                if namespace.is_some() {
                    return Err(BackendError::Build("duplicate key `namespace`".to_owned()));
                }
                backends::namespace::validate_go_namespace(&entry.value)
                    .map_err(BackendError::Build)?;
                namespace = Some(entry.value.as_str());
            }
            _ => {
                let other = &entry.key;
                return Err(BackendError::Build(format!(
                    "unknown key `{other}` for the `go` backend \
                     (recognized: `out`, `namespace`)"
                )));
            }
        }
    }
    let out =
        out_dir.ok_or_else(|| BackendError::Build("missing required key `out`".to_owned()))?;
    let out_path = Path::new(out);
    if out_path.is_absolute() {
        return Err(BackendError::Build(format!(
            "`out` must be a path relative to the package root, got `{out}`"
        )));
    }
    let target_dir = cwd.join(out_path);
    let sig_content = read_sig_content(
        cwd,
        &package
            .package_file()
            .expect("kio build validates the root package file before backend dispatch")
            .package_name,
    )?;
    let artifact_key = artifact_key_for_workspace_if_enabled(
        artifact_cache,
        "go",
        target,
        workspace,
        sig_content.as_deref(),
    );
    let artifact_restore_start = Instant::now();
    let artifact_hit = restore_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_restore_ms = build_timing::elapsed_ms(artifact_restore_start);
    if artifact_hit {
        build_timing::log_target(
            &package_label,
            target,
            "go",
            true,
            &[("artifact_restore", artifact_restore_ms)],
            build_timing::elapsed_ms(build_start),
        );
        return Ok(());
    }
    let prepare_start = Instant::now();
    prepare_target_dir(&target_dir)?;
    let prepare_ms = build_timing::elapsed_ms(prepare_start);

    let pkg_name = package
        .package_file()
        .expect("kio build validates the root package file before backend dispatch")
        .package_name
        .clone();
    let ns: String = namespace
        .map(|s| s.to_owned())
        .unwrap_or_else(|| backends::namespace::default_go_namespace(&pkg_name));

    let recovered_root = routed.get();

    let sig = match &sig_content {
        Some(content) => Some(replay_sig_for_build(content, &pkg_name)?),
        None => None,
    };
    let emit_start = Instant::now();
    let go_pkg = backends::go::lower_package_with_signature(recovered_root, &ns, sig.as_ref())
        .map_err(|e| BackendError::Build(format!("cannot emit package for `{pkg_name}`: {e}")))?;
    let emit_ms = build_timing::elapsed_ms(emit_start);

    let write_start = Instant::now();
    let write = |rel: &str, content: &str| -> Result<(), BackendError> {
        let path = target_dir.join(rel);
        fs::write(&path, content)
            .map_err(|e| BackendError::Build(format!("cannot write `{}`: {e}", DisplayPath(&path))))
    };
    write("pkg.go", &go_pkg.pkg_go)?;
    write("host.go", &go_pkg.host_go)?;
    write("shapes.go", &go_pkg.shapes_go)?;
    write("ffi.go", &go_pkg.ffi_go)?;
    write(backends::go::RUNTIME_SUPPORT_FILE_PATH, &go_pkg.runtime_go)?;
    let write_ms = build_timing::elapsed_ms(write_start);

    let artifact_store_start = Instant::now();
    store_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_store_ms = build_timing::elapsed_ms(artifact_store_start);
    build_timing::log_target(
        &package_label,
        target,
        "go",
        false,
        &[
            ("artifact_restore", artifact_restore_ms),
            ("prepare", prepare_ms),
            ("emit", emit_ms),
            ("write", write_ms),
            ("artifact_store", artifact_store_ms),
        ],
        build_timing::elapsed_ms(build_start),
    );
    Ok(())
}

/// Python backend. Validates target keys, creates the output directory,
/// and emits an `<ns>.py` module exposing a branded `create_<ns>(host)`
/// factory plus an `<ns>/` typed-stub package declaring the same public
/// surface for pyright / mypy hosts. The namespace (the module
/// stem) defaults to the kio package name; the `namespace` key overrides
/// it. See `specs/backends/python.md` § Output layout and § Typed stub.
fn python_backend(
    cwd: &Path,
    target: &TargetBlock,
    workspace: &check::TypedPackageCollection,
    routed: &SharedRoutedPackage,
    artifact_cache: &crate::cache::artifact::ArtifactCache,
) -> Result<(), BackendError> {
    let build_start = Instant::now();
    let package = workspace
        .root_package
        .as_ref()
        .expect("kio build passes skip_ok = false, so the root is always typechecked");
    let package_label = build_timing::package_label(package);
    let mut out_dir: Option<&str> = None;
    let mut namespace: Option<&str> = None;
    for entry in &target.entries {
        match BuildTarget::Python.key(&entry.key) {
            Some("out") => {
                if out_dir.is_some() {
                    return Err(BackendError::Build("duplicate key `out`".to_owned()));
                }
                out_dir = Some(entry.value.as_str());
            }
            Some("namespace") => {
                if namespace.is_some() {
                    return Err(BackendError::Build("duplicate key `namespace`".to_owned()));
                }
                backends::namespace::validate_python_namespace(&entry.value)
                    .map_err(BackendError::Build)?;
                namespace = Some(entry.value.as_str());
            }
            _ => {
                let other = &entry.key;
                return Err(BackendError::Build(format!(
                    "unknown key `{other}` for the `python` backend \
                     (recognized: `out`, `namespace`)"
                )));
            }
        }
    }
    let out =
        out_dir.ok_or_else(|| BackendError::Build("missing required key `out`".to_owned()))?;
    let out_path = Path::new(out);
    if out_path.is_absolute() {
        return Err(BackendError::Build(format!(
            "`out` must be a path relative to the package root, got `{out}`"
        )));
    }
    let target_dir = cwd.join(out_path);
    let pkg_name = package
        .package_file()
        .expect("kio build validates the root package file before backend dispatch")
        .package_name
        .clone();
    let sig_content = read_sig_content(cwd, &pkg_name)?;
    // Python's artifacts are live-only, so sealed history does not partition
    // their cache identity. A present changelog remains an authoritative build
    // input: validate its complete replay before accepting even a cache hit.
    if let Some(content) = &sig_content {
        replay_sig_for_build(content, &pkg_name)?;
    }
    let artifact_key =
        artifact_key_for_workspace_if_enabled(artifact_cache, "python", target, workspace, None);
    let artifact_restore_start = Instant::now();
    let artifact_hit = restore_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_restore_ms = build_timing::elapsed_ms(artifact_restore_start);
    if artifact_hit {
        build_timing::log_target(
            &package_label,
            target,
            "python",
            true,
            &[("artifact_restore", artifact_restore_ms)],
            build_timing::elapsed_ms(build_start),
        );
        return Ok(());
    }
    let prepare_start = Instant::now();
    prepare_target_dir(&target_dir)?;
    let prepare_ms = build_timing::elapsed_ms(prepare_start);

    let ns: String = namespace
        .map(|s| s.to_owned())
        .unwrap_or_else(|| backends::namespace::default_python_namespace(&pkg_name));
    let recovered_routed = routed.get();

    let emit_start = Instant::now();
    let py = backends::python::lower_package_to_module(recovered_routed, &ns).map_err(|e| {
        BackendError::Build(format!(
            "cannot emit Python module for package `{pkg_name}`: {}",
            e.message
        ))
    })?;
    // The typed-stub package declares the same public surface for pyright /
    // mypy hosts (`specs/backends/python.md` § Typed stub). Declarations
    // only; the sibling `.py` remains the runtime.
    let stub =
        backends::python::lower_package_to_stub_package(recovered_routed, &ns).map_err(|e| {
            BackendError::Build(format!(
                "cannot emit typed-stub package for package `{pkg_name}`: {}",
                e.message
            ))
        })?;
    let emit_ms = build_timing::elapsed_ms(emit_start);

    let write_start = Instant::now();
    let py_file = target_dir.join(format!("{ns}.py"));
    fs::write(&py_file, py).map_err(|e| {
        BackendError::Build(format!("cannot write `{}`: {e}", DisplayPath(&py_file)))
    })?;
    let stub_dir = target_dir.join(&ns);
    fs::create_dir_all(&stub_dir).map_err(|e| {
        BackendError::Build(format!("cannot create `{}`: {e}", DisplayPath(&stub_dir)))
    })?;
    for (relative, content) in stub.files() {
        let stub_file = stub_dir.join(relative);
        fs::write(&stub_file, content).map_err(|e| {
            BackendError::Build(format!("cannot write `{}`: {e}", DisplayPath(&stub_file)))
        })?;
    }
    let write_ms = build_timing::elapsed_ms(write_start);

    let artifact_store_start = Instant::now();
    store_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_store_ms = build_timing::elapsed_ms(artifact_store_start);
    build_timing::log_target(
        &package_label,
        target,
        "python",
        false,
        &[
            ("artifact_restore", artifact_restore_ms),
            ("prepare", prepare_ms),
            ("emit", emit_ms),
            ("write", write_ms),
            ("artifact_store", artifact_store_ms),
        ],
        build_timing::elapsed_ms(build_start),
    );
    Ok(())
}

/// Java backend. Validates target keys, creates the output directory,
/// and emits the four-file typed facade (`<Handle>.java`,
/// `<Handle>Host.java`, `Shapes.java`, `KioRuntime.java`) under the
/// package-namespace directory per `specs/backends/java.md`.
fn java_backend(
    cwd: &Path,
    target: &TargetBlock,
    workspace: &check::TypedPackageCollection,
    routed: &SharedRoutedPackage,
    artifact_cache: &crate::cache::artifact::ArtifactCache,
) -> Result<(), BackendError> {
    let build_start = Instant::now();
    let package = workspace
        .root_package
        .as_ref()
        .expect("kio build passes skip_ok = false, so the root is always typechecked");
    let package_label = build_timing::package_label(package);
    let mut out_dir: Option<&str> = None;
    let mut namespace: Option<&str> = None;
    for entry in &target.entries {
        match BuildTarget::Java.key(&entry.key) {
            Some("out") => {
                if out_dir.is_some() {
                    return Err(BackendError::Build("duplicate key `out`".to_owned()));
                }
                out_dir = Some(entry.value.as_str());
            }
            Some("namespace") => {
                if namespace.is_some() {
                    return Err(BackendError::Build("duplicate key `namespace`".to_owned()));
                }
                backends::namespace::validate_java_namespace(&entry.value)
                    .map_err(BackendError::Build)?;
                namespace = Some(entry.value.as_str());
            }
            _ => {
                let other = &entry.key;
                return Err(BackendError::Build(format!(
                    "unknown key `{other}` for the `java` backend \
                     (recognized: `out`, `namespace`)"
                )));
            }
        }
    }
    let out =
        out_dir.ok_or_else(|| BackendError::Build("missing required key `out`".to_owned()))?;
    let out_path = Path::new(out);
    if out_path.is_absolute() {
        return Err(BackendError::Build(format!(
            "`out` must be a path relative to the package root, got `{out}`"
        )));
    }
    let target_dir = cwd.join(out_path);
    let sig_content = read_sig_content(
        cwd,
        &package
            .package_file()
            .expect("kio build validates the root package file before backend dispatch")
            .package_name,
    )?;
    let artifact_key = artifact_key_for_workspace_if_enabled(
        artifact_cache,
        "java",
        target,
        workspace,
        sig_content.as_deref(),
    );
    let artifact_restore_start = Instant::now();
    let artifact_hit = restore_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_restore_ms = build_timing::elapsed_ms(artifact_restore_start);
    if artifact_hit {
        build_timing::log_target(
            &package_label,
            target,
            "java",
            true,
            &[("artifact_restore", artifact_restore_ms)],
            build_timing::elapsed_ms(build_start),
        );
        return Ok(());
    }
    let prepare_start = Instant::now();
    prepare_target_dir(&target_dir)?;
    let prepare_ms = build_timing::elapsed_ms(prepare_start);

    let pkg_name = package
        .package_file()
        .expect("kio build validates the root package file before backend dispatch")
        .package_name
        .clone();
    let namespace_str: String = namespace
        .map(|s| s.to_owned())
        .unwrap_or_else(|| backends::namespace::default_java_namespace(&pkg_name));
    let recovered_routed = routed.get();

    let sig = match &sig_content {
        Some(content) => Some(replay_sig_for_build(content, &pkg_name)?),
        None => None,
    };
    let emit_start = Instant::now();
    let java = backends::java::lower_package(recovered_routed, &namespace_str, sig.as_ref())
        .map_err(|e| {
            BackendError::Build(format!(
                "cannot emit Java source for package `{pkg_name}`: {}",
                e.message
            ))
        })?;
    let emit_ms = build_timing::elapsed_ms(emit_start);

    let write_start = Instant::now();
    let ns_dir = namespace_str
        .split('.')
        .fold(target_dir.clone(), |dir, seg| dir.join(seg));
    fs::create_dir_all(&ns_dir).map_err(|e| {
        BackendError::Build(format!("cannot create `{}`: {e}", DisplayPath(&ns_dir)))
    })?;
    let write = |name: &str, content: &str| -> Result<(), BackendError> {
        let path = ns_dir.join(name);
        fs::write(&path, content)
            .map_err(|e| BackendError::Build(format!("cannot write `{}`: {e}", DisplayPath(&path))))
    };
    write(&format!("{}.java", java.handle), &java.handle_java)?;
    write(&format!("{}Host.java", java.handle), &java.host_java)?;
    write("Shapes.java", &java.shapes_java)?;
    write("KioRuntime.java", &java.runtime_java)?;
    let write_ms = build_timing::elapsed_ms(write_start);

    let artifact_store_start = Instant::now();
    store_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_store_ms = build_timing::elapsed_ms(artifact_store_start);
    build_timing::log_target(
        &package_label,
        target,
        "java",
        false,
        &[
            ("artifact_restore", artifact_restore_ms),
            ("prepare", prepare_ms),
            ("emit", emit_ms),
            ("write", write_ms),
            ("artifact_store", artifact_store_ms),
        ],
        build_timing::elapsed_ms(build_start),
    );
    Ok(())
}

/// Swift backend. Validates target keys, creates the output directory, and
/// emits a self-contained multi-file Swift package (`pkg.swift`,
/// `host.swift`, `shapes.swift`, `ffi.swift`, `kio_runtime.swift`) exposing
/// a branded `create<Handle>(host:)` factory. The module name is the
/// package's namespace (published in the `pkg.swift` marker, imposed at
/// compile time via `-module-name`, since Swift sources carry no module
/// declaration); it defaults to the PascalCase of the kio package name and
/// the `namespace` key overrides it. See `specs/backends/swift.md`
/// § Output layout.
fn swift_backend(
    cwd: &Path,
    target: &TargetBlock,
    workspace: &check::TypedPackageCollection,
    routed: &SharedRoutedPackage,
    artifact_cache: &crate::cache::artifact::ArtifactCache,
) -> Result<(), BackendError> {
    let build_start = Instant::now();
    let package = workspace
        .root_package
        .as_ref()
        .expect("kio build passes skip_ok = false, so the root is always typechecked");
    let package_label = build_timing::package_label(package);
    let mut out_dir: Option<&str> = None;
    let mut namespace: Option<&str> = None;
    for entry in &target.entries {
        match BuildTarget::Swift.key(&entry.key) {
            Some("out") => {
                if out_dir.is_some() {
                    return Err(BackendError::Build("duplicate key `out`".to_owned()));
                }
                out_dir = Some(entry.value.as_str());
            }
            Some("namespace") => {
                if namespace.is_some() {
                    return Err(BackendError::Build("duplicate key `namespace`".to_owned()));
                }
                backends::namespace::validate_swift_namespace(&entry.value)
                    .map_err(BackendError::Build)?;
                namespace = Some(entry.value.as_str());
            }
            _ => {
                let other = &entry.key;
                return Err(BackendError::Build(format!(
                    "unknown key `{other}` for the `swift` backend \
                     (recognized: `out`, `namespace`)"
                )));
            }
        }
    }
    let out =
        out_dir.ok_or_else(|| BackendError::Build("missing required key `out`".to_owned()))?;
    let out_path = Path::new(out);
    if out_path.is_absolute() {
        return Err(BackendError::Build(format!(
            "`out` must be a path relative to the package root, got `{out}`"
        )));
    }
    let target_dir = cwd.join(out_path);
    let sig_content = read_sig_content(
        cwd,
        &package
            .package_file()
            .expect("kio build validates the root package file before backend dispatch")
            .package_name,
    )?;
    let artifact_key = artifact_key_for_workspace_if_enabled(
        artifact_cache,
        "swift",
        target,
        workspace,
        sig_content.as_deref(),
    );
    let artifact_restore_start = Instant::now();
    let artifact_hit = restore_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_restore_ms = build_timing::elapsed_ms(artifact_restore_start);
    if artifact_hit {
        build_timing::log_target(
            &package_label,
            target,
            "swift",
            true,
            &[("artifact_restore", artifact_restore_ms)],
            build_timing::elapsed_ms(build_start),
        );
        return Ok(());
    }
    let prepare_start = Instant::now();
    prepare_target_dir(&target_dir)?;
    let prepare_ms = build_timing::elapsed_ms(prepare_start);

    let pkg_name = package
        .package_file()
        .expect("kio build validates the root package file before backend dispatch")
        .package_name
        .clone();

    let ns: String = namespace
        .map(|s| s.to_owned())
        .unwrap_or_else(|| backends::namespace::default_swift_namespace(&pkg_name));

    let recovered_root = routed.get();

    let sig = match &sig_content {
        Some(content) => Some(replay_sig_for_build(content, &pkg_name)?),
        None => None,
    };

    let emit_start = Instant::now();
    let swift_pkg =
        backends::swift::lower_package_with_signature(recovered_root, &ns, sig.as_ref()).map_err(
            |e| BackendError::Build(format!("cannot emit package for `{pkg_name}`: {e}")),
        )?;
    let emit_ms = build_timing::elapsed_ms(emit_start);

    let write_start = Instant::now();
    let write = |rel: &str, content: &str| -> Result<(), BackendError> {
        let path = target_dir.join(rel);
        fs::write(&path, content)
            .map_err(|e| BackendError::Build(format!("cannot write `{}`: {e}", DisplayPath(&path))))
    };
    write("pkg.swift", &swift_pkg.pkg_swift)?;
    write("host.swift", &swift_pkg.host_swift)?;
    write("shapes.swift", &swift_pkg.shapes_swift)?;
    write("ffi.swift", &swift_pkg.ffi_swift)?;
    write(
        backends::swift::RUNTIME_SUPPORT_FILE_PATH,
        &swift_pkg.runtime_swift,
    )?;
    let write_ms = build_timing::elapsed_ms(write_start);

    let artifact_store_start = Instant::now();
    store_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_store_ms = build_timing::elapsed_ms(artifact_store_start);
    build_timing::log_target(
        &package_label,
        target,
        "swift",
        false,
        &[
            ("artifact_restore", artifact_restore_ms),
            ("prepare", prepare_ms),
            ("emit", emit_ms),
            ("write", write_ms),
            ("artifact_store", artifact_store_ms),
        ],
        build_timing::elapsed_ms(build_start),
    );
    Ok(())
}

/// Haskell backend. Validates target keys, creates the output directory,
/// and emits one self-contained Haskell facade (`<Ns>.hs`) exposing a branded
/// `create<Handle>` factory. The namespace defaults to the package name
/// PascalCased; the `namespace` key overrides it. GHC's module-name = path
/// rule maps a dotted namespace to directories. Haskell is the native-HKT
/// family — the body renders the IR at native Haskell types, and every
/// emitted function is monad-polymorphic. See `specs/backends/haskell.md`
/// § Output layout.
fn haskell_backend(
    cwd: &Path,
    target: &TargetBlock,
    workspace: &check::TypedPackageCollection,
    routed: &SharedRoutedPackage,
    artifact_cache: &crate::cache::artifact::ArtifactCache,
) -> Result<(), BackendError> {
    let build_start = Instant::now();
    let package = workspace
        .root_package
        .as_ref()
        .expect("kio build passes skip_ok = false, so the root is always typechecked");
    let package_label = build_timing::package_label(package);
    let mut out_dir: Option<&str> = None;
    let mut namespace: Option<&str> = None;
    for entry in &target.entries {
        match BuildTarget::Haskell.key(&entry.key) {
            Some("out") => {
                if out_dir.is_some() {
                    return Err(BackendError::Build("duplicate key `out`".to_owned()));
                }
                out_dir = Some(entry.value.as_str());
            }
            Some("namespace") => {
                if namespace.is_some() {
                    return Err(BackendError::Build("duplicate key `namespace`".to_owned()));
                }
                backends::namespace::validate_haskell_namespace(&entry.value)
                    .map_err(BackendError::Build)?;
                namespace = Some(entry.value.as_str());
            }
            _ => {
                let other = &entry.key;
                return Err(BackendError::Build(format!(
                    "unknown key `{other}` for the `haskell` backend \
                     (recognized: `out`, `namespace`)"
                )));
            }
        }
    }
    let out =
        out_dir.ok_or_else(|| BackendError::Build("missing required key `out`".to_owned()))?;
    let out_path = Path::new(out);
    if out_path.is_absolute() {
        return Err(BackendError::Build(format!(
            "`out` must be a path relative to the package root, got `{out}`"
        )));
    }
    let target_dir = cwd.join(out_path);
    let sig_content = read_sig_content(
        cwd,
        &package
            .package_file()
            .expect("kio build validates the root package file before backend dispatch")
            .package_name,
    )?;
    let artifact_key = artifact_key_for_workspace_if_enabled(
        artifact_cache,
        "haskell",
        target,
        workspace,
        sig_content.as_deref(),
    );
    let artifact_restore_start = Instant::now();
    let artifact_hit = restore_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_restore_ms = build_timing::elapsed_ms(artifact_restore_start);
    if artifact_hit {
        build_timing::log_target(
            &package_label,
            target,
            "haskell",
            true,
            &[("artifact_restore", artifact_restore_ms)],
            build_timing::elapsed_ms(build_start),
        );
        return Ok(());
    }
    let prepare_start = Instant::now();
    prepare_target_dir(&target_dir)?;
    let prepare_ms = build_timing::elapsed_ms(prepare_start);

    let pkg_name = package
        .package_file()
        .expect("kio build validates the root package file before backend dispatch")
        .package_name
        .clone();

    let ns: String = namespace
        .map(|s| s.to_owned())
        .unwrap_or_else(|| backends::namespace::default_haskell_namespace(&pkg_name));

    let recovered_root = routed.get();

    let sig = match &sig_content {
        Some(content) => Some(replay_sig_for_build(content, &pkg_name)?),
        None => None,
    };
    let emit_start = Instant::now();
    let haskell_pkg =
        backends::haskell::lower_package_with_signature(recovered_root, &ns, sig.as_ref())
            .map_err(|e| {
                BackendError::Build(format!("cannot emit package for `{pkg_name}`: {e}"))
            })?;
    let emit_ms = build_timing::elapsed_ms(emit_start);

    let write_start = Instant::now();
    // The dotted namespace maps to a directory path (GHC's module-name =
    // path rule): package module `<Ns>` at `<Ns>.hs`. Parent directories are
    // created for a dotted namespace.
    let ns_path: PathBuf = ns.split('.').collect();
    let write = |rel: &Path, content: &str| -> Result<(), BackendError> {
        let path = target_dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                BackendError::Build(format!("cannot create `{}`: {e}", DisplayPath(parent)))
            })?;
        }
        fs::write(&path, content)
            .map_err(|e| BackendError::Build(format!("cannot write `{}`: {e}", DisplayPath(&path))))
    };
    write(&ns_path.with_extension("hs"), &haskell_pkg.pkg_hs)?;
    let write_ms = build_timing::elapsed_ms(write_start);

    let artifact_store_start = Instant::now();
    store_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_store_ms = build_timing::elapsed_ms(artifact_store_start);
    build_timing::log_target(
        &package_label,
        target,
        "haskell",
        false,
        &[
            ("artifact_restore", artifact_restore_ms),
            ("prepare", prepare_ms),
            ("emit", emit_ms),
            ("write", write_ms),
            ("artifact_store", artifact_store_ms),
        ],
        build_timing::elapsed_ms(build_start),
    );
    Ok(())
}

/// Kio' backend. Validates the output
/// directory, and emits one `*.kio` file per source module under
/// that directory plus the re-emitted package file (which carries
/// its `build { ... }` block verbatim). The emitted regular-module
/// files are required to be valid Kio' — `ci/infra/kio-prime-check-rs`
/// accepts them without modification.
///
/// The input is `Module<Prime>`, so the AST is already strictly
/// Kio'-shaped: every surface-only variant is uninhabited at the
/// type level, and the emit module reuses `pretty.rs` after a
/// mechanical `Prime → Surface` embed.
///
/// Map a module's **declared** `module a/b;` path to the relative
/// file path its declaration implies under the package root. Under
/// the new spec (per `specs/package.md` § Module-name rules), the
/// declared segments equal the file's path relative to the package
/// root, with `.` as the directory separator and the `.kio`
/// extension stripped: `module util/list;` lives at `util/list.kio`,
/// `module main;` lives at `main.kio`, and a host-using
/// `module pkg/main;` lives at `pkg/main.kio`. The package name is
/// not stripped here — it's already part of the declared name when
/// it appears (rule 2), and absent from it otherwise (rule 1).
fn module_file_relpath(declared_path: &crate::ast::ModulePath) -> PathBuf {
    let segs: Vec<&str> = declared_path
        .segments
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    let mut path = PathBuf::new();
    for seg in &segs[..segs.len().saturating_sub(1)] {
        path.push(seg);
    }
    path.push(format!("{}.kio", segs.last().copied().unwrap_or("module")));
    path
}

/// Prepare a target's output directory: remove any prior contents,
/// then create the directory fresh.
///
/// Each `target <name> { out "..."; }` block in the package
/// file's `build { ... }` block names its own output directory. The
/// directory is owned by `kio build` for that target — its contents
/// are the artifacts of the previous emit and have no other use.
/// Wiping before re-emitting
/// keeps the on-disk tree faithful to the current source: a renamed
/// module no longer leaves its previous file behind, a removed
/// module is removed from the output, and a layout change (e.g. the
/// module-name-rule landing) does not leave the previous layout
/// alongside the new one.
///
/// The scope is **this target's directory only**. Sibling targets
/// (`out/js/`, `out/rust/`, `out/kio-prime/`, …) keep their
/// contents; a build of one target does not invalidate another. The
/// shared cache (e.g. `out/.kio-cache/`) sits at a sibling path
/// outside any one target's `out =` value and is also untouched.
fn prepare_target_dir(target_dir: &Path) -> Result<(), BackendError> {
    // `remove_dir_all` is the right primitive: it succeeds on a
    // missing directory (the common case for a fresh checkout) and
    // recursively removes everything otherwise. We translate any
    // other I/O error to a `BackendError::Build` with the path so
    // the user sees what couldn't be cleaned.
    match fs::remove_dir_all(target_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(BackendError::Build(format!(
                "cannot clean target dir `{}`: {e}",
                DisplayPath(target_dir)
            )));
        }
    }
    fs::create_dir_all(target_dir).map_err(|e| {
        BackendError::Build(format!(
            "cannot create target dir `{}`: {e}",
            DisplayPath(target_dir)
        ))
    })?;
    // Mark this generated-output directory so package discovery prunes
    // it by path identity, not just by the `out` / `target` name
    // heuristic (a custom emit path would otherwise be re-discovered).
    crate::package_collection::mark_generated_dir(target_dir);
    Ok(())
}

fn artifact_key_for_workspace(
    target_id: &str,
    target: &TargetBlock,
    workspace: &check::TypedPackageCollection,
    sig_content: Option<&str>,
) -> crate::cache::artifact::ArtifactCacheKey {
    #[cfg(test)]
    cache_work_counters::record_artifact_key();
    let target_profile = format!("{target:?}");
    let mut input = String::new();
    input.push_str("root\n");
    if let Some(root) = &workspace.root_package {
        append_package_fingerprint(&mut input, root, sig_content);
    }
    crate::cache::artifact::ArtifactCacheKey::new(
        CacheTarget::new(target_id),
        ArtifactTargetProfileFingerprint::from_bytes(target_profile.as_bytes()),
        ArtifactInputFingerprint::from_parts(&[input.as_bytes()]),
    )
}

fn artifact_key_for_package(
    target_id: &str,
    target: &TargetBlock,
    package: &Package<crate::ast::Prime>,
    sig_content: Option<&str>,
) -> crate::cache::artifact::ArtifactCacheKey {
    #[cfg(test)]
    cache_work_counters::record_artifact_key();
    let target_profile = format!("{target:?}");
    let mut input = String::new();
    append_package_fingerprint(&mut input, package, sig_content);
    crate::cache::artifact::ArtifactCacheKey::new(
        CacheTarget::new(target_id),
        ArtifactTargetProfileFingerprint::from_bytes(target_profile.as_bytes()),
        ArtifactInputFingerprint::from_parts(&[input.as_bytes()]),
    )
}

fn artifact_key_for_workspace_if_enabled(
    cache: &crate::cache::artifact::ArtifactCache,
    target_id: &str,
    target: &TargetBlock,
    workspace: &check::TypedPackageCollection,
    sig_content: Option<&str>,
) -> Option<crate::cache::artifact::ArtifactCacheKey> {
    cache
        .is_enabled()
        .then(|| artifact_key_for_workspace(target_id, target, workspace, sig_content))
}

fn artifact_key_for_package_if_enabled(
    cache: &crate::cache::artifact::ArtifactCache,
    target_id: &str,
    target: &TargetBlock,
    package: &Package<crate::ast::Prime>,
    sig_content: Option<&str>,
) -> Option<crate::cache::artifact::ArtifactCacheKey> {
    cache
        .is_enabled()
        .then(|| artifact_key_for_package(target_id, target, package, sig_content))
}

fn restore_cached_artifact(
    cache: &crate::cache::artifact::ArtifactCache,
    key: Option<&crate::cache::artifact::ArtifactCacheKey>,
    target_dir: &Path,
) -> bool {
    key.is_some_and(|key| cache.restore(key, target_dir))
}

fn store_cached_artifact(
    cache: &crate::cache::artifact::ArtifactCache,
    key: Option<&crate::cache::artifact::ArtifactCacheKey>,
    target_dir: &Path,
) {
    if let Some(key) = key {
        cache.store(key, target_dir);
    }
}

/// Fold a package's identity into the artifact-cache fingerprint: every
/// module's Kio′ bytes, the package file's bytes, and — when the caller's
/// output depends on a `*.sig.kio` changelog — the verbatim signature-file
/// content.
///
/// The sig is a **declared, committed build input** (compatibility
/// facades re-emit retained host items from it, so build output is
/// `f(source, sig)`); folding its content here is the required cache
/// invariant from `specs/versioning.md` § The signature artifact, so a
/// sealed sig change on otherwise-fixed `.kio` source regenerates a
/// different package artifact rather than serving a stale one. Folding the
/// verbatim bytes (rather than the replayed interface) also busts the key on a
/// draft-only edit — a spurious-but-sound miss, since build output
/// tracks only the last sealed generation. A live-only backend may pass
/// `None` after separately validating a present changelog; Python does this so
/// history that cannot affect either emitted artifact also cannot partition
/// their cache identity.
fn append_package_fingerprint(
    out: &mut String,
    package: &Package<crate::ast::Prime>,
    sig_content: Option<&str>,
) {
    for (module_key, entry) in package.modules() {
        out.push_str("module ");
        out.push_str(module_key);
        out.push('\n');
        #[cfg(test)]
        cache_work_counters::record_artifact_render();
        out.push_str(&kio_prime::emit_module(&entry.module));
        out.push('\n');
    }
    if let Some(entry) = package.package_file() {
        out.push_str("package ");
        out.push_str(&entry.package_name);
        out.push('\n');
        #[cfg(test)]
        cache_work_counters::record_artifact_render();
        out.push_str(&kio_prime::emit_package_file(&entry.package_file));
        out.push('\n');
    }
    if let Some(sig) = sig_content {
        out.push_str("sig\n");
        out.push_str(sig);
        out.push('\n');
    }
}

/// Read the package's on-disk `<pkg_name>.sig.kio` content, or `None`
/// when the package ships no signature changelog. Compatibility facades feed
/// the verbatim bytes to [`append_package_fingerprint`] and parse + replay
/// them after an artifact miss. A live-only backend may instead validate them
/// before a cache lookup without including them in the key. A read error other
/// than "not found" is surfaced as a build error — a present-but-unreadable
/// sig is an input the build can't honor, not a silent skip.
fn read_sig_content(cwd: &Path, package_name: &str) -> Result<Option<String>, BackendError> {
    let path = cwd.join(format!("{package_name}.sig.kio"));
    match fs::read_to_string(&path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(BackendError::Build(format!(
            "cannot read signature changelog `{}`: {e}",
            DisplayPath(&path)
        ))),
    }
}

/// Parse + replay a `*.sig.kio` content into `(version, replayed)` for
/// compatibility-facade emission. The header's `v(N)` names the open draft;
/// compatibility output observes only the last sealed generation `v(N-1)`.
/// The whole changelog is replay-validated first so an incoherent draft stays
/// a build error, but the returned interface is replayed only through that
/// sealed boundary. The package-name coherence is checked by the parser
/// (`Some(package_name)` — the file is `<package_name>.sig.kio` and its
/// header must agree).
fn replay_sig_for_build(
    sig_content: &str,
    package_name: &str,
) -> Result<(u32, crate::sig::ReplayedInterface), BackendError> {
    let file = crate::pass::parser::parse_signature_file(sig_content, Some(package_name)).map_err(
        |e| {
            BackendError::Build(format!(
                "signature changelog `{package_name}.sig.kio` failed to parse: {}",
                e.diag().1
            ))
        },
    )?;
    crate::sig::replay(&file).map_err(|e| {
        BackendError::Build(format!(
            "signature changelog `{package_name}.sig.kio` failed to replay: {}",
            e.diag().1
        ))
    })?;
    let version = file.version.saturating_sub(1);
    let replayed = crate::sig::replay_through(&file, version).map_err(|e| {
        BackendError::Build(format!(
            "signature changelog `{package_name}.sig.kio` failed to replay through its last sealed generation: {}",
            e.diag().1
        ))
    })?;
    Ok((version, replayed))
}

fn kio_prime_backend(
    cwd: &Path,
    target: &TargetBlock,
    package: &Package<crate::ast::Prime>,
    emit_cache: &crate::cache::emit::EmitCache,
    artifact_cache: &crate::cache::artifact::ArtifactCache,
) -> Result<(), BackendError> {
    let build_start = Instant::now();
    let package_label = build_timing::package_label(package);
    let mut out_dir: Option<&str> = None;
    for entry in &target.entries {
        match BuildTarget::KioPrime.key(&entry.key) {
            Some("out") => {
                if out_dir.is_some() {
                    return Err(BackendError::Build("duplicate key `out`".to_owned()));
                }
                out_dir = Some(entry.value.as_str());
            }
            _ => {
                let other = &entry.key;
                return Err(BackendError::Build(format!(
                    "unknown key `{other}` for the `kio-prime` backend (only `out` is recognized)"
                )));
            }
        }
    }
    let out =
        out_dir.ok_or_else(|| BackendError::Build("missing required key `out`".to_owned()))?;
    let out_path = Path::new(out);
    if out_path.is_absolute() {
        return Err(BackendError::Build(format!(
            "`out` must be a path relative to the package root, got `{out}`"
        )));
    }
    let target_dir = cwd.join(out_path);
    // The sig is a declared build input; fold its bytes into the cache
    // key for uniformity with the other backends. kio-prime re-emits the
    // package's Kio′ and does not read the sig, so its output is
    // unchanged by a sig edit (a spurious-but-sound miss). See
    // `append_package_fingerprint`.
    let sig_content = read_sig_content(
        cwd,
        &package
            .package_file()
            .expect("kio build validates the root package file before backend dispatch")
            .package_name,
    )?;
    let artifact_key = artifact_key_for_package_if_enabled(
        artifact_cache,
        "kio-prime",
        target,
        package,
        sig_content.as_deref(),
    );
    let artifact_restore_start = Instant::now();
    let artifact_hit = restore_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_restore_ms = build_timing::elapsed_ms(artifact_restore_start);
    if artifact_hit {
        build_timing::log_target(
            &package_label,
            target,
            "kio-prime",
            true,
            &[("artifact_restore", artifact_restore_ms)],
            build_timing::elapsed_ms(build_start),
        );
        return Ok(());
    }
    let prepare_start = Instant::now();
    prepare_target_dir(&target_dir)?;
    let prepare_ms = build_timing::elapsed_ms(prepare_start);

    // One `*.kio` file per regular module. The file lives at the
    // path the module's declaration implies relative to the package
    // root: `module util/list;` → `util/list.kio`, `module main;` →
    // `main.kio`, `module pkg/main;` (host-using) → `pkg/main.kio`
    // (see `specs/package.md` § Module-name rules). The
    // path-coherence check in `Package::build` requires this, so the
    // emitted directory re-builds as a fresh package. `emit_module`
    // is pure on one `Module<Prime>` and each output file
    // is independent of every other, so the per-module emit + write
    // fans out across rayon (gated on the `parallel` feature — see
    // `maybe_into_par_iter!`). Each checked Prime module carries every
    // exact nominal owner needed to render independently.
    let shape_start = Instant::now();
    build_timing::log_package_shape(
        &package_label,
        target,
        "kio-prime",
        "prime",
        "root",
        package,
    );
    let shape_ms = build_timing::elapsed_ms(shape_start);
    let emit_write_start = Instant::now();
    crate::maybe_into_par_iter!(package.modules().collect::<Vec<_>>())
        .map(|(_, entry)| {
            let source = emit_kio_prime_module_cached(&entry.module, emit_cache)?;
            let out_file = target_dir.join(module_file_relpath(&entry.module.path));
            if let Some(parent) = out_file.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    BackendError::Build(format!("cannot create `{}`: {e}", DisplayPath(parent)))
                })?;
            }
            fs::write(&out_file, source).map_err(|e| {
                BackendError::Build(format!("cannot write `{}`: {e}", DisplayPath(&out_file)))
            })
        })
        .collect::<Result<Vec<()>, _>>()?;
    let emit_write_ms = build_timing::elapsed_ms(emit_write_start);

    // Package file (if the package declares one). Re-emit by
    // formatting the parsed AST: the typer already validated it. The
    // re-emitted file carries the package's `build { ... }` block
    // verbatim (it threads through every phase), so the Kio'-output
    // directory re-builds as a fresh package without a separate
    // build-file copy.
    let package_write_start = Instant::now();
    if let Some(entry) = package.package_file() {
        let source = kio_prime::emit_package_file(&entry.package_file);
        let out_file = target_dir.join(format!("{}.pkg.kio", entry.package_name));
        fs::write(&out_file, source).map_err(|e| {
            BackendError::Build(format!("cannot write `{}`: {e}", DisplayPath(&out_file)))
        })?;
    }
    let package_write_ms = build_timing::elapsed_ms(package_write_start);

    let artifact_store_start = Instant::now();
    store_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_store_ms = build_timing::elapsed_ms(artifact_store_start);
    build_timing::log_target(
        &package_label,
        target,
        "kio-prime",
        false,
        &[
            ("artifact_restore", artifact_restore_ms),
            ("prepare", prepare_ms),
            ("shape", shape_ms),
            ("emit_write", emit_write_ms),
            ("package_write", package_write_ms),
            ("artifact_store", artifact_store_ms),
        ],
        build_timing::elapsed_ms(build_start),
    );
    Ok(())
}

fn emit_kio_prime_module_cached(
    module: &crate::ast::Module<crate::ast::Prime>,
    emit_cache: &crate::cache::emit::EmitCache,
) -> Result<String, BackendError> {
    if !emit_cache.is_enabled() {
        return Ok(kio_prime::emit_module(module));
    }
    #[cfg(test)]
    cache_work_counters::record_prime_emit_serialization();
    let module_bytes = postcard::to_allocvec(module)
        .map_err(|e| BackendError::Build(format!("cannot encode kio-prime emit cache key: {e}")))?;
    let module_path = module
        .path
        .segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("/");
    #[cfg(test)]
    cache_work_counters::record_prime_emit_key();
    let key = crate::cache::emit::EmitCacheKey::new(
        CacheTarget::new("kio-prime"),
        EmitTargetProfileFingerprint::from_bytes(b"target=kio-prime"),
        EmitInputFingerprint::from_parts([module_path.as_bytes(), &module_bytes].as_slice()),
    );
    if let Some(source) = emit_cache.lookup(&key) {
        return Ok(source);
    }
    let source = kio_prime::emit_module(module);
    emit_cache.store(&key, &source);
    Ok(source)
}

/// Rust backend. Emits a self-contained Cargo crate under `out`,
/// per the contract in `specs/backends/rust.md` and the per-slice
/// scope in `kio-rs/src/backends/rust/emit.rs`. The `namespace`
/// build-block key sets the Cargo crate name — the root the emitter's
/// branded facade names (the package handle, host-contract trait, and
/// factory) derive from; it defaults to the kio package name when
/// absent.
fn rust_backend(
    cwd: &Path,
    target: &TargetBlock,
    workspace: &check::TypedPackageCollection,
    routed: &SharedRoutedPackage,
    artifact_cache: &crate::cache::artifact::ArtifactCache,
) -> Result<(), BackendError> {
    let build_start = Instant::now();
    let root_package = workspace
        .root_package
        .as_ref()
        .expect("kio build passes skip_ok = false, so the root is always typechecked");
    let package_label = build_timing::package_label(root_package);
    let mut out_dir: Option<&str> = None;
    let mut namespace: Option<&str> = None;
    // The `thread_safety` build-block key. Absent ⇒ the historical
    // single-threaded `Rc<dyn …>` default; the three opt-in values
    // (`send` / `sync` / `send_sync`) select the `Arc`-flavored shape.
    // Validating the value is the Rust backend's responsibility per
    // `specs/package.md` § Per-target keys ("the backend validates
    // keys and values"). An unknown value is an input error (a bad
    // build-block value), surfaced as `BackendError::Build`.
    let mut thread_safety = backends::rust::ThreadSafety::RcLocal;
    let mut thread_safety_set = false;
    for entry in &target.entries {
        match BuildTarget::Rust.key(&entry.key) {
            Some("out") => {
                if out_dir.is_some() {
                    return Err(BackendError::Build("duplicate key `out`".to_owned()));
                }
                out_dir = Some(entry.value.as_str());
            }
            Some("namespace") => {
                if namespace.is_some() {
                    return Err(BackendError::Build("duplicate key `namespace`".to_owned()));
                }
                backends::namespace::validate_rust_namespace(&entry.value)
                    .map_err(BackendError::Build)?;
                namespace = Some(entry.value.as_str());
            }
            None if entry.key == "crate_name" => {
                return Err(BackendError::Build(
                    "unknown key `crate_name` for the `rust` backend — renamed to \
                     `namespace` (same meaning: sets the emitted crate's name)"
                        .to_owned(),
                ));
            }
            Some("thread_safety") => {
                if thread_safety_set {
                    return Err(BackendError::Build(
                        "duplicate key `thread_safety`".to_owned(),
                    ));
                }
                thread_safety = backends::rust::ThreadSafety::parse(&entry.value)
                    .map_err(BackendError::Build)?;
                thread_safety_set = true;
            }
            _ => {
                let other = &entry.key;
                return Err(BackendError::Build(format!(
                    "unknown key `{other}` for the `rust` backend \
                     (recognized: `out`, `namespace`, `thread_safety`)"
                )));
            }
        }
    }
    let out =
        out_dir.ok_or_else(|| BackendError::Build("missing required key `out`".to_owned()))?;
    let out_path = Path::new(out);
    if out_path.is_absolute() {
        return Err(BackendError::Build(format!(
            "`out` must be a path relative to the package root, got `{out}`"
        )));
    }
    let target_dir = cwd.join(out_path);
    // The sig is a declared build input — the Rust backend re-emits a
    // removed `host fn` as a `#[deprecated]` trait method from it, so
    // build output is `f(source, sig)`. Fold the verbatim bytes into the
    // cache key (the required invariant in `specs/versioning.md` § The
    // signature artifact) so a sealed sig change on otherwise-fixed
    // source regenerates a different crate rather than serving a stale
    // one. Parse + replay happens after the cache check, on a miss.
    let sig_content = read_sig_content(
        cwd,
        &root_package
            .package_file()
            .expect("kio build validates the root package file before backend dispatch")
            .package_name,
    )?;
    let artifact_key = artifact_key_for_workspace_if_enabled(
        artifact_cache,
        "rust",
        target,
        workspace,
        sig_content.as_deref(),
    );
    let artifact_restore_start = Instant::now();
    let artifact_hit = restore_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_restore_ms = build_timing::elapsed_ms(artifact_restore_start);
    if artifact_hit {
        build_timing::log_target(
            &package_label,
            target,
            "rust",
            true,
            &[("artifact_restore", artifact_restore_ms)],
            build_timing::elapsed_ms(build_start),
        );
        return Ok(());
    }
    let prepare_start = Instant::now();
    prepare_target_dir(&target_dir)?;
    let src_dir = target_dir.join("src");
    fs::create_dir_all(&src_dir).map_err(|e| {
        BackendError::Build(format!("cannot create `{}`: {e}", DisplayPath(&src_dir)))
    })?;
    let prepare_ms = build_timing::elapsed_ms(prepare_start);

    let setup_start = Instant::now();
    let pkg_name: String = pick_rust_package_name(root_package)?;
    let crate_name: String = namespace
        .map(|s| s.to_owned())
        .unwrap_or_else(|| backends::namespace::default_rust_namespace(&pkg_name));

    let setup_ms = build_timing::elapsed_ms(setup_start);

    // Root crate (always emitted, scaffolding included). The
    // recover → optimize → route → annotate prefix is backend-agnostic
    // and computed once for the whole build (see `SharedRoutedPackage`);
    // the Rust backend shares the same `Package<Routed>` the JS backend
    // would, reading the derived `Lifetime` off the capability
    // annotations (the `captured_from` captures are internal to the
    // annotation passes). It consumes `Module<Routed>` directly; no
    // unlower round-trip needed.
    let recovered_root = routed.get();

    // Parse + replay the sig (on a cache miss) so the host-trait
    // renderer re-emits the removed env-side host fns as deprecated
    // trait methods. The deprecation window is emitter policy; the Rust
    // backend re-emits every removed host fn the changelog records.
    let sig = match &sig_content {
        Some(content) => Some(replay_sig_for_build(content, &pkg_name)?),
        None => None,
    };
    let root_opts = backends::rust::LowerOptions {
        crate_root_prefix: "crate".to_owned(),
        thread_safety,
        sig,
    };
    let root_emit_start = Instant::now();
    let root_krate =
        backends::rust::lower_package_with_options(recovered_root, &crate_name, &root_opts)
            .map_err(|e| {
                BackendError::Build(format!("cannot emit crate for package `{pkg_name}`: {e}"))
            })?;

    let root_emit_ms = build_timing::elapsed_ms(root_emit_start);

    let write_start = Instant::now();
    let write = |rel: &str, content: &str| -> Result<(), BackendError> {
        let path = target_dir.join(rel);
        fs::write(&path, content)
            .map_err(|e| BackendError::Build(format!("cannot write `{}`: {e}", DisplayPath(&path))))
    };
    write("Cargo.toml", &root_krate.cargo_toml)?;
    write("src/lib.rs", &root_krate.lib_rs)?;
    write("src/host.rs", &root_krate.host_rs)?;
    write("src/shapes.rs", &root_krate.shapes_rs)?;
    // FFI boundary type aliases (`crate::ffi::{env,exp}::<member>::<role>`).
    // Names the Rust type at every env-fn / export-fn boundary slot so a
    // host implementation can reach a shape type by alias rather than
    // re-deriving its mangled spelling. See `backends::rust::emit::emit_ffi_rs`.
    write("src/ffi.rs", &root_krate.ffi_rs)?;
    // The runtime-support file (per `backends::rust`'s Profile
    // `runtime_support: EmbeddedFile { path: "src/__kio_runtime.rs" }`).
    // Same byte content for every emitted crate; holds the
    // `as_any` / `from_any` erase / recover helpers the per-variant
    // emit calls into. Not user-modifiable; overwritten on every
    // build.
    write(
        backends::rust::RUNTIME_SUPPORT_FILE_PATH,
        &root_krate.runtime_rs,
    )?;
    let write_ms = build_timing::elapsed_ms(write_start);

    let artifact_store_start = Instant::now();
    store_cached_artifact(artifact_cache, artifact_key.as_ref(), &target_dir);
    let artifact_store_ms = build_timing::elapsed_ms(artifact_store_start);
    build_timing::log_target(
        &package_label,
        target,
        "rust",
        false,
        &[
            ("artifact_restore", artifact_restore_ms),
            ("prepare", prepare_ms),
            ("setup", setup_ms),
            ("root_emit", root_emit_ms),
            ("write", write_ms),
            ("artifact_store", artifact_store_ms),
        ],
        build_timing::elapsed_ms(build_start),
    );
    Ok(())
}

fn pick_rust_package_name<P: crate::ast::Phase>(
    package: &Package<P>,
) -> Result<String, BackendError> {
    Ok(package
        .package_file()
        .expect("kio build validates the root package file before backend dispatch")
        .package_name
        .clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Allocate a fresh empty directory under the OS temp dir so two
    /// concurrent test threads don't trip over each other when writing
    /// `*.pkg.kio` files.
    fn fresh_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let dir = env::temp_dir().join(format!("kio-build-test-{label}-{pid}-{n}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    // ---- find_package_file ---------------------------------------------

    #[test]
    fn find_package_file_missing() {
        let dir = fresh_dir("missing");
        match find_package_file(&dir) {
            Err(PackageFileError::Missing) => {}
            other => panic!("expected Missing, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_package_file_single() {
        let dir = fresh_dir("single");
        let path = dir.join("mypkg.pkg.kio");
        fs::write(&path, "package mypkg;\n").unwrap();
        let found = find_package_file(&dir).expect("find Single");
        assert_eq!(found, path);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_package_file_multiple_lists_sorted() {
        let dir = fresh_dir("multi");
        let a = dir.join("a.pkg.kio");
        let b = dir.join("b.pkg.kio");
        fs::write(&a, "").unwrap();
        fs::write(&b, "").unwrap();
        match find_package_file(&dir) {
            Err(PackageFileError::Multiple(paths)) => {
                assert_eq!(paths, vec![a, b]); // sorted
            }
            other => panic!("expected Multiple, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    // `PackageFileError` doesn't derive `Debug`; provide a manual one
    // for the tests above.
    impl std::fmt::Debug for PackageFileError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                PackageFileError::Missing => write!(f, "Missing"),
                PackageFileError::Multiple(p) => write!(f, "Multiple({p:?})"),
                PackageFileError::Io(e) => write!(f, "Io({e})"),
            }
        }
    }

    // ---- check_unique_ids --------------------------------------------

    fn target(id: &str) -> TargetBlock {
        TargetBlock {
            trailing_trivia: Vec::new(),
            id: id.to_owned(),
            entries: Vec::new(),
            span: crate::span::Span { start: 0, end: 0 },
            leading_trivia: Vec::new(),
        }
    }

    #[cfg(feature = "surface")]
    const SEALED_RETAINED_HOST_SIG: &str = r#"signature app v(3);
v(1) {
  nonbreaking {
    add {
      module api {
        host type H role(i32);
        host fn old(value: H) -> H;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#;

    #[cfg(feature = "surface")]
    const OPEN_HOST_REMOVAL_SIG: &str = r#"signature app v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type H role(i32);
        host fn old(value: H) -> H;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#;

    #[cfg(feature = "surface")]
    const SEALED_RETAINED_CARRIER_SIG: &str = r#"signature app v(3);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Box[A];
        host type Unused[A];
        newtype Token[A] : A { pub constructor make_token; projector read_token; };
        host fn old(value: Box(Token(.))) -> Box(Token(.));
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Box;
        Unused;
        Token;
        old;
      }
    }
  }
}
"#;

    #[cfg(feature = "surface")]
    const OPEN_RETAINED_CARRIER_SIG: &str = r#"signature app v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Box[A];
        host type Unused[A];
        newtype Token[A] : A { pub constructor make_token; projector read_token; };
        host fn old(value: Box(Token(.))) -> Box(Token(.));
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Box;
        Unused;
        Token;
        old;
      }
    }
  }
}
"#;

    #[cfg(feature = "surface")]
    fn typed_go_workspace(dir: &Path) -> (check::TypedPackageCollection, TargetBlock) {
        fs::write(
            dir.join("app.pkg.kio"),
            "package app;\n\
             build {\n\
               cache ();\n\
               target go {\n\
                 out \"out/go/\";\n\
               }\n\
             }\n\
             bridge { api; }\n",
        )
        .expect("write package file");
        fs::write(
            dir.join("api.kio"),
            "module api;\n\
             pub fn unit() -> . { () }\n",
        )
        .expect("write module");

        let mut diagnostics = String::new();
        let workspace =
            match check::compile_workspace_at_buffered(dir, false, false, &mut diagnostics) {
                Ok(workspace) => workspace,
                Err(code) => panic!("minimal Go workspace failed with {code:?}: {}", diagnostics),
            };
        let target = workspace
            .root_package
            .as_ref()
            .and_then(|package| package.package_file())
            .and_then(|entry| entry.package_file.build.as_ref())
            .and_then(|build| build.targets.iter().find(|target| target.id == "go"))
            .cloned()
            .expect("Go target");
        (workspace, target)
    }

    #[cfg(feature = "surface")]
    fn typed_python_workspace(dir: &Path) -> (check::TypedPackageCollection, TargetBlock) {
        fs::write(
            dir.join("app.pkg.kio"),
            "package app;\n\
             build {\n\
               cache ();\n\
               target python {\n\
                 out \"out/python/\";\n\
               }\n\
             }\n\
             bridge { api; }\n",
        )
        .expect("write package file");
        fs::write(
            dir.join("api.kio"),
            "module api;\n\
             pub fn unit() -> . { () }\n",
        )
        .expect("write module");

        let mut diagnostics = String::new();
        let workspace =
            match check::compile_workspace_at_buffered(dir, false, false, &mut diagnostics) {
                Ok(workspace) => workspace,
                Err(code) => {
                    panic!("minimal Python workspace failed with {code:?}: {diagnostics}")
                }
            };
        let target = workspace
            .root_package
            .as_ref()
            .and_then(|package| package.package_file())
            .and_then(|entry| entry.package_file.build.as_ref())
            .and_then(|build| build.targets.iter().find(|target| target.id == "python"))
            .cloned()
            .expect("Python target");
        (workspace, target)
    }

    #[cfg(feature = "surface")]
    fn typed_carrier_workspace(
        dir: &Path,
        backend: &str,
    ) -> (check::TypedPackageCollection, TargetBlock) {
        fs::write(
            dir.join("app.pkg.kio"),
            format!(
                "package app;\n\
                 build {{\n\
                   cache ();\n\
                   target {backend} {{\n\
                     out \"out/{backend}/\";\n\
                   }}\n\
                 }}\n\
                 bridge {{ api; }}\n"
            ),
        )
        .expect("write retained-carrier package file");
        fs::write(
            dir.join("api.kio"),
            "module api;\n\
             pub fn unit() -> . { () }\n",
        )
        .expect("write retained-carrier module");

        let mut diagnostics = String::new();
        let workspace =
            match check::compile_workspace_at_buffered(dir, false, false, &mut diagnostics) {
                Ok(workspace) => workspace,
                Err(code) => {
                    panic!("minimal {backend} workspace failed with {code:?}: {diagnostics}")
                }
            };
        let target = workspace
            .root_package
            .as_ref()
            .and_then(|package| package.package_file())
            .and_then(|entry| entry.package_file.build.as_ref())
            .and_then(|build| build.targets.iter().find(|target| target.id == backend))
            .cloned()
            .unwrap_or_else(|| panic!("{backend} target"));
        (workspace, target)
    }

    #[cfg(feature = "surface")]
    fn expect_go_build(result: Result<(), BackendError>) {
        match result {
            Ok(()) => {}
            Err(BackendError::Build(message)) => panic!("Go build failed: {message}"),
            Err(BackendError::UnknownBackend(message)) => {
                panic!("unexpected backend error: {message}")
            }
        }
    }

    #[cfg(feature = "surface")]
    fn expect_python_build(result: Result<(), BackendError>) {
        match result {
            Ok(()) => {}
            Err(BackendError::Build(message)) => panic!("Python build failed: {message}"),
            Err(BackendError::UnknownBackend(message)) => {
                panic!("unexpected backend error: {message}")
            }
        }
    }

    #[cfg(feature = "surface")]
    fn expect_backend_build(backend: &str, result: Result<(), BackendError>) {
        match result {
            Ok(()) => {}
            Err(BackendError::Build(message)) => panic!("{backend} build failed: {message}"),
            Err(BackendError::UnknownBackend(message)) => {
                panic!("unexpected backend error: {message}")
            }
        }
    }

    #[cfg(feature = "surface")]
    fn go_artifact_bytes(dir: &Path) -> Vec<u8> {
        let mut bytes = Vec::new();
        for file in ["pkg.go", "host.go", "shapes.go", "ffi.go"] {
            bytes.extend(fs::read(dir.join("out/go").join(file)).expect("read Go artifact"));
        }
        bytes
    }

    #[cfg(feature = "surface")]
    fn python_artifact_bytes(dir: &Path) -> Vec<u8> {
        fn append_tree(root: &Path, path: &Path, bytes: &mut Vec<u8>) {
            let mut entries = fs::read_dir(path)
                .expect("read Python artifact directory")
                .map(|entry| entry.expect("read Python artifact entry"))
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                if entry.file_type().expect("stat Python artifact").is_dir() {
                    append_tree(root, &path, bytes);
                } else {
                    let relative = path.strip_prefix(root).expect("relative Python artifact");
                    bytes.extend(relative.as_os_str().as_encoded_bytes());
                    bytes.push(0);
                    bytes.extend(fs::read(&path).expect("read Python artifact"));
                    bytes.push(0xff);
                }
            }
        }

        let root = dir.join("out/python");
        let mut bytes = Vec::new();
        let runtime = root.join("app.py");
        bytes.extend(b"app.py\0");
        bytes.extend(fs::read(runtime).expect("read Python runtime artifact"));
        bytes.push(0xff);
        append_tree(&root, &root.join("app"), &mut bytes);
        bytes
    }

    #[cfg(feature = "surface")]
    fn ts_artifacts(dir: &Path) -> (Vec<u8>, String) {
        let out = dir.join("out/ts");
        (
            fs::read(out.join("app.js")).expect("read TypeScript runtime"),
            fs::read_to_string(out.join("app.d.ts")).expect("read TypeScript declarations"),
        )
    }

    #[cfg(feature = "surface")]
    fn swift_artifacts(dir: &Path) -> (String, String, String, String) {
        let out = dir.join("out/swift");
        (
            fs::read_to_string(out.join("host.swift")).expect("read Swift host"),
            fs::read_to_string(out.join("pkg.swift")).expect("read Swift package"),
            fs::read_to_string(out.join("ffi.swift")).expect("read Swift FFI aliases"),
            fs::read_to_string(out.join("shapes.swift")).expect("read Swift shapes"),
        )
    }

    #[test]
    fn unique_ids_accepts_distinct_ids() {
        let bf = BuildBlock {
            leading_trivia: Vec::new(),
            trailing_trivia: Vec::new(),
            cache: crate::ast::BuildBlockCache::Disabled {
                span: crate::span::Span { start: 0, end: 0 },
                leading_trivia: Vec::new(),
            },
            docs: None,
            targets: vec![target("js"), target("kio-prime")],
            span: crate::span::Span { start: 0, end: 0 },
        };
        check_unique_ids(&bf).expect("distinct ids should pass");
    }

    #[test]
    fn unique_ids_rejects_duplicates() {
        let bf = BuildBlock {
            leading_trivia: Vec::new(),
            trailing_trivia: Vec::new(),
            cache: crate::ast::BuildBlockCache::Disabled {
                span: crate::span::Span { start: 0, end: 0 },
                leading_trivia: Vec::new(),
            },
            docs: None,
            targets: vec![target("js"), target("kio-prime"), target("js")],
            span: crate::span::Span { start: 0, end: 0 },
        };
        let err = check_unique_ids(&bf).expect_err("duplicate id should fail");
        assert!(err.contains("duplicate target id"), "got: {err}");
        assert!(err.contains("`js`"), "should name the offender: {err}");
    }

    // ---- signature replay and artifact identity ---------------------

    #[cfg(feature = "surface")]
    #[test]
    fn go_build_replays_only_sealed_retained_root_after_artifact_miss() {
        let dir = fresh_dir("go-sig-replay");
        let (workspace, target) = typed_go_workspace(&dir);
        let root = workspace.root_package.as_ref().expect("typed root");
        let label = build_timing::package_label(root);
        let enriched_cache = crate::cache::enriched::EnrichedCache::disabled();
        let routed = SharedRoutedPackage::new(root, &enriched_cache, &label);
        let artifact_cache = crate::cache::artifact::ArtifactCache::disabled();

        expect_go_build(go_backend(
            &dir,
            &target,
            &workspace,
            &routed,
            &artifact_cache,
        ));
        let without_signature = go_artifact_bytes(&dir);

        fs::write(dir.join("app.sig.kio"), OPEN_HOST_REMOVAL_SIG)
            .expect("write open-draft signature");
        expect_go_build(go_backend(
            &dir,
            &target,
            &workspace,
            &routed,
            &artifact_cache,
        ));
        let with_open_draft = go_artifact_bytes(&dir);
        assert_eq!(
            without_signature, with_open_draft,
            "an open-draft retirement must not reach the emitted Go facade"
        );

        fs::write(dir.join("app.sig.kio"), SEALED_RETAINED_HOST_SIG)
            .expect("write retained signature");
        expect_go_build(go_backend(
            &dir,
            &target,
            &workspace,
            &routed,
            &artifact_cache,
        ));
        let with_retained_root = go_artifact_bytes(&dir);
        assert_ne!(
            without_signature, with_retained_root,
            "the replayed retained host root must reach the emitted Go facade"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn python_build_omits_sealed_retained_history_after_artifact_miss() {
        let dir = fresh_dir("python-sig-replay");
        let (workspace, target) = typed_python_workspace(&dir);
        let root = workspace.root_package.as_ref().expect("typed root");
        let label = build_timing::package_label(root);
        let enriched_cache = crate::cache::enriched::EnrichedCache::disabled();
        let routed = SharedRoutedPackage::new(root, &enriched_cache, &label);
        let artifact_cache = crate::cache::artifact::ArtifactCache::disabled();

        expect_python_build(python_backend(
            &dir,
            &target,
            &workspace,
            &routed,
            &artifact_cache,
        ));
        assert!(dir.join("out/python/app.py").is_file());
        assert!(dir.join("out/python/app/__init__.pyi").is_file());
        assert!(dir.join("out/python/app/_kio_stub_0000.pyi").is_file());
        assert!(!dir.join("out/python/app.pyi").exists());
        let without_signature = python_artifact_bytes(&dir);
        let stale_shard = dir.join("out/python/app/_kio_stub_9999.pyi");
        fs::write(&stale_shard, "obsolete shard\n").expect("write stale Python stub shard");
        let sibling_witness = dir.join("out/user-owned-witness");
        fs::write(&sibling_witness, "outside the Python target\n")
            .expect("write sibling output witness");

        fs::write(dir.join("app.sig.kio"), OPEN_HOST_REMOVAL_SIG)
            .expect("write open-draft signature");
        expect_python_build(python_backend(
            &dir,
            &target,
            &workspace,
            &routed,
            &artifact_cache,
        ));
        assert!(
            !stale_shard.exists(),
            "Python build retained an obsolete stub shard"
        );
        assert_eq!(
            fs::read_to_string(&sibling_witness).expect("read sibling output witness"),
            "outside the Python target\n",
            "Python stale-artifact cleanup reached outside its target directory"
        );
        let with_open_draft = python_artifact_bytes(&dir);
        assert_eq!(
            without_signature, with_open_draft,
            "an open-draft retirement must not reach the emitted Python facade"
        );
        fs::write(
            dir.join("out/python/cache_witness"),
            "same live-only artifact\n",
        )
        .expect("add Python cache witness");
        let artifact_cache = crate::cache::artifact::ArtifactCache::open(dir.join("cache"))
            .expect("open Python artifact cache");
        let live_only_key = artifact_key_for_workspace("python", &target, &workspace, None);
        artifact_cache.store(&live_only_key, &dir.join("out/python"));

        fs::write(dir.join("app.sig.kio"), SEALED_RETAINED_HOST_SIG)
            .expect("write retained signature");
        expect_python_build(python_backend(
            &dir,
            &target,
            &workspace,
            &routed,
            &artifact_cache,
        ));
        let with_retained_root = python_artifact_bytes(&dir);
        assert_eq!(
            without_signature, with_retained_root,
            "sealed retained history must not change live-only Python artifacts"
        );
        assert_eq!(
            fs::read_to_string(dir.join("out/python/cache_witness"))
                .expect("valid retained history reused the live-only artifact"),
            "same live-only artifact\n",
            "valid signature history unnecessarily partitioned Python's artifact cache"
        );

        fs::write(dir.join("app.sig.kio"), "signature app v(1)\n")
            .expect("write malformed changed signature");
        let err = match python_backend(&dir, &target, &workspace, &routed, &artifact_cache) {
            Ok(()) => panic!("malformed signature incorrectly accepted a Python cache hit"),
            Err(BackendError::Build(message)) => message,
            Err(BackendError::UnknownBackend(message)) => {
                panic!("unexpected backend error: {message}")
            }
        };
        assert!(err.contains("failed to parse"), "got: {err}");

        fs::write(
            dir.join("app.sig.kio"),
            "signature app v(2);\n\
             v(1) {\n  breaking {\n    remove {\n      module api {\n        ghost;\n      }\n    }\n  }\n}\n",
        )
        .expect("write replay-incoherent changed signature");
        let err = match python_backend(&dir, &target, &workspace, &routed, &artifact_cache) {
            Ok(()) => panic!("incoherent signature incorrectly accepted a Python cache hit"),
            Err(BackendError::Build(message)) => message,
            Err(BackendError::UnknownBackend(message)) => {
                panic!("unexpected backend error: {message}")
            }
        };
        assert!(err.contains("failed to replay"), "got: {err}");
        assert!(err.contains("never added"), "got: {err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn ts_build_replays_carriers_without_changing_the_js_runtime() {
        let dir = fresh_dir("ts-retained-carriers");
        let (workspace, target) = typed_carrier_workspace(&dir, "ts");
        let root = workspace.root_package.as_ref().expect("typed root");
        let label = build_timing::package_label(root);
        let enriched_cache = crate::cache::enriched::EnrichedCache::disabled();
        let routed = SharedRoutedPackage::new(root, &enriched_cache, &label);
        let emit_cache = crate::cache::emit::EmitCache::disabled();
        let artifact_cache = crate::cache::artifact::ArtifactCache::disabled();

        expect_backend_build(
            "TypeScript",
            ts_backend(
                &dir,
                &target,
                &workspace,
                &routed,
                &emit_cache,
                &artifact_cache,
            ),
        );
        let without_signature = ts_artifacts(&dir);

        fs::write(dir.join("app.sig.kio"), OPEN_RETAINED_CARRIER_SIG)
            .expect("write open retained-carrier signature");
        expect_backend_build(
            "TypeScript",
            ts_backend(
                &dir,
                &target,
                &workspace,
                &routed,
                &emit_cache,
                &artifact_cache,
            ),
        );
        let with_open_draft = ts_artifacts(&dir);
        assert_eq!(without_signature, with_open_draft);

        fs::write(dir.join("app.sig.kio"), SEALED_RETAINED_CARRIER_SIG)
            .expect("write sealed retained-carrier signature");
        expect_backend_build(
            "TypeScript",
            ts_backend(
                &dir,
                &target,
                &workspace,
                &routed,
                &emit_cache,
                &artifact_cache,
            ),
        );
        let with_retained = ts_artifacts(&dir);
        assert_eq!(without_signature.0, with_retained.0);
        assert_ne!(without_signature.1, with_retained.1);
        assert!(with_retained.1.contains("readonly Box?: AppTypeLambda<"));
        assert!(
            with_retained
                .1
                .contains("/** @deprecated Host type `api.Box` was removed at v(2). */")
        );
        assert!(with_retained.1.contains("declare class __KioOpaque_"));
        assert!(
            with_retained.1.contains(
                "/** @deprecated Retained only for host declarations removed at v(2). */"
            )
        );
        assert!(!with_retained.1.contains("Unused"));
        assert!(with_retained.1.contains("readonly old?:"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn swift_build_replays_retained_source_facade_without_package_callable_reemission() {
        let dir = fresh_dir("swift-retained-carriers");
        let (workspace, target) = typed_carrier_workspace(&dir, "swift");
        let root = workspace.root_package.as_ref().expect("typed root");
        let label = build_timing::package_label(root);
        let enriched_cache = crate::cache::enriched::EnrichedCache::disabled();
        let routed = SharedRoutedPackage::new(root, &enriched_cache, &label);
        let artifact_cache = crate::cache::artifact::ArtifactCache::disabled();

        expect_backend_build(
            "Swift",
            swift_backend(&dir, &target, &workspace, &routed, &artifact_cache),
        );
        let without_signature = swift_artifacts(&dir);

        fs::write(dir.join("app.sig.kio"), OPEN_RETAINED_CARRIER_SIG)
            .expect("write open retained-carrier signature");
        expect_backend_build(
            "Swift",
            swift_backend(&dir, &target, &workspace, &routed, &artifact_cache),
        );
        let with_open_draft = swift_artifacts(&dir);
        assert_eq!(without_signature, with_open_draft);

        fs::write(dir.join("app.sig.kio"), SEALED_RETAINED_CARRIER_SIG)
            .expect("write sealed retained-carrier signature");
        expect_backend_build(
            "Swift",
            swift_backend(&dir, &target, &workspace, &routed, &artifact_cache),
        );
        let with_retained = swift_artifacts(&dir);
        assert_ne!(without_signature.0, with_retained.0);
        assert_eq!(without_signature.1, with_retained.1);
        assert_ne!(without_signature.2, with_retained.2);
        assert_ne!(without_signature.3, with_retained.3);
        assert!(with_retained.3.contains("public struct KioHostType_"));
        assert!(with_retained.3.contains("public struct KioNewtype_"));
        assert!(!with_retained.3.contains("Unused"));
        let old_message = "Kio host fn `api.old` was removed at contract v2";
        let old_attribute = format!("@available(*, deprecated, message: \"{old_message}\")");
        assert_eq!(with_retained.0.matches(&old_attribute).count(), 2);
        assert!(with_retained.0.contains("\tfunc api__old("));
        assert!(
            with_retained
                .0
                .contains(&format!("fatalError(\"{old_message}\")"))
        );
        assert!(with_retained.2.contains("Env_api__old_arg0"));
        assert!(!with_retained.1.contains("api__old"));
        assert!(!with_retained.1.contains("KioType_Token"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn build_signature_replay_excludes_the_open_draft() {
        let (version, replayed) = match replay_sig_for_build(OPEN_HOST_REMOVAL_SIG, "app") {
            Ok(replayed) => replayed,
            Err(BackendError::Build(message)) => panic!("signature replay failed: {message}"),
            Err(BackendError::UnknownBackend(message)) => {
                panic!("unexpected backend error: {message}")
            }
        };

        assert_eq!(
            version, 1,
            "compatibility emission observes the last sealed generation"
        );
        assert!(
            replayed.removed.is_empty(),
            "an open-draft removal must not enter a compatibility facade"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn build_signature_replay_rejects_an_invalid_fresh_kio_artifact() {
        let malformed = r#"signature app v(2);
v(1) {
  with { module api {
    pub rec newtype Bad : (Bad -> .) { pub constructor mk; pub projector un; };
  } };
  nonbreaking { add { api.Bad; } }
}
"#;
        let message = match replay_sig_for_build(malformed, "app") {
            Err(BackendError::Build(message)) => message,
            Err(BackendError::UnknownBackend(message)) => {
                panic!("unexpected backend error: {message}")
            }
            Ok(_) => panic!("build accepted a malformed sealed signature before projection"),
        };
        assert!(message.contains("failed to replay"), "got: {message}");
        assert!(message.contains("strictly positive"), "got: {message}");
    }

    #[cfg(feature = "surface")]
    #[test]
    fn malformed_go_signature_fails_on_artifact_miss() {
        let dir = fresh_dir("go-sig-miss");
        let (workspace, target) = typed_go_workspace(&dir);
        fs::write(dir.join("app.sig.kio"), "signature app v(1)\n")
            .expect("write malformed signature");
        let root = workspace.root_package.as_ref().expect("typed root");
        let label = build_timing::package_label(root);
        let enriched_cache = crate::cache::enriched::EnrichedCache::disabled();
        let routed = SharedRoutedPackage::new(root, &enriched_cache, &label);

        let err = match go_backend(
            &dir,
            &target,
            &workspace,
            &routed,
            &crate::cache::artifact::ArtifactCache::disabled(),
        ) {
            Ok(()) => panic!("malformed signature unexpectedly built"),
            Err(BackendError::Build(message)) => message,
            Err(BackendError::UnknownBackend(message)) => {
                panic!("unexpected backend error: {message}")
            }
        };
        assert!(err.contains("failed to parse"), "got: {err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn go_artifact_hit_does_not_replay_signature() {
        let dir = fresh_dir("go-sig-hit");
        let (workspace, target) = typed_go_workspace(&dir);
        let malformed = "signature app v(1)\n";
        fs::write(dir.join("app.sig.kio"), malformed).expect("write malformed signature");

        let artifact_cache = crate::cache::artifact::ArtifactCache::open(dir.join("cache"))
            .expect("open artifact cache");
        let key = artifact_key_for_workspace("go", &target, &workspace, Some(malformed));
        let target_dir = dir.join("out/go");
        fs::create_dir_all(&target_dir).expect("create cached artifact");
        fs::write(target_dir.join("cached"), "hit\n").expect("write cached artifact");
        artifact_cache.store(&key, &target_dir);
        fs::remove_dir_all(&target_dir).expect("remove source artifact");

        let root = workspace.root_package.as_ref().expect("typed root");
        let label = build_timing::package_label(root);
        let enriched_cache = crate::cache::enriched::EnrichedCache::disabled();
        let routed = SharedRoutedPackage::new(root, &enriched_cache, &label);
        expect_go_build(go_backend(
            &dir,
            &target,
            &workspace,
            &routed,
            &artifact_cache,
        ));

        assert_eq!(
            fs::read_to_string(target_dir.join("cached")).expect("restored cached artifact"),
            "hit\n"
        );
        assert!(
            routed.routed.get().is_none(),
            "an artifact hit must return before routing and signature replay"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn disabled_artifact_cache_skips_key_and_prime_render_work() {
        let dir = fresh_dir("disabled-artifact-work");
        let (workspace, target) = typed_go_workspace(&dir);
        let disabled = crate::cache::artifact::ArtifactCache::disabled();

        cache_work_counters::reset();
        let disabled_key = artifact_key_for_workspace_if_enabled(
            &disabled,
            "go",
            &target,
            &workspace,
            Some("signature app v(1);\n"),
        );
        assert!(disabled_key.is_none());
        assert_eq!(
            cache_work_counters::snapshot(),
            cache_work_counters::Counts::default()
        );

        let active = crate::cache::artifact::ArtifactCache::open(dir.join("cache-artifact"))
            .expect("open artifact cache");
        cache_work_counters::reset();
        let active_key = artifact_key_for_workspace_if_enabled(
            &active,
            "go",
            &target,
            &workspace,
            Some("signature app v(1);\n"),
        );
        assert!(active_key.is_some());
        let counts = cache_work_counters::snapshot();
        assert_eq!(counts.artifact_keys, 1);
        assert_eq!(counts.artifact_renders, 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn disabled_kio_prime_emit_cache_skips_serialization_and_key_work() {
        let dir = fresh_dir("disabled-prime-emit-work");
        let (workspace, _) = typed_go_workspace(&dir);
        let package = workspace.root_package.as_ref().expect("typed root package");
        let (_, entry) = package.modules().next().expect("one module");
        let expected = kio_prime::emit_module(&entry.module);
        let disabled = crate::cache::emit::EmitCache::disabled();

        cache_work_counters::reset();
        let disabled_output = match emit_kio_prime_module_cached(&entry.module, &disabled) {
            Ok(output) => output,
            Err(BackendError::Build(message)) => {
                panic!("disabled-cache emit failed: {message}")
            }
            Err(BackendError::UnknownBackend(message)) => {
                panic!("unexpected backend error: {message}")
            }
        };
        assert_eq!(disabled_output, expected);
        assert_eq!(
            cache_work_counters::snapshot(),
            cache_work_counters::Counts::default()
        );

        let active =
            crate::cache::emit::EmitCache::open(dir.join("cache-emit")).expect("open emit cache");
        cache_work_counters::reset();
        let active_output = match emit_kio_prime_module_cached(&entry.module, &active) {
            Ok(output) => output,
            Err(BackendError::Build(message)) => {
                panic!("active-cache emit failed: {message}")
            }
            Err(BackendError::UnknownBackend(message)) => {
                panic!("unexpected backend error: {message}")
            }
        };
        assert_eq!(active_output, expected);
        let counts = cache_work_counters::snapshot();
        assert_eq!(counts.prime_emit_serializations, 1);
        assert_eq!(counts.prime_emit_keys, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn signature_content_partitions_artifact_cache_entries() {
        let dir = fresh_dir("sig-artifact-key");
        let package: Package<crate::ast::Prime> =
            Package::build(&dir, Vec::new(), None).expect("build empty package");
        let target = target("go");
        let first =
            artifact_key_for_package("go", &target, &package, Some("signature app v(1);\n"));
        let second =
            artifact_key_for_package("go", &target, &package, Some("signature app v(2);\n"));
        assert_ne!(first, second);

        let cache = crate::cache::artifact::ArtifactCache::open(dir.join("cache"))
            .expect("open artifact cache");
        let emitted = dir.join("emitted");
        fs::create_dir_all(&emitted).expect("create emitted tree");
        fs::write(emitted.join("pkg.go"), "package app\n").expect("write emitted tree");
        cache.store(&first, &emitted);

        assert!(
            !cache.restore(&second, &dir.join("wrong-signature")),
            "a different signature must not restore the first artifact"
        );
        assert!(cache.restore(&first, &dir.join("matching-signature")));
        let _ = fs::remove_dir_all(&dir);
    }
}
