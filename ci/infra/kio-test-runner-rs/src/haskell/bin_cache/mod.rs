//! Haskell adapter over the shared content-addressed build cache for
//! `kio build haskell` artifacts.
//!
//! The generic keying / locking / publication / eviction machinery
//! lives in [`crate::build_cache`]; this module is the **GHC compiler
//! adapter** over it. It supplies the ghc-specific policy: the compiler
//! identity ([`ghc_identity`]), the key-input field list ([`key`]), the
//! one `ghc --make` invocation, the `meta.json` body, and the `bin`
//! on-disk namespacing. The shared cache owns everything else.
//!
//! A build pays the `ghc --make` cost only once per
//! `(build-tree-content, toolchain, flags)` tuple: racing processes
//! lock-and-wait on the shared cache's per-key file lock, partial
//! writes recover via its atomic-rename publication, and `meta.json`
//! sidecars let `cat` / `jq` inspect entries.
//!
//! ## One-level caching
//!
//! Like the Go adapter, the Haskell runner compiles every `.hs` file in the
//! assembled build tree in a single `ghc --make`. An ordinary run has one
//! emitted facade plus `Main.hs`; coexistence also has its second facade and
//! synthesized host modules. The cache is therefore **one-level**: one
//! [`ArtifactRequest`], one namespace
//! (`bin`), one key folding `{ghc identity, build flags, all `.hs`
//! source files}`. No chained sub-key, no second namespace — see
//! [`crate::build_cache`] for the contrast with the two-level Rust
//! adapter.
//!
//! ## Path-neutral artifacts: GHC has no source-path remap
//!
//! Go's adapter adds `-trimpath` and the Rust adapter adds
//! `--remap-path-prefix` to strip the build directory from the emitted
//! binary, so the bytes are identical across worktrees. **GHC has no
//! analogue** — no `--remap-path-prefix`, `-ffile-prefix-map`, or
//! `-trimpath` flag exists (`-working-dir` changes the working
//! directory but does not remap embedded paths). So the adapter adds no
//! remap flag; faking path-neutrality is not an option.
//!
//! In practice this is a non-issue because the runner never asks for
//! debug info: with no `-g`, GHC embeds no build-directory path in the
//! linked executable at any `--profile` optimization level (`-O0` or
//! `-O2`), and two compiles of the same sources from different
//! directories produce byte-identical binaries. The cross-worktree
//! reuse the cache relies on therefore holds. The residual is narrow:
//! *were* a future change to enable debug info (`-g`) or profiling, GHC
//! would embed source paths with no flag to strip them — a documented
//! Haskell-backend limitation, distinct from go / rust where the
//! toolchain offers a remap.

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

/// Cache-kind segment under the cache root for Haskell artifacts. The
/// shared cache partitions every kind into its own subtree; `"haskell"`
/// is this adapter's.
const HASKELL_CACHE_KIND: &str = "haskell";

/// The published binary's filename inside its `<hex>/` key directory.
const BIN_NAME: &str = "bin";

/// The Haskell adapter's error surface. Carries the shared cache's I/O
/// failures and this adapter's own admission, `ghc`, and spawn failures under
/// one enum. The `produce` closure returns only adapter variants; the
/// shared cache splits the two kinds into [`ProduceError`], which
/// [`unwrap_produce`] folds back.
#[derive(Debug)]
pub enum CacheError {
    /// A cache I/O failure surfaced by the shared machinery (lock,
    /// rename, directory create).
    Io(crate::build_cache::CacheError),
    /// The shared scheduler could not admit the GHC command.
    Admission(crate::compiler_admission::Error),
    /// `ghc --make` exited non-zero (a compile error in the emitted
    /// package or the driver). Carries the captured stderr so the
    /// caller can surface the toolchain's own diagnostics.
    GhcFailed { stderr: String },
    /// `ghc` or its configured observer could not be spawned.
    GhcSpawn { source: std::io::Error },
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
            CacheError::GhcFailed { stderr } => write!(f, "`ghc --make` failed:\n{stderr}"),
            CacheError::GhcSpawn { source } => write!(f, "could not spawn ghc: {source}"),
        }
    }
}

impl std::error::Error for CacheError {}

