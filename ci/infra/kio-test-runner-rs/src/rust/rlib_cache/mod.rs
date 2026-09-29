//! Rust adapter over the shared content-addressed build cache for
//! `kio build rust` artifacts.
//!
//! The generic keying / locking / publication / eviction machinery
//! lives in [`crate::build_cache`]; this module is the **Rust
//! compiler adapter** over it. It supplies the rustc-specific policy:
//! the compiler identity ([`rustc_identity`]), the codegen
//! [`Profile`], the key-input field lists for the rlib and bin
//! ([`RlibInput`] / [`BinInput`]), the two rustc invocations, the
//! `meta.json` bodies, and the `rlib`/`rlibs`/`bins` on-disk
//! namespacing. The shared cache owns everything else.
//!
//! A build pays the rustc cost only once per
//! `(emitted-crate-content, toolchain, target, profile)` tuple:
//! racing processes lock-and-wait on the shared cache's per-key file
//! lock, partial writes recover via its atomic-rename publication, and
//! `meta.json` sidecars let `cat` / `jq` inspect entries.
//!
//! ## Two-level caching
//!
//! [`RlibCache::get_or_compile_bin`] composes **two** shared-cache
//! artifacts — the package rlib and the driver bin — keyed
//! independently in two namespaces (`rlibs` / `bins`). The bin's
//! key folds the rlib's key as a chained sub-key (see [`BinInput`]),
//! so a custom `Host` impl invalidates only the bin compile, not the
//! package rlib. This is the **two-level** adapter shape; a one-level
//! adapter (go / haskell / swift, a later stage) caches a single final
//! binary in one namespace — see [`crate::build_cache`] for the
//! contrast.
//!
//! The cache code intentionally never reaches for `cargo`: rustc is
//! the cheap path, and the design's wall-clock budget assumes no
//! Cargo machinery on the call path.

use crate::build_cache::{ArtifactRequest, BuildCache, CompilerAdapter, ProduceCtx, ProduceError};
use crate::compiler_admission::CompilerAdmission;
use crate::compiler_observer::CompilerObserver;
use crate::path_display::DisplayPath;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

pub mod key;

#[cfg(test)]
mod tests;

pub use key::{BinInput, RlibInput, collect_crate_files};

/// Cache-kind segment under the cache root for Rust artifacts. The
/// shared cache partitions every kind into its own subtree; `"rlib"`
/// is this adapter's.
const RUST_CACHE_KIND: &str = "rlib";

/// The Rust adapter's error surface, the single error type the
/// runner-facing API returns. Carries the shared cache's I/O failures
/// ([`CacheError::Io`]) and this adapter's own admission and rustc
/// compile/spawn failures under one enum. The `produce` closure returns
/// adapter variants; the shared cache splits adapter and cache failures into
/// [`ProduceError`], which [`unwrap_produce`] folds back.
#[derive(Debug)]
pub enum CacheError {
    /// A cache I/O failure surfaced by the shared machinery (lock,
    /// rename, directory create).
    Io(crate::build_cache::CacheError),
    /// The shared scheduler could not admit the rustc command.
    Admission(crate::compiler_admission::Error),
    /// rustc invocation failed (compile error in the emitted crate or
    /// the driver). Not a cache failure — propagated up so the caller
    /// surfaces rustc's own stderr.
    RustcFailed { status: std::process::ExitStatus },
    /// rustc or a configured outer wrapper/observer could not be spawned
    /// (binary not on PATH, etc.).
    RustcSpawn {
        program: String,
        source: std::io::Error,
    },
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
            CacheError::RustcFailed { status } => {
                write!(f, "rustc failed (exit {})", status.code().unwrap_or(-1))
            }
            CacheError::RustcSpawn { program, source } => {
                write!(f, "could not spawn {program}: {source}")
            }
        }
    }
}

impl std::error::Error for CacheError {}

/// The cache's public handle. Construct via [`RlibCache::open`];
/// everything else hangs off `self`. A thin adapter wrapper around the
/// shared [`BuildCache`]; the rustc identity / target the adapter keys
/// on are supplied per call through [`RlibInput`] / [`BinInput`].
#[derive(Debug, Clone)]
pub struct RlibCache {
    cache: BuildCache,
    compiler_observer: CompilerObserver,
}

