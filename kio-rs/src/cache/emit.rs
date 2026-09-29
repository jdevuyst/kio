//! Per-backend emitted-string cache.
//!
//! Stores deterministic UTF-8 backend output under
//! `<cache>/emit/<target>/<key>.txt`. The cache is deliberately
//! a thin storage layer: callers decide which IR bytes and target
//! context are part of the key, then store the exact string they would
//! otherwise write to the target artifact. Exact-key and payload-digest
//! validation makes misplaced or damaged entries ordinary misses.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cache::identity::{
    COMPILER_CACHE_ID, COMPILER_VERSION, FEATURE_SET, IMPLEMENTATION_TAG,
};
use crate::cache::keys::{CacheTarget, EmitInputFingerprint, EmitTargetProfileFingerprint};
use crate::path_display::DisplayPath;

pub const CACHE_NAMESPACE: &str = "emit";

static TEMPFILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct EmitCache {
    inner: Backend,
}

#[derive(Debug, Clone)]
enum Backend {
    Active { cache_root: PathBuf, root: PathBuf },
    Disabled,
}

impl EmitCache {
    pub fn open(cache_root: PathBuf) -> std::io::Result<Self> {
        let root = cache_root.join("emit");
        fs::create_dir_all(&root)?;
        write_gitignore_if_absent(&cache_root)?;
        crate::package_collection::mark_generated_dir(&cache_root);
        crate::cache::gc::record_cache_open(&cache_root, crate::cache::gc::CacheFamily::Emit);
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

    pub fn lookup(&self, key: &EmitCacheKey) -> Option<String> {
        match &self.inner {
            Backend::Disabled => {
                log_probe("miss", key, "cache disabled");
                None
            }
            Backend::Active { cache_root, root } => match read_entry(root, key) {
                Ok(Some(text)) => {
                    crate::cache::gc::record_entry_path_access(
                        cache_root,
                        crate::cache::gc::CacheFamily::Emit,
                        root,
                        &entry_path(root, key),
                    );
                    log_probe("hit", key, "");
                    Some(text)
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

    pub fn store(&self, key: &EmitCacheKey, text: &str) {
        let Backend::Active { cache_root, root } = &self.inner else {
            return;
        };
        let existed = entry_path(root, key).exists();
        match write_entry(root, key, text) {
            Ok(()) if existed => {
                crate::cache::gc::record_entry_path_access(
                    cache_root,
                    crate::cache::gc::CacheFamily::Emit,
                    root,
                    &entry_path(root, key),
                );
                log_probe("write", key, "");
            }
            Ok(()) => {
                crate::cache::gc::record_entry_path_access(
                    cache_root,
                    crate::cache::gc::CacheFamily::Emit,
                    root,
                    &entry_path(root, key),
                );
                log_probe("first-write", key, "");
            }
            Err(reason) => log_probe("store-fail", key, &reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmitCacheKey {
    target: String,
    profile_hash: String,
    input_hash: String,
    hex: String,
}

impl EmitCacheKey {
    pub fn new(
        target: CacheTarget,
        profile: EmitTargetProfileFingerprint,
        input: EmitInputFingerprint,
    ) -> Self {
        let profile_hash = profile.as_str().to_owned();
        let input_hash = input.as_str().to_owned();

        let mut h = blake3::Hasher::new();
        write_framed(&mut h, CACHE_NAMESPACE.as_bytes());
        write_framed(&mut h, IMPLEMENTATION_TAG.as_bytes());
        write_framed(&mut h, COMPILER_CACHE_ID.as_bytes());
        write_framed(&mut h, target.as_str().as_bytes());
        write_framed(&mut h, profile_hash.as_bytes());
        write_framed(&mut h, input_hash.as_bytes());
        Self {
            target: target.path_component(),
            profile_hash,
            input_hash,
            hex: h.finalize().to_hex().to_string(),
        }
    }

    pub fn hex(&self) -> &str {
        &self.hex
    }
}

fn read_entry(root: &Path, key: &EmitCacheKey) -> Result<Option<String>, String> {
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
    String::from_utf8(body.to_vec())
        .map(Some)
        .map_err(|e| format!("stale {}: output is not utf-8: {e}", DisplayPath(&path)))
}

fn write_entry(root: &Path, key: &EmitCacheKey, text: &str) -> Result<(), String> {
    fs::create_dir_all(entry_dir(root, key))
        .map_err(|e| format!("mkdir {}: {e}", DisplayPath(&entry_dir(root, key))))?;
    let final_path = entry_path(root, key);
    let tmp_path = tempfile_path(root, key);
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| format!("create {}: {e}", DisplayPath(&tmp_path)))?;
        f.write_all(encode_header(key, text.as_bytes()).as_bytes())
            .map_err(|e| format!("write {}: {e}", DisplayPath(&tmp_path)))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("write {}: {e}", DisplayPath(&tmp_path)))?;
    }
    crate::cache::publish_temp_file(&tmp_path, &final_path, || {
        read_entry(root, key).ok().flatten().is_some()
    })
}

fn entry_dir(root: &Path, key: &EmitCacheKey) -> PathBuf {
    root.join(&key.target)
}

fn entry_path(root: &Path, key: &EmitCacheKey) -> PathBuf {
    entry_dir(root, key).join(format!("{}.txt", key.hex))
}

fn tempfile_path(root: &Path, key: &EmitCacheKey) -> PathBuf {
    let n = TEMPFILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    entry_dir(root, key).join(format!("{}.txt.tmp.{}.{}", key.hex, std::process::id(), n))
}

fn encode_header(key: &EmitCacheKey, body: &[u8]) -> String {
    format!(
        "KIO-EMIT-CACHE namespace={} impl={} compiler={} features={} compiler_cache_id={} target={} profile={} input={} key={} body={}\n",
        CACHE_NAMESPACE,
        IMPLEMENTATION_TAG,
        COMPILER_VERSION,
        FEATURE_SET,
        COMPILER_CACHE_ID,
        key.target,
        key.profile_hash,
        key.input_hash,
        key.hex,
        blake3::hash(body).to_hex(),
    )
}

fn parse_header(buf: &[u8]) -> Result<(usize, std::collections::HashMap<String, String>), String> {
    let Some(nl) = buf.iter().position(|b| *b == b'\n') else {
        return Err("missing header newline".to_owned());
    };
    let line = std::str::from_utf8(&buf[..nl]).map_err(|e| format!("header is not utf-8: {e}"))?;
    let rest = line
        .strip_prefix("KIO-EMIT-CACHE ")
        .ok_or_else(|| "bad header magic".to_owned())?;
    let mut fields = std::collections::HashMap::new();
    for tok in rest.split(' ') {
        let Some((k, v)) = tok.split_once('=') else {
            return Err(format!("bad header token `{tok}`"));
        };
        fields.insert(k.to_owned(), v.to_owned());
    }
    Ok((nl + 1, fields))
}

fn validate_header(
    fields: &std::collections::HashMap<String, String>,
    key: &EmitCacheKey,
    body: &[u8],
) -> Result<(), String> {
    expect(fields, "namespace", CACHE_NAMESPACE)?;
    expect(fields, "impl", IMPLEMENTATION_TAG)?;
    expect(fields, "compiler_cache_id", COMPILER_CACHE_ID)?;
    expect(fields, "target", &key.target)?;
    expect(fields, "profile", &key.profile_hash)?;
    expect(fields, "input", &key.input_hash)?;
    expect(fields, "key", &key.hex)?;
    expect(fields, "body", blake3::hash(body).to_hex().as_str())?;
    Ok(())
}

fn expect(
    fields: &std::collections::HashMap<String, String>,
    name: &str,
    expected: &str,
) -> Result<(), String> {
    match fields.get(name) {
        Some(actual) if actual == expected => Ok(()),
        Some(actual) => Err(format!(
            "field `{name}` is `{actual}`, expected `{expected}`"
        )),
        None => Err(format!("missing field `{name}`")),
    }
}

pub fn resolve_from_cache_field(
    workspace_root: &Path,
    cache: &crate::ast::BuildBlockCache,
) -> EmitCache {
    match cache {
        crate::ast::BuildBlockCache::Path { path, .. } => {
            let abs = absolutize(workspace_root, path);
            EmitCache::open(abs).unwrap_or_else(|_| EmitCache::disabled())
        }
        crate::ast::BuildBlockCache::Disabled { .. } => EmitCache::disabled(),
    }
}

fn absolutize(workspace_root: &Path, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        workspace_root.join(p)
    }
}

fn write_gitignore_if_absent(cache_root: &Path) -> std::io::Result<()> {
    fs::create_dir_all(cache_root)?;
    let path = cache_root.join(".gitignore");
    if path.exists() {
        return Ok(());
    }
    fs::write(path, "*\n!.gitignore\n")
}

fn write_framed(h: &mut blake3::Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

fn log_probe(status: &str, key: &EmitCacheKey, detail: &str) {
    if std::env::var("KIO_DEBUG_EMIT_CACHE").ok().as_deref() != Some("1") {
        return;
    }
    if detail.is_empty() {
        eprintln!("emit-cache: {status} {} {}", key.target, key.hex);
    } else {
        eprintln!("emit-cache: {status} {} {} ({detail})", key.target, key.hex);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::keys::{CacheTarget, EmitInputFingerprint, EmitTargetProfileFingerprint};

    fn key() -> EmitCacheKey {
        EmitCacheKey::new(
            CacheTarget::new("js"),
            EmitTargetProfileFingerprint::from_bytes(b"profile"),
            EmitInputFingerprint::from_parts(&[b"input"]),
        )
    }

    fn key_with_input(input: &[u8]) -> EmitCacheKey {
        EmitCacheKey::new(
            CacheTarget::new("js"),
            EmitTargetProfileFingerprint::from_bytes(b"profile"),
            EmitInputFingerprint::from_parts(&[input]),
        )
    }

    #[test]
    fn entry_roundtrips_under_unversioned_target_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = EmitCache::open(dir.path().join("cache")).expect("open cache");
        let key = key();
        cache.store(&key, "output");
        assert_eq!(cache.lookup(&key).as_deref(), Some("output"));

        let Backend::Active { root, .. } = &cache.inner else {
            unreachable!();
        };
        assert_eq!(root, &dir.path().join("cache").join("emit"));
        assert!(entry_path(root, &key).is_file());
    }

    #[test]
    fn store_repairs_a_non_file_destination() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = EmitCache::open(dir.path().join("cache")).expect("open cache");
        let key = key();
        let Backend::Active { root, .. } = &cache.inner else {
            unreachable!();
        };
        fs::create_dir_all(entry_path(root, &key)).expect("create invalid destination directory");

        assert!(cache.lookup(&key).is_none());
        cache.store(&key, "recomputed output");
        assert_eq!(cache.lookup(&key).as_deref(), Some("recomputed output"));
    }

    #[test]
    fn wrong_compiler_cache_id_misses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = EmitCache::open(dir.path().join("cache")).expect("open cache");
        let key = key();
        cache.store(&key, "output");
        let Backend::Active { root, .. } = &cache.inner else {
            unreachable!();
        };
        let path = entry_path(root, &key);
        let mut bytes = fs::read(&path).expect("read cache file");
        let needle = format!("compiler_cache_id={COMPILER_CACHE_ID}").into_bytes();
        let replacement = b"compiler_cache_id=bad-cache-id";
        let start = bytes
            .windows(needle.len())
            .position(|window| window == needle.as_slice())
            .expect("compiler cache id in header");
        bytes.splice(start..start + needle.len(), replacement.iter().copied());
        fs::write(&path, bytes).expect("mutate cache file");

        assert!(cache.lookup(&key).is_none());
    }

    #[test]
    fn exact_key_rejects_a_swapped_valid_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = EmitCache::open(dir.path().join("cache")).expect("open cache");
        let first_key = key_with_input(b"first");
        let second_key = key_with_input(b"second");
        cache.store(&first_key, "first output");
        cache.store(&second_key, "second output");
        let Backend::Active { root, .. } = &cache.inner else {
            unreachable!();
        };

        let first_bytes = fs::read(entry_path(root, &first_key)).expect("read first entry");
        fs::write(entry_path(root, &second_key), first_bytes).expect("swap entry");

        assert!(cache.lookup(&second_key).is_none());
        cache.store(&second_key, "second output");
        assert_eq!(cache.lookup(&second_key).as_deref(), Some("second output"));
    }

    #[test]
    fn body_digest_rejects_different_valid_utf8() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = EmitCache::open(dir.path().join("cache")).expect("open cache");
        let key = key();
        cache.store(&key, "first output");
        let Backend::Active { root, .. } = &cache.inner else {
            unreachable!();
        };
        let path = entry_path(root, &key);
        let mut bytes = fs::read(&path).expect("read entry");
        let header_end = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        bytes.truncate(header_end);
        bytes.extend_from_slice(b"different output");
        fs::write(&path, bytes).expect("replace body");

        assert!(cache.lookup(&key).is_none());
        cache.store(&key, "first output");
        assert_eq!(cache.lookup(&key).as_deref(), Some("first output"));
    }
}
