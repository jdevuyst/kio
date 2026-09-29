//! Directory layout, atomic-rename writes, advisory locking, and
//! init-time tempdir reap for the build cache.
//!
//! Layout (the segments the caller controls are bracketed):
//!
//! ```text
//! <cache>/
//!   [<kind>]/                       cache-kind segment (e.g. "rlib")
//!     v1/                           schema version (owned here)
//!       [<subroot…>]/               toolchain/target partition
//!         [<namespace>]/            artifact namespace (e.g. "rlibs")
//!           <hex>/{<artifact>, meta.json, .lock}
//!         tmp/
//!           <pid>-<counter>-<role>-<hex>/
//! ```
//!
//! The `<kind>` segment, the `<subroot…>` relative path, and the
//! per-artifact `<namespace>` are supplied by the cache's caller (a
//! compiler adapter); the `v1/` schema segment and the `tmp/` staging
//! namespace are owned here. Every cache directory carries a
//! `.gitignore` at its root on first write so the contents stay out of
//! `git status` regardless of where the user placed it.

use crate::path_display::DisplayPath;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;

/// On-disk schema version segment. Bumping requires a key-derivation
/// or layout change (paired with the [`super::key::SCHEMA_TAG`] bump);
/// the user clears the stale tree with `rm -rf <cache>/<kind>/v<N-1>/`.
pub const CACHE_SCHEMA_VERSION: &str = "v1";

/// Twenty-four hours, in seconds. Tempdir orphans older than this
/// are reaped at the namespace's first use so a crashed prior
/// process's half-written compile doesn't waste disk forever.
pub const TEMPDIR_REAP_AGE_SECS: u64 = 24 * 60 * 60;

/// Per-process counter feeding the tempdir name so two concurrent
/// compiles in the same process never collide on the same
/// `<pid>-<counter>` segment.
static TEMPDIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Errors surfaced from the cache I/O layer. Each variant carries a
/// path so the user can see which directory the failure came from.
///
/// A compile/link or compiler-admission failure is the adapter's own error
/// type, returned through the `produce`-closure boundary and surfaced by the
/// caller (see [`super::CompilerAdapter`]). Keeping compiler concerns out of
/// the cache's error enum is what lets the machinery stay compiler-agnostic.
#[derive(Debug)]
pub enum CacheError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheError::Io { path, source } => {
                write!(f, "kio cache I/O at {}: {}", DisplayPath(&path), source)
            }
        }
    }
}

impl std::error::Error for CacheError {}

fn io_err(path: &Path, source: std::io::Error) -> CacheError {
    CacheError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Create `dir` and every missing parent. The cache root and its
/// sub-namespaces are created lazily on first write.
pub fn ensure_dir(dir: &Path) -> Result<(), CacheError> {
    if dir.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(dir).map_err(|e| io_err(dir, e))
}

/// On first write to a cache root, drop a `.gitignore` containing
/// `*\n!.gitignore` so the cache contents are gitignored
/// automatically — mirrors cargo's `target/` convention. Idempotent;
/// the file is only written if absent.
pub fn write_gitignore(cache_root: &Path) -> Result<(), CacheError> {
    let path = cache_root.join(".gitignore");
    if path.exists() {
        return Ok(());
    }
    ensure_dir(cache_root)?;
    let opened = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path);
    match opened {
        Ok(mut file) => {
            file.write_all(b"*\n!.gitignore\n")
                .map_err(|e| io_err(&path, e))?;
            Ok(())
        }
        // A racing process won the create-new; the existing file
        // is fine.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(io_err(&path, e)),
    }
}

/// A resolved cache subroot: the directory that holds one named
/// artifact namespace plus the shared `tmp/` staging namespace. The
/// caller resolves one of these per `(kind, subroot, namespace)` it
/// touches; the `tmp/` sibling is shared across every namespace under
/// the same subroot.
#[derive(Debug, Clone)]
pub struct NamespaceDir {
    /// The artifact namespace directory (`…/<subroot>/<namespace>/`).
    pub artifacts: PathBuf,
    /// The shared staging namespace (`…/<subroot>/tmp/`).
    pub tmp: PathBuf,
}