impl RlibCache {
    /// Open with compiler wrapper/observer, size policy, and admission.
    pub fn open(
        root: PathBuf,
        compiler_wrapper: Option<OsString>,
        compiler_observer: CompilerObserver,
        max_bytes: Option<u64>,
        compiler_admission: CompilerAdmission,
    ) -> Result<Self, CacheError> {
        let cache = BuildCache::open(root, compiler_wrapper, max_bytes, compiler_admission)?;
        Ok(RlibCache {
            cache,
            compiler_observer,
        })
    }

    /// Resolve an rlib for the given input. On hit returns the existing
    /// rlib path; on miss the shared cache acquires the per-key lock,
    /// runs rustc into a tempdir, and atomic-renames into place.
    ///
    /// `crate_root` is the path of the emitted crate on disk (the
    /// directory containing `Cargo.toml` and `src/lib.rs`). The caller
    /// is responsible for ensuring the input's `crate_files` was built
    /// from the same path; a mismatch surfaces as a cache miss followed
    /// by a rustc compile against the on-disk content.
    pub fn get_or_compile_rlib(
        &self,
        input: &RlibInput,
        crate_root: &Path,
    ) -> Result<PathBuf, CacheError> {
        let adapter = RlibArtifact {
            input,
            crate_root,
            compiler_observer: &self.compiler_observer,
        };
        self.cache.resolve(&adapter).map_err(unwrap_produce)
    }

    /// Resolve a bin for the given input. The rlib it links against is
    /// resolved (or compiled) via the same cache; the bin is keyed
    /// independently — its key folds the rlib's key as a chained
    /// sub-key — so a driver-source edit invalidates only the bin
    /// compile.
    ///
    /// `crate_root` and `driver_path` are the on-disk paths the rustc
    /// invocation reads.
    pub fn get_or_compile_bin(
        &self,
        rlib_input: &RlibInput,
        crate_root: &Path,
        driver_path: &Path,
        bin_input: &BinInput,
    ) -> Result<PathBuf, CacheError> {
        let rlib_path = self.get_or_compile_rlib(rlib_input, crate_root)?;
        let adapter = BinArtifact {
            rlib_input,
            rlib_path: &rlib_path,
            driver_path,
            bin_input,
            compiler_observer: &self.compiler_observer,
        };
        self.cache.resolve(&adapter).map_err(unwrap_produce)
    }
}

/// Fold the shared cache's split `ProduceError<CacheError>` back into
/// the adapter's flat [`CacheError`]: the `produce` closure already
/// returns a `CacheError`, and a cache I/O failure becomes
/// [`CacheError::Io`].
fn unwrap_produce(e: ProduceError<CacheError>) -> CacheError {
    match e {
        ProduceError::Compile(c) => c,
        ProduceError::Cache(io) => CacheError::Io(io),
    }
}

/// The rlib artifact adapter: builds the rlib's [`ArtifactRequest`]
/// and runs `rustc --crate-type=rlib` on a miss.
struct RlibArtifact<'a> {
    input: &'a RlibInput,
    crate_root: &'a Path,
    compiler_observer: &'a CompilerObserver,
}

impl CompilerAdapter for RlibArtifact<'_> {
    type Error = CacheError;

    fn request(&self) -> ArtifactRequest {
        ArtifactRequest {
            kind: RUST_CACHE_KIND.to_owned(),
            subroot_rel: key::subroot_rel(&self.input.rustc_identity, &self.input.target_triple),
            namespace: "rlibs".to_owned(),
            role: "rlib".to_owned(),
            key_inputs: key::rlib_fields(self.input),
            hash_domain: None,
            artifact_name: "lib.rlib".to_owned(),
            staged_name: Some(format!("lib{}.rlib", self.input.crate_name)),
            executable: false,
            meta_json: key::rlib_meta_json(self.input),
        }
    }

    fn produce(&self, ctx: &ProduceCtx<'_>) -> Result<(), CacheError> {
        do_rustc_rlib(self.input, self.crate_root, self.compiler_observer, ctx)
    }
}

