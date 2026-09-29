//! Swift adapter over the shared content-addressed build cache for
//! `kio build swift` artifacts.
//!
//! The generic keying / locking / publication / eviction machinery
//! lives in [`crate::build_cache`]; this module is the **swiftc
//! compiler adapter** over it. It supplies the swift-specific policy:
//! the compiler identity ([`swiftc_identity`]), the key-input field
//! list ([`key`]), the two-step module + driver `swiftc` build, the
//! `meta.json` body, and the `bin` on-disk namespacing. The shared
//! cache owns everything else.
//!
//! A build pays the `swiftc` cost only once per
//! `(build-tree-content, toolchain, flags)` tuple: racing processes
//! lock-and-wait on the shared cache's per-key file lock, partial
//! writes recover via its atomic-rename publication, and `meta.json`
//! sidecars let `cat` / `jq` inspect entries.
//!
//! ## One-level caching over a two-step module + driver compile
//!
//! The Swift runner compiles the build tree in **two** `swiftc`
//! invocations, the way a real host builds against a kio package: the
//! emitted `pkg.swift` / `host.swift` / `shapes.swift` / `ffi.swift` /
//! `kio_runtime.swift` compile into the package module `<ns>` (a
//! `.swiftmodule` interface + a **static** `lib<ns>.a`), and the
//! synthesized `main.swift` driver then `import <ns>`s and static-links
//! it into the final binary. Only the driver reaches the package across
//! the module boundary, so a `public`/`internal` slip on any symbol the
//! host needs fails the compile — the fidelity a same-module shortcut
//! could not enforce.
//!
//! The cache is still **one-level**: one [`ArtifactRequest`], one
//! namespace (`bin`), one key folding `{swiftc identity, build flags
//! (incl. the `<ns>` module name), all `.swift` source files}`. Both
//! invocations run on a single miss and stage one binary; there is no
//! chained sub-key and no second cache namespace — see
//! [`crate::build_cache`] for the contrast with the two-level Rust
//! adapter.
//!
//! **Static linking is what keeps the cached binary relocatable.** The
//! `-static` `lib<ns>.a` is folded into the binary, which then depends
//! only on the Swift toolchain's own stdlib (a fixed runpath into the
//! installed toolchain, identical across worktrees), so a cached copy
//! stays runnable wherever the cache is reused. The shape to avoid is a
//! *dynamic* `lib<ns>.so` linked with a tempdir-relative rpath: that
//! binary loads a sibling `.so` from a path that vanishes with the
//! staging tempdir, so a cached copy is useless. A static archive has no
//! such rpath — the relocatability the single-invocation form gave for
//! free, kept across the module boundary.
//!
//! ## Path-neutrality and the module-hash residual
//!
//! The `swiftc` runs with **`-file-prefix-map <build-dir>=/kio-build`**
//! (swift's umbrella source-path remap, covering debug info, coverage,
//! and `#file`), the analogue of go's `-trimpath` and rust's
//! `--remap-path-prefix`. Because the runner asks for no debug info,
//! swift embeds **no build-directory path** in the binary at any
//! `--profile` level (`-Onone` / `-O`) — only the constant toolchain
//! stdlib runpath — so the remap is belt-and-suspenders, exactly the
//! Haskell-adapter situation. It is
//! folded into the key all the same (it changes the output were a path
//! ever embedded), so a remapped build never aliases a non-remapped
//! one.
//!
//! What swift does *not* offer is byte-reproducible output: swiftc
//! stamps a **per-invocation random module hash** into the binary's
//! `.swift_modhash` section (and the `.note.gnu.build-id` that hashes
//! it), so two compiles of the same sources differ in those bytes.
//! `-module-name <ns>` pins the package module's name so its mangled
//! symbols stay stable; only the modhash fingerprint and the build-id
//! vary, and both are fingerprints, not paths — so they do not impair
//! cross-worktree cache reuse: **the cache content-addresses the binary
//! by its inputs (the source bytes), never by its output bytes**. This
//! is a test-infra reproducibility note (it describes the runner's
//! cached artifact, not what `kio build swift` emits), documented in
//! the runner README § Runner build cache and compiler wrappers; it
//! mirrors the Haskell adapter's reproducibility residual, with a
//! different root cause (a nonce rather than nothing to strip).
//!
//! ## Hermetic module cache
//!
//! The `swiftc` invocation pins **`-module-cache-path`** under the
//! staging tempdir. swiftc's default writes clang's implicit module
//! cache — the `.pcm` chain the driver's `import Foundation` pulls
//! (SwiftGlibc / Dispatch / Foundation) — into the machine-shared
//! `~/.cache/clang/ModuleCache`, state outside the runner's own cache
//! root. The runner-hygiene norm (the runner README § Runner build cache
//! and compiler wrappers) forbids that: a runner writes machine-shared
//! state only under its build-cache dir and its staging tempdirs. This
//! is the exact analogue of the Go adapter's per-compile `GOCACHE` — the
//! toolchain's incidental module cache, distinct from *our*
//! content-addressed binary (which the shared build cache owns), and
//! discarded with the tempdir. It re-derives only on an artifact-cache
//! miss (a warm hit skips `swiftc` entirely); the module rebuild is a
//! small constant per miss.

