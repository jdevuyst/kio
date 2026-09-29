//! `kio-test-runner-haskell` — pointed at a `kio build haskell` output
//! directory, compiles the emitted Haskell module together with a
//! synthesized driver and reports the exit code.
//!
//! The package layout the emitter produces (see
//! `specs/backends/haskell.md` § Output layout) is the package module
//! `<Ns>.hs` (the invocable surface: the `<Handle>HostTypes h` class, the
//! `<Handle>Host h m` value record, the structural families and boundary
//! patterns, the
//! `<Handle> h m` handle, the `create<Handle>` factory, every exported
//! boundary wrapper, every module fn as a monad-polymorphic top-level
//! function, and unexported private runtime support), where `<Ns>` is the
//! package's namespace. The runner assembles that facade at its relative path
//! under the namespace tree alongside a synthesized `Main.hs` driver that imports the package
//! qualified (`import qualified <Ns> as Pkg`) through its documented public
//! facade, then resolves the binary through the shared
//! content-addressed build cache ([`haskell_cache`], a one-level adapter
//! over [`build_cache`]): a warm run returns the cached binary, a miss
//! runs `ghc --make Main.hs` (GHC discovers `<Ns>` by its module-name = path
//! rule, compiles it, and links a binary) into a staging tempdir. Compiling the driver
//! as a separate module that imports the emitted package exercises the
//! package's exported surface as a real cross-module boundary: an emitter
//! export-list slip surfaces as a GHC "not in scope" error in the driver.
//!
//! The binary is keyed by `{ghc identity, build flags, all `.hs` source
//! bytes}` — the build flags include the `--profile`-selected `-O<level>`
//! so an `-O0` binary never aliases an `-O2` one. Unlike go
//! (`-trimpath`) and rust (`--remap-path-prefix`), **GHC offers no
//! source-path remap flag**, so the adapter strips no path. Since the
//! runner asks for no debug info, this is moot at every `-O` level — GHC
//! embeds no build path and produces byte-identical binaries across
//! build directories, so the cache reuses across worktrees regardless.
//! See [`haskell_cache`]'s module docs for the residual.
//!
//! ## The protocol is the sole semantic authority
//!
//! The host API this runner implements comes from the **protocol**
//! (`--protocol <name>`): one complete contract whose host types, host
//! functions, native fixtures and bodies, exports, and execution are baked
//! in. The corpus harness supplies the
//! source package name and target-local effective namespace from which every branded facade
//! name (`<Handle>`, `<Handle>Host`, `create<Handle>`) derives per
//! `specs/backends/haskell.md` § Output layout. The runner derives those
//! names independently and reads emitted `.hs` files only as compiler input.
//! Every host type, host fn, native
//! fixture, host body, and export invocation comes exclusively from the
//! selected protocol contract. Exact associated-family, record-field, and
//! boundary-alias spellings are independently reconstructed from the public
//! Haskell ABI using the contract's module/item identities and the supplied
//! artifact namespace. The runner never inspects emitted source to discover
//! or filter semantic members. It builds a
//! `stubHost` value of the emitted `<Handle>Host h m` record type; Haskell
//! type-checks the record construction at compile time, so if the emitted
//! FFI drifts, the `stubHost` stops type-checking and `ghc` fails — the
//! regression surfaces at compile time.
//!
//! ## Native-typed host bodies
//!
//! The Haskell backend emits a **native-typed** boundary through exact host
//! associated families. For a `role(str)` family, this runner's marker
//! instance chooses `Data.Text.Text`; its `print` stub can therefore use the
//! native `\s -> Data.Text.IO.putStr s` body in `IO`. Production hosts choose
//! their own family equations and monad.
//!
//! Exits 0 on success, 1 on a `ghc` / runtime error; the CLI tier is 2 per
//! `specs/exit-codes.md`. A module call to host `exit(n)` propagates `n`
//! through the spawned process's exit code (clamped to 0..=125 per
//! `specs/exit-codes.md`).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

#[path = "../shared/artifact_identity.rs"]
mod artifact_identity;
#[path = "../shared/build_cache/mod.rs"]
mod build_cache;
#[path = "../shared/canonical.rs"]
mod canonical;
#[path = "../shared/compiler_observer.rs"]
mod compiler_observer;
use kio_ci_scheduler as compiler_admission;
#[path = "../haskell/abi.rs"]
mod haskell_abi;
#[path = "../haskell/bin_cache/mod.rs"]
mod haskell_cache;
#[path = "../shared/host_api.rs"]
mod host_api;
#[path = "../shared/opt_profile.rs"]
mod opt_profile;
#[path = "../shared/path_display.rs"]
mod path_display;
#[path = "../shared/protocol.rs"]
mod protocol;
#[path = "../shared/runner.rs"]
mod runner;
#[path = "../shared/runner_cache_env.rs"]
mod runner_cache_env;

use artifact_identity::{ArtifactIdentity, ArtifactIdentityArgs};
use canonical::{ArrayOp, CanonicalKind};
use compiler_observer::CompilerObserver;
use haskell_abi::{BoundaryId, BoundaryRoot, BoundaryStep, ItemId, ModuleItemId, StructuralKey};
use haskell_cache::{HaskellBuildTree, HaskellCache, ghc_identity};
use host_api::{AssocType, HostApi, TraitMethod};
use opt_profile::OptProfile;
use protocol::{
    ExportDriver, HostFnBinding, HostFnBodyKind, HostRoleRef, HostTypeBinding, HostTypeFixture,
    HostTypeIdentity, ProtocolExecution, RoleFixture, RunnerProtocol, WIDE_CALLABLE_SLOT_COUNT,
};
use runner::{EXIT_RUNTIME_FAILURE, EXIT_USAGE, TestRunner};
use runner_cache_env::RunnerCacheConfig;

const USAGE: &str = "\
Usage: kio-test-runner-haskell [--protocol <name>] [--profile <name>] <output-dir>

Compile and run the Haskell module emitted by `kio build haskell` and
report the exit code.

Arguments:
  <output-dir>      Directory containing the emitted Haskell package
                    facade (`<Ns>.hs`, where dotted namespace components map
                    to directories and `<Ns>` is supplied independently).

Environment:
  KIO_TEST_RUNNER_BUILD_CACHE_DIR
                    Required. Directory where reusable runner-built
                    binaries live. Must persist across invocations for
                    warm-cache benefits, unless
                    KIO_TEST_RUNNER_CACHE_DISABLE=1.

  KIO_TEST_RUNNER_BUILD_CACHE_SIZE
                    Optional. Maximum persistent cache size. Accepts
                    bytes or K/M/G/T suffixes.

  KIO_TEST_RUNNER_COMPILER_WRAPPER
                    Optional Rust compiler cache wrapper. The Haskell
                    runner ignores it because sccache does not wrap `ghc`.

  KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER
                    Optional internal debug executable placed outermost
                    around each actual `ghc --make`. The value is one
                    opaque executable, not shell syntax.

  KIO_TEST_RUNNER_CACHE_DISABLE
                    Optional. Set to 1 to disable persistent runner
                    cache behavior and compiler wrappers; the runner
                    uses a fresh temporary cache for the invocation. The
                    debug compiler observer remains active. Empty, unset,
                    or 0 means enabled; any other value is an error.

  KIO_TEST_RUNNER_PROFILE
                    Optional. Optimization profile for the compile:
                    `unoptimized` or `default` (both -O0; ghc's mild -O1
                    costs real compile time, so default stays cheap), or
                    `optimized` (-O2). Defaults to `default`. --profile
                    overrides it.

Other options:
  --package-name <name>
                    Kio source package name supplied by the corpus harness.
                    Repeat exactly twice for `--protocol coexist`.
  --artifact-namespace <namespace>
                    Effective namespace of the preceding package
                    artifact.
  --protocol <name>
                    Host/Kio interaction protocol to run. Defaults to
                    `empty-main`, the exact empty-host contract. See the
                    runner README for the catalogue.
  --profile <name>
                    Optimization profile: `unoptimized`, `default`, or
                    `optimized`. Overrides KIO_TEST_RUNNER_PROFILE.
                    The profile feeds the artifact cache key.
  -h, --help        Show this help and exit.
";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    process::exit(run(&args));
}

fn run(args: &[String]) -> i32 {
    let mut positional: Vec<&str> = Vec::new();
    let mut protocol = RunnerProtocol::default();
    let mut protocol_seen = false;
    let mut profile_override: Option<OptProfile> = None;
    let mut profile_seen = false;
    let mut identity_args = ArtifactIdentityArgs::default();
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match identity_args.consume(a, &mut iter) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => {
                eprintln!("error: {e}");
                return EXIT_USAGE;
            }
        }
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            "--protocol" => {
                if protocol_seen {
                    eprintln!("error: --protocol specified more than once");
                    return EXIT_USAGE;
                }
                let Some(name) = iter.next() else {
                    eprintln!("error: --protocol requires a value");
                    return EXIT_USAGE;
                };
                protocol = match RunnerProtocol::parse(name) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return EXIT_USAGE;
                    }
                };
                protocol_seen = true;
            }
            s if s.starts_with("--protocol=") => {
                if protocol_seen {
                    eprintln!("error: --protocol specified more than once");
                    return EXIT_USAGE;
                }
                protocol = match RunnerProtocol::parse(&s["--protocol=".len()..]) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return EXIT_USAGE;
                    }
                };
                protocol_seen = true;
            }
            "--profile" => {
                if profile_seen {
                    eprintln!("error: --profile specified more than once");
                    return EXIT_USAGE;
                }
                let Some(name) = iter.next() else {
                    eprintln!("error: --profile requires a value");
                    return EXIT_USAGE;
                };
                profile_override = match OptProfile::parse(name) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return EXIT_USAGE;
                    }
                };
                profile_seen = true;
            }
            s if s.starts_with("--profile=") => {
                if profile_seen {
                    eprintln!("error: --profile specified more than once");
                    return EXIT_USAGE;
                }
                profile_override = match OptProfile::parse(&s["--profile=".len()..]) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return EXIT_USAGE;
                    }
                };
                profile_seen = true;
            }
            s if s.starts_with("--") => {
                eprintln!("error: unknown option: {s}");
                return EXIT_USAGE;
            }
            s => positional.push(s),
        }
    }

    // The coexist protocol is the one two-artifact protocol: exactly two
    // positional output dirs; every other protocol takes exactly one.
    let is_coexist = matches!(
        protocol.contract().execution,
        ProtocolExecution::Invoke(ExportDriver::Coexist)
    );
    let (dir, coexist_second) = match (is_coexist, positional.as_slice()) {
        (true, [a, b]) => (Path::new(*a), Some(Path::new(*b))),
        (true, _) => {
            eprintln!("error: --protocol coexist takes exactly two <output-dir> arguments");
            return EXIT_USAGE;
        }
        (false, [d]) => (Path::new(*d), None),
        _ => {
            eprintln!("{USAGE}");
            return EXIT_USAGE;
        }
    };
    let expected_packages = if is_coexist { 2 } else { 1 };
    let identities = match identity_args.resolve("haskell", expected_packages) {
        Ok(identities) => identities,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    let cache_config = match RunnerCacheConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };
    let compiler_observer = match CompilerObserver::from_env() {
        Ok(observer) => observer,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };
    let compiler_admission = match compiler_admission::CompilerAdmission::from_env() {
        Ok(admission) => admission,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    let profile = match OptProfile::resolve(profile_override) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    let r = HaskellRunner {
        cache_config,
        compiler_observer,
        compiler_admission,
        protocol,
        profile,
        identities,
    };
    if let Some(second) = coexist_second {
        return match r.run_coexist(dir, second) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: executing coexist artifacts: {e}");
                EXIT_RUNTIME_FAILURE
            }
        };
    }
    r.run(dir, protocol)
}

struct HaskellRunner {
    cache_config: RunnerCacheConfig,
    compiler_observer: CompilerObserver,
    compiler_admission: compiler_admission::CompilerAdmission,
    protocol: RunnerProtocol,
    identities: Vec<ArtifactIdentity>,
    /// The `--profile`-selected optimization level (see [`opt_profile`]);
    /// maps to ghc's `-O0` (unoptimized/default) or `-O2` (optimized) and
    /// feeds the build cache key.
    profile: OptProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContractHostType {
    assoc: AssocType,
    module_path: String,
    source_name: String,
    param_kind_arities: Vec<usize>,
    fixture: HostTypeFixture,
}

impl TestRunner for HaskellRunner {
    fn host_api(&self) -> HostApi {
        let ns = self.identities[0].namespace.as_str();
        haskell_host_api_for_protocol(self.protocol, ns)
    }

    fn execute_artifact(
        &self,
        output_dir: &Path,
        host: &HostApi,
        protocol: RunnerProtocol,
    ) -> Result<i32, String> {
        let ns = self.identities[0].namespace.as_str();
        let host_types = contract_host_types(protocol, ns);
        let driver = build_driver(host, protocol, ns, &host_types)?;

        // Assemble the build tree's `.hs` file set in memory: the emitted
        // package facade `<Ns>.hs` at its relative namespace path plus the
        // synthesized `Main.hs` driver. This same set both defines the cache
        // key and is what `produce` writes to disk before `ghc --make` — so
        // the keyed and compiled bytes coincide.
        let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        collect_haskell_files(output_dir, &mut files)?;
        files.push((PathBuf::from("Main.hs"), driver.into_bytes()));
        files.sort_by(|a, b| a.0.cmp(&b.0));

        let ghc_id = ghc_identity().map_err(|e| format!("probing ghc identity: {e}"))?;
        let tree = HaskellBuildTree {
            ghc_identity: ghc_id,
            files,
            main_module: "Main.hs".to_owned(),
            profile: self.profile,
        };

        // Resolve the binary through the shared cache. A warm hit skips
        // `ghc --make` entirely; a miss compiles into a staging tempdir
        // and atomic-renames into place.
        //
        // `compiler_wrapper` (typically `sccache`) is deliberately
        // *not* threaded into the `ghc` invocation: sccache wraps
        // C/C++/rustc-shaped compilers and rejects `ghc`. Like the JS
        // runner, the haskell runner ignores the wrapper. The shared
        // build cache is the runner's acceleration layer here.
        let (cache, _disabled_cache_temp) = self.open_cache()?;

        let bin_path = cache
            .get_or_compile_bin(&tree)
            .map_err(|e| format!("building Haskell bin for {}: {e}", output_dir.display()))?;

        let status = Command::new(&bin_path)
            .status()
            .map_err(|e| format!("spawning driver bin {}: {e}", bin_path.display()))?;
        Ok(status.code().unwrap_or(EXIT_RUNTIME_FAILURE))
    }
}

impl HaskellRunner {
    fn open_cache(&self) -> Result<(HaskellCache, Option<tempfile::TempDir>), String> {
        let (cache_dir, max_bytes, disabled_cache_temp, cache_label) = match &self.cache_config {
            RunnerCacheConfig::Persistent {
                cache_dir,
                compiler_wrapper: _,
                max_bytes,
            } => (cache_dir.clone(), *max_bytes, None, "haskell build cache"),
            RunnerCacheConfig::Disabled => {
                let temp = tempfile::TempDir::new()
                    .map_err(|e| format!("cannot create disabled-cache tempdir: {e}"))?;
                (
                    temp.path().to_path_buf(),
                    None,
                    Some(temp),
                    "disabled-cache temp haskell cache",
                )
            }
        };
        let cache = HaskellCache::open(
            cache_dir.clone(),
            None,
            self.compiler_observer.clone(),
            max_bytes,
            self.compiler_admission.clone(),
        )
        .map_err(|e| format!("cannot open {cache_label} at {}: {e}", cache_dir.display()))?;
        Ok((cache, disabled_cache_temp))
    }
    /// The `coexist` protocol's two-artifact execution
    /// (`shared/protocol.rs` § The coexist protocol): both packages'
    /// namespace trees land in one build root — the maximal-collision
    /// shape the namespace rule exists for — and one `Main.hs`
    /// imports both qualified, hosting each behind a positional
    /// prefixing host record. The whole two-package file set rides the
    /// same content-addressed build cache as the one-package path.
    fn run_coexist(&self, dir_a: &Path, dir_b: &Path) -> Result<i32, String> {
        let ns_a = self.identities[0].namespace.as_str();
        let ns_b = self.identities[1].namespace.as_str();
        if ns_a == ns_b {
            return Err(format!(
                "coexist requires two distinct package namespaces; both artifacts are `{ns_a}`"
            ));
        }
        let host_types_a: Vec<_> = contract_host_types(self.protocol, ns_a)
            .into_iter()
            .map(|binding| binding.assoc)
            .collect();
        let host_types_b: Vec<_> = contract_host_types(self.protocol, ns_b)
            .into_iter()
            .map(|binding| binding.assoc)
            .collect();
        let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        collect_haskell_files(dir_a, &mut files)?;
        collect_haskell_files(dir_b, &mut files)?;
        let (host_module_a, host_module_b) = coexist_host_module_names(&files);
        let driver = coexist_driver_source(
            ns_a,
            ns_b,
            &host_module_a,
            &host_module_b,
            &host_types_a,
            &host_types_b,
        )?;
        files.push((PathBuf::from("Main.hs"), driver.main.into_bytes()));
        files.push((
            PathBuf::from(format!("{host_module_a}.hs")),
            driver.host_a.into_bytes(),
        ));
        files.push((
            PathBuf::from(format!("{host_module_b}.hs")),
            driver.host_b.into_bytes(),
        ));
        files.sort_by(|a, b| a.0.cmp(&b.0));

        let ghc_id = ghc_identity().map_err(|e| format!("probing ghc identity: {e}"))?;
        let tree = HaskellBuildTree {
            ghc_identity: ghc_id,
            files,
            main_module: "Main.hs".to_owned(),
            profile: self.profile,
        };
        let (cache, _disabled_cache_temp) = self.open_cache()?;
        let bin_path = cache
            .get_or_compile_bin(&tree)
            .map_err(|e| format!("building Haskell coexist bin: {e}"))?;
        let status = Command::new(&bin_path)
            .status()
            .map_err(|e| format!("spawning driver bin {}: {e}", bin_path.display()))?;
        Ok(status.code().unwrap_or(EXIT_RUNTIME_FAILURE))
    }
}

/// Collect the emitted Haskell package's `*.hs` files into `files`, each
/// keyed by its path **relative to `src`** so the namespace tree
/// is preserved when the cache writes the build tree. A dotted facade such as
/// `Foo.Runtime` lives at `Foo/Runtime.hs`, so the walk remains recursive even
/// though each artifact now contains one source file. The runner reads files
/// only as opaque bytes to compile.
fn collect_haskell_files(src: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<(), String> {
    collect_haskell_files_under(src, src, files)?;
    if files.is_empty() {
        return Err(format!("no .hs files in {}", src.display()));
    }
    Ok(())
}

/// Recurse `dir` (a subtree of `root`), pushing each `.hs` file at its
/// path relative to `root`.
fn collect_haskell_files_under(
    root: &Path,
    dir: &Path,
    files: &mut Vec<(PathBuf, Vec<u8>)>,
) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            collect_haskell_files_under(root, &path, files)?;
        } else if path.extension().is_some_and(|x| x == "hs") {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_path_buf();
            let bytes = fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
            files.push((rel, bytes));
        }
    }
    Ok(())
}