/// The bin artifact adapter: builds the bin's [`ArtifactRequest`]
/// (folding the resolved rlib's key as a chained sub-key) and runs
/// `rustc --crate-type=bin` linking the rlib on a miss.
struct BinArtifact<'a> {
    rlib_input: &'a RlibInput,
    rlib_path: &'a Path,
    driver_path: &'a Path,
    bin_input: &'a BinInput,
    compiler_observer: &'a CompilerObserver,
}

impl CompilerAdapter for BinArtifact<'_> {
    type Error = CacheError;

    fn request(&self) -> ArtifactRequest {
        ArtifactRequest {
            kind: RUST_CACHE_KIND.to_owned(),
            subroot_rel: key::subroot_rel(
                &self.rlib_input.rustc_identity,
                &self.rlib_input.target_triple,
            ),
            namespace: "bins".to_owned(),
            role: "bin".to_owned(),
            key_inputs: key::bin_fields(self.bin_input),
            hash_domain: Some(b"bin\0".to_vec()),
            artifact_name: "bin".to_owned(),
            staged_name: None,
            executable: true,
            meta_json: key::bin_meta_json(self.rlib_input, self.bin_input),
        }
    }

    fn produce(&self, ctx: &ProduceCtx<'_>) -> Result<(), CacheError> {
        do_rustc_bin(
            self.rlib_input,
            self.rlib_path,
            self.driver_path,
            self.bin_input,
            self.compiler_observer,
            ctx,
        )
    }
}

fn rustc_command(observer: &CompilerObserver, wrapper: Option<&OsString>) -> Command {
    observer.command(
        std::ffi::OsStr::new("rustc"),
        wrapper.map(std::ffi::OsString::as_os_str),
    )
}

fn rustc_program_name(command: &Command) -> String {
    command.get_program().to_string_lossy().into_owned()
}

/// The stable token every per-compile build path is remapped onto so
/// the emitted artifact embeds no run-specific absolute path. rustc
/// records the source path of the crate it compiles (in panic
/// locations via `file!()`, and in any retained debug info); without a
/// remap that path is the per-run staging / scratch directory, so two
/// compiles of the same sources from different worktrees would embed
/// different bytes — defeating cross-worktree cache reuse. Mapping every
/// varying root onto one token makes the artifact path-neutral. Mirrors
/// go's `-trimpath`.
const REMAP_TOKEN: &str = "/kio-build";

/// Push a `--remap-path-prefix=<dir>=<token>` arg mapping `dir` onto the
/// stable [`REMAP_TOKEN`]. Applied to every per-compile root rustc is
/// handed (the source crate root, the driver dir, the staging out-dir)
/// so no run-specific path survives into the artifact.
fn push_remap(cmd: &mut Command, dir: &Path) {
    let mut arg = std::ffi::OsString::from("--remap-path-prefix=");
    arg.push(dir.as_os_str());
    arg.push("=");
    arg.push(REMAP_TOKEN);
    cmd.arg(arg);
}

fn do_rustc_rlib(
    input: &RlibInput,
    crate_root: &Path,
    observer: &CompilerObserver,
    ctx: &ProduceCtx<'_>,
) -> Result<(), CacheError> {
    let lib_rs = crate_root.join("src").join("lib.rs");
    let mut cmd = rustc_command(observer, ctx.compiler_wrapper);
    cmd.arg("--crate-type=rlib")
        .arg("--crate-name")
        .arg(&input.crate_name)
        .arg("--edition")
        .arg(&input.edition)
        .arg("--target")
        .arg(&input.target_triple)
        .arg("--out-dir")
        .arg(ctx.out_dir);
    // Path-neutral artifact: remap the source crate root and the staging
    // out-dir onto the stable token so the rlib embeds no run-specific
    // path. See `REMAP_TOKEN`.
    push_remap(&mut cmd, crate_root);
    push_remap(&mut cmd, ctx.out_dir);
    for flag in key::rustc_flags(input.profile) {
        cmd.arg(flag);
    }
    cmd.arg(&lib_rs);
    let program = rustc_program_name(&cmd);
    let admitted = ctx
        .compiler_admission
        .acquire_for(&mut cmd)
        .map_err(CacheError::Admission)?;
    let status = admitted
        .status()
        .map_err(|e| CacheError::RustcSpawn { program, source: e })?;
    if !status.success() {
        return Err(CacheError::RustcFailed { status });
    }
    Ok(())
}