use crate::build_cache::{ArtifactRequest, BuildCache, CompilerAdapter, ProduceCtx, ProduceError};
use crate::compiler_admission::CompilerAdmission;
use crate::compiler_observer::CompilerObserver;
use crate::opt_profile::OptProfile;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub mod key;

#[cfg(test)]
mod tests;

pub use key::BinInput;

/// Cache-kind segment under the cache root for Swift artifacts. The
/// shared cache partitions every kind into its own subtree; `"swift"`
/// is this adapter's.
const SWIFT_CACHE_KIND: &str = "swift";

/// The published binary's filename inside its `<hex>/` key directory.
const BIN_NAME: &str = "bin";

/// The remapped build-directory prefix folded into both the
/// `-file-prefix-map` flag value and the cache key. The actual build
/// dir is mapped *to* this fixed string so two compiles of the same
/// sources from different worktrees agree on the remapped half.
pub const REMAP_TO: &str = "/kio-build";

/// The Swift adapter's error surface. Carries the shared cache's I/O
/// failures and this adapter's own admission, `swiftc`, and spawn failures
/// under one enum. The `produce` closure returns only adapter variants; the
/// shared cache splits the two kinds into [`ProduceError`], which
/// [`unwrap_produce`] folds back.
#[derive(Debug)]
pub enum CacheError {
    /// A cache I/O failure surfaced by the shared machinery (lock,
    /// rename, directory create).
    Io(crate::build_cache::CacheError),
    /// The shared scheduler could not admit the swiftc command.
    Admission(crate::compiler_admission::Error),
    /// `swiftc` exited non-zero (a compile error in the emitted package
    /// or the driver). Carries the captured stderr so the caller can
    /// surface the toolchain's own diagnostics.
    SwiftcFailed { stderr: String },
    /// `swiftc` or its configured observer could not be spawned.
    SwiftcSpawn { source: std::io::Error },
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
            CacheError::SwiftcFailed { stderr } => write!(f, "`swiftc` failed:\n{stderr}"),
            CacheError::SwiftcSpawn { source } => write!(f, "could not spawn swiftc: {source}"),
        }
    }
}

impl std::error::Error for CacheError {}

/// The swiftc optimization flag for a runner [`OptProfile`]. Swift has a
/// single optimizing level (`-O`), reserved for `optimized`; unlike
/// rust's near-free `opt-level=1`, `-O` costs real compile time, so
/// `default` stays cheap at `-Onone` (coinciding with `unoptimized` —
/// same flag, same cache key, one shared entry). `optimized` is the
/// on-demand thorough `-O` pass.
fn swift_opt_flag(profile: OptProfile) -> &'static str {
    match profile {
        OptProfile::Unoptimized | OptProfile::Default => "-Onone",
        OptProfile::Optimized => "-O",
    }
}

