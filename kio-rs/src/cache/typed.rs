//! Per-module on-disk cache for typed `Module<Prime>` values.
//!
//! The cache sits after package lowering / resolution and before
//! per-module type checking. A hit deserializes the already typed
//! `Module<Prime>` for one regular module; package-file export block
//! bodies are still checked each run because they are package-boundary
//! forms rather than module bodies.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use blake3::Hasher;

use crate::ast::{Module, Prime};
use crate::cache::identity::{
    COMPILER_CACHE_ID, COMPILER_VERSION, FEATURE_SET, IMPLEMENTATION_TAG,
};
use crate::cache::keys::{PackageModuleKey, PackageName, PipelineTag, TypedModuleDependency};
use crate::path_display::DisplayPath;

pub const CACHE_NAMESPACE: &str = "typed-module";

static TEMPFILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct TypedModuleCache {
    inner: Backend,
}

#[derive(Debug, Clone)]
enum Backend {
    Active { cache_root: PathBuf, root: PathBuf },
    Disabled,
}

impl TypedModuleCache {
    pub fn open(cache_root: PathBuf) -> std::io::Result<Self> {
        let root = cache_root.join("typed");
        fs::create_dir_all(&root)?;
        write_gitignore_if_absent(&cache_root)?;
        crate::cache::gc::record_cache_open(&cache_root, crate::cache::gc::CacheFamily::Typed);
        Ok(Self {
            inner: Backend::Active { cache_root, root },
        })
    }

