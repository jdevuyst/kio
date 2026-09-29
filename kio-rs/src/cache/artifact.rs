//! Per-backend package artifact cache.
//!
//! Stores complete backend output directories under
//! `<cache>/artifacts/<target>/<key>/tree`. A hit replaces the
//! configured target output directory with the cached tree, preserving
//! the same stale-file behavior as a fresh backend run. The manifest
//! authenticates the exact semantic key and a deterministic digest of
//! the cached content tree before any output is restored.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cache::identity::{
    COMPILER_CACHE_ID, COMPILER_VERSION, FEATURE_SET, IMPLEMENTATION_TAG,
};
use crate::cache::keys::{ArtifactInputFingerprint, ArtifactTargetProfileFingerprint, CacheTarget};
use crate::path_display::DisplayPath;

pub const CACHE_NAMESPACE: &str = "artifact";

static TEMPDIR_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct ArtifactCache {
    inner: Backend,
}

#[derive(Debug, Clone)]
enum Backend {
    Active { cache_root: PathBuf, root: PathBuf },
    Disabled,
}

impl ArtifactCache {
    pub fn open(cache_root: PathBuf) -> std::io::Result<Self> {
        let root = cache_root.join("artifacts");
        fs::create_dir_all(&root)?;
        write_gitignore_if_absent(&cache_root)?;
        crate::package_collection::mark_generated_dir(&cache_root);
        crate::cache::gc::record_cache_open(&cache_root, crate::cache::gc::CacheFamily::Artifacts);
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

    pub fn restore(&self, key: &ArtifactCacheKey, target_dir: &Path) -> bool {
        let Backend::Active { cache_root, root } = &self.inner else {
            log_probe("miss", key, "cache disabled");
            return false;
        };
        match restore_entry(root, key, target_dir) {
            Ok(true) => {
                crate::cache::gc::record_entry_path_access(
                    cache_root,
                    crate::cache::gc::CacheFamily::Artifacts,
                    root,
                    &entry_dir(root, key),
                );
                log_probe("hit", key, "");
                true
            }
            Ok(false) => {
                log_probe("miss", key, "no entry");
                false
            }
            Err(reason) => {
                log_probe("miss", key, &reason);
                false
            }
        }
    }

    pub fn store(&self, key: &ArtifactCacheKey, target_dir: &Path) {
        let Backend::Active { cache_root, root } = &self.inner else {
            return;
        };
        let existed = entry_dir(root, key).exists();
        match write_entry(root, key, target_dir) {
            Ok(()) if existed => {
                crate::cache::gc::record_entry_path_access(
                    cache_root,
                    crate::cache::gc::CacheFamily::Artifacts,
                    root,
                    &entry_dir(root, key),
                );
                log_probe("write", key, "");
            }
            Ok(()) => {
                crate::cache::gc::record_entry_path_access(
                    cache_root,
                    crate::cache::gc::CacheFamily::Artifacts,
                    root,
                    &entry_dir(root, key),
                );
                log_probe("first-write", key, "");
            }
            Err(reason) => log_probe("store-fail", key, &reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArtifactCacheKey {
    target: String,
    target_hash: String,
    input_hash: String,
    hex: String,
}

impl ArtifactCacheKey {
    pub fn new(
        target: CacheTarget,
        target_profile: ArtifactTargetProfileFingerprint,
        input: ArtifactInputFingerprint,
    ) -> Self {
        let target_hash = target_profile.as_str().to_owned();
        let input_hash = input.as_str().to_owned();

        let mut h = blake3::Hasher::new();
        write_framed(&mut h, CACHE_NAMESPACE.as_bytes());
        write_framed(&mut h, IMPLEMENTATION_TAG.as_bytes());
        write_framed(&mut h, COMPILER_CACHE_ID.as_bytes());
        write_framed(&mut h, target.as_str().as_bytes());
        write_framed(&mut h, target_hash.as_bytes());
        write_framed(&mut h, input_hash.as_bytes());
        Self {
            target: target.path_component(),
            target_hash,
            input_hash,
            hex: h.finalize().to_hex().to_string(),
        }
    }
}

fn restore_entry(root: &Path, key: &ArtifactCacheKey, target_dir: &Path) -> Result<bool, String> {
    let dir = entry_dir(root, key);
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(format!(
                "artifact cache entry `{}` is not a directory",
                DisplayPath(&dir)
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("stat `{}`: {error}", DisplayPath(&dir))),
    }
    validate_entry_dir(&dir, key)?;
    let tree = dir.join("tree");
    replace_dir(target_dir, &tree)?;
    Ok(true)
}

fn write_entry(root: &Path, key: &ArtifactCacheKey, target_dir: &Path) -> Result<(), String> {
    write_entry_with_rename(root, key, target_dir, |from, to| fs::rename(from, to))
}

fn write_entry_with_rename(
    root: &Path,
    key: &ArtifactCacheKey,
    target_dir: &Path,
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    if !target_dir.is_dir() {
        return Err(format!(
            "target dir `{}` does not exist",
            DisplayPath(target_dir)
        ));
    }
    let final_dir = entry_dir(root, key);
    if validate_entry_dir(&final_dir, key).is_ok() {
        return Ok(());
    }
    let tmp_dir = temp_entry_dir(root, key);
    crate::cache::remove_path_if_exists(&tmp_dir)
        .map_err(|e| format!("remove stale temp `{}`: {e}", DisplayPath(&tmp_dir)))?;
    let prepare_result = (|| {
        fs::create_dir_all(tmp_dir.join("tree"))
            .map_err(|e| format!("mkdir `{}`: {e}", DisplayPath(&tmp_dir)))?;
        copy_dir_contents(target_dir, &tmp_dir.join("tree"))?;
        write_manifest(&tmp_dir.join("manifest.txt"), key, &tmp_dir.join("tree"))?;
        validate_entry_dir(&tmp_dir, key)?;
        if let Some(parent) = final_dir.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir `{}`: {e}", DisplayPath(parent)))?;
        }
        Ok::<(), String>(())
    })();
    if let Err(error) = prepare_result {
        let _ = crate::cache::remove_path_if_exists(&tmp_dir);
        return Err(error);
    }

    if let Err(rename_error) = rename(&tmp_dir, &final_dir) {
        if validate_entry_dir(&final_dir, key).is_ok() {
            let _ = crate::cache::remove_path_if_exists(&tmp_dir);
            return Ok(());
        }
        if let Err(remove_error) = crate::cache::remove_path_if_exists(&final_dir) {
            let _ = crate::cache::remove_path_if_exists(&tmp_dir);
            return Err(format!(
                "rename {} -> {}: {rename_error}; remove invalid destination: {remove_error}",
                DisplayPath(&tmp_dir),
                DisplayPath(&final_dir)
            ));
        }
        if let Err(retry_error) = rename(&tmp_dir, &final_dir) {
            if validate_entry_dir(&final_dir, key).is_ok() {
                let _ = crate::cache::remove_path_if_exists(&tmp_dir);
                return Ok(());
            }
            let _ = crate::cache::remove_path_if_exists(&tmp_dir);
            return Err(format!(
                "rename {} -> {}: {rename_error}; retry: {retry_error}",
                DisplayPath(&tmp_dir),
                DisplayPath(&final_dir)
            ));
        }
    }
    validate_entry_dir(&final_dir, key)
}

fn replace_dir(target_dir: &Path, source_tree: &Path) -> Result<(), String> {
    crate::cache::remove_path_if_exists(target_dir)
        .map_err(|e| format!("remove `{}`: {e}", DisplayPath(target_dir)))?;
    fs::create_dir_all(target_dir)
        .map_err(|e| format!("mkdir `{}`: {e}", DisplayPath(target_dir)))?;
    copy_dir_contents(source_tree, target_dir)
}

fn copy_dir_contents(from: &Path, to: &Path) -> Result<(), String> {
    for entry in fs::read_dir(from).map_err(|e| format!("read `{}`: {e}", DisplayPath(from)))? {
        let entry = entry.map_err(|e| format!("read `{}`: {e}", DisplayPath(from)))?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let ty = entry
            .file_type()
            .map_err(|e| format!("stat `{}`: {e}", DisplayPath(&src)))?;
        if ty.is_dir() {
            fs::create_dir_all(&dst).map_err(|e| format!("mkdir `{}`: {e}", DisplayPath(&dst)))?;
            copy_dir_contents(&src, &dst)?;
        } else if ty.is_file() {
            fs::copy(&src, &dst)
                .map_err(|e| format!("copy {} -> {}: {e}", DisplayPath(&src), DisplayPath(&dst)))?;
        } else {
            return Err(format!(
                "unsupported artifact entry `{}`",
                DisplayPath(&src)
            ));
        }
    }
    Ok(())
}

fn validate_entry_dir(dir: &Path, key: &ArtifactCacheKey) -> Result<(), String> {
    let metadata = fs::symlink_metadata(dir)
        .map_err(|error| format!("stat `{}`: {error}", DisplayPath(dir)))?;
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "artifact cache entry `{}` is not a directory",
            DisplayPath(dir)
        ));
    }
    let tree = dir.join("tree");
    let tree_metadata = fs::symlink_metadata(&tree)
        .map_err(|error| format!("stat `{}`: {error}", DisplayPath(&tree)))?;
    if !tree_metadata.file_type().is_dir() {
        return Err(format!("missing tree dir `{}`", DisplayPath(&tree)));
    }
    validate_manifest(&dir.join("manifest.txt"), key, &tree)
}

fn write_manifest(path: &Path, key: &ArtifactCacheKey, tree: &Path) -> Result<(), String> {
    let tree_digest = tree_digest(tree)?;
    let mut f =
        fs::File::create(path).map_err(|e| format!("create `{}`: {e}", DisplayPath(path)))?;
    f.write_all(encode_manifest(key, &tree_digest).as_bytes())
        .map_err(|e| format!("write `{}`: {e}", DisplayPath(path)))
}

fn validate_manifest(path: &Path, key: &ArtifactCacheKey, tree: &Path) -> Result<(), String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("read `{}`: {e}", DisplayPath(path)))?;
    let fields = parse_manifest_fields(&text)?;
    expect_field(&fields, "namespace", CACHE_NAMESPACE)?;
    expect_field(&fields, "impl", IMPLEMENTATION_TAG)?;
    expect_field(&fields, "compiler_cache_id", COMPILER_CACHE_ID)?;
    expect_field(&fields, "target", &key.target)?;
    expect_field(&fields, "target_hash", &key.target_hash)?;
    expect_field(&fields, "input", &key.input_hash)?;
    expect_field(&fields, "key", &key.hex)?;
    expect_field(&fields, "tree", &tree_digest(tree)?)?;
    Ok(())
}

