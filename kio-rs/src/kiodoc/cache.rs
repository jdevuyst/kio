//! Per-snippet on-disk cache for `kio doc`.
//!
//! Sits between [`super::validate`] and the `kio check` it would
//! otherwise run for every snippet on every invocation. A snippet
//! whose `(harness_body, snippet_body)` pair hasn't changed and
//! whose compiler cache identity matches the cached entry resolves
//! on the read path with no typechecker call; only a miss runs the
//! full validator and writes the result on the way out.
//!
//! ## Layout
//!
//! ```text
//! <cache>/doc/
//!   <hex>.bin        — one cache entry per full key
//!   <hex>.bin.tmp.*  — in-flight writes; atomic-renamed into place
//! ```
//!
//! The flat layout keeps invalidation trivial: a stale entry is
//! overwritten on next miss, and the whole directory can be removed
//! manually (`rm -rf <cache>/doc/`) to force a full re-check. No
//! GC, no LRU — same convention as [`crate::cache::package_check`].
//!
//! ## Cache file format
//!
//! Each entry is a small line-based text file. Mirrors
//! [`crate::cache::package_check`]'s header discipline: magic,
//! cache namespace, implementation/compiler identity, exact key, and body
//! digest on line 1, with the cached result shape on its own line. A
//! header mismatch (different namespace, different implementation, etc.)
//! is treated as a miss — the loader **never panics** on a stale
//! or malformed file, only logs a one-line warning and falls
//! through to recomputation.
//!
//! ## Cached result shape
//!
//! The validator's only externally-observable outcome is the
//! `kio check` exit code. The cache stores that integer; the
//! validator's own diagnostic for a `check_exit_code` mismatch is
//! a function of the current snippet body, the current snippet's
//! `open_line`, and the cached exit code — so it re-renders
//! identically on a hit. `kio check`'s own diagnostics (printed
//! during the cold call) are *not* cached for replay; every
//! existing golden that exercises the snippet-fails path declares
//! `expected.stderr.ignore`, so the warm path's empty stderr from
//! the typechecker is not user-visible.
//!
//! ## Hit / miss instrumentation
//!
//! Setting `KIO_DEBUG_DOC_CACHE=1` prints one line per snippet to
//! stderr: `kiodoc-cache: hit <key>` or `kiodoc-cache: miss <key>`.
//! Mirrors the convention the package-check cache uses for its own probe
//! lines.

use crate::cache::identity::{
    COMPILER_CACHE_ID, COMPILER_VERSION, FEATURE_SET, IMPLEMENTATION_TAG,
};
use crate::path_display::DisplayPath;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use blake3::Hasher;

pub const CACHE_NAMESPACE: &str = "doc";

/// Per-process counter feeding the tempfile name so two concurrent
/// writes in the same process never collide on the same
/// `<pid>-<counter>` segment. Mirrors the enriched-IR cache's
/// tempdir naming.
static TEMPFILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Public handle on the per-snippet doc cache. Construct via
/// [`DocCache::open`] (active cache rooted at a user-declared
/// path) or [`DocCache::disabled`] (the `cache ();` form).
#[derive(Debug, Clone)]
pub struct DocCache {
    inner: Backend,
}

#[derive(Debug, Clone)]
enum Backend {
    /// Active cache: every miss writes, every hit returns the
    /// cached result.
    Active { cache_root: PathBuf, root: PathBuf },
    /// Caching opted out: every lookup misses, every store is a
    /// no-op. Mirrors the `cache ();` build-block form.
    Disabled,
}

impl DocCache {
    /// Open a doc cache rooted under `cache_root` — typically the
    /// `<cache>` directory the build block's `cache "<path>";`
    /// declared. Materializes the doc-cache subdirectory and
    /// writes the cache root's `.gitignore` if absent.
    pub fn open(cache_root: PathBuf) -> std::io::Result<Self> {
        let doc_dir = cache_root.join("doc");
        fs::create_dir_all(&doc_dir)?;
        write_gitignore_if_absent(&cache_root)?;
        crate::cache::gc::record_cache_open(&cache_root, crate::cache::gc::CacheFamily::Doc);
        Ok(DocCache {
            inner: Backend::Active {
                cache_root,
                root: doc_dir,
            },
        })
    }

