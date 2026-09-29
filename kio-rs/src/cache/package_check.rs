//! Package-check cache for `kio check`.
//!
//! One package-check entry is the complete proof that a package can
//! skip its post-parse frontend work on a later run. The header
//! carries the current package source fingerprint and an exact body
//! digest; the body carries the lowered package-file summary needed
//! when this package is skipped.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use blake3::Hasher;

use crate::ast::Phase;
use crate::cache::identity::{
    COMPILER_CACHE_ID, COMPILER_VERSION, FEATURE_SET, IMPLEMENTATION_TAG,
};
use crate::cache::keys::{PackageName, PipelineTag, SourceHash, SurfaceFingerprint};
use crate::pass::resolve::PackageFileEntry;
use crate::path_display::DisplayPath;

pub const CACHE_NAMESPACE: &str = "package-check";

static TEMPFILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct PackageCheckCache {
    inner: Backend,
}

#[derive(Debug, Clone)]
enum Backend {
    Active {
        cache_root: PathBuf,
        root: PathBuf,
        may_have_entries: Arc<AtomicBool>,
    },
    Disabled,
}

impl PackageCheckCache {
    pub fn open(cache_root: PathBuf) -> std::io::Result<Self> {
        let root = cache_root.join("package-check");
        let may_have_entries = cache_tree_has_entries(&root);
        fs::create_dir_all(&root)?;
        write_gitignore_if_absent(&cache_root)?;
        crate::package_collection::mark_generated_dir(&cache_root);
        crate::cache::gc::record_cache_open(
            &cache_root,
            crate::cache::gc::CacheFamily::PackageCheck,
        );
        Ok(Self {
            inner: Backend::Active {
                cache_root,
                root,
                may_have_entries: Arc::new(AtomicBool::new(may_have_entries)),
            },
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

    pub fn entry_path(&self, key: &PackageCheckCacheKey) -> Option<PathBuf> {
        match &self.inner {
            Backend::Active { root, .. } => Some(entry_path(root, key)),
            Backend::Disabled => None,
        }
    }

    pub fn lookup<P>(
        &self,
        key: &PackageCheckCacheKey,
    ) -> Result<Option<PackageCheckEntry<P>>, String>
    where
        P: Phase,
        PackageCheckEntry<P>: serde::de::DeserializeOwned,
    {
        match &self.inner {
            Backend::Disabled => Ok(None),
            Backend::Active {
                cache_root,
                root,
                may_have_entries,
            } => {
                if !may_have_entries.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                let entry = read_entry(root, key)?;
                if entry.is_some() {
                    crate::cache::gc::record_entry_path_access(
                        cache_root,
                        crate::cache::gc::CacheFamily::PackageCheck,
                        root,
                        &entry_path(root, key),
                    );
                }
                Ok(entry)
            }
        }
    }

    pub fn store<P>(
        &self,
        key: &PackageCheckCacheKey,
        entry: &PackageCheckEntry<P>,
    ) -> Result<(), String>
    where
        P: Phase,
        PackageCheckEntry<P>: serde::Serialize + serde::de::DeserializeOwned,
    {
        match &self.inner {
            Backend::Disabled => Ok(()),
            Backend::Active {
                cache_root,
                root,
                may_have_entries,
            } => {
                write_entry(root, key, entry)?;
                crate::cache::gc::record_entry_path_access(
                    cache_root,
                    crate::cache::gc::CacheFamily::PackageCheck,
                    root,
                    &entry_path(root, key),
                );
                may_have_entries.store(true, Ordering::Relaxed);
                Ok(())
            }
        }
    }
}

fn cache_tree_has_entries(root: &Path) -> bool {
    let Ok(mut package_dirs) = fs::read_dir(root) else {
        return false;
    };
    package_dirs.any(|entry| {
        let Ok(entry) = entry else {
            return false;
        };
        let path = entry.path();
        if path.is_file() {
            return true;
        }
        path.is_dir()
            && fs::read_dir(path)
                .map(|mut entries| entries.next().is_some())
                .unwrap_or(false)
    })
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(bound(
    serialize = "Option<PackageFileEntry<P>>: serde::Serialize",
    deserialize = "Option<PackageFileEntry<P>>: serde::Deserialize<'de>"
))]
pub struct PackageCheckEntry<P: Phase> {
    pub package_file: Option<PackageFileEntry<P>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageCheckCacheKey {
    package_name: PackageName,
    pipeline_tag: PipelineTag,
    source_hash: SourceHash,
    hex: String,
}

impl PackageCheckCacheKey {
    pub fn new(
        package_name: PackageName,
        pipeline_tag: PipelineTag,
        source_hash: SourceHash,
    ) -> Self {
        let mut h = Hasher::new();
        write_framed(&mut h, CACHE_NAMESPACE.as_bytes());
        write_framed(&mut h, IMPLEMENTATION_TAG.as_bytes());
        write_framed(&mut h, COMPILER_CACHE_ID.as_bytes());
        write_framed(&mut h, pipeline_tag.as_str().as_bytes());
        write_framed(&mut h, package_name.as_str().as_bytes());
        write_framed(&mut h, source_hash.as_str().as_bytes());
        Self {
            package_name,
            pipeline_tag,
            source_hash,
            hex: h.finalize().to_hex().to_string(),
        }
    }

    pub fn hex(&self) -> &str {
        &self.hex
    }

    pub fn package_name(&self) -> &PackageName {
        &self.package_name
    }
}

pub fn source_hash(sources: &BTreeMap<PathBuf, String>) -> SourceHash {
    let mut h = Hasher::new();
    for (path, content) in sources {
        write_framed(&mut h, path.to_string_lossy().as_bytes());
        write_framed(&mut h, content.as_bytes());
    }
    SourceHash::new(h.finalize().to_hex().to_string())
}

pub fn fingerprint_parts<'a, I>(parts: I) -> SurfaceFingerprint
where
    I: IntoIterator<Item = &'a str>,
{
    let mut h = Hasher::new();
    for part in parts {
        write_framed(&mut h, part.as_bytes());
    }
    SurfaceFingerprint::new(h.finalize().to_hex().to_string())
}

pub fn resolve_cache_root(package_root: &Path, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        package_root.join(p)
    }
}

pub fn cache_path(
    cache_root: &Path,
    package_name: &PackageName,
    pipeline_tag: PipelineTag,
) -> PathBuf {
    cache_root
        .join("package-check")
        .join(sanitize_path_component(package_name.as_str()))
        .join(format!("{}.bin", pipeline_file_tag(pipeline_tag)))
}

fn read_entry<P>(
    root: &Path,
    key: &PackageCheckCacheKey,
) -> Result<Option<PackageCheckEntry<P>>, String>
where
    P: Phase,
    PackageCheckEntry<P>: serde::de::DeserializeOwned,
{
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
    let entry = postcard::from_bytes(body)
        .map_err(|e| format!("stale {}: postcard decode failed: {e}", DisplayPath(&path)))?;
    Ok(Some(entry))
}

fn write_entry<P>(
    root: &Path,
    key: &PackageCheckCacheKey,
    entry: &PackageCheckEntry<P>,
) -> Result<(), String>
where
    P: Phase,
    PackageCheckEntry<P>: serde::Serialize + serde::de::DeserializeOwned,
{
    fs::create_dir_all(entry_dir(root, key))
        .map_err(|e| format!("mkdir {}: {e}", DisplayPath(&entry_dir(root, key))))?;
    let final_path = entry_path(root, key);
    let tmp_path = tempfile_path(root, key);
    let body = postcard::to_allocvec(entry).map_err(|e| format!("postcard encode failed: {e}"))?;
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| format!("create {}: {e}", DisplayPath(&tmp_path)))?;
        f.write_all(encode_header(key, &body).as_bytes())
            .map_err(|e| format!("write {}: {e}", DisplayPath(&tmp_path)))?;
        f.write_all(&body)
            .map_err(|e| format!("write {}: {e}", DisplayPath(&tmp_path)))?;
    }
    crate::cache::publish_temp_file(&tmp_path, &final_path, || {
        read_entry::<P>(root, key).ok().flatten().is_some()
    })
}

fn entry_dir(root: &Path, key: &PackageCheckCacheKey) -> PathBuf {
    root.join(sanitize_path_component(key.package_name.as_str()))
}

fn entry_path(root: &Path, key: &PackageCheckCacheKey) -> PathBuf {
    entry_dir(root, key).join(format!("{}.bin", pipeline_file_tag(key.pipeline_tag)))
}

fn tempfile_path(root: &Path, key: &PackageCheckCacheKey) -> PathBuf {
    let pid = std::process::id();
    let counter = TEMPFILE_COUNTER.fetch_add(1, Ordering::SeqCst);
    entry_dir(root, key).join(format!(
        "{}.bin.tmp.{pid}-{counter}",
        pipeline_file_tag(key.pipeline_tag)
    ))
}

fn pipeline_file_tag(pipeline_tag: PipelineTag) -> String {
    if pipeline_tag.as_str().is_empty() {
        "full".to_owned()
    } else {
        pipeline_tag.as_str().trim_matches('.').replace('.', "_")
    }
}

fn sanitize_path_component(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => c,
            _ => '_',
        })
        .collect()
}