/// The build flags that affect the emitted binary, in the order they're
/// rendered into the key's [`crate::build_cache::InputField::KvPairs`]
/// field. The `-O<suffix>` optimization level is `--profile`-selected
/// (see [`swift_opt_flag`]) and folded into the key so a `-Onone` binary
/// never aliases a `-O` one. `-file-prefix-map` records that the build
/// dir is remapped to the fixed [`REMAP_TO`] prefix — only the *target*
/// of the remap participates in the key, since the source half (the
/// per-compile build dir) is collapsed to that constant.
///
/// `produce` re-derives the live `-file-prefix-map <build-dir>=/kio-build`
/// from the staging dir; this list is what the key folds, so the key is
/// build-dir-independent while still recording that a remap is in
/// effect.
///
/// `module_name` is the harness-supplied artifact namespace that the package
/// module is built under (`-module-name`) and the driver imports. It affects
/// symbol mangling and the driver's `import` / factory spellings, so it is
/// folded into the key (belt-and-suspenders: it normally also rides the
/// emitted source layout in the file set).
fn build_flags(profile: OptProfile, module_name: &str) -> Vec<(String, String)> {
    // Fold the flag as `("-O", <suffix>)` (suffix `none` or empty) so the
    // KV stays in lockstep with the `swiftc` argument and the historical
    // `-Onone` entry keeps its key.
    let opt_suffix = swift_opt_flag(profile)
        .strip_prefix("-O")
        .unwrap_or_default()
        .to_owned();
    vec![
        ("-O".to_owned(), opt_suffix),
        ("-module-name".to_owned(), module_name.to_owned()),
        ("-file-prefix-map".to_owned(), REMAP_TO.to_owned()),
    ]
}

/// The cache's public handle. Construct via
/// [`SwiftCache::open`]; `get_or_compile_bin` hangs off
/// `self`. A thin adapter wrapper around the shared [`BuildCache`]; the
/// swiftc identity / build flags the adapter keys on are supplied per
/// call through [`BinInput`].
#[derive(Debug, Clone)]
pub struct SwiftCache {
    cache: BuildCache,
    compiler_observer: CompilerObserver,
}

impl SwiftCache {
    /// Open with build-cache policy, compiler observation, and admission.
    pub fn open(
        root: PathBuf,
        compiler_wrapper: Option<OsString>,
        compiler_observer: CompilerObserver,
        max_bytes: Option<u64>,
        compiler_admission: CompilerAdmission,
    ) -> Result<Self, CacheError> {
        let cache = BuildCache::open(root, compiler_wrapper, max_bytes, compiler_admission)?;
        Ok(SwiftCache {
            cache,
            compiler_observer,
        })
    }

