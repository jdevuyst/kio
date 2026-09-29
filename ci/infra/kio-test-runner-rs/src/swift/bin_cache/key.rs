//! Swift-specific inputs for the one-level binary cache, and their
//! mapping to the shared cache's key-input field list + `meta.json`
//! body.
//!
//! The generic keying — BLAKE3 over a framed [`InputField`] list — is
//! [`crate::build_cache::key`]. This module supplies the Swift
//! *policy*: which fields the binary's key folds in ([`bin_fields`]),
//! the toolchain subroot path ([`subroot_rel`]), and the readable
//! sidecar body ([`bin_meta_json`]).
//!
//! Unlike the two-level Rust rlib→bin split, the Swift runner produces a
//! single cached binary from the whole build tree — the emitted
//! `pkg.swift` / `host.swift` / `shapes.swift` / `ffi.swift` /
//! `kio_runtime.swift` plus the synthesized `main.swift` driver. It takes
//! two `swiftc` invocations (the package module, then the importing
//! driver — see [`super`]), but both run on one miss and stage one
//! binary, so the cache stays **one-level**: a single binary keyed by
//! `{swiftc identity, build flags (incl. the module name), all `.swift`
//! source files}`. There is no chained sub-key and no second namespace.

use crate::build_cache::{InputField, id_segment};
use std::path::PathBuf;

/// Inputs that derive the Swift binary cache key. Every field
/// participates; dropping or adding one requires a schema bump (the
/// shared [`crate::build_cache::key::SCHEMA_TAG`]).
#[derive(Debug, Clone)]
pub struct BinInput {
    /// The Swift toolchain identity — `swiftc --version` output (e.g.
    /// `Swift version 6.3.2 (swift-6.3.2-RELEASE)\nTarget:
    /// x86_64-unknown-linux-gnu`). Probed once per process by
    /// [`super::swiftc_identity`]. A toolchain bump rekeys every entry.
    pub swiftc_identity: String,
    /// Sorted, KV-rendered build flags that affect the emitted binary
    /// (the `--profile`-selected `-O<suffix>` optimization level, the
    /// `-module-name <ns>` the package module is built under, and the
    /// `-file-prefix-map` path-remap directive). Path-varying flags
    /// (`-o`, the build dir the remap maps *from*) are never folded —
    /// they're derived per compile and the remap collapses the build dir
    /// to a fixed string, so they have no bearing on the output bytes.
    pub build_flags: Vec<(String, String)>,
    /// The path-sorted `(relative path, file bytes)` list for every
    /// `.swift` file the `swiftc` invocations read: the emitted
    /// `pkg.swift` / `host.swift` / `shapes.swift` / `ffi.swift` /
    /// `kio_runtime.swift` and the synthesized `main.swift` driver. This
    /// is the content hash that invalidates the cached binary when the
    /// emitted sources or the driver change.
    pub build_files: Vec<(PathBuf, Vec<u8>)>,
}

/// The binary's ordered key-input field list: `{swiftc identity, build
/// flags, build-tree files}`. The shared keyer
/// ([`crate::build_cache::hash_fields`]) folds these into the hex key.
/// A one-level adapter, so no domain separator and no chained sub-key.
pub fn bin_fields(input: &BinInput) -> Vec<InputField> {
    vec![
        InputField::Str(input.swiftc_identity.clone()),
        InputField::KvPairs(input.build_flags.clone()),
        InputField::Files(input.build_files.clone()),
    ]
}

/// The toolchain subroot relative path the Swift adapter partitions
/// the cache by: `swift-<hex8>`. The `<hex8>` is the first-8 of a
/// BLAKE3 hash of the swiftc identity (see [`id_segment`]) so a user
/// listing the cache can see which subtree a Swift-toolchain update
/// invalidated.
///
/// The runner builds for the host platform; swiftc's `Target:` line is
/// part of the `--version` identity, so the host platform is already
/// baked into the identity and there is no separate triple segment.
pub fn subroot_rel(swiftc_identity: &str) -> PathBuf {
    PathBuf::from(format!("swift-{}", id_segment(swiftc_identity)))
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
        "{{\n  \"kind\": \"bin\",\n  \"target\": \"swift\",\n  \"key\": \"{}\",\n  \
         \"swiftc_identity\": {},\n  \"build_flags\": [{}],\n  \"created_at\": {},\n  \
         \"file_count\": {}\n}}\n",
        crate::build_cache::hash_fields(None, &bin_fields(input)),
        json_str(&input.swiftc_identity),
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
/// here (swift-version strings, flag names) never contain control
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
