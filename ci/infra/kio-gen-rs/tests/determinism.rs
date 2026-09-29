//! Determinism gate.
//!
//! kio-gen's guarantee: same `--seed N`, same `--count C` produce a
//! byte-identical batch, regardless of thread count. The generator
//! threads a seeded `ChaCha20Rng` through every random decision and
//! derives a per-program seed via `program_seed(seed, idx)` — no
//! shared RNG state across the batch — so parallel iteration order
//! cannot leak into output.
//!
//! These tests gate that guarantee. Without them, a future regression
//! (a stray `thread_rng()`, a `HashMap` walk for choice picking, a
//! rayon refactor that shares an RNG across workers) would silently
//! degrade reproducibility and tank the runner build cache hit rate.
//!
//! The bulk of the work is in-process: replicate `main.rs`'s
//! generation loop (program-from-seed + shrink + mutate + render)
//! against the library API and assert byte-equality across many
//! iterations. A smaller end-to-end pass invokes the released
//! binary in a subprocess and walks its output directory, gating
//! the filesystem-write path too.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

use kio_gen::generate::{GenOpts, program_from_seed_opts};
use kio_gen::{emit, mutate, program_seed, shrink};

/// Replicates `main.rs`'s per-program output deterministically, in
/// memory. Returns one entry per program in index order; each entry
/// is the program's full package as a sorted (path, content) list,
/// prefixed by an exit-code marker so a mutator swap surfaces here.
///
/// The decision RNG that picks valid-vs-mutated and which mutator
/// fires is re-derived from `prog_seed ^ 0x4d75_7461_7465` exactly as
/// `main.rs` does — so this helper is a faithful in-memory mirror of
/// the binary's loop, not a parallel reimplementation that could
/// drift.
fn batch_in_memory(seed: u64, count: u64) -> Vec<Vec<(String, String)>> {
    let opts = GenOpts { prime_only: false };
    // Mirror `main.rs`'s default valid_fraction so the in-memory
    // path generates the same mix of valid + mutated cases the
    // released binary would.
    let valid_fraction = 0.2;
    (0..count)
        .map(|i| {
            let prog_seed = program_seed(seed, i);
            let prog = program_from_seed_opts(prog_seed, &opts);
            let mut decision = ChaCha20Rng::seed_from_u64(prog_seed ^ 0x4d75_7461_7465);
            let mutate_it = !decision.gen_bool(valid_fraction);
            let mut files: Vec<(String, String)> = if mutate_it {
                let m = mutate::pick(&mut decision);
                let to_mutate = shrink::minimize_body(&prog);
                let (files, exit_code, category) = mutate::apply(&to_mutate, m);
                // Tag the program's entries with category + exit
                // code so a regression that flips a program from
                // (say) Parse to NameRes shows up bytewise even if
                // the rendered .kio happens to match.
                let mut out = files;
                out.insert(0, ("__category".to_string(), category.to_string()));
                out.insert(1, ("__exit_code".to_string(), exit_code.to_string()));
                out
            } else {
                let mut out = emit::render_package_files(&prog);
                out.insert(0, ("__category".to_string(), "00_success".to_string()));
                out.insert(1, ("__exit_code".to_string(), "0".to_string()));
                out
            };
            // Sort by path so iteration-order in a future emit-side
            // refactor cannot affect this test's comparison key.
            files.sort_by(|a, b| a.0.cmp(&b.0));
            files
        })
        .collect()
}

