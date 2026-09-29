//! Unit tests for the shared build cache (`src/shared/build_cache/`).
//!
//! Coverage map:
//!
//! - Keying determinism + per-field invalidation + length-prefix
//!   framing + cross-variant disambiguation + domain separation.
//! - The structural-tree file walk's sort-stability.
//! - Atomic-rename publication, miss-on-empty-artifact.
//! - Locking primitives — same-process concurrent acquire via
//!   `acquire_lock` returns one writer at a time.
//! - Schema versioning: lookups against `v1/` ignore a planted
//!   `v0/` tree.
//! - Tempdir orphan reap with mtime windows.
//! - LRU prune over the structural entry walk (two-level `rlibs`/
//!   `bins` layout and a one-level `bin` layout), with protection.
//! - `get_or_produce` end-to-end: a hit skips the produce closure, a
//!   miss runs it and publishes, a chained sub-key threads one
//!   artifact's key into the next.
//!
//! Tests that would invoke a real compiler live with the per-backend
//! runner and its goldens; the unit tests here stub the produce
//! closure or plant artifact bytes directly so they're hermetic and
//! fast.

use crate::compiler_admission::CompilerAdmission;
use crate::path_display::DisplayPath;
use std::cell::Cell;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use super::io as cache_io;
use super::key::{self, InputField};
use super::{ArtifactRequest, BuildCache, ProduceCtx, is_regular_file_nonempty, prune_lru_cache};

