//! Go adapter over the shared content-addressed build cache for
//! `kio build go` artifacts.
//!
//! The generic keying / locking / publication / eviction machinery
//! lives in [`crate::build_cache`]; this module is the **Go compiler
//! adapter** over it. It supplies the go-specific policy: the compiler
//! identity ([`go_identity`]), the key-input field list ([`key`]), the
//! one `go build` invocation, the `meta.json` body, and the `bin`
//! on-disk namespacing. The shared cache owns everything else.
//!
//! A build pays the `go build` cost only once per
//! `(build-tree-content, toolchain, flags)` tuple: racing processes
//! lock-and-wait on the shared cache's per-key file lock, partial
//! writes recover via its atomic-rename publication, and `meta.json`
//! sidecars let `cat` / `jq` inspect entries.
//!
//! ## One-level caching
//!
//! Unlike the Rust adapter's two-level rlib→bin split, the Go runner
//! compiles the whole build tree — the emitted package `.go` files plus
//! the synthesized `main.go` driver and the `go.mod` wiring — in a
//! single `go build`. So the cache is **one-level**: one
//! [`ArtifactRequest`], one namespace (`bin`), one key folding `{go
//! identity, build settings, all build-tree source files}`. No chained
//! sub-key, no second namespace — see [`crate::build_cache`] for the
//! contrast with the two-level Rust adapter.
//!
//! ## Path-neutral artifacts
//!
//! The `go build` runs with **`-trimpath`** so the emitted binary
//! embeds no build-directory path. Without it, the binary records the
//! absolute build dir and two compiles of the same sources from
//! different worktrees produce *different* bytes — defeating
//! cross-worktree cache reuse. With `-trimpath` the bytes are identical
//! across build locations, so a populated cache stays valid when the
//! worktree moves. The flag is folded into the key (it changes the
//! output), so a `-trimpath` build never aliases a non-`-trimpath` one.
//!
//! The per-compile `GOCACHE` is kept under the staging tempdir. Left at
//! its default, `go build` writes the toolchain's incremental cache to
//! the machine-shared `~/.cache/go-build` — state outside the runner's
//! cache root, which the runner-hygiene norm forbids (a runner writes
//! machine-shared state only under its build-cache dir and staging
//! tempdirs; see the runner README § Runner build cache and compiler
//! wrappers). Pinning it per-compile keeps that write inside the
//! tempdir; isolating concurrent runner invocations from racing on a
//! shared `GOCACHE` is a second benefit. That toolchain cache is
//! incidental to *our* artifact (the staged binary), which the shared
//! build cache owns; the two caches are distinct layers, and it is
//! discarded with the tempdir.
//!
//! Every Go command also sets **`GOENV=off`**, **`GOTOOLCHAIN=local`**, and an
//! empty **`GOEXPERIMENT`**. The first prevents per-user `go env -w`
//! configuration from changing a supposedly identical compile; the second
//! prevents a module directive or ambient configuration from selecting and
//! downloading another toolchain; the third prevents a stale experiment
//! selection from changing the Go 1.26 language surface. The same settings
//! govern the `go version` identity probe, and they are folded into the
//! artifact key so entries built before this hermetic policy cannot alias
//! entries built under it. **`TEST_TELEMETRY_DIR`** redirects cmd/go's local
//! telemetry state beneath the cache root for the identity probe and beneath
//! the staging tempdir for a build. Those per-invocation paths do not affect
//! compiler selection or output bytes and therefore do not enter the key.
//!
//! ## Resource isolation under a parallel harness
//!
//! Compiler-process fan-out is bounded by the generic admission capability
//! injected into the shared cache. Go has no backend-specific slot count or
//! internal `-p` policy. **`GOTMPDIR` pinned under the staging tempdir** keeps the
//!    `$WORK` build scratch off `TMPDIR` — commonly a small RAM-backed
//!    tmpfs that a wide run fills to quota (`write $WORK/...: disk
//!    quota exceeded`) — and on the cache root's filesystem, discarded
//!    with the tempdir (an OOM-killed build's leaked `$WORK` is then
//!    reaped with the orphaned tempdir rather than accumulating in the
//!    shared temp root).