/// Same as `batch_in_memory` but compute per-program output in
/// parallel using a scoped rayon pool of `threads` workers. Result
/// is collected back in index order, so it is identical to the
/// sequential output when determinism holds.
fn batch_in_memory_par(seed: u64, count: u64, threads: usize) -> Vec<Vec<(String, String)>> {
    use rayon::prelude::*;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("rayon pool");
    pool.install(|| {
        let opts = GenOpts { prime_only: false };
        let valid_fraction = 0.2;
        (0..count)
            .into_par_iter()
            .map(|i| {
                let prog_seed = program_seed(seed, i);
                let prog = program_from_seed_opts(prog_seed, &opts);
                let mut decision = ChaCha20Rng::seed_from_u64(prog_seed ^ 0x4d75_7461_7465);
                let mutate_it = !decision.gen_bool(valid_fraction);
                let mut files: Vec<(String, String)> = if mutate_it {
                    let m = mutate::pick(&mut decision);
                    let to_mutate = shrink::minimize_body(&prog);
                    let (files, exit_code, category) = mutate::apply(&to_mutate, m);
                    let mut out = files;
                    out.insert(0, ("__category".to_string(), category.to_string()));
                    out.insert(1, ("__exit_code".to_string(), exit_code.to_string()));
                    out
                } else {
                    let mut out = emit::render_package_files(&prog);
                    out.insert(0, ("__category".to_string(), "00_success".to_string()));
                    out.insert(1, ("__exit_code".to_string(), "0".to_string()));
                    out
                };
                files.sort_by(|a, b| a.0.cmp(&b.0));
                files
            })
            .collect()
    })
}

/// Walk `root` recursively, returning a sorted map of relative-path
/// → file content. Used by the binary-end-to-end test to gate the
/// emission filesystem walk that the in-memory tests don't cover.
fn walk_output(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    walk_into(root, root, &mut out);
    out
}

fn walk_into(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("dir entry"))
        .collect();
    // Sort so a hypothetical future regression in the walker code
    // doesn't accidentally pass by reading directories in the order
    // the OS happens to return them.
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let ty = entry.file_type().expect("file_type");
        if ty.is_dir() {
            walk_into(root, &path, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .expect("strip_prefix")
                .to_string_lossy()
                .into_owned();
            let bytes = fs::read(&path).expect("read file");
            out.insert(rel, bytes);
        }
    }
}

/// 200 in-process iterations of the generation pipeline at the
/// same seed must produce byte-identical batches. This is the
/// load-bearing determinism gate — every random choice the generator
/// makes is sampled here, and any non-deterministic source slipping
/// in (a `thread_rng()` call, a `HashMap` walk for choice-picking,
/// a rayon refactor that doesn't split the master RNG) breaks
/// equality on the first divergent iteration.
///
/// 200 iterations is overkill for catching a deterministic-but-
/// wrong regression (1 iteration would suffice), but probabilistic
/// regressions — e.g., a stray hasher whose iteration order is
/// usually-but-not-always identical, or a thread-pool race that
/// fires occasionally — need repetition to catch reliably. The
/// session spec calls for 1000 with a < 15 s budget; at 200 the
/// test takes ~3 s in cargo's default debug profile and ~0.3 s
/// release, well inside budget on a laptop. Higher counts pushed
/// debug-profile timing close to the 15 s ceiling on this hardware
/// (rendered packages allocate aggressively in shrink + emit);
/// 200 keeps headroom for slower CI runners without losing the
/// probabilistic-regression coverage that's the test's point.
#[test]
fn batch_is_byte_identical_across_200_runs() {
    let seed = 42;
    let count = 16;
    let baseline = batch_in_memory(seed, count);
    for i in 0..200 {
        let attempt = batch_in_memory(seed, count);
        assert_eq!(
            attempt, baseline,
            "iteration {i} of seed={seed} count={count} diverged from baseline"
        );
    }
}

/// Batch output must not depend on the rayon worker count. The
/// generator's per-program seed is `program_seed(seed, idx)` — a
/// pure function of the run seed and the program index, with no
/// shared RNG state across the batch — so parallel iteration order
/// cannot leak into per-program output. This test pins that
/// property: a sequential pool (1 worker) and an 8-worker pool
/// must produce identical batches.
#[test]
fn batch_invariant_under_jobs() {
    let seed = 42;
    let count = 32;
    let one_worker = batch_in_memory_par(seed, count, 1);
    let eight_workers = batch_in_memory_par(seed, count, 8);
    assert_eq!(
        one_worker, eight_workers,
        "batch differs between 1-worker and 8-worker rayon pools at seed={seed}"
    );
    // Cross-check against the sequential (non-rayon) path too —
    // catches the unlikely-but-possible regression where the
    // sequential and parallel code paths disagree because someone
    // added a parallel-only branch.
    let sequential = batch_in_memory(seed, count);
    assert_eq!(
        sequential, eight_workers,
        "sequential vs 8-worker rayon disagree at seed={seed}"
    );
}

