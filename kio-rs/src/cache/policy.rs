//! Operator-facing policy for the Kio-semantic on-disk caches.
//!
//! Kio ships Kio-semantic caches that persist work between runs:
//! the package-check cache ([`package_check_cache`]), typed-module cache,
//! enriched-IR cache ([`enriched_cache`]),
//! emit/artifact caches, the `kio test` equiv-result cache
//! (`equiv_cache`), and the Kiodoc snippet cache ([`kiodoc::cache`]).
//! Each is keyed by inputs the compiler logic reads — source hashes,
//! per-package side tables, cache namespace, compiler cache identity —
//! so a stable cache hit reflects byte-identical work in production and
//! during local compiler development after a rebuild.
//!
//! Cache correctness comes from real identity inputs. When a cache
//! depends on new source data, side tables, target configuration, or
//! other semantic inputs, add those bytes to that cache's key; do not
//! add manual "bump this version" constants for compiler-source
//! behavior. The build-time compiler cache identity already covers
//! compiler source, manifest, feature-set, and wire-format
//! implementation edits after a rebuild.
//!
//! The caches are **on by default**. The global `--no-cache` flag on
//! the `kio` (and `kio-prime`) binary turns them off for one
//! invocation: the CLI dispatcher strips the flag from argv and
//! calls [`disable_for_process`] before subcommand handlers run, and
//! the cache-resolution call sites then fall back to the disabled
//! variant of each cache. `kio` reads no environment variable for
//! this — every opt-out is an explicit CLI flag.
//!
//! Why an opt-out: it remains useful for reproducing cold runs and
//! for debugging cache storage itself. Correctness does not depend on
//! routinely clearing caches: the build-time compiler cache identity
//! folds in the package version, enabled feature set, Cargo manifests,
//! and Rust source files, so a local compiler edit followed by a
//! rebuild retires stale semantic entries automatically.
//!
//! This module is the single source of truth for the process-flag
//! latch; the cache modules themselves never see it. Their
//! constructors continue to expose only the `Active`/`Disabled`
//! distinction, and the call sites in `check.rs`, `build.rs`,
//! `kiodoc/mod.rs` choose between the two variants by consulting
//! [`caches_enabled`]. Cache-family access metadata and throttled garbage
//! collection live in [`gc`].
//!
//! [`package_check_cache`]: crate::cache::package_check
//! [`enriched_cache`]: crate::cache::enriched
//! [`gc`]: crate::cache::gc
//! [`kiodoc::cache`]: crate::kiodoc::cache

use std::sync::OnceLock;

/// Set the first time the CLI dispatcher sees `--no-cache` in argv.
/// Once set, every subsequent [`caches_enabled`] call returns
/// `false` for the remainder of the process.
static FLAG_DISABLED: OnceLock<()> = OnceLock::new();

/// Are the Kio-semantic on-disk caches in effect for this process?
///
/// Returns `false` iff the CLI dispatcher called
/// [`disable_for_process`] after stripping `--no-cache` from argv.
/// Otherwise returns `true` — the production default, where the
/// cache keys' content-addressing keeps stale hits out.
pub fn caches_enabled() -> bool {
    FLAG_DISABLED.get().is_none()
}

/// Latch the per-process "no caches" flag. Idempotent: subsequent
/// calls are no-ops.
///
/// Call this from the CLI dispatcher the moment `--no-cache` is
/// observed in argv, before any downstream code reads
/// [`caches_enabled`]. Tests that exercise cache-enabled paths must
/// not call this.
pub fn disable_for_process() {
    let _ = FLAG_DISABLED.set(());
}
