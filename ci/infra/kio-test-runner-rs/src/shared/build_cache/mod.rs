//! Generic, compiler-agnostic content-addressed build cache.
//!
//! Sits between a per-backend test runner's compile step and the
//! eventual artifact use. A build pays the compiler cost only once per
//! `(key-input)` tuple: racing processes lock-and-wait on a per-key
//! advisory file lock, partial writes recover via atomic-rename
//! publication, an LRU sweep bounds the on-disk size, an init-time
//! orphan sweep reaps crashed-compile leftovers, and `meta.json`
//! sidecars let `cat` / `jq` inspect entries.
//!
//! ## What the cache owns vs. what the adapter supplies
//!
//! The cache owns the *mechanism*: BLAKE3 keying over a framed input
//! list ([`key`]), the on-disk layout, `flock` + atomic-rename
//! publication, LRU eviction, the orphan sweep, the `meta.json`
//! sidecars, and the compiler-wrapper hook ([`io`]). It is entirely
//! compiler-agnostic — it never names `rustc`, `go`, `ghc`, or
//! `swiftc`.
//!
//! A [`CompilerAdapter`] supplies the *policy* for one cached
//! artifact: the cache-kind segment and toolchain/target subroot, the
//! ordered [`key::InputField`] list that defines the key (compiler
//! identity, flags, source bytes), the `meta.json` body, and — through
//! the `produce` closure handed to [`BuildCache::get_or_produce`] —
//! the compile/link invocation and the staged artifact's path.
//!
//! ## Two-level vs. one-level adapters
//!
//! The Rust runner caches **two** artifacts per golden — a package
//! rlib and a driver bin — and composes them: it resolves (or
//! compiles) the rlib via one [`ArtifactRequest`], folds the resulting
//! rlib *key* into the bin's input list (a chained sub-key), then
//! resolves the bin via a second request that links against the rlib.
//! Two [`BuildCache::get_or_produce`] calls, two namespaces
//! (`rlibs` / `bins`); a driver-only edit re-keys just the bin.
//!
//! A **one-level** adapter (the go / haskell / swift runners, a later
//! stage) caches a **single** final per-golden binary: one
//! [`ArtifactRequest`] whose input list folds `{compiler identity,
//! flags, all emitted source bytes}` and whose `produce` closure runs
//! the one compile/link that emits the binary, into a single namespace
//! (e.g. `bin`). The trait already fits: such an adapter implements
//! [`CompilerAdapter`] once and makes one `get_or_produce` call — no
//! rlib level, no chained sub-key, no second namespace.

use std::fs;
use std::path::{Path, PathBuf};

use crate::compiler_admission::CompilerAdmission;

pub mod io;
pub mod key;

#[cfg(test)]
mod tests;

pub use io::CacheError;
#[cfg(feature = "rust")]
// Only the rust adapter's key wraps the tree collector; the other
// adapters collect flat file sets themselves.
#[allow(unused_imports)]
pub use key::collect_tree_files;
pub use key::{InputField, hash_fields, id_segment};

/// The build cache's handle. Construct via
/// [`BuildCache::open`]; `get_or_produce` / `resolve`
/// hang off it.
///
/// The handle is `kind`-agnostic — a single `BuildCache` over a cache
/// root serves every artifact namespace a caller asks for; the
/// per-artifact [`ArtifactRequest`] carries the cache-kind segment and
/// subroot. The compiler wrapper and size budget are cache-wide
/// policy.
#[derive(Debug, Clone)]
pub struct BuildCache {
    /// Cache root — the directory the harness supplies. Every miss
    /// writes here; every hit returns the existing artifact.
    root: PathBuf,
    /// Optional compiler wrapper, handed to each `produce` closure via
    /// [`ProduceCtx::compiler_wrapper`]. The wrapper accelerates a
    /// compile but does *not* participate in the key — keying still
    /// uses the real compiler identity the adapter supplies.
    compiler_wrapper: Option<std::ffi::OsString>,
    /// Optional byte budget for cache entries under this root. When
    /// present, successful writes prune least-recently-used entries.
    max_bytes: Option<u64>,
    /// Compiler-process admission injected by runner setup. The cache does
    /// not read scheduler environment; it hands this capability to the
    /// cache-miss producer after the per-key lock and second probe.
    compiler_admission: CompilerAdmission,
}

