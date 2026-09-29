//! Go-specific inputs for the one-level binary cache, and their
//! mapping to the shared cache's key-input field list + `meta.json`
//! body.
//!
//! The generic keying — BLAKE3 over a framed [`InputField`] list — is
//! [`crate::build_cache::key`]. This module supplies the Go *policy*:
//! which fields the binary's key folds in ([`bin_fields`]), the
//! toolchain subroot path ([`subroot_rel`]), and the readable sidecar
//! body ([`bin_meta_json`]).
//!
//! Unlike the Rust adapter's two-level rlib→bin split, the Go runner
//! compiles the whole build tree (the emitted package `.go` files plus
//! the synthesized `main.go` driver) in one `go build`. So the cache is
//! **one-level**: a single binary keyed by `{go identity, build settings,
//! all build-tree source files}`. There is no chained sub-key and no
//! second namespace.

use crate::build_cache::{InputField, id_segment};
use std::path::PathBuf;

/// Inputs that derive the Go binary cache key. Every field
/// participates; dropping or adding one requires a schema bump (the
/// shared [`crate::build_cache::key::SCHEMA_TAG`]).
#[derive(Debug, Clone)]
pub struct BinInput {
    /// The Go toolchain identity — `go version` output (e.g.
    /// `go version go1.26.4 linux/amd64`). Probed once per process by
    /// [`super::go_identity`]. A toolchain bump rekeys every entry.
    pub go_identity: String,
    /// Sorted, KV-rendered build settings that affect the selected compiler
    /// or emitted binary (`-trimpath`, `GOENV=off`, an empty
    /// `GOEXPERIMENT`, `GOTOOLCHAIN=local`, and the `go.mod` `go` directive).
    /// Path-varying settings (`-o`, `GOCACHE`, `TEST_TELEMETRY_DIR`) are never
    /// folded — they're derived per compile and have no bearing on the output bytes.
    /// `-trimpath` *is* folded: it changes the binary (it strips the build
    /// path), so a cache built with it must not alias one built without.
    pub build_flags: Vec<(String, String)>,
    /// The path-sorted `(relative path, file bytes)` list for the whole
    /// build tree the `go build` reads: every emitted package `.go`
    /// file, the synthesized `main.go` driver, and the `go.mod` files
    /// the runner writes. Built by [`super::collect_build_files`]. This
    /// is the content hash that invalidates the cached binary when the
    /// emitted sources, the driver, or the module wiring change.
    pub build_files: Vec<(PathBuf, Vec<u8>)>,
}

/// The binary's ordered key-input field list: `{go identity, build
/// settings, build-tree files}`. The shared keyer
/// ([`crate::build_cache::hash_fields`]) folds these into the hex key.
/// A one-level adapter, so no domain separator and no chained sub-key.
pub fn bin_fields(input: &BinInput) -> Vec<InputField> {
    vec![
        InputField::Str(input.go_identity.clone()),
        InputField::KvPairs(input.build_flags.clone()),
        InputField::Files(input.build_files.clone()),
    ]
}

/// The toolchain subroot relative path the Go adapter partitions the
/// cache by: `go-<hex8>`. The `<hex8>` is the first-8 of a BLAKE3 hash
/// of the go identity (see [`id_segment`]) so a user listing the cache
/// can see which subtree a Go-toolchain update event invalidated.
///
/// Go cross-compiles via `GOOS`/`GOARCH`, not a target-triple
/// path-segment the runner sets, and the runner builds for the host
/// platform, so there is no per-target segment under the toolchain
/// (the host platform is already baked into the `go version` identity's
/// `linux/amd64` suffix).
pub fn subroot_rel(go_identity: &str) -> PathBuf {
    PathBuf::from(format!("go-{}", id_segment(go_identity)))
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
            if v.is_empty() && k.starts_with('-') {
                json_str(k)
            } else {
                json_str(&format!("{k}={v}"))
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{{\n  \"kind\": \"bin\",\n  \"target\": \"go\",\n  \"key\": \"{}\",\n  \
         \"go_identity\": {},\n  \"build_flags\": [{}],\n  \"created_at\": {},\n  \
         \"file_count\": {}\n}}\n",
        crate::build_cache::hash_fields(None, &bin_fields(input)),
        json_str(&input.go_identity),
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
/// here (go-version strings, flag names) never contain control
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
