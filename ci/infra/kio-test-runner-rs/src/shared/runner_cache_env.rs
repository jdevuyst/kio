//! Shared parsing of the runner build-cache environment variables.
//!
//! The harness configures every cache-backed runner through the same
//! four variables (see the runner README § Runner build cache):
//!
//! - `KIO_TEST_RUNNER_BUILD_CACHE_DIR` — required cache root.
//! - `KIO_TEST_RUNNER_BUILD_CACHE_SIZE` — optional LRU byte budget.
//! - `KIO_TEST_RUNNER_COMPILER_WRAPPER` — optional compiler prefix.
//! - `KIO_TEST_RUNNER_CACHE_DISABLE` — `1` to use a fresh temp cache.
//!
//! The Rust runner parses these inline (it predates this helper); the
//! go / haskell one-level runners share this module so the three agree
//! on variable names, the size-suffix grammar, and the disable
//! semantics without each re-deriving them.

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

/// The resolved cache configuration for one runner invocation: either a
/// persistent harness-supplied root (with optional wrapper + size
/// budget) or a disabled mode the caller backs with a fresh temp root.
#[derive(Debug)]
pub enum RunnerCacheConfig {
    /// Use the harness cache root, persisting across invocations.
    Persistent {
        cache_dir: PathBuf,
        /// The parsed `KIO_TEST_RUNNER_COMPILER_WRAPPER`, if any. Carried
        /// so the env-var grammar stays shared and a future
        /// wrapper-compatible one-level runner can thread it. The current
        /// consumers (go / haskell) deliberately ignore it: `sccache`
        /// wraps C/C++/rustc-shaped compilers and rejects `go` / `ghc`,
        /// so threading it would break the build (it passes `-E`). Hence
        /// `dead_code`-allowed — read by no current consumer, kept for
        /// the parser's completeness and forward use.
        #[allow(dead_code)]
        compiler_wrapper: Option<OsString>,
        max_bytes: Option<u64>,
    },
    /// `KIO_TEST_RUNNER_CACHE_DISABLE=1` — ignore the cache variables;
    /// the caller opens a fresh temp cache for this invocation.
    Disabled,
}

impl RunnerCacheConfig {
    /// Read the four cache variables and resolve a config. Returns
    /// `Err(message)` for a malformed variable (an empty cache dir, a
    /// bad size literal, a non-`{0,1,empty}` disable value) — the caller
    /// surfaces it as a usage error.
    pub fn from_env() -> Result<Self, String> {
        if cache_disabled_from_env()? {
            return Ok(RunnerCacheConfig::Disabled);
        }
        let cache_dir = match env::var_os("KIO_TEST_RUNNER_BUILD_CACHE_DIR") {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            Some(_) => return Err("KIO_TEST_RUNNER_BUILD_CACHE_DIR must not be empty".to_owned()),
            None => return Err("KIO_TEST_RUNNER_BUILD_CACHE_DIR is required".to_owned()),
        };
        let max_bytes = cache_size_from_env()?;
        let compiler_wrapper = match env::var_os("KIO_TEST_RUNNER_COMPILER_WRAPPER") {
            Some(w) if !w.is_empty() => Some(w),
            _ => None,
        };
        Ok(RunnerCacheConfig::Persistent {
            cache_dir,
            compiler_wrapper,
            max_bytes,
        })
    }
}

fn cache_disabled_from_env() -> Result<bool, String> {
    match env::var("KIO_TEST_RUNNER_CACHE_DISABLE") {
        Ok(value) => parse_cache_disable_value(&value),
        Err(env::VarError::NotPresent) => Ok(false),
        Err(env::VarError::NotUnicode(_)) => {
            Err("KIO_TEST_RUNNER_CACHE_DISABLE must be valid Unicode".to_owned())
        }
    }
}

fn parse_cache_disable_value(value: &str) -> Result<bool, String> {
    match value {
        "" | "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(format!(
            "KIO_TEST_RUNNER_CACHE_DISABLE must be 1, 0, or empty; got {value:?}"
        )),
    }
}

fn cache_size_from_env() -> Result<Option<u64>, String> {
    match env::var("KIO_TEST_RUNNER_BUILD_CACHE_SIZE") {
        Ok(value) if value.is_empty() => Ok(None),
        Ok(value) => parse_cache_size_literal(&value).map(Some),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => {
            Err("KIO_TEST_RUNNER_BUILD_CACHE_SIZE must be valid Unicode".to_owned())
        }
    }
}

/// Parse a byte count with an optional binary `K`/`M`/`G`/`T` (or
/// `KB`/`MB`/…) suffix into bytes. Rejects zero and overflow.
pub fn parse_cache_size_literal(value: &str) -> Result<u64, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("KIO_TEST_RUNNER_BUILD_CACHE_SIZE must not be blank".to_owned());
    }
    let upper = trimmed.to_ascii_uppercase();
    let (digits, multiplier) = match upper.as_str() {
        s if s.ends_with("KB") => (&trimmed[..trimmed.len() - 2], 1024_u64),
        s if s.ends_with('K') => (&trimmed[..trimmed.len() - 1], 1024_u64),
        s if s.ends_with("MB") => (&trimmed[..trimmed.len() - 2], 1024_u64.pow(2)),
        s if s.ends_with('M') => (&trimmed[..trimmed.len() - 1], 1024_u64.pow(2)),
        s if s.ends_with("GB") => (&trimmed[..trimmed.len() - 2], 1024_u64.pow(3)),
        s if s.ends_with('G') => (&trimmed[..trimmed.len() - 1], 1024_u64.pow(3)),
        s if s.ends_with("TB") => (&trimmed[..trimmed.len() - 2], 1024_u64.pow(4)),
        s if s.ends_with('T') => (&trimmed[..trimmed.len() - 1], 1024_u64.pow(4)),
        _ => (trimmed, 1_u64),
    };
    let base = digits.trim().parse::<u64>().map_err(|_| {
        format!(
            "KIO_TEST_RUNNER_BUILD_CACHE_SIZE must be a byte count with optional K/M/G/T suffix; got {value:?}"
        )
    })?;
    if base == 0 {
        return Err("KIO_TEST_RUNNER_BUILD_CACHE_SIZE must be greater than zero".to_owned());
    }
    base.checked_mul(multiplier)
        .ok_or_else(|| format!("KIO_TEST_RUNNER_BUILD_CACHE_SIZE is too large: {value:?}"))
}

#[cfg(test)]
mod tests {
    use super::parse_cache_size_literal;

    #[test]
    fn plain_bytes_parse() {
        assert_eq!(parse_cache_size_literal("1024").unwrap(), 1024);
    }

    #[test]
    fn binary_suffixes_parse() {
        assert_eq!(parse_cache_size_literal("1K").unwrap(), 1024);
        assert_eq!(parse_cache_size_literal("2M").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_cache_size_literal("1GB").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn zero_is_rejected() {
        assert!(parse_cache_size_literal("0").is_err());
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse_cache_size_literal("abc").is_err());
    }
}