/// The ghc optimization-level digit for a runner [`OptProfile`]. The
/// `ghc --make` flag is `-O<digit>`. Unlike rust's near-free
/// `opt-level=1`, ghc `-O1` costs real compile time, so `default` stays
/// cheap at `-O0` (coinciding with `unoptimized`); `optimized` is the
/// on-demand thorough `-O2` pass.
fn ghc_opt_level(profile: OptProfile) -> &'static str {
    match profile {
        OptProfile::Unoptimized | OptProfile::Default => "0",
        OptProfile::Optimized => "2",
    }
}

/// The build flags, in the order they're rendered into the key's
/// [`crate::build_cache::InputField::KvPairs`] field and applied to the
/// `ghc --make` invocation. The `-O<level>` optimization level is
/// `--profile`-selected (see [`ghc_opt_level`]); folding it into the key
/// keeps a `-O0` binary from aliasing a `-O2` one. No path-remap flag —
/// GHC has none (see the module docs).
fn build_flags(profile: OptProfile) -> Vec<(String, String)> {
    vec![("-O".to_owned(), ghc_opt_level(profile).to_owned())]
}

/// The cache's public handle. Construct via
/// [`HaskellCache::open`]; `get_or_compile_bin` hangs off
/// `self`. A thin adapter wrapper around the shared [`BuildCache`]; the
/// ghc identity / build flags the adapter keys on are supplied per call
/// through [`BinInput`].
#[derive(Debug, Clone)]
pub struct HaskellCache {
    cache: BuildCache,
    compiler_observer: CompilerObserver,
}

impl HaskellCache {
    /// Open with build-cache policy, compiler observation, and admission.
    pub fn open(
        root: PathBuf,
        compiler_wrapper: Option<OsString>,
        compiler_observer: CompilerObserver,
        max_bytes: Option<u64>,
        compiler_admission: CompilerAdmission,
    ) -> Result<Self, CacheError> {
        let cache = BuildCache::open(root, compiler_wrapper, max_bytes, compiler_admission)?;
        Ok(HaskellCache {
            cache,
            compiler_observer,
        })
    }

    /// Resolve the binary for `tree`. On hit returns the existing binary
    /// path; on miss the shared cache acquires the per-key lock, runs
    /// `ghc --make` into a tempdir, and atomic-renames into place.
    pub fn get_or_compile_bin(&self, tree: &HaskellBuildTree) -> Result<PathBuf, CacheError> {
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

/// The assembled Haskell build tree the runner hands the cache: the
/// toolchain identity and the full set of `(relative path, bytes)`
/// `.hs` files `ghc --make` compiles. This includes every emitted facade and
/// synthesized driver or host module in the assembled tree.
///
/// The runner builds this once. Its file set both *defines the cache
/// key* (via [`BinInput`]) and *is what `produce` writes to disk* before
/// invoking `ghc`, so the keyed content and the compiled content are the
/// same bytes by construction.
#[derive(Debug, Clone)]
pub struct HaskellBuildTree {
    pub ghc_identity: String,
    /// Path-sorted `(relative path, bytes)` for the `.hs` build tree.
    /// A facade keeps its path under the package namespace tree; for example,
    /// namespace `Foo.Runtime` maps to `Foo/Runtime.hs`. GHC discovers `<Ns>`
    /// from `Main` by its module-name = path rule.
    pub files: Vec<(PathBuf, Vec<u8>)>,
    /// The driver's filename `ghc --make` is pointed at (`Main.hs`). GHC
    /// follows its imports to compile the rest of the tree.
    pub main_module: String,
    /// The `--profile`-selected optimization level, mapped to ghc's
    /// `-O<level>` (see [`ghc_opt_level`]). Folded into the cache key via
    /// [`build_flags`] and applied to the `ghc --make` invocation.
    pub profile: OptProfile,
}

impl HaskellBuildTree {
    fn bin_input(&self) -> BinInput {
        BinInput {
            ghc_identity: self.ghc_identity.clone(),
            build_flags: build_flags(self.profile),
            build_files: self.files.clone(),
        }
    }
}

/// The binary artifact adapter: builds the binary's
/// [`ArtifactRequest`] and runs `ghc --make -O<level>` on a miss, where
/// `<level>` is the tree's `--profile`-selected optimization level.
struct BinArtifact<'a> {
    tree: &'a HaskellBuildTree,
    compiler_observer: &'a CompilerObserver,
}

impl CompilerAdapter for BinArtifact<'_> {
    type Error = CacheError;