    /// Construct a no-op cache — mirrors `cache ();` in the build
    /// block. Every [`lookup`] misses and every [`store`] is a no-op.
    ///
    /// [`lookup`]: DocCache::lookup
    /// [`store`]: DocCache::store
    pub fn disabled() -> Self {
        DocCache {
            inner: Backend::Disabled,
        }
    }

    /// Resolve a doc cache from the package's package file at
    /// `workspace_root`. The rule mirrors `kio build` / the rlib
    /// cache: if the directory contains a `<name>.pkg.kio`
    /// whose `build { cache "<path>"; }` block declares a path,
    /// open the cache rooted there (`<path>` is relative to
    /// `workspace_root` for relative strings, taken as-is for
    /// absolute strings). If the block declares `cache ();`, the
    /// package file has no build block, no package file is
    /// present, or the file is unreadable / unparseable, return a
    /// [`disabled`] cache. The `kio doc` driver is a read-only
    /// consumer — a bad package file is a build-time problem the
    /// build / check commands surface, not a doc-validation one.
    ///
    /// [`disabled`]: DocCache::disabled
    pub fn resolve_from_workspace(workspace_root: &Path) -> Self {
        match resolve_cache_dir(workspace_root) {
            Some(cache_root) => match DocCache::open(cache_root) {
                Ok(c) => c,
                // I/O failure opening the cache directory: don't
                // block `kio doc`, just run every snippet uncached.
                // The user gets the same observable behavior as
                // `cache ();`.
                Err(_) => DocCache::disabled(),
            },
            None => DocCache::disabled(),
        }
    }

    /// True iff caching is enabled (the active variant). Exposed
    /// so callers (notably the kiodoc driver) can avoid the
    /// per-snippet key computation when the cache is disabled.
    pub fn is_enabled(&self) -> bool {
        matches!(self.inner, Backend::Active { .. })
    }

    /// Look up a cached result. Returns `Some(result)` only when an
    /// entry exists *and* its header validates against the running
    /// binary's identity. Any I/O failure, format mismatch, or
    /// truncated body is treated as a miss; a one-line warning is
    /// emitted to stderr so the operator can investigate but the
    /// validator falls through to recomputation cleanly.
    pub fn lookup(&self, key: &DocCacheKey) -> Option<CachedResult> {
        match &self.inner {
            Backend::Disabled => {
                log_probe("miss", key, "cache disabled");
                None
            }
            Backend::Active { cache_root, root } => match read_entry(root, key) {
                Ok(Some(r)) => {
                    crate::cache::gc::record_entry_path_access(
                        cache_root,
                        crate::cache::gc::CacheFamily::Doc,
                        root,
                        &entry_path(root, key),
                    );
                    log_probe("hit", key, "");
                    Some(r)
                }
                Ok(None) => {
                    log_probe("miss", key, "no entry");
                    None
                }
                Err(reason) => {
                    log_probe("miss", key, &reason);
                    None
                }
            },
        }
    }

    /// Persist a result to the cache. On the disabled backend this
    /// is a no-op; on the active backend writes go through an
    /// same-directory atomic rename so concurrent rayon workers
    /// writing the same key cannot corrupt the entry. A platform
    /// that refuses replacement accepts only an exact validated
    /// peer entry and repairs an invalid one.
    ///
    /// Any I/O failure on the store path is reported via the
    /// hit/miss log gate (as `store-fail`) but does not error the
    /// caller — the next miss will retry the write.
    pub fn store(&self, key: &DocCacheKey, result: &CachedResult) {
        let Backend::Active { cache_root, root } = &self.inner else {
            return;
        };
        if let Err(reason) = write_entry(root, key, result) {
            log_probe("store-fail", key, &reason);
        } else {
            crate::cache::gc::record_entry_path_access(
                cache_root,
                crate::cache::gc::CacheFamily::Doc,
                root,
                &entry_path(root, key),
            );
        }
    }
}

