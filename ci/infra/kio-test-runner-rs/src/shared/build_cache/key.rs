//! Generic, compiler-agnostic key derivation for the build cache.
//!
//! A cache key is the BLAKE3 hash of a canonical byte-blob built from
//! a caller-supplied, ordered list of [`InputField`]s, each
//! length-prefixed so two distinct field lists cannot collide via
//! concatenation ambiguity. The on-disk filename is the lowercase hex
//! of the 32-byte hash, no truncation.
//!
//! The schema tag ([`SCHEMA_TAG`]) is the first field of every key and
//! changes when the on-disk layout or the key derivation changes; the
//! cache root carries a matching [`super::io::CACHE_SCHEMA_VERSION`]
//! segment. Bumping requires a fresh schema version so the old tree
//! stays intact for users who keep their cache populated across
//! upgrades.
//!
//! This module owns *how* fields are framed and hashed. *Which*
//! fields a given artifact's key folds in is the caller's job — a
//! compiler adapter (see [`super::CompilerAdapter`]) hands the cache
//! an ordered `Vec<InputField>` that pins the compiler identity,
//! flags, and source bytes that define that artifact.

#[cfg(feature = "rust")]
use crate::path_display::DisplayPath;
#[cfg(feature = "rust")]
use std::path::Path;
use std::path::PathBuf;

use blake3::Hasher;

/// Schema tag baked into every cache key. Bump in lockstep with the
/// `v<N>/` path segment ([`super::io::CACHE_SCHEMA_VERSION`]) when the
/// on-disk layout or key derivation changes incompatibly, so an old
/// populated tree stays untouched for users who carry their cache
/// across upgrades.
pub const SCHEMA_TAG: &[u8; 4] = b"krl1";

/// One ordered field of a cache key.
///
/// The adapter builds a `Vec<InputField>` describing everything that,
/// if changed, must produce a different cached artifact: the compiler
/// identity string, codegen flags, each emitted source file's relative
/// path and bytes, a chained sub-key, and so on.
///
/// ## Field order is significant
///
/// The key disambiguates fields by **position + length framing**, not
/// by a per-field type tag: a `Str("ab")` and a `Bytes(b"ab")` at the
/// same position hash identically (both are framed length-prefixed
/// bytes). An adapter therefore pins a *fixed field order* for a given
/// cache schema, and reordering fields or swapping a field's variant
/// is a key-derivation change that must move the schema version (see
/// [`SCHEMA_TAG`] / [`super::io::CACHE_SCHEMA_VERSION`]). This framing
/// is byte-for-byte what the original two-level rlib/bin keys used, so
/// the Rust adapter's keys are unchanged across the extraction — warm
/// caches stay warm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputField {
    /// A UTF-8 string field (compiler identity, target triple, edition,
    /// profile name, a chained sub-key, …). Framed as length-prefixed
    /// UTF-8 bytes.
    Str(String),
    /// A raw byte field (a driver's source bytes, an opaque blob).
    /// Framed as length-prefixed bytes. Constructed by the two-level
    /// Rust adapter (the driver source); the one-level go / haskell
    /// adapters fold their whole build tree through [`Self::Files`]
    /// instead, so under their feature builds this variant is unused
    /// API surface, not dead code.
    // Shared cache source is path-included by each native runner. An
    // all-features build therefore sees the Rust-only field in other bins.
    #[allow(dead_code)]
    Bytes(Vec<u8>),
    /// A sorted `(relative path, file bytes)` list — the content hash
    /// of an emitted source tree. Framed as a `u64` count followed by
    /// per-file framed `(path, bytes)` pairs. The caller is responsible
    /// for sorting (e.g. via [`collect_tree_files`], which sorts by
    /// relative path); two lists differing only in order hash
    /// differently, by design, so the adapter must canonicalize order
    /// before handing the field over.
    Files(Vec<(PathBuf, Vec<u8>)>),
    /// Sorted, KV-rendered flag pairs (linker flags, codegen options).
    /// Framed as a `u64` count followed by per-pair framed
    /// `(key, value)`.
    KvPairs(Vec<(String, String)>),
}

/// Length-prefixed framing for one byte run. Hashing
/// `(len_le_u64, bytes)` lets [`Hasher::update`] fold any number of
/// fields without an ambiguity between, say, `(a, bc)` and `(ab, c)`.
fn write_framed(h: &mut Hasher, bytes: &[u8]) {
    let len = bytes.len() as u64;
    h.update(&len.to_le_bytes());
    h.update(bytes);
}