use crate::build_cache::{ArtifactRequest, BuildCache, CompilerAdapter, ProduceCtx, ProduceError};
use crate::compiler_admission::CompilerAdmission;
use crate::compiler_observer::CompilerObserver;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub mod key;

#[cfg(test)]
mod tests;

pub use key::BinInput;

/// Cache-kind segment under the cache root for Go artifacts. The shared
/// cache partitions every kind into its own subtree; `"go"` is this
/// adapter's.
const GO_CACHE_KIND: &str = "go";

/// The published binary's filename inside its `<hex>/` key directory.
const BIN_NAME: &str = "bin";

const GOENV_VALUE: &str = "off";
const GOTOOLCHAIN_VALUE: &str = "local";
const GOEXPERIMENT_VALUE: &str = "";
const TELEMETRY_DIR_NAME: &str = ".telemetry";

/// The Go adapter's error surface. Carries the shared cache's I/O
/// failures and this adapter's own admission, `go build`, and spawn failures
/// under one enum. The `produce` closure returns only adapter variants;
/// the shared cache splits the two kinds into [`ProduceError`], which
/// [`unwrap_produce`] folds back.
#[derive(Debug)]
pub enum CacheError {
    /// A cache I/O failure surfaced by the shared machinery (lock,
    /// rename, directory create).
    Io(crate::build_cache::CacheError),
    /// The shared scheduler could not admit the `go build` command.
    Admission(crate::compiler_admission::Error),
    /// `go build` exited non-zero (a compile error in the emitted
    /// package or the driver). Carries the captured stderr so the
    /// caller can surface the toolchain's own diagnostics.
    GoBuildFailed { stderr: String },
    /// `go` or its configured observer could not be spawned.
    GoSpawn { source: std::io::Error },
}

impl From<crate::build_cache::CacheError> for CacheError {
    fn from(e: crate::build_cache::CacheError) -> Self {
        CacheError::Io(e)
    }
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheError::Io(e) => write!(f, "{e}"),
            CacheError::Admission(e) => write!(f, "{e}"),
            CacheError::GoBuildFailed { stderr } => write!(f, "`go build` failed:\n{stderr}"),
            CacheError::GoSpawn { source } => write!(f, "could not spawn go: {source}"),
        }
    }
}

impl std::error::Error for CacheError {}

/// The stable build settings, in the order they're rendered into the key's
/// [`crate::build_cache::InputField::KvPairs`] field. `-trimpath` is a
/// valueless flag (empty value); `GOENV`, `GOTOOLCHAIN`, and the deliberately
/// empty `GOEXPERIMENT` record the hermetic command environment; the
/// `go-directive` records the `go 1.NN` line the runner writes into `go.mod`.
/// Each can change the selected compiler or emitted binary, so each
/// participates in cache identity.
fn build_flags(go_directive: &str) -> Vec<(String, String)> {
    vec![
        ("-trimpath".to_owned(), String::new()),
        ("GOENV".to_owned(), GOENV_VALUE.to_owned()),
        ("GOEXPERIMENT".to_owned(), GOEXPERIMENT_VALUE.to_owned()),
        ("GOTOOLCHAIN".to_owned(), GOTOOLCHAIN_VALUE.to_owned()),
        ("go-directive".to_owned(), go_directive.to_owned()),
    ]
}

fn isolated_telemetry_dir(root: &Path) -> Result<PathBuf, CacheError> {
    let path = root.join(TELEMETRY_DIR_NAME);
    fs::create_dir_all(&path).map_err(|e| {
        CacheError::Io(crate::build_cache::CacheError::Io {
            path: path.clone(),
            source: e,
        })
    })?;
    Ok(path)
}

fn apply_hermetic_go_environment(cmd: &mut Command, telemetry_dir: &Path) {
    cmd.env("GOENV", GOENV_VALUE)
        .env("GOEXPERIMENT", GOEXPERIMENT_VALUE)
        .env("GOTOOLCHAIN", GOTOOLCHAIN_VALUE)
        .env("TEST_TELEMETRY_DIR", telemetry_dir);
}