    /// Resolve the binary for `tree`. On hit returns the existing binary
    /// path; on miss the shared cache acquires the per-key lock, runs
    /// the two `swiftc` invocations into a tempdir, and atomic-renames
    /// into place.
    pub fn get_or_compile_bin(&self, tree: &SwiftBuildTree) -> Result<PathBuf, CacheError> {
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

/// The assembled Swift build tree the runner hands the cache: the
/// toolchain identity, the package module name, and the full set of
/// `(relative path, bytes)` `.swift` files the two `swiftc` invocations
/// compile (the emitted `pkg.swift` / `host.swift` / `shapes.swift` /
/// `ffi.swift` / `kio_runtime.swift` and the synthesized `main.swift`).
///
/// The runner builds this once. Its file set both *defines the cache
/// key* (via [`BinInput`]) and *is what `produce` writes to disk* before
/// invoking `swiftc`, so the keyed content and the compiled content are
/// the same bytes by construction.
#[derive(Debug, Clone)]
pub struct SwiftBuildTree {
    pub swiftc_identity: String,
    /// Path-sorted `(relative path, bytes)` for the `.swift` build tree:
    /// the emitted package's files plus the synthesized `main.swift`
    /// driver. The two build steps partition this set — the package files
    /// compile into the `<module_name>` module, `main.swift` imports it.
    pub files: Vec<(PathBuf, Vec<u8>)>,
    /// The harness-supplied artifact namespace that the package module is
    /// built under (`-module-name`) and the driver imports. Folded into the
    /// cache key via [`build_flags`].
    pub module_name: String,
    /// The `--profile`-selected optimization level, mapped to swiftc's
    /// `-Onone` / `-O` (see [`swift_opt_flag`]). Folded into the cache
    /// key via [`build_flags`] and applied to the `swiftc` invocation.
    pub profile: OptProfile,
}

impl SwiftBuildTree {
    fn bin_input(&self) -> BinInput {
        BinInput {
            swiftc_identity: self.swiftc_identity.clone(),
            build_flags: build_flags(self.profile, &self.module_name),
            build_files: self.files.clone(),
        }
    }
}

/// The binary artifact adapter: builds the binary's
/// [`ArtifactRequest`] and runs the two `swiftc` invocations on a
/// miss.
struct BinArtifact<'a> {
    tree: &'a SwiftBuildTree,
    compiler_observer: &'a CompilerObserver,
}

impl CompilerAdapter for BinArtifact<'_> {
    type Error = CacheError;