fn encode_manifest(key: &ArtifactCacheKey, tree_digest: &str) -> String {
    format!(
        "KIO-ARTIFACT-CACHE namespace={} impl={} compiler={} features={} compiler_cache_id={} target={} target_hash={} input={} key={} tree={}\n",
        CACHE_NAMESPACE,
        IMPLEMENTATION_TAG,
        COMPILER_VERSION,
        FEATURE_SET,
        COMPILER_CACHE_ID,
        key.target,
        key.target_hash,
        key.input_hash,
        key.hex,
        tree_digest,
    )
}

fn tree_digest(root: &Path) -> Result<String, String> {
    let mut hasher = blake3::Hasher::new();
    write_framed(&mut hasher, b"artifact-tree-v1");
    hash_tree_dir(root, &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}

fn hash_tree_dir(dir: &Path, hasher: &mut blake3::Hasher) -> Result<(), String> {
    let mut entries = fs::read_dir(dir)
        .map_err(|e| format!("read `{}`: {e}", DisplayPath(dir)))?
        .map(|entry| {
            let entry = entry.map_err(|e| format!("read `{}`: {e}", DisplayPath(dir)))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| format!("non-UTF-8 artifact name below `{}`", DisplayPath(dir)))?;
            Ok((name, entry))
        })
        .collect::<Result<Vec<_>, String>>()?;
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));

    for (name, entry) in entries {
        let path = entry.path();
        let ty = entry
            .file_type()
            .map_err(|e| format!("stat `{}`: {e}", DisplayPath(&path)))?;
        if ty.is_dir() {
            hasher.update(b"D");
            write_framed(hasher, name.as_bytes());
            hash_tree_dir(&path, hasher)?;
            hasher.update(b"E");
        } else if ty.is_file() {
            hasher.update(b"F");
            write_framed(hasher, name.as_bytes());
            let mut file =
                fs::File::open(&path).map_err(|e| format!("open `{}`: {e}", DisplayPath(&path)))?;
            let expected_len = file
                .metadata()
                .map_err(|e| format!("stat `{}`: {e}", DisplayPath(&path)))?
                .len();
            hasher.update(&expected_len.to_le_bytes());
            let mut actual_len = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = file
                    .read(&mut buffer)
                    .map_err(|e| format!("read `{}`: {e}", DisplayPath(&path)))?;
                if read == 0 {
                    break;
                }
                actual_len += read as u64;
                hasher.update(&buffer[..read]);
            }
            if actual_len != expected_len {
                return Err(format!(
                    "artifact file `{}` changed while hashing",
                    DisplayPath(&path)
                ));
            }
        } else {
            return Err(format!(
                "unsupported artifact entry `{}`",
                DisplayPath(&path)
            ));
        }
    }
    Ok(())
}