/// The cache's public handle. Construct via [`GoCache::open`];
/// `get_or_compile_bin` hangs off `self`. A thin adapter wrapper around
/// the shared [`BuildCache`]; the go identity / build flags the adapter
/// keys on are supplied per call through [`BinInput`].
#[derive(Debug, Clone)]
pub struct GoCache {
    cache: BuildCache,
    compiler_observer: CompilerObserver,
}

impl GoCache {
    /// Open with build-cache policy, compiler observation, and admission.
    pub fn open(
        root: PathBuf,
        compiler_wrapper: Option<OsString>,
        compiler_observer: CompilerObserver,
        max_bytes: Option<u64>,
        compiler_admission: CompilerAdmission,
    ) -> Result<Self, CacheError> {
        let cache = BuildCache::open(root, compiler_wrapper, max_bytes, compiler_admission)?;
        Ok(GoCache {
            cache,
            compiler_observer,
        })
    }

    /// Resolve the binary for `tree`. On hit returns the existing binary
    /// path; on miss the shared cache acquires the per-key lock, runs
    /// `go build` into a tempdir, and atomic-renames into place.
    pub fn get_or_compile_bin(&self, tree: &GoBuildTree) -> Result<PathBuf, CacheError> {
        let adapter = BinArtifact {
            tree,
            compiler_observer: &self.compiler_observer,
        };
        self.cache.resolve(&adapter).map_err(unwrap_produce)
    }
}

/// Fold the shared cache's split `ProduceError<CacheError>` back into
/// the adapter's flat [`CacheError`].
fn unwrap_produce(e: ProduceError<CacheError>) -> CacheError {
    match e {
        ProduceError::Compile(c) => c,
        ProduceError::Cache(io) => CacheError::Io(io),
    }
}

/// The assembled Go build tree the runner hands the cache: the
/// toolchain identity, the `go 1.NN` directive, and the full set of
/// `(relative path, bytes)` files the `go build` compiles (emitted
/// package `.go`, the synthesized `main.go`, the `go.mod` wiring).
///
/// The runner builds this once. Its file set both *defines the cache
/// key* (via [`BinInput`]) and *is what `produce` writes to disk* before
/// invoking `go build`, so the keyed content and the compiled content
/// are the same bytes by construction.
#[derive(Debug, Clone)]
pub struct GoBuildTree {
    pub go_identity: String,
    pub go_directive: String,
    /// Path-sorted `(relative path, bytes)` for the whole build tree.
    /// The driver lives at `driver/main.go`, the package at
    /// `<ns>/<file>.go`, the `go.mod`s at their respective roots —
    /// the exact relative layout `produce` recreates and `go build`
    /// reads.
    pub files: Vec<(PathBuf, Vec<u8>)>,
    /// The relative path of the package directory `go build` builds
    /// against (the driver imports it). The runner sets this so the
    /// `go build ./<driver-dir>` invocation knows which package to
    /// compile — incidental to the key (it's already implied by the
    /// driver's import path inside `files`).
    pub build_package: String,
}

impl GoBuildTree {
    fn bin_input(&self) -> BinInput {
        BinInput {
            go_identity: self.go_identity.clone(),
            build_flags: build_flags(&self.go_directive),
            build_files: self.files.clone(),
        }
    }
}

/// The binary artifact adapter: builds the binary's
/// [`ArtifactRequest`] and runs `go build -trimpath` on a miss.
struct BinArtifact<'a> {
    tree: &'a GoBuildTree,
    compiler_observer: &'a CompilerObserver,
}

impl CompilerAdapter for BinArtifact<'_> {
    type Error = CacheError;

    fn request(&self) -> ArtifactRequest {
        let input = self.tree.bin_input();
        ArtifactRequest {
            kind: GO_CACHE_KIND.to_owned(),
            subroot_rel: key::subroot_rel(&self.tree.go_identity),
            namespace: "bin".to_owned(),
            role: "bin".to_owned(),
            key_inputs: key::bin_fields(&input),
            hash_domain: None,
            artifact_name: BIN_NAME.to_owned(),
            staged_name: None,
            executable: true,
            meta_json: key::bin_meta_json(&input),
        }
    }

    fn produce(&self, ctx: &ProduceCtx<'_>) -> Result<(), CacheError> {
        let _ignored_cache_wrapper = ctx.compiler_wrapper;
        do_go_build(
            self.tree,
            ctx.out_dir,
            self.compiler_observer,
            ctx.compiler_admission,
        )
    }
}