/// Reconstruct the exact Haskell associated-family inventory from the
/// protocol's source identities. This mirrors the public Haskell name ABI;
/// it does not inspect emitted source.
fn contract_host_types(protocol: RunnerProtocol, target_namespace: &str) -> Vec<ContractHostType> {
    protocol
        .contract()
        .host_types
        .iter()
        .map(|binding| contract_host_type(binding, target_namespace))
        .collect()
}

fn contract_host_type(binding: &HostTypeBinding, target_namespace: &str) -> ContractHostType {
    ContractHostType {
        assoc: AssocType {
            name: haskell_abi::host_assoc(target_namespace, binding.module, binding.leaf),
            boundary_name: None,
            role: match binding.fixture {
                HostTypeFixture::Role(role) | HostTypeFixture::SelectedRole(role) => {
                    role.role().to_owned()
                }
                _ => String::new(),
            },
            type_params: (0..binding.type_arity)
                .map(|index| format!("t{index}"))
                .collect(),
        },
        module_path: binding.module.to_owned(),
        source_name: binding.leaf.to_owned(),
        param_kind_arities: vec![0; binding.type_arity as usize],
        fixture: binding.fixture,
    }
}

fn haskell_assoc_ref(
    target_namespace: &str,
    identity: HostTypeIdentity,
    type_args: &[&str],
) -> String {
    let member = haskell_abi::host_assoc(target_namespace, identity.module, identity.leaf);
    if type_args.is_empty() {
        format!("Self::{member}")
    } else {
        format!("Self::{member}<{}>", type_args.join(", "))
    }
}

/// The branded package handle for a namespace: its final `.`-separated
/// segment (`Com.Acme.Greeter` → `Greeter`), matching the emitter's
/// derivation so the driver's `Pkg.create<Handle>` / `Pkg.<Handle>Host`
/// spellings agree with the emitted names.
fn handle_of(ns: &str) -> &str {
    ns.rsplit('.').next().unwrap_or(ns)
}

/// The coexist driver's Haskell source. The branded record / factory
/// names derive from each namespace's **final segment** ([`handle_of`],
/// the emitter's `HaskellNames` derivation): a dotted `Com.Acme.Alpha`
/// brands as `AlphaHost` / `createAlpha`, while module qualification
/// keeps the full dotted path (`Com.Acme.Alpha.createAlpha`). Branding
/// from the whole namespace would only coincide for the single-segment
/// case (`create<Ns>` with `handle == ns`) and is not even a valid
/// Haskell identifier for a dotted one.
///
/// The driver interleaves the greeting calls first → second → first,
/// then reads the same-shaped `pair()` export from both packages. Each
/// package exposes its own flat boundary pattern for the structurally
/// identical positional product, so the witness also exercises package
/// isolation for structural presentation names.
struct CoexistDriverSources {
    main: String,
    host_a: String,
    host_b: String,
}

fn coexist_host_module_names(files: &[(PathBuf, Vec<u8>)]) -> (String, String) {
    let mut used: Vec<PathBuf> = files.iter().map(|(path, _)| path.clone()).collect();
    let mut allocate = |stem: &str| {
        for suffix in 0usize.. {
            let name = if suffix == 0 {
                stem.to_owned()
            } else {
                format!("{stem}{suffix}")
            };
            let path = PathBuf::from(format!("{name}.hs"));
            if !used.contains(&path) {
                used.push(path);
                return name;
            }
        }
        unreachable!("an unbounded numeric suffix always yields a fresh Haskell module")
    };
    let host_a = allocate("KioRunnerHostA");
    let host_b = allocate("KioRunnerHostB");
    (host_a, host_b)
}

fn coexist_driver_source(
    ns_a: &str,
    ns_b: &str,
    host_module_a: &str,
    host_module_b: &str,
    host_types_a: &[AssocType],
    host_types_b: &[AssocType],
) -> Result<CoexistDriverSources, String> {
    let ha = handle_of(ns_a);
    let hb = handle_of(ns_b);
    let print = haskell_abi::host_field(&ModuleItemId::new("greeter", "print"));
    let main = haskell_abi::export_wrapper(&ItemId::item("greeter/main", "main"));
    let pair = haskell_abi::export_wrapper(&ItemId::item("greeter/main", "pair"));
    let pair_pattern_a = haskell_abi::product_pattern(
        ns_a,
        &BoundaryId::exp(ItemId::item("greeter/main", "pair"), BoundaryRoot::Ret),
    );
    let pair_pattern_b = haskell_abi::product_pattern(
        ns_b,
        &BoundaryId::exp(ItemId::item("greeter/main", "pair"), BoundaryRoot::Ret),
    );
    let mut driver = format!(
        "{{-# LANGUAGE PackageImports #-}}\n\nmodule Main (main) where\n\nimport qualified \"text\" Data.Text as T\nimport qualified \"text\" Data.Text.IO as TIO\nimport qualified {host_module_a} as HostA\nimport qualified {host_module_b} as HostB\n",
    );
    driver.push_str(&format!(
        "import qualified {ns_a} as PkgA\nimport qualified {ns_b} as PkgB\n\n"
    ));
    driver.push_str("main :: IO ()\nmain = do\n");
    driver.push_str(&format!(
        "  let hostA :: PkgA.{ha}Host HostA.KioRunnerHostTypes IO\n      hostA = PkgA.{ha}Host {{ PkgA.{print} = \\s -> TIO.putStr (T.pack \"first: \" <> s) }}\n"
    ));
    driver.push_str(&format!(
        "  let hostB :: PkgB.{hb}Host HostB.KioRunnerHostTypes IO\n      hostB = PkgB.{hb}Host {{ PkgB.{print} = \\s -> TIO.putStr (T.pack \"second: \" <> s) }}\n"
    ));
    driver.push_str(&format!("  let pa = PkgA.create{ha} hostA\n"));
    driver.push_str(&format!("  let pb = PkgB.create{hb} hostB\n"));
    driver.push_str(&format!(
        "  PkgA.{main} pa\n  PkgB.{main} pb\n  PkgA.{main} pa\n"
    ));
    driver.push_str(&format!(
        "  PkgA.{pair} pa >>= \\(PkgA.{pair_pattern_a} n s) -> putStrLn (\"first pair: \" ++ show n ++ \" \" ++ T.unpack s)\n"
    ));
    driver.push_str(&format!(
        "  PkgB.{pair} pb >>= \\(PkgB.{pair_pattern_b} n s) -> putStrLn (\"second pair: \" ++ show n ++ \" \" ++ T.unpack s)\n"
    ));
    driver.push_str(&format!(
        "  PkgA.{pair} pa >>= \\(PkgA.{pair_pattern_a} n s) -> putStrLn (\"first pair: \" ++ show n ++ \" \" ++ T.unpack s)\n"
    ));
    Ok(CoexistDriverSources {
        main: driver,
        host_a: coexist_host_type_module(host_module_a, ns_a, ha, host_types_a)?,
        host_b: coexist_host_type_module(host_module_b, ns_b, hb, host_types_b)?,
    })
}

fn coexist_host_type_module(
    module: &str,
    namespace: &str,
    handle: &str,
    host_types: &[AssocType],
) -> Result<String, String> {
    let marker = "KioRunnerHostTypes";
    let mut out = format!(
        "{{-# LANGUAGE FlexibleInstances #-}}\n{{-# LANGUAGE PackageImports #-}}\n{{-# LANGUAGE TypeFamilies #-}}\n\nmodule {module} ({marker}) where\n\nimport qualified \"base\" Data.Int\nimport qualified \"text\" Data.Text\nimport qualified \"base\" Data.Word\nimport {namespace} as PkgTypes ({handle}HostTypes(..))\n\ndata {marker} = {marker}\n\ninstance {handle}HostTypes {marker}"
    );
    if host_types.is_empty() {
        out.push_str("\n\n");
        return Ok(out);
    }
    out.push_str(" where\n");
    for assoc in host_types {
        if assoc.role.is_empty() || !assoc.type_params.is_empty() {
            return Err(format!(
                "coexist host type `{}` must be a role-bearing scalar",
                assoc.name
            ));
        }
        out.push_str(&format!(
            "  type {} {marker} = {}\n",
            assoc.name,
            haskell_role_type(&assoc.role)
        ));
    }
    out.push('\n');
    Ok(out)
}

// =========================================================================
// Host API construction (Haskell-typed).
// =========================================================================

/// The protocol's exact [`HostApi`], independently rendered against the
/// public Haskell ABI.
fn haskell_host_api_for_protocol(protocol: RunnerProtocol, target_namespace: &str) -> HostApi {
    let contract = protocol.contract();
    host_api::project_host_api(
        contract,
        |binding| contract_host_type(binding, target_namespace).assoc,
        |binding| haskell_method(binding, contract.host_types, target_namespace),
    )
}

fn haskell_method(
    binding: &HostFnBinding,
    host_types: &[HostTypeBinding],
    target_namespace: &str,
) -> TraitMethod {
    let member = haskell_abi::host_field(&ModuleItemId::new(binding.module, binding.leaf));
    let method = |args: Vec<String>, ret: String| TraitMethod {
        name: member.clone(),
        type_params: Vec::new(),
        arg_types: args,
        ret_type: ret,
        where_clause: String::new(),
    };
    let boundary = |root| BoundaryId::env(binding.module, binding.leaf, root);
    let alias = |root| format!("({} m)", env_alias(target_namespace, &boundary(root)));
    let role = |role| haskell_role_ref_type(role, host_types, target_namespace);
    match binding.body {
        HostFnBodyKind::CallStep { i32, string, bool_ } => method(
            vec![
                format!(
                    "({} -> {} -> {} -> m {})",
                    role(i32),
                    role(string),
                    role(bool_),
                    role(i32),
                ),
                role(i32),
            ],
            role(i32),
        ),
        HostFnBodyKind::MakePairCallback { i32, .. } => {
            let callback_ret = boundary(BoundaryRoot::Arg(0)).nested(BoundaryStep::CallbackRet);
            method(
                vec![
                    format!(
                        "({} -> m ({} m))",
                        role(i32),
                        env_alias(target_namespace, &callback_ret)
                    ),
                    role(i32),
                ],
                role(i32),
            )
        }
        HostFnBodyKind::MakeStep { i32 } => {
            method(vec![role(i32)], format!("({0} -> m {0})", role(i32)))
        }
        HostFnBodyKind::BoxMake { box_type } => {
            let boxed = haskell_assoc_ref(target_namespace, box_type, &["t"]);
            TraitMethod {
                name: member.clone(),
                type_params: vec!["t".to_owned()],
                arg_types: vec!["t".to_owned()],
                ret_type: boxed,
                where_clause: String::new(),
            }
        }
        HostFnBodyKind::BoxGet { box_type } => {
            let boxed = haskell_assoc_ref(target_namespace, box_type, &["t"]);
            TraitMethod {
                name: member.clone(),
                type_params: vec!["t".to_owned()],
                arg_types: vec![boxed],
                ret_type: "t".to_owned(),
                where_clause: String::new(),
            }
        }
        HostFnBodyKind::ApplyPoly { string } => method(
            vec!["(forall (t :: Data.Kind.Type). m (t -> m t))".to_owned()],
            role(string),
        ),
        HostFnBodyKind::MakeToken { value_i32, token } => method(
            vec![role(value_i32)],
            haskell_assoc_ref(target_namespace, token, &[]),
        ),
        HostFnBodyKind::TokenValue { token, value_i32 } => method(
            vec![haskell_assoc_ref(target_namespace, token, &[])],
            role(value_i32),
        ),
        HostFnBodyKind::RoundFunctor
        | HostFnBodyKind::RoundPicker
        | HostFnBodyKind::RoundPolyThunk
        | HostFnBodyKind::RoundPolyUnitSlot => {
            method(vec![alias(BoundaryRoot::Arg(0))], alias(BoundaryRoot::Ret))
        }
        HostFnBodyKind::StagedSecond { string } => {
            method(vec![role(string), role(string)], role(string))
        }
        HostFnBodyKind::MakePairStructural { i32, string } => {
            method(vec![role(i32), role(string)], alias(BoundaryRoot::Ret))
        }
        HostFnBodyKind::ProducePair { .. } => method(Vec::new(), alias(BoundaryRoot::Ret)),
        HostFnBodyKind::NestedCurriedRoundtrip { string } => {
            let string = role(string);
            let callback = format!("({string} -> m ({string} -> m {string}))");
            method(vec![callback.clone()], callback)
        }
        HostFnBodyKind::InvokeSubstitutedUnitCallback { text } => {
            method(vec![format!("(() -> m {})", role(text))], role(text))
        }
        HostFnBodyKind::ReturnedForallUnit => {
            method(Vec::new(), "(forall (t :: Data.Kind.Type). m t)".to_owned())
        }
        HostFnBodyKind::ObservePacked { i32 } => {
            method(vec![alias(BoundaryRoot::Arg(0))], role(i32))
        }
        HostFnBodyKind::TraceUnit { .. } => method(Vec::new(), "()".to_owned()),
        HostFnBodyKind::StagedUnitCall => TraitMethod {
            name: member.clone(),
            type_params: vec!["t0".to_owned(), "t1".to_owned()],
            arg_types: vec!["t0".to_owned()],
            ret_type: "()".to_owned(),
            where_clause: String::new(),
        },
        HostFnBodyKind::SumToString { string, .. } => {
            method(vec![alias(BoundaryRoot::Arg(0))], role(string))
        }
        HostFnBodyKind::UnreachableI32Print { i32 } => method(vec![role(i32)], "()".to_owned()),
        _ => haskell_canonical_method(binding, host_types, target_namespace),
    }
}

/// A package-qualified boundary alias produced from its typed source
/// identity. The exact hexadecimal component is reconstructed independently
/// from the public ABI contract; no emitted identifier is reverse-parsed.
fn env_alias(target_namespace: &str, boundary: &BoundaryId) -> String {
    format!(
        "Pkg.{}",
        haskell_abi::boundary_alias(target_namespace, boundary)
    )
}

/// Build one canonical host fn's Haskell-typed [`TraitMethod`]. The arg /
/// return types are spelled in Haskell so the driver can render the host
/// record field. A structural return names its boundary family application
/// through the emitted package's stable typed boundary alias (qualified by
/// the driver's `Pkg` import alias).
fn haskell_canonical_method(
    binding: &HostFnBinding,
    host_types: &[HostTypeBinding],
    target_namespace: &str,
) -> TraitMethod {
    let member = haskell_abi::host_field(&ModuleItemId::new(binding.module, binding.leaf));
    // The typed boundary alias for this leaf's sub-slot `slot`.
    let boundary = |root| BoundaryId::env(binding.module, binding.leaf, root);
    let alias = |root| env_alias(target_namespace, &boundary(root));
    let m = |args: Vec<String>, ret: String| TraitMethod {
        name: member.clone(),
        type_params: Vec::new(),
        arg_types: args,
        ret_type: ret,
        where_clause: String::new(),
    };
    let role = |role| haskell_role_ref_type(role, host_types, target_namespace);
    match binding.body {
        HostFnBodyKind::Print { string } | HostFnBodyKind::Eprint { string } => {
            m(vec![role(string)], "()".to_owned())
        }
        HostFnBodyKind::PrintI32 { value } => m(vec![role(value)], "()".to_owned()),
        HostFnBodyKind::Exit { status_i32 } => {
            m(vec![role(status_i32)], "Data.Void.Void".to_owned())
        }
        HostFnBodyKind::NumericToString { value, string } => m(vec![role(value)], role(string)),
        HostFnBodyKind::BoolToString { bool_, string } => m(vec![role(bool_)], role(string)),
        HostFnBodyKind::StringConcat { string } => {
            m(vec![role(string), role(string)], role(string))
        }
        HostFnBodyKind::StringEq { string, bool_ } => {
            m(vec![role(string), role(string)], role(bool_))
        }
        HostFnBodyKind::StringLen { string, index } => m(vec![role(string)], role(index)),
        HostFnBodyKind::StringSlice { string, index } => {
            m(vec![role(string), role(index), role(index)], role(string))
        }
        // `Int | .` / `String | .` sum returns, named through the `_ret`
        // alias. The structured protocol body supplies the operation, while
        // the alias preserves its exact return shape.
        HostFnBodyKind::StringCodeAt { string, index } => {
            m(vec![role(string), role(index)], alias(BoundaryRoot::Ret))
        }
        HostFnBodyKind::StringToInt { string, .. } => {
            m(vec![role(string)], alias(BoundaryRoot::Ret))
        }
        HostFnBodyKind::ReadAsciiLine { .. } => m(Vec::new(), alias(BoundaryRoot::Ret)),
        HostFnBodyKind::Arithmetic { number, .. }
        | HostFnBodyKind::FloatArithmetic { number, .. } => {
            let native = role(number);
            m(vec![native.clone(), native.clone()], native)
        }
        HostFnBodyKind::Compare { number, bool_, .. } => {
            let native = role(number);
            m(vec![native.clone(), native], role(bool_))
        }
        HostFnBodyKind::Loop => {
            // `loop[s][r](step: s -> (s | r), state: s) -> r`. The host
            // record retains both native type variables. The step's `s | r`
            // return is the `arg0_cbret` sum, named through its public alias.
            // The structured protocol body supplies the `loop` operation; the
            // body reads the cbret alias off the where-clause.
            let cbret = env_alias(
                target_namespace,
                &boundary(BoundaryRoot::Arg(0)).nested(BoundaryStep::CallbackRet),
            );
            TraitMethod {
                name: member.clone(),
                type_params: vec!["s".to_owned(), "r".to_owned()],
                arg_types: vec![format!("(s -> m ({cbret} h m s r))"), "s".to_owned()],
                ret_type: "r".to_owned(),
                where_clause: format!("__cbret={cbret}"),
            }
        }
        HostFnBodyKind::Array {
            operation,
            array,
            index,
        } => haskell_array_method(
            operation,
            array,
            index,
            host_types,
            target_namespace,
            &member,
            &alias,
        ),
        HostFnBodyKind::MakeScalar { string, scalar } => m(
            vec![role(string), role(string)],
            haskell_assoc_ref(target_namespace, scalar, &[]),
        ),
        HostFnBodyKind::ScalarOf { value, scalar } => m(
            vec![role(value)],
            haskell_assoc_ref(target_namespace, scalar, &[]),
        ),
        HostFnBodyKind::ScalarAs { scalar, .. } => {
            let scalar = haskell_assoc_ref(target_namespace, scalar, &[]);
            m(vec![scalar], alias(BoundaryRoot::Ret))
        }
        HostFnBodyKind::ScalarIsTrue { scalar, bool_ } => {
            let scalar = haskell_assoc_ref(target_namespace, scalar, &[]);
            m(vec![scalar], role(bool_))
        }
        HostFnBodyKind::MakeToken { .. }
        | HostFnBodyKind::TokenValue { .. }
        | HostFnBodyKind::BoxGet { .. }
        | HostFnBodyKind::BoxMake { .. }
        | HostFnBodyKind::CallStep { .. }
        | HostFnBodyKind::MakePairCallback { .. }
        | HostFnBodyKind::MakeStep { .. }
        | HostFnBodyKind::ApplyPoly { .. }
        | HostFnBodyKind::MakePairStructural { .. }
        | HostFnBodyKind::ProducePair { .. }
        | HostFnBodyKind::SumToString { .. }
        | HostFnBodyKind::RoundFunctor
        | HostFnBodyKind::RoundPicker
        | HostFnBodyKind::RoundPolyThunk
        | HostFnBodyKind::RoundPolyUnitSlot
        | HostFnBodyKind::StagedSecond { .. }
        | HostFnBodyKind::NestedCurriedRoundtrip { .. }
        | HostFnBodyKind::InvokeSubstitutedUnitCallback { .. }
        | HostFnBodyKind::ReturnedForallUnit
        | HostFnBodyKind::ObservePacked { .. }
        | HostFnBodyKind::TraceUnit { .. }
        | HostFnBodyKind::StagedUnitCall
        | HostFnBodyKind::UnreachableI32Print { .. } => {
            unreachable!(
                "bespoke Haskell host body `{}` reached canonical signature rendering",
                binding.leaf
            )
        }
    }
}

