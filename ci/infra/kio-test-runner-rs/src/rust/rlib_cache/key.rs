//! Rust-specific inputs for the rlib + bin cache, and their mapping
//! to the shared cache's key-input field list + `meta.json` bodies.
//!
//! The generic keying — BLAKE3 over a framed [`InputField`] list — is
//! [`crate::build_cache::key`]. This module supplies the Rust
//! *policy*: which fields each artifact's key folds in
//! ([`rlib_fields`] / [`bin_fields`]), the toolchain/target subroot
//! path ([`subroot_rel`]), and the readable sidecar bodies
//! ([`rlib_meta_json`] / [`bin_meta_json`]).
//!
//! The bin key chains the rlib key: change `host.rs`, the rlib's
//! source-tree field changes → its key changes → the bin's chained
//! sub-key field changes. Supply a different driver, only the bin's
//! own driver-bytes field changes. A `b"bin\0"` domain separator (set
//! on the bin's [`crate::build_cache::ArtifactRequest`]) keeps the two
//! sub-keys from colliding even with identical fields.

use crate::build_cache::{InputField, collect_tree_files, id_segment};
use crate::opt_profile::OptProfile;
use std::path::{Path, PathBuf};

/// The rustc flags a runner [`OptProfile`] expands to for the Rust
/// compile. Applied by the write path when invoking rustc; *not* fed to
/// the key directly — the profile's [`OptProfile::name`] is, and the
/// name → flags mapping is fixed for the cache's lifetime, so the name
/// suffices to keep one profile's artifacts from aliasing another's.
/// Every profile keeps `-C debuginfo=0`: the runner never needs debug
/// info, so the profiles vary only the optimization level.
pub fn rustc_flags(profile: OptProfile) -> &'static [&'static str] {
    match profile {
        OptProfile::Unoptimized => &["-C", "opt-level=0", "-C", "debuginfo=0"],
        OptProfile::Default => &["-C", "opt-level=1", "-C", "debuginfo=0"],
        OptProfile::Optimized => &["-C", "opt-level=2", "-C", "debuginfo=0"],
    }
}

/// Inputs that derive an rlib cache key. Every field participates;
/// dropping or adding a field requires a schema bump.
#[derive(Debug, Clone)]
pub struct RlibInput {
    /// Stable subset of `rustc --version --verbose`: version line +
    /// commit-hash + host triple. The cache probes rustc once per
    /// process and the result lives in [`super::rustc_identity`].
    pub rustc_identity: String,
    /// Target triple (`x86_64-unknown-linux-gnu`, etc.).
    pub target_triple: String,
    /// Rust edition the emitted crate pins (e.g. `2024`).
    pub edition: String,
    /// The `--profile`-selected optimization level. See [`OptProfile`]
    /// and [`rustc_flags`]. Its [`OptProfile::name`] is folded into the
    /// key so a `default`-built rlib never aliases an `optimized` one.
    pub profile: OptProfile,
    /// The harness-supplied artifact namespace that the runner passes to
    /// `--crate-name`. Part of the key so two namespace configurations
    /// never alias in the cache (their `crate_files` normally differ too,
    /// so this is belt-and-suspenders).
    pub crate_name: String,
    /// The path-sorted list of `(relative path, file bytes)` for
    /// every file under the emitted crate's `src/`. Built by
    /// [`collect_crate_files`]. The top-level `Cargo.toml` is not
    /// included — the runner compiles `src/lib.rs` under a fixed
    /// crate name, so the manifest's content never affects the rlib.
    pub crate_files: Vec<(PathBuf, Vec<u8>)>,
}

/// Walk an emitted crate root and collect every regular file under
/// `src/`, sorted by relative path. Output is the `crate_files` input
/// for [`RlibInput`] — a content hash that invalidates the cached rlib
/// when the emitted sources change. The walk covers `src/` only and
/// never the top-level `Cargo.toml` (the runner forces a fixed
/// `--crate-name`, so the manifest has no bearing on the rlib). It
/// rejects symlinks: the emitter never writes them, so one appearing
/// indicates tampering.
///
/// A thin wrapper over the shared [`collect_tree_files`] pinned to the
/// `src/` subtree.
pub fn collect_crate_files(crate_dir: &Path) -> std::io::Result<Vec<(PathBuf, Vec<u8>)>> {
    collect_tree_files(&crate_dir.join("src"), "src")
}

/// The rlib's ordered key-input field list: `{rustc identity, target,
/// edition, profile name, crate name, src-tree files}`. The shared
/// keyer ([`crate::build_cache::hash_fields`]) folds these into the
/// hex key.
pub fn rlib_fields(input: &RlibInput) -> Vec<InputField> {
    vec![
        InputField::Str(input.rustc_identity.clone()),
        InputField::Str(input.target_triple.clone()),
        InputField::Str(input.edition.clone()),
        InputField::Str(input.profile.name().to_owned()),
        InputField::Str(input.crate_name.clone()),
        InputField::Files(input.crate_files.clone()),
    ]
}