    fn request(&self) -> ArtifactRequest {
        let input = self.tree.bin_input();
        ArtifactRequest {
            kind: HASKELL_CACHE_KIND.to_owned(),
            subroot_rel: key::subroot_rel(&self.tree.ghc_identity),
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
        do_ghc_build(
            self.tree,
            ctx.out_dir,
            self.compiler_observer,
            ctx.compiler_admission,
        )
    }
}

/// Materialize the `.hs` build tree under `out_dir`, run `ghc --make
/// -O<level>` (the tree's `--profile`-selected level) against it, and
/// stage the binary as `out_dir/bin` for the shared cache to publish.
fn do_ghc_build(
    tree: &HaskellBuildTree,
    out_dir: &Path,
    observer: &CompilerObserver,
    compiler_admission: &CompilerAdmission,
) -> Result<(), CacheError> {
    let build_root = out_dir.join("build");
    write_tree(&build_root, &tree.files)?;

    let bin_path = out_dir.join(BIN_NAME);
    // Confine GHC's `.hi` / `.o` intermediates to a subdir of the
    // staging tempdir so they don't litter the build root and are
    // discarded with the tempdir. (They're the toolchain's own
    // intermediates, distinct from our content-addressed binary cache.)
    let work = out_dir.join(".ghcwork");

    let mut cmd = ghc_compile_command(observer);
    cmd.arg("--make")
        .arg(format!("-O{}", ghc_opt_level(tree.profile)))
        .arg(&tree.main_module)
        .arg("-o")
        .arg(&bin_path)
        .arg("-outputdir")
        .arg(&work)
        .current_dir(&build_root);
    let admitted = compiler_admission
        .acquire_for(&mut cmd)
        .map_err(CacheError::Admission)?;
    let out = admitted
        .output()
        .map_err(|e| CacheError::GhcSpawn { source: e })?;
    if !out.status.success() {
        return Err(CacheError::GhcFailed {
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

fn ghc_compile_command(observer: &CompilerObserver) -> Command {
    observer.command(std::ffi::OsStr::new("ghc"), None)
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

/// Probe GHC once and return a stable identity string: the numeric
/// version (`ghc --numeric-version`) plus the `Target platform` field
/// of `ghc --info`. The platform is part of the identity, so the subroot
/// already partitions by host platform without a separate triple
/// segment.
pub fn ghc_identity() -> Result<String, CacheError> {
    let version = Command::new("ghc")
        .arg("--numeric-version")
        .output()
        .map_err(|e| CacheError::GhcSpawn { source: e })?;
    if !version.status.success() {
        return Err(CacheError::GhcFailed {
            stderr: String::from_utf8_lossy(&version.stderr).into_owned(),
        });
    }
    let version = String::from_utf8_lossy(&version.stdout).trim().to_owned();

    let info = Command::new("ghc")
        .arg("--info")
        .output()
        .map_err(|e| CacheError::GhcSpawn { source: e })?;
    if !info.status.success() {
        return Err(CacheError::GhcFailed {
            stderr: String::from_utf8_lossy(&info.stderr).into_owned(),
        });
    }
    let info = String::from_utf8_lossy(&info.stdout);
    let platform = parse_ghc_target_platform(&info).unwrap_or_default();

    Ok(format!("{version}\n{platform}"))
}

/// Pull the `Target platform` value out of `ghc --info`'s output. The
/// `--info` output is a Haskell-rendered list of `("key","value")`
/// pairs, one per line; this finds the `Target platform` pair and
/// returns its value (e.g. `x86_64-unknown-linux`).
fn parse_ghc_target_platform(info: &str) -> Option<String> {
    for line in info.lines() {
        let line = line.trim().trim_start_matches('[').trim_start_matches(',');
        if let Some(rest) = line.strip_prefix("(\"Target platform\"") {
            // `rest` is like `,"x86_64-unknown-linux")`. Take the text
            // between the next pair of double quotes.
            let after_comma = rest.trim_start_matches(',').trim();
            let inner = after_comma.strip_prefix('"')?;
            let end = inner.find('"')?;
            return Some(inner[..end].to_owned());
        }
    }
    None
}