/// Allocate a fresh empty directory under the OS temp dir. Each
/// test gets its own root so parallel `cargo test` doesn't trip.
fn fresh_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("kio-build-cache-test-{label}-{pid}-{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Open a plain cache (no wrapper, unbounded) rooted at `root`.
fn open_cache(root: PathBuf) -> BuildCache {
    BuildCache::open(root, None, None, CompilerAdmission::disabled()).expect("open")
}

/// The Rust adapter's rlib field list, reproduced here so the keying
/// tests exercise the same `(identity, triple, edition, profile,
/// crate-name, files)` shape the runner builds.
fn rlib_fields(crate_name: &str, files: &[(&str, &[u8])]) -> Vec<InputField> {
    vec![
        InputField::Str("rustc 1.85.0\ncommit-hash:abcd\nhost:x86_64-unknown-linux-gnu".into()),
        InputField::Str("x86_64-unknown-linux-gnu".into()),
        InputField::Str("2024".into()),
        InputField::Str("release".into()),
        InputField::Str(crate_name.into()),
        InputField::Files(
            files
                .iter()
                .map(|(p, b)| (PathBuf::from(p), b.to_vec()))
                .collect(),
        ),
    ]
}

fn rlib_key(crate_name: &str, files: &[(&str, &[u8])]) -> String {
    key::hash_fields(None, &rlib_fields(crate_name, files))
}

// =========================================================================
// Keying
// =========================================================================

#[test]
fn key_is_deterministic() {
    let a = rlib_key("p", &[("src/lib.rs", b"// hi")]);
    let b = rlib_key("p", &[("src/lib.rs", b"// hi")]);
    assert_eq!(a, b);
    assert_eq!(a.len(), 64, "blake3 hex");
}

#[test]
fn key_changes_on_identity_field() {
    let a = rlib_key("p", &[("src/lib.rs", b"// hi")]);
    let mut fields = rlib_fields("p", &[("src/lib.rs", b"// hi")]);
    fields[0] = InputField::Str("rustc 1.85.0-changed".into());
    let b = key::hash_fields(None, &fields);
    assert_ne!(a, b);
}

#[test]
fn key_changes_on_target_field() {
    let a = rlib_key("p", &[("src/lib.rs", b"// hi")]);
    let mut fields = rlib_fields("p", &[("src/lib.rs", b"// hi")]);
    fields[1] = InputField::Str("aarch64-apple-darwin".into());
    let b = key::hash_fields(None, &fields);
    assert_ne!(a, b);
}

#[test]
fn key_changes_on_profile_field() {
    let a = rlib_key("p", &[("src/lib.rs", b"// hi")]);
    let mut fields = rlib_fields("p", &[("src/lib.rs", b"// hi")]);
    fields[3] = InputField::Str("debug".into());
    let b = key::hash_fields(None, &fields);
    assert_ne!(a, b);
}

#[test]
fn key_changes_on_source_byte() {
    let a = rlib_key("p", &[("src/lib.rs", b"// hi")]);
    let b = rlib_key("p", &[("src/lib.rs", b"// HI")]);
    assert_ne!(a, b);
}

#[test]
fn key_changes_on_file_rename() {
    let a = rlib_key("p", &[("src/lib.rs", b"x")]);
    let b = rlib_key("p", &[("src/main.rs", b"x")]);
    assert_ne!(a, b);
}

#[test]
fn key_length_prefix_disambiguates_concatenation() {
    // `(a, bc)` vs `(ab, c)`: without length-prefix framing both
    // hashes would coincide when the field contents are byte-
    // adjacent. The framing makes them distinct.
    let a = rlib_key("p", &[("src/a", b"bc"), ("src/d", b"")]);
    let b = rlib_key("p", &[("src/ab", b"c"), ("src/d", b"")]);
    assert_ne!(a, b);
}

#[test]
fn key_disambiguates_fields_by_position_and_length() {
    // The key frames by position + length, not by a per-field type
    // tag: moving content across a field boundary changes the hash.
    // `[Str("a"), Str("bc")]` vs `[Str("ab"), Str("c")]` differ because
    // each field is length-prefixed. (A `Str` and a `Bytes` of equal
    // content at the same position *do* coincide — disambiguation is
    // positional, by design, matching the original key derivation.)
    let a = key::hash_fields(
        None,
        &[InputField::Str("a".into()), InputField::Str("bc".into())],
    );
    let b = key::hash_fields(
        None,
        &[InputField::Str("ab".into()), InputField::Str("c".into())],
    );
    assert_ne!(a, b);
}

#[test]
fn key_domain_separator_distinguishes_sibling_keys() {
    // The two-level adapter's bin key passes `Some(b"bin\0")`; with the
    // same field list it must differ from the un-domained key so a
    // driver-only bin can't alias the rlib it links against.
    let fields = vec![InputField::Str("same".into())];
    let plain = key::hash_fields(None, &fields);
    let domained = key::hash_fields(Some(b"bin\0"), &fields);
    assert_ne!(plain, domained);
}

#[test]
fn chained_subkey_threads_through() {
    // A bin field list that folds an rlib key as a `Str` sub-key
    // re-keys when the rlib key changes — the mechanism a two-level
    // adapter relies on.
    let bin = |rlib: &str| {
        key::hash_fields(
            Some(b"bin\0"),
            &[
                InputField::Str(rlib.into()),
                InputField::Bytes(b"fn main() {}".to_vec()),
            ],
        )
    };
    assert_ne!(bin(&"0".repeat(64)), bin(&"1".repeat(64)));
    // Driver byte change re-keys too.
    let a = bin(&"0".repeat(64));
    let b = key::hash_fields(
        Some(b"bin\0"),
        &[
            InputField::Str("0".repeat(64)),
            InputField::Bytes(b"fn main() { /* */ }".to_vec()),
        ],
    );
    assert_ne!(a, b);
}

// `collect_tree_files` is the Rust adapter's `src/`-subtree collector;
// it's gated to the rust feature (the one-level go / haskell adapters
// assemble their heterogeneous `Files` payload directly). Its tests
// gate likewise.
#[cfg(feature = "rust")]
#[test]
fn collect_tree_files_walk_is_sorted() {
    // The walk reads in arbitrary directory order; the sort step makes
    // the key stable. Plant two files and assert the collected list is
    // path-sorted regardless of creation order.
    let root = fresh_dir("walk-sorted");
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("b.rs"), b"b").unwrap();
    fs::write(src.join("a.rs"), b"a").unwrap();
    let files = key::collect_tree_files(&src, "src").expect("walk");
    let paths: Vec<_> = files.iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(
        paths,
        vec![PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")]
    );
    let _ = fs::remove_dir_all(&root);
}

#[cfg(feature = "rust")]
#[test]
fn collect_tree_files_rejects_symlink() {
    #[cfg(unix)]
    {
        let root = fresh_dir("walk-symlink");
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("real.rs"), b"x").unwrap();
        std::os::unix::fs::symlink(src.join("real.rs"), src.join("link.rs")).unwrap();
        let err = key::collect_tree_files(&src, "src").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let _ = fs::remove_dir_all(&root);
    }
}