    pub fn disabled() -> Self {
        Self {
            inner: Backend::Disabled,
        }
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self.inner, Backend::Active { .. })
    }

    pub fn lookup(&self, key: &TypedModuleCacheKey) -> Option<Module<Prime>> {
        match &self.inner {
            Backend::Disabled => {
                log_probe("miss", key, "cache disabled");
                None
            }
            Backend::Active { cache_root, root } => match read_entry(root, key) {
                Ok(Some(module)) => {
                    crate::cache::gc::record_entry_path_access(
                        cache_root,
                        crate::cache::gc::CacheFamily::Typed,
                        root,
                        &entry_path(root, key),
                    );
                    log_probe("hit", key, "");
                    Some(module)
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

    pub fn store(&self, key: &TypedModuleCacheKey, module: &Module<Prime>) {
        let Backend::Active { cache_root, root } = &self.inner else {
            return;
        };
        if !writes_enabled() {
            return;
        }
        match write_entry(root, key, module) {
            Ok(()) => {
                crate::cache::gc::record_entry_path_access(
                    cache_root,
                    crate::cache::gc::CacheFamily::Typed,
                    root,
                    &entry_path(root, key),
                );
                // Publication is idempotent, but another writer can race this
                // one between lookup and rename. Do not claim which process
                // created the content-addressed entry first.
                log_probe("write", key, "");
            }
            Err(reason) => log_probe("store-fail", key, &reason),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TypedModuleCacheKey {
    // Provenance for probes and the human-readable header only; package
    // configuration is validated before lookup and is not a module-typing
    // input, so this label is deliberately absent from the semantic digest.
    diagnostic_package_name: PackageName,
    module_path: PackageModuleKey,
    pipeline_tag: PipelineTag,
    source_hash: String,
    dep_hash: String,
    hex: String,
}

impl TypedModuleCacheKey {
    pub fn new(
        package_name: PackageName,
        module_path: PackageModuleKey,
        pipeline_tag: PipelineTag,
        source_bytes: &[u8],
        dep_fingerprints: &[TypedModuleDependency],
    ) -> Self {
        let source_hash = blake3::hash(source_bytes).to_hex().to_string();
        let dep_hash = hash_dep_fingerprints(dep_fingerprints);
        let mut h = Hasher::new();
        write_framed(&mut h, CACHE_NAMESPACE.as_bytes());
        write_framed(&mut h, IMPLEMENTATION_TAG.as_bytes());
        write_framed(&mut h, COMPILER_CACHE_ID.as_bytes());
        write_framed(&mut h, pipeline_tag.as_str().as_bytes());
        write_framed(&mut h, module_path.as_str().as_bytes());
        write_framed(&mut h, source_hash.as_bytes());
        write_framed(&mut h, dep_hash.as_bytes());
        Self {
            diagnostic_package_name: package_name,
            module_path,
            pipeline_tag,
            source_hash,
            dep_hash,
            hex: h.finalize().to_hex().to_string(),
        }
    }

    pub fn hex(&self) -> &str {
        &self.hex
    }
}

fn hash_dep_fingerprints(dep_fingerprints: &[TypedModuleDependency]) -> String {
    let mut h = Hasher::new();
    for dep in dep_fingerprints {
        write_framed(&mut h, dep.label().as_bytes());
        write_framed(&mut h, dep.fingerprint().as_str().as_bytes());
    }
    h.finalize().to_hex().to_string()
}

fn read_entry(root: &Path, key: &TypedModuleCacheKey) -> Result<Option<Module<Prime>>, String> {
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
    let module = postcard::from_bytes(body)
        .map_err(|e| format!("stale {}: postcard decode failed: {e}", DisplayPath(&path)))?;
    Ok(Some(module))
}

fn write_entry(
    root: &Path,
    key: &TypedModuleCacheKey,
    module: &Module<Prime>,
) -> Result<(), String> {
    write_entry_with_rename(root, key, module, |from, to| fs::rename(from, to))
}

fn write_entry_with_rename(
    root: &Path,
    key: &TypedModuleCacheKey,
    module: &Module<Prime>,
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|e| format!("mkdir {}: {e}", DisplayPath(root)))?;
    let final_path = entry_path(root, key);
    let tmp_path = tempfile_path(root, key);
    let body = postcard::to_allocvec(module).map_err(|e| format!("postcard encode failed: {e}"))?;
    let header = encode_header(key, &body);
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| format!("create {}: {e}", DisplayPath(&tmp_path)))?;
        f.write_all(header.as_bytes())
            .map_err(|e| format!("write {}: {e}", DisplayPath(&tmp_path)))?;
        f.write_all(&body)
            .map_err(|e| format!("write {}: {e}", DisplayPath(&tmp_path)))?;
    }
    if let Err(rename_error) = rename(&tmp_path, &final_path) {
        if read_entry(root, key).ok().flatten().is_some() {
            let _ = fs::remove_file(&tmp_path);
            return Ok(());
        }
        match fs::remove_file(&final_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(remove_error) => {
                let _ = fs::remove_file(&tmp_path);
                return Err(format!(
                    "rename {} -> {}: {rename_error}; remove invalid destination: {remove_error}",
                    DisplayPath(&tmp_path),
                    DisplayPath(&final_path)
                ));
            }
        }
        if let Err(retry_error) = rename(&tmp_path, &final_path) {
            if read_entry(root, key).ok().flatten().is_some() {
                let _ = fs::remove_file(&tmp_path);
                return Ok(());
            }
            let _ = fs::remove_file(&tmp_path);
            return Err(format!(
                "rename {} -> {}: {rename_error}; retry: {retry_error}",
                DisplayPath(&tmp_path),
                DisplayPath(&final_path)
            ));
        }
    }
    Ok(())
}

fn entry_path(root: &Path, key: &TypedModuleCacheKey) -> PathBuf {
    root.join(format!("{}.bin", key.hex()))
}

fn tempfile_path(root: &Path, key: &TypedModuleCacheKey) -> PathBuf {
    let pid = std::process::id();
    let counter = TEMPFILE_COUNTER.fetch_add(1, Ordering::SeqCst);
    root.join(format!("{}.bin.tmp.{pid}-{counter}", key.hex()))
}

fn encode_header(key: &TypedModuleCacheKey, body: &[u8]) -> String {
    format!(
        "KIO-TYPED-MODULE-CACHE namespace={namespace} impl={impl_tag} \
         compiler={compiler} features={features} compiler_cache_id={compiler_cache_id} \
         pipeline={pipeline} package={package} module={module} source={source} deps={deps} \
         key={hex} body={body_digest}\n",
        namespace = CACHE_NAMESPACE,
        impl_tag = IMPLEMENTATION_TAG,
        compiler = COMPILER_VERSION,
        features = FEATURE_SET,
        compiler_cache_id = COMPILER_CACHE_ID,
        pipeline = key.pipeline_tag.as_str(),
        package = key.diagnostic_package_name.as_str(),
        module = key.module_path.as_str(),
        source = key.source_hash,
        deps = key.dep_hash,
        hex = key.hex,
        body_digest = blake3::hash(body).to_hex(),
    )
}

fn parse_header(buf: &[u8]) -> Result<(usize, HashMap<String, String>), String> {
    let newline_idx = buf
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| "missing header newline".to_owned())?;
    let header_bytes = &buf[..newline_idx];
    let header_str =
        std::str::from_utf8(header_bytes).map_err(|_| "header is not valid UTF-8".to_owned())?;
    let rest = header_str
        .strip_prefix("KIO-TYPED-MODULE-CACHE ")
        .ok_or_else(|| format!("missing magic: header `{header_str}`"))?;
    let mut fields = HashMap::new();
    for tok in rest.split(' ') {
        if let Some((k, v)) = tok.split_once('=') {
            fields.insert(k.to_owned(), v.to_owned());
        }
    }
    Ok((newline_idx + 1, fields))
}

fn validate_header(
    fields: &HashMap<String, String>,
    expected: &TypedModuleCacheKey,
    body: &[u8],
) -> Result<(), String> {
    expect_field(fields, "namespace", CACHE_NAMESPACE)?;
    expect_field(fields, "impl", IMPLEMENTATION_TAG)?;
    expect_field(fields, "compiler_cache_id", COMPILER_CACHE_ID)?;
    expect_field(fields, "pipeline", expected.pipeline_tag.as_str())?;
    expect_field(fields, "module", expected.module_path.as_str())?;
    expect_field(fields, "source", &expected.source_hash)?;
    expect_field(fields, "deps", &expected.dep_hash)?;
    expect_field(fields, "key", &expected.hex)?;
    expect_field(fields, "body", blake3::hash(body).to_hex().as_str())?;
    Ok(())
}

fn expect_field(
    fields: &HashMap<String, String>,
    name: &str,
    expected: &str,
) -> Result<(), String> {
    let actual = fields
        .get(name)
        .ok_or_else(|| format!("header missing `{name}=`"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{name} mismatch: file `{actual}`, expected `{expected}`"
        ))
    }
}

fn write_framed(h: &mut Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

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
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

fn log_probe(kind: &str, key: &TypedModuleCacheKey, reason: &str) {
    let on = std::env::var("KIO_DEBUG_TYPED_CACHE")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);
    if !on {
        return;
    }
    if reason.is_empty() {
        eprintln!(
            "typed-cache: {kind} {} {} {}",
            key.diagnostic_package_name, key.module_path, key.hex
        );
    } else {
        eprintln!(
            "typed-cache: {kind} {} {} {} ({reason})",
            key.diagnostic_package_name, key.module_path, key.hex
        );
    }
}

fn writes_enabled() -> bool {
    std::env::var("KIO_DEBUG_WRITE_TYPED_CACHE")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!(
            "kio-typed-cache-test-{nonce}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn empty_module() -> Module<Prime> {
        module_named("main")
    }

    fn module_named(name: &str) -> Module<Prime> {
        Module {
            path: crate::ast::ModulePath {
                segments: vec![crate::ast::PathSegment::new(
                    name,
                    crate::span::Span::new(0, 4),
                )],
                span: crate::span::Span::new(0, 4),
            },
            imports: Vec::new(),
            items: Vec::new(),
            meta: crate::ast::Meta::new(crate::span::Span::new(0, 4)),
            doc: None,
        }
    }

    fn assert_no_tempfiles(root: &Path, context: &str) {
        assert!(
            fs::read_dir(root).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp.")
            }),
            "{context} must not leave temporary files behind"
        );
    }

    fn key(source: &[u8], deps: &[TypedModuleDependency]) -> TypedModuleCacheKey {
        key_for_package("pkg", source, deps)
    }

    fn key_for_package(
        package_name: &str,
        source: &[u8],
        deps: &[TypedModuleDependency],
    ) -> TypedModuleCacheKey {
        TypedModuleCacheKey::new(
            PackageName::new(package_name),
            PackageModuleKey::new("main"),
            PipelineTag::new(""),
            source,
            deps,
        )
    }

    #[test]
    fn key_changes_on_source_dependency_and_pipeline_inputs() {
        let base = key(b"module main;", &[]);
        let source = key(b"module main; ", &[]);
        let dep = key(
            b"module main;",
            &[TypedModuleDependency::same_package_module(
                PackageModuleKey::new("dep"),
                crate::cache::keys::SurfaceFingerprint::new("dep"),
            )],
        );
        let pipeline = TypedModuleCacheKey::new(
            PackageName::new("pkg"),
            PackageModuleKey::new("main"),
            PipelineTag::new("another-pipeline."),
            b"module main;",
            &[],
        );
        assert_ne!(base.hex(), source.hex());
        assert_ne!(base.hex(), dep.hex());
        assert_ne!(base.hex(), pipeline.hex());
        assert_eq!(base.hex().len(), 64);
    }

    #[test]
    fn key_separates_dependency_domains_and_module_identities() {
        let fingerprint = crate::cache::keys::SurfaceFingerprint::new("dep");
        let ordinary = key(
            b"module main;",
            &[TypedModuleDependency::same_package_module(
                PackageModuleKey::new("dep"),
                fingerprint.clone(),
            )],
        );
        let other_module = key(
            b"module main;",
            &[TypedModuleDependency::same_package_module(
                PackageModuleKey::new("other-dep"),
                fingerprint.clone(),
            )],
        );
        let elaborator = key(
            b"module main;",
            &[TypedModuleDependency::same_package_elaborator_module(
                PackageModuleKey::new("dep"),
                fingerprint,
            )],
        );
        assert_ne!(ordinary.hex(), other_module.hex());
        assert_ne!(ordinary.hex(), elaborator.hex());
    }

    #[test]
    fn disabled_cache_misses_and_ignores_store() {
        let cache = TypedModuleCache::disabled();
        assert!(!cache.is_enabled());
        let key = key(b"module main;", &[]);
        cache.store(&key, &empty_module());
        assert!(cache.lookup(&key).is_none());
    }

    #[test]
    fn active_cache_roundtrips_module() {
        let dir = tempdir();
        let cache = TypedModuleCache::open(dir.clone()).unwrap();
        let key = key(b"module main;", &[]);
        assert!(cache.lookup(&key).is_none());
        let module = empty_module();
        cache.store(&key, &module);
        assert_eq!(cache.lookup(&key), Some(module));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn cache_observes_entries_published_after_it_opens() {
        let dir = tempdir();
        let early = TypedModuleCache::open(dir.clone()).unwrap();
        let peer = TypedModuleCache::open(dir.clone()).unwrap();
        let key = key(b"module main;", &[]);
        let module = empty_module();

        peer.store(&key, &module);

        assert_eq!(early.lookup(&key), Some(module));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn concurrent_same_key_publication_is_idempotent() {
        const WRITERS: usize = 2;

        let dir = tempdir();
        let cache = TypedModuleCache::open(dir.clone()).unwrap();
        let root = dir.join("typed");
        let module = empty_module();
        let lookup_key = key_for_package("reader", b"module main;", &[]);
        assert!(!entry_path(&root, &lookup_key).exists());

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(WRITERS + 1));
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..WRITERS)
                .map(|writer| {
                    let barrier = barrier.clone();
                    let root = root.clone();
                    scope.spawn(move || {
                        let key =
                            key_for_package(&format!("writer-{writer}"), b"module main;", &[]);
                        barrier.wait();
                        write_entry(&root, &key, &empty_module())
                    })
                })
                .collect();
            barrier.wait();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });

        assert!(results.iter().all(Result::is_ok), "{results:?}");
        assert_eq!(cache.lookup(&lookup_key), Some(module));
        assert_no_tempfiles(&root, "same-key publication");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn nonreplacing_rename_accepts_a_valid_peer_publication() {
        let dir = tempdir();
        let root = dir.join("typed");
        let key = key(b"module main;", &[]);
        let module = empty_module();
        write_entry(&root, &key, &module).unwrap();
        let attempts = std::cell::Cell::new(0);

        write_entry_with_rename(&root, &key, &module, |_, _| {
            attempts.set(attempts.get() + 1);
            Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
        })
        .unwrap();

        assert_eq!(attempts.get(), 1);
        assert_eq!(read_entry(&root, &key).unwrap(), Some(module));
        assert_no_tempfiles(&root, "accepting a valid peer entry");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn nonreplacing_rename_repairs_a_corrupt_destination() {
        let dir = tempdir();
        let root = dir.join("typed");
        fs::create_dir_all(&root).unwrap();
        let key = key(b"module main;", &[]);
        let module = empty_module();
        fs::write(entry_path(&root, &key), b"corrupt").unwrap();
        let attempts = std::cell::Cell::new(0);

        write_entry_with_rename(&root, &key, &module, |from, to| {
            let attempt = attempts.get();
            attempts.set(attempt + 1);
            if attempt == 0 {
                Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
            } else {
                fs::rename(from, to)
            }
        })
        .unwrap();

        assert_eq!(attempts.get(), 2);
        assert_eq!(read_entry(&root, &key).unwrap(), Some(module));
        assert_no_tempfiles(&root, "repairing an invalid destination");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_destination_repair_accepts_a_peer_winning_the_retry() {
        let dir = tempdir();
        let root = dir.join("typed");
        fs::create_dir_all(&root).unwrap();
        let key = key(b"module main;", &[]);
        let module = empty_module();
        fs::write(entry_path(&root, &key), b"corrupt").unwrap();
        let peer_body = postcard::to_allocvec(&module).unwrap();
        let mut peer_entry = encode_header(&key, &peer_body).into_bytes();
        peer_entry.extend(peer_body);
        let attempts = std::cell::Cell::new(0);

        write_entry_with_rename(&root, &key, &module, |_, to| {
            let attempt = attempts.get();
            attempts.set(attempt + 1);
            if attempt == 1 {
                fs::write(to, &peer_entry).unwrap();
            }
            Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
        })
        .unwrap();

        assert_eq!(attempts.get(), 2);
        assert_eq!(read_entry(&root, &key).unwrap(), Some(module));
        assert_no_tempfiles(&root, "accepting the peer that won a repair retry");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn semantic_module_identity_reuses_entries_across_package_names() {
        let dir = tempdir();
        let cache = TypedModuleCache::open(dir.clone()).unwrap();
        let package_a = key_for_package("package-a", b"module main;", &[]);
        let package_b = key_for_package("package-b", b"module main;", &[]);

        assert_eq!(package_a.hex(), package_b.hex());
        assert_eq!(
            entry_path(Path::new("/cache"), &package_a),
            entry_path(Path::new("/cache"), &package_b)
        );

        let module = empty_module();
        cache.store(&package_a, &module);
        let rel_path = PathBuf::from(format!("{}.bin", package_a.hex()));
        assert!(
            crate::cache::gc::entry_access_secs(
                &dir,
                crate::cache::gc::CacheFamily::Typed,
                &rel_path,
            )
            .is_some()
        );
        crate::cache::gc::remove_entry_access(
            &dir,
            crate::cache::gc::CacheFamily::Typed,
            &rel_path,
        );
        assert_eq!(cache.lookup(&package_b), Some(module));
        assert!(
            crate::cache::gc::entry_access_secs(
                &dir,
                crate::cache::gc::CacheFamily::Typed,
                &rel_path,
            )
            .is_some(),
            "a cross-package hit should refresh the flat semantic entry's access marker"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn typed_entries_are_stored_by_semantic_key() {
        let key = key_for_package("diagnostic-label", b"module main;", &[]);
        assert_eq!(
            entry_path(Path::new("/cache"), &key),
            Path::new("/cache").join(format!("{}.bin", key.hex()))
        );
    }

    #[test]
    fn module_path_is_carried_by_the_semantic_key() {
        let main = TypedModuleCacheKey::new(
            PackageName::new("pkg"),
            PackageModuleKey::new("pkg/main"),
            PipelineTag::new(""),
            b"module pkg/main;",
            &[],
        );
        let other = TypedModuleCacheKey::new(
            PackageName::new("pkg"),
            PackageModuleKey::new("pkg/other"),
            PipelineTag::new(""),
            b"module pkg/main;",
            &[],
        );
        assert_ne!(main.hex(), other.hex());
        assert_ne!(
            entry_path(Path::new("/cache"), &main),
            entry_path(Path::new("/cache"), &other)
        );
    }

    #[test]
    fn stale_header_and_malformed_body_miss() {
        let dir = tempdir();
        let cache = TypedModuleCache::open(dir.clone()).unwrap();
        let old_key = key(b"module main;", &[]);
        let new_key = key(b"module main; ", &[]);
        let path = entry_path(&dir.join("typed"), &new_key);
        let body = postcard::to_allocvec(&empty_module()).unwrap();
        let mut stale = encode_header(&old_key, &body).into_bytes();
        stale.extend_from_slice(&body);
        fs::write(&path, stale).unwrap();
        assert!(cache.lookup(&new_key).is_none());

        let malformed_body = b"not a postcard module";
        let mut malformed = encode_header(&new_key, malformed_body).into_bytes();
        malformed.extend_from_slice(malformed_body);
        fs::write(path, malformed).unwrap();
        assert!(cache.lookup(&new_key).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn exact_key_header_rejects_a_different_valid_module_body() {
        let dir = tempdir();
        let cache = TypedModuleCache::open(dir.clone()).unwrap();
        let key = key(b"module main;", &[]);
        let expected_body = postcard::to_allocvec(&empty_module()).unwrap();
        let replacement_body = postcard::to_allocvec(&module_named("test")).unwrap();
        assert_ne!(expected_body, replacement_body);

        let mut swapped = encode_header(&key, &expected_body).into_bytes();
        swapped.extend(replacement_body);
        fs::write(entry_path(&dir.join("typed"), &key), swapped).unwrap();

        assert!(cache.lookup(&key).is_none());
        let _ = fs::remove_dir_all(dir);
    }
}