fn encode_header(key: &PackageCheckCacheKey, body: &[u8]) -> String {
    format!(
        "KIO-PACKAGE-CHECK-CACHE namespace={namespace} impl={impl_tag} \
         compiler={compiler} features={features} compiler_cache_id={compiler_cache_id} \
         pipeline={pipeline} package={package} source={source} key={hex} body={body_digest}\n",
        namespace = CACHE_NAMESPACE,
        impl_tag = IMPLEMENTATION_TAG,
        compiler = COMPILER_VERSION,
        features = FEATURE_SET,
        compiler_cache_id = COMPILER_CACHE_ID,
        pipeline = key.pipeline_tag.as_str(),
        package = key.package_name.as_str(),
        source = key.source_hash,
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
        .strip_prefix("KIO-PACKAGE-CHECK-CACHE ")
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
    expected: &PackageCheckCacheKey,
    body: &[u8],
) -> Result<(), String> {
    expect_field(fields, "namespace", CACHE_NAMESPACE)?;
    expect_field(fields, "impl", IMPLEMENTATION_TAG)?;
    expect_field(fields, "compiler_cache_id", COMPILER_CACHE_ID)?;
    expect_field(fields, "pipeline", expected.pipeline_tag.as_str())?;
    expect_field(fields, "package", expected.package_name.as_str())?;
    expect_field(fields, "source", expected.source_hash.as_str())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Prime;

    fn key() -> PackageCheckCacheKey {
        key_with_source("source")
    }

    fn key_with_source(source_hash: &str) -> PackageCheckCacheKey {
        PackageCheckCacheKey::new(
            PackageName::new("pkg"),
            PipelineTag::new(""),
            SourceHash::new(source_hash),
        )
    }

    #[test]
    fn source_hash_changes_with_content_and_path() {
        let mut base = BTreeMap::new();
        base.insert(PathBuf::from("a.kio"), "module a;".to_owned());
        let h_base = source_hash(&base);
        let mut renamed = BTreeMap::new();
        renamed.insert(PathBuf::from("b.kio"), "module a;".to_owned());
        assert_ne!(h_base, source_hash(&renamed));
        let mut edited = BTreeMap::new();
        edited.insert(PathBuf::from("a.kio"), "module a; ".to_owned());
        assert_ne!(h_base, source_hash(&edited));
        let mut same = BTreeMap::new();
        same.insert(PathBuf::from("a.kio"), "module a;".to_owned());
        assert_eq!(h_base, source_hash(&same));
    }

    #[test]
    fn source_hash_is_order_independent_via_btreemap() {
        let mut m1 = BTreeMap::new();
        m1.insert(PathBuf::from("z.kio"), "z".to_owned());
        m1.insert(PathBuf::from("a.kio"), "a".to_owned());
        let mut m2 = BTreeMap::new();
        m2.insert(PathBuf::from("a.kio"), "a".to_owned());
        m2.insert(PathBuf::from("z.kio"), "z".to_owned());
        assert_eq!(source_hash(&m1), source_hash(&m2));
    }

    #[test]
    fn active_cache_roundtrips_empty_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = PackageCheckCache::open(dir.path().join("cache")).expect("open cache");
        let key = key();
        let entry = PackageCheckEntry::<Prime> { package_file: None };
        cache.store(&key, &entry).expect("store");
        let loaded = cache.lookup::<Prime>(&key).expect("lookup").expect("hit");
        assert!(loaded.package_file.is_none());
    }

    #[test]
    fn stable_path_key_mismatch_misses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = PackageCheckCache::open(dir.path().join("cache")).expect("open cache");
        let old_key = key_with_source("source");
        let new_key = key_with_source("changed-source");
        let entry = PackageCheckEntry::<Prime> { package_file: None };
        cache.store(&old_key, &entry).expect("store");
        assert!(cache.lookup::<Prime>(&new_key).ok().flatten().is_none());
        cache.store(&new_key, &entry).expect("repair stale entry");
        assert!(
            cache
                .lookup::<Prime>(&new_key)
                .expect("lookup repaired entry")
                .is_some()
        );
    }

    #[test]
    fn body_digest_rejects_a_different_valid_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache_root = dir.path().join("cache");
        let cache = PackageCheckCache::open(cache_root).expect("open cache");
        let first_key = key_with_source("first-source");
        let second_key = key_with_source("second-source");
        let first_entry = PackageCheckEntry::<Prime> { package_file: None };
        let second_entry = PackageCheckEntry::<Prime> {
            package_file: Some(PackageFileEntry {
                file_path: PathBuf::from("pkg.pkg.kio"),
                package_name: "pkg".to_owned(),
                package_file: crate::ast::PackageFile {
                    name: "pkg".to_owned(),
                    build: None,
                    bridge: None,
                    meta: crate::ast::Meta::new(crate::span::Span::new(0, 3)),
                },
            }),
        };
        cache.store(&first_key, &first_entry).expect("store first");
        let first_path = cache.entry_path(&first_key).expect("active cache path");
        let first_bytes = fs::read(&first_path).expect("read first entry");
        cache
            .store(&second_key, &second_entry)
            .expect("store second");
        let second_path = cache.entry_path(&second_key).expect("active cache path");
        let second_bytes = fs::read(&second_path).expect("read second entry");
        let first_header_end = first_bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        let second_header_end = second_bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        let mut swapped = first_bytes[..first_header_end].to_vec();
        swapped.extend_from_slice(&second_bytes[second_header_end..]);
        fs::write(&first_path, swapped).expect("swap valid body");

        assert!(cache.lookup::<Prime>(&first_key).is_err());
        cache
            .store(&first_key, &first_entry)
            .expect("repair corrupt entry");
        let repaired = cache
            .lookup::<Prime>(&first_key)
            .expect("lookup repaired entry")
            .expect("repaired hit");
        assert!(repaired.package_file.is_none());
    }

    #[test]
    fn cache_path_uses_package_dir_and_pipeline_tag() {
        assert_eq!(
            cache_path(
                Path::new("out/.kio-cache"),
                &PackageName::new("pkg"),
                PipelineTag::new("")
            ),
            PathBuf::from("out/.kio-cache/package-check/pkg/full.bin")
        );
        assert_eq!(
            cache_path(
                Path::new("out/.kio-cache"),
                &PackageName::new("pkg"),
                PipelineTag::new("prime.")
            ),
            PathBuf::from("out/.kio-cache/package-check/pkg/prime.bin")
        );
    }
}