#[test]
fn id_segment_is_eight_lowercase_hex() {
    let seg = key::id_segment("rustc 1.85.0");
    assert_eq!(seg.len(), 8);
    assert!(
        seg.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
    );
}

// =========================================================================
// I/O
// =========================================================================

#[test]
fn write_gitignore_creates_with_correct_contents() {
    let dir = fresh_dir("gitignore");
    cache_io::write_gitignore(&dir).expect("write");
    let body = fs::read_to_string(dir.join(".gitignore")).expect("read");
    assert!(body.contains("*"));
    assert!(body.contains("!.gitignore"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn write_gitignore_is_idempotent() {
    let dir = fresh_dir("gitignore-idempotent");
    cache_io::write_gitignore(&dir).expect("write 1");
    fs::write(dir.join(".gitignore"), "user-overrode\n").expect("user override");
    cache_io::write_gitignore(&dir).expect("write 2");
    let body = fs::read_to_string(dir.join(".gitignore")).expect("read");
    assert_eq!(body, "user-overrode\n", "second call must not overwrite");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn namespace_dir_creates_artifact_and_tmp() {
    let root = fresh_dir("namespace");
    let sub = PathBuf::from("rustc-deadbeef").join("x86_64-unknown-linux-gnu");
    let nd = cache_io::namespace_dir(&root, "rlib", &sub, "rlibs").expect("namespace");
    assert!(nd.artifacts.is_dir());
    assert!(nd.tmp.is_dir());
    // The artifact namespace sits under the per-(kind, schema, subroot)
    // path; testing it pins the layout.
    assert!(
        nd.artifacts
            .ends_with("rlib/v1/rustc-deadbeef/x86_64-unknown-linux-gnu/rlibs")
    );
    assert!(
        nd.tmp
            .ends_with("rlib/v1/rustc-deadbeef/x86_64-unknown-linux-gnu/tmp")
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn atomic_rename_publishes_artifact() {
    let root = fresh_dir("rename");
    let staged = root.join("staged.rlib");
    fs::write(&staged, b"rlib-bytes").expect("write staged");
    let final_path = root.join("final/lib.rlib");
    cache_io::atomic_rename(&staged, &final_path).expect("rename");
    assert!(!staged.exists());
    assert_eq!(fs::read(&final_path).unwrap(), b"rlib-bytes");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn acquire_lock_creates_lock_file() {
    let root = fresh_dir("lock");
    let key_dir = root.join("rlibs/abc");
    let lock = cache_io::acquire_lock(&key_dir).expect("acquire");
    drop(lock);
    assert!(key_dir.join(".lock").exists());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn reap_orphan_tempdirs_removes_old_entries() {
    let root = fresh_dir("reap");
    let tmp = root.join("tmp");
    fs::create_dir_all(&tmp).unwrap();

    let fresh = tmp.join("fresh-1-rlib-aa");
    let aged = tmp.join("aged-2-rlib-bb");
    fs::create_dir_all(&fresh).unwrap();
    fs::create_dir_all(&aged).unwrap();

    use std::time::{Duration, SystemTime};
    let past = SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
    let times = fs::FileTimes::new().set_modified(past).set_accessed(past);
    let f = {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .access_mode(0x0080 | 0x0100)
                .custom_flags(0x02000000)
                .open(&aged)
                .expect("open aged dir")
        }
        #[cfg(not(windows))]
        {
            fs::OpenOptions::new()
                .read(true)
                .open(&aged)
                .expect("open aged dir")
        }
    };
    f.set_times(times).expect("set times on aged dir");
    drop(f);

    cache_io::reap_orphan_tempdirs(&tmp).expect("reap");
    assert!(fresh.exists(), "fresh tempdir survives");
    assert!(!aged.exists(), "aged tempdir reaped");
    let _ = fs::remove_dir_all(&root);
}

// =========================================================================
// Read path / write path
// =========================================================================

#[test]
fn build_cache_open_writes_gitignore() {
    let root = fresh_dir("open-gitignore");
    let _cache = open_cache(root.clone());
    assert!(root.join(".gitignore").is_file());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn corrupted_entry_reads_as_miss() {
    // The read path treats an empty / symlink / non-regular-file
    // as a miss. Plant a zero-byte artifact and assert the probe fails.
    let root = fresh_dir("corrupted");
    let key_dir = root.join("rlibs/abc");
    fs::create_dir_all(&key_dir).unwrap();
    let rlib = key_dir.join("lib.rlib");
    fs::write(&rlib, b"").unwrap();
    assert!(!is_regular_file_nonempty(&rlib));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn populated_entry_reads_as_hit() {
    let root = fresh_dir("hit");
    let key_dir = root.join("rlibs/abc");
    fs::create_dir_all(&key_dir).unwrap();
    let rlib = key_dir.join("lib.rlib");
    fs::write(&rlib, b"!<arch>\n").unwrap();
    assert!(is_regular_file_nonempty(&rlib));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn lru_prune_removes_oldest_entries_until_under_budget() {
    let root = fresh_dir("lru-prune");
    let old = plant_cache_entry(&root, "rlibs", "old", 10, 60);
    let new = plant_cache_entry(&root, "bins", "new", 20, 1);

    prune_lru_cache(&root, "rlib", 20, &new);

    assert!(!old.exists(), "oldest entry should be pruned");
    assert!(new.exists(), "newer protected entry should remain");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn lru_prune_keeps_protected_entry_even_when_over_budget() {
    let root = fresh_dir("lru-protected");
    let old = plant_cache_entry(&root, "rlibs", "old", 10, 60);
    let protected = plant_cache_entry(&root, "bins", "protected", 30, 1);

    prune_lru_cache(&root, "rlib", 5, &protected);

    assert!(!old.exists(), "unprotected entry should be pruned");
    assert!(protected.exists(), "protected entry should remain");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn lru_prune_walks_one_level_bin_namespace() {
    // A one-level adapter publishes into a single `bin/` namespace. The
    // structural sweep must find and evict those entries too — the
    // entry namespace, not just the two-level `rlibs`/`bins` split, is
    // discovered by walking for a key-dir holding the artifact.
    let root = fresh_dir("lru-onelevel");
    let old = plant_cache_entry(&root, "bin", "old", 10, 60);
    let new = plant_cache_entry(&root, "bin", "new", 20, 1);

    // `plant_cache_entry` plants under the `rlib` cache-kind segment;
    // sweep that same kind. The structural walk is kind-agnostic — what
    // it exercises here is the single-`bin/`-namespace layout.
    prune_lru_cache(&root, "rlib", 20, &new);

    assert!(!old.exists(), "oldest one-level entry should be pruned");
    assert!(
        new.exists(),
        "newer protected one-level entry should remain"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn lru_prune_never_evicts_tmp_staging() {
    // A half-written compile lives under `tmp/`; the sweep must never
    // treat it as a publishable entry, even when over budget.
    let root = fresh_dir("lru-tmp-safe");
    let old = plant_cache_entry(&root, "rlibs", "old", 10, 60);
    let staging = root
        .join("rlib")
        .join(cache_io::CACHE_SCHEMA_VERSION)
        .join("rustc-deadbeef")
        .join("x86_64-unknown-linux-gnu")
        .join("tmp")
        .join("12345-0-rlib-deadbeef");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("libgreeter.rlib"), vec![b'x'; 99]).unwrap();

    prune_lru_cache(&root, "rlib", 1, &PathBuf::from("/nonexistent"));

    assert!(!old.exists(), "published entry pruned");
    assert!(staging.exists(), "tmp staging must survive the sweep");
    let _ = fs::remove_dir_all(&root);
}

fn plant_cache_entry(
    root: &std::path::Path,
    namespace: &str,
    key: &str,
    artifact_bytes: usize,
    age_secs: u64,
) -> PathBuf {
    let dir = root
        .join("rlib")
        .join(cache_io::CACHE_SCHEMA_VERSION)
        .join("rustc-deadbeef")
        .join("x86_64-unknown-linux-gnu")
        .join(namespace)
        .join(key);
    fs::create_dir_all(&dir).unwrap();
    let artifact = match namespace {
        "rlibs" => "lib.rlib",
        _ => "bin",
    };
    fs::write(dir.join(artifact), vec![b'x'; artifact_bytes]).unwrap();
    fs::write(dir.join(".last_used"), b"").unwrap();

    use std::time::{Duration, SystemTime};
    let used = SystemTime::now() - Duration::from_secs(age_secs);
    let times = fs::FileTimes::new().set_modified(used).set_accessed(used);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(dir.join(".last_used"))
        .unwrap();
    file.set_times(times).unwrap();
    dir
}

#[test]
fn schema_v1_segment_is_part_of_namespace_path() {
    // Plant a v0/ tree and assert v1/ lookups don't see it. The
    // assertion is implicit in `namespace_dir` always pinning v1.
    let root = fresh_dir("schema");
    let stale = root.join("rlib/v0/rustc-x/x86_64-unknown-linux-gnu/rlibs/abc/lib.rlib");
    fs::create_dir_all(stale.parent().unwrap()).unwrap();
    fs::write(&stale, b"old").unwrap();
    let sub = PathBuf::from("rustc-x").join("x86_64-unknown-linux-gnu");
    let nd = cache_io::namespace_dir(&root, "rlib", &sub, "rlibs").unwrap();
    // Path-component check rather than substring so the assertion
    // doesn't bake the Unix `/` separator into the comparison.
    assert!(
        nd.artifacts.components().any(|c| c.as_os_str() == "v1"),
        "expected v1 path component in {}",
        DisplayPath(&nd.artifacts)
    );
    assert!(
        !nd.artifacts.join("abc/lib.rlib").exists(),
        "v1 lookup must not consult v0 entries"
    );
    let _ = fs::remove_dir_all(&root);
}

// =========================================================================
// Locking
// =========================================================================

#[test]
fn concurrent_lock_serializes_within_process() {
    // Spawn N threads, each acquiring the lock and bumping a shared
    // counter under it. If `flock` is enforced the observed
    // in-critical-section count is always 1.
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::thread;
    use std::time::Duration;

    let root = fresh_dir("concurrent-lock");
    let key_dir = Arc::new(root.join("rlibs/abc"));
    fs::create_dir_all(&*key_dir).unwrap();

    let in_section = Arc::new(AtomicUsize::new(0));
    let max_overlap = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..4 {
        let dir = Arc::clone(&key_dir);
        let in_section = Arc::clone(&in_section);
        let max_overlap = Arc::clone(&max_overlap);
        handles.push(thread::spawn(move || {
            let lock = cache_io::acquire_lock(&dir).expect("acquire");
            let now = in_section.fetch_add(1, Ordering::SeqCst) + 1;
            max_overlap.fetch_max(now, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(20));
            in_section.fetch_sub(1, Ordering::SeqCst);
            drop(lock);
        }));
    }
    for h in handles {
        h.join().expect("join");
    }
    assert_eq!(max_overlap.load(Ordering::SeqCst), 1);
    let _ = fs::remove_dir_all(&root);
}

// =========================================================================
// get_or_produce end-to-end (stub produce closure)
// =========================================================================

fn stub_request(kind: &str, namespace: &str, fields: Vec<InputField>) -> ArtifactRequest {
    ArtifactRequest {
        kind: kind.to_owned(),
        subroot_rel: PathBuf::from("rustc-deadbeef").join("x86_64-unknown-linux-gnu"),
        namespace: namespace.to_owned(),
        role: "rlib".to_owned(),
        key_inputs: fields,
        hash_domain: None,
        artifact_name: "lib.rlib".to_owned(),
        staged_name: Some("libgreeter.rlib".to_owned()),
        executable: false,
        meta_json: "{\n  \"kind\": \"rlib\"\n}\n".to_owned(),
    }
}

#[test]
fn get_or_produce_miss_then_hit() {
    let root = fresh_dir("produce-miss-hit");
    let cache = open_cache(root.clone());
    let req = stub_request("rlib", "rlibs", rlib_fields("p", &[("src/lib.rs", b"x")]));

    let calls = Cell::new(0);
    let produce = |ctx: &ProduceCtx<'_>| -> Result<(), String> {
        calls.set(calls.get() + 1);
        fs::write(ctx.out_dir.join("libgreeter.rlib"), b"!<arch>\n").map_err(|e| e.to_string())
    };

    let p1 = cache.get_or_produce(&req, produce).expect("first");
    assert!(p1.is_file());
    assert_eq!(fs::read(&p1).unwrap(), b"!<arch>\n");
    // The meta sidecar published next to the artifact.
    assert!(p1.parent().unwrap().join("meta.json").is_file());
    assert_eq!(calls.get(), 1);

    // Second call: a hit; the produce closure must not run again.
    let p2 = cache.get_or_produce(&req, produce).expect("second");
    assert_eq!(p1, p2);
    assert_eq!(calls.get(), 1, "warm hit must skip produce");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn warm_hit_does_not_take_compiler_admission() {
    let root = fresh_dir("admission-hit");
    let request = stub_request("rlib", "rlibs", rlib_fields("p", &[("src/lib.rs", b"x")]));
    open_cache(root.clone())
        .get_or_produce(&request, |ctx| -> Result<(), String> {
            fs::write(ctx.out_dir.join("libgreeter.rlib"), b"!<arch>\n").map_err(|e| e.to_string())
        })
        .expect("seed cache");

    let schedule = fresh_dir("admission-hit-schedule");
    let admission = CompilerAdmission::shared(schedule.clone(), 1).unwrap();
    let occupied = admission.acquire().unwrap();
    let cache = BuildCache::open(root.clone(), None, None, admission).expect("open");
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let result = cache.get_or_produce(&request, |_ctx| -> Result<(), String> {
            panic!("warm hit must not invoke its producer")
        });
        tx.send(result).unwrap();
    });

    let result = match rx.recv_timeout(Duration::from_millis(500)) {
        Ok(result) => result,
        Err(error) => {
            drop(occupied);
            thread.join().unwrap();
            panic!("warm hit waited for compiler admission: {error}");
        }
    };
    result.expect("warm hit");
    drop(occupied);
    thread.join().unwrap();
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(schedule);
}

#[test]
fn fresh_cache_miss_staging_holds_no_compiler_permit() {
    let root = fresh_dir("admission-miss");
    let schedule = fresh_dir("admission-miss-schedule");
    let admission = CompilerAdmission::shared(schedule.clone(), 1).unwrap();
    let occupied = admission.acquire().unwrap();
    let cache = BuildCache::open(root.clone(), None, None, admission).expect("open");
    let request = stub_request("rlib", "rlibs", rlib_fields("p", &[("src/lib.rs", b"x")]));
    let (tx, rx) = mpsc::channel();

    let thread = std::thread::spawn(move || {
        let result = cache.get_or_produce(&request, |ctx| -> Result<(), String> {
            tx.send(()).map_err(|e| e.to_string())?;
            fs::write(ctx.out_dir.join("libgreeter.rlib"), b"!<arch>\n").map_err(|e| e.to_string())
        });
        result.expect("produce");
    });
    rx.recv_timeout(Duration::from_millis(500))
        .expect("cache staging must not wait for compiler admission");

    drop(occupied);
    thread.join().unwrap();
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(schedule);
}

#[test]
fn same_key_waiter_holds_no_compiler_permit() {
    let root = fresh_dir("admission-same-key");
    let schedule = fresh_dir("admission-same-key-schedule");
    let admission = CompilerAdmission::shared(schedule.clone(), 2).unwrap();
    let cache = BuildCache::open(root.clone(), None, None, admission).expect("open");
    let producer_cache = cache.clone();
    let producer_request = stub_request("rlib", "rlibs", rlib_fields("p", &[("src/lib.rs", b"x")]));
    let ready = root.join("compiler-ready");
    let release = root.join("compiler-release");

    let producer = std::thread::spawn(move || {
        producer_cache
            .get_or_produce(&producer_request, |ctx| -> Result<(), String> {
                let mut command = Command::new("sh");
                command
                    .arg("-c")
                    .arg(": >\"$READY\"; while [ ! -e \"$RELEASE\" ]; do sleep 0.01; done")
                    .env("READY", &ready)
                    .env("RELEASE", &release);
                let admitted = ctx
                    .compiler_admission
                    .acquire_for(&mut command)
                    .map_err(|e| e.to_string())?;
                let status = admitted.status().map_err(|e| e.to_string())?;
                if !status.success() {
                    return Err(format!("fixture compiler exited {status}"));
                }
                fs::write(ctx.out_dir.join("libgreeter.rlib"), b"!<arch>\n")
                    .map_err(|e| e.to_string())
            })
            .expect("producer");
    });
    for _ in 0..500 {
        if root.join("compiler-ready").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(root.join("compiler-ready").exists());

    let waiter_request = stub_request("rlib", "rlibs", rlib_fields("p", &[("src/lib.rs", b"x")]));
    let (started_tx, started_rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        cache
            .get_or_produce(&waiter_request, |_ctx| -> Result<(), String> {
                panic!("same-key waiter must observe the first producer's artifact")
            })
            .expect("waiter");
    });
    started_rx.recv().unwrap();
    std::thread::sleep(Duration::from_millis(150));
    let claim_count = fs::read_dir(schedule.join("compiler/claims"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "lease")
        })
        .count();
    assert_eq!(
        claim_count, 1,
        "same-key cache waiter must not claim compiler admission"
    );

    fs::write(root.join("compiler-release"), b"").unwrap();
    producer.join().unwrap();
    waiter.join().unwrap();
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(schedule);
}

#[test]
fn get_or_produce_publishes_under_hex_key() {
    let root = fresh_dir("produce-keyed");
    let cache = open_cache(root.clone());
    let fields = rlib_fields("p", &[("src/lib.rs", b"x")]);
    let req = stub_request("rlib", "rlibs", fields.clone());
    let expect_hex = key::hash_fields(None, &fields);

    let path = cache
        .get_or_produce(&req, |ctx| -> Result<(), String> {
            fs::write(ctx.out_dir.join("libgreeter.rlib"), b"bytes").map_err(|e| e.to_string())
        })
        .expect("produce");
    // …/rlibs/<hex>/lib.rlib
    assert_eq!(
        path.parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy(),
        expect_hex
    );
    assert_eq!(req.key(), expect_hex, "request key matches publish dir");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn get_or_produce_compile_error_leaves_no_entry() {
    let root = fresh_dir("produce-err");
    let cache = open_cache(root.clone());
    let req = stub_request("rlib", "rlibs", rlib_fields("p", &[("src/lib.rs", b"x")]));

    let err = cache
        .get_or_produce(&req, |_ctx| -> Result<(), String> {
            Err("compile blew up".to_owned())
        })
        .unwrap_err();
    match err {
        super::ProduceError::Compile(msg) => assert_eq!(msg, "compile blew up"),
        super::ProduceError::Cache(e) => panic!("expected compile error, got cache error: {e}"),
    }
    // No published artifact: the next read still misses.
    let nd = cache_io::namespace_dir(
        &root,
        "rlib",
        &PathBuf::from("rustc-deadbeef").join("x86_64-unknown-linux-gnu"),
        "rlibs",
    )
    .unwrap();
    let key_dir = nd.artifacts.join(req.key());
    assert!(!is_regular_file_nonempty(&key_dir.join("lib.rlib")));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn get_or_produce_sets_executable_bit_for_bin() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let root = fresh_dir("produce-exec");
        let cache = open_cache(root.clone());
        let mut req = stub_request("rlib", "bins", rlib_fields("p", &[("src/lib.rs", b"x")]));
        req.artifact_name = "bin".to_owned();
        req.staged_name = None;
        req.executable = true;

        let path = cache
            .get_or_produce(&req, |ctx| -> Result<(), String> {
                fs::write(ctx.out_dir.join("bin"), b"\x7fELF").map_err(|e| e.to_string())
            })
            .expect("produce");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "executable bits set on published bin");
        let _ = fs::remove_dir_all(&root);
    }
}

/// Back-date a published key-entry's `.last_used` so the LRU order is
/// deterministic without sleeping between produces.
fn backdate_last_used(entry_dir: &std::path::Path, age_secs: u64) {
    use std::time::{Duration, SystemTime};
    let used = SystemTime::now() - Duration::from_secs(age_secs);
    let times = fs::FileTimes::new().set_modified(used).set_accessed(used);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(entry_dir.join(".last_used"))
        .unwrap();
    file.set_times(times).unwrap();
}

#[test]
fn get_or_produce_evicts_oldest_when_budget_exceeded() {
    // End-to-end budget enforcement: a `max_bytes`-bounded cache prunes
    // the least-recently-used entry on the publish that pushes the kind
    // over budget. This exercises the full
    // `get_or_produce -> prune_lru -> prune_lru_cache` chain that the
    // orchestrators' default KIO_TEST_RUNNER_BUILD_CACHE_SIZE relies on,
    // not just the free `prune_lru_cache` the planted-entry tests cover.
    let body = vec![b'x'; 4096];
    let entry_bytes = body.len() as u64;
    // Budget holds two entries' artifacts but not three (the meta.json
    // sidecars push two entries just over 2*entry_bytes, so size at
    // 2*entry_bytes + a small slack to seat exactly two before the
    // third produce trips the sweep).
    let budget = entry_bytes * 2 + 64;

    let root = fresh_dir("produce-evict");
    let cache = BuildCache::open(
        root.clone(),
        None,
        Some(budget),
        CompilerAdmission::disabled(),
    )
    .expect("open");

    let produce_entry = |crate_name: &str| -> PathBuf {
        let req = stub_request(
            "rlib",
            "bins",
            rlib_fields(crate_name, &[("src/lib.rs", crate_name.as_bytes())]),
        );
        let mut req = req;
        req.artifact_name = "bin".to_owned();
        req.staged_name = None;
        req.executable = true;
        let body = body.clone();
        cache
            .get_or_produce(&req, move |ctx| -> Result<(), String> {
                fs::write(ctx.out_dir.join("bin"), &body).map_err(|e| e.to_string())
            })
            .expect("produce")
    };

    // Two entries seated under budget; back-date so `a` is the oldest.
    let a = produce_entry("aaa");
    let a_dir = a.parent().unwrap().to_path_buf();
    backdate_last_used(&a_dir, 120);
    let b = produce_entry("bbb");
    let b_dir = b.parent().unwrap().to_path_buf();
    backdate_last_used(&b_dir, 60);
    assert!(
        a.is_file(),
        "first entry present before the over-budget write"
    );
    assert!(
        b.is_file(),
        "second entry present before the over-budget write"
    );

    // Third produce pushes total to 3*entry_bytes > budget: the sweep
    // evicts the oldest (`a`), protecting the just-published `c`.
    let c = produce_entry("ccc");
    assert!(
        c.is_file(),
        "freshly published entry must survive its own prune"
    );
    assert!(b.is_file(), "newer entry must remain under budget");
    assert!(
        !a.exists(),
        "least-recently-used entry must be evicted once over budget"
    );
    let _ = fs::remove_dir_all(&root);
}

// =========================================================================
// IO write failure helpers
// =========================================================================

#[test]
fn ensure_dir_creates_missing_parents() {
    let root = fresh_dir("ensure-parents");
    let deep = root.join("a/b/c/d");
    cache_io::ensure_dir(&deep).expect("ensure");
    assert!(deep.is_dir());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ensure_dir_is_idempotent_when_dir_exists() {
    let root = fresh_dir("ensure-idempotent");
    cache_io::ensure_dir(&root).expect("create-1");
    cache_io::ensure_dir(&root).expect("create-2");
    let _ = fs::remove_dir_all(&root);
}