/// One `array_*` host fn's Haskell-typed method. The polymorphic
/// `Array(T)` host type is represented by the runner's `KioArray`, retaining
/// `T` in every public slot. The array slots are spelled `Self::Array<t>` to
/// preserve the structured protocol body's exact association.
/// `array_pop_back` returns a `T | .` sum, named through its `_ret` alias.
fn haskell_array_method(
    operation: &str,
    array: HostTypeIdentity,
    index: Option<HostRoleRef>,
    host_types: &[HostTypeBinding],
    target_namespace: &str,
    member: &str,
    alias: &dyn Fn(BoundaryRoot) -> String,
) -> TraitMethod {
    let m = |args: Vec<String>, ret: String| TraitMethod {
        name: member.to_owned(),
        type_params: vec!["t".to_owned()],
        arg_types: args,
        ret_type: ret,
        where_clause: String::new(),
    };
    let arr = haskell_assoc_ref(target_namespace, array, &["t"]);
    let index = index.map(|index| haskell_role_ref_type(index, host_types, target_namespace));
    match operation {
        "make-empty" => m(Vec::new(), arr.clone()),
        "make-filled" => m(
            vec![index.expect("make-filled index role"), "t".to_owned()],
            arr.clone(),
        ),
        "len" => m(vec![arr.clone()], index.expect("len index role")),
        "get" => m(
            vec![arr.clone(), index.expect("get index role")],
            "t".to_owned(),
        ),
        "set" => m(
            vec![arr.clone(), index.expect("set index role"), "t".to_owned()],
            "()".to_owned(),
        ),
        "push" => m(vec![arr.clone(), "t".to_owned()], "()".to_owned()),
        "pop-back" => m(vec![arr.clone()], alias(BoundaryRoot::Ret)),
        "swap" => {
            let index = index.expect("swap index role");
            m(vec![arr.clone(), index.clone(), index], "()".to_owned())
        }
        "clear" => m(vec![arr.clone()], "()".to_owned()),
        "clone" => m(vec![arr.clone()], arr),
        other => unreachable!("unknown protocol array operation `{other}`"),
    }
}

/// The canonical fixture type this test runner chooses for a role-bearing
/// associated type. Production hosts choose their own exact type.
fn haskell_role_type(token: &str) -> &'static str {
    match token {
        "i8" => "Data.Int.Int8",
        "i16" => "Data.Int.Int16",
        "i32" => "Data.Int.Int32",
        "i64" => "Data.Int.Int64",
        "i128" | "u128" => "Integer",
        "u8" => "Data.Word.Word8",
        "u16" => "Data.Word.Word16",
        "u32" => "Data.Word.Word32",
        "u64" => "Data.Word.Word64",
        "f32" => "Float",
        "f64" => "Double",
        "bool" => "Bool",
        "str" => "Data.Text.Text",
        other => unreachable!("unknown Haskell runner role fixture `{other}`"),
    }
}

fn haskell_role_ref_type(
    role: HostRoleRef,
    host_types: &[HostTypeBinding],
    target_namespace: &str,
) -> String {
    let binding = role.resolve(host_types);
    match binding.fixture {
        HostTypeFixture::Role(role) => haskell_role_type(role.role()).to_owned(),
        HostTypeFixture::SelectedRole(_) => {
            haskell_selected_role_type(target_namespace, binding.module, binding.leaf)
        }
        other => unreachable!("role reference resolved to non-role fixture {other:?}"),
    }
}

fn haskell_selected_role_type(target_namespace: &str, module: &str, leaf: &str) -> String {
    format!(
        "KioRunnerSelected_{}",
        haskell_abi::host_assoc(target_namespace, module, leaf)
    )
}

fn haskell_role_into_native(
    role: HostRoleRef,
    host_types: &[HostTypeBinding],
    target_namespace: &str,
    expression: &str,
) -> String {
    let binding = role.resolve(host_types);
    match binding.fixture {
        HostTypeFixture::Role(_) => expression.to_owned(),
        HostTypeFixture::SelectedRole(_) => {
            let selected =
                haskell_selected_role_type(target_namespace, binding.module, binding.leaf);
            format!("(case ({expression}) of {{ {selected} __selected -> __selected }})")
        }
        other => unreachable!("role reference resolved to non-role fixture {other:?}"),
    }
}

fn haskell_role_from_native(
    role: HostRoleRef,
    host_types: &[HostTypeBinding],
    target_namespace: &str,
    expression: &str,
) -> String {
    let binding = role.resolve(host_types);
    match binding.fixture {
        HostTypeFixture::Role(_) => expression.to_owned(),
        HostTypeFixture::SelectedRole(_) => format!(
            "({} ({expression}))",
            haskell_selected_role_type(target_namespace, binding.module, binding.leaf)
        ),
        other => unreachable!("role reference resolved to non-role fixture {other:?}"),
    }
}

// =========================================================================
// Driver synthesis.
// =========================================================================

fn render_runner_host_types(
    host_types: &[ContractHostType],
    target_namespace: &str,
) -> Result<(String, String), String> {
    let mut decls = String::new();
    let mut equations = String::new();
    let mut emitted_support = std::collections::BTreeSet::new();
    for host_type in host_types {
        let assoc = &host_type.assoc;
        let rhs = match host_type.fixture {
            HostTypeFixture::Role(role) => haskell_role_type(role.role()).to_owned(),
            HostTypeFixture::SelectedRole(role) => {
                let selected = haskell_selected_role_type(
                    target_namespace,
                    &host_type.module_path,
                    &host_type.source_name,
                );
                if emitted_support.insert(selected.clone()) {
                    decls.push_str(&format!(
                        "newtype {selected} = {selected} {}\n",
                        haskell_role_type(role.role())
                    ));
                    match role {
                        RoleFixture::I8
                        | RoleFixture::I16
                        | RoleFixture::I32
                        | RoleFixture::I64
                        | RoleFixture::I128
                        | RoleFixture::U8
                        | RoleFixture::U16
                        | RoleFixture::U32
                        | RoleFixture::U64
                        | RoleFixture::U128 => {
                            decls.push_str("  deriving (Num)\n\n");
                        }
                        RoleFixture::F32 | RoleFixture::F64 => {
                            decls.push_str("  deriving (Num, Fractional)\n\n");
                        }
                        RoleFixture::String => {
                            decls.push_str(&format!(
                                "\ninstance Data.String.IsString {selected} where\n  fromString = {selected} . Data.String.fromString\n\n"
                            ));
                        }
                        RoleFixture::Bool => {
                            unreachable!(
                                "Haskell exact boolean syntax requires the selected host type to equal Bool"
                            )
                        }
                    }
                }
                selected
            }
            HostTypeFixture::Token => {
                if emitted_support.insert("token".to_owned()) {
                    decls.push_str("newtype KioRunnerToken = KioRunnerToken Data.Int.Int32\n\n");
                }
                "KioRunnerToken".to_owned()
            }
            HostTypeFixture::Box => {
                if emitted_support.insert("box".to_owned()) {
                    decls.push_str("newtype KioRunnerBox a = KioRunnerBox a\n\n");
                }
                "KioRunnerBox".to_owned()
            }
            HostTypeFixture::Array => {
                if emitted_support.insert("array".to_owned()) {
                    decls.push_str(
                        "newtype KioRunnerArray a = KioRunnerArray (Data.IORef.IORef [a])\n\n",
                    );
                }
                "KioRunnerArray".to_owned()
            }
            HostTypeFixture::Scalar => {
                if emitted_support.insert("scalar".to_owned()) {
                    decls.push_str(
                        "data KioRunnerScalar\n  = KioRunnerScalarI32 Data.Int.Int32\n  | KioRunnerScalarF64 Double\n  | KioRunnerScalarString T.Text\n  | KioRunnerScalarBool Bool\n\n",
                    );
                }
                "KioRunnerScalar".to_owned()
            }
        };
        equations.push_str(&format!(
            "  type {} KioRunnerHostTypes = {rhs}\n",
            assoc.name
        ));
    }
    Ok((decls, equations))
}

