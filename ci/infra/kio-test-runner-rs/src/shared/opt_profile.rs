//! Shared, backend-independent optimization-level **profile** selection
//! for the compiled test runners (rust / go / haskell / swift).
//!
//! A profile is a backend-independent name; each runner maps it to its
//! compiler's real optimization flag. The names are deliberately *not*
//! `opt-level=N` — the numbers differ per compiler:
//!
//! | profile       | rustc            | ghc   | swiftc   |
//! | ------------- | ---------------- | ----- | -------- |
//! | `unoptimized` | `-C opt-level=0` | `-O0` | `-Onone` |
//! | `default`     | `-C opt-level=1` | `-O0` | `-Onone` |
//! | `optimized`   | `-C opt-level=2` | `-O2` | `-O`     |
//!
//! Go has no optimization levels, so its runner accepts the flag and
//! ignores it. Every profile keeps debug info off (the runner never
//! needs it), so the profile varies only the optimization level.
//!
//! `default` preserves **each backend's current cheap level**. Rust's
//! `opt-level=1` is near-free over `-O0` yet still runs the optimizer
//! (which has caught codegen bugs `-O0` masks), so Rust's `default`
//! differs from `unoptimized`. Under ghc / swiftc, mild optimization
//! (`-O1` / `-O`) costs real compile time, so `default` stays at the
//! zero level (`-O0` / `-Onone`, coinciding with `unoptimized`) and
//! `optimized` (`-O2` / `-O`) is the on-demand thorough pass. The
//! profile is selected by a `--profile <name>` runner flag or the
//! [`PROFILE_ENV`] environment variable, defaulting to `default`; the
//! flag wins over the env var, which wins over the built-in default.
//!
//! The selected profile feeds each optimizing runner's build-artifact
//! cache key — Rust via the profile [`OptProfile::name`], ghc / swiftc
//! via the real `-O` flag — so a `default`-built artifact is never
//! served for an `optimized` request: a different optimization level is
//! a different compiled artifact for the same source, and the key must
//! separate them.

use std::env;

/// The environment variable the harness (`ci/run-tests.sh`) and a
/// developer set to pick the compile profile for a whole run. A
/// `--profile` runner flag overrides it.
pub const PROFILE_ENV: &str = "KIO_TEST_RUNNER_PROFILE";

/// A backend-independent optimization-level profile. Each runner maps it
/// to its compiler's real flag (see the module docs); the profile's
/// [`OptProfile::name`] feeds the runner's cache key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OptProfile {
    /// Fastest compile, no optimization (`-C opt-level=0` / `-O0` /
    /// `-Onone`).
    Unoptimized,
    /// The default — each backend's current cheap level. Rust's near-free
    /// `-C opt-level=1` (mild optimization that catches codegen bugs
    /// `-O0` masks); ghc `-O0` / swiftc `-Onone` (where mild opt costs
    /// real compile time, so `default` coincides with `unoptimized` and
    /// `optimized` is the on-demand higher tier).
    #[default]
    Default,
    /// Most optimization (`-C opt-level=2` / `-O2` / `-O`) — slowest
    /// compile, most opt-sensitive coverage and most realistic runtime.
    Optimized,
}

impl OptProfile {
    /// The profile's stable name — the `--profile` spelling and the
    /// [`PROFILE_ENV`] value. The Rust runner also folds it into its
    /// rlib/bin cache key (the go / haskell / swift runners key on their
    /// own optimization flag, or — for go, whose artifact is
    /// profile-independent — not at all), so it is `rust`-gated to keep
    /// each per-feature build's dead-code analysis honest.
    #[cfg(feature = "rust")]
    // The file is path-included by several runner bins; not every adapter
    // includes the profile in its generated compiler command.
    #[allow(dead_code)]
    pub fn name(self) -> &'static str {
        match self {
            OptProfile::Unoptimized => "unoptimized",
            OptProfile::Default => "default",
            OptProfile::Optimized => "optimized",
        }
    }

    /// Parse a profile name. The accepted spellings are exactly the three
    /// [`OptProfile::name`] values.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "unoptimized" => Ok(OptProfile::Unoptimized),
            "default" => Ok(OptProfile::Default),
            "optimized" => Ok(OptProfile::Optimized),
            other => Err(format!(
                "unknown profile {other:?}; expected one of: unoptimized, default, optimized"
            )),
        }
    }

    /// Resolve the effective profile: an explicit `--profile` override
    /// wins; else the [`PROFILE_ENV`] environment variable; else
    /// [`OptProfile::Default`]. An empty or unset env var means the
    /// default; a malformed one is an error.
    pub fn resolve(flag_override: Option<OptProfile>) -> Result<Self, String> {
        if let Some(p) = flag_override {
            return Ok(p);
        }
        match env::var(PROFILE_ENV) {
            Ok(v) if v.trim().is_empty() => Ok(OptProfile::default()),
            Ok(v) => OptProfile::parse(v.trim()).map_err(|e| format!("{PROFILE_ENV}: {e}")),
            Err(env::VarError::NotPresent) => Ok(OptProfile::default()),
            Err(env::VarError::NotUnicode(_)) => {
                Err(format!("{PROFILE_ENV} must be valid Unicode"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{OptProfile, PROFILE_ENV};
    use std::env;
    use std::sync::{Mutex, MutexGuard};

    // `resolve` reads a process-global env var; serialize the env-mutating
    // tests so they don't race each other under the test harness's
    // threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_guard() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(feature = "rust")]
    #[test]
    fn name_round_trips_through_parse() {
        for p in [
            OptProfile::Unoptimized,
            OptProfile::Default,
            OptProfile::Optimized,
        ] {
            assert_eq!(OptProfile::parse(p.name()).unwrap(), p);
        }
    }

    #[test]
    fn default_is_default_profile() {
        assert_eq!(OptProfile::default(), OptProfile::Default);
    }

    #[test]
    fn parse_rejects_opt_level_spelling_and_garbage() {
        // The names are deliberately not `opt-level=N`.
        assert!(OptProfile::parse("opt-level=2").is_err());
        assert!(OptProfile::parse("O2").is_err());
        assert!(OptProfile::parse("").is_err());
    }

    #[test]
    fn flag_override_wins_over_env() {
        let _g = env_guard();
        unsafe { env::set_var(PROFILE_ENV, "unoptimized") };
        assert_eq!(
            OptProfile::resolve(Some(OptProfile::Optimized)).unwrap(),
            OptProfile::Optimized
        );
        unsafe { env::remove_var(PROFILE_ENV) };
    }

    #[test]
    fn env_selects_when_no_flag() {
        let _g = env_guard();
        unsafe { env::set_var(PROFILE_ENV, "optimized") };
        assert_eq!(OptProfile::resolve(None).unwrap(), OptProfile::Optimized);
        unsafe { env::remove_var(PROFILE_ENV) };
    }

    #[test]
    fn empty_or_unset_env_is_default() {
        let _g = env_guard();
        unsafe { env::remove_var(PROFILE_ENV) };
        assert_eq!(OptProfile::resolve(None).unwrap(), OptProfile::Default);
        unsafe { env::set_var(PROFILE_ENV, "  ") };
        assert_eq!(OptProfile::resolve(None).unwrap(), OptProfile::Default);
        unsafe { env::remove_var(PROFILE_ENV) };
    }

    #[test]
    fn malformed_env_is_an_error() {
        let _g = env_guard();
        unsafe { env::set_var(PROFILE_ENV, "fast") };
        assert!(OptProfile::resolve(None).is_err());
        unsafe { env::remove_var(PROFILE_ENV) };
    }
}