    fn request(&self) -> ArtifactRequest {
        let input = self.tree.bin_input();
        ArtifactRequest {
            kind: SWIFT_CACHE_KIND.to_owned(),
            subroot_rel: key::subroot_rel(&self.tree.swiftc_identity),
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
        do_swiftc_build(
            self.tree,
            ctx.out_dir,
            self.compiler_observer,
            ctx.compiler_admission,
        )
    }
}

/// Materialize the `.swift` build tree under `out_dir`, run the two
/// `swiftc` invocations (build the package module, then the importing
/// driver), and stage the binary as `out_dir/bin` for the shared cache to
/// publish.
fn do_swiftc_build(
    tree: &SwiftBuildTree,
    out_dir: &Path,
    observer: &CompilerObserver,
    compiler_admission: &CompilerAdmission,
) -> Result<(), CacheError> {
    let build_root = out_dir.join("build");
    write_tree(&build_root, &tree.files)?;

    // Partition the `.swift` files: the emitted package's files compile
    // into the `<ns>` module a host `import`s; `main.swift` is the
    // importing driver, compiled and linked against that module — a real
    // host's two-step build, so a `public`/`internal` slip on any symbol
    // the driver reaches now fails the driver compile (the module boundary
    // the single-module shortcut could not enforce). Bare filenames keep
    // the only absolute path swiftc sees the remap source.
    let mut pkg_names: Vec<String> = Vec::new();
    let mut driver_name: Option<String> = None;
    for (rel, _) in &tree.files {
        if rel.extension().is_none_or(|x| x != "swift") {
            continue;
        }
        let name = rel.to_string_lossy().into_owned();
        if name == "main.swift" {
            driver_name = Some(name);
        } else {
            pkg_names.push(name);
        }
    }
    pkg_names.sort();
    let driver_name = driver_name.ok_or_else(|| CacheError::SwiftcFailed {
        stderr: "swift build tree has no `main.swift` driver".to_owned(),
    })?;

    let ns = &tree.module_name;
    let bin_path = out_dir.join(BIN_NAME);
    // Remap the build dir to the fixed prefix so no build-directory path
    // can leak into the binary (belt-and-suspenders with no debug info at
    // any -O level; see the module docs). swiftc accepts the source half
    // as the absolute build root.
    let prefix_map = format!("{}={}", build_root.display(), REMAP_TO);

    // Pin clang's implicit module cache under the staging tempdir, shared
    // by both invocations. Left to its default, swiftc writes the `.pcm`
    // chain its `import Foundation` driver pulls (SwiftGlibc / Dispatch /
    // Foundation, ~17M) into the machine-shared `~/.cache/clang/ModuleCache`
    // — state outside the runner's own cache root. The runner build cache
    // owns the artifact; this is the toolchain's incidental module cache,
    // and per the runner-hygiene norm (README § Runner build cache and
    // compiler wrappers) it must not escape the tempdir. Mirrors the Go
    // adapter's per-compile GOCACHE; discarded with the tempdir. It re-
    // derives only on an artifact-cache miss (a warm hit skips swiftc
    // entirely), a small constant per miss.
    let module_cache = out_dir.join(".module-cache");

    let opt = swift_opt_flag(tree.profile);
    let swiftmodule = format!("{ns}.swiftmodule");
    let lib = format!("lib{ns}.a");

    // Invocation 1 — the package module `<ns>`: a static archive plus its
    // `.swiftmodule` interface, so the driver can `import <ns>` and the
    // final binary static-links it (a static `.a`, not a dynamic `.so`
    // with a tempdir-relative rpath — that shape was non-relocatable; see
    // the module docs). No `-num-threads`: at the corpus's per-file scale
    // the compile is dominated by swiftc startup + frontend, not codegen.
    let mut cmd1 = swiftc_command(observer);
    cmd1.arg(opt)
        .arg("-module-name")
        .arg(ns)
        .arg("-file-prefix-map")
        .arg(&prefix_map)
        .arg("-module-cache-path")
        .arg(&module_cache)
        .arg("-emit-module")
        .arg("-emit-module-path")
        .arg(&swiftmodule)
        .arg("-emit-library")
        .arg("-static")
        .arg("-o")
        .arg(&lib)
        .args(&pkg_names)
        .current_dir(&build_root);
    run_swiftc(cmd1, compiler_admission)?;

    // Invocation 2 — the driver: `import <ns>` resolves against the
    // `.swiftmodule` in the build root; static-linking `lib<ns>.a` folds
    // the package into the self-contained, relocatable binary.
    let mut cmd2 = swiftc_command(observer);
    cmd2.arg(opt)
        .arg("-file-prefix-map")
        .arg(&prefix_map)
        .arg("-module-cache-path")
        .arg(&module_cache)
        .arg("-I")
        .arg(".")
        .arg("-L")
        .arg(".")
        .arg(format!("-l{ns}"))
        .arg(&driver_name)
        .arg("-o")
        .arg(&bin_path)
        .current_dir(&build_root);
    run_swiftc(cmd2, compiler_admission)?;

    Ok(())
}

/// A `swiftc` [`Command`], optionally prefixed by the debug observer.
fn swiftc_command(observer: &CompilerObserver) -> Command {
    observer.command(std::ffi::OsStr::new("swiftc"), None)
}

/// Run one `swiftc` invocation, mapping a spawn failure / non-zero exit
/// to [`CacheError`].
fn run_swiftc(mut cmd: Command, compiler_admission: &CompilerAdmission) -> Result<(), CacheError> {
    let admitted = compiler_admission
        .acquire_for(&mut cmd)
        .map_err(CacheError::Admission)?;
    let out = admitted
        .output()
        .map_err(|e| CacheError::SwiftcSpawn { source: e })?;
    if !out.status.success() {
        return Err(CacheError::SwiftcFailed {
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
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

/// Probe `swiftc --version` once and return its trimmed stdout as the
/// toolchain identity (e.g. `Swift version 6.3.2 (swift-6.3.2-RELEASE)
/// \nTarget: x86_64-unknown-linux-gnu`). The `Target:` line is part of
/// the identity, so the subroot already partitions by host platform
/// without a separate triple segment.
pub fn swiftc_identity() -> Result<String, CacheError> {
    let out = Command::new("swiftc")
        .arg("--version")
        .output()
        .map_err(|e| CacheError::SwiftcSpawn { source: e })?;
    if !out.status.success() {
        return Err(CacheError::SwiftcFailed {
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