/// Derive a cache key from the schema tag, an optional domain
/// separator, and an ordered field list. The hex string is lowercase,
/// 64 characters (the full 32-byte BLAKE3 hash).
///
/// `domain` separates sibling sub-keys that share a field set so they
/// cannot collide even when every field has the same value — the
/// two-level Rust adapter passes `Some(b"bin\0")` for its bin key so a
/// driver-only bin can never alias the rlib it links against. A
/// one-level adapter passes `None`.
///
/// The byte stream is `framed(SCHEMA_TAG)`, then the raw `domain`
/// bytes (if any), then each field framed in order: a `Str`/`Bytes`
/// field as length-prefixed bytes; a `Files`/`KvPairs` field as a
/// `u64` element count followed by each element's two framed parts.
/// This reproduces the original rlib/bin key derivation exactly — see
/// [`InputField`] on why disambiguation is by position, not type tag.
pub fn hash_fields(domain: Option<&[u8]>, fields: &[InputField]) -> String {
    let mut h = Hasher::new();
    write_framed(&mut h, SCHEMA_TAG);
    if let Some(sep) = domain {
        h.update(sep);
    }
    for field in fields {
        match field {
            InputField::Str(s) => write_framed(&mut h, s.as_bytes()),
            InputField::Bytes(b) => write_framed(&mut h, b),
            InputField::Files(files) => {
                let count = files.len() as u64;
                h.update(&count.to_le_bytes());
                for (path, bytes) in files {
                    // Path bytes via lossy UTF-8 (the emitters never
                    // write non-UTF8 names). Lossy because Rust's
                    // `Path` can carry arbitrary bytes on POSIX, but
                    // the emitters don't.
                    write_framed(&mut h, path.to_string_lossy().as_bytes());
                    write_framed(&mut h, bytes);
                }
            }
            InputField::KvPairs(pairs) => {
                let count = pairs.len() as u64;
                h.update(&count.to_le_bytes());
                for (k, v) in pairs {
                    write_framed(&mut h, k.as_bytes());
                    write_framed(&mut h, v.as_bytes());
                }
            }
        }
    }
    h.finalize().to_hex().to_string()
}

/// First-8 hex characters of a BLAKE3 hash of an identity string. Used
/// to demultiplex the cache by toolchain at a path segment so a user
/// listing the cache can see which subtree a toolchain-update event
/// invalidated (the Rust adapter builds `rustc-<hex8>/` from this).
pub fn id_segment(identity: &str) -> String {
    let h = blake3::hash(identity.as_bytes());
    let hex = h.to_hex();
    hex[..8].to_string()
}

/// Walk `tree_dir` and collect every regular file under it, sorted by
/// relative path (canonical-order walk). The relative paths are
/// prefixed with `rel_root` (e.g. `"src"`) so two trees that differ
/// only in where a file sits hash differently. Output is the payload
/// for an [`InputField::Files`] field — a content hash that
/// invalidates the cached artifact when the emitted sources change.
///
/// The walker is recursive and rejects symlinks: the emitters never
/// write them, so one appearing indicates tampering with the
/// directory. Anything outside `tree_dir` is out of scope by
/// construction.
///
/// Used by the Rust adapter ([`crate::rlib_cache::collect_crate_files`]),
/// which keys a homogeneous `src/` subtree. The one-level go / haskell
/// runners build a heterogeneous tree (a package dir + a driver dir +
/// `go.mod`s) and assemble their `Files` payload directly, so this
/// helper is gated to the rust feature.
#[cfg(feature = "rust")]
#[allow(dead_code)]
pub fn collect_tree_files(
    tree_dir: &Path,
    rel_root: &str,
) -> std::io::Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut out: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    if tree_dir.is_dir() {
        walk_dir(tree_dir, &PathBuf::from(rel_root), &mut out)?;
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[cfg(feature = "rust")]
#[allow(dead_code)]
fn walk_dir(
    dir: &Path,
    rel_prefix: &Path,
    out: &mut Vec<(PathBuf, Vec<u8>)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let name = entry.file_name();
        let rel = rel_prefix.join(&name);
        if ft.is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("symlink in emitted tree: {}", DisplayPath(&rel)),
            ));
        } else if ft.is_dir() {
            walk_dir(&entry.path(), &rel, out)?;
        } else if ft.is_file() {
            let bytes = std::fs::read(entry.path())?;
            out.push((rel, bytes));
        }
    }
    Ok(())
}