fn parse_manifest_fields(text: &str) -> Result<HashMap<String, String>, String> {
    let line = text
        .lines()
        .next()
        .ok_or_else(|| "empty artifact cache manifest".to_owned())?;
    let rest = line
        .strip_prefix("KIO-ARTIFACT-CACHE ")
        .ok_or_else(|| format!("bad artifact cache manifest magic: `{line}`"))?;
    let mut fields = HashMap::new();
    for tok in rest.split(' ') {
        let Some((k, v)) = tok.split_once('=') else {
            return Err(format!("bad artifact cache manifest token `{tok}`"));
        };
        fields.insert(k.to_owned(), v.to_owned());
    }
    Ok(fields)
}

fn expect_field(
    fields: &HashMap<String, String>,
    name: &str,
    expected: &str,
) -> Result<(), String> {
    let actual = fields
        .get(name)
        .ok_or_else(|| format!("missing artifact cache manifest field `{name}`"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "artifact cache manifest field `{name}` is `{actual}`, expected `{expected}`"
        ))
    }
}

fn entry_dir(root: &Path, key: &ArtifactCacheKey) -> PathBuf {
    root.join(&key.target).join(&key.hex)
}

fn temp_entry_dir(root: &Path, key: &ArtifactCacheKey) -> PathBuf {
    let n = TEMPDIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    root.join(&key.target)
        .join(format!("{}.tmp.{}.{}", key.hex, std::process::id(), n))
}