/// The full cache key for one snippet validation. Built once per
/// snippet at lookup time; the on-disk file is named by the
/// lowercase-hex of this key's BLAKE3 digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DocCacheKey {
    hex: String,
}

impl DocCacheKey {
    /// Build a key from the inputs that determine what `kio check`
    /// would see. The cross-cutting compiler cache identity is folded
    /// into the hash so a toolchain change invalidates every entry on
    /// the next read.
    pub fn new(harness_body: &str, snippet_body: &str, marker_position: Option<usize>) -> Self {
        let mut h = Hasher::new();
        write_framed(&mut h, CACHE_NAMESPACE.as_bytes());
        write_framed(&mut h, IMPLEMENTATION_TAG.as_bytes());
        write_framed(&mut h, COMPILER_CACHE_ID.as_bytes());
        let marker_pos = marker_position.unwrap_or(usize::MAX);
        h.update(&(marker_pos as u64).to_le_bytes());
        write_framed(&mut h, harness_body.as_bytes());
        write_framed(&mut h, snippet_body.as_bytes());
        DocCacheKey {
            hex: h.finalize().to_hex().to_string(),
        }
    }

    /// The lowercase-hex digest; this is also the entry's filename
    /// stem under `<cache>/doc/`.
    pub fn hex(&self) -> &str {
        &self.hex
    }
}

/// The cached outcome of one `kio check` invocation. The validator
/// uses the exit code to drive its own match-or-mismatch
/// diagnostic; that diagnostic is re-rendered against the current
/// snippet body on a hit, so two warm hits against the same
/// snippet produce byte-identical user-facing output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedResult {
    /// The exit code `compile_workspace_at` returned (or `0` for a
    /// success). Stored as i32 so the on-disk format mirrors what
    /// `ExitCode::as_i32` produces.
    pub check_exit_code: i32,
}

// =========================================================================
// Header / file I/O
// =========================================================================

/// Length-prefixed framing for one input field. Mirrors the rlib
/// cache's key derivation: `(len_le_u64, bytes)` lets `Hasher::update`
/// fold any number of fields without ambiguity between, say,
/// `(a, bc)` and `(ab, c)`.
fn write_framed(h: &mut Hasher, bytes: &[u8]) {
    let len = bytes.len() as u64;
    h.update(&len.to_le_bytes());
    h.update(bytes);
}

/// Try to read an entry. Returns `Ok(Some(result))` on a header-
/// validated hit, `Ok(None)` if the file doesn't exist, and `Err`
/// with a short diagnostic on any other failure (I/O, malformed
/// header, truncated body). The caller treats every `Err` as a
/// miss; the message feeds the hit/miss log gate.
fn read_entry(root: &Path, key: &DocCacheKey) -> Result<Option<CachedResult>, String> {
    let path = entry_path(root, key);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", DisplayPath(&path))),
    };
    decode_entry(&text, key)
        .map(Some)
        .map_err(|reason| format!("stale {}: {reason}", DisplayPath(&path)))
}

/// Atomic-rename write. The body is rendered to a tempfile, closed,
/// then renamed into the final filename. The tempfile lives in the
/// same directory as the final entry so they share one filesystem.
fn write_entry(root: &Path, key: &DocCacheKey, result: &CachedResult) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|e| format!("mkdir {}: {e}", DisplayPath(&root)))?;
    let final_path = entry_path(root, key);
    let tmp_path = tempfile_path(root, key);
    let body = encode_entry(key, result);
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| format!("create {}: {e}", DisplayPath(&tmp_path)))?;
        f.write_all(body.as_bytes())
            .map_err(|e| format!("write {}: {e}", DisplayPath(&tmp_path)))?;
    }
    crate::cache::publish_temp_file(&tmp_path, &final_path, || {
        read_entry(root, key).ok().flatten().is_some()
    })
}

fn entry_path(root: &Path, key: &DocCacheKey) -> PathBuf {
    root.join(format!("{}.bin", key.hex))
}

