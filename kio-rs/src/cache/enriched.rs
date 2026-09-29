//! Per-module on-disk cache for `Module<Enriched>`.
//!
//! Sits between the structural-recovery + optimization passes
//! ([`crate::pass::structural_recovery::recover_module_with_package_inputs`] +
//! [`crate::pass::optimize::optimize_module_in_ctx`]) and per-backend
//! lowering. A module whose typed `Module<Prime>` shape hasn't
//! changed and whose module-scoped newtype-field / newtype-member
//! side-table entries are unchanged hits the cache; the cached bytes
//! deserialize to a `Module<Enriched>` byte-identical to a fresh
//! recovery + optimization sweep.
//!
//! ## Why between the typed-module and codegen-output caches
//!
//! Structural recovery and optimization are target-independent. The build
//! driver shares one result across selected backends within an invocation; this
//! cache extends that reuse across invocations by retaining each module's
//! `Enriched` result while its typed module and module-scoped side tables are
//! unchanged. The cache key isolates exactly the
//! inputs that affect the walk's output, so a target-only edit
//! (per-backend lowering changes) does not invalidate the
//! enriched-IR cache.
//!
//! ## Storage layout
//!
//! Mirrors the doc-cache conventions ([`crate::kiodoc::cache`]):
//!
//! ```text
//! <cache>/enriched-ir/
//!   <hex>.bin        — one cache entry per full key
//!   <hex>.bin.tmp.*  — in-flight writes; atomic-renamed into place
//! ```
//!
//! Flat layout (no per-module subdirectories): the BLAKE3 key is
//! globally unique, so colocating every entry in one directory
//! keeps the I/O surface minimal and `rm -rf <cache>/enriched-ir/`
//! still suffices as a manual nuke.
//!
//! ## Cache file format
//!
//! Each entry is a small binary file:
//!
//! - **Header.** A line-based ASCII header ending in `\n`,
//!   carrying the magic, cache namespace, implementation tag,
//!   compiler identity, readable compiler metadata, exact key, and body
//!   digest. A header mismatch (different namespace, different implementation,
//!   different compiler identity, etc.) is treated as a miss —
//!   the loader **never panics** on a stale or malformed file,
//!   only logs a one-line warning on the `KIO_DEBUG_ENRICHED_CACHE=1`
//!   probe channel and falls through to recomputation.
//! - **Body.** Postcard-encoded `Module<Enriched>` bytes. Postcard
//!   is the wire format: compact, deterministic, no_std-friendly,
//!   no external schema. Determinism is the load-bearing
//!   property — the cache's contract is "the cached value is
//!   byte-identical to a fresh computation," which postcard's
//!   stable encoding gives us for free given a stable AST shape.
//!
//! ## Cache key derivation
//!
//! The key is `BLAKE3` over a length-prefixed framing of:
//!
//! 1. Cache namespace (`"enriched-ir"`).
//! 2. Implementation tag (`"kio-rs"`).
//! 3. Compiler cache identity (`KIO_COMPILER_CACHE_ID`).
//! 4. The current module's serialized newtype-field side-table entries
//!    (`crate::pass::structural_recovery::Recoverer::ctx`'s
//!    `newtype_fields`).
//! 5. The current module's serialized optimizer side-table entries
//!    (`crate::pass::optimize::OptimizerCtx::newtype_members`).
//! 6. The postcard-encoded typed `Module<Prime>` bytes. The encoding
//!    is deterministic for a fixed AST, so a module body change moves
//!    the key without first rendering a large transient Kio' string.
//!
//! ## Atomicity and concurrency
//!
//! Writes go through a same-directory atomic rename so concurrent
//! rayon workers writing the same key cannot corrupt the entry. If
//! the platform refuses to replace an existing destination, the
//! writer accepts only an exact validated peer entry and repairs an
//! invalid one. Reads are
//! lock-free: a stale or truncated entry is detected on the
//! header / body validation and treated as a miss.
//!
//! ## Hit / miss instrumentation
//!
//! Setting `KIO_DEBUG_ENRICHED_CACHE=1` prints one line per module
//! to stderr: `enriched-cache: hit <key>` or
//! `enriched-cache: miss <key>`. Same convention as
//! [`crate::cache::package_check`] uses for its own probe lines.
//!
//! ## Disabled cache
//!
//! The cache's disabled variant ([`EnrichedCache::disabled`])
//! mirrors `cache ();` in the build block. Every lookup misses,
//! every store is a no-op. The recovery + optimization passes
//! still run on every invocation; nothing is written to disk.

// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(â¦)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
use crate::path_display::DisplayPath;
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use blake3::Hasher;

use crate::ast::{Enriched, Module, Prime};
use crate::cache::identity::{
    COMPILER_CACHE_ID, COMPILER_VERSION, FEATURE_SET, IMPLEMENTATION_TAG,
};

pub const CACHE_NAMESPACE: &str = "enriched-ir";

/// Per-process counter feeding the tempfile name so two concurrent
/// writes in the same process never collide on the same
/// `<pid>-<counter>` segment. Mirrors the doc-cache tempdir
/// naming.
static TEMPFILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Public handle on the per-module enriched-IR cache. Construct via
/// [`EnrichedCache::open`] (active cache rooted at a user-declared
/// path) or [`EnrichedCache::disabled`] (the `cache ();` form).
#[derive(Debug, Clone)]
pub struct EnrichedCache {
    inner: Backend,
    #[cfg(test)]
    recovered_module_hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Debug, Clone)]
enum Backend {
    /// Active cache: every miss writes, every hit returns the
    /// cached result. `root` is the `<cache>/enriched-ir/`
    /// directory.
    Active { cache_root: PathBuf, root: PathBuf },
    /// Caching opted out: every lookup misses, every store is a
    /// no-op. Mirrors the `cache ();` build-block form.
    Disabled,
}

impl EnrichedCache {
    /// Open an enriched-IR cache rooted under `cache_root` —
    /// typically the `<cache>` directory the build block's
    /// `cache "<path>";` declared. Materializes the
    /// enriched-IR subdirectory and writes the cache root's
    /// `.gitignore` if absent.
    pub fn open(cache_root: PathBuf) -> std::io::Result<Self> {
        let dir = cache_root.join("enriched-ir");
        fs::create_dir_all(&dir)?;
        write_gitignore_if_absent(&cache_root)?;
        crate::package_collection::mark_generated_dir(&cache_root);
        crate::cache::gc::record_cache_open(&cache_root, crate::cache::gc::CacheFamily::EnrichedIr);
        Ok(EnrichedCache {
            inner: Backend::Active {
                cache_root,
                root: dir,
            },
            #[cfg(test)]
            recovered_module_hits: Default::default(),
        })
    }

    /// Construct a no-op cache — mirrors `cache ();` in the build
    /// file. Every [`lookup`] misses and every [`store`] is a no-op.
    ///
    /// [`lookup`]: EnrichedCache::lookup
    /// [`store`]: EnrichedCache::store
    pub fn disabled() -> Self {
        EnrichedCache {
            inner: Backend::Disabled,
            #[cfg(test)]
            recovered_module_hits: Default::default(),
        }
    }

    /// True iff caching is enabled (the active variant). Exposed
    /// so callers can avoid the per-module key computation when
    /// the cache is disabled.
    pub fn is_enabled(&self) -> bool {
        matches!(self.inner, Backend::Active { .. })
    }