/// Build the Haskell `Main.hs` driver: a `stubHost` value of the emitted
/// `<Handle>Host` record plus a `main` that instantiates the package and
/// runs the protocol. The driver imports the emitted package qualified as
/// `Pkg` (`import qualified <Ns> as Pkg`) and uses only that public facade.
/// Every package name is therefore spelled `Pkg.<name>`; only the import line
/// and the branded `Pkg.create<Handle>` / `Pkg.<Handle>Host` spellings depend
/// on `ns`, whose final segment is the handle.
fn build_driver(
    host: &HostApi,
    protocol: RunnerProtocol,
    ns: &str,
    host_types: &[ContractHostType],
) -> Result<String, String> {
    let contract = protocol.contract();
    let needs_polymorphic_unit = contract
        .host_fns
        .iter()
        .any(|binding| matches!(binding.body, HostFnBodyKind::ReturnedForallUnit));
    let needs_impredicative_identity = protocol == RunnerProtocol::HostPolyUnitPayloadRoundtrip;
    if contract.execution == ProtocolExecution::CompileOnly {
        return Ok(format!(
            "module Main (main) where\n\nimport qualified {ns} as Pkg\n\nmain :: IO ()\nmain = pure ()\n"
        ));
    }
    let handle = handle_of(ns);
    let classified: Vec<(TraitMethod, HostFnBinding, ModuleItemId, CanonicalKind)> = host
        .methods()
        .zip(contract.host_fns)
        .map(|(method, binding)| {
            (
                method.clone(),
                *binding,
                ModuleItemId::new(binding.module, binding.leaf),
                canonical_kind(binding.body),
            )
        })
        .collect();

    let mut out = String::new();
    out.push_str("-- Generated by kio-test-runner-haskell — do not edit by hand.\n");
    out.push_str("{-# LANGUAGE GeneralizedNewtypeDeriving #-}\n");
    out.push_str("{-# LANGUAGE ImpredicativeTypes #-}\n");
    out.push_str("{-# LANGUAGE PackageImports #-}\n");
    out.push_str("{-# LANGUAGE RankNTypes #-}\n");
    out.push_str("{-# LANGUAGE ScopedTypeVariables #-}\n");
    out.push_str("{-# LANGUAGE TypeApplications #-}\n");
    out.push_str("{-# LANGUAGE TypeFamilies #-}\n");
    out.push_str("{-# LANGUAGE FlexibleInstances #-}\n");
    out.push_str("module Main (main) where\n\n");
    out.push_str(&format!("import qualified {ns} as Pkg\n"));
    if !host.types.is_empty() {
        out.push_str(&format!(
            "import {ns} as PkgTypes ({})\n",
            host.assoc_types()
                .map(|assoc| assoc.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push_str("import qualified \"text\" Data.Text\n");
    out.push_str("import qualified \"text\" Data.Text as T\n");
    out.push_str("import qualified \"text\" Data.Text.IO as TIO\n");
    out.push_str("import qualified \"base\" Data.String\n");
    if needs_impredicative_identity {
        out.push_str("import qualified \"base\" Data.Kind\n");
    }
    out.push_str("import qualified \"base\" Data.Int\n");
    out.push_str("import qualified \"base\" Data.Word\n");
    out.push_str("import qualified \"base\" Data.Char\n");
    out.push_str("import qualified \"base\" Data.IORef\n");
    out.push_str("import qualified \"base\" Data.List\n");
    out.push_str("import qualified \"base\" Text.Read\n");
    if needs_polymorphic_unit {
        out.push_str("import qualified \"base\" Unsafe.Coerce\n");
        out.push_str("import qualified \"base\" Control.Exception\n");
        out.push_str("import qualified \"base\" System.IO.Error\n");
    }
    out.push_str("import \"base\" System.Exit (exitWith, ExitCode(..))\n");
    out.push_str(
        "import \"base\" System.IO (hPutStr, stderr, hSetBuffering, stdout, BufferMode(..), isEOF)\n\n",
    );

    let (host_type_decls, host_type_equations) = render_runner_host_types(host_types, ns)?;
    out.push_str(&host_type_decls);
    out.push_str("data KioRunnerHostTypes = KioRunnerHostTypes\n\n");
    if host_type_equations.is_empty() {
        out.push_str(&format!(
            "instance Pkg.{handle}HostTypes KioRunnerHostTypes\n"
        ));
    } else {
        out.push_str(&format!(
            "instance Pkg.{handle}HostTypes KioRunnerHostTypes where\n"
        ));
        out.push_str(&host_type_equations);
    }
    out.push('\n');

    if needs_impredicative_identity {
        out.push_str("kioPure :: forall a. a -> IO a\nkioPure = pure\n\n");
    }

    if needs_polymorphic_unit {
        out.push_str(
            "-- This fixed witness is observed only at Unit by its owning protocol.\n\
             kioRunnerPolymorphicUnit :: forall a. IO a\n\
             kioRunnerPolymorphicUnit = pure (Unsafe.Coerce.unsafeCoerce ())\n\n",
        );
    }

    // The stub host record: one field per env method, a native-typed body.
    // The record type / constructor / field labels are all spelled through
    // the `Pkg` import alias; the `host__…` field names are the emitted
    // record's, so a drifted FFI surfaces as a GHC error here.
    if protocol == RunnerProtocol::HostExistentialRoundtrip {
        out.push_str(&format!("stubHost :: Pkg.{handle} KioRunnerHostTypes IO -> Data.IORef.IORef (Int, Int) -> Pkg.{handle}Host KioRunnerHostTypes IO\n"));
    } else if needs_polymorphic_unit {
        out.push_str(&format!(
            "stubHost :: Data.IORef.IORef Bool -> Pkg.{handle}Host KioRunnerHostTypes IO\n"
        ));
    } else {
        out.push_str(&format!(
            "stubHost :: Pkg.{handle}Host KioRunnerHostTypes IO\n"
        ));
    }
    let stub_host_lhs = if protocol == RunnerProtocol::HostExistentialRoundtrip {
        "stubHost pkg existentialCounts"
    } else if needs_polymorphic_unit {
        "stubHost returnedForallProduced"
    } else {
        "stubHost"
    };
    if classified.is_empty() {
        out.push_str(&format!("{stub_host_lhs} = Pkg.{handle}Host {{}}\n\n"));
    } else {
        out.push_str(&format!("{stub_host_lhs} = Pkg.{handle}Host\n"));
        for (i, (m, binding, source, kind)) in classified.iter().enumerate() {
            let lead = if i == 0 { "  { " } else { "  , " };
            let field = m.name.as_str();
            let body =
                render_haskell_body_for_binding(binding, kind, source, ns, contract.host_types);
            out.push_str(&format!("{lead}Pkg.{field} = {body}\n"));
        }
        out.push_str("  }\n\n");
    }

    out.push_str("main :: IO ()\n");
    out.push_str("main = do\n");
    out.push_str("  hSetBuffering stdout NoBuffering\n");
    if protocol == RunnerProtocol::HostExistentialRoundtrip {
        out.push_str("  existentialCounts <- Data.IORef.newIORef (0, 0)\n");
        out.push_str(&format!(
            "  let pkg = Pkg.create{handle} (stubHost pkg existentialCounts)\n"
        ));
    } else if needs_polymorphic_unit {
        out.push_str("  returnedForallProduced <- Data.IORef.newIORef False\n");
        out.push_str(&format!(
            "  let pkg = Pkg.create{handle} (stubHost returnedForallProduced)\n"
        ));
    } else {
        out.push_str(&format!("  let pkg = Pkg.create{handle} stubHost\n"));
    }
    out.push_str(&render_main_call(contract, ns));
    Ok(out)
}

/// Render the `main` body line that invokes the package for `protocol`.
fn render_main_call(contract: protocol::ProtocolContract, target_namespace: &str) -> String {
    match contract.execution {
        ProtocolExecution::CompileOnly => {
            unreachable!("compile-only returns before host synthesis")
        }
        ProtocolExecution::ConstructOnly => "  pkg `seq` pure ()\n".to_owned(),
        ProtocolExecution::Invoke(ExportDriver::Main { module }) => {
            format!("  {} pkg\n", main_wrapper_name(module))
        }
        ProtocolExecution::Invoke(ExportDriver::Coexist) => {
            unreachable!("--protocol coexist dispatches through run_coexist")
        }
        ProtocolExecution::Invoke(driver) => {
            render_export_driver(driver, target_namespace, contract.host_types)
                .unwrap_or_else(|| unreachable!("non-main Haskell export driver has no renderer"))
        }
    }
}

/// The export-surface roundtrip drivers: each calls the package's exported
/// items through their typed `export__…` wrappers and
/// prints the results, matching the golden's `expected.stdout`. A compound
/// arg / result is built / destructured through the emitter's stable
/// `ExpP_H…` product pattern synonym or `ExpS_H…` sum arm synonym, keeping
/// the driver independent of the nested pair / `Either` representation.
/// Mirrors the Go runner's `render_export_driver`. Returns
/// `None` for a non-export protocol (the caller falls back to the `main`
/// call).
fn pkg_export(module: &str, item: &str) -> String {
    format!(
        "Pkg.{}",
        haskell_abi::export_wrapper(&ItemId::item(module, item))
    )
}

fn pkg_newtype_export(module: &str, newtype: &str, member: &str) -> String {
    format!(
        "Pkg.{}",
        haskell_abi::export_wrapper(&ItemId::newtype_member(module, newtype, member))
    )
}

fn export_boundary(module: &str, item: &str, root: BoundaryRoot) -> BoundaryId {
    BoundaryId::exp(ItemId::item(module, item), root)
}

fn newtype_export_boundary(
    module: &str,
    newtype: &str,
    member: &str,
    root: BoundaryRoot,
) -> BoundaryId {
    BoundaryId::exp(ItemId::newtype_member(module, newtype, member), root)
}

fn pkg_product_pattern(target_namespace: &str, boundary: &BoundaryId) -> String {
    format!(
        "Pkg.{}",
        haskell_abi::product_pattern(target_namespace, boundary)
    )
}

fn pkg_sum_pattern(
    target_namespace: &str,
    boundary: &BoundaryId,
    key: StructuralKey,
    index: u32,
) -> String {
    format!(
        "Pkg.{}",
        haskell_abi::sum_pattern(target_namespace, boundary, key, index)
    )
}

fn render_export_driver(
    driver: ExportDriver,
    target_namespace: &str,
    host_types: &[HostTypeBinding],
) -> Option<String> {
    // A `Data.Text` show that drops the surrounding quotes a derived `Show`
    // would add — the runner prints raw payloads.
    // Every exported wrapper and per-slot pattern synonym is rendered from
    // its typed public identity through the driver's `Pkg` import alias
    // (`import qualified <Ns> as Pkg`), so these bodies stay
    // namespace-independent.
    let body = match driver {
        ExportDriver::RustCallbackAliases => {
            "  fail \"rust-callback-aliases tests the Rust public naming contract only\"\n"
                .to_owned()
        }
        // `tag()` / `value()` / `echo` exported in `testapi/api`.
        ExportDriver::ModuleRoundtrip => format!(
            "  {} pkg >>= TIO.putStrLn\n  {} pkg >>= (putStrLn . show)\n  {} pkg (T.pack \"module-echo\") >>= TIO.putStrLn\n",
            pkg_export("testapi/api", "tag"),
            pkg_export("testapi/api", "value"),
            pkg_export("testapi/api", "echo"),
        ),
        // `answer()` in `testapi/main`, `echo` in `testapi/utils`.
        ExportDriver::NamespaceRoundtrip => format!(
            "  {} pkg >>= (putStrLn . show)\n  {} pkg (T.pack \"namespace-utils\") >>= TIO.putStrLn\n",
            pkg_export("testapi/main", "answer"),
            pkg_export("testapi/utils", "echo"),
        ),
        // Polymorphic fns exported at the testapi root. Instantiate their
        // native `forall` binders at ordinary Haskell types.
        ExportDriver::PolyRoundtrip => format!(
            "  {echo} pkg (T.pack \"poly-string\") >>= TIO.putStrLn\n  {echo} pkg (42 :: Integer) >>= (putStrLn . show)\n  {keep_left} pkg (T.pack \"left\") (99 :: Integer) >>= TIO.putStrLn\n",
            echo = pkg_export("testapi", "poly_echo"),
            keep_left = pkg_export("testapi", "keep_left"),
        ),
        // `apply_twice(step, seed)` and `make_step(delta)` in `testapi/main`;
        // the callback / returned closure are native `Int32 -> m Int32`.
        ExportDriver::CallbackRoundtrip => format!(
            "  {} pkg (\\n -> pure (n + 3)) 10 >>= (putStrLn . show)\n  step <- {} pkg 4\n  step 5 >>= (putStrLn . show)\n",
            pkg_export("testapi/main", "apply_twice"),
            pkg_export("testapi/main", "make_step"),
        ),
        // `make_pair(I32, String) -> (I32 & String)` exported in the flat
        // package's `main`; read the returned product's
        // positional slots off the `_P` pattern synonym.
        ExportDriver::PositionalProductRoundtrip => format!(
            "  {} pkg 7 (T.pack \"hello\") >>= \\({} a b) -> putStrLn (show a ++ \" \" ++ T.unpack b)\n",
            pkg_export("main", "make_pair"),
            pkg_product_pattern(
                target_namespace,
                &export_boundary("main", "make_pair", BoundaryRoot::Ret),
            ),
        ),
        // `say(p: A & B) -> .` exported in `testapi/main`. Its source product
        // is the declaration-head value domain, so the public facade exposes
        // the two canonical positional slots directly.
        ExportDriver::MultilabelRoundtrip => {
            let echo_pair_ret = pkg_product_pattern(
                target_namespace,
                &export_boundary("testapi/main", "echo_pair", BoundaryRoot::Ret),
            );
            format!(
                "  {say} pkg 42 (T.pack \"shown\\n\")\n\
                 \x20 {echo_pair} pkg 88 (T.pack \"99\") >>= \\({echo_pair_ret} outA outB) -> do\n\
                 \x20   print outA\n\
                 \x20   TIO.putStrLn outB\n\
                 \x20 a <- {make_a} pkg 111\n\
                 \x20 a <- {echo_a} pkg a\n\
                 \x20 {read_a} pkg a >>= (putStrLn . show)\n",
                say = pkg_export("testapi/main", "say"),
                echo_pair = pkg_export("testapi/main", "echo_pair"),
                make_a = pkg_newtype_export("testapi/main", "A", "mk"),
                echo_a = pkg_export("testapi/main", "echo_a"),
                read_a = pkg_newtype_export("testapi/main", "A", "get"),
            )
        }
        ExportDriver::HostExistentialRoundtrip => {
            let exercise = pkg_export("testapi/main", "exercise");
            let result = pkg_product_pattern(
                target_namespace,
                &export_boundary("testapi/main", "exercise", BoundaryRoot::Ret),
            );
            format!(
                "  {result} first second <- {exercise} pkg\n  counts <- Data.IORef.readIORef existentialCounts\n  if first == 37 && second == 83 && counts == (2, 2) then putStrLn \"existential host opening ok\" else fail \"existential host observations changed\"\n"
            )
        }
        ExportDriver::FunctorDictRoundtrip => {
            let unbox_pattern = pkg_product_pattern(
                target_namespace,
                &newtype_export_boundary("testapi/types", "Box", "un_box", BoundaryRoot::Ret),
            );
            format!(
                r#"  integers <- Data.IORef.newIORef ([] :: [Data.Int.Int32])
  texts <- Data.IORef.newIORef ([] :: [T.Text])
  let toText value = Data.IORef.modifyIORef' integers (++ [value]) >> pure (T.pack ("v:" ++ show value))
      toInteger value = Data.IORef.modifyIORef' texts (++ [value]) >> pure (fromIntegral (T.length value) :: Data.Int.Int32)
      check actual expected = if actual == expected then pure () else fail "functor payload changed"
  dict <- {make} pkg >>= {echo} pkg
  input1 <- {mk_box} pkg (42 :: Data.Int.Int32) ()
  first <- {apply} pkg dict toText input1
  {unbox_pattern} firstValue () <- {un_box} pkg first
  check firstValue (T.pack "v:42")
  input2 <- {mk_box} pkg (T.pack "apple") ()
  second <- {apply} pkg dict toInteger input2
  {unbox_pattern} secondValue () <- {un_box} pkg second
  check secondValue 5
  mapping <- {fmap} pkg dict
  firstType <- mapping @Data.Int.Int32
  mapInteger <- firstType @T.Text
  input3 <- {mk_box} pkg (7 :: Data.Int.Int32) ()
  third <- mapInteger toText input3
  {unbox_pattern} thirdValue () <- {un_box} pkg third
  check thirdValue (T.pack "v:7")
  secondType <- mapping @T.Text
  mapText <- secondType @Data.Int.Int32
  input4 <- {mk_box} pkg (T.pack "pear") ()
  fourth <- mapText toInteger input4
  {unbox_pattern} fourthValue () <- {un_box} pkg fourth
  check fourthValue 4
  Data.IORef.readIORef integers >>= flip check [42, 7]
  Data.IORef.readIORef texts >>= flip check [T.pack "apple", T.pack "pear"]
  putStrLn "functor dictionary ok"
"#,
                make = pkg_export("testapi/main", "box_functor"),
                echo = pkg_export("testapi/main", "echo_functor"),
                apply = pkg_export("testapi/main", "apply_functor"),
                mk_box = pkg_newtype_export("testapi/types", "Box", "mk_box"),
                un_box = pkg_newtype_export("testapi/types", "Box", "un_box"),
                fmap = pkg_newtype_export("testapi/types", "Functor", "fmap"),
            )
        }
        ExportDriver::CallableSlotsRoundtrip => {
            let product_ret = |leaf| {
                pkg_product_pattern(
                    target_namespace,
                    &export_boundary("testapi/main", leaf, BoundaryRoot::Ret),
                )
            };
            let sum_pattern = |leaf, root, arm| {
                pkg_sum_pattern(
                    target_namespace,
                    &export_boundary("testapi/main", leaf, root),
                    StructuralKey::positional(arm),
                    arm,
                )
            };
            format!(
                "  let check :: Eq a => a -> a -> IO (); check actual expected = if actual == expected then pure () else error \"callable slot payload changed\"\n\
                 \x20 productCalls <- Data.IORef.newIORef (0 :: Int)\n\
                 \x20 let productStep value = Data.IORef.modifyIORef' productCalls (+ 1) >> pure (value + 5)\n\
                 \x20 {apply_product} pkg productStep 11 >>= (\\value -> check value 16)\n\
                 \x20 {echo_product_ret} echoedStep echoedSeed <- {echo_product} pkg productStep 17\n\
                 \x20 check echoedSeed 17\n\
                 \x20 echoedStep echoedSeed >>= (\\value -> check value 22)\n\
                 \x20 {make_product_ret} madeStep madeSeed <- {make_product} pkg 23\n\
                 \x20 check madeSeed 23\n\
                 \x20 madeStep 29 >>= (\\value -> check value 29)\n\
                 \x20 sumCalls <- Data.IORef.newIORef (0 :: Int)\n\
                 \x20 let sumStep value = Data.IORef.modifyIORef' sumCalls (+ 1) >> pure (value + 7)\n\
                 \x20 let suppliedSum = {input_sum} sumStep\n\
                 \x20 {apply_sum} pkg suppliedSum 31 >>= (\\value -> check value 38)\n\
                 \x20 echoedSum <- {echo_sum} pkg suppliedSum\n\
                 \x20 case echoedSum of\n\
                 \x20   {echo_sum_0} step -> step 37 >>= (\\value -> check value 44)\n\
                 \x20   _ -> error \"callable sum arm changed\"\n\
                 \x20 madeSum <- {make_callable_sum} pkg\n\
                 \x20 case madeSum of\n\
                 \x20   {make_sum_0} step -> step 41 >>= (\\value -> check value 41)\n\
                 \x20   _ -> error \"callable sum arm changed\"\n\
                 \x20 scalar <- {make_scalar_sum} pkg 97\n\
                 \x20 case scalar of\n\
                 \x20   {make_sum_1} value -> check value 97\n\
                 \x20   _ -> error \"scalar sum arm changed\"\n\
                 \x20 {apply_sum} pkg scalar 43 >>= (\\value -> check value 97)\n\
                 \x20 echoedScalar <- {echo_sum} pkg scalar\n\
                 \x20 case echoedScalar of\n\
                 \x20   {echo_sum_1} value -> check value 97\n\
                 \x20   _ -> error \"scalar sum arm changed\"\n\
                 \x20 Data.IORef.readIORef productCalls >>= (\\value -> check value 2)\n\
                 \x20 Data.IORef.readIORef sumCalls >>= (\\value -> check value 2)\n\
                 \x20 putStrLn \"callable slots ok\"\n",
                apply_product = pkg_export("testapi/main", "apply_product"),
                echo_product = pkg_export("testapi/main", "echo_product"),
                make_product = pkg_export("testapi/main", "make_product"),
                apply_sum = pkg_export("testapi/main", "apply_sum"),
                echo_sum = pkg_export("testapi/main", "echo_sum"),
                make_callable_sum = pkg_export("testapi/main", "make_callable_sum"),
                make_scalar_sum = pkg_export("testapi/main", "make_scalar_sum"),
                echo_product_ret = product_ret("echo_product"),
                make_product_ret = product_ret("make_product"),
                input_sum = sum_pattern("apply_sum", BoundaryRoot::Arg(0), 0),
                echo_sum_0 = sum_pattern("echo_sum", BoundaryRoot::Ret, 0),
                echo_sum_1 = sum_pattern("echo_sum", BoundaryRoot::Ret, 1),
                make_sum_0 = sum_pattern("make_callable_sum", BoundaryRoot::Ret, 0),
                make_sum_1 = sum_pattern("make_scalar_sum", BoundaryRoot::Ret, 1),
            )
        }
        ExportDriver::ScalarRoundtrip => {
            let scalar_samples = [
                ("echo_i128", "(-1208925819614629174706299)"),
                ("echo_u128", "2417851639229258349412391"),
                ("echo_f32", "1.5"),
                ("echo_f32", "(-2.25)"),
                ("echo_f64", "1.0000000000000002"),
                ("echo_f64", "(-3.125)"),
            ];
            let mut script = String::new();
            for (index, (leaf, value)) in scalar_samples.into_iter().enumerate() {
                let echo = pkg_export("testapi/main", leaf);
                script.push_str(&format!(
                    "  scalar{index} <- {echo} pkg {value}\n  if scalar{index} == {value} then pure () else error \"scalar payload changed\"\n"
                ));
            }
            script.push_str("  putStrLn \"scalar payloads ok\"\n");
            script
        }
        ExportDriver::HostOwnedRoundtrip => format!(
            "  mapM_ (\\value -> do\n\
             \x20   KioRunnerToken returned <- {echo_token} pkg (KioRunnerToken value)\n\
             \x20   if returned == value then pure () else error \"token payload changed\") [7, 19]\n\
             \x20 KioRunnerBox integer <- {echo_box} pkg (KioRunnerBox (42 :: Data.Int.Int32))\n\
             \x20 if integer == 42 then pure () else error \"integer box payload changed\"\n\
             \x20 KioRunnerBox text <- {echo_box} pkg (KioRunnerBox (T.pack \"box-value\"))\n\
             \x20 if text == T.pack \"box-value\" then pure () else error \"string box payload changed\"\n\
             \x20 putStrLn \"host-owned payloads ok\"\n",
            echo_token = pkg_export("testapi/main", "echo_token"),
            echo_box = pkg_export("testapi/main", "echo_box"),
        ),
        // `pair_swap((I32, String)) -> (String & I32)` and
        // `dispatch_left(I32 | String) -> String`, exported in `testapi/main`.
        // Product declaration-head domains expose their positional slots
        // directly; product returns still read through `_P` synonyms. The sum
        // param is built through the arm synonyms (`arg0_0` = I32, `arg0_1` =
        // String).
        ExportDriver::StructuralRoundtrip => {
            let pair_swap = pkg_export("testapi/main", "pair_swap");
            let pair_ret = pkg_product_pattern(
                target_namespace,
                &export_boundary("testapi/main", "pair_swap", BoundaryRoot::Ret),
            );
            let dispatch = pkg_export("testapi/main", "dispatch_left");
            let dispatch_boundary =
                export_boundary("testapi/main", "dispatch_left", BoundaryRoot::Arg(0));
            let dispatch_0 = pkg_sum_pattern(
                target_namespace,
                &dispatch_boundary,
                StructuralKey::positional(0),
                0,
            );
            let dispatch_1 = pkg_sum_pattern(
                target_namespace,
                &dispatch_boundary,
                StructuralKey::positional(1),
                1,
            );
            let rotate = pkg_export("testapi/main", "rotate");
            let rotate_ret = pkg_product_pattern(
                target_namespace,
                &export_boundary("testapi/main", "rotate", BoundaryRoot::Ret),
            );
            let classify = pkg_export("testapi/main", "classify");
            let classify_boundary =
                export_boundary("testapi/main", "classify", BoundaryRoot::Arg(0));
            let classify_0 = pkg_sum_pattern(
                target_namespace,
                &classify_boundary,
                StructuralKey::positional(0),
                0,
            );
            let classify_4 = pkg_sum_pattern(
                target_namespace,
                &classify_boundary,
                StructuralKey::positional(4),
                4,
            );
            let classify_9 = pkg_sum_pattern(
                target_namespace,
                &classify_boundary,
                StructuralKey::positional(9),
                9,
            );
            let echo_sum = pkg_export("testapi/main", "echo_sum");
            let echo_boundary = export_boundary("testapi/main", "echo_sum", BoundaryRoot::Ret);
            let samples = [
                (0, "(-101)"),
                (1, "(-12345)"),
                (2, "(-123456789)"),
                (3, "(-9007199254740993)"),
                (4, "201"),
                (5, "54321"),
                (6, "3456789012"),
                (7, "18014398509481987"),
                (8, "False"),
                (8, "True"),
                (9, "(T.pack \"sum-value\")"),
            ]
            .into_iter()
            .map(|(arm, value)| {
                let pattern = pkg_sum_pattern(
                    target_namespace,
                    &classify_boundary,
                    StructuralKey::positional(arm),
                    arm,
                );
                format!("{pattern} {value}")
            })
            .collect::<Vec<_>>()
            .join(", ");
            let payload_arms = (0..10)
                .map(|arm| {
                    let pattern = pkg_sum_pattern(
                        target_namespace,
                        &echo_boundary,
                        StructuralKey::positional(arm),
                        arm,
                    );
                    let render = match arm {
                        8 => "if value then \"true\" else \"false\"",
                        9 => "T.unpack value",
                        _ => "show value",
                    };
                    format!("          {pattern} value -> {render}\n")
                })
                .collect::<String>();
            let roundtrip = format!(
                "  mapM_ (\\sample -> do\n\
                 \x20   returned <- {echo_sum} pkg sample\n\
                 \x20   tag <- {classify} pkg returned\n\
                 \x20   let payload = case returned of\n{payload_arms}\
                 \x20   putStrLn (show tag ++ \" \" ++ payload)) [{samples}]\n"
            );
            let script = format!(
                "  {pair_swap} pkg 42 (T.pack \"hello\") >>= \\({pair_ret} s i) -> putStrLn (T.unpack s ++ \" \" ++ show i)\n  {dispatch} pkg ({dispatch_0} 7) >>= TIO.putStrLn\n  {dispatch} pkg ({dispatch_1} (T.pack \"from-sum\")) >>= TIO.putStrLn\n  {rotate} pkg 1 2 3 4 5 6 7 8 9 10 11 12 >>= \\({rotate_ret} a b _ _ _ _ _ _ _ _ _ l) -> putStrLn (show a ++ \" \" ++ show b ++ \" \" ++ show l)\n  {classify} pkg ({classify_0} 1) >>= (putStrLn . show)\n  {classify} pkg ({classify_4} 5) >>= (putStrLn . show)\n  {classify} pkg ({classify_9} (T.pack \"ten\")) >>= (putStrLn . show)\n  {choose_first} pkg >>= {classify} pkg >>= (putStrLn . show)\n  {choose_middle} pkg >>= {classify} pkg >>= (putStrLn . show)\n  {choose_last} pkg >>= {classify} pkg >>= (putStrLn . show)\n",
                choose_first = pkg_export("testapi/main", "choose_first"),
                choose_middle = pkg_export("testapi/main", "choose_middle"),
                choose_last = pkg_export("testapi/main", "choose_last"),
            );
            script + &roundtrip
        }
        // `pack(I32, String) -> Tagged` and `first_or(I32, Tagged) -> I32`
        // exported in `testapi/main`, `Tagged = Pr | .`. Chaining them
        // round-trips a sum-arm-over-a-newtype-over-a-product; `first_or`
        // recovers the first field, printed as `7`.
        ExportDriver::NewtypeSumRoundtrip => format!(
            "  packed <- {} pkg 7 (T.pack \"hi\")\n  {} pkg 0 packed >>= (putStrLn . show)\n",
            pkg_export("testapi/main", "pack"),
            pkg_export("testapi/main", "first_or"),
        ),
        // `bump(Wrap) -> Wrap` exported in `testapi/main`, where `Wrap` is
        // a bare scalar-payload newtype (`newtype Wrap : I32`). The
        // Haskell skin erases the bare scalar newtype at the boundary, so
        // the export takes / returns the raw `Int32` payload directly
        // (`Int32 -> m Int32`). Round-trip `7` through it; it prints as `7`.
        ExportDriver::NewtypeScalarRoundtrip => format!(
            "  {} pkg 7 >>= (putStrLn . show)\n",
            pkg_export("testapi/main", "bump"),
        ),
        ExportDriver::NewtypeIgnoredArgumentRoundtrip => format!(
            "  wrapped <- {} pkg 7\n  {} pkg wrapped >>= (putStrLn . show)\n",
            pkg_export("testapi/main", "from_i32"),
            pkg_export("testapi/main", "to_i32"),
        ),
        ExportDriver::RecursiveNewtypeBoundary => format!(
            "  payload <- {base_payload} pkg\n\
             \x20\x20root <- {make_root} pkg payload\n\
             \x20\x20kept <- {keep} pkg root\n\
             \x20\x20projected <- {read_root} pkg kept\n\
             \x20\x20{accept_payload} pkg projected >>= (putStrLn . show)\n",
            base_payload = pkg_export("main", "base_payload"),
            make_root = pkg_newtype_export("main", "Root", "make_root"),
            keep = pkg_export("main", "keep"),
            read_root = pkg_newtype_export("main", "Root", "read_root"),
            accept_payload = pkg_export("main", "accept_payload"),
        ),
        ExportDriver::NewtypeVisibilityFacade => {
            let selected = host_types
                .iter()
                .find(|binding| matches!(binding.fixture, HostTypeFixture::SelectedRole(_)))
                .map(|binding| {
                    haskell_selected_role_type(target_namespace, binding.module, binding.leaf)
                })
                .expect("newtype-visibility protocol selects one exact host type");
            format!(
            "  a <- {make_a} pkg (KioRunnerSelectedI32 11)\n\
             \x20 KioRunnerSelectedI32 outA <- {read_a} pkg a\n\
             \x20 print outA\n\
             \x20 b <- {make_b} pkg (KioRunnerSelectedI32 22)\n\
             \x20 KioRunnerSelectedI32 outB <- {read_b} pkg b\n\
             \x20 print outB\n\
             \x20 c <- {make_constructor_only} pkg (KioRunnerSelectedI32 33)\n\
             \x20 KioRunnerSelectedI32 outC <- {read_constructor_only} pkg c\n\
             \x20 print outC\n\
             \x20 p <- {make_projector_only} pkg (KioRunnerSelectedI32 44)\n\
             \x20 KioRunnerSelectedI32 outP <- {read_projector_only} pkg p\n\
             \x20 print outP\n\
             \x20 both <- {make_both_public} pkg (KioRunnerSelectedI32 55)\n\
             \x20 KioRunnerSelectedI32 outBoth <- {read_both_public} pkg both\n\
             \x20 print outBoth\n\
             \x20 left <- {make_left} pkg (KioRunnerSelectedI32 66)\n\
             \x20 leftPayload <- {read_left} pkg left\n\
             \x20 leftReboxed <- {make_shared_left} pkg leftPayload\n\
             \x20 KioRunnerSelectedI32 outLeft <- {read_shared_left} pkg leftReboxed\n\
             \x20 print outLeft\n\
             \x20 right <- {make_right} pkg (KioRunnerSelectedI32 77)\n\
             \x20 rightPayload <- {read_right} pkg right\n\
             \x20 rightReboxed <- {make_shared_right} pkg rightPayload\n\
             \x20 KioRunnerSelectedI32 outRight <- {read_shared_right} pkg rightReboxed\n\
             \x20 print outRight\n\
             \x20 constructorPair <- {make_constructor_pair} pkg (KioRunnerSelectedI32 81) (KioRunnerSelectedI32 82)\n\
             \x20 {constructor_pair_ret} (KioRunnerSelectedI32 constructorPairLeft) (KioRunnerSelectedI32 constructorPairRight) <- {read_constructor_pair} pkg constructorPair\n\
             \x20 putStrLn (show constructorPairLeft ++ \" \" ++ show constructorPairRight)\n\
             \x20 projectorPair <- {make_projector_pair} pkg (KioRunnerSelectedI32 constructorPairLeft) (KioRunnerSelectedI32 constructorPairRight)\n\
             \x20 {projector_pair_ret} (KioRunnerSelectedI32 projectorPairLeft) (KioRunnerSelectedI32 projectorPairRight) <- {read_projector_pair} pkg projectorPair\n\
             \x20 putStrLn (show projectorPairLeft ++ \" \" ++ show projectorPairRight)\n\
             \x20 constructorGeneric <- {make_constructor_generic} pkg (KioRunnerSelectedI32 85)\n\
             \x20 KioRunnerSelectedI32 constructorGenericOut <- {read_constructor_generic} pkg constructorGeneric\n\
             \x20 print constructorGenericOut\n\
             \x20 projectorGeneric <- {make_projector_generic} pkg (KioRunnerSelectedI32 86)\n\
             \x20 KioRunnerSelectedI32 projectorGenericOut <- {read_projector_generic} pkg projectorGeneric\n\
             \x20 print projectorGenericOut\n\
             \x20 packed <- {make_packed_function} pkg (\\(KioRunnerSelectedI32 l) (KioRunnerSelectedI32 r) -> pure (KioRunnerSelectedI32 (l + r)))\n\
             \x20 unpacked <- {read_packed_function} pkg packed\n\
             \x20 unpacked (KioRunnerSelectedI32 constructorPairLeft) (KioRunnerSelectedI32 constructorPairRight) >>= \\(KioRunnerSelectedI32 packedOut) -> print packedOut\n\
             \x20 existential <- {make_existential_unit} pkg\n\
             \x20 {read_existential_unit} pkg existential (pure (\\_ -> pure (89 :: Int))) >>= print\n\
             \x20 existentialEmpty <- {make_existential_empty} pkg\n\
             \x20 {read_existential_empty} pkg existentialEmpty (pure (pure (90 :: Int))) >>= print\n\
             \x20 recursiveBothPayload <- {recursive_both_base_payload} pkg\n\
             \x20 recursiveBoth <- {make_recursive_both} pkg recursiveBothPayload\n\
             \x20 recursiveBothOut <- {read_recursive_both} pkg recursiveBoth\n\
             \x20 {recursive_both_payload_is_base} pkg recursiveBothOut >>= \\(KioRunnerSelectedI32 n) -> print n\n\
             \x20 recursiveConstructorPayload <- {recursive_constructor_base_payload} pkg\n\
             \x20 recursiveConstructor <- {make_recursive_constructor} pkg recursiveConstructorPayload\n\
             \x20 recursiveConstructorOut <- {read_recursive_constructor} pkg recursiveConstructor\n\
             \x20 {recursive_constructor_payload_is_base} pkg recursiveConstructorOut >>= \\(KioRunnerSelectedI32 n) -> print n\n\
             \x20 recursiveProjector <- {make_recursive_projector_base} pkg\n\
             \x20 recursiveProjectorOut <- {read_recursive_projector} pkg recursiveProjector\n\
             \x20 {recursive_projector_payload_is_base} pkg recursiveProjectorOut >>= \\(KioRunnerSelectedI32 n) -> print n\n\
             \x20 inputs <- Data.IORef.newIORef ([] :: [Data.Int.Int32])\n\
             \x20 constructed <- {make_constructor_spread} pkg (pure (\\(KioRunnerSelectedI32 seed) _ -> Data.IORef.modifyIORef' inputs (++ [seed]) >> pure (KioRunnerSelectedI32 (seed + 3))))\n\
             \x20 KioRunnerSelectedI32 constructedI32 <- {invoke_constructor_i32} pkg constructed (KioRunnerSelectedI32 101)\n\
             \x20 KioRunnerSelectedI32 constructedUnit <- {invoke_constructor_unit} pkg constructed (KioRunnerSelectedI32 102)\n\
             \x20 constructorInputs <- Data.IORef.readIORef inputs\n\
             \x20 if constructedI32 == 104 && constructedUnit == 105 && constructorInputs == [101, 102] then pure () else fail \"constructor callback changed\"\n\
             \x20 projectorValue <- {make_projector_spread} pkg\n\
             \x20 projected <- {read_projector_spread} pkg projectorValue\n\
             \x20 projectedI32 <- projected @KioRunnerSelectedI32\n\
             \x20 projectedUnit <- projected @()\n\
             \x20 KioRunnerSelectedI32 outputI32 <- projectedI32 (KioRunnerSelectedI32 111) (KioRunnerSelectedI32 1)\n\
             \x20 KioRunnerSelectedI32 outputUnit <- projectedUnit (KioRunnerSelectedI32 112) ()\n\
             \x20 if outputI32 == 111 && outputUnit == 112 then pure () else fail \"projector callback changed\"\n\
             \x20 spreadOpens <- Data.IORef.newIORef (0 :: Int)\n\
             \x20 recursiveOpens <- Data.IORef.newIORef (0 :: Int)\n\
             \x20 spreadValue <- {make_existential_spread} pkg\n\
             \x20 spreadResult <- {read_existential_spread} pkg spreadValue (pure (\\payload -> do {{ _ <- payload @(); Data.IORef.modifyIORef' spreadOpens (+ 1); pure (91 :: Int) }}))\n\
             \x20 recursiveValue <- {make_recursive_existential} pkg\n\
             \x20 recursiveResult <- {read_recursive_existential} pkg recursiveValue (pure (\\_ -> Data.IORef.modifyIORef' recursiveOpens (+ 1) >> pure (92 :: Int)))\n\
             \x20 spreadOpenCount <- Data.IORef.readIORef spreadOpens\n\
             \x20 recursiveOpenCount <- Data.IORef.readIORef recursiveOpens\n\
             \x20 if spreadResult == 91 && recursiveResult == 92 && spreadOpenCount == 1 && recursiveOpenCount == 1 then pure () else fail \"existential continuation count changed\"\n",
            make_constructor_spread = pkg_newtype_export("testapi/types", "Constructor_spread", "make_constructor_spread"),
            invoke_constructor_i32 = pkg_export("testapi/types", "invoke_constructor_spread_i32"),
            invoke_constructor_unit = pkg_export("testapi/types", "invoke_constructor_spread_unit"),
            make_projector_spread = pkg_export("testapi/types", "make_projector_spread_value"),
            read_projector_spread = pkg_newtype_export("testapi/types", "Projector_spread", "read_projector_spread"),
            make_existential_spread = pkg_export("testapi/types", "make_existential_spread_value"),
            read_existential_spread = pkg_newtype_export("testapi/types", "Existential_spread", "read_existential_spread"),
            make_recursive_existential = pkg_export("testapi/types", "make_recursive_existential_function_value"),
            read_recursive_existential = pkg_newtype_export("testapi/types", "Recursive_existential_function", "read_recursive_existential_function"),
            make_a = pkg_export("testapi/types", "make_a"),
            read_a = pkg_export("testapi/types", "read_a"),
            make_b = pkg_export("testapi/types", "make_b"),
            read_b = pkg_export("testapi/types", "read_b"),
            make_constructor_only =
                pkg_newtype_export("testapi/types", "Constructor_only", "make_constructor_only",),
            read_constructor_only = pkg_export("testapi/types", "read_constructor_only_value",),
            make_projector_only = pkg_export("testapi/types", "make_projector_only_value",),
            read_projector_only =
                pkg_newtype_export("testapi/types", "Projector_only", "read_projector_only",),
            make_both_public =
                pkg_newtype_export("testapi/types", "Both_public", "make_both_public",),
            read_both_public =
                pkg_newtype_export("testapi/types", "Both_public", "read_both_public",),
            make_left = pkg_export("testapi/left", "make"),
            read_left = pkg_export("testapi/left", "read"),
            make_shared_left = pkg_newtype_export("testapi/left", "Shared", "make_shared"),
            read_shared_left = pkg_newtype_export("testapi/left", "Shared", "read_shared"),
            make_right = pkg_export("testapi/right", "make"),
            read_right = pkg_export("testapi/right", "read"),
            make_shared_right = pkg_newtype_export("testapi/right", "Shared", "make_shared"),
            read_shared_right = pkg_newtype_export("testapi/right", "Shared", "read_shared"),
            make_constructor_pair =
                pkg_newtype_export("testapi/types", "Constructor_pair", "make_constructor_pair",),
            constructor_pair_ret = pkg_product_pattern(
                target_namespace,
                &export_boundary(
                    "testapi/types",
                    "read_constructor_pair_value",
                    BoundaryRoot::Ret,
                ),
            ),
            read_constructor_pair = pkg_export("testapi/types", "read_constructor_pair_value"),
            make_projector_pair = pkg_export("testapi/types", "make_projector_pair_value"),
            projector_pair_ret = pkg_product_pattern(
                target_namespace,
                &newtype_export_boundary(
                    "testapi/types",
                    "Projector_pair",
                    "read_projector_pair",
                    BoundaryRoot::Ret,
                ),
            ),
            read_projector_pair =
                pkg_newtype_export("testapi/types", "Projector_pair", "read_projector_pair",),
            make_constructor_generic = pkg_newtype_export(
                "testapi/types",
                "Constructor_generic",
                "make_constructor_generic",
            ),
            read_constructor_generic =
                pkg_export("testapi/types", "read_constructor_generic_value"),
            make_projector_generic = pkg_export("testapi/types", "make_projector_generic_value"),
            read_projector_generic = pkg_newtype_export(
                "testapi/types",
                "Projector_generic",
                "read_projector_generic",
            ),
            make_packed_function =
                pkg_newtype_export("testapi/types", "Packed_function", "make_packed_function",),
            read_packed_function =
                pkg_newtype_export("testapi/types", "Packed_function", "read_packed_function",),
            make_existential_unit = pkg_export("testapi/types", "make_existential_unit_value"),
            read_existential_unit =
                pkg_newtype_export("testapi/types", "Existential_unit", "read_existential_unit",),
            make_existential_empty = pkg_export("testapi/types", "make_existential_empty_value"),
            read_existential_empty = pkg_newtype_export(
                "testapi/types",
                "Existential_empty",
                "read_existential_empty",
            ),
            recursive_both_base_payload =
                pkg_export("testapi/types", "recursive_both_base_payload"),
            make_recursive_both =
                pkg_newtype_export("testapi/types", "Recursive_both", "make_recursive_both",),
            read_recursive_both =
                pkg_newtype_export("testapi/types", "Recursive_both", "read_recursive_both",),
            recursive_both_payload_is_base =
                pkg_export("testapi/types", "recursive_both_payload_is_base"),
            recursive_constructor_base_payload =
                pkg_export("testapi/types", "recursive_constructor_base_payload"),
            make_recursive_constructor = pkg_newtype_export(
                "testapi/types",
                "Recursive_constructor",
                "make_recursive_constructor",
            ),
            read_recursive_constructor =
                pkg_export("testapi/types", "read_recursive_constructor_value"),
            recursive_constructor_payload_is_base =
                pkg_export("testapi/types", "recursive_constructor_payload_is_base",),
            make_recursive_projector_base =
                pkg_export("testapi/types", "make_recursive_projector_base"),
            read_recursive_projector = pkg_newtype_export(
                "testapi/types",
                "Recursive_projector",
                "read_recursive_projector",
            ),
            recursive_projector_payload_is_base =
                pkg_export("testapi/types", "recursive_projector_payload_is_base"),
        )
            .replace("KioRunnerSelectedI32", &selected)
        }
        // `make(String, I32, I32) -> Outer` exported in `testapi/main`,
        // where the label-minted `Inner` wraps `(I32 & I32)` and `Outer`
        // wraps `(String & Inner)`. Their transparent payloads are exposed
        // through the stable typed `Outer.get` / `Inner.get` return-boundary
        // patterns (the same shapes the
        // newtype projectors read). Print all three leaf slots; the
        // nested-named-product return is the nested-binder regression
        // shape.
        ExportDriver::CompoundInputOnce => {
            let product = |item| {
                pkg_product_pattern(
                    target_namespace,
                    &export_boundary("testapi/main", item, BoundaryRoot::Ret),
                )
            };
            let direct = product("direct");
            let callback = product("callback");
            let callback_input = pkg_product_pattern(
                target_namespace,
                &export_boundary("testapi/main", "callback", BoundaryRoot::Arg(0))
                    .nested(BoundaryStep::CallbackRet),
            );
            let inner = pkg_product_pattern(
                target_namespace,
                &newtype_export_boundary("testapi/main", "Inner", "un_inner", BoundaryRoot::Ret),
            );
            let outer = pkg_product_pattern(
                target_namespace,
                &newtype_export_boundary("testapi/main", "Outer", "un_outer", BoundaryRoot::Ret),
            );
            let choice =
                newtype_export_boundary("testapi/main", "Choice", "un_choice", BoundaryRoot::Ret);
            let first = pkg_sum_pattern(target_namespace, &choice, StructuralKey::positional(0), 0);
            let middle =
                pkg_sum_pattern(target_namespace, &choice, StructuralKey::bare("Inner"), 1);
            let last = pkg_sum_pattern(target_namespace, &choice, StructuralKey::bare("Outer"), 2);
            format!(
                r#"  {call_direct} pkg >>= \({direct} number text) -> print number >> TIO.putStrLn text
  {call_callback} pkg (putStrLn "callback" >> pure ({callback_input} 9 (T.pack "callback-value"))) >>= \({callback} number text) -> print number >> TIO.putStrLn text
  {make_outer} pkg (T.pack "nest") 11 13 >>= {echo_outer} pkg >>= \({outer} text ({inner} left right)) -> TIO.putStrLn text >> print left >> print right
  let printChoice value = {echo_choice} pkg value >>= \result -> case result of
        {first} number -> print number
        {middle} ({inner} left right) -> print left >> print right
        {last} ({outer} text ({inner} left right)) -> TIO.putStrLn text >> print left >> print right
  {make_first} pkg 17 >>= printChoice
  {make_middle} pkg 19 23 >>= printChoice
  {make_last} pkg (T.pack "choice") 29 31 >>= printChoice
  {echo_text} pkg (T.pack "atomic") >>= TIO.putStrLn
"#,
                call_direct = pkg_export("testapi/main", "direct"),
                call_callback = pkg_export("testapi/main", "callback"),
                make_outer = pkg_export("testapi/main", "make_outer"),
                echo_outer = pkg_export("testapi/main", "echo_outer"),
                echo_choice = pkg_export("testapi/main", "echo_choice"),
                make_first = pkg_export("testapi/main", "first"),
                make_middle = pkg_export("testapi/main", "middle"),
                make_last = pkg_export("testapi/main", "last"),
                echo_text = pkg_export("testapi/main", "echo_text"),
            )
        }
        ExportDriver::NestedProductRoundtrip => format!(
            "  {} pkg (T.pack \"nest\") 7 9 >>= \\({} s ({} a b)) -> do\n    TIO.putStrLn s\n    putStrLn (show a)\n    putStrLn (show b)\n",
            pkg_export("testapi/main", "make"),
            pkg_product_pattern(
                target_namespace,
                &newtype_export_boundary("testapi/main", "Outer", "get", BoundaryRoot::Ret,)
            ),
            pkg_product_pattern(
                target_namespace,
                &newtype_export_boundary("testapi/main", "Inner", "get", BoundaryRoot::Ret,)
            ),
        ),
        ExportDriver::CurriedFacade => format!(
            "  {} pkg (T.pack \"ku\") (T.pack \"rz\") >>= TIO.putStrLn\n  {} pkg 1 2 3 >>= (putStrLn . show)\n",
            pkg_export("testapi/main", "pick"),
            pkg_export("testapi/main", "last"),
        ),
        ExportDriver::NestedCurriedRoundtrip => {
            let via_host = pkg_export("testapi/api", "via_host");
            let round_export = pkg_export("testapi/api", "round_export");
            format!(
                "  let join left = pure (\\right -> pure (left <> T.pack \"/\" <> right))\n\
                 \x20\x20viaHost <- {via_host} pkg join\n\
                 \x20\x20viaHostSecond <- viaHost (T.pack \"env-left\")\n\
                 \x20\x20viaHostSecond (T.pack \"env-right\") >>= TIO.putStrLn . (T.pack \"via host: \" <>)\n\
                 \x20\x20roundExport <- {round_export} pkg join\n\
                 \x20\x20roundExportSecond <- roundExport (T.pack \"export-left\")\n\
                 \x20\x20roundExportSecond (T.pack \"export-right\") >>= TIO.putStrLn . (T.pack \"round export: \" <>)\n"
            )
        }
        ExportDriver::HostSubstitutedUnitCallback => format!(
            "  {} pkg (\\() -> pure (T.pack \"callback\")) >>= TIO.putStrLn\n",
            pkg_export("testapi/api", "via_host"),
        ),
        ExportDriver::ReturnedForallCallByValue => format!(
            "  {main} pkg\n\
             \x20\x20result <- Control.Exception.try ({main} pkg) :: IO (Either IOError ())\n\
             \x20\x20case result of\n\
             \x20\x20  Left err\n\
             \x20\x20    | System.IO.Error.ioeGetErrorString err == \"produce failed\" -> putStrLn \"caught\"\n\
             \x20\x20    | otherwise -> Control.Exception.throwIO err\n\
             \x20\x20  Right () -> error \"produce did not throw\"\n",
            main = pkg_export("testapi/main", "main"),
        ),
        ExportDriver::FacadeSelectorCollisions => format!(
            "  {pkg_fn} pkg\n\
             \x20\x20{value_fn} pkg\n\
             \x20\x20{host_value} pkg\n\
             \x20\x20{mod_api_value} pkg\n\
             \x20\x20{foo_bar_value} pkg 9 1 >>= (putStrLn . show)\n\
             \x20\x20{foo_ubar_value} pkg 10 2 >>= (putStrLn . show)\n\
             \x20\x20{i_value} pkg 41 1 >>= (putStrLn . show)\n\
             \x20\x20{child_fn} pkg\n\
             \x20\x20putStrLn \"api.child function\"\n\
             \x20\x20{child_module} pkg\n\
             \x20\x20putStrLn \"api/child module\"\n\
             \x20\x20child <- {make_child} pkg 30\n\
             \x20\x20_ <- {read_child} pkg child\n\
             \x20\x20putStrLn \"api.Child type\"\n",
            pkg_fn = pkg_export("testapi/api", "pkg"),
            value_fn = pkg_export("testapi/api", "value"),
            host_value = pkg_export("testapi/host", "value"),
            mod_api_value = pkg_export("testapi/mod_api_value", "value"),
            foo_bar_value = pkg_export("testapi/foo/bar", "value"),
            foo_ubar_value = pkg_export("testapi/foo_bar", "value"),
            i_value = pkg_export("testapi/i", "value"),
            child_fn = pkg_export("testapi/api", "child"),
            child_module = pkg_export("testapi/api/child", "value"),
            make_child = pkg_newtype_export("testapi/api", "Child", "make_child"),
            read_child = pkg_newtype_export("testapi/api", "Child", "read_child"),
        ),
        ExportDriver::PublicWordNames => format!(
            "  {plain} pkg >>= print\n\
             \x20\x20{leading} pkg >>= print\n\
             \x20\x20{trailing} pkg >>= print\n\
             \x20\x20{both} pkg >>= print\n\
             \x20\x20{double} pkg >>= print\n\
             \x20\x20boxed <- {wrap} pkg (55 :: Data.Int.Int32)\n\
             \x20\x20{unwrap} pkg boxed >>= print\n\
             \x20\x20{keep} pkg (66 :: Data.Int.Int32) >>= print\n\
             \x20\x20firstBox <- {wrap} pkg (77 :: Data.Int.Int32)\n\
             \x20\x20secondBox <- {other_wrap} pkg (88 :: Data.Int.Int32)\n\
             \x20\x20{keep_pair} pkg firstBox secondBox >>= \\({pair_ret} firstWord secondWord) -> do\n\
             \x20\x20\x20\x20{unwrap} pkg firstWord >>= print\n\
             \x20\x20\x20\x20{other_unwrap} pkg secondWord >>= print\n",
            plain = pkg_export("word_api", "read_word"),
            leading = pkg_export("word_api", "_read_word"),
            trailing = pkg_export("word_api", "read_word_"),
            both = pkg_export("word_api", "_read_word_"),
            double = pkg_export("word_api", "read_word__"),
            wrap = pkg_newtype_export("word_api/word_nodes", "_Word_box__", "wrap_word"),
            unwrap = pkg_newtype_export("word_api/word_nodes", "_Word_box__", "unwrap_word"),
            other_wrap = pkg_newtype_export("word_api/other_nodes", "_Word_box__", "wrap_word"),
            other_unwrap = pkg_newtype_export("word_api/other_nodes", "_Word_box__", "unwrap_word"),
            keep = pkg_export("word_api/word_nodes", "keep_word"),
            keep_pair = pkg_export("word_api", "keep_pair"),
            pair_ret = pkg_product_pattern(
                target_namespace,
                &export_boundary("word_api", "keep_pair", BoundaryRoot::Ret),
            ),
        ),
        ExportDriver::ModuleAliasScopeCollision => {
            let a_ret = pkg_product_pattern(
                target_namespace,
                &export_boundary("testapi/a", "make", BoundaryRoot::Ret),
            );
            let b_ret = pkg_product_pattern(
                target_namespace,
                &export_boundary("testapi/b", "make", BoundaryRoot::Ret),
            );
            format!(
                "  _ <- {make_a} pkg >>= \\({a_ret} a0 a1) -> {consume_a} pkg a0 a1\n\
                 \x20\x20_ <- {make_b} pkg >>= \\({b_ret} b0 b1 b2) -> {consume_b} pkg b0 b1 b2\n\
                 \x20\x20pure ()\n",
                make_a = pkg_export("testapi/a", "make"),
                consume_a = pkg_export("testapi/a", "consume"),
                make_b = pkg_export("testapi/b", "make"),
                consume_b = pkg_export("testapi/b", "consume"),
            )
        }
        ExportDriver::WideCallable => {
            let arguments = (0..WIDE_CALLABLE_SLOT_COUNT)
                .map(|index| index.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            let ret = pkg_product_pattern(
                target_namespace,
                &export_boundary("testapi/main", "select", BoundaryRoot::Ret),
            );
            let callback_ret = pkg_product_pattern(
                target_namespace,
                &export_boundary("testapi/main", "make_select", BoundaryRoot::Ret)
                    .nested(BoundaryStep::CallbackRet),
            );
            format!(
                "  {select} pkg {arguments} >>= \\({ret} first middle last) -> do\n    print first\n    print middle\n    print last\n    callback <- {make_select} pkg\n    callback {arguments} >>= \\({callback_ret} callbackFirst callbackMiddle callbackLast) -> do\n      print callbackFirst\n      print callbackMiddle\n      print callbackLast\n",
                select = pkg_export("testapi/main", "select"),
                make_select = pkg_export("testapi/main", "make_select"),
            )
        }
        // `apply_via[K][R](f: K -> R, x: K) -> R` stays natively
        // polymorphic on the Haskell facade
        // (`(t_k -> m t_r) -> t_k -> m t_r`), so the driver instantiates it
        // at two ordinary Haskell types.
        ExportDriver::PolyCallbackRoundtrip => {
            let apply_via = pkg_export("testapi/main", "apply_via");
            format!(
                "  {apply_via} pkg (\\s -> pure (T.pack \"via: \" <> s)) (T.pack \"apply\") >>= TIO.putStrLn\n  {apply_via} pkg (\\n -> pure (n + 8)) (7 :: Int) >>= (putStrLn . show)\n"
            )
        }
        // `mk_pair` constructs the exported `Pair` newtype's nominal carrier
        // from its two canonical declaration-head slots; `un_pair` projects
        // the product return through its boundary pattern.
        ExportDriver::TypeRoundtrip => format!(
            "  boxed <- {} pkg (T.pack \"export-type-left\") (T.pack \"export-type-right\")\n  {} pkg boxed >>= \\({} l r) -> do\n    TIO.putStrLn l\n    TIO.putStrLn r\n",
            pkg_newtype_export("testapi/types", "Pair", "mk_pair"),
            pkg_newtype_export("testapi/types", "Pair", "un_pair"),
            pkg_product_pattern(
                target_namespace,
                &newtype_export_boundary("testapi/types", "Pair", "un_pair", BoundaryRoot::Ret,)
            ),
        ),
        ExportDriver::Main { .. } | ExportDriver::Coexist => return None,
    };
    Some(body)
}

/// The exported `main` boundary-wrapper name for its exact declaring module.
fn main_wrapper_name(module: &str) -> String {
    pkg_export(module, "main")
}

fn canonical_kind(body: HostFnBodyKind) -> CanonicalKind {
    match body {
        HostFnBodyKind::Print { .. } => CanonicalKind::Print,
        HostFnBodyKind::Eprint { .. } => CanonicalKind::Eprint,
        HostFnBodyKind::Exit { .. } => CanonicalKind::Exit,
        HostFnBodyKind::ReadAsciiLine { .. } => CanonicalKind::ReadAsciiLine,
        HostFnBodyKind::StringConcat { .. } => CanonicalKind::StringConcat,
        HostFnBodyKind::StringEq { .. } => CanonicalKind::StringEq,
        HostFnBodyKind::StringLen { .. } => CanonicalKind::StringLen,
        HostFnBodyKind::StringSlice { .. } => CanonicalKind::StringSlice,
        HostFnBodyKind::StringCodeAt { .. } => CanonicalKind::StringCodeAt,
        HostFnBodyKind::Loop => CanonicalKind::Loop,
        HostFnBodyKind::NumericToString { value, .. } => CanonicalKind::NumericToString {
            kind: value.fixture.role().to_owned(),
        },
        HostFnBodyKind::BoolToString { .. } => CanonicalKind::BoolToString,
        HostFnBodyKind::PrintI32 { .. } => CanonicalKind::PrintI32,
        HostFnBodyKind::StringToInt { .. } => CanonicalKind::StringToInt,
        HostFnBodyKind::Arithmetic { operation, number } => CanonicalKind::Arith {
            op: operation.to_owned(),
            kind: number.fixture.role().to_owned(),
        },
        HostFnBodyKind::FloatArithmetic { operation, number } => CanonicalKind::FloatArith {
            op: operation.to_owned(),
            kind: number.fixture.role().to_owned(),
        },
        HostFnBodyKind::Compare {
            operation, number, ..
        } => CanonicalKind::Cmp {
            cmp: operation.to_owned(),
            kind: number.fixture.role().to_owned(),
        },
        HostFnBodyKind::Array { operation, .. } => CanonicalKind::Array(match operation {
            "make-empty" => ArrayOp::MakeEmpty,
            "make-filled" => ArrayOp::MakeFilled,
            "len" => ArrayOp::Len,
            "get" => ArrayOp::Get,
            "set" => ArrayOp::Set,
            "push" => ArrayOp::Push,
            "pop-back" => ArrayOp::PopBack,
            "swap" => ArrayOp::Swap,
            "clear" => ArrayOp::Clear,
            "clone" => ArrayOp::Clone,
            other => unreachable!("unknown protocol array operation `{other}`"),
        }),
        HostFnBodyKind::MakeScalar { .. } => CanonicalKind::MakeScalar,
        HostFnBodyKind::ScalarOf { value, .. } => CanonicalKind::ScalarOf {
            kind: value.fixture.role().to_owned(),
        },
        HostFnBodyKind::ScalarAs { value, .. } => CanonicalKind::ScalarAs {
            kind: value.fixture.role().to_owned(),
        },
        HostFnBodyKind::ScalarIsTrue { .. } => CanonicalKind::ScalarIsTrue,
        HostFnBodyKind::MakeToken { .. }
        | HostFnBodyKind::TokenValue { .. }
        | HostFnBodyKind::BoxGet { .. }
        | HostFnBodyKind::BoxMake { .. }
        | HostFnBodyKind::CallStep { .. }
        | HostFnBodyKind::MakePairCallback { .. }
        | HostFnBodyKind::MakeStep { .. }
        | HostFnBodyKind::ApplyPoly { .. }
        | HostFnBodyKind::MakePairStructural { .. }
        | HostFnBodyKind::ProducePair { .. }
        | HostFnBodyKind::SumToString { .. }
        | HostFnBodyKind::RoundFunctor
        | HostFnBodyKind::RoundPicker
        | HostFnBodyKind::RoundPolyThunk
        | HostFnBodyKind::RoundPolyUnitSlot
        | HostFnBodyKind::StagedSecond { .. }
        | HostFnBodyKind::NestedCurriedRoundtrip { .. }
        | HostFnBodyKind::InvokeSubstitutedUnitCallback { .. }
        | HostFnBodyKind::ReturnedForallUnit
        | HostFnBodyKind::ObservePacked { .. }
        | HostFnBodyKind::TraceUnit { .. }
        | HostFnBodyKind::StagedUnitCall
        | HostFnBodyKind::UnreachableI32Print { .. } => CanonicalKind::Custom,
    }
}

/// Render a host fn body, honoring the host-* roundtrip protocols' bespoke
/// shaped host fns (`make_token` / `token_value`, `box_make` / `box_get`,
/// `apply_poly`, `make_step`, `make_pair`, `call_step`, `sum_to_string`)
/// before the canonical-kind dispatch. The bespoke bodies key on the
/// binding's structured body and mirror the Go runner's semantics, building /
/// destructuring shaped values through the emitted `EnvP_H…` (product) /
/// `EnvS_H…` (sum arm) boundary patterns. Exact host types use the ordinary
/// fixture types selected by
/// `KioRunnerHostTypes`.
fn render_haskell_body_for_binding(
    binding: &HostFnBinding,
    kind: &CanonicalKind,
    source: &ModuleItemId,
    target_namespace: &str,
    host_types: &[HostTypeBinding],
) -> String {
    let boundary = |root| BoundaryId::env_item(source, root);
    match binding.body {
        // `make_token(I32) -> Token` using the runner's exact Token newtype.
        HostFnBodyKind::MakeToken { value_i32, .. } => {
            let value =
                haskell_role_into_native(value_i32, host_types, target_namespace, "arg0");
            format!("\\arg0 -> pure (KioRunnerToken ({value}))")
        }
        // `token_value(Token) -> I32`: unwrap the fixture newtype.
        HostFnBodyKind::TokenValue { value_i32, .. } => {
            let value =
                haskell_role_from_native(value_i32, host_types, target_namespace, "arg0");
            format!("\\(KioRunnerToken arg0) -> pure ({value})")
        }
        // `box_make` / `box_get` wrap and unwrap the runner's exact Box type.
        HostFnBodyKind::BoxMake { box_type: _ } => "\\arg0 -> pure (KioRunnerBox arg0)".to_owned(),
        HostFnBodyKind::BoxGet { box_type: _ } => "\\(KioRunnerBox arg0) -> pure arg0".to_owned(),
        // The emitted polymorphic-function newtype crosses unchanged.
        HostFnBodyKind::RoundFunctor | HostFnBodyKind::RoundPicker => {
            "\\arg0 -> pure arg0".to_owned()
        }
        HostFnBodyKind::RoundPolyThunk => {
            "\\arg0 -> kioPure @(forall (t :: Data.Kind.Type). IO (IO ())) arg0".to_owned()
        }
        HostFnBodyKind::RoundPolyUnitSlot => "\\arg0 -> pure arg0".to_owned(),
        HostFnBodyKind::ObservePacked { .. } => {
            let read = pkg_newtype_export("testapi/types", "Packed", "read_packed");
            format!("\\arg0 -> do {{ Data.IORef.modifyIORef' existentialCounts (\\(observations, openings) -> (observations + 1, openings)); {read} pkg arg0 (pure (\\seed step -> do {{ Data.IORef.modifyIORef' existentialCounts (\\(observations, openings) -> (observations, openings + 1)); step seed }})) }}")
        }
        HostFnBodyKind::StagedSecond { .. } => "\\_arg0 arg1 -> pure arg1".to_owned(),
        HostFnBodyKind::NestedCurriedRoundtrip { .. } => {
            "\\arg0 -> do { step <- arg0 (T.pack \"host-left\"); value <- step (T.pack \"host-right\"); TIO.putStrLn (T.pack \"round host probe: \" <> value); pure arg0 }".to_owned()
        }
        HostFnBodyKind::InvokeSubstitutedUnitCallback { .. } => {
            "\\arg0 -> arg0 () >>= \\value -> pure (T.pack \"host/\" <> value)".to_owned()
        }
        HostFnBodyKind::ReturnedForallUnit => {
            "do { produced <- Data.IORef.readIORef returnedForallProduced; if produced then do { TIO.putStrLn (T.pack \"throw\"); Control.Exception.throwIO (System.IO.Error.userError \"produce failed\") } else do { Data.IORef.writeIORef returnedForallProduced True; TIO.putStrLn (T.pack \"produce\"); pure kioRunnerPolymorphicUnit } }".to_owned()
        }
        HostFnBodyKind::TraceUnit { text } => {
            format!("TIO.putStrLn (T.pack {text:?})")
        }
        HostFnBodyKind::StagedUnitCall => {
            "\\_arg0 -> TIO.putStrLn (T.pack \"staged Unit host call\")".to_owned()
        }
        // `apply_poly(f) -> String`: instantiate the rank-N callback at Text,
        // sequence the observable type-application stage, then invoke the
        // resulting value function.
        HostFnBodyKind::ApplyPoly { .. } => {
            "\\arg0 -> arg0 @Data.Text.Text >>= \\__poly -> __poly (T.pack \"rank-n\\n\")"
                .to_owned()
        }
        // `make_step(delta) -> I32 -> I32`: return a native step closure.
        HostFnBodyKind::MakeStep { .. } => "\\arg0 -> pure (\\n -> pure (n + arg0))".to_owned(),
        // `make_pair(n, s) -> (I32 & String)`: build the product through its
        // `_ret_P` synonym.
        HostFnBodyKind::MakePairStructural { .. } => {
            let pat = format!(
                "Pkg.{}",
                haskell_abi::product_pattern(target_namespace, &boundary(BoundaryRoot::Ret))
            );
            format!("\\arg0 arg1 -> pure ({pat} arg0 arg1)")
        }
        HostFnBodyKind::ProducePair { .. } => {
            let pat = format!("Pkg.{}", haskell_abi::product_pattern(target_namespace, &boundary(BoundaryRoot::Ret)));
            format!("putStrLn \"direct\" >> pure ({pat} 7 (T.pack \"direct-value\"))")
        }
        // `sum_to_string(v) -> String`: match the `(I32 | String)` sum through
        // its per-arm synonyms; arm 0 stringifies the I32, arm 1 is the String.
        HostFnBodyKind::SumToString { .. } => {
            let sum = boundary(BoundaryRoot::Arg(0));
            let arm0 = format!(
                "Pkg.{}",
                haskell_abi::sum_pattern(target_namespace, &sum, StructuralKey::positional(0), 0,)
            );
            let arm1 = format!(
                "Pkg.{}",
                haskell_abi::sum_pattern(target_namespace, &sum, StructuralKey::positional(1), 1,)
            );
            format!(
                "\\arg0 -> pure (case arg0 of {{ {arm0} n -> T.pack (show n); {arm1} s -> s }})"
            )
        }
        // `call_step(step, seed) -> I32`: the callback's source product is its
        // public value domain, so apply its three canonical slots directly.
        HostFnBodyKind::CallStep { .. } => {
            "\\arg0 arg1 -> arg0 arg1 (T.pack \"compound-callback\") True".to_owned()
        }
        // `make_pair(build, seed) -> I32`: apply the callback (returns `(I32 &
        // String)` through its `_cbret_P` synonym) and take the first field.
        HostFnBodyKind::MakePairCallback { .. } => {
            let shaped = boundary(BoundaryRoot::Arg(0)).nested(BoundaryStep::CallbackRet);
            let pat = format!(
                "Pkg.{}",
                haskell_abi::product_pattern(target_namespace, &shaped)
            );
            format!("\\arg0 arg1 -> arg0 arg1 >>= \\p -> case p of {{ {pat} f0 _ -> pure f0 }}")
        }
        HostFnBodyKind::UnreachableI32Print { .. } => {
            "\\_ -> error \"unreachable host fixture\"".to_owned()
        }
        HostFnBodyKind::Print { string } => {
            let value = haskell_role_into_native(string, host_types, target_namespace, "arg0");
            format!("\\arg0 -> TIO.putStr ({value})")
        }
        HostFnBodyKind::Eprint { string } => {
            let value = haskell_role_into_native(string, host_types, target_namespace, "arg0");
            format!("\\arg0 -> hPutStr stderr (T.unpack ({value}))")
        }
        HostFnBodyKind::NumericToString { value, string } => {
            let native = haskell_role_into_native(value, host_types, target_namespace, "arg0");
            let rendered = match value.fixture.role() {
                "f32" => format!(
                    "T.pack (let {{ s = show (realToFrac ({native}) :: Double) }} in if Data.List.isSuffixOf \".0\" s then take (length s - 2) s else s)"
                ),
                "f64" => format!(
                    "T.pack (let {{ s = show ({native}) }} in if Data.List.isSuffixOf \".0\" s then take (length s - 2) s else s)"
                ),
                _ => format!("T.pack (show ({native}))"),
            };
            let result =
                haskell_role_from_native(string, host_types, target_namespace, &rendered);
            format!("\\arg0 -> pure ({result})")
        }
        HostFnBodyKind::Arithmetic {
            operation, number, ..
        } if matches!(
            number.resolve(host_types).fixture,
            HostTypeFixture::SelectedRole(_)
        ) =>
        {
            let left = haskell_role_into_native(number, host_types, target_namespace, "arg0");
            let right = haskell_role_into_native(number, host_types, target_namespace, "arg1");
            let value = match operation {
                "add" => format!("({left}) + ({right})"),
                "sub" => format!("({left}) - ({right})"),
                "mul" => format!("({left}) * ({right})"),
                "div" => format!("quot ({left}) ({right})"),
                "mod" => format!("rem ({left}) ({right})"),
                other => unreachable!(
                    "unsupported selected-role arithmetic operation `{other}` in Haskell runner"
                ),
            };
            let result =
                haskell_role_from_native(number, host_types, target_namespace, &value);
            format!("\\arg0 arg1 -> pure ({result})")
        }
        _ => render_haskell_body(kind, source, target_namespace),
    }
}

/// Render a native-typed Haskell host body (an `IO`-returning lambda) for
/// one canonical kind. The args arrive as native Haskell values (`arg0`,
/// `arg1`, …); the body returns `IO <native ret>`.
fn render_haskell_body(
    kind: &CanonicalKind,
    source: &ModuleItemId,
    target_namespace: &str,
) -> String {
    let source_leaf = source.leaf();
    let ret_boundary = || BoundaryId::env_item(source, BoundaryRoot::Ret);
    match kind {
        CanonicalKind::Print => "\\arg0 -> TIO.putStr arg0".to_owned(),
        CanonicalKind::Eprint => "\\arg0 -> hPutStr stderr (T.unpack arg0)".to_owned(),
        CanonicalKind::PrintI32 => "\\arg0 -> TIO.putStr (T.pack (show arg0))".to_owned(),
        CanonicalKind::Exit => {
            // `exitWith :: ExitCode -> IO a` is return-polymorphic through
            // divergence, so it inhabits the emitted `IO Data.Void.Void`
            // field directly. A trailing concrete return would break that
            // boundary type.
            "\\arg0 -> let { n = fromIntegral arg0 :: Int; c = max 0 (min 125 n) } in \
             exitWith (if c == 0 then ExitSuccess else ExitFailure c)"
                .to_owned()
        }
        CanonicalKind::StringLen => "\\arg0 -> pure (fromIntegral (T.length arg0))".to_owned(),
        CanonicalKind::StringSlice => render_string_slice(),
        CanonicalKind::StringConcat => "\\arg0 arg1 -> pure (T.append arg0 arg1)".to_owned(),
        CanonicalKind::StringEq => "\\arg0 arg1 -> pure (arg0 == arg1)".to_owned(),
        CanonicalKind::BoolToString => {
            "\\arg0 -> pure (T.pack (if arg0 then \"true\" else \"false\"))".to_owned()
        }
        CanonicalKind::NumericToString { kind } => numeric_to_string_body(kind),
        CanonicalKind::Arith { op, kind } => int_arith_body(op, kind),
        CanonicalKind::FloatArith { op, .. } => float_arith_body(op),
        CanonicalKind::Cmp { cmp, kind } => cmp_body(cmp, kind),
        // Sum-returning host fns build their result through the emitted
        // package's per-arm boundary patterns (`<ret-alias>_<k>`), keeping
        // the bodies independent of the nested `Either` representation.
        CanonicalKind::StringCodeAt => render_string_code_at(target_namespace, &ret_boundary()),
        CanonicalKind::StringToInt => render_string_to_int(target_namespace, &ret_boundary()),
        CanonicalKind::ReadAsciiLine => render_read_ascii_line(target_namespace, &ret_boundary()),
        CanonicalKind::Loop => {
            let callback_ret = BoundaryId::env_item(source, BoundaryRoot::Arg(0))
                .nested(BoundaryStep::CallbackRet);
            render_loop_body(target_namespace, &callback_ret)
        }
        CanonicalKind::Array(op) => render_array_body(op, target_namespace, &ret_boundary()),
        // The runner chooses one ordinary tagged host type for the package's
        // exact non-role `Scalar` declaration.
        CanonicalKind::MakeScalar => render_make_scalar_body(),
        // `scalar_of_<kind>(v) -> Scalar`: inject the native value into the
        // runner's exact tagged Scalar type.
        CanonicalKind::ScalarOf { kind } => render_scalar_of_body(kind),
        // `scalar_as_<kind>(Scalar) -> . | <Kind>`: inspect the tagged
        // scalar; arm `_1` is the typed value when it holds that kind,
        // arm `_0` is unit otherwise.
        CanonicalKind::ScalarAs { kind } => {
            render_scalar_as_body(kind, target_namespace, &ret_boundary())
        }
        // `scalar_is_true(Scalar) -> Bool`: a bool-shaped scalar yields its
        // truth; any other shape is not true (a well-typed guest never asks).
        CanonicalKind::ScalarIsTrue => {
            "\\arg0 -> pure (case arg0 of { KioRunnerScalarBool b -> b; _ -> False })".to_owned()
        }
        CanonicalKind::Custom => unreachable!(
            "Haskell protocol body `{source_leaf}` reached canonical rendering without an exact body"
        ),
    }
}

/// `make_scalar(text, representation) -> Scalar`: use the fixture-private
/// representation key to parse the value text into the runner's chosen exact
/// `Scalar` host type.
fn render_make_scalar_body() -> String {
    "\\arg0 arg1 -> pure (case arg1 of \
     { _ | arg1 == T.pack \"I32\" || arg1 == T.pack \"Int\" -> KioRunnerScalarI32 (read (T.unpack arg0)) \
     ; _ | arg1 == T.pack \"F64\" || arg1 == T.pack \"F32\" -> KioRunnerScalarF64 (read (T.unpack arg0)) \
     ; _ | arg1 == T.pack \"String\" || arg1 == T.pack \"Str\" -> KioRunnerScalarString arg0 \
     ; _ | arg1 == T.pack \"Bool\" -> KioRunnerScalarBool (arg0 == T.pack \"t\") \
     ; _ -> error (\"make_scalar: unknown representation key \" ++ T.unpack arg1) })"
        .to_owned()
}

/// `scalar_of_<kind>(v) -> Scalar`: inject into the runner's exact scalar.
fn render_scalar_of_body(kind: &str) -> String {
    let wrap = match kind {
        "i32" => "KioRunnerScalarI32 arg0",
        "f64" => "KioRunnerScalarF64 arg0",
        "str" => "KioRunnerScalarString arg0",
        "bool" => "KioRunnerScalarBool arg0",
        other => return format!("\\arg0 -> error \"scalar_of_{other}: unsupported kind\""),
    };
    format!("\\arg0 -> pure ({wrap})")
}

/// `scalar_as_<kind>(Scalar) -> . | <Kind>`: project the erased scalar. Arm
/// `_1` is the typed value when the box holds that kind; arm `_0` is unit.
fn render_scalar_as_body(kind: &str, target_namespace: &str, boundary: &BoundaryId) -> String {
    let (v0, v1) = sum_arm_patterns(target_namespace, boundary);
    match kind {
        "i32" => format!(
            "\\arg0 -> pure (case arg0 of {{ KioRunnerScalarI32 n -> {v1} n; _ -> {v0} () }})"
        ),
        "f64" => format!(
            "\\arg0 -> pure (case arg0 of {{ KioRunnerScalarF64 d -> {v1} d; _ -> {v0} () }})"
        ),
        "str" => format!(
            "\\arg0 -> pure (case arg0 of {{ KioRunnerScalarString s -> {v1} s; _ -> {v0} () }})"
        ),
        "bool" => format!(
            "\\arg0 -> pure (case arg0 of {{ KioRunnerScalarBool b -> {v1} b; _ -> {v0} () }})"
        ),
        other => format!("\\arg0 -> error \"scalar_as_{other}: unsupported kind\""),
    }
}

fn render_string_slice() -> String {
    // `string_slice(s, start, end) -> Str`: half-open byte slice; invalid
    // ranges fail loudly. Text indexes by character; Kio's canonical
    // strings here are ASCII, so char index == byte index.
    "\\arg0 arg1 arg2 -> let { st = fromIntegral arg1 :: Int; en = fromIntegral arg2 :: Int; \
     n = T.length arg0 } in if st < 0 || st > en || en > n \
     then (hPutStr stderr \"string_slice: invalid range\\n\" >> exitWith (ExitFailure 1) >> pure T.empty) \
     else pure (T.take (en - st) (T.drop st arg0))"
        .to_owned()
}

/// The two public per-arm pattern synonyms for a positional binary sum.
fn sum_arm_patterns(target_namespace: &str, boundary: &BoundaryId) -> (String, String) {
    (
        pkg_sum_pattern(target_namespace, boundary, StructuralKey::positional(0), 0),
        pkg_sum_pattern(target_namespace, boundary, StructuralKey::positional(1), 1),
    )
}

/// `string_code_at(s, i) -> Int | .`: arm `_0` is the byte value, arm `_1`
/// is unit. Build through the emitted per-arm pattern synonyms.
fn render_string_code_at(target_namespace: &str, boundary: &BoundaryId) -> String {
    let (v0, v1) = sum_arm_patterns(target_namespace, boundary);
    format!(
        "\\arg0 arg1 -> let {{ n = fromIntegral arg1 :: Int }} in \
         if n < 0 || n >= T.length arg0 then pure ({v1} ()) \
         else pure ({v0} (fromIntegral (Data.Char.ord (T.index arg0 n))))"
    )
}

/// `string_to_int(s) -> Int | .`: arm `_0` is the parsed i32, arm `_1` is
/// unit when the text is not a valid signed base-10 i32.
fn render_string_to_int(target_namespace: &str, boundary: &BoundaryId) -> String {
    let (v0, v1) = sum_arm_patterns(target_namespace, boundary);
    // `Text.Read.readMaybe` over the unpacked String, narrowed to Int32 so
    // out-of-range input yields the `()` arm (matching the JS / Go / Swift
    // runners' i32 parse).
    format!(
        "\\arg0 -> case (Text.Read.readMaybe (T.unpack arg0) :: Maybe Integer) of \
         {{ Just n | n >= -2147483648 && n <= 2147483647 -> pure ({v0} (fromInteger n)); \
         _ -> pure ({v1} ()) }}"
    )
}

/// `read_ascii_line() -> Str | .`: arm `_0` is the next ASCII line from
/// stdin, arm `_1` is unit at EOF. Non-ASCII input is a runner error.
fn render_read_ascii_line(target_namespace: &str, boundary: &BoundaryId) -> String {
    let (v0, v1) = sum_arm_patterns(target_namespace, boundary);
    format!(
        "do {{ eof <- isEOF; \
         if eof then pure ({v1} ()) else do {{ \
         line <- getLine; \
         if all (\\c -> Data.Char.ord c < 128) line \
         then pure ({v0} (T.pack line)) \
         else (hPutStr stderr \"read_ascii_line: non-ASCII input\\n\" >> exitWith (ExitFailure 1) >> pure ({v1} ())) }} }}"
    )
}

/// `loop[s][r](step, state) -> r`: drive the step callback until it returns
/// the exit arm. The step's `s | r` return is the `arg0_cbret` sum; arm
/// `_0` continues (the next state), `_1` exits (the result). The per-arm
/// pattern synonyms are rendered from the callback-return boundary identity.
fn render_loop_body(target_namespace: &str, boundary: &BoundaryId) -> String {
    let (v0, v1) = sum_arm_patterns(target_namespace, boundary);
    format!(
        "\\arg0 arg1 -> let {{ go st = arg0 st >>= \\res -> case res of \
         {{ {v0} next -> go next; {v1} result -> pure result }} }} in go arg1"
    )
}

/// Render an `array_*` host body. The runner's exact `Array(T)` type wraps an
/// `IORef [T]`; `array_pop_back` returns a `T | .` sum through its per-arm
/// pattern synonyms.
fn render_array_body(op: &ArrayOp, target_namespace: &str, ret_boundary: &BoundaryId) -> String {
    // The receiver array handle is `arg0`; bind its `IORef` once.
    let recv = "let KioRunnerArray r = arg0 in ";
    match op {
        ArrayOp::MakeEmpty => "fmap KioRunnerArray (Data.IORef.newIORef [])".to_owned(),
        ArrayOp::MakeFilled => {
            // arg0 = count (Int32), arg1 = fill element.
            "\\arg0 arg1 -> if arg0 < 0 then error \"array_make_filled: negative size\" \
             else fmap KioRunnerArray (Data.IORef.newIORef (replicate (fromIntegral arg0) arg1))"
                .to_owned()
        }
        ArrayOp::Len => {
            format!("\\arg0 -> {recv} fmap (fromIntegral . length) (Data.IORef.readIORef r)")
        }
        ArrayOp::Get => {
            format!(
                "\\arg0 arg1 -> {recv} do {{ xs <- Data.IORef.readIORef r; \
                 let {{ i = fromIntegral arg1 :: Int }}; \
                 if i < 0 || i >= length xs then error \"array_get: out of bounds\" \
                 else pure (xs !! i) }}"
            )
        }
        ArrayOp::Set => {
            format!(
                "\\arg0 arg1 arg2 -> {recv} do {{ xs <- Data.IORef.readIORef r; \
                 let {{ i = fromIntegral arg1 :: Int }}; \
                 if i < 0 || i >= length xs then error \"array_set: out of bounds\" \
                 else Data.IORef.writeIORef r (take i xs ++ [arg2] ++ drop (i + 1) xs) }}"
            )
        }
        ArrayOp::Push => {
            format!("\\arg0 arg1 -> {recv} Data.IORef.modifyIORef' r (++ [arg1])")
        }
        ArrayOp::PopBack => {
            let (v0, v1) = sum_arm_patterns(target_namespace, ret_boundary);
            format!(
                "\\arg0 -> {recv} do {{ xs <- Data.IORef.readIORef r; \
                 if null xs then pure ({v1} ()) \
                 else do {{ Data.IORef.writeIORef r (init xs); pure ({v0} (last xs)) }} }}"
            )
        }
        ArrayOp::Swap => {
            format!(
                "\\arg0 arg1 arg2 -> {recv} do {{ xs <- Data.IORef.readIORef r; \
                 let {{ i = fromIntegral arg1 :: Int; j = fromIntegral arg2 :: Int; n = length xs }}; \
                 if i < 0 || i >= n || j < 0 || j >= n then error \"array_swap: out of bounds\" \
                 else Data.IORef.writeIORef r [ if k == i then xs !! j else if k == j then xs !! i else x | (k, x) <- zip [0..] xs ] }}"
            )
        }
        ArrayOp::Clear => {
            format!("\\arg0 -> {recv} Data.IORef.writeIORef r []")
        }
        ArrayOp::Clone => {
            format!(
                "\\arg0 -> {recv} do {{ xs <- Data.IORef.readIORef r; \
                 fmap KioRunnerArray (Data.IORef.newIORef xs) }}"
            )
        }
    }
}

fn int_arith_body(op: &str, kind: &str) -> String {
    if kind == "i128" || kind == "u128" {
        let signed = kind == "i128";
        let core = match op {
            "add" => "(arg0 + arg1)",
            "sub" => "(arg0 - arg1)",
            "mul" => "(arg0 * arg1)",
            "div" => "(quot arg0 arg1)",
            "mod" => "(rem arg0 arg1)",
            _ => "arg0",
        };
        // Wrap an Integer into the 128-bit two's-complement / modular range.
        let wrap = if signed {
            format!(
                "let {{ m = 340282366920938463463374607431768211456; h = 170141183460469231731687303715884105728; r0 = {core} `mod` m; r = if r0 < 0 then r0 + m else r0 }} in if r >= h then r - m else r"
            )
        } else {
            format!(
                "let {{ m = 340282366920938463463374607431768211456; r0 = {core} `mod` m }} in if r0 < 0 then r0 + m else r0"
            )
        };
        return format!("\\arg0 arg1 -> pure ({wrap})");
    }
    // Fixed-width Haskell integers wrap on overflow (the `Int8`/.../`Word64`
    // arithmetic is modular), matching Kio's width-mod semantics.
    let core = match op {
        "add" => "arg0 + arg1",
        "sub" => "arg0 - arg1",
        "mul" => "arg0 * arg1",
        "div" => "quot arg0 arg1",
        "mod" => "rem arg0 arg1",
        _ => "arg0",
    };
    format!("\\arg0 arg1 -> pure ({core})")
}

fn float_arith_body(op: &str) -> String {
    let core = match op {
        "add" => "arg0 + arg1",
        "sub" => "arg0 - arg1",
        "mul" => "arg0 * arg1",
        "div" => "arg0 / arg1",
        _ => "arg0",
    };
    format!("\\arg0 arg1 -> pure ({core})")
}

fn cmp_body(cmp: &str, _kind: &str) -> String {
    let op = match cmp {
        "eq" => "==",
        "lt" => "<",
        "leq" | "le" => "<=",
        "gt" => ">",
        "geq" | "ge" => ">=",
        _ => "==",
    };
    format!("\\arg0 arg1 -> pure (arg0 {op} arg1)")
}

fn numeric_to_string_body(kind: &str) -> String {
    match kind {
        "f32" | "f64" => float_to_string_body(kind),
        _ => "\\arg0 -> pure (T.pack (show arg0))".to_owned(),
    }
}

fn float_to_string_body(kind: &str) -> String {
    // The canonical Kio float string is the shortest round-trip form with no
    // trailing `.0` on a whole-valued float — what JS (`String(3.0)` → `"3"`)
    // and the Go runner (`strconv.FormatFloat(x, 'g', -1, 64)`) produce.
    // Haskell `show` on a `Double` prints the shortest decimal that
    // round-trips but keeps the `.0` (`3.0`, `1.5`, `0.1`); strip a trailing
    // `.0` so a whole-valued float matches (`3.0` → `3`). A non-whole value
    // (`562.406`) and scientific notation (`…e21`, never ending in `.0`) are
    // left as `show` gives them. An f32 widens to `Double` first so both
    // kinds format identically.
    let show_expr = if kind == "f32" {
        "show (realToFrac arg0 :: Double)"
    } else {
        "show arg0"
    };
    format!(
        "\\arg0 -> pure (T.pack (let {{ s = {show_expr} }} in \
         if Data.List.isSuffixOf \".0\" s then take (length s - 2) s else s))"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_collection_preserves_a_dotted_facade_path() {
        let root = tempfile::tempdir().expect("create source tree");
        let nested = root.path().join("Foo");
        fs::create_dir_all(&nested).expect("create namespace directory");
        fs::write(nested.join("Runtime.hs"), b"module Foo.Runtime where\n")
            .expect("write dotted facade");

        let mut files = Vec::new();
        collect_haskell_files(root.path(), &mut files).expect("collect Haskell facade");

        assert_eq!(
            files,
            vec![(
                PathBuf::from("Foo/Runtime.hs"),
                b"module Foo.Runtime where\n".to_vec()
            )]
        );
    }

    #[test]
    fn main_wrapper_uses_the_exact_declaring_module() {
        assert_eq!(main_wrapper_name("main"), pkg_export("main", "main"));
        assert_eq!(
            main_wrapper_name("testapi/main"),
            pkg_export("testapi/main", "main")
        );
        assert_eq!(main_wrapper_name("prog"), pkg_export("prog", "main"));
        assert_eq!(main_wrapper_name("api"), pkg_export("api", "main"));
    }

    #[test]
    fn single_package_drivers_use_only_the_public_package_module() {
        for &protocol in RunnerProtocol::ALL {
            if protocol == RunnerProtocol::Coexist {
                continue;
            }
            let namespace = "RunnerFixture";
            let host = haskell_host_api_for_protocol(protocol, namespace);
            let host_types = contract_host_types(protocol, namespace);
            let driver = build_driver(&host, protocol, namespace, &host_types)
                .unwrap_or_else(|error| panic!("{protocol:?}: {error}"));

            for private_name in ["RunnerFixture.Runtime", "KioRuntime", "KioOpaque"] {
                for method in host.methods() {
                    assert!(
                        method
                            .arg_types
                            .iter()
                            .chain(std::iter::once(&method.ret_type))
                            .chain(std::iter::once(&method.where_clause))
                            .all(|part| !part.contains(private_name)),
                        "{protocol:?} host model depends on private `{private_name}`: {method:?}"
                    );
                }
                assert!(
                    !driver.contains(private_name),
                    "{protocol:?} runner depends on private `{private_name}`:\n{driver}"
                );
            }
        }
    }

    #[test]
    fn rejects_removed_main_entry_option() {
        assert_eq!(run(&["--main-entry=function".to_owned()]), EXIT_USAGE);
    }

    #[test]
    fn canonical_exit_uses_the_void_result_without_a_concrete_tail() {
        let contract = RunnerProtocol::TestApiIo.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::Exit { .. }))
            .expect("exit binding");
        let method = haskell_canonical_method(binding, contract.host_types, "Pkg");
        assert_eq!(method.ret_type, "Data.Void.Void");

        let source = ModuleItemId::new("testapi/io", "exit");
        let body = render_haskell_body(&CanonicalKind::Exit, &source, "Pkg");
        assert!(body.contains("exitWith"));
        assert!(!body.contains("pure"));
    }

    #[test]
    fn module_alias_driver_matches_exact_products_before_flattened_calls() {
        let contract = RunnerProtocol::ModuleAliasScopeCollision.contract();
        let driver = render_export_driver(
            ExportDriver::ModuleAliasScopeCollision,
            "Fixture",
            contract.host_types,
        )
        .expect("module-alias driver");
        let a_ret = pkg_product_pattern(
            "Fixture",
            &export_boundary("testapi/a", "make", BoundaryRoot::Ret),
        );
        let b_ret = pkg_product_pattern(
            "Fixture",
            &export_boundary("testapi/b", "make", BoundaryRoot::Ret),
        );

        assert!(driver.contains(&format!("\\({a_ret} a0 a1)")), "{driver}");
        assert!(
            driver.contains(&format!("\\({b_ret} b0 b1 b2)")),
            "{driver}"
        );
        assert!(driver.contains("pkg a0 a1"), "{driver}");
        assert!(driver.contains("pkg b0 b1 b2"), "{driver}");
    }

    #[test]
    fn public_word_names_driver_constructs_and_projects_generic_nominals() {
        let contract = RunnerProtocol::PublicWordNames.contract();
        let driver = render_export_driver(
            ExportDriver::PublicWordNames,
            "Fixture",
            contract.host_types,
        )
        .expect("public-word-names driver");
        let first_wrap = pkg_newtype_export("word_api/word_nodes", "_Word_box__", "wrap_word");
        let second_wrap = pkg_newtype_export("word_api/other_nodes", "_Word_box__", "wrap_word");
        let first_unwrap = pkg_newtype_export("word_api/word_nodes", "_Word_box__", "unwrap_word");
        let second_unwrap =
            pkg_newtype_export("word_api/other_nodes", "_Word_box__", "unwrap_word");
        for literal in [
            format!("firstBox <- {first_wrap} pkg (77 :: Data.Int.Int32)"),
            format!("secondBox <- {second_wrap} pkg (88 :: Data.Int.Int32)"),
            "pkg firstBox secondBox >>= ".to_owned(),
            format!("{first_unwrap} pkg firstWord >>= print"),
            format!("{second_unwrap} pkg secondWord >>= print"),
        ] {
            assert!(driver.contains(&literal), "{driver}");
        }
    }

    // The coexist driver must brand the emitted record / factory names
    // from the namespace's **final segment** (`handle_of`), matching the
    // emitter's `HaskellNames` derivation (`specs/backends/haskell.md`
    // § Output layout). A whole-namespace brand only coincides for a
    // single-segment namespace; for a dotted one, `create<Ns>` is not
    // even a valid Haskell identifier. The single-package driver path
    // already brands through `handle_of`; these pin the coexist driver
    // to the same derivation.
    #[test]
    fn coexist_driver_brands_from_final_segment_for_dotted_namespace() {
        let src = coexist_driver_source(
            "Com.Acme.Alpha",
            "Com.Acme.Beta",
            "KioRunnerHostA",
            "KioRunnerHostB",
            &[],
            &[],
        )
        .expect("empty host-type surfaces are valid");
        assert!(src.main.contains("PkgA.createAlpha hostA"));
        assert!(src.main.contains("PkgB.createBeta hostB"));
        assert!(src.main.contains("PkgA.AlphaHost {"));
        assert!(src.main.contains("PkgB.BetaHost {"));
        assert!(!src.main.contains("createCom.Acme.Alpha"));
        assert!(!src.main.contains("createCom.Acme.Beta"));
    }

    #[test]
    fn coexist_driver_single_segment_brand_equals_namespace() {
        let src = coexist_driver_source(
            "PkgAlpha",
            "PkgBeta",
            "KioRunnerHostA",
            "KioRunnerHostB",
            &[],
            &[],
        )
        .expect("empty host-type surfaces are valid");
        assert!(src.main.contains("PkgA.createPkgAlpha hostA"));
        assert!(src.main.contains("PkgB.createPkgBeta hostB"));
        assert!(src.main.contains("PkgA.PkgAlphaHost {"));
    }

    #[test]
    fn coexist_driver_disambiguates_artifacts_from_standard_package_imports() {
        let src = coexist_driver_source(
            "Data.Text",
            "Data.Text.Runtime",
            "KioRunnerHostA",
            "KioRunnerHostB",
            &[],
            &[],
        )
        .expect("standard-module-shaped package namespaces are valid");

        assert!(
            src.main
                .contains("import qualified \"text\" Data.Text as T\n")
        );
        assert!(src.main.contains("import qualified Data.Text as PkgA\n"));
        assert!(
            src.main
                .contains("import qualified Data.Text.Runtime as PkgB\n")
        );
        assert!(
            src.host_a
                .contains("import Data.Text as PkgTypes (TextHostTypes(..))")
        );
    }

    #[test]
    fn coexist_driver_binds_each_packages_host_types_to_its_own_marker() {
        let host_types = vec![AssocType {
            name: "HostType__greeter__String".to_owned(),
            boundary_name: None,
            role: "str".to_owned(),
            type_params: Vec::new(),
        }];
        let src = coexist_driver_source(
            "PkgAlpha",
            "PkgBeta",
            "KioRunnerHostA",
            "KioRunnerHostB",
            &host_types,
            &host_types,
        )
        .expect("role-bearing scalar host types are supported");
        assert!(
            src.host_a
                .contains("instance PkgAlphaHostTypes KioRunnerHostTypes where")
        );
        assert!(
            src.host_a
                .contains("type HostType__greeter__String KioRunnerHostTypes = Data.Text.Text")
        );
        assert!(
            src.host_a
                .contains("import PkgAlpha as PkgTypes (PkgAlphaHostTypes(..))")
        );
        assert!(
            src.host_b
                .contains("instance PkgBetaHostTypes KioRunnerHostTypes where")
        );
        assert!(
            src.main
                .contains("hostA :: PkgA.PkgAlphaHost HostA.KioRunnerHostTypes IO")
        );
        assert!(
            src.main
                .contains("hostB :: PkgB.PkgBetaHost HostB.KioRunnerHostTypes IO")
        );
    }

    #[test]
    fn coexist_driver_helper_modules_do_not_collide_with_package_modules() {
        let files = vec![(PathBuf::from("KioRunnerHostA.hs"), Vec::new())];
        let (host_a, host_b) = coexist_host_module_names(&files);
        assert_eq!(host_a, "KioRunnerHostA1");
        assert_eq!(host_b, "KioRunnerHostB");

        let src = coexist_driver_source("KioRunnerHostA", "PkgBeta", &host_a, &host_b, &[], &[])
            .expect("a package namespace may share the helper's preferred stem");
        assert!(
            src.main
                .contains("import qualified KioRunnerHostA as PkgA\n")
        );
        assert!(
            src.main
                .contains("import qualified KioRunnerHostA1 as HostA")
        );
        assert!(src.host_a.contains("module KioRunnerHostA1"));
    }

    #[test]
    fn contract_host_types_reconstruct_exact_family_names_and_fixtures() {
        let host_types = contract_host_types(RunnerProtocol::HostGenericTypeRoundtrip, "Pkg");
        let box_type = host_types
            .iter()
            .find(|host_type| host_type.source_name == "Box")
            .expect("Box contract binding");
        assert_eq!(box_type.module_path, "testapi");
        assert_eq!(box_type.fixture, HostTypeFixture::Box);
        assert_eq!(box_type.param_kind_arities, vec![0]);
        assert_eq!(box_type.assoc.name, "HostType__testapi__Box");

        let (decls, equations) = render_runner_host_types(&host_types, "Pkg")
            .expect("contract Box declaration is renderable");
        assert!(decls.contains("newtype KioRunnerBox a = KioRunnerBox a"));
        assert!(
            equations.contains("type HostType__testapi__Box KioRunnerHostTypes = KioRunnerBox")
        );
    }

    #[test]
    fn selected_role_fixture_drives_the_distinct_i32_host_type() {
        let host_types = contract_host_types(RunnerProtocol::NewtypeVisibilityFacade, "Pkg");
        assert_eq!(host_types.len(), 1);
        assert_eq!(
            host_types[0].fixture,
            HostTypeFixture::SelectedRole(RoleFixture::I32)
        );

        let (decls, equations) = render_runner_host_types(&host_types, "Pkg")
            .expect("selected I32 declaration is renderable");
        let selected_type = haskell_selected_role_type("Pkg", "testapi", "I32");
        assert!(decls.contains(&format!("newtype {selected_type}")));
        assert!(equations.contains(&format!("= {selected_type}")));

        let selected = HostRoleRef::new("testapi", "I32", RoleFixture::I32);
        let ordinary = HostRoleRef::new("testapi", "Int", RoleFixture::I32);
        let exact_types = [
            HostTypeBinding::selected_role("testapi", "I32", RoleFixture::I32),
            HostTypeBinding::role("testapi", "Int", RoleFixture::I32),
        ];
        assert_eq!(
            haskell_role_ref_type(selected, &exact_types, "Pkg"),
            selected_type
        );
        assert_eq!(
            haskell_role_ref_type(ordinary, &exact_types, "Pkg"),
            "Data.Int.Int32"
        );
        let binding = HostFnBinding {
            module: "testapi/io",
            leaf: "print_i32",
            body: HostFnBodyKind::PrintI32 { value: selected },
        };
        assert_eq!(
            haskell_method(&binding, &exact_types, "Pkg").arg_types,
            [haskell_selected_role_type("Pkg", "testapi", "I32")]
        );
    }

    #[test]
    fn selected_role_markers_preserve_each_exact_host_type_identity() {
        let host_types = contract_host_types(RunnerProtocol::HostTypeRoundtrip, "Pkg");
        let (decls, equations) = render_runner_host_types(&host_types, "Pkg")
            .expect("host-type roundtrip markers are renderable");
        let count = haskell_selected_role_type("Pkg", "testapi", "Count");
        let i32_ = haskell_selected_role_type("Pkg", "testapi", "I32");
        let string = haskell_selected_role_type("Pkg", "testapi", "String");

        assert_ne!(count, i32_);
        assert_ne!(i32_, string);
        for selected in [&count, &i32_, &string] {
            assert!(decls.contains(&format!("newtype {selected}")), "{decls}");
            assert!(equations.contains(&format!("= {selected}")), "{equations}");
        }
    }

    #[test]
    fn same_leaf_contract_types_keep_distinct_module_identity() {
        let host_types = contract_host_types(RunnerProtocol::SameLeafHostLiteralRoles, "Pkg");
        let shared: Vec<_> = host_types
            .iter()
            .filter(|host_type| host_type.source_name == "Shared")
            .collect();
        assert_eq!(shared.len(), 2);
        assert_ne!(shared[0].module_path, shared[1].module_path);
        assert_ne!(shared[0].assoc.name, shared[1].assoc.name);
    }

    #[test]
    fn nominal_host_fn_signatures_use_their_exact_type_identity() {
        let token_binding = RunnerProtocol::HostTypeRoundtrip
            .contract()
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::MakeToken { .. }))
            .expect("make-token binding");
        let HostFnBodyKind::MakeToken { token, .. } = token_binding.body else {
            unreachable!()
        };
        let token_contract = RunnerProtocol::HostTypeRoundtrip.contract();
        assert_eq!(
            haskell_method(token_binding, token_contract.host_types, "Pkg").ret_type,
            haskell_assoc_ref("Pkg", token, &[])
        );

        let box_contract = RunnerProtocol::HostGenericTypeRoundtrip.contract();
        let box_binding = box_contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::BoxMake { .. }))
            .expect("box-make binding");
        let HostFnBodyKind::BoxMake { box_type } = box_binding.body else {
            unreachable!()
        };
        assert_eq!(
            haskell_method(box_binding, box_contract.host_types, "Pkg").ret_type,
            haskell_assoc_ref("Pkg", box_type, &["t"])
        );

        let array_contract = RunnerProtocol::TestApiArray.contract();
        let array_binding = array_contract
            .host_fns
            .iter()
            .find(|binding| {
                matches!(
                    binding.body,
                    HostFnBodyKind::Array {
                        operation: "make-empty",
                        ..
                    }
                )
            })
            .expect("array-make-empty binding");
        let HostFnBodyKind::Array { array, .. } = array_binding.body else {
            unreachable!()
        };
        assert_eq!(
            haskell_method(array_binding, array_contract.host_types, "Pkg").ret_type,
            haskell_assoc_ref("Pkg", array, &["t"])
        );

        let scalar_contract = RunnerProtocol::TestApiDynLoad.contract();
        let scalar_binding = scalar_contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::MakeScalar { .. }))
            .expect("make-scalar binding");
        let HostFnBodyKind::MakeScalar { scalar, .. } = scalar_binding.body else {
            unreachable!()
        };
        assert_eq!(
            haskell_method(scalar_binding, scalar_contract.host_types, "Pkg").ret_type,
            haskell_assoc_ref("Pkg", scalar, &[])
        );
    }

    #[test]
    fn host_functor_dictionary_roundtrip_preserves_the_nominal_value() {
        let contract = RunnerProtocol::HostFunctorDictRoundtrip.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| binding.body == HostFnBodyKind::RoundFunctor)
            .expect("round-functor contract binding");
        let source = ModuleItemId::new("testapi/arith", "round_functor");

        let body = render_haskell_body_for_binding(
            binding,
            &CanonicalKind::Custom,
            &source,
            "Pkg",
            contract.host_types,
        );

        assert_eq!(body, "\\arg0 -> pure arg0");
    }

    #[test]
    fn rank_n_protocol_sequences_the_public_type_application_stage() {
        let contract = RunnerProtocol::HostRanknRoundtrip.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::ApplyPoly { .. }))
            .expect("rank-N host binding");
        let method = haskell_method(binding, contract.host_types, "Pkg");
        assert_eq!(
            method.arg_types,
            ["(forall (t :: Data.Kind.Type). m (t -> m t))"]
        );
        let source = ModuleItemId::new(binding.module, binding.leaf);
        let body = render_haskell_body_for_binding(
            binding,
            &CanonicalKind::Custom,
            &source,
            "Pkg",
            contract.host_types,
        );
        assert!(body.contains("arg0 @Data.Text.Text >>= \\__poly"), "{body}");
        assert!(body.contains("__poly (T.pack \"rank-n\\n\")"), "{body}");
    }

    #[test]
    fn loop_body_uses_the_emitted_callback_sum_patterns() {
        let source = ModuleItemId::new("testapi/iter", "loop");
        let boundary =
            BoundaryId::env_item(&source, BoundaryRoot::Arg(0)).nested(BoundaryStep::CallbackRet);
        let (arm0, arm1) = sum_arm_patterns("Pkg", &boundary);
        let body = render_loop_body("Pkg", &boundary);
        assert!(body.contains(&format!("{arm0} next")));
        assert!(body.contains(&format!("{arm1} result")));
        assert!(!body.contains("Left next"));
        assert!(!body.contains("Right result"));
    }

    #[test]
    fn array_pop_back_body_uses_the_emitted_return_sum_patterns() {
        let source = ModuleItemId::new("testapi/array", "array_pop_back");
        let boundary = BoundaryId::env_item(&source, BoundaryRoot::Ret);
        let (arm0, arm1) = sum_arm_patterns("Pkg", &boundary);
        let body = render_array_body(&ArrayOp::PopBack, "Pkg", &boundary);
        assert!(body.contains(&format!("{arm0} (last xs)")));
        assert!(body.contains(&format!("{arm1} ()")));
        assert!(!body.contains("Left (last xs)"));
        assert!(!body.contains("Right ()"));
    }
}