fn tempfile_path(root: &Path, key: &DocCacheKey) -> PathBuf {
    let pid = std::process::id();
    let counter = TEMPFILE_COUNTER.fetch_add(1, Ordering::SeqCst);
    root.join(format!("{}.bin.tmp.{pid}-{counter}", key.hex))
}

/// Render a cache entry. The format is line-based and human-
/// readable so a developer can `cat` an entry while debugging; the
/// loader checks each header field before trusting the body.
fn encode_entry(key: &DocCacheKey, result: &CachedResult) -> String {
    let body = format!("exit={}\n", result.check_exit_code);
    format!(
        "KIO-DOC-CACHE namespace={namespace} impl={impl_tag} \
         compiler={version} features={features} compiler_cache_id={compiler_cache_id} \
         key={key} body={body_digest}\n{body}",
        namespace = CACHE_NAMESPACE,
        impl_tag = IMPLEMENTATION_TAG,
        version = COMPILER_VERSION,
        features = FEATURE_SET,
        compiler_cache_id = COMPILER_CACHE_ID,
        key = key.hex,
        body_digest = blake3::hash(body.as_bytes()).to_hex(),
    )
}

/// Parse a cache entry. Every header field is validated against
/// the running binary's identity before the body is touched; any
/// mismatch is a stale-entry diagnostic that the caller surfaces
/// via the hit/miss log gate and treats as a miss.
fn decode_entry(text: &str, expected: &DocCacheKey) -> Result<CachedResult, String> {
    let (header, body) = text
        .split_once('\n')
        .ok_or_else(|| "missing header newline".to_owned())?;
    let rest = header
        .strip_prefix("KIO-DOC-CACHE ")
        .ok_or_else(|| format!("missing magic: header `{header}`"))?;
    let mut namespace: Option<String> = None;
    let mut impl_tag: Option<String> = None;
    let mut compiler_cache_id: Option<String> = None;
    let mut stored_key: Option<String> = None;
    let mut body_digest: Option<String> = None;
    for tok in rest.split(' ') {
        if let Some(v) = tok.strip_prefix("namespace=") {
            namespace = Some(v.to_owned());
        } else if let Some(v) = tok.strip_prefix("impl=") {
            impl_tag = Some(v.to_owned());
        } else if let Some(v) = tok.strip_prefix("compiler_cache_id=") {
            compiler_cache_id = Some(v.to_owned());
        } else if let Some(v) = tok.strip_prefix("key=") {
            stored_key = Some(v.to_owned());
        } else if let Some(v) = tok.strip_prefix("body=") {
            body_digest = Some(v.to_owned());
        }
    }
    let namespace = namespace.ok_or_else(|| "header missing `namespace=`".to_owned())?;
    if namespace != CACHE_NAMESPACE {
        return Err(format!(
            "namespace mismatch: file `{namespace}`, running `{CACHE_NAMESPACE}`"
        ));
    }
    let impl_tag = impl_tag.ok_or_else(|| "header missing `impl=`".to_owned())?;
    if impl_tag != IMPLEMENTATION_TAG {
        return Err(format!(
            "implementation mismatch: file `{impl_tag}`, running `{IMPLEMENTATION_TAG}`"
        ));
    }
    let compiler_cache_id =
        compiler_cache_id.ok_or_else(|| "header missing `compiler_cache_id=`".to_owned())?;
    if compiler_cache_id != COMPILER_CACHE_ID {
        return Err(format!(
            "compiler-cache-id mismatch: file `{compiler_cache_id}`, running `{COMPILER_CACHE_ID}`"
        ));
    }
    let stored_key = stored_key.ok_or_else(|| "header missing `key=`".to_owned())?;
    if stored_key != expected.hex() {
        return Err(format!(
            "key mismatch: file `{stored_key}`, expected `{}`",
            expected.hex()
        ));
    }
    let body_digest = body_digest.ok_or_else(|| "header missing `body=`".to_owned())?;
    let expected_body_digest = blake3::hash(body.as_bytes()).to_hex().to_string();
    if body_digest != expected_body_digest {
        return Err(format!(
            "body mismatch: file `{body_digest}`, expected `{expected_body_digest}`"
        ));
    }
    // Body line: `exit=<i32>`.
    let body_line = body
        .lines()
        .next()
        .ok_or_else(|| "missing body line".to_owned())?;
    let exit_str = body_line
        .strip_prefix("exit=")
        .ok_or_else(|| format!("body line missing `exit=`: `{body_line}`"))?;
    let check_exit_code = exit_str
        .parse::<i32>()
        .map_err(|_| format!("`exit=` not an i32: `{exit_str}`"))?;
    Ok(CachedResult { check_exit_code })
}