/// One cached artifact's identity and on-disk shape — everything the
/// cache needs to find or place it, minus the compile itself.
///
/// The adapter builds one of these per artifact it caches. The
/// `key_inputs` list is the content-addressed key (see [`key`]); the
/// remaining fields pin where the artifact lands and how it is
/// published.
pub struct ArtifactRequest {
    /// Cache-kind segment — the top-level partition under the cache
    /// root (the Rust adapter uses `"rlib"`). Distinct kinds keep
    /// unrelated caches in separate subtrees under one root.
    pub kind: String,
    /// Toolchain/target subroot relative path (the Rust adapter builds
    /// `rustc-<hex8>/<triple>`). Partitions the cache so a
    /// toolchain-update event invalidates one visible subtree.
    pub subroot_rel: PathBuf,
    /// Artifact namespace under the subroot (`"rlibs"`, `"bins"`, or a
    /// one-level adapter's single `"bin"`).
    pub namespace: String,
    /// Short role tag baked into staging-tempdir names
    /// (`<pid>-<counter>-<role>-<hex>`) so the orphan reap recognizes
    /// the layout. `"rlib"` / `"bin"`.
    pub role: String,
    /// Ordered key-input field list. Hashed (together with the
    /// `hash_domain` below) into the entry's hex key. Changing any field
    /// re-keys the artifact.
    pub key_inputs: Vec<InputField>,
    /// Optional domain separator folded into the key ahead of the
    /// fields, separating sibling sub-keys that share a field set (the
    /// Rust bin key passes `Some(b"bin\0")`; a one-level adapter passes
    /// `None`). See [`hash_fields`].
    pub hash_domain: Option<Vec<u8>>,
    /// The published artifact's filename inside its `<hex>/` key
    /// directory (`"lib.rlib"`, `"bin"`).
    pub artifact_name: String,
    /// The staged artifact's filename inside the `produce` closure's
    /// out-dir, when it differs from `artifact_name` (rustc emits
    /// `lib<crate>.rlib`, published as `lib.rlib`). `None` means the
    /// closure stages the file under `artifact_name` directly.
    pub staged_name: Option<String>,
    /// Whether to set the Unix executable bit on the published
    /// artifact (`true` for a bin, `false` for an rlib).
    pub executable: bool,
    /// The `meta.json` sidecar body — readable JSON the adapter renders
    /// so a user can `cat` / `jq` the entry. The cache writes it
    /// alongside the artifact; its shape is the adapter's business.
    pub meta_json: String,
}

/// Context handed to a `produce` closure on a cache miss.
///
/// The closure compiles/links the artifact into `out_dir` (staging it
/// under the request's `staged_name`, or `artifact_name` when that is
/// `None`), invoking the compiler through `compiler_wrapper` when set.
pub struct ProduceCtx<'a> {
    /// The staging tempdir the closure must write its artifact into.
    /// The cache atomic-renames the staged file into place on success.
    pub out_dir: &'a Path,
    /// The cache-wide compiler wrapper, if any. The closure runs the
    /// compiler as `<wrapper> <compiler> …` when present, bare
    /// otherwise. The wrapper never feeds the key.
    pub compiler_wrapper: Option<&'a std::ffi::OsString>,
    /// Admission capability for the adapter's actual compiler command. Cache
    /// staging and publication deliberately hold no compiler permit.
    pub compiler_admission: &'a CompilerAdmission,
}

/// The per-artifact policy a runner implements to plug a compiler into
/// the shared cache.
///
/// One implementation describes one cached artifact. A two-level
/// runner (Rust) implements it twice — once for the rlib, once for the
/// bin — and composes the two with a chained sub-key. A one-level
/// runner (go / haskell / swift, later) implements it once for the
/// final binary. Either way the adapter never touches keying, locking,
/// publication, or eviction — those are [`BuildCache`]'s.
///
/// The trait is intentionally thin: it bundles the artifact's
/// [`ArtifactRequest`] (its identity + on-disk shape) with its
/// `produce` (the compile/link). A runner is free to skip the trait
/// and call [`BuildCache::get_or_produce`] with an inline request +
/// closure; the trait exists to give a multi-artifact runner a named,
/// testable unit per artifact.
pub trait CompilerAdapter {
    /// The adapter's compile error type, surfaced through the
    /// `produce`-closure boundary when a compile/link fails. Kept off
    /// the cache's own [`CacheError`] so the machinery stays
    /// compiler-agnostic.
    type Error;

    /// Build this artifact's [`ArtifactRequest`] — its cache-kind,
    /// subroot, namespace, key-input list, and on-disk shape.
    fn request(&self) -> ArtifactRequest;

    /// Compile/link the artifact into `ctx.out_dir` on a cache miss.
    /// Stages the file under the request's `staged_name` (or
    /// `artifact_name`); the cache publishes it atomically.
    fn produce(&self, ctx: &ProduceCtx<'_>) -> Result<(), Self::Error>;
}

impl BuildCache {
    /// Open with an injected compiler-process admission capability. The
    /// caller chooses an active or explicitly disabled capability; the cache
    /// never consults scheduler environment itself.
    pub fn open(
        root: PathBuf,
        compiler_wrapper: Option<std::ffi::OsString>,
        max_bytes: Option<u64>,
        compiler_admission: CompilerAdmission,
    ) -> Result<Self, CacheError> {
        io::ensure_dir(&root)?;
        io::write_gitignore(&root)?;
        Ok(BuildCache {
            root,
            compiler_wrapper,
            max_bytes,
            compiler_admission,
        })
    }