/// Resolve and materialise the `<cache>/<kind>/v1/<subroot…>/<namespace>`
/// artifact directory and its sibling `tmp/`.
///
/// `subroot_rel` is an adapter-built relative path partitioning the
/// cache by toolchain/target (the Rust adapter passes
/// `rustc-<hex8>/<triple>`). `namespace` is the artifact bucket under
/// it (`rlibs`, `bins`, or a one-level adapter's single `bin`).
pub fn namespace_dir(
    cache_root: &Path,
    kind: &str,
    subroot_rel: &Path,
    namespace: &str,
) -> Result<NamespaceDir, CacheError> {
    let subroot = cache_root
        .join(kind)
        .join(CACHE_SCHEMA_VERSION)
        .join(subroot_rel);
    let artifacts = subroot.join(namespace);
    let tmp = subroot.join("tmp");
    ensure_dir(&artifacts)?;
    ensure_dir(&tmp)?;
    Ok(NamespaceDir { artifacts, tmp })
}

/// The `<cache>/<kind>/v1` schema root, used by the LRU sweep to walk
/// every subroot under a cache kind.
pub fn schema_root(cache_root: &Path, kind: &str) -> PathBuf {
    cache_root.join(kind).join(CACHE_SCHEMA_VERSION)
}

/// Acquire the exclusive per-key advisory lock and return an open
/// file handle. The kernel releases the lock when the handle drops
/// (including on process death). Callers hold the handle for the
/// duration of the compile / rename.
///
/// The lock file lives inside the per-key directory at `.lock`; the
/// directory is created if absent.
pub fn acquire_lock(key_dir: &Path) -> Result<fs::File, CacheError> {
    ensure_dir(key_dir)?;
    let lock_path = key_dir.join(".lock");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| io_err(&lock_path, e))?;
    file.lock_exclusive().map_err(|e| io_err(&lock_path, e))?;
    Ok(file)
}

/// Allocate an in-flight tempdir under `tmp/`. Naming follows
/// `<pid>-<counter>-<role>-<hex>` so post-mortem orphan reaping can
/// recognize the layout.
pub fn make_tempdir(tmp_root: &Path, role: &str, hex: &str) -> Result<PathBuf, CacheError> {
    let pid = std::process::id();
    let counter = TEMPDIR_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = tmp_root.join(format!("{pid}-{counter}-{role}-{hex}"));
    ensure_dir(&dir)?;
    Ok(dir)
}

/// Atomically publish `from` → `to`. POSIX `rename` is atomic
/// within a filesystem; the cache co-locates tempdir + final
/// directory on the same filesystem by construction.
pub fn atomic_rename(from: &Path, to: &Path) -> Result<(), CacheError> {
    if let Some(parent) = to.parent() {
        ensure_dir(parent)?;
    }
    fs::rename(from, to).map_err(|e| io_err(to, e))
}

/// Best-effort tempdir cleanup. A failure here is a stderr warning,
/// not a hard error — the orphan reap on the next init handles it.
pub fn cleanup_tempdir(tempdir: &Path) {
    let _ = fs::remove_dir_all(tempdir);
}

/// Init-time orphan-tempdir sweep: any entry under `<tmp>/` whose
/// `<pid>-...` mtime is older than [`TEMPDIR_REAP_AGE_SECS`] is
/// removed. Bounded-window so the orphan disk-waste from killed
/// compiles stays finite.
pub fn reap_orphan_tempdirs(tmp_root: &Path) -> Result<(), CacheError> {
    if !tmp_root.is_dir() {
        return Ok(());
    }
    let now = std::time::SystemTime::now();
    for entry in fs::read_dir(tmp_root).map_err(|e| io_err(tmp_root, e))? {
        let entry = entry.map_err(|e| io_err(tmp_root, e))?;
        let path = entry.path();
        let md = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = match md.modified() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let age = match now.duration_since(mtime) {
            Ok(d) => d,
            Err(_) => continue,
        };
        if age.as_secs() > TEMPDIR_REAP_AGE_SECS {
            let _ = fs::remove_dir_all(&path);
        }
    }
    Ok(())
}