/// Inputs that derive a bin cache key. Sits on top of an rlib cache
/// key: change `host.rs`, the rlib invalidates; supply a different
/// driver, only the bin invalidates.
#[derive(Debug, Clone)]
pub struct BinInput {
    /// Same rustc / target / edition / profile inputs as the rlib
    /// (the bin compile inherits the toolchain settings).
    pub rustc_identity: String,
    pub target_triple: String,
    pub edition: String,
    pub profile: OptProfile,
    /// The rlib's cache key — *not* the rlib bytes. The rlib bytes
    /// are a deterministic function of the inputs that produced its
    /// key, so the key suffices. Folded into the field list below as a
    /// chained sub-key.
    pub rlib_key: String,
    /// Crate name the driver `--extern`s against. Carried in the
    /// linker-flags input below but also fed directly to keep
    /// future driver-render changes from accidentally drifting.
    pub crate_name: String,
    /// The driver `.rs` file's byte stream. Stable across runs for
    /// a given host trait so cache hit rate is high.
    pub driver_source: Vec<u8>,
    /// Sorted, KV-rendered linker flags (excluding paths that vary
    /// per run — `--out-dir`, the rlib path; both are derived from
    /// the key on the other side).
    pub linker_flags: Vec<(String, String)>,
}

/// The bin's ordered key-input field list: `{rustc identity, target,
/// edition, profile name, chained rlib key, crate name, driver bytes,
/// linker flags}`. Hashed under the `b"bin\0"` domain separator (set
/// on the request) so the bin sub-key can't collide with the rlib's.
pub fn bin_fields(input: &BinInput) -> Vec<InputField> {
    vec![
        InputField::Str(input.rustc_identity.clone()),
        InputField::Str(input.target_triple.clone()),
        InputField::Str(input.edition.clone()),
        InputField::Str(input.profile.name().to_owned()),
        InputField::Str(input.rlib_key.clone()),
        InputField::Str(input.crate_name.clone()),
        InputField::Bytes(input.driver_source.clone()),
        InputField::KvPairs(input.linker_flags.clone()),
    ]
}

/// The toolchain/target subroot relative path the Rust adapter
/// partitions the cache by: `rustc-<hex8>/<triple>`. The `<hex8>` is
/// the first-8 of a BLAKE3 hash of the rustc identity (see
/// [`id_segment`]) so a user listing the cache can see which subtree a
/// `rustup update` event invalidated.
pub fn subroot_rel(rustc_identity: &str, target_triple: &str) -> PathBuf {
    PathBuf::from(format!("rustc-{}", id_segment(rustc_identity))).join(target_triple)
}

/// Render the rlib `meta.json` sidecar. Readable JSON so a user can
/// `cat` / `jq` it. Hand-rolled to avoid pulling a JSON crate just for
/// this — the sidecar's shape is narrow.
pub fn rlib_meta_json(input: &RlibInput) -> String {
    let now = unix_now();
    format!(
        "{{\n  \"kind\": \"rlib\",\n  \"key\": \"{}\",\n  \"crate_name\": {},\n  \
         \"rustc_identity\": {},\n  \"target_triple\": {},\n  \"edition\": {},\n  \
         \"profile\": \"{}\",\n  \"created_at\": {},\n  \"file_count\": {}\n}}\n",
        super::rlib_key(input),
        json_str(&input.crate_name),
        json_str(&input.rustc_identity),
        json_str(&input.target_triple),
        json_str(&input.edition),
        input.profile.name(),
        now,
        input.crate_files.len(),
    )
}

/// Render the bin `meta.json` sidecar.
pub fn bin_meta_json(rlib_input: &RlibInput, bin_input: &BinInput) -> String {
    let now = unix_now();
    format!(
        "{{\n  \"kind\": \"bin\",\n  \"key\": \"{}\",\n  \"rlib_key\": {},\n  \
         \"crate_name\": {},\n  \"rustc_identity\": {},\n  \"target_triple\": {},\n  \
         \"edition\": {},\n  \"profile\": \"{}\",\n  \"created_at\": {}\n}}\n",
        crate::build_cache::hash_fields(Some(b"bin\0"), &bin_fields(bin_input)),
        json_str(&bin_input.rlib_key),
        json_str(&bin_input.crate_name),
        json_str(&rlib_input.rustc_identity),
        json_str(&rlib_input.target_triple),
        json_str(&rlib_input.edition),
        bin_input.profile.name(),
        now,
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Minimal JSON string escaping for the `meta.json` sidecar. The
/// values here (paths, rustc-version strings, identifiers) never
/// contain control characters in practice; we still handle quote /
/// backslash / control-byte cases so a hostile filename can't break
/// the sidecar.
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