pub fn resolve_from_cache_field(
    workspace_root: &Path,
    cache: &crate::ast::BuildBlockCache,
) -> ArtifactCache {
    match cache {
        crate::ast::BuildBlockCache::Path { path, .. } => {
            let abs = absolutize(workspace_root, path);
            ArtifactCache::open(abs).unwrap_or_else(|_| ArtifactCache::disabled())
        }
        crate::ast::BuildBlockCache::Disabled { .. } => ArtifactCache::disabled(),
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

fn log_probe(status: &str, key: &ArtifactCacheKey, detail: &str) {
    if std::env::var("KIO_DEBUG_ARTIFACT_CACHE").ok().as_deref() != Some("1") {
        return;
    }
    if detail.is_empty() {
        eprintln!("artifact-cache: {status} {} {}", key.target, key.hex);
    } else {
        eprintln!(
            "artifact-cache: {status} {} {} ({detail})",
            key.target, key.hex
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::keys::{
        ArtifactInputFingerprint, ArtifactTargetProfileFingerprint, CacheTarget,
    };

    fn key() -> ArtifactCacheKey {
        ArtifactCacheKey::new(
            CacheTarget::new("js"),
            ArtifactTargetProfileFingerprint::from_bytes(b"profile"),
            ArtifactInputFingerprint::from_parts(&[b"input"]),
        )
    }

    fn key_with_input(input: &[u8]) -> ArtifactCacheKey {
        ArtifactCacheKey::new(
            CacheTarget::new("js"),
            ArtifactTargetProfileFingerprint::from_bytes(b"profile"),
            ArtifactInputFingerprint::from_parts(&[input]),
        )
    }

    fn target_with_output(root: &Path, name: &str, output: &str) -> PathBuf {
        let target = root.join(name);
        fs::create_dir_all(&target).expect("create target");
        fs::write(target.join("pkg.js"), output).expect("write target output");
        target
    }

    fn assert_no_tempdirs(root: &Path, context: &str) {
        let target_root = root.join("js");
        let leftovers = fs::read_dir(&target_root)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp."))
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty(), "{context}: {leftovers:?}");
    }

    #[test]
    fn artifact_roundtrips_under_unversioned_target_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = ArtifactCache::open(dir.path().join("cache")).expect("open cache");
        let key = key();
        let target = dir.path().join("target");
        fs::create_dir_all(&target).expect("create target");
        fs::write(target.join("pkg.js"), "output").expect("write target file");

        cache.store(&key, &target);

        let Backend::Active { root, .. } = &cache.inner else {
            unreachable!();
        };
        assert_eq!(root, &dir.path().join("cache").join("artifacts"));
        assert!(entry_dir(root, &key).join("tree").is_dir());

        let restored = dir.path().join("restored");
        assert!(cache.restore(&key, &restored));
        assert_eq!(
            fs::read_to_string(restored.join("pkg.js")).expect("read restored file"),
            "output"
        );
    }

    #[test]
    fn concurrent_same_key_publication_is_idempotent() {
        const WRITERS: usize = 2;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cache/artifacts");
        let target = target_with_output(dir.path(), "target", "output");
        let key = key();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(WRITERS + 1));
        let results = std::thread::scope(|scope| {
            let handles = (0..WRITERS)
                .map(|_| {
                    let barrier = barrier.clone();
                    let root = root.clone();
                    let target = target.clone();
                    let key = key.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        write_entry(&root, &key, &target)
                    })
                })
                .collect::<Vec<_>>();
            barrier.wait();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("writer thread"))
                .collect::<Vec<_>>()
        });

        assert!(results.iter().all(Result::is_ok), "{results:?}");
        validate_entry_dir(&entry_dir(&root, &key), &key).expect("valid published entry");
        assert_no_tempdirs(&root, "same-key publication");
    }

    #[test]
    fn nonreplacing_rename_accepts_a_valid_peer_publication() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cache/artifacts");
        let target = target_with_output(dir.path(), "target", "output");
        let key = key();
        let attempts = std::cell::Cell::new(0);

        write_entry_with_rename(&root, &key, &target, |from, to| {
            attempts.set(attempts.get() + 1);
            fs::create_dir_all(to)?;
            copy_dir_contents(from, to).map_err(std::io::Error::other)?;
            Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
        })
        .expect("accept valid peer");

        assert_eq!(attempts.get(), 1);
        validate_entry_dir(&entry_dir(&root, &key), &key).expect("valid peer entry");
        assert_no_tempdirs(&root, "accepting a valid peer entry");
    }

    #[test]
    fn corrupt_destination_repair_accepts_a_peer_winning_the_retry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cache/artifacts");
        let target = target_with_output(dir.path(), "target", "output");
        let key = key();
        let final_dir = entry_dir(&root, &key);
        fs::create_dir_all(&final_dir).expect("create corrupt destination");
        fs::write(final_dir.join("manifest.txt"), "corrupt").expect("write corruption");
        let attempts = std::cell::Cell::new(0);

        write_entry_with_rename(&root, &key, &target, |from, to| {
            let attempt = attempts.get();
            attempts.set(attempt + 1);
            if attempt == 1 {
                fs::create_dir_all(to)?;
                copy_dir_contents(from, to).map_err(std::io::Error::other)?;
            }
            Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
        })
        .expect("accept peer winning retry");

        assert_eq!(attempts.get(), 2);
        validate_entry_dir(&final_dir, &key).expect("valid peer entry");
        assert_no_tempdirs(&root, "accepting peer that won repair retry");
    }

    #[test]
    fn failed_repair_cleans_its_temporary_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cache/artifacts");
        let target = target_with_output(dir.path(), "target", "output");
        let key = key();
        let final_dir = entry_dir(&root, &key);
        fs::create_dir_all(&final_dir).expect("create corrupt destination");
        fs::write(final_dir.join("manifest.txt"), "corrupt").expect("write corruption");

        let error = write_entry_with_rename(&root, &key, &target, |_, _| {
            Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
        })
        .expect_err("both publication attempts must fail");

        assert!(error.contains("retry"), "{error}");
        assert_no_tempdirs(&root, "failed repair");
    }

    #[test]
    fn repair_retry_rejects_an_invalid_peer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cache/artifacts");
        let target = target_with_output(dir.path(), "target", "output");
        let key = key();
        let final_dir = entry_dir(&root, &key);
        fs::create_dir_all(&final_dir).expect("create corrupt destination");
        fs::write(final_dir.join("manifest.txt"), "corrupt").expect("write corruption");
        let attempts = std::cell::Cell::new(0);

        let error = write_entry_with_rename(&root, &key, &target, |_, to| {
            let attempt = attempts.get();
            attempts.set(attempt + 1);
            if attempt == 1 {
                fs::create_dir_all(to)?;
                fs::write(to.join("manifest.txt"), "invalid peer")?;
            }
            Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
        })
        .expect_err("invalid peer must not be accepted");

        assert_eq!(attempts.get(), 2);
        assert!(error.contains("retry"), "{error}");
        assert!(validate_entry_dir(&final_dir, &key).is_err());
        assert_no_tempdirs(&root, "rejecting invalid repair peer");
    }

    #[test]
    fn store_repairs_a_non_directory_destination() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = ArtifactCache::open(dir.path().join("cache")).expect("open cache");
        let key = key();
        let target = target_with_output(dir.path(), "target", "output");
        let Backend::Active { root, .. } = &cache.inner else {
            unreachable!();
        };
        let final_dir = entry_dir(root, &key);
        fs::create_dir_all(final_dir.parent().unwrap()).expect("create entry parent");
        fs::write(&final_dir, "not a directory").expect("write invalid destination");

        cache.store(&key, &target);

        let restored = dir.path().join("restored");
        assert!(cache.restore(&key, &restored));
        assert_eq!(
            fs::read_to_string(restored.join("pkg.js")).unwrap(),
            "output"
        );
        assert_no_tempdirs(root, "repairing non-directory destination");
    }

    #[test]
    fn wrong_compiler_cache_id_misses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = ArtifactCache::open(dir.path().join("cache")).expect("open cache");
        let key = key();
        let target = dir.path().join("target");
        fs::create_dir_all(&target).expect("create target");
        fs::write(target.join("pkg.js"), "output").expect("write target file");
        cache.store(&key, &target);

        let Backend::Active { root, .. } = &cache.inner else {
            unreachable!();
        };
        let manifest = entry_dir(root, &key).join("manifest.txt");
        let mut bytes = fs::read(&manifest).expect("read manifest");
        let needle = format!("compiler_cache_id={COMPILER_CACHE_ID}").into_bytes();
        let replacement = b"compiler_cache_id=bad-cache-id";
        let start = bytes
            .windows(needle.len())
            .position(|window| window == needle.as_slice())
            .expect("compiler cache id in manifest");
        bytes.splice(start..start + needle.len(), replacement.iter().copied());
        fs::write(&manifest, bytes).expect("mutate manifest");

        let restored = dir.path().join("restored");
        assert!(!cache.restore(&key, &restored));
    }

    #[test]
    fn exact_key_rejects_a_swapped_valid_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = ArtifactCache::open(dir.path().join("cache")).expect("open cache");
        let first_key = key_with_input(b"first");
        let second_key = key_with_input(b"second");
        let first_target = dir.path().join("first-target");
        let second_target = dir.path().join("second-target");
        fs::create_dir_all(&first_target).expect("create first target");
        fs::create_dir_all(&second_target).expect("create second target");
        fs::write(first_target.join("pkg.js"), "first").expect("write first target");
        fs::write(second_target.join("pkg.js"), "second").expect("write second target");
        cache.store(&first_key, &first_target);
        cache.store(&second_key, &second_target);
        let Backend::Active { root, .. } = &cache.inner else {
            unreachable!();
        };
        let second_dir = entry_dir(root, &second_key);
        fs::remove_dir_all(&second_dir).expect("remove second entry");
        fs::create_dir_all(&second_dir).expect("recreate second entry directory");
        copy_dir_contents(&entry_dir(root, &first_key), &second_dir).expect("swap entry");

        let restored = dir.path().join("restored");
        assert!(!cache.restore(&second_key, &restored));

        cache.store(&second_key, &second_target);
        assert!(cache.restore(&second_key, &restored));
        assert_eq!(
            fs::read_to_string(restored.join("pkg.js")).expect("read repaired output"),
            "second"
        );
    }

    #[test]
    fn tree_digest_rejects_mutated_cached_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = ArtifactCache::open(dir.path().join("cache")).expect("open cache");
        let key = key();
        let target = dir.path().join("target");
        fs::create_dir_all(&target).expect("create target");
        fs::write(target.join("pkg.js"), "output").expect("write target file");
        cache.store(&key, &target);
        let Backend::Active { root, .. } = &cache.inner else {
            unreachable!();
        };
        fs::write(
            entry_dir(root, &key).join("tree/pkg.js"),
            "different output",
        )
        .expect("mutate cached output");

        let restored = dir.path().join("restored");
        assert!(!cache.restore(&key, &restored));
        assert!(!restored.exists());

        cache.store(&key, &target);
        assert!(cache.restore(&key, &restored));
        assert_eq!(
            fs::read_to_string(restored.join("pkg.js")).expect("read repaired output"),
            "output"
        );
    }

    #[test]
    fn tree_digest_is_independent_of_creation_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        fs::create_dir_all(first.join("nested")).expect("create first tree");
        fs::write(first.join("z.txt"), "z").expect("write first z");
        fs::write(first.join("nested/a.txt"), "a").expect("write first a");
        fs::create_dir_all(second.join("nested")).expect("create second tree");
        fs::write(second.join("nested/a.txt"), "a").expect("write second a");
        fs::write(second.join("z.txt"), "z").expect("write second z");

        assert_eq!(tree_digest(&first).unwrap(), tree_digest(&second).unwrap());
    }

    #[test]
    fn tree_digest_distinguishes_renames_nesting_and_empty_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let flat = dir.path().join("flat");
        let renamed = dir.path().join("renamed");
        let nested = dir.path().join("nested");
        let with_empty = dir.path().join("with-empty");
        fs::create_dir_all(&flat).expect("create flat tree");
        fs::write(flat.join("a.txt"), "same").expect("write flat file");
        fs::create_dir_all(&renamed).expect("create renamed tree");
        fs::write(renamed.join("b.txt"), "same").expect("write renamed file");
        fs::create_dir_all(nested.join("dir")).expect("create nested tree");
        fs::write(nested.join("dir/a.txt"), "same").expect("write nested file");
        fs::create_dir_all(with_empty.join("empty")).expect("create empty directory");
        fs::write(with_empty.join("a.txt"), "same").expect("write empty-dir tree file");

        let flat_digest = tree_digest(&flat).unwrap();
        assert_ne!(flat_digest, tree_digest(&renamed).unwrap());
        assert_ne!(flat_digest, tree_digest(&nested).unwrap());
        assert_ne!(flat_digest, tree_digest(&with_empty).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn copy_dir_contents_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::create_dir_all(&from).expect("create source");
        fs::create_dir_all(&to).expect("create destination");
        fs::write(from.join("target"), "content").expect("write target");
        symlink("target", from.join("link")).expect("create symlink");

        let error = copy_dir_contents(&from, &to).expect_err("symlink must be rejected");
        assert!(error.contains("unsupported artifact entry"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn copy_dir_contents_rejects_special_files() {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        fs::create_dir_all(&from).expect("create source");
        fs::create_dir_all(&to).expect("create destination");
        let _listener = UnixListener::bind(from.join("socket")).expect("create socket");

        let error = copy_dir_contents(&from, &to).expect_err("socket must be rejected");
        assert!(error.contains("unsupported artifact entry"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn rejected_source_entry_cleans_the_temporary_directory() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cache/artifacts");
        let target = target_with_output(dir.path(), "target", "output");
        let key = key();
        symlink("pkg.js", target.join("link")).expect("create symlink");

        let error = write_entry(&root, &key, &target).expect_err("symlink must reject store");

        assert!(error.contains("unsupported artifact entry"), "{error}");
        assert!(!entry_dir(&root, &key).exists());
        assert_no_tempdirs(&root, "rejected source entry");
    }
}