    /// Resolve an artifact via an [`CompilerAdapter`]: hit returns the
    /// existing path; miss acquires the per-key lock, runs
    /// `adapter.produce` into a tempdir, and atomic-renames into place.
    ///
    /// The adapter's compile error is wrapped in [`ProduceError::Compile`];
    /// a cache I/O failure is [`ProduceError::Cache`].
    pub fn resolve<A: CompilerAdapter>(
        &self,
        adapter: &A,
    ) -> Result<PathBuf, ProduceError<A::Error>> {
        let request = adapter.request();
        self.get_or_produce(&request, |ctx| adapter.produce(ctx))
    }

    /// The lower-level entry: resolve `request`, calling `produce` on a
    /// miss. Useful when a runner wants an inline request + closure
    /// rather than a named [`CompilerAdapter`]; `resolve` is the thin
    /// wrapper over it.
    ///
    /// The returned hex key (via the published path's parent dir name)
    /// is what a two-level caller folds into its next artifact's input
    /// list as a chained sub-key — call [`ArtifactRequest::key`] to
    /// compute it without producing.
    pub fn get_or_produce<E>(
        &self,
        request: &ArtifactRequest,
        produce: impl FnOnce(&ProduceCtx<'_>) -> Result<(), E>,
    ) -> Result<PathBuf, ProduceError<E>> {
        let hex = request.key();
        let ns = io::namespace_dir(
            &self.root,
            &request.kind,
            &request.subroot_rel,
            &request.namespace,
        )
        .map_err(ProduceError::Cache)?;
        io::reap_orphan_tempdirs(&ns.tmp).map_err(ProduceError::Cache)?;
        let key_dir = ns.artifacts.join(&hex);
        let final_artifact = key_dir.join(&request.artifact_name);
        if is_regular_file_nonempty(&final_artifact) {
            mark_used(&key_dir);
            return Ok(final_artifact);
        }
        // Miss: acquire lock, re-probe, produce if still miss.
        let _lock = io::acquire_lock(&key_dir).map_err(ProduceError::Cache)?;
        if is_regular_file_nonempty(&final_artifact) {
            mark_used(&key_dir);
            return Ok(final_artifact);
        }
        let tempdir =
            io::make_tempdir(&ns.tmp, &request.role, &hex).map_err(ProduceError::Cache)?;
        let ctx = ProduceCtx {
            out_dir: &tempdir,
            compiler_wrapper: self.compiler_wrapper.as_ref(),
            compiler_admission: &self.compiler_admission,
        };
        let produce_result = produce(&ctx);
        match produce_result {
            Ok(()) => {
                let staged = tempdir.join(
                    request
                        .staged_name
                        .as_ref()
                        .unwrap_or(&request.artifact_name),
                );
                let meta_path = tempdir.join("meta.json");
                let publish = (|| {
                    fs::write(&meta_path, &request.meta_json).map_err(|e| io::CacheError::Io {
                        path: meta_path.clone(),
                        source: e,
                    })?;
                    io::atomic_rename(&staged, &final_artifact)?;
                    if request.executable {
                        set_executable(&final_artifact);
                    }
                    let _ = fs::rename(&meta_path, key_dir.join("meta.json"));
                    Ok(())
                })();
                match publish {
                    Ok(()) => {
                        mark_used(&key_dir);
                        self.prune_lru(&request.kind, &key_dir);
                        io::cleanup_tempdir(&tempdir);
                        Ok(final_artifact)
                    }
                    Err(e) => {
                        io::cleanup_tempdir(&tempdir);
                        Err(ProduceError::Cache(e))
                    }
                }
            }
            Err(e) => {
                io::cleanup_tempdir(&tempdir);
                Err(ProduceError::Compile(e))
            }
        }
    }

    fn prune_lru(&self, kind: &str, protected: &Path) {
        if let Some(max_bytes) = self.max_bytes {
            prune_lru_cache(&self.root, kind, max_bytes, protected);
        }
    }
}

/// The error a `get_or_produce` / `resolve` call can fail with: either
/// the adapter's compile error, or a cache I/O failure.
#[derive(Debug)]
pub enum ProduceError<E> {
    /// `adapter.produce` failed — a compile/link error. Carries the
    /// adapter's own error type unchanged.
    Compile(E),
    /// The cache's own I/O failed (lock, rename, directory create).
    Cache(CacheError),
}

impl<E: std::fmt::Display> std::fmt::Display for ProduceError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProduceError::Compile(e) => write!(f, "{e}"),
            ProduceError::Cache(e) => write!(f, "{e}"),
        }
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for ProduceError<E> {}

impl ArtifactRequest {
    /// This artifact's hex cache key — the BLAKE3 hash of its
    /// `key_inputs` under [`hash_fields`], with the request's
    /// `hash_domain`. A two-level caller folds an already-resolved
    /// artifact's key (computed here) into the next artifact's input
    /// list as a chained sub-key, without re-resolving.
    pub fn key(&self) -> String {
        hash_fields(self.hash_domain.as_deref(), &self.key_inputs)
    }
}

/// Probe a path: is this a regular file with non-zero length? The
/// read path treats symlinks / directories / empty files as a miss
/// so a corrupted entry falls through to the write path cleanly.
fn is_regular_file_nonempty(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(md) => md.is_file() && md.len() > 0,
        Err(_) => false,
    }
}