    /// Root directory of the active cache, or `None` for the
    /// disabled variant. Public so tests can inspect the on-disk
    /// layout.
    pub fn root(&self) -> Option<&Path> {
        match &self.inner {
            Backend::Active { root, .. } => Some(root),
            Backend::Disabled => None,
        }
    }

    /// Look up a cached `Module<Enriched>`. Returns `Some(module)`
    /// only when an entry exists *and* its header validates against
    /// the running binary's identity *and* its postcard body
    /// deserializes cleanly. Any I/O failure, header mismatch, or
    /// decode error is treated as a miss; a one-line warning is
    /// emitted via the [`KIO_DEBUG_ENRICHED_CACHE`] probe so the
    /// operator can investigate but the caller falls through to
    /// recomputation cleanly.
    pub fn lookup(&self, key: &EnrichedCacheKey) -> Option<Module<Enriched>> {
        match &self.inner {
            Backend::Disabled => {
                log_probe("miss", key, "cache disabled");
                None
            }
            Backend::Active { cache_root, root } => match read_entry(root, key) {
                Ok(Some(m)) => {
                    crate::cache::gc::record_entry_path_access(
                        cache_root,
                        crate::cache::gc::CacheFamily::EnrichedIr,
                        root,
                        &entry_path(root, key),
                    );
                    log_probe("hit", key, "");
                    Some(m)
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

    /// Persist a `Module<Enriched>` to the cache. On the disabled
    /// backend this is a no-op; on the active backend writes go
    /// through a same-directory atomic rename so concurrent rayon
    /// workers writing the same key cannot corrupt the entry. A
    /// platform that refuses replacement accepts only an exact
    /// validated peer entry and repairs an invalid one.
    ///
    /// Any I/O failure on the store path is reported via the
    /// hit/miss log gate (as `store-fail`) but does not error the
    /// caller — the next miss will retry the write.
    pub fn store(&self, key: &EnrichedCacheKey, module: &Module<Enriched>) {
        let Backend::Active { cache_root, root } = &self.inner else {
            return;
        };
        if let Err(reason) = write_entry(root, key, module) {
            log_probe("store-fail", key, &reason);
        } else {
            crate::cache::gc::record_entry_path_access(
                cache_root,
                crate::cache::gc::CacheFamily::EnrichedIr,
                root,
                &entry_path(root, key),
            );
        }
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ScopedNewtypeName(String, Vec<String>);

impl ScopedNewtypeName {
    pub fn new(module_path: impl Into<String>, visible_name: impl Into<String>) -> Self {
        Self(module_path.into(), vec![visible_name.into()])
    }

    pub fn from_head(module_path: impl Into<String>, visible_head: &[String]) -> Self {
        Self(module_path.into(), visible_head.to_vec())
    }

    pub fn module_path(&self) -> &str {
        &self.0
    }

    pub fn visible_head(&self) -> &[String] {
        &self.1
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct NewtypeIdentityKeyInput(String, String);

impl NewtypeIdentityKeyInput {
    pub fn new(module_path: impl Into<String>, declaration_name: impl Into<String>) -> Self {
        Self(module_path.into(), declaration_name.into())
    }

    pub fn module_path(&self) -> &str {
        &self.0
    }

    pub fn declaration_name(&self) -> &str {
        &self.1
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct NewtypeFfiKey(String);

impl NewtypeFfiKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct NewtypeConstructorName(String);

impl NewtypeConstructorName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct NewtypeProjectorName(String);

impl NewtypeProjectorName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct NewtypeFieldKeyInput(
    NewtypeIdentityKeyInput,
    NewtypeFfiKey,
    NewtypeConstructorName,
);

impl NewtypeFieldKeyInput {
    pub fn new(
        identity: NewtypeIdentityKeyInput,
        ffi_key: NewtypeFfiKey,
        constructor: NewtypeConstructorName,
    ) -> Self {
        Self(identity, ffi_key, constructor)
    }

    pub fn identity(&self) -> &NewtypeIdentityKeyInput {
        &self.0
    }

    pub fn ffi_key(&self) -> &NewtypeFfiKey {
        &self.1
    }

    pub fn constructor(&self) -> &NewtypeConstructorName {
        &self.2
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct NewtypeMemberKeyInput(
    NewtypeIdentityKeyInput,
    NewtypeConstructorName,
    NewtypeProjectorName,
);

impl NewtypeMemberKeyInput {
    pub fn new(
        identity: NewtypeIdentityKeyInput,
        constructor: NewtypeConstructorName,
        projector: NewtypeProjectorName,
    ) -> Self {
        Self(identity, constructor, projector)
    }

    pub fn identity(&self) -> &NewtypeIdentityKeyInput {
        &self.0
    }

    pub fn constructor(&self) -> &NewtypeConstructorName {
        &self.1
    }

    pub fn projector(&self) -> &NewtypeProjectorName {
        &self.2
    }
}

/// Per-package inputs that flow into the enriched-IR cache key
/// alongside each module's own typed AST bytes. These are the
/// side tables that structural recovery and the optimization catalog read
/// while walking a module's body. Keys include the consumer module so each
/// cache lookup can select exactly that module's semantic inputs.
///
/// Built once per package by [`PackageKeyInputs::from_package`]
/// and reused across the per-module fan-out.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PackageKeyInputs {
    /// Newtype field inputs indexed by (consumer module, visible head):
    /// exact nominal identity, FFI key, and constructor member name.
    /// Recovery joins these with `newtype_members` for the projector name.
    /// Sorted by key into a `BTreeMap` for deterministic key
    /// derivation across iteration-order changes upstream.
    pub newtype_fields: std::collections::BTreeMap<ScopedNewtypeName, NewtypeFieldKeyInput>,
    /// Mirror of `optimize::OptimizerCtx::newtype_members`:
    /// (consumer module, visible newtype head) -> (exact nominal identity,
    /// constructor name, projector name).
    /// Same deterministic-iteration rationale as
    /// [`newtype_fields`](Self::newtype_fields).
    pub newtype_members: std::collections::BTreeMap<ScopedNewtypeName, NewtypeMemberKeyInput>,
}

impl PackageKeyInputs {
    /// Construct from a typed `Package<Prime>`. Each module contributes only
    /// the exact literal newtypes reached by its local, selective-import, and
    /// written qualified-import member heads. Unrelated same-spelled
    /// declarations therefore neither enter that module's recovery context nor
    /// change its meaning.
    pub fn from_package(package: &crate::pass::resolve::Package<Prime>) -> Self {
        let mut newtype_fields: std::collections::BTreeMap<
            ScopedNewtypeName,
            NewtypeFieldKeyInput,
        > = std::collections::BTreeMap::new();
        let mut newtype_members: std::collections::BTreeMap<
            ScopedNewtypeName,
            NewtypeMemberKeyInput,
        > = std::collections::BTreeMap::new();
        for (module_path, entry) in package.modules() {
            crate::pass::resolve::for_each_resolved_newtype_member_head(
                package,
                &entry.module,
                |visible_head, owner, declaration| {
                    let owner_path = owner
                        .segments
                        .iter()
                        .map(crate::ast::PathSegment::as_str)
                        .collect::<Vec<_>>()
                        .join("/");
                    let identity =
                        NewtypeIdentityKeyInput::new(owner_path, declaration.name.clone());
                    let key = ScopedNewtypeName::from_head(module_path, visible_head);
                    newtype_fields.insert(
                        key.clone(),
                        NewtypeFieldKeyInput::new(
                            identity.clone(),
                            NewtypeFfiKey::new(declaration.ffi_key()),
                            NewtypeConstructorName::new(declaration.constructor.name.clone()),
                        ),
                    );
                    newtype_members.insert(
                        key,
                        NewtypeMemberKeyInput::new(
                            identity,
                            NewtypeConstructorName::new(declaration.constructor.name.clone()),
                            NewtypeProjectorName::new(declaration.projector.name.clone()),
                        ),
                    );
                },
            );
        }
        PackageKeyInputs {
            newtype_fields,
            newtype_members,
        }
    }

    fn scoped_to(&self, module_path: &str) -> Self {
        // `ScopedNewtypeName` orders by `(module_path, visible_head)`. NUL is
        // forbidden by the module-name grammar, so appending it provides the
        // exclusive successor bound for one exact module without scanning the
        // rest of the package catalogue.
        let start = ScopedNewtypeName(module_path.to_owned(), Vec::new());
        let end = ScopedNewtypeName(format!("{module_path}\0"), Vec::new());
        Self {
            newtype_fields: self
                .newtype_fields
                .range(start.clone()..end.clone())
                .map(|(name, input)| (name.clone(), input.clone()))
                .collect(),
            newtype_members: self
                .newtype_members
                .range(start..end)
                .map(|(name, input)| (name.clone(), input.clone()))
                .collect(),
        }
    }
}

/// Deterministic bytes for one typed `Module<Prime>`, ready to fold
/// into an enriched-cache key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnrichedModuleInput(Vec<u8>);

impl EnrichedModuleInput {
    pub fn from_prime_text(module_prime_text: impl Into<String>) -> Self {
        Self(module_prime_text.into().into_bytes())
    }

    pub fn from_prime_bytes(module_prime_bytes: impl Into<Vec<u8>>) -> Self {
        Self(module_prime_bytes.into())
    }

    fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// The full cache key for one module's enriched form. Built once
/// per module at the recovery + optimization call site; the
/// on-disk file is named by the lowercase-hex of this key's
/// BLAKE3 digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnrichedCacheKey {
    hex: String,
}

impl EnrichedCacheKey {
    /// Build a key from the inputs that determine what the
    /// recovery + optimization passes would produce. The
    /// cross-cutting cache identity is folded into the hash so a
    /// toolchain change invalidates every entry on the next read.
    ///
    /// `module` is the deterministic byte encoding of the typed
    /// `Module<Prime>`. Use [`EnrichedCacheKey::from_module`] to
    /// compute it from a `Module<Prime>` directly.
    pub fn new(module: &EnrichedModuleInput, package_inputs: &PackageKeyInputs) -> Self {
        let mut h = Hasher::new();
        write_framed(&mut h, CACHE_NAMESPACE.as_bytes());
        write_framed(&mut h, IMPLEMENTATION_TAG.as_bytes());
        write_framed(&mut h, COMPILER_CACHE_ID.as_bytes());
        // The caller's side tables fold in via postcard. Postcard encodes the
        // `BTreeMap` deterministically — same shape
        // produces the same bytes — so the side-table contribution
        // to the key is stable across runs.
        let field_bytes = postcard::to_allocvec(&package_inputs.newtype_fields)
            .expect("enriched-cache newtype-field key input postcard-encodes infallibly");
        write_framed(&mut h, &field_bytes);
        let opt_bytes = postcard::to_allocvec(&package_inputs.newtype_members)
            .expect("enriched-cache newtype-member key input postcard-encodes infallibly");
        write_framed(&mut h, &opt_bytes);
        write_framed(&mut h, module.as_bytes());
        EnrichedCacheKey {
            hex: h.finalize().to_hex().to_string(),
        }
    }

    /// Convenience: build a key from a typed `Module<Prime>` and a
    /// pre-computed [`PackageKeyInputs`]. Postcard-encodes the typed
    /// AST to derive the per-module fingerprint, then narrows the package
    /// catalogue and folds in only this module's side-table entries.
    pub fn from_module(module: &Module<Prime>, package_inputs: &PackageKeyInputs) -> Self {
        let bytes = postcard::to_allocvec(module)
            .expect("enriched-cache typed module key input postcard-encodes infallibly");
        let input = EnrichedModuleInput::from_prime_bytes(bytes);
        let module_path = module
            .path
            .segments
            .iter()
            .map(crate::ast::PathSegment::as_str)
            .collect::<Vec<_>>()
            .join("/");
        Self::new(&input, &package_inputs.scoped_to(&module_path))
    }

    /// The lowercase-hex digest; this is also the entry's filename
    /// stem under `<cache>/enriched-ir/`.
    pub fn hex(&self) -> &str {
        &self.hex
    }
}

// =========================================================================
// Header / file I/O
// =========================================================================

/// Length-prefixed framing for one input field. Mirrors the rlib
/// cache's key derivation: `(len_le_u64, bytes)` lets
/// `Hasher::update` fold any number of fields without ambiguity
/// between, say, `(a, bc)` and `(ab, c)`.
fn write_framed(h: &mut Hasher, bytes: &[u8]) {
    let len = bytes.len() as u64;
    h.update(&len.to_le_bytes());
    h.update(bytes);
}

/// Try to read an entry. Returns `Ok(Some(module))` on a header-
/// validated hit, `Ok(None)` if the file doesn't exist, and `Err`
/// with a short diagnostic on any other failure (I/O, malformed
/// header, postcard decode error). The caller treats every `Err`
/// as a miss; the message feeds the hit/miss log gate.
fn read_entry(root: &Path, key: &EnrichedCacheKey) -> Result<Option<Module<Enriched>>, String> {
    let path = entry_path(root, key);
    let mut file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("open {}: {e}", DisplayPath(&path))),
    };
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| format!("read {}: {e}", DisplayPath(&path)))?;
    let (header_end, header_fields) =
        parse_header(&buf).map_err(|reason| format!("stale {}: {reason}", DisplayPath(&path)))?;
    let body = &buf[header_end..];
    validate_header(&header_fields, key, body)
        .map_err(|reason| format!("stale {}: {reason}", DisplayPath(&path)))?;
    let module: Module<Enriched> = postcard::from_bytes(body)
        .map_err(|e| format!("stale {}: postcard decode failed: {e}", DisplayPath(&path)))?;
    Ok(Some(module))
}

/// Atomic-rename write. The body is rendered to a tempfile, closed,
/// then renamed into the final filename. The tempfile lives in the
/// same directory as the final entry so they share one filesystem.
fn write_entry(
    root: &Path,
    key: &EnrichedCacheKey,
    module: &Module<Enriched>,
) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|e| format!("mkdir {}: {e}", DisplayPath(&root)))?;
    let final_path = entry_path(root, key);
    let tmp_path = tempfile_path(root, key);
    let body = postcard::to_allocvec(module).map_err(|e| format!("postcard encode failed: {e}"))?;
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| format!("create {}: {e}", DisplayPath(&tmp_path)))?;
        let header = encode_header(key, &body);
        f.write_all(header.as_bytes())
            .map_err(|e| format!("write {}: {e}", DisplayPath(&tmp_path)))?;
        f.write_all(&body)
            .map_err(|e| format!("write {}: {e}", DisplayPath(&tmp_path)))?;
    }
    crate::cache::publish_temp_file(&tmp_path, &final_path, || {
        read_entry(root, key).ok().flatten().is_some()
    })
}

fn entry_path(root: &Path, key: &EnrichedCacheKey) -> PathBuf {
    root.join(format!("{}.bin", key.hex))
}

fn tempfile_path(root: &Path, key: &EnrichedCacheKey) -> PathBuf {
    let pid = std::process::id();
    let counter = TEMPFILE_COUNTER.fetch_add(1, Ordering::SeqCst);
    root.join(format!("{}.bin.tmp.{pid}-{counter}", key.hex))
}

/// Render the ASCII header line. Format:
///
/// `KIO-ENRICHED-CACHE namespace=<name> impl=<tag>
///  compiler=<ver> features=<set> compiler_cache_id=<hex> key=<hex>
///  body=<hex>\n`
///
/// All fields appear in one line, separated by single spaces. The
/// loader splits on space, picks out each `<k>=<v>` pair, validates
/// against the running binary's identity, then treats the bytes
/// after the trailing newline as the postcard body.
fn encode_header(key: &EnrichedCacheKey, body: &[u8]) -> String {
    format!(
        "KIO-ENRICHED-CACHE namespace={namespace} impl={impl_tag} \
         compiler={compiler} features={features} compiler_cache_id={compiler_cache_id} \
         key={key} body={body_digest}\n",
        namespace = CACHE_NAMESPACE,
        impl_tag = IMPLEMENTATION_TAG,
        compiler = COMPILER_VERSION,
        features = FEATURE_SET,
        compiler_cache_id = COMPILER_CACHE_ID,
        key = key.hex,
        body_digest = blake3::hash(body).to_hex(),
    )
}

/// Parse the header line. Returns `(byte_offset_of_body_start,
/// header_fields)`. The header is everything up to and including
/// the first `\n`; the body begins at the byte after.
fn parse_header(buf: &[u8]) -> Result<(usize, HashMap<String, String>), String> {
    let newline_idx = buf
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| "missing header newline".to_owned())?;
    let header_bytes = &buf[..newline_idx];
    let header_str =
        std::str::from_utf8(header_bytes).map_err(|_| "header is not valid UTF-8".to_owned())?;
    let rest = header_str
        .strip_prefix("KIO-ENRICHED-CACHE ")
        .ok_or_else(|| format!("missing magic: header `{header_str}`"))?;
    let mut fields: HashMap<String, String> = HashMap::new();
    for tok in rest.split(' ') {
        if let Some((k, v)) = tok.split_once('=') {
            fields.insert(k.to_owned(), v.to_owned());
        }
    }
    Ok((newline_idx + 1, fields))
}

/// Validate parsed header fields against the running binary's
/// identity. Any mismatch is a stale-entry error.
fn validate_header(
    fields: &HashMap<String, String>,
    expected: &EnrichedCacheKey,
    body: &[u8],
) -> Result<(), String> {
    let namespace = fields
        .get("namespace")
        .ok_or_else(|| "header missing `namespace=`".to_owned())?;
    if namespace != CACHE_NAMESPACE {
        return Err(format!(
            "namespace mismatch: file `{namespace}`, running `{CACHE_NAMESPACE}`",
        ));
    }
    let impl_tag = fields
        .get("impl")
        .ok_or_else(|| "header missing `impl=`".to_owned())?;
    if impl_tag != IMPLEMENTATION_TAG {
        return Err(format!(
            "implementation mismatch: file `{impl_tag}`, running `{IMPLEMENTATION_TAG}`"
        ));
    }
    let compiler_cache_id = fields
        .get("compiler_cache_id")
        .ok_or_else(|| "header missing `compiler_cache_id=`".to_owned())?;
    if compiler_cache_id != COMPILER_CACHE_ID {
        return Err(format!(
            "compiler-cache-id mismatch: file `{compiler_cache_id}`, running `{COMPILER_CACHE_ID}`"
        ));
    }
    let key = fields
        .get("key")
        .ok_or_else(|| "header missing `key=`".to_owned())?;
    if key != expected.hex() {
        return Err(format!(
            "key mismatch: file `{key}`, expected `{}`",
            expected.hex()
        ));
    }
    let body_digest = fields
        .get("body")
        .ok_or_else(|| "header missing `body=`".to_owned())?;
    let expected_body_digest = blake3::hash(body).to_hex().to_string();
    if body_digest != &expected_body_digest {
        return Err(format!(
            "body mismatch: file `{body_digest}`, expected `{expected_body_digest}`"
        ));
    }
    Ok(())
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
// Hit / miss logging
// =========================================================================

/// Gate the per-module hit/miss line on `KIO_DEBUG_ENRICHED_CACHE=1`.
fn log_probe(kind: &str, key: &EnrichedCacheKey, reason: &str) {
    if !probe_enabled() {
        return;
    }
    if reason.is_empty() {
        eprintln!("enriched-cache: {kind} {}", key.hex);
    } else {
        eprintln!("enriched-cache: {kind} {} ({reason})", key.hex);
    }
}

fn probe_enabled() -> bool {
    std::env::var("KIO_DEBUG_ENRICHED_CACHE")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

// =========================================================================
// Resolve from a build block
// =========================================================================

/// Resolve an [`EnrichedCache`] from the package's build-block
/// `cache` field. The build block's `cache "<path>";` value
/// drives whether the cache is active and where it lives:
///
/// - `cache "<path>";` enables every on-disk cache `kio` ships,
///   sub-namespaced under that one directory; the enriched-IR
///   cache lives at `<path>/enriched-ir/`.
/// - `cache ();` disables every on-disk cache; this function
///   returns the [`disabled`](EnrichedCache::disabled) variant.
///
/// Relative paths in the build block resolve against
/// `workspace_root` (the package's directory); absolute paths
/// pass through unchanged.
///
/// I/O failures on the cache directory (e.g. permission denied)
/// degrade to a disabled cache: the recovery + optimization
/// passes still run, just without write-side persistence. The
/// failure is silent at this level — the per-build cache-resolve
/// step is best-effort, not a build-failing event.
pub fn resolve_from_cache_field(
    workspace_root: &Path,
    cache: &crate::ast::BuildBlockCache,
) -> EnrichedCache {
    match cache {
        crate::ast::BuildBlockCache::Path { path, .. } => {
            let abs = absolutize(workspace_root, path);
            match EnrichedCache::open(abs) {
                Ok(c) => c,
                Err(_) => EnrichedCache::disabled(),
            }
        }
        crate::ast::BuildBlockCache::Disabled { .. } => EnrichedCache::disabled(),
    }
}

/// Resolve a possibly-relative `path` (from the build block's
/// `cache` field) against `workspace_root`. Absolute paths pass
/// through unchanged.
fn absolutize(workspace_root: &Path, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        workspace_root.join(p)
    }
}

// =========================================================================
// Cached recover + optimize
// =========================================================================

/// Run structural recovery + the optimization catalog over `package`,
/// consulting `cache` to skip per-module work when the cached
/// `Module<Enriched>` is fresh.
///
/// The per-module driver:
///
/// 1. Computes the cache key from the typed `Module<Prime>` and the
///    module-scoped side-table entries.
/// 2. On a hit, deserializes the cached `Module<Enriched>` and
///    drops it into the package's module entry — recovery and
///    every optimization pass are skipped.
/// 3. On a miss, runs
///    [`crate::pass::structural_recovery::recover_module_with_package_inputs`]
///    and [`crate::pass::optimize::optimize_module_with_package_inputs`]
///    in sequence on the typed module body, stores the result via
///    [`EnrichedCache::store`], and drops it into the package
///    entry.
///
/// Package-file bodies are recovered + optimized unconditionally —
/// they are not module bodies, and the cache contract is scoped to
/// `Module<Enriched>`. The per-module fan-out continues to use
/// rayon's `par_iter`; per-key cache lookups are independent across
/// modules so no synchronization is needed beyond the per-entry
/// atomic-rename publish.
///
/// On the [`EnrichedCache::disabled`] variant this function runs the
/// same per-module recovery + optimization work directly and skips
/// key construction entirely. On the active variant the observable
/// output is byte-identical; only the timing changes.
pub fn recover_and_optimize_package_cached(
    package: &crate::pass::resolve::Package<Prime>,
    cache: &EnrichedCache,
) -> crate::pass::resolve::Package<Enriched> {
    // Side tables: built once per package, immutable for the
    // duration of the fan-out. Both `structural_recovery` and
    // `optimize` build their own internal `Ctx` from the package;
    // `PackageKeyInputs` mirrors both, so the cache key reflects
    // exactly the inputs the un-cached path reads.
    let key_inputs = PackageKeyInputs::from_package(package);

    if !cache.is_enabled() {
        return recover_and_optimize_package_without_cache(package, &key_inputs);
    }

    // Per-module driver: hit → load cached `Module<Enriched>`;
    // miss → recover + optimize, store, return.
    let entries: Vec<(String, crate::pass::resolve::ModuleEntry<Enriched>)> =
        crate::maybe_into_par_iter!(package.modules().collect::<Vec<_>>())
            .map(|(path, entry)| {
                let key = EnrichedCacheKey::from_module(&entry.module, &key_inputs);
                let module = match cache.lookup(&key) {
                    Some(m) => {
                        #[cfg(test)]
                        cache.recovered_module_hits.fetch_add(1, Ordering::Relaxed);
                        m
                    }
                    None => {
                        // Run the same per-module work the un-cached path
                        // would, selecting the consumer module's exact
                        // side-table entries for both passes. This preserves
                        // the cache's "byte-identical to fresh computation"
                        // contract without admitting unrelated same-spelled
                        // declarations.
                        let timing = probe_enabled().then(Instant::now);
                        let recovered_start = probe_enabled().then(Instant::now);
                        let recovered =
                            crate::pass::structural_recovery::recover_module_with_package_inputs(
                                &entry.module,
                                &key_inputs,
                            );
                        let recovered_duration = recovered_start.map(|start| start.elapsed());
                        let optimize_start = probe_enabled().then(Instant::now);
                        let optimized = crate::pass::optimize::optimize_module_with_package_inputs(
                            recovered,
                            &key_inputs,
                        );
                        let optimize_duration = optimize_start.map(|start| start.elapsed());
                        cache.store(&key, &optimized);
                        if let Some(total_start) = timing {
                            eprintln!(
                                "enriched-cache: compute {} recover_ms={:.3} optimize_ms={:.3} total_ms={:.3}",
                                path,
                                recovered_duration
                                    .map(|duration| duration.as_secs_f64() * 1000.0)
                                    .unwrap_or(0.0),
                                optimize_duration
                                    .map(|duration| duration.as_secs_f64() * 1000.0)
                                    .unwrap_or(0.0),
                                total_start.elapsed().as_secs_f64() * 1000.0
                            );
                        }
                        optimized
                    }
                };
                (
                    (*path).to_owned(),
                    crate::pass::resolve::ModuleEntry::<Enriched> {
                        file_path: entry.file_path.clone(),
                        module,
                        scope: entry.scope.clone(),
                    },
                )
            })
            .collect();
    let modules: std::collections::BTreeMap<String, crate::pass::resolve::ModuleEntry<Enriched>> =
        entries.into_iter().collect();

    // Package-file bodies are not in the cache's scope; reuse the
    // un-cached path for them. Build a package from just the module
    // bodies first, then attach the recovered + optimized package file
    // via the same `recover_package` + `optimize_package` pair would.
    let package_file_entry =
        package
            .package_file()
            .map(|entry| crate::pass::resolve::PackageFileEntry::<Enriched> {
                file_path: entry.file_path.clone(),
                package_name: entry.package_name.clone(),
                package_file: {
                    // Package-file body is not in the cache's scope —
                    // it's run uncached every time. The single-shot
                    // helpers in `structural_recovery` and `optimize`
                    // build their own per-package-file `OptimizerCtx` /
                    // `RecoveryCtx`, mirroring the un-cached package
                    // path's behavior for the package-file slice.
                    let recovered_package_file =
                        crate::pass::structural_recovery::recover_package_file(&entry.package_file);
                    crate::pass::optimize::optimize_package_file(recovered_package_file)
                },
            });

    crate::pass::resolve::Package::<Enriched>::from_parts(modules, package_file_entry)
}

fn recover_and_optimize_package_without_cache(
    package: &crate::pass::resolve::Package<Prime>,
    key_inputs: &PackageKeyInputs,
) -> crate::pass::resolve::Package<Enriched> {
    let entries: Vec<(String, crate::pass::resolve::ModuleEntry<Enriched>)> =
        crate::maybe_into_par_iter!(package.modules().collect::<Vec<_>>())
            .map(|(path, entry)| {
                let recovered =
                    crate::pass::structural_recovery::recover_module_with_package_inputs(
                        &entry.module,
                        key_inputs,
                    );
                let optimized = crate::pass::optimize::optimize_module_with_package_inputs(
                    recovered, key_inputs,
                );
                (
                    (*path).to_owned(),
                    crate::pass::resolve::ModuleEntry::<Enriched> {
                        file_path: entry.file_path.clone(),
                        module: optimized,
                        scope: entry.scope.clone(),
                    },
                )
            })
            .collect();
    let modules: std::collections::BTreeMap<String, crate::pass::resolve::ModuleEntry<Enriched>> =
        entries.into_iter().collect();
    let package_file_entry =
        package
            .package_file()
            .map(|entry| crate::pass::resolve::PackageFileEntry::<Enriched> {
                file_path: entry.file_path.clone(),
                package_name: entry.package_name.clone(),
                package_file: {
                    let recovered_package_file =
                        crate::pass::structural_recovery::recover_package_file(&entry.package_file);
                    crate::pass::optimize::optimize_package_file(recovered_package_file)
                },
            });
    crate::pass::resolve::Package::<Enriched>::from_parts(modules, package_file_entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(feature = "surface", feature = "prime"))]
    use crate::pipeline::Pipeline;

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!(
            "kio-enriched-cache-test-{nonce}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn empty_inputs() -> PackageKeyInputs {
        PackageKeyInputs {
            newtype_fields: std::collections::BTreeMap::new(),
            newtype_members: std::collections::BTreeMap::new(),
        }
    }

    fn key(module_prime_text: &str, inputs: &PackageKeyInputs) -> EnrichedCacheKey {
        EnrichedCacheKey::new(
            &EnrichedModuleInput::from_prime_text(module_prime_text),
            inputs,
        )
    }

    fn module_named(name: &str) -> Module<Enriched> {
        let end = u32::try_from(name.len()).unwrap();
        Module::<Enriched> {
            path: crate::ast::ModulePath {
                segments: vec![crate::ast::PathSegment::new(
                    name,
                    crate::span::Span::new(0, end),
                )],
                span: crate::span::Span::new(0, end),
            },
            imports: Vec::new(),
            items: Vec::new(),
            meta: crate::ast::Meta::new(crate::span::Span::new(0, end)),
            doc: None,
        }
    }

    #[cfg(all(feature = "surface", feature = "prime"))]
    fn prime_package(source: &str) -> crate::pass::resolve::Package<Prime> {
        prime_package_many(&[("main.kio", source)])
    }

    #[cfg(all(feature = "surface", feature = "prime"))]
    fn prime_package_many(sources: &[(&str, &str)]) -> crate::pass::resolve::Package<Prime> {
        let parsed = sources
            .iter()
            .map(|(path, source)| {
                (
                    PathBuf::from(path),
                    crate::pass::parser::parse(source).expect("parse cache-key fixture"),
                )
            })
            .collect();
        let (modules, _) = crate::pass::full::FullPipeline::lower_package(parsed, None)
            .expect("lower cache-key fixture");
        let package = crate::pass::resolve::Package::build(Path::new(""), modules, None)
            .expect("build cache-key fixture");
        package
            .resolve_imports()
            .expect("resolve cache-key fixture");
        package
            .check_in_body_resolution()
            .expect("resolve cache-key fixture bodies");
        crate::pass::typecheck_full::check_package(&package).expect("typecheck cache-key fixture")
    }

    fn nt(module_path: &str, visible_name: &str) -> ScopedNewtypeName {
        ScopedNewtypeName::new(module_path, visible_name)
    }

    fn ntq(module_path: &str, alias: &str, visible_name: &str) -> ScopedNewtypeName {
        ScopedNewtypeName::from_head(module_path, &[alias.to_owned(), visible_name.to_owned()])
    }

    fn field_input(ffi_key: &str, constructor: &str) -> NewtypeFieldKeyInput {
        NewtypeFieldKeyInput::new(
            NewtypeIdentityKeyInput::new("owner", ffi_key),
            NewtypeFfiKey::new(ffi_key),
            NewtypeConstructorName::new(constructor),
        )
    }

    fn member(constructor: &str, projector: &str) -> NewtypeMemberKeyInput {
        NewtypeMemberKeyInput::new(
            NewtypeIdentityKeyInput::new("owner", "Newtype"),
            NewtypeConstructorName::new(constructor),
            NewtypeProjectorName::new(projector),
        )
    }

    fn member_for(
        owner: &str,
        declaration: &str,
        constructor: &str,
        projector: &str,
    ) -> NewtypeMemberKeyInput {
        NewtypeMemberKeyInput::new(
            NewtypeIdentityKeyInput::new(owner, declaration),
            NewtypeConstructorName::new(constructor),
            NewtypeProjectorName::new(projector),
        )
    }

    #[test]
    fn key_is_deterministic() {
        let inputs = empty_inputs();
        let k1 = key("module a;", &inputs);
        let k2 = key("module a;", &inputs);
        assert_eq!(k1, k2);
        assert_eq!(k1.hex().len(), 64);
    }

    #[test]
    fn key_changes_on_module_text() {
        let inputs = empty_inputs();
        let k1 = key("module a;", &inputs);
        let k2 = key("module b;", &inputs);
        assert_ne!(k1, k2);
    }

    #[test]
    fn key_changes_on_newtype_fields() {
        let k_empty = key("module a;", &empty_inputs());
        let mut populated = empty_inputs();
        populated
            .newtype_fields
            .insert(nt("a", "Foo"), field_input("Foo", "mk"));
        let k_populated = key("module a;", &populated);
        assert_ne!(k_empty, k_populated);
    }

    #[test]
    fn key_changes_on_newtype_members() {
        let k_empty = key("module a;", &empty_inputs());
        let mut populated = empty_inputs();
        populated
            .newtype_members
            .insert(nt("a", "Bar"), member("mk_bar", "un_bar"));
        let k_populated = key("module a;", &populated);
        assert_ne!(k_empty, k_populated);
    }

    #[test]
    fn key_changes_on_exact_newtype_provider_with_same_member_spellings() {
        let mut provider = empty_inputs();
        provider.newtype_members.insert(
            ntq("main", "p", "Box"),
            member_for("provider", "Box", "wrap", "unwrap"),
        );
        let mut decoy = empty_inputs();
        decoy.newtype_members.insert(
            ntq("main", "p", "Box"),
            member_for("decoy", "Box", "wrap", "unwrap"),
        );
        assert_ne!(key("module main;", &provider), key("module main;", &decoy));
    }

    #[test]
    #[cfg(all(feature = "surface", feature = "prime"))]
    fn typed_module_key_ignores_other_consumers_side_tables() {
        let package = prime_package("module main;");
        let module = &package.module("main").expect("main module").module;
        let base = empty_inputs();
        let base_key = EnrichedCacheKey::from_module(module, &base);

        let mut inputs = base.clone();
        inputs
            .newtype_members
            .insert(nt("other", "Foo"), member("make", "un"));
        assert_eq!(base_key, EnrichedCacheKey::from_module(module, &inputs));

        inputs
            .newtype_members
            .insert(nt("main", "Foo"), member("make", "un"));
        assert_ne!(base_key, EnrichedCacheKey::from_module(module, &inputs));
    }

    #[test]
    #[cfg(all(feature = "surface", feature = "prime"))]
    fn package_inputs_include_recursive_group_newtype_members() {
        let package = prime_package(
            "module main; rec { \
               newtype Left : Right { constructor make_left; projector read_left; }; \
               newtype Right : Left { constructor make_right; projector read_right; }; \
             }",
        );
        let inputs = PackageKeyInputs::from_package(&package);
        assert_eq!(
            inputs.newtype_members.keys().collect::<Vec<_>>(),
            vec![&nt("main", "Left"), &nt("main", "Right")]
        );
        assert_eq!(
            inputs.newtype_fields.keys().collect::<Vec<_>>(),
            vec![&nt("main", "Left"), &nt("main", "Right")]
        );
    }

    #[test]
    #[cfg(all(feature = "surface", feature = "prime"))]
    fn package_inputs_keep_same_named_newtypes_scoped_per_consumer() {
        let package = prime_package_many(&[
            (
                "a.kio",
                "module a; newtype Foo : . { constructor make_a; projector un_a; };",
            ),
            (
                "b.kio",
                "module b; newtype Foo : . { constructor make_b; projector un_b; };",
            ),
        ]);
        let inputs = PackageKeyInputs::from_package(&package);

        assert_eq!(
            inputs.newtype_members.get(&nt("a", "Foo")),
            Some(&member_for("a", "Foo", "make_a", "un_a"))
        );
        assert_eq!(
            inputs.newtype_members.get(&nt("b", "Foo")),
            Some(&member_for("b", "Foo", "make_b", "un_b"))
        );
    }

    #[test]
    #[cfg(all(feature = "surface", feature = "prime"))]
    fn package_inputs_retain_qualified_head_and_exact_provider() {
        let package = prime_package_many(&[
            (
                "provider.kio",
                "module provider; \
                 pub newtype Box : . { pub constructor wrap; pub projector unwrap; };",
            ),
            (
                "decoy.kio",
                "module decoy; \
                 pub newtype Box : . { pub constructor wrap; pub projector unwrap; };",
            ),
            ("consumer.kio", "module consumer; import provider as p;"),
            ("unrelated.kio", "module unrelated;"),
        ]);
        let inputs = PackageKeyInputs::from_package(&package);

        assert_eq!(
            inputs.newtype_members.get(&ntq("consumer", "p", "Box")),
            Some(&member_for("provider", "Box", "wrap", "unwrap"))
        );
        assert!(
            !inputs
                .newtype_members
                .keys()
                .any(|key| key.module_path() == "unrelated" && key.visible_head().len() == 2),
            "a consumer with no written qualified edge receives no package-wide heads"
        );
    }

    #[test]
    #[cfg(all(feature = "surface", feature = "prime"))]
    #[should_panic(expected = "enriched package inputs select one exact nominal per lexical head")]
    fn recovery_rejects_inconsistent_nominal_inputs() {
        let package = prime_package_many(&[(
            "main.kio",
            "module main; newtype Box : . { constructor wrap; projector unwrap; };",
        )]);
        let mut inputs = PackageKeyInputs::from_package(&package);
        inputs.newtype_members.insert(
            nt("main", "Box"),
            member_for("other", "Box", "wrap", "unwrap"),
        );
        crate::pass::structural_recovery::recover_module_with_package_inputs(
            &package.module("main").expect("main module").module,
            &inputs,
        );
    }

    #[test]
    #[cfg(all(feature = "surface", feature = "prime"))]
    fn recursive_group_newtypes_are_identical_across_direct_and_cached_enrichment() {
        let package = prime_package(
            "module main; import __intrinsics__; \
             rec { \
               labels Node = { head: ., next: Link }; \
               newtype Link : Node { \
                 pub constructor mk_link; pub projector un_link; \
               }; \
             } \
             fn read(r: (Head & Next)) -> Link { Next.get(__snd__(r)) } \
             fn round(x: Node) -> Node { Link.un_link(Link.mk_link(x)) }",
        );

        let direct = crate::pass::optimize::optimize_package(
            crate::pass::structural_recovery::recover_package(&package),
        );
        let disabled = recover_and_optimize_package_cached(&package, &EnrichedCache::disabled());

        let dir = tempdir();
        let cache = EnrichedCache::open(dir.clone()).expect("open cache");
        let miss = recover_and_optimize_package_cached(&package, &cache);
        assert_eq!(cache.recovered_module_hits.load(Ordering::Relaxed), 0);
        let hit = recover_and_optimize_package_cached(&package, &cache);
        assert_eq!(
            cache.recovered_module_hits.load(Ordering::Relaxed),
            package.modules().count(),
            "every warm module must return from the production cache-hit branch"
        );

        let direct_module = &direct.module("main").expect("direct module").module;
        assert_eq!(
            direct_module,
            &disabled.module("main").expect("disabled module").module,
            "disabled-cache enrichment must match the direct package passes"
        );
        assert_eq!(
            direct_module,
            &miss.module("main").expect("cache miss module").module,
            "active-cache miss must match the direct package passes"
        );
        assert_eq!(
            direct_module,
            &hit.module("main").expect("cache hit module").module,
            "active-cache hit must deserialize the same enriched AST"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    #[cfg(all(feature = "surface", feature = "prime"))]
    fn qualified_newtype_recovery_is_identical_across_cache_paths() {
        let package = prime_package_many(&[
            (
                "provider.kio",
                "module provider; \
                 pub newtype Hed : . { pub constructor put; pub projector get; }; \
                 pub newtype Tal : . { pub constructor put_tal; pub projector get_tal; }; \
                 pub newtype Box : . { pub constructor wrap; pub projector unwrap; };",
            ),
            (
                "consumer.kio",
                "module consumer; import __intrinsics__; import provider as p; \
                 fn read(r: (p.Hed & p.Tal)) -> . { p.Hed.get(__fst__(r)) } \
                 fn round(value: .) -> . { p.Box.unwrap(p.Box.wrap(value)) }",
            ),
        ]);

        let direct = crate::pass::optimize::optimize_package(
            crate::pass::structural_recovery::recover_package(&package),
        );
        let disabled = recover_and_optimize_package_cached(&package, &EnrichedCache::disabled());
        let dir = tempdir();
        let cache = EnrichedCache::open(dir.clone()).expect("open cache");
        let miss = recover_and_optimize_package_cached(&package, &cache);
        assert_eq!(cache.recovered_module_hits.load(Ordering::Relaxed), 0);
        let hit = recover_and_optimize_package_cached(&package, &cache);
        assert_eq!(
            cache.recovered_module_hits.load(Ordering::Relaxed),
            package.modules().count(),
            "every qualified warm module must return from the production cache-hit branch"
        );
        let direct_module = &direct.module("consumer").expect("direct consumer").module;

        for (route, observed) in [
            (
                "disabled",
                &disabled
                    .module("consumer")
                    .expect("disabled consumer")
                    .module,
            ),
            (
                "miss",
                &miss.module("consumer").expect("miss consumer").module,
            ),
            ("hit", &hit.module("consumer").expect("hit consumer").module),
        ] {
            assert_eq!(
                direct_module, observed,
                "qualified enriched IR differs on the {route} cache route"
            );
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn disabled_cache_always_misses() {
        let c = EnrichedCache::disabled();
        assert!(!c.is_enabled());
        let k = key("module a;", &empty_inputs());
        assert!(c.lookup(&k).is_none());
        let m = Module::<Enriched> {
            path: crate::ast::ModulePath {
                segments: vec![crate::ast::PathSegment::new(
                    "a",
                    crate::span::Span::new(0, 1),
                )],
                span: crate::span::Span::new(0, 1),
            },
            imports: Vec::new(),
            items: Vec::new(),
            meta: crate::ast::Meta::new(crate::span::Span::new(0, 1)),
            doc: None,
        };
        // Store is a silent no-op; second lookup still misses.
        c.store(&k, &m);
        assert!(c.lookup(&k).is_none());
    }

    #[test]
    fn active_cache_roundtrips_empty_module() {
        let dir = tempdir();
        let c = EnrichedCache::open(dir.clone()).unwrap();
        assert!(c.is_enabled());
        let k = key("module a;", &empty_inputs());
        assert!(c.lookup(&k).is_none());
        let m = Module::<Enriched> {
            path: crate::ast::ModulePath {
                segments: vec![crate::ast::PathSegment::new(
                    "a",
                    crate::span::Span::new(0, 1),
                )],
                span: crate::span::Span::new(0, 1),
            },
            imports: Vec::new(),
            items: Vec::new(),
            meta: crate::ast::Meta::new(crate::span::Span::new(0, 1)),
            doc: None,
        };
        c.store(&k, &m);
        let got = c.lookup(&k).expect("must hit after store");
        assert_eq!(got, m);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_writes_gitignore() {
        let dir = tempdir();
        let _c = EnrichedCache::open(dir.clone()).unwrap();
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
        let c = EnrichedCache::open(dir.clone()).unwrap();
        let k = key("module a;", &empty_inputs());
        let cache_dir = dir.join("enriched-ir");
        let path = cache_dir.join(format!("{}.bin", k.hex()));
        fs::write(&path, "this is not a valid cache file\n").unwrap();
        // Loader must miss, not panic.
        assert!(c.lookup(&k).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn exact_key_rejects_a_swapped_valid_entry() {
        let dir = tempdir();
        let c = EnrichedCache::open(dir.clone()).unwrap();
        let first_key = key("module first;", &empty_inputs());
        let second_key = key("module second;", &empty_inputs());
        let first_module = module_named("first");
        let second_module = module_named("second");
        c.store(&first_key, &first_module);
        c.store(&second_key, &second_module);

        let root = dir.join("enriched-ir");
        let first_bytes = fs::read(entry_path(&root, &first_key)).unwrap();
        fs::write(entry_path(&root, &second_key), first_bytes).unwrap();

        assert!(c.lookup(&second_key).is_none());
        c.store(&second_key, &second_module);
        assert_eq!(c.lookup(&second_key), Some(second_module));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn body_digest_rejects_a_different_valid_module() {
        let dir = tempdir();
        let c = EnrichedCache::open(dir.clone()).unwrap();
        let first_key = key("module first;", &empty_inputs());
        let second_key = key("module second;", &empty_inputs());
        let first_module = module_named("first");
        let second_module = module_named("second");
        c.store(&first_key, &first_module);
        c.store(&second_key, &second_module);

        let root = dir.join("enriched-ir");
        let first_bytes = fs::read(entry_path(&root, &first_key)).unwrap();
        let second_bytes = fs::read(entry_path(&root, &second_key)).unwrap();
        let first_header_end = first_bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        let second_header_end = second_bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        let mut swapped = first_bytes[..first_header_end].to_vec();
        swapped.extend_from_slice(&second_bytes[second_header_end..]);
        fs::write(entry_path(&root, &first_key), swapped).unwrap();

        assert!(c.lookup(&first_key).is_none());
        c.store(&first_key, &first_module);
        assert_eq!(c.lookup(&first_key), Some(first_module));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncated_body_misses_without_panic() {
        let dir = tempdir();
        let c = EnrichedCache::open(dir.clone()).unwrap();
        let k = key("module a;", &empty_inputs());
        let cache_dir = dir.join("enriched-ir");
        let path = cache_dir.join(format!("{}.bin", k.hex()));
        // Valid header, garbage body (postcard rejects).
        let valid_header = encode_header(&k, &[0xff, 0xff, 0xff]);
        let mut bytes = valid_header.into_bytes();
        bytes.extend_from_slice(&[0xff, 0xff, 0xff]);
        fs::write(&path, &bytes).unwrap();
        assert!(c.lookup(&k).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_compiler_cache_id_misses() {
        let dir = tempdir();
        let c = EnrichedCache::open(dir.clone()).unwrap();
        let k = key("module a;", &empty_inputs());
        let cache_dir = dir.join("enriched-ir");
        let path = cache_dir.join(format!("{}.bin", k.hex()));
        let bad = format!(
            "KIO-ENRICHED-CACHE namespace={CACHE_NAMESPACE} impl={IMPLEMENTATION_TAG} \
             compiler={COMPILER_VERSION} features={FEATURE_SET} compiler_cache_id=bad-cache-id \
             key={k}\n",
            k = k.hex(),
        );
        fs::write(&path, bad).unwrap();
        assert!(c.lookup(&k).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_write_overwrites_atomically() {
        let dir = tempdir();
        let c = EnrichedCache::open(dir.clone()).unwrap();
        let k = key("module a;", &empty_inputs());
        let m1 = Module::<Enriched> {
            path: crate::ast::ModulePath {
                segments: vec![crate::ast::PathSegment::new(
                    "a",
                    crate::span::Span::new(0, 1),
                )],
                span: crate::span::Span::new(0, 1),
            },
            imports: Vec::new(),
            items: Vec::new(),
            meta: crate::ast::Meta::new(crate::span::Span::new(0, 1)),
            doc: None,
        };
        c.store(&k, &m1);
        // Second store with a different value overwrites cleanly.
        let m2 = Module::<Enriched> {
            path: crate::ast::ModulePath {
                segments: vec![crate::ast::PathSegment::new(
                    "b",
                    crate::span::Span::new(0, 1),
                )],
                span: crate::span::Span::new(0, 1),
            },
            imports: Vec::new(),
            items: Vec::new(),
            meta: crate::ast::Meta::new(crate::span::Span::new(0, 1)),
            doc: None,
        };
        c.store(&k, &m2);
        let got = c.lookup(&k).unwrap();
        assert_eq!(got.path.segments[0].name, "b");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_from_disabled_cache_field_is_disabled() {
        let dir = tempdir();
        let cache_field = crate::ast::BuildBlockCache::Disabled {
            span: crate::span::Span::new(0, 0),
            leading_trivia: Vec::new(),
        };
        let c = resolve_from_cache_field(&dir, &cache_field);
        assert!(!c.is_enabled());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_from_relative_cache_field_resolves_against_workspace() {
        let dir = tempdir();
        let cache_field = crate::ast::BuildBlockCache::Path {
            path: "out/.kio-cache/".to_owned(),
            span: crate::span::Span::new(0, 0),
            leading_trivia: Vec::new(),
        };
        let c = resolve_from_cache_field(&dir, &cache_field);
        assert!(c.is_enabled());
        // Caching is rooted under <dir>/out/.kio-cache/enriched-ir/.
        let cache_dir = dir.join("out").join(".kio-cache").join("enriched-ir");
        assert!(
            cache_dir.is_dir(),
            "expected {} to exist",
            DisplayPath(&cache_dir)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn package_inputs_iteration_order_stable() {
        // BTreeMap iterates sorted, so two PackageKeyInputs with
        // the same content have identical postcard encoding even
        // when populated in different orders. This is the
        // open-world invariant for the side-table contribution:
        // adding a newtype in one module shifts the key, but
        // permuting source modules does not.
        let mut a = PackageKeyInputs {
            newtype_fields: std::collections::BTreeMap::new(),
            newtype_members: std::collections::BTreeMap::new(),
        };
        a.newtype_fields
            .insert(nt("main", "Z"), field_input("Z", "mk"));
        a.newtype_fields
            .insert(nt("main", "A"), field_input("A", "mk"));
        let mut b = PackageKeyInputs {
            newtype_fields: std::collections::BTreeMap::new(),
            newtype_members: std::collections::BTreeMap::new(),
        };
        b.newtype_fields
            .insert(nt("main", "A"), field_input("A", "mk"));
        b.newtype_fields
            .insert(nt("main", "Z"), field_input("Z", "mk"));
        assert_eq!(key("module a;", &a), key("module a;", &b));
    }

    #[test]
    fn typed_package_inputs_encode_lexical_heads_and_canonical_identities() {
        let mut inputs = empty_inputs();
        inputs
            .newtype_fields
            .insert(nt("main", "Foo"), field_input("Foo", "mk"));
        inputs
            .newtype_members
            .insert(nt("main", "Foo"), member("mk", "get"));

        let mut expected_fields = std::collections::BTreeMap::new();
        expected_fields.insert(
            ("main".to_owned(), vec!["Foo".to_owned()]),
            (
                ("owner".to_owned(), "Foo".to_owned()),
                "Foo".to_owned(),
                "mk".to_owned(),
            ),
        );
        let mut expected_members = std::collections::BTreeMap::new();
        expected_members.insert(
            ("main".to_owned(), vec!["Foo".to_owned()]),
            (
                ("owner".to_owned(), "Newtype".to_owned()),
                "mk".to_owned(),
                "get".to_owned(),
            ),
        );

        assert_eq!(
            postcard::to_allocvec(&inputs.newtype_fields).unwrap(),
            postcard::to_allocvec(&expected_fields).unwrap()
        );
        assert_eq!(
            postcard::to_allocvec(&inputs.newtype_members).unwrap(),
            postcard::to_allocvec(&expected_members).unwrap()
        );
    }
}
