//! Haskell-specific inputs for the one-level binary cache, and their
//! mapping to the shared cache's key-input field list + `meta.json`
//! body.
//!
//! The generic keying — BLAKE3 over a framed [`InputField`] list — is
//! [`crate::build_cache::key`]. This module supplies the Haskell
//! *policy*: which fields the binary's key folds in ([`bin_fields`]),
//! the toolchain subroot path ([`subroot_rel`]), and the readable
//! sidecar body ([`bin_meta_json`]).
//!
//! Like the Go adapter (and unlike the two-level Rust rlib→bin split),
//! the Haskell runner compiles the whole build tree — the emitted package
//! facade `<Ns>.hs` plus the synthesized `Main.hs` driver — in one `ghc
//! --make`. So the cache is **one-level**: a single
//! binary keyed by `{ghc identity, build flags, all `.hs` source
//! files}`. There is no chained sub-key and no second namespace.

use crate::build_cache::{InputField, id_segment};
use std::path::PathBuf;

/// Inputs that derive the Haskell binary cache key. Every field
/// participates; dropping or adding one requires a schema bump (the
/// shared [`crate::build_cache::key::SCHEMA_TAG`]).
#[derive(Debug, Clone)]
pub struct BinInput {
    /// The GHC toolchain identity — `ghc --numeric-version` plus the
    /// `Target platform` field of `ghc --info` (e.g.
    /// `9.10.3\nx86_64-unknown-linux`). Probed once per process by
    /// [`super::ghc_identity`]. A toolchain bump rekeys every entry.
    pub ghc_identity: String,
    /// Sorted, KV-rendered build flags that affect the emitted binary
    /// (the `--profile`-selected `-O<level>` optimization level).
    /// Path-varying flags (`-o`, the `-outputdir`/`-hidir` staging dirs)
    /// are never folded — they're derived per compile and have no
    /// bearing on the output bytes.
    pub build_flags: Vec<(String, String)>,
    /// The path-sorted `(relative path, file bytes)` list for every `.hs` file
    /// in the assembled build tree that `ghc --make` reads. An ordinary run
    /// contains one emitted package facade plus synthesized `Main.hs`;
    /// coexistence contains both facades and its synthesized host modules too.
    /// The runner assembles this list before opening the cache. It is the
    /// content hash that invalidates the cached binary when any compiled source
    /// changes.
    pub build_files: Vec<(PathBuf, Vec<u8>)>,
}

/// The binary's ordered key-input field list: `{ghc identity, build
/// flags, build-tree files}`. The shared keyer
/// ([`crate::build_cache::hash_fields`]) folds these into the hex key.
/// A one-level adapter, so no domain separator and no chained sub-key.
pub fn bin_fields(input: &BinInput) -> Vec<InputField> {
    vec![
        InputField::Str(input.ghc_identity.clone()),
        InputField::KvPairs(input.build_flags.clone()),
        InputField::Files(input.build_files.clone()),
    ]
}

/// The toolchain subroot relative path the Haskell adapter partitions
/// the cache by: `ghc-<hex8>`. The `<hex8>` is the first-8 of a BLAKE3
/// hash of the ghc identity (see [`id_segment`]) so a user listing the
/// cache can see which subtree a GHC-toolchain update invalidated.
///
/// The runner builds for the host platform; GHC's target platform is
/// part of the `ghc --info` identity, so the host platform is already
/// baked into the identity and there is no separate triple segment.
pub fn subroot_rel(ghc_identity: &str) -> PathBuf {
    PathBuf::from(format!("ghc-{}", id_segment(ghc_identity)))
}

/// Render the binary `meta.json` sidecar. Readable JSON so a user can
/// `cat` / `jq` it. Hand-rolled to avoid pulling a JSON crate just for
/// this — the sidecar's shape is narrow.
pub fn bin_meta_json(input: &BinInput) -> String {
    let now = unix_now();
    let flags = input
        .build_flags
        .iter()
        .map(|(k, v)| {
            if v.is_empty() {
                json_str(k)
            } else {
                json_str(&format!("{k}={v}"))
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{{\n  \"kind\": \"bin\",\n  \"target\": \"haskell\",\n  \"key\": \"{}\",\n  \
         \"ghc_identity\": {},\n  \"build_flags\": [{}],\n  \"created_at\": {},\n  \
         \"file_count\": {}\n}}\n",
        crate::build_cache::hash_fields(None, &bin_fields(input)),
        json_str(&input.ghc_identity),
        flags,
        now,
        input.build_files.len(),
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Minimal JSON string escaping for the `meta.json` sidecar. The values
/// here (ghc-version strings, flag names) never contain control
/// characters in practice; we still handle quote / backslash /
/// control-byte cases so a hostile value can't break the sidecar.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