fn mark_used(key_dir: &Path) {
    let _ = fs::write(key_dir.join(".last_used"), b"");
}

fn set_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[derive(Debug)]
struct LruEntry {
    path: PathBuf,
    bytes: u64,
    used: std::time::SystemTime,
}

fn prune_lru_cache(root: &Path, kind: &str, max_bytes: u64, protected: &Path) {
    let mut entries = Vec::new();
    let mut total = 0_u64;
    collect_cache_entries(root, kind, &mut entries, &mut total);
    if total <= max_bytes {
        return;
    }
    entries.sort_by_key(|entry| entry.used);
    for entry in entries {
        if total <= max_bytes {
            break;
        }
        if entry.path == protected {
            continue;
        }
        if fs::remove_dir_all(&entry.path).is_ok() {
            total = total.saturating_sub(entry.bytes);
        }
    }
}

/// Walk every published key-entry under a cache kind and gather its
/// size + last-used time for the LRU sweep.
///
/// The walk is **structural**, not name-driven: a directory that
/// directly holds the artifact (a non-empty regular file, past the
/// `.lock` / `.last_used` bookkeeping) is a key-entry — measured and
/// recorded; any other directory is an intermediate segment to descend
/// into. So the sweep covers the Rust adapter's `…/<triple>/rlibs/<hex>/`
/// and `…/bins/<hex>/` entries and a one-level adapter's
/// `…/bin/<hex>/` entries alike, with no namespace-name list to keep in
/// sync. The `tmp/` staging namespace is skipped: a half-written
/// compile lives there and must never be evicted as if published.
fn collect_cache_entries(root: &Path, kind: &str, entries: &mut Vec<LruEntry>, total: &mut u64) {
    let schema = io::schema_root(root, kind);
    collect_subtree(&schema, entries, total);
}

fn collect_subtree(dir: &Path, entries: &mut Vec<LruEntry>, total: &mut u64) {
    if dir_has_nonempty_file(dir) {
        // This directory is a published key-entry (`<hex>/` holding the
        // artifact). Record it; do not descend further.
        let bytes = dir_size(dir);
        *total = total.saturating_add(bytes);
        entries.push(LruEntry {
            used: entry_used_at(dir),
            path: dir.to_path_buf(),
            bytes,
        });
        return;
    }
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for child in read.filter_map(Result::ok) {
        let path = child.path();
        if !path.is_dir() {
            continue;
        }
        if child.file_name() == "tmp" {
            // Staging namespace — a half-written compile, never a
            // publishable entry.
            continue;
        }
        collect_subtree(&path, entries, total);
    }
}

/// Whether a key dir holds at least one non-empty regular file other
/// than the bookkeeping `.lock` / `.last_used`. Mirrors the read
/// path's "an entry exists iff its artifact is a non-empty regular
/// file" rule without hard-coding the artifact's name.
fn dir_has_nonempty_file(dir: &Path) -> bool {
    let Ok(read) = fs::read_dir(dir) else {
        return false;
    };
    for entry in read.filter_map(Result::ok) {
        let name = entry.file_name();
        if name == ".lock" || name == ".last_used" {
            continue;
        }
        if is_regular_file_nonempty(&entry.path()) {
            return true;
        }
    }
    false
}

fn entry_used_at(path: &Path) -> std::time::SystemTime {
    path.join(".last_used")
        .metadata()
        .and_then(|md| md.modified())
        .or_else(|_| path.metadata().and_then(|md| md.modified()))
        .unwrap_or(std::time::UNIX_EPOCH)
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0_u64;
    for entry in entries.filter_map(Result::ok) {
        let Ok(md) = entry.metadata() else {
            continue;
        };
        if md.is_dir() {
            total = total.saturating_add(dir_size(&entry.path()));
        } else if md.is_file() {
            total = total.saturating_add(md.len());
        }
    }
    total
}