/// On first write to a cache root, drop a `.gitignore` containing
/// `*\n!.gitignore` so the cache contents stay out of `git status`
/// regardless of where the user placed the directory. Mirrors the
/// same convention `kio-rs`'s other on-disk caches use.
fn write_gitignore_if_absent(cache_root: &Path) -> std::io::Result<()> {
    fs::create_dir_all(cache_root)?;
    let path = cache_root.join(".gitignore");
    if path.exists() {
        return Ok(());
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut f) => f.write_all(b"*\n!.gitignore\n"),
        // A racing process won the create-new; the existing file
        // is fine.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

// =========================================================================
// Package-file driven cache-directory resolution
// =========================================================================

/// Locate a `<name>.pkg.kio` at the workspace root and resolve
/// its `build { ... }` block's `cache` field. Returns `Some(abs_path)`
/// when caching is enabled (the block declared `cache "<path>";`),
/// or `None` otherwise — no package file, multiple package
/// files, unreadable file, unparseable file, no build block, or
/// `cache ();`. The driver maps a `None` to a disabled cache.
fn resolve_cache_dir(workspace_root: &Path) -> Option<PathBuf> {
    let package_path = find_package_file(workspace_root).ok()?;
    let source = fs::read_to_string(&package_path).ok()?;
    let stem = package_path
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(crate::file_kind::package_stem);
    let package = crate::pass::parser::parse_package_file(&source, stem).ok()?;
    match package.build?.cache {
        crate::ast::BuildBlockCache::Path { path, .. } => {
            let p = PathBuf::from(&path);
            Some(if p.is_absolute() {
                p
            } else {
                workspace_root.join(p)
            })
        }
        crate::ast::BuildBlockCache::Disabled { .. } => None,
    }
}

/// Find the package's `<name>.pkg.kio` at `dir`. Subdirectories
/// are not searched — the package file lives at the package root
/// per spec. Returns `Err(())` for "missing", "multiple", or I/O
/// failure; the caller treats all three as "no cache directory."
fn find_package_file(dir: &Path) -> Result<PathBuf, ()> {
    let mut hits = Vec::new();
    let read = fs::read_dir(dir).map_err(|_| ())?;
    for entry in read {
        let entry = entry.map_err(|_| ())?;
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if crate::file_kind::is_package_file(name) {
            hits.push(path);
        }
    }
    if hits.len() == 1 {
        Ok(hits.into_iter().next().unwrap())
    } else {
        Err(())
    }
}

// =========================================================================
// Hit / miss logging
// =========================================================================

/// Gate the per-snippet hit/miss line on `KIO_DEBUG_DOC_CACHE=1`.
fn log_probe(kind: &str, key: &DocCacheKey, reason: &str) {
    let on = std::env::var("KIO_DEBUG_DOC_CACHE")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);
    if !on {
        return;
    }
    if reason.is_empty() {
        eprintln!("kiodoc-cache: {kind} {}", key.hex);
    } else {
        eprintln!("kiodoc-cache: {kind} {} ({reason})", key.hex);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        tempfile::Builder::new()
            .prefix("kio-doc-cache-test-")
            .tempdir()
            .unwrap()
            .keep()
    }

    #[test]
    fn key_is_deterministic() {
        let k1 = DocCacheKey::new("h", "s", Some(3));
        let k2 = DocCacheKey::new("h", "s", Some(3));
        assert_eq!(k1, k2);
        assert_eq!(k1.hex().len(), 64);
    }

    #[test]
    fn key_changes_on_snippet_body() {
        let k1 = DocCacheKey::new("h", "s1", Some(3));
        let k2 = DocCacheKey::new("h", "s2", Some(3));
        assert_ne!(k1, k2);
    }

    #[test]
    fn key_changes_on_harness_body() {
        let k1 = DocCacheKey::new("h1", "s", Some(3));
        let k2 = DocCacheKey::new("h2", "s", Some(3));
        assert_ne!(k1, k2);
    }

    #[test]
    fn key_changes_on_marker_position() {
        let k1 = DocCacheKey::new("h", "s", Some(3));
        let k2 = DocCacheKey::new("h", "s", Some(4));
        let k3 = DocCacheKey::new("h", "s", None);
        assert_ne!(k1, k2);
        assert_ne!(k1, k3);
    }

    #[test]
    fn disabled_cache_always_misses() {
        let c = DocCache::disabled();
        assert!(!c.is_enabled());
        let k = DocCacheKey::new("h", "s", None);
        assert!(c.lookup(&k).is_none());
        // Store is a silent no-op; second lookup still misses.
        c.store(&k, &CachedResult { check_exit_code: 0 });
        assert!(c.lookup(&k).is_none());
    }

    #[test]
    fn active_cache_roundtrips() {
        let dir = tempdir();
        let c = DocCache::open(dir.clone()).unwrap();
        assert!(c.is_enabled());
        let k = DocCacheKey::new("h", "s", Some(5));
        assert!(c.lookup(&k).is_none());
        let r = CachedResult {
            check_exit_code: 14,
        };
        c.store(&k, &r);
        let got = c.lookup(&k).expect("must hit after store");
        assert_eq!(got, r);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_writes_gitignore() {
        let dir = tempdir();
        let _c = DocCache::open(dir.clone()).unwrap();
        let gitignore = dir.join(".gitignore");
        assert!(gitignore.is_file());
        let text = fs::read_to_string(&gitignore).unwrap();
        assert!(text.contains("*"));
        assert!(text.contains("!.gitignore"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_file_misses_without_panic() {
        let dir = tempdir();
        let c = DocCache::open(dir.clone()).unwrap();
        let k = DocCacheKey::new("h", "s", Some(5));
        // Write garbage at the entry's expected path.
        let doc_dir = dir.join("doc");
        let path = doc_dir.join(format!("{}.bin", k.hex()));
        fs::write(&path, "this is not a valid cache file\n").unwrap();
        // Loader must miss, not panic.
        assert!(c.lookup(&k).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncated_file_misses_without_panic() {
        let dir = tempdir();
        let c = DocCache::open(dir.clone()).unwrap();
        let k = DocCacheKey::new("h", "s", Some(5));
        let doc_dir = dir.join("doc");
        let path = doc_dir.join(format!("{}.bin", k.hex()));
        // Only the header, no body line.
        let header_only = format!(
            "KIO-DOC-CACHE namespace={CACHE_NAMESPACE} impl={IMPLEMENTATION_TAG} \
             compiler={COMPILER_VERSION} features={FEATURE_SET} compiler_cache_id={COMPILER_CACHE_ID} \
             key={k}\n",
            k = k.hex(),
        );
        fs::write(&path, header_only).unwrap();
        assert!(c.lookup(&k).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_compiler_cache_id_misses() {
        let dir = tempdir();
        let c = DocCache::open(dir.clone()).unwrap();
        let k = DocCacheKey::new("h", "s", Some(5));
        let doc_dir = dir.join("doc");
        let path = doc_dir.join(format!("{}.bin", k.hex()));
        let bad = format!(
            "KIO-DOC-CACHE namespace={CACHE_NAMESPACE} impl={IMPLEMENTATION_TAG} \
             compiler={COMPILER_VERSION} features={FEATURE_SET} compiler_cache_id=bad-cache-id \
             key={k}\nexit=0\n",
            k = k.hex(),
        );
        fs::write(&path, bad).unwrap();
        assert!(c.lookup(&k).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn exact_key_rejects_a_swapped_valid_entry() {
        let dir = tempdir();
        let c = DocCache::open(dir.clone()).unwrap();
        let first_key = DocCacheKey::new("h", "first", Some(5));
        let second_key = DocCacheKey::new("h", "second", Some(5));
        c.store(&first_key, &CachedResult { check_exit_code: 0 });
        c.store(&second_key, &CachedResult { check_exit_code: 7 });
        let root = dir.join("doc");
        let first = fs::read(entry_path(&root, &first_key)).unwrap();
        fs::write(entry_path(&root, &second_key), first).unwrap();

        assert!(c.lookup(&second_key).is_none());
        c.store(&second_key, &CachedResult { check_exit_code: 7 });
        assert_eq!(
            c.lookup(&second_key),
            Some(CachedResult { check_exit_code: 7 })
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn body_digest_rejects_a_different_valid_result() {
        let dir = tempdir();
        let c = DocCache::open(dir.clone()).unwrap();
        let key = DocCacheKey::new("h", "s", Some(5));
        c.store(&key, &CachedResult { check_exit_code: 0 });
        let path = entry_path(&dir.join("doc"), &key);
        let text = fs::read_to_string(&path).unwrap();
        let (header, _) = text.split_once('\n').unwrap();
        fs::write(&path, format!("{header}\nexit=7\n")).unwrap();

        assert!(c.lookup(&key).is_none());
        c.store(&key, &CachedResult { check_exit_code: 0 });
        assert_eq!(c.lookup(&key), Some(CachedResult { check_exit_code: 0 }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_from_workspace_no_package_file_disabled() {
        let dir = tempdir();
        let c = DocCache::resolve_from_workspace(&dir);
        assert!(!c.is_enabled());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_from_workspace_disabled_build_block() {
        let dir = tempdir();
        fs::write(
            dir.join("pkg.pkg.kio"),
            "package pkg;\n\nbuild {\n  cache ();\n}\n",
        )
        .unwrap();
        let c = DocCache::resolve_from_workspace(&dir);
        assert!(!c.is_enabled());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_from_workspace_no_build_block_disabled() {
        let dir = tempdir();
        fs::write(dir.join("pkg.pkg.kio"), "package pkg;\n").unwrap();
        let c = DocCache::resolve_from_workspace(&dir);
        assert!(!c.is_enabled());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_from_workspace_relative_path_resolved() {
        let dir = tempdir();
        fs::write(
            dir.join("pkg.pkg.kio"),
            "package pkg;\n\nbuild {\n  cache \"out/.kio-cache/\";\n}\n",
        )
        .unwrap();
        let c = DocCache::resolve_from_workspace(&dir);
        assert!(c.is_enabled());
        // Caching is rooted under <dir>/out/.kio-cache/doc/.
        let doc_dir = dir.join("out").join(".kio-cache").join("doc");
        assert!(
            doc_dir.is_dir(),
            "expected {} to exist",
            DisplayPath(&doc_dir)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_from_workspace_multiple_package_files_disabled() {
        let dir = tempdir();
        fs::write(dir.join("a.pkg.kio"), "package a;\n").unwrap();
        fs::write(dir.join("b.pkg.kio"), "package b;\n").unwrap();
        // Ambiguous: don't pick one; treat as no cache directory.
        let c = DocCache::resolve_from_workspace(&dir);
        assert!(!c.is_enabled());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_from_workspace_unparseable_package_file_disabled() {
        let dir = tempdir();
        fs::write(dir.join("pkg.pkg.kio"), "this is not a package file\n").unwrap();
        let c = DocCache::resolve_from_workspace(&dir);
        assert!(!c.is_enabled());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_write_overwrites_atomically() {
        let dir = tempdir();
        let c = DocCache::open(dir.clone()).unwrap();
        let k = DocCacheKey::new("h", "s", Some(5));
        c.store(&k, &CachedResult { check_exit_code: 0 });
        c.store(&k, &CachedResult { check_exit_code: 7 });
        let got = c.lookup(&k).unwrap();
        // The on-disk content is a deterministic function of (key,
        // result); both stores write identical bytes for the same
        // (key, result), and the second store with a different
        // result overwrites cleanly.
        assert_eq!(got.check_exit_code, 7);
        let _ = fs::remove_dir_all(&dir);
    }
}