fn do_rustc_bin(
    rlib_input: &RlibInput,
    rlib_path: &Path,
    driver_path: &Path,
    bin_input: &BinInput,
    observer: &CompilerObserver,
    ctx: &ProduceCtx<'_>,
) -> Result<(), CacheError> {
    let bin_path = ctx.out_dir.join("bin");
    let mut cmd = rustc_command(observer, ctx.compiler_wrapper);
    cmd.arg("--crate-type=bin")
        .arg("--edition")
        .arg(&rlib_input.edition)
        .arg("--target")
        .arg(&rlib_input.target_triple)
        .arg("--extern")
        .arg(format!(
            "{}={}",
            bin_input.crate_name,
            DisplayPath(&rlib_path)
        ))
        .arg("-o")
        .arg(&bin_path);
    // Path-neutral artifact: remap the driver's source dir and the
    // staging out-dir onto the stable token. See `REMAP_TOKEN`.
    if let Some(driver_dir) = driver_path.parent() {
        push_remap(&mut cmd, driver_dir);
    }
    push_remap(&mut cmd, ctx.out_dir);
    for flag in key::rustc_flags(rlib_input.profile) {
        cmd.arg(flag);
    }
    cmd.arg(driver_path);
    let program = rustc_program_name(&cmd);
    let admitted = ctx
        .compiler_admission
        .acquire_for(&mut cmd)
        .map_err(CacheError::Admission)?;
    let status = admitted
        .status()
        .map_err(|e| CacheError::RustcSpawn { program, source: e })?;
    if !status.success() {
        return Err(CacheError::RustcFailed { status });
    }
    Ok(())
}

/// Probe rustc once and return the stable subset of
/// `rustc --version --verbose` as a single string: the version line,
/// the `commit-hash` field, and the `host` field. Streaming fields
/// (`release`, `LLVM version`) are deliberately excluded.
pub fn rustc_identity(rustc_path: &Path) -> Result<String, CacheError> {
    let out = Command::new(rustc_path)
        .arg("--version")
        .arg("--verbose")
        .output()
        .map_err(|e| CacheError::RustcSpawn {
            program: DisplayPath(rustc_path).to_string(),
            source: e,
        })?;
    if !out.status.success() {
        return Err(CacheError::RustcFailed { status: out.status });
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut version_line = String::new();
    let mut commit_hash = String::new();
    let mut host = String::new();
    for line in stdout.lines() {
        if version_line.is_empty() {
            version_line = line.to_string();
            continue;
        }
        if let Some(rest) = line.strip_prefix("commit-hash:") {
            commit_hash = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("host:") {
            host = rest.trim().to_string();
        }
    }
    Ok(format!(
        "{version_line}\ncommit-hash:{commit_hash}\nhost:{host}"
    ))
}

/// Default host target triple discovered via `rustc -vV`. Used by
/// callers that don't pin a target explicitly.
pub fn default_target_triple(rustc_path: &Path) -> Result<String, CacheError> {
    let out = Command::new(rustc_path)
        .arg("--version")
        .arg("--verbose")
        .output()
        .map_err(|e| CacheError::RustcSpawn {
            program: DisplayPath(rustc_path).to_string(),
            source: e,
        })?;
    if !out.status.success() {
        return Err(CacheError::RustcFailed { status: out.status });
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("host:") {
            return Ok(rest.trim().to_string());
        }
    }
    Err(CacheError::RustcFailed { status: out.status })
}

/// Re-exported framing entry point for callers that need a key without
/// producing — the runner computes the rlib key locally to fold it
/// into the bin input. Delegates to the shared keyer over the rlib's
/// field list.
pub fn rlib_key(input: &RlibInput) -> String {
    crate::build_cache::hash_fields(None, &key::rlib_fields(input))
}