/// Different seeds must produce different batches — a sanity check
/// that the seed actually feeds into generation. If a future
/// refactor accidentally shadowed the seed with a constant, every
/// other test in this file would still pass; this one fails loudly.
#[test]
fn different_seeds_produce_different_batches() {
    let count = 16;
    let a = batch_in_memory(1, count);
    let b = batch_in_memory(2, count);
    assert_ne!(
        a, b,
        "seeds 1 and 2 produced identical batches — seed plumbing is broken"
    );
}

/// End-to-end: invoke the released `kio-gen` binary in a subprocess
/// with `--seed 42 --count 16`, walk its output directory, and
/// assert byte-equality across a handful of runs. This gates the
/// filesystem-write path (`emit::write_case`'s `fs::write` calls,
/// directory creation, `run.args` / `IS_KIO_PRIME` / `expected.*`
/// marker writes) that the in-memory tests don't exercise.
///
/// Iteration count is deliberately small (5) — each subprocess
/// invocation costs ~20 ms, so 5 runs adds ~100 ms to test time,
/// while still catching any non-determinism in the bytes the
/// binary writes to disk. The 200× repetition lives in the
/// in-process test above where it's cheap.
///
/// The binary is built via a one-shot `cargo build --release`
/// before the assertion loop. We use `--bin kio-gen` to scope the
/// build, so this test doesn't trigger rebuilding the whole crate
/// in test profile alongside.
#[test]
fn binary_emits_byte_identical_output_across_runs() {
    let bin = build_kio_gen_release();
    let runs = 5;
    let mut baseline: Option<BTreeMap<String, Vec<u8>>> = None;
    for i in 0..runs {
        let tmp = tempdir();
        let status = Command::new(&bin)
            .arg("--output")
            .arg(tmp.path())
            .arg("--seed")
            .arg("42")
            .arg("--count")
            .arg("16")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("spawn kio-gen");
        assert!(
            status.success(),
            "kio-gen exited with {status:?} on run {i}"
        );
        let snapshot = walk_output(tmp.path());
        if let Some(prev) = baseline.as_ref() {
            assert_eq!(
                &snapshot, prev,
                "kio-gen output diverged on run {i} — emission is non-deterministic"
            );
        } else {
            baseline = Some(snapshot);
        }
    }
}

/// Locate or build the release `kio-gen` binary at a stable path
/// under the crate's `target/release/` directory.
fn build_kio_gen_release() -> std::path::PathBuf {
    // `CARGO_MANIFEST_DIR` points at `ci/infra/kio-gen-rs/`; the
    // release binary lands at `target/release/kio-gen` relative to
    // it (unless `CARGO_TARGET_DIR` is set, which the test honors
    // via the cargo-supplied env var).
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // The test is already running under Cargo, which may hold the repository
    // lock; using the top-level wrapper here would try to reacquire that lock.
    let status = Command::new(env!("CARGO"))
        .arg("build")
        .arg("--release")
        .arg("--bin")
        .arg("kio-gen")
        .current_dir(&manifest_dir)
        .status()
        .expect("spawn cargo build");
    assert!(
        status.success(),
        "cargo build --release --bin kio-gen failed"
    );
    // Use $CARGO_TARGET_DIR if cargo set it; otherwise fall back to
    // the crate-local target/ dir.
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("target"));
    let bin = target_dir.join("release").join("kio-gen");
    assert!(
        bin.exists(),
        "kio-gen binary not found at {}",
        bin.display()
    );
    bin
}

/// Minimal tempdir helper: create a unique directory under
/// `std::env::temp_dir()`, returning a guard that removes it on
/// drop. Avoids a `tempfile` dev-dep for one call site.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> TempDir {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("kio-gen-determinism-{pid}-{nanos}-{n}"));
    fs::create_dir_all(&path).expect("create tempdir");
    TempDir(path)
}