/// Materialize the build tree under `out_dir`, run `go build -trimpath`
/// against it, and stage the binary as `out_dir/bin` for the shared
/// cache to publish.
fn do_go_build(
    tree: &GoBuildTree,
    out_dir: &Path,
    observer: &CompilerObserver,
    compiler_admission: &CompilerAdmission,
) -> Result<(), CacheError> {
    let build_root = out_dir.join("build");
    write_tree(&build_root, &tree.files)?;

    let bin_path = out_dir.join(BIN_NAME);
    // Pin the go toolchain's incremental GOCACHE under the staging
    // tempdir. Left at its default it writes to the machine-shared
    // ~/.cache/go-build, outside the runner's cache root — the
    // runner-hygiene norm (README § Runner build cache and compiler
    // wrappers) keeps that machine-shared write inside the tempdir;
    // isolating concurrent invocations from a shared GOCACHE is a second
    // benefit. Distinct from our content-addressed binary cache;
    // discarded with the tempdir.
    let gocache = out_dir.join(".gocache");
    // Pin the toolchain's $WORK scratch (GOTMPDIR) under the staging
    // tempdir, next to GOCACHE: its default is TMPDIR, commonly a small
    // RAM-backed tmpfs that a wide parallel harness run fills to quota.
    // `go` requires the directory to exist. See the module docs
    // § Resource isolation under a parallel harness.
    let gotmp = out_dir.join(".gotmp");
    fs::create_dir_all(&gotmp).map_err(|e| {
        CacheError::Io(crate::build_cache::CacheError::Io {
            path: gotmp.clone(),
            source: e,
        })
    })?;
    let telemetry_dir = isolated_telemetry_dir(out_dir)?;

    let mut cmd = go_compile_command(observer);
    cmd.arg("build")
        // Path-neutral artifact: strip the build path so the binary is
        // identical across worktrees. See the module docs.
        .arg("-trimpath")
        .arg("-o")
        .arg(&bin_path)
        .arg(format!("./{}", tree.build_package))
        .current_dir(&build_root)
        .env("GOCACHE", &gocache)
        .env("GOTMPDIR", &gotmp)
        .env("GOFLAGS", "-mod=mod");
    apply_hermetic_go_environment(&mut cmd, &telemetry_dir);
    let admitted = compiler_admission
        .acquire_for(&mut cmd)
        .map_err(CacheError::Admission)?;
    let out = admitted
        .output()
        .map_err(|e| CacheError::GoSpawn { source: e })?;
    if !out.status.success() {
        return Err(CacheError::GoBuildFailed {
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

fn go_compile_command(observer: &CompilerObserver) -> Command {
    observer.command(std::ffi::OsStr::new("go"), None)
}

/// Write a `(relative path, bytes)` file set under `root`, creating
/// parent directories as needed.
fn write_tree(root: &Path, files: &[(PathBuf, Vec<u8>)]) -> Result<(), CacheError> {
    for (rel, bytes) in files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                CacheError::Io(crate::build_cache::CacheError::Io {
                    path: parent.to_path_buf(),
                    source: e,
                })
            })?;
        }
        fs::write(&path, bytes).map_err(|e| {
            CacheError::Io(crate::build_cache::CacheError::Io {
                path: path.clone(),
                source: e,
            })
        })?;
    }
    Ok(())
}

/// Probe `go version` once and return its trimmed stdout as the
/// toolchain identity (e.g. `go version go1.26.4 linux/amd64`). The
/// platform suffix is part of the identity, so the subroot already
/// partitions by host platform without a separate triple segment.
pub fn go_identity(cache_root: &Path) -> Result<String, CacheError> {
    let telemetry_dir = isolated_telemetry_dir(cache_root)?;
    let mut cmd = Command::new("go");
    cmd.arg("version");
    apply_hermetic_go_environment(&mut cmd, &telemetry_dir);
    let out = cmd
        .output()
        .map_err(|e| CacheError::GoSpawn { source: e })?;
    if !out.status.success() {
        return Err(CacheError::GoBuildFailed {
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
