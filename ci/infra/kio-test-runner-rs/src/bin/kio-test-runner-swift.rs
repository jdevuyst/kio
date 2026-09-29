//! `kio-test-runner-swift` — pointed at a `kio build swift` output
//! directory, builds the emitted Swift package as a module plus a
//! synthesized host driver that imports it, and reports the exit code.
//!
//! The package layout the emitter produces (see `specs/backends/swift.md`
//! § Output layout) is a flat directory of `pkg.swift`, `host.swift`,
//! `shapes.swift`, `ffi.swift`, `kio_runtime.swift`. The runner
//! synthesizes a `main.swift` driver and compiles in **two `swiftc`
//! steps**, the way a real host builds against a kio package: the emitted
//! files into the package module `<Ns>` (a `.swiftmodule` + a static
//! `lib<Ns>.a`), then the driver — `import <Ns>` and static-linked
//! against it — into one statically-linked binary. That shape is
//! relocatable (it depends only on the fixed toolchain stdlib runpath,
//! never a tempdir-relative rpath), so it resolves through the shared
//! content-addressed build cache ([`swift_cache`], a one-level adapter
//! over [`build_cache`]): a warm run returns the cached binary, a miss
//! runs the two `swiftc` steps into a staging tempdir. Because the driver
//! is a *separate* module importing the package, an emitter
//! `public`/`internal` slip on any symbol the host reaches fails the
//! driver compile — a real host's fidelity, which the earlier same-module
//! compile could not enforce.
//!
//! The binary is keyed by `{swiftc identity, build flags, all `.swift`
//! source bytes}` — the build flags include the `--profile`-selected
//! `-O<suffix>` (so a `-Onone` binary never aliases a `-O` one) and the
//! `<Ns>` module name. Each `swiftc` step runs with
//! `-file-prefix-map <build-dir>=/kio-build` (swift's umbrella
//! source-path remap), the analogue of go's `-trimpath` / rust's
//! `--remap-path-prefix`; with no debug info swift embeds no build path
//! at any `-O` level, so the remap is belt-and-suspenders. swiftc does
//! stamp a per-invocation random module hash into the binary, so outputs
//! are not byte-reproducible — but the cache content-addresses by
//! *inputs*, so cross-worktree reuse holds. See [`swift_cache`]'s module
//! docs and the runner README § Runner build cache and compiler wrappers
//! for the residual.
//!
//! Artifact identity is supplied independently by the corpus harness. The
//! selected protocol is the sole semantic authority: it supplies the exact
//! host types and functions, native fixtures and bodies, export driver, and
//! execution mode. The runner never reads emitted source to discover or
//! filter semantic members.
//!
//! The driver builds a `StubHost` conforming to the emitted `<Ns>Host`
//! protocol; Swift conformance is checked at compile time, so if the
//! emitted FFI drifts, the `StubHost` stops conforming and `swiftc` fails.
//!
//! Shaped slots (sum returns, the `loop` step's callback return, a
//! roundtrip's product / sum payloads) are named through the emitted
//! package's `ffi.swift` `public typealias`es (`Env_<member>_<slot>` /
//! `Exp_<member>_<slot>`), referenced **unqualified** because the driver
//! `import`s the module — a `<Ns>.`-qualified spelling would bind `<Ns>`
//! to the handle *type* and fail. A sum is a native `enum`; the protocol
//! fixtures use positional `._<k>` semantic keys, built
//! `<EnumAlias>._<k>(payload)` and matched `case ._<k>(let p)`.
//!
//! Exits 0 on success, 1 on a `swiftc` / runtime error; the CLI tier is 2
//! per `specs/exit-codes.md`. A module call to host `exit(n)` propagates
//! `n` through the spawned process's exit code (clamped to 0..=125).

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
#[path = "../swift/bin_cache/mod.rs"]
mod swift_cache;

use artifact_identity::{ArtifactIdentity, ArtifactIdentityArgs};
use canonical::{ArrayOp, CanonicalKind};
use compiler_observer::CompilerObserver;
use host_api::{
    AssocType, HostApi, TraitMethod, facade_module_selector, facade_type_selector,
    swift_export_newtype_member, swift_host_member, swift_host_type_member,
};
use opt_profile::OptProfile;
use protocol::{
    ExportDriver, HostFnBinding, HostFnBodyKind, HostRoleRef, HostTypeBinding, HostTypeFixture,
    HostTypeIdentity, ProtocolExecution, RoleFixture, RunnerProtocol, WIDE_CALLABLE_SLOT_COUNT,
};
use runner::{EXIT_RUNTIME_FAILURE, EXIT_USAGE, TestRunner};
use runner_cache_env::RunnerCacheConfig;
use swift_cache::{SwiftBuildTree, SwiftCache, swiftc_identity};

/// The effective artifact module name is also the emitted Swift handle.
/// The `coexist` protocol's two-artifact execution (`shared/protocol.rs`
/// § The coexist protocol): each package compiles into its own static
/// module (the independently supplied artifact namespace is `-module-name`),
/// one driver imports
/// both — the maximal-collision shape the namespace rule exists
/// for — and interleaves the calls through positional prefixing hosts. One
/// golden exercises the protocol, so it compiles cold with direct
/// `swiftc` invocations (the bin cache is one-module-shaped).
fn run_coexist(
    dir_a: &Path,
    identity_a: &ArtifactIdentity,
    dir_b: &Path,
    identity_b: &ArtifactIdentity,
    compiler_observer: &CompilerObserver,
    compiler_admission: &compiler_admission::CompilerAdmission,
) -> Result<i32, String> {
    let mod_a = identity_a.namespace.as_str();
    let mod_b = identity_b.namespace.as_str();
    if mod_a == mod_b {
        return Err(format!(
            "coexist requires two distinct package namespaces; both artifacts are `{mod_a}`"
        ));
    }
    let build =
        tempfile::TempDir::new().map_err(|e| format!("cannot create build tempdir: {e}"))?;
    let module_cache = build.path().join(".module-cache");
    let swiftc = |args: &[String]| -> Result<(), String> {
        let mut command = coexist_swiftc_command(compiler_observer);
        command.args(args).current_dir(build.path());
        let admitted = compiler_admission
            .acquire_for(&mut command)
            .map_err(|e| format!("acquiring compiler admission: {e}"))?;
        let out = admitted
            .output()
            .map_err(|e| format!("spawning swiftc: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "swiftc {} failed:\n{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    };
    for (module, dir) in [(mod_a, dir_a), (mod_b, dir_b)] {
        let mut args: Vec<String> = vec![
            "-Onone".into(),
            "-module-name".into(),
            module.to_string(),
            "-module-cache-path".into(),
            module_cache.display().to_string(),
            "-emit-module".into(),
            "-emit-module-path".into(),
            format!("{module}.swiftmodule"),
            "-emit-library".into(),
            "-static".into(),
            "-o".into(),
            format!("lib{module}.a"),
        ];
        let entries = fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
        let mut found = 0usize;
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_some_and(|x| x == "swift") {
                args.push(path.display().to_string());
                found += 1;
            }
        }
        if found == 0 {
            return Err(format!("no .swift files in {}", dir.display()));
        }
        swiftc(&args)?;
    }
    let mut driver = format!("import {mod_a}\nimport {mod_b}\n\n");
    for (tag, prefix, module) in [("A", "first", mod_a), ("B", "second", mod_b)] {
        driver.push_str(&format!(
            "struct Host{tag}: {module}Host {{\n    typealias greeter__I32 = Int32\n    typealias greeter__String = String\n\n    func greeter__print(_ arg0: String) {{\n        print(\"{prefix}: \" + arg0, terminator: \"\")\n    }}\n}}\n\n"
        ));
    }
    // The structural half of the witness: both packages export the same
    // `pair() -> (I32 & String)` generic product shell. Reading `._0` / `._1`
    // from both modules in one program proves their independently specialized
    // package surfaces coexist.
    let main = facade_module_selector("main", false);
    driver.push_str(&format!(
        "let pa = create{mod_a}(host: HostA())\nlet pb = create{mod_b}(host: HostB())\npa.greeter.{main}.main()\npb.greeter.{main}.main()\npa.greeter.{main}.main()\nvar qa = pa.greeter.{main}.pair()\nprint(\"first pair: \\(qa._0) \\(qa._1)\")\nlet qb = pb.greeter.{main}.pair()\nprint(\"second pair: \\(qb._0) \\(qb._1)\")\nqa = pa.greeter.{main}.pair()\nprint(\"first pair: \\(qa._0) \\(qa._1)\")\n"
    ));
    fs::write(build.path().join("main.swift"), driver)
        .map_err(|e| format!("writing driver: {e}"))?;
    let driver_args: Vec<String> = vec![
        "-Onone".into(),
        "-module-cache-path".into(),
        module_cache.display().to_string(),
        "-I".into(),
        ".".into(),
        "-L".into(),
        ".".into(),
        format!("-l{mod_a}"),
        format!("-l{mod_b}"),
        "main.swift".into(),
        "-o".into(),
        "driver".into(),
    ];
    swiftc(&driver_args)?;
    let status = Command::new(build.path().join("driver"))
        .status()
        .map_err(|e| format!("spawning coexist driver: {e}"))?;
    Ok(status.code().unwrap_or(EXIT_RUNTIME_FAILURE))
}

fn coexist_swiftc_command(compiler_observer: &CompilerObserver) -> Command {
    compiler_observer.command(std::ffi::OsStr::new("swiftc"), None)
}

const USAGE: &str = "\
Usage: kio-test-runner-swift [--protocol <name>] [--profile <name>] <output-dir>

Compile and run the Swift module emitted by `kio build swift` and report
the exit code.

Arguments:
  <output-dir>      Directory containing the emitted Swift package
                    (`pkg.swift`, `host.swift`, `shapes.swift`,
                    `ffi.swift`, `kio_runtime.swift`) per
                    `specs/backends/swift.md`.

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
                    Optional. Command prepended to the `swiftc`
                    invocation. sccache rejects swiftc, so the swift
                    runner ignores it; the build cache is the
                    acceleration layer.

  KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER
                    Optional internal debug executable placed outermost
                    around each actual `swiftc` compile. The value is one
                    opaque executable, not shell syntax.

  KIO_TEST_RUNNER_CACHE_DISABLE
                    Optional. Set to 1 to disable persistent runner
                    cache behavior and compiler wrappers; the runner
                    uses a fresh temporary cache for the invocation. The
                    debug compiler observer remains active. Empty, unset,
                    or 0 means enabled; any other value is an error.

  KIO_TEST_RUNNER_PROFILE
                    Optional. Optimization profile for the compile:
                    `unoptimized` or `default` (both -Onone; swift's -O
                    costs real compile time, so default stays cheap), or
                    `optimized` (-O). Defaults to `default`. --profile
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

    let compiler_admission = match compiler_admission::CompilerAdmission::from_env() {
        Ok(admission) => admission,
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
    let coexist = matches!(
        protocol.contract().execution,
        ProtocolExecution::Invoke(ExportDriver::Coexist)
    );
    let expected_packages = if coexist { 2 } else { 1 };
    let identities = match identity_args.resolve("swift", expected_packages) {
        Ok(identities) => identities,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    // The coexist protocol is the one two-artifact protocol: exactly two
    // positional output dirs; every other protocol takes exactly one.
    if coexist {
        return match positional.as_slice() {
            [a, b] => match run_coexist(
                Path::new(*a),
                &identities[0],
                Path::new(*b),
                &identities[1],
                &compiler_observer,
                &compiler_admission,
            ) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("error: executing coexist artifacts: {e}");
                    EXIT_RUNTIME_FAILURE
                }
            },
            _ => {
                eprintln!("error: --protocol coexist takes exactly two <output-dir> arguments");
                EXIT_USAGE
            }
        };
    }

    let dir = match positional.as_slice() {
        [d] => Path::new(*d),
        _ => {
            eprintln!("{USAGE}");
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
    let profile = match OptProfile::resolve(profile_override) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    let r = SwiftRunner {
        cache_config,
        compiler_observer,
        compiler_admission,
        protocol,
        profile,
        identity: identities.into_iter().next().unwrap(),
    };
    r.run(dir, protocol)
}

struct SwiftRunner {
    cache_config: RunnerCacheConfig,
    compiler_observer: CompilerObserver,
    compiler_admission: compiler_admission::CompilerAdmission,
    protocol: RunnerProtocol,
    identity: ArtifactIdentity,
    /// The `--profile`-selected optimization level (see [`opt_profile`]);
    /// maps to swiftc's `-Onone` / `-O` and feeds the build cache key.
    profile: OptProfile,
}

impl TestRunner for SwiftRunner {
    fn host_api(&self) -> HostApi {
        swift_host_api_for_protocol(self.protocol)
    }

    fn execute_artifact(
        &self,
        output_dir: &Path,
        host: &HostApi,
        protocol: RunnerProtocol,
    ) -> Result<i32, String> {
        // Artifact identity supplies the module name independently of the
        // emitted source.
        let module = self.identity.namespace.as_str();
        let driver = build_driver(module, host, protocol);

        // Assemble the build tree's `.swift` file set in memory: the
        // emitted `pkg.swift` / `host.swift` / `shapes.swift` /
        // `ffi.swift` / `kio_runtime.swift` plus the synthesized
        // `main.swift` driver. This same set both defines the cache key
        // and is what `produce` writes to disk before `swiftc` — so the
        // keyed and compiled bytes coincide. The emitted files compile
        // into the package module; `main.swift` imports it (§ two-step
        // build in [`swift_cache`]).
        let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        collect_swift_files(output_dir, &mut files)?;
        files.push((PathBuf::from("main.swift"), driver.into_bytes()));
        files.sort_by(|a, b| a.0.cmp(&b.0));

        let swiftc_id = swiftc_identity().map_err(|e| format!("probing swiftc identity: {e}"))?;
        let tree = SwiftBuildTree {
            swiftc_identity: swiftc_id,
            files,
            module_name: module.to_owned(),
            profile: self.profile,
        };

        // Resolve the binary through the shared cache. A warm hit skips
        // `swiftc` entirely; a miss compiles into a staging tempdir and
        // atomic-renames into place.
        //
        // `compiler_wrapper` (typically `sccache`) is deliberately
        // *not* threaded into the `swiftc` invocation: sccache wraps
        // C/C++/rustc-shaped compilers and rejects `swiftc`. Like the JS
        // / go / haskell runners, the swift runner ignores the wrapper.
        // The shared build cache is the runner's acceleration layer here.
        let disabled_cache_temp;
        let (cache_dir, max_bytes, cache_label) = match &self.cache_config {
            RunnerCacheConfig::Persistent {
                cache_dir,
                compiler_wrapper: _,
                max_bytes,
            } => (cache_dir.clone(), *max_bytes, "swift build cache"),
            RunnerCacheConfig::Disabled => {
                disabled_cache_temp = tempfile::TempDir::new()
                    .map_err(|e| format!("cannot create disabled-cache tempdir: {e}"))?;
                (
                    disabled_cache_temp.path().to_path_buf(),
                    None,
                    "disabled-cache temp swift cache",
                )
            }
        };
        let cache = SwiftCache::open(
            cache_dir.clone(),
            None,
            self.compiler_observer.clone(),
            max_bytes,
            self.compiler_admission.clone(),
        )
        .map_err(|e| format!("cannot open {cache_label} at {}: {e}", cache_dir.display()))?;

        let bin_path = cache
            .get_or_compile_bin(&tree)
            .map_err(|e| format!("building Swift bin for {}: {e}", output_dir.display()))?;

        let status = Command::new(&bin_path)
            .status()
            .map_err(|e| format!("spawning driver bin {}: {e}", bin_path.display()))?;
        Ok(status.code().unwrap_or(EXIT_RUNTIME_FAILURE))
    }
}

/// Collect the emitted Swift package's `*.swift` files into `files`, each
/// at its bare filename (the tree is flat). The runner reads them only
/// as opaque bytes to compile — never to discover the host API.
fn collect_swift_files(src: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<(), String> {
    let entries = fs::read_dir(src).map_err(|e| format!("reading {}: {e}", src.display()))?;
    let mut count = 0usize;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().is_some_and(|x| x == "swift") {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let bytes = fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
            files.push((PathBuf::from(name), bytes));
            count += 1;
        }
    }
    if count == 0 {
        return Err(format!("no .swift files in {}", src.display()));
    }
    Ok(())
}

// =========================================================================
// Host API construction (Swift-typed).
// =========================================================================

/// The protocol's fixed [`HostApi`], Swift-typed.
fn swift_host_api_for_protocol(protocol: RunnerProtocol) -> HostApi {
    let contract = protocol.contract();
    host_api::project_host_api(
        contract,
        |binding| AssocType {
            name: binding.leaf.to_owned(),
            boundary_name: (!binding.module.is_empty())
                .then(|| swift_host_type_member(binding.module, binding.leaf)),
            role: match binding.fixture {
                HostTypeFixture::Role(role) | HostTypeFixture::SelectedRole(role) => {
                    role.role().to_owned()
                }
                _ => String::new(),
            },
            type_params: (0..binding.type_arity)
                .map(|index| format!("T{index}"))
                .collect(),
        },
        |binding| swift_method(binding, contract.host_types),
    )
}

fn swift_role_type(role: RoleFixture) -> &'static str {
    match role {
        RoleFixture::I8 => "Int8",
        RoleFixture::I16 => "Int16",
        RoleFixture::I32 => "Int32",
        RoleFixture::I64 => "Int64",
        RoleFixture::I128 => "Int128",
        RoleFixture::U8 => "UInt8",
        RoleFixture::U16 => "UInt16",
        RoleFixture::U32 => "UInt32",
        RoleFixture::U64 => "UInt64",
        RoleFixture::U128 => "UInt128",
        RoleFixture::F32 => "Float",
        RoleFixture::F64 => "Double",
        RoleFixture::Bool => "Bool",
        RoleFixture::String => "String",
    }
}

fn swift_selected_role_type(binding: &HostTypeBinding) -> String {
    format!(
        "KioRunnerSelected_{}",
        swift_host_type_member(binding.module, binding.leaf)
    )
}

fn swift_exact_component(source: &str) -> String {
    source
        .split('/')
        .map(host_api::host_name_core)
        .collect::<Vec<_>>()
        .join("/")
        .replace('_', "_u")
        .replace('/', "_s")
}

fn swift_host_carrier(identity: HostTypeIdentity, args: &[&str]) -> String {
    let name = format!(
        "KioHostType_{}__{}",
        swift_exact_component(identity.module),
        swift_exact_component(identity.leaf)
    );
    if args.is_empty() {
        name
    } else {
        format!("{name}<{}>", args.join(", "))
    }
}

fn swift_role_ref_type(role: HostRoleRef, host_types: &[HostTypeBinding]) -> String {
    let binding = role.resolve(host_types);
    match binding.fixture {
        HostTypeFixture::Role(role) => swift_role_type(role).to_owned(),
        HostTypeFixture::SelectedRole(_) => swift_selected_role_type(binding),
        other => unreachable!("role reference resolved to non-role fixture {other:?}"),
    }
}

fn swift_assoc_decl_name(assoc: &AssocType) -> String {
    assoc
        .boundary_name
        .clone()
        .unwrap_or_else(|| assoc.name.clone())
}

fn swift_assoc_fixture_type(
    contract: protocol::ProtocolContract,
    assoc: &AssocType,
) -> Option<String> {
    let boundary = assoc.boundary_name.as_deref().unwrap_or(&assoc.name);
    let binding = contract.host_types.iter().find(|binding| {
        let candidate = if binding.module.is_empty() {
            binding.leaf.to_owned()
        } else {
            swift_host_type_member(binding.module, binding.leaf)
        };
        candidate == boundary
    })?;
    match binding.fixture {
        HostTypeFixture::Role(role) => Some(swift_role_type(role).to_owned()),
        HostTypeFixture::SelectedRole(_) => Some(swift_selected_role_type(binding)),
        HostTypeFixture::Token => Some("Int32".to_owned()),
        HostTypeFixture::Scalar => Some("Any".to_owned()),
        HostTypeFixture::Array | HostTypeFixture::Box => None,
    }
}

fn swift_selected_role_decl(binding: &HostTypeBinding, role: RoleFixture) -> String {
    let selected = swift_selected_role_type(binding);
    let native = swift_role_type(role);
    let (protocol, associated, label) = match role {
        RoleFixture::String => (
            "ExpressibleByStringLiteral",
            "StringLiteralType",
            "stringLiteral",
        ),
        RoleFixture::Bool => (
            "ExpressibleByBooleanLiteral, Equatable",
            "BooleanLiteralType",
            "booleanLiteral",
        ),
        RoleFixture::F32 | RoleFixture::F64 => (
            "ExpressibleByFloatLiteral",
            "FloatLiteralType",
            "floatLiteral",
        ),
        RoleFixture::I8
        | RoleFixture::I16
        | RoleFixture::I32
        | RoleFixture::I64
        | RoleFixture::I128
        | RoleFixture::U8
        | RoleFixture::U16
        | RoleFixture::U32
        | RoleFixture::U64
        | RoleFixture::U128 => (
            "ExpressibleByIntegerLiteral",
            "IntegerLiteralType",
            "integerLiteral",
        ),
    };
    format!(
        "struct {selected}: {protocol} {{\n\ttypealias {associated} = {native}\n\tlet value: {native}\n\tinit(_ value: {native}) {{ self.value = value }}\n\tinit({label} value: {native}) {{ self.value = value }}\n}}\n\n"
    )
}

fn swift_method(binding: &HostFnBinding, host_types: &[HostTypeBinding]) -> TraitMethod {
    let module = (!binding.module.is_empty()).then_some(binding.module);
    let member = match module {
        Some(module) => swift_host_member(module, binding.leaf),
        None => binding.leaf.to_owned(),
    };
    let method = |args: Vec<String>, ret: String| TraitMethod {
        name: member.clone(),
        type_params: Vec::new(),
        arg_types: args,
        ret_type: ret,
        where_clause: String::new(),
    };
    let alias = |slot: &str| env_alias(binding.module, binding.leaf, slot);
    let role = |role| swift_role_ref_type(role, host_types);
    match binding.body {
        HostFnBodyKind::CallStep { i32, .. } => method(
            vec![format!("({}) -> Int32", alias("arg0_cbarg0")), role(i32)],
            role(i32),
        ),
        HostFnBodyKind::MakePairCallback { i32, .. } => method(
            vec![format!("(Int32) -> {}", alias("arg0_cbret")), role(i32)],
            role(i32),
        ),
        HostFnBodyKind::MakeStep { i32 } => {
            method(vec![role(i32)], format!("({0}) -> {0}", role(i32)))
        }
        HostFnBodyKind::NestedCurriedRoundtrip { string } => {
            let string = role(string);
            let callback = format!("({string}) -> ({string}) -> {string}");
            method(vec![callback.clone()], callback)
        }
        HostFnBodyKind::InvokeSubstitutedUnitCallback { text } => {
            method(vec![format!("(KioUnit) -> {}", role(text))], role(text))
        }
        HostFnBodyKind::ReturnedForallUnit => method(Vec::new(), alias("ret")),
        HostFnBodyKind::ObservePacked { i32 } => method(vec![alias("arg0")], role(i32)),
        HostFnBodyKind::TraceUnit { .. } => method(Vec::new(), "()".to_owned()),
        HostFnBodyKind::StagedUnitCall => TraitMethod {
            name: member,
            type_params: vec!["KioType_0".to_owned()],
            arg_types: vec!["KioType_0".to_owned()],
            ret_type: "()".to_owned(),
            where_clause: String::new(),
        },
        HostFnBodyKind::BoxMake { box_type } => TraitMethod {
            name: member,
            type_params: vec!["t".to_owned()],
            arg_types: vec!["t".to_owned()],
            ret_type: swift_host_carrier(box_type, &["t"]),
            where_clause: String::new(),
        },
        HostFnBodyKind::BoxGet { box_type } => TraitMethod {
            name: member,
            type_params: vec!["t".to_owned()],
            arg_types: vec![swift_host_carrier(box_type, &["t"])],
            ret_type: "t".to_owned(),
            where_clause: String::new(),
        },
        HostFnBodyKind::ApplyPoly { string } => method(vec![alias("arg0")], role(string)),
        HostFnBodyKind::MakeToken { value_i32, token } => method(
            vec![role(value_i32)],
            swift_host_type_member(token.module, token.leaf),
        ),
        HostFnBodyKind::TokenValue { token, value_i32 } => method(
            vec![swift_host_type_member(token.module, token.leaf)],
            role(value_i32),
        ),
        HostFnBodyKind::RoundFunctor
        | HostFnBodyKind::RoundPicker
        | HostFnBodyKind::RoundPolyThunk
        | HostFnBodyKind::RoundPolyUnitSlot => method(vec![alias("arg0")], alias("ret")),
        HostFnBodyKind::StagedSecond { string } => {
            method(vec![role(string), role(string)], role(string))
        }
        HostFnBodyKind::MakePairStructural { i32, string } => {
            method(vec![role(i32), role(string)], alias("ret"))
        }
        HostFnBodyKind::ProducePair { .. } => method(Vec::new(), alias("ret")),
        HostFnBodyKind::SumToString { string, .. } => method(vec![alias("arg0")], role(string)),
        HostFnBodyKind::UnreachableI32Print { i32 } => method(vec![role(i32)], "()".to_owned()),
        _ => swift_canonical_method(binding, host_types),
    }
}

/// Build one canonical host fn's Swift-typed [`TraitMethod`]. Shaped
/// returns are named through the emitted package's `ffi.swift`
/// `Env_<member>_<slot>` typealiases (referenced unqualified — the driver
/// `import`s the package module).
fn swift_canonical_method(binding: &HostFnBinding, host_types: &[HostTypeBinding]) -> TraitMethod {
    let module = (!binding.module.is_empty()).then_some(binding.module);
    let member = |l: &str| match module {
        Some(m) => swift_host_member(m, l),
        None => l.to_owned(),
    };
    let ffi = |sub: &str| format!("Env_{}_{sub}<StubHost>", member(binding.leaf));
    let ffi_host_free = |sub: &str| format!("Env_{}_{sub}", member(binding.leaf));
    let m = |args: Vec<String>, ret: String| TraitMethod {
        name: member(binding.leaf),
        type_params: Vec::new(),
        arg_types: args,
        ret_type: ret,
        where_clause: String::new(),
    };
    let role = |role| swift_role_ref_type(role, host_types);
    match binding.body {
        HostFnBodyKind::Print { string } | HostFnBodyKind::Eprint { string } => {
            m(vec![role(string)], "()".to_owned())
        }
        HostFnBodyKind::PrintI32 { value } => m(vec![role(value)], "()".to_owned()),
        HostFnBodyKind::Exit { status_i32 } => m(vec![role(status_i32)], "Never".to_owned()),
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
        HostFnBodyKind::StringCodeAt { string, index } => {
            m(vec![role(string), role(index)], ffi("ret"))
        }
        HostFnBodyKind::StringToInt { string, .. } => m(vec![role(string)], ffi("ret")),
        HostFnBodyKind::ReadAsciiLine { .. } => m(Vec::new(), ffi("ret")),
        HostFnBodyKind::Arithmetic { number, .. }
        | HostFnBodyKind::FloatArithmetic { number, .. } => {
            let native = role(number);
            m(vec![native.clone(), native.clone()], native)
        }
        HostFnBodyKind::Compare { number, bool_, .. } => {
            let native = role(number);
            m(vec![native.clone(), native], role(bool_))
        }
        HostFnBodyKind::Loop => TraitMethod {
            name: member(binding.leaf),
            type_params: vec!["s".to_owned(), "r".to_owned()],
            arg_types: vec![
                format!("(s) -> {}<s, r>", ffi_host_free("arg0_cbret")),
                "s".to_owned(),
            ],
            ret_type: "r".to_owned(),
            where_clause: String::new(),
        },
        HostFnBodyKind::Array {
            operation, array, ..
        } => swift_array_method(binding.leaf, operation, array, &member, &ffi_host_free),
        HostFnBodyKind::MakeScalar { string, .. } => {
            m(vec![role(string), role(string)], "Any".to_owned())
        }
        HostFnBodyKind::ScalarOf { value, .. } => m(vec![role(value)], "Any".to_owned()),
        HostFnBodyKind::ScalarAs { scalar: _, .. } => m(vec!["Any".to_owned()], ffi("ret")),
        HostFnBodyKind::ScalarIsTrue { bool_, .. } => m(vec!["Any".to_owned()], role(bool_)),
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
                "bespoke Swift host body `{}` reached canonical signature rendering",
                binding.leaf
            )
        }
    }
}

/// One `array_*` host fn's Swift-typed method. The public method uses the
/// declaration-owned exact `Array(T)` carrier; the carrier's hidden native
/// payload is the runner's reference-semantics `KioArray<T>`.
fn swift_array_method(
    leaf: &str,
    operation: &str,
    array: HostTypeIdentity,
    member: &dyn Fn(&str) -> String,
    ffi: &dyn Fn(&str) -> String,
) -> TraitMethod {
    let m = |args: &[&str], ret: &str| TraitMethod {
        name: member(leaf),
        type_params: vec!["t".to_owned()],
        arg_types: args.iter().map(|s| (*s).to_owned()).collect(),
        ret_type: ret.to_owned(),
        where_clause: String::new(),
    };
    let arr = swift_host_carrier(array, &["t"]);
    match operation {
        "make-empty" => m(&[], &arr),
        "make-filled" => m(&["Int32", "t"], &arr),
        "len" => m(&[&arr], "Int32"),
        "get" => m(&[&arr, "Int32"], "t"),
        "set" => m(&[&arr, "Int32", "t"], "()"),
        "push" => m(&[&arr, "t"], "()"),
        "pop-back" => m(&[&arr], &format!("{}<t>", ffi("ret"))),
        "swap" => m(&[&arr, "Int32", "Int32"], "()"),
        "clear" => m(&[&arr], "()"),
        "clone" => m(&[&arr], &arr),
        other => unreachable!("unknown protocol array operation `{other}`"),
    }
}

/// The Swift `ffi.swift` alias naming an env member's shaped slot.
fn env_alias(module: &str, leaf: &str, slot: &str) -> String {
    format!("Env_{}_{slot}<StubHost>", swift_host_member(module, leaf))
}

/// One exported public-newtype member slot in `ffi.swift`.
fn export_newtype_member_alias(module: &str, newtype: &str, member: &str, slot: &str) -> String {
    format!(
        "Exp_{}_{slot}",
        swift_export_newtype_member(module, newtype, member)
    )
}

fn render_export_driver_support(contract: protocol::ProtocolContract) -> String {
    if contract.execution == ProtocolExecution::Invoke(ExportDriver::HostExistentialRoundtrip) {
        let continuation =
            export_newtype_member_alias("testapi/types", "Packed", "read_packed", "ret_cbarg0");
        let payload = env_alias("testapi/arith", "observe", "arg0");
        return format!(
            "final class KioRunnerExistentialState {{\n  var observe: (({payload}) -> Int32)?\n  var observations = 0\n  var openings = 0\n}}\nstruct KioRunnerOpenPacked: {continuation}Implementation {{\n  typealias H = StubHost\n  typealias KioOuterType_0 = Int32\n  let state: KioRunnerExistentialState\n  func call<U>(_ type: U.Type, _ payload: {continuation}_cbarg0<StubHost, U>) -> Int32 {{ state.openings += 1; return payload._1(payload._0) }}\n}}\n"
        );
    }
    if contract
        .host_fns
        .iter()
        .any(|binding| matches!(binding.body, HostFnBodyKind::ReturnedForallUnit))
    {
        return "struct KioRunnerReturnedForall: Env_testapi_main__produce_retImplementation {\n\
                \ttypealias H = StubHost\n\
                \tfunc call<KioType_0>(_ type: KioType_0.Type) -> KioType_0 {\n\
                \t\t_ = type\n\
                \t\treturn (KioUnit() as Any) as! KioType_0\n\
                \t}\n\
                }\n\n"
            .to_owned();
    }
    if contract.execution != ProtocolExecution::Invoke(ExportDriver::NewtypeVisibilityFacade) {
        return String::new();
    }
    let binding = contract
        .host_types
        .iter()
        .find(|binding| binding.fixture == HostTypeFixture::SelectedRole(RoleFixture::I32))
        .unwrap_or_else(|| {
            unreachable!("newtype visibility protocol has no selected I32 host type")
        });
    let selected = swift_selected_role_type(binding);
    let unit = export_newtype_member_alias(
        "testapi/types",
        "Existential_unit",
        "read_existential_unit",
        "ret_cbarg0",
    );
    let empty = export_newtype_member_alias(
        "testapi/types",
        "Existential_empty",
        "read_existential_empty",
        "ret_cbarg0",
    );
    let constructor = export_newtype_member_alias(
        "testapi/types",
        "Constructor_spread",
        "make_constructor_spread",
        "arg0",
    );
    let spread = export_newtype_member_alias(
        "testapi/types",
        "Existential_spread",
        "read_existential_spread",
        "ret_cbarg0",
    );
    let recursive = export_newtype_member_alias(
        "testapi/types",
        "Recursive_existential_function",
        "read_recursive_existential_function",
        "ret_cbarg0",
    );
    format!(
        "struct KioRunnerExistentialUnitContinuation: {unit}Implementation {{\n\
         \ttypealias H = StubHost\n\
         \ttypealias KioOuterType_0 = {selected}\n\
         \tfunc call<KioType_1>(_ type: KioType_1.Type, _ arg0: KioType_1) -> {selected} {{\n\
         \t\t_ = type\n\
         \t\t_ = arg0\n\
         \t\treturn {selected}(89)\n\
         \t}}\n\
         }}\n\n\
         struct KioRunnerExistentialEmptyContinuation: {empty}Implementation {{\n\
         \ttypealias H = StubHost\n\
         \ttypealias KioOuterType_0 = {selected}\n\
         \tfunc call<KioType_1>(_ type: KioType_1.Type) -> {selected} {{\n\
         \t\t_ = type\n\
         \t\treturn {selected}(90)\n\
         \t}}\n\
         }}\n\n\
         final class KioRunnerVisibilityCounts {{ var inputs: [Int32] = []; var spreadOpens = 0; var recursiveOpens = 0 }}\n\
         struct KioRunnerConstructorSpread: {constructor}Implementation {{\n\
           typealias H = StubHost\n\
           let counts: KioRunnerVisibilityCounts\n\
           func call<A>(_ type: A.Type, _ value: Product<{selected}, A>) -> {selected} {{ counts.inputs.append(value._0.value); return {selected}(value._0.value + 3) }}\n\
         }}\n\
         struct KioRunnerOpenSpread: {spread}Implementation {{\n\
           typealias H = StubHost\n\
           typealias KioOuterType_0 = {selected}\n\
           let counts: KioRunnerVisibilityCounts\n\
           func call<U>(_ type: U.Type, _ payload: {spread}_cbarg0<StubHost, {selected}, U>) -> {selected} {{ counts.spreadOpens += 1; return {selected}(91) }}\n\
         }}\n\
         struct KioRunnerOpenRecursive: {recursive}Implementation {{\n\
           typealias H = StubHost\n\
           typealias KioOuterType_0 = {selected}\n\
           let counts: KioRunnerVisibilityCounts\n\
           func call<U>(_ type: U.Type, _ payload: {recursive}_cbarg0<StubHost, U>) -> {selected} {{ counts.recursiveOpens += 1; return {selected}(92) }}\n\
         }}\n\n"
    )
}

// =========================================================================
// Driver synthesis.
// =========================================================================

/// Build the Swift `main.swift` driver: a `StubHost` conforming to the
/// emitted `<Ns>Host` protocol plus top-level code that instantiates the
/// package and runs the protocol. `module` is the harness-supplied effective
/// artifact namespace; the branded host protocol (`<Handle>Host`) and factory
/// (`create<Handle>`) use that module as their handle verbatim.
fn build_driver(module: &str, host: &HostApi, protocol: RunnerProtocol) -> String {
    let contract = protocol.contract();
    let handle = module;
    let mut out = String::new();
    out.push_str("// Generated by kio-test-runner-swift — do not edit by hand.\n");
    // The driver is a *separate* module — a real host — that `import`s the
    // emitted package `<module>`, so it references the package's `public`
    // symbols **unqualified**. A `<module>.<name>` spelling would bind
    // `<module>` to the handle *type* (which equals the module name) and
    // fail; import brings the names into scope directly.
    out.push_str("import Foundation\n");
    out.push_str(&format!("import {module}\n\n"));

    if contract.execution == ProtocolExecution::CompileOnly {
        return out;
    }

    let needs_array = contract
        .host_types
        .iter()
        .any(|binding| binding.fixture == HostTypeFixture::Array);
    if needs_array {
        out.push_str(KIO_ARRAY_DECL);
    }
    for binding in contract.host_types {
        if let HostTypeFixture::SelectedRole(role) = binding.fixture {
            out.push_str(&swift_selected_role_decl(binding, role));
        }
    }
    let needs_stdin = contract
        .host_fns
        .iter()
        .any(|binding| matches!(binding.body, HostFnBodyKind::ReadAsciiLine { .. }));
    if needs_stdin {
        out.push_str(STDIN_READER_DECL);
    }
    let needs_exit = contract
        .host_fns
        .iter()
        .any(|binding| matches!(binding.body, HostFnBodyKind::Exit { .. }));
    if needs_exit {
        out.push_str(CLAMP_EXIT_DECL);
    }

    if contract
        .host_fns
        .iter()
        .any(|binding| matches!(binding.body, HostFnBodyKind::ReturnedForallUnit))
    {
        out.push_str("var returnedForallProduced = false\nvar returnedForallFailed = false\n\n");
    }

    out.push_str(&format!("struct StubHost: {handle}Host {{\n"));
    if protocol == RunnerProtocol::HostExistentialRoundtrip {
        out.push_str("let state: KioRunnerExistentialState\n");
    }
    for assoc in host.assoc_types() {
        if let Some(native) = swift_assoc_fixture_type(contract, assoc) {
            let member = swift_assoc_decl_name(assoc);
            out.push_str(&format!("typealias {member} = {native}\n"));
        }
    }
    if !host.types.is_empty() && !host.functions.is_empty() {
        out.push('\n');
    }
    for (method, binding) in host.methods().zip(contract.host_fns) {
        let kind = canonical_kind(binding.body);
        out.push_str(&render_method(method, binding, &kind, contract.host_types));
        out.push('\n');
    }
    out.push_str("}\n\n");
    out.push_str(&render_export_driver_support(contract));

    if protocol == RunnerProtocol::HostExistentialRoundtrip {
        out.push_str(&format!("let state = KioRunnerExistentialState()\nlet pkg = create{handle}(host: StubHost(state: state))\n"));
    } else {
        out.push_str(&format!("let pkg = create{handle}(host: StubHost())\n"));
    }
    out.push_str(&render_main_body(contract));
    out.push_str("_ = pkg\n");
    out
}

/// Render the top-level code that invokes the package for `protocol`.
fn render_main_body(contract: protocol::ProtocolContract) -> String {
    let export_root = contract.testapi_conformed.then_some("testapi");
    let root_ns = match export_root {
        Some(root) => format!("pkg.{}", facade_module_selector(root, true)),
        None => "pkg".to_owned(),
    };
    match contract.execution {
        ProtocolExecution::CompileOnly | ProtocolExecution::ConstructOnly => String::new(),
        ProtocolExecution::Invoke(ExportDriver::Main { module }) => swift_main_call(module),
        ProtocolExecution::Invoke(ExportDriver::Coexist) => {
            unreachable!("--protocol coexist dispatches through run_coexist")
        }
        ProtocolExecution::Invoke(driver) => {
            let rendered = render_export_driver(driver, &root_ns, export_root.is_some())
                .unwrap_or_else(|| unreachable!("non-main Swift export driver has no renderer"));
            match driver {
                ExportDriver::NewtypeVisibilityFacade => {
                    let binding = contract
                        .host_types
                        .iter()
                        .find(|binding| {
                            binding.fixture == HostTypeFixture::SelectedRole(RoleFixture::I32)
                        })
                        .unwrap_or_else(|| {
                            unreachable!("newtype visibility protocol has no selected I32 type")
                        });
                    rendered.replace("KioRunnerSelectedI32", &swift_selected_role_type(binding))
                }
                _ => rendered,
            }
        }
    }
}

fn swift_main_call(module: &str) -> String {
    let path = module
        .split('/')
        .enumerate()
        .map(|(index, segment)| facade_module_selector(segment, index == 0))
        .collect::<Vec<_>>()
        .join(".");
    format!("pkg.{path}.main()\n")
}

/// The export-surface roundtrip drivers, in Swift. `root_ns` is the
/// exported root namespace access (`pkg.testapi` / `pkg`).
fn render_export_driver(
    driver: ExportDriver,
    root_ns: &str,
    has_export_root: bool,
) -> Option<String> {
    let module = |source| {
        format!(
            "{root_ns}.{}",
            facade_module_selector(source, !has_export_root)
        )
    };
    let api_ns = module("api");
    let main_ns = module("main");
    let utils_ns = module("utils");
    let types_ns = module("types");
    let left_ns = module("left");
    let right_ns = module("right");
    let constructor_only = facade_type_selector("Constructor_only");
    let projector_only = facade_type_selector("Projector_only");
    let both_public = facade_type_selector("Both_public");
    let shared = facade_type_selector("Shared");
    let constructor_pair = facade_type_selector("Constructor_pair");
    let projector_pair = facade_type_selector("Projector_pair");
    let constructor_generic = facade_type_selector("Constructor_generic");
    let projector_generic = facade_type_selector("Projector_generic");
    let packed_function = facade_type_selector("Packed_function");
    let existential_unit = facade_type_selector("Existential_unit");
    let existential_empty = facade_type_selector("Existential_empty");
    let recursive_both = facade_type_selector("Recursive_both");
    let recursive_constructor = facade_type_selector("Recursive_constructor");
    let recursive_projector = facade_type_selector("Recursive_projector");
    let constructor_spread = facade_type_selector("Constructor_spread");
    let projector_spread = facade_type_selector("Projector_spread");
    let existential_spread = facade_type_selector("Existential_spread");
    let recursive_existential_function = facade_type_selector("Recursive_existential_function");
    let constructor_spread_arg = export_newtype_member_alias(
        "testapi/types",
        "Constructor_spread",
        "make_constructor_spread",
        "arg0",
    );
    let existential_spread_continuation = export_newtype_member_alias(
        "testapi/types",
        "Existential_spread",
        "read_existential_spread",
        "ret_cbarg0",
    );
    let recursive_existential_continuation = export_newtype_member_alias(
        "testapi/types",
        "Recursive_existential_function",
        "read_recursive_existential_function",
        "ret_cbarg0",
    );
    let pair = facade_type_selector("Pair");
    let root = facade_type_selector("Root");
    let wrap = facade_type_selector("Wrap");
    let a = facade_type_selector("A");
    let b = facade_type_selector("B");
    let constructor_pair_arg = export_newtype_member_alias(
        "testapi/types",
        "Constructor_pair",
        "make_constructor_pair",
        "arg0",
    );
    let pair_arg = export_newtype_member_alias("testapi/types", "Pair", "mk_pair", "arg0");
    let existential_unit_continuation = export_newtype_member_alias(
        "testapi/types",
        "Existential_unit",
        "read_existential_unit",
        "ret_cbarg0",
    );
    let existential_empty_continuation = export_newtype_member_alias(
        "testapi/types",
        "Existential_empty",
        "read_existential_empty",
        "ret_cbarg0",
    );

    let body = match driver {
        ExportDriver::RustCallbackAliases => {
            "fatalError(\"rust-callback-aliases tests the Rust public naming contract only\")\n"
                .to_owned()
        }
        ExportDriver::ModuleRoundtrip => format!(
            "print({api_ns}.tag())\n\
             print({api_ns}.value())\n\
             print({api_ns}.echo(\"module-echo\"))\n"
        ),
        ExportDriver::NamespaceRoundtrip => format!(
            "print({main_ns}.answer())\n\
             print({utils_ns}.echo(\"namespace-utils\"))\n"
        ),
        ExportDriver::PolyRoundtrip => format!(
            "print({root_ns}.polyEcho(\"poly-string\") as! String)\n\
             print({root_ns}.polyEcho(Int32(42)) as! Int32)\n\
             print({root_ns}.keepLeft(\"left\", Int32(99)) as! String)\n"
        ),
        ExportDriver::CallbackRoundtrip => format!(
            "print({main_ns}.applyTwice({{ (n: Int32) -> Int32 in n + 3 }}, 10))\n\
             let step = {main_ns}.makeStep(4)\n\
             print(step(5))\n"
        ),
        ExportDriver::PositionalProductRoundtrip => {
            format!("let q = {main_ns}.makePair(7, \"hello\")\nprint(q._0, q._1)\n")
        }
        ExportDriver::MultilabelRoundtrip => format!(
            "let payload = Exp_testapi_main__say_arg0<StubHost>(A: {main_ns}.{a}.mk(42), B: {main_ns}.{b}.mk(\"shown\\n\"))\n\
             {main_ns}.say(payload)\n\
             let row = {main_ns}.echoPair(Exp_testapi_main__echoPair_arg0<StubHost>(A: {main_ns}.{a}.mk(88), B: {main_ns}.{b}.mk(\"99\")))\n\
             print({main_ns}.{a}.get(row.A))\n\
             print({main_ns}.{b}.get(row.B))\n\
             print({main_ns}.{a}.get({main_ns}.echoA({main_ns}.{a}.mk(111))))\n"
        ),
        ExportDriver::HostExistentialRoundtrip => {
            let packed = facade_type_selector("Packed");
            let continuation =
                export_newtype_member_alias("testapi/types", "Packed", "read_packed", "ret_cbarg0");
            format!(
                "state.observe = {{ value in {types_ns}.{packed}.readPacked(value).call({continuation}<StubHost, Int32>(KioRunnerOpenPacked(state: state))) }}\nlet result = {main_ns}.exercise()\nprecondition(result._0 == 37 && result._1 == 83 && state.observations == 2 && state.openings == 2)\nprint(\"existential host opening ok\")\n"
            )
        }
        ExportDriver::FunctorDictRoundtrip => {
            let box_type = format!("{types_ns}.{}", facade_type_selector("Box"));
            let functor = format!("{types_ns}.{}", facade_type_selector("Functor"));
            format!(
                r#"var integers: [Int32] = []
var texts: [String] = []
let toText: (Int32) -> String = {{ value in integers.append(value); return "v:\(value)" }}
let toInteger: (String) -> Int32 = {{ value in texts.append(value); return Int32(value.count) }}
let dict = {main_ns}.echoFunctor({main_ns}.boxFunctor())
let first = {main_ns}.applyFunctor(dict, toText, {box_type}.mkBox(Product(_0: Int32(42), _1: KioUnit())))
precondition({box_type}.unBox(first)._0 == "v:42")
let second = {main_ns}.applyFunctor(dict, toInteger, {box_type}.mkBox(Product(_0: "apple", _1: KioUnit())))
precondition({box_type}.unBox(second)._0 == 5)
let mapping = {functor}.fmap(dict)
let integerInput = pkg.KioNewtypeApplication_testapi_stypes__Box_lift({box_type}.mkBox(Product(_0: Int32(7), _1: KioUnit())))
let third = mapping.call(Product(_0: toText, _1: integerInput))
let thirdBox = pkg.KioNewtypeApplication_testapi_stypes__Box_project(third)
precondition({box_type}.unBox(thirdBox)._0 == "v:7")
let textInput = pkg.KioNewtypeApplication_testapi_stypes__Box_lift({box_type}.mkBox(Product(_0: "pear", _1: KioUnit())))
let fourth = mapping.call(Product(_0: toInteger, _1: textInput))
let fourthBox = pkg.KioNewtypeApplication_testapi_stypes__Box_project(fourth)
precondition({box_type}.unBox(fourthBox)._0 == 4)
precondition(integers == [42, 7] && texts == ["apple", "pear"])
print("functor dictionary ok")
"#
            )
        }
        ExportDriver::CallableSlotsRoundtrip => format!(
            "var productCalls = 0\n\
             let productStep: (Int32) -> Int32 = {{ value in productCalls += 1; return value + 5 }}\n\
             precondition({main_ns}.applyProduct(Exp_testapi_main__applyProduct_arg0<StubHost>(_0: productStep, _1: 11)) == 16)\n\
             let echoedProduct = {main_ns}.echoProduct(Exp_testapi_main__echoProduct_arg0<StubHost>(_0: productStep, _1: 17))\n\
             precondition(echoedProduct._1 == 17 && echoedProduct._0(echoedProduct._1) == 22)\n\
             let madeProduct = {main_ns}.makeProduct(23)\n\
             precondition(madeProduct._1 == 23 && madeProduct._0(29) == 29)\n\
             var sumCalls = 0\n\
             let sumStep: (Int32) -> Int32 = {{ value in sumCalls += 1; return value + 7 }}\n\
             let sum = Exp_testapi_main__applySum_arg0<StubHost>._0(sumStep)\n\
             precondition({main_ns}.applySum(sum, 31) == 38)\n\
             switch {main_ns}.echoSum(sum) {{ case ._0(let step): precondition(step(37) == 44); case ._1: fatalError(\"callable sum arm changed\") }}\n\
             switch {main_ns}.makeCallableSum() {{ case ._0(let step): precondition(step(41) == 41); case ._1: fatalError(\"callable sum arm changed\") }}\n\
             let scalar = {main_ns}.makeScalarSum(97)\n\
             switch scalar {{ case ._1(let value): precondition(value == 97); case ._0: fatalError(\"scalar sum arm changed\") }}\n\
             precondition({main_ns}.applySum(scalar, 43) == 97)\n\
             switch {main_ns}.echoSum(scalar) {{ case ._1(let value): precondition(value == 97); case ._0: fatalError(\"scalar sum arm changed\") }}\n\
             precondition(productCalls == 2 && sumCalls == 2)\n\
             print(\"callable slots ok\")\n"
        ),
        ExportDriver::ScalarRoundtrip => format!(
            "let signed: Int128 = -1208925819614629174706299\n\
             let unsigned: UInt128 = 2417851639229258349412391\n\
             precondition({main_ns}.echoI128(signed) == signed)\n\
             precondition({main_ns}.echoU128(unsigned) == unsigned)\n\
             for value: Float in [1.5, -2.25] {{ precondition({main_ns}.echoF32(value) == value) }}\n\
             for value: Double in [1.0000000000000002, -3.125] {{ precondition({main_ns}.echoF64(value) == value) }}\n\
             print(\"scalar payloads ok\")\n"
        ),
        ExportDriver::HostOwnedRoundtrip => {
            let integer_box =
                swift_host_carrier(HostTypeIdentity::new("testapi", "Box"), &["Int32"]);
            let string_box =
                swift_host_carrier(HostTypeIdentity::new("testapi", "Box"), &["String"]);
            format!(
                "for value: Int32 in [7, 19] {{ precondition({main_ns}.echoToken(value) == value) }}\n\
                 let integer: {integer_box} = {main_ns}.echoBox({integer_box}(Int32(42)))\n\
                 precondition(integer.value(as: Int32.self) == 42)\n\
                 let text: {string_box} = {main_ns}.echoBox({string_box}(\"box-value\"))\n\
                 precondition(text.value(as: String.self) == \"box-value\")\n\
                 print(\"host-owned payloads ok\")\n"
            )
        }
        ExportDriver::StructuralRoundtrip => format!(
            "let p = Exp_testapi_main__pairSwap_arg0<StubHost>(_0: 42, _1: \"hello\")\n\
             let q = {main_ns}.pairSwap(p)\n\
             print(q._0, q._1)\n\
             print({main_ns}.dispatchLeft(Exp_testapi_main__dispatchLeft_arg0<StubHost>._0(7)))\n\
             print({main_ns}.dispatchLeft(Exp_testapi_main__dispatchLeft_arg0<StubHost>._1(\"from-sum\")))\n\
             let wide = Exp_testapi_main__rotate_arg0<StubHost>(_0: 1, _1: 2, _2: 3, _3: 4, _4: 5, _5: 6, _6: 7, _7: 8, _8: 9, _9: 10, _10: 11, _11: 12)\n\
             let rotated = {main_ns}.rotate(wide)\n\
             print(rotated._0, rotated._1, rotated._11)\n\
             print({main_ns}.classify(Exp_testapi_main__classify_arg0<StubHost>._0(1)))\n\
             print({main_ns}.classify(Exp_testapi_main__classify_arg0<StubHost>._4(5)))\n\
             print({main_ns}.classify(Exp_testapi_main__classify_arg0<StubHost>._9(\"ten\")))\n\
             let first = {main_ns}.chooseFirst()\n\
             print({main_ns}.classify(first))\n\
             let middle = {main_ns}.chooseMiddle()\n\
             print({main_ns}.classify(middle))\n\
             let last = {main_ns}.chooseLast()\n\
             print({main_ns}.classify(last))\n\
             let samples: [Exp_testapi_main__echoSum_arg0<StubHost>] = [\n\
                 ._0(-101), ._1(-12345), ._2(-123456789), ._3(-9007199254740993),\n\
                 ._4(201), ._5(54321), ._6(3456789012), ._7(18014398509481987),\n\
                 ._8(false), ._8(true), ._9(\"sum-value\")\n\
             ]\n\
             for sample in samples {{\n\
                 let returned = {main_ns}.echoSum(sample)\n\
                 let payload: String\n\
                 switch returned {{\n\
                 case ._0(let value): payload = String(value)\n\
                 case ._1(let value): payload = String(value)\n\
                 case ._2(let value): payload = String(value)\n\
                 case ._3(let value): payload = String(value)\n\
                 case ._4(let value): payload = String(value)\n\
                 case ._5(let value): payload = String(value)\n\
                 case ._6(let value): payload = String(value)\n\
                 case ._7(let value): payload = String(value)\n\
                 case ._8(let value): payload = String(value)\n\
                 case ._9(let value): payload = value\n\
                 }}\n\
                 print({main_ns}.classify(returned), payload)\n\
             }}\n"
        ),
        ExportDriver::NewtypeSumRoundtrip => {
            format!("print({main_ns}.firstOr(0, {main_ns}.pack(7, \"hi\")))\n")
        }
        // `bump(Wrap) -> Wrap` retains `Wrap`'s exact nominal carrier. Use
        // the public constructor/projector around the exported roundtrip.
        ExportDriver::NewtypeScalarRoundtrip => {
            format!(
                "let wrapped = {main_ns}.{wrap}.mkWrap(7)\n\
                 let bumped = {main_ns}.bump(wrapped)\n\
                 print({main_ns}.{wrap}.unWrap(bumped))\n"
            )
        }
        ExportDriver::NewtypeIgnoredArgumentRoundtrip => {
            format!("print({main_ns}.toI32({main_ns}.fromI32(7)))\n")
        }
        ExportDriver::RecursiveNewtypeBoundary => format!(
            "let payload = {main_ns}.basePayload()\n\
             let root = {main_ns}.{root}.makeRoot(payload)\n\
             let kept = {main_ns}.keep(root)\n\
             let projected = {main_ns}.{root}.readRoot(kept)\n\
             print({main_ns}.acceptPayload(projected))\n"
        ),
        ExportDriver::NewtypeVisibilityFacade => format!(
            "let inputA = KioRunnerSelectedI32(11)\n\
             let a = {types_ns}.makeA(inputA)\n\
             let outA: KioRunnerSelectedI32 = {types_ns}.readA(a)\n\
             print(outA.value)\n\
             let inputB = KioRunnerSelectedI32(22)\n\
             let b = {types_ns}.makeB(inputB)\n\
             let outB: KioRunnerSelectedI32 = {types_ns}.readB(b)\n\
             print(outB.value)\n\
             let inputC = KioRunnerSelectedI32(33)\n\
             let c = {types_ns}.{constructor_only}.makeConstructorOnly(inputC)\n\
             let outC: KioRunnerSelectedI32 = {types_ns}.readConstructorOnlyValue(c)\n\
             print(outC.value)\n\
             let inputP = KioRunnerSelectedI32(44)\n\
             let p = {types_ns}.makeProjectorOnlyValue(inputP)\n\
             let outP: KioRunnerSelectedI32 = {types_ns}.{projector_only}.readProjectorOnly(p)\n\
             print(outP.value)\n\
             let both = {types_ns}.{both_public}.makeBothPublic(KioRunnerSelectedI32(55))\n\
             let outBoth: KioRunnerSelectedI32 = {types_ns}.{both_public}.readBothPublic(both)\n\
             print(outBoth.value)\n\
             let left = {left_ns}.make(KioRunnerSelectedI32(66))\n\
             let outLeft: KioRunnerSelectedI32 = {left_ns}.{shared}.readShared({left_ns}.{shared}.makeShared({left_ns}.read(left)))\n\
             print(outLeft.value)\n\
             let right = {right_ns}.make(KioRunnerSelectedI32(77))\n\
             let outRight: KioRunnerSelectedI32 = {right_ns}.{shared}.readShared({right_ns}.{shared}.makeShared({right_ns}.read(right)))\n\
             print(outRight.value)\n\
             let constructorPairPayload = {constructor_pair_arg}<StubHost>(_0: KioRunnerSelectedI32(81), _1: KioRunnerSelectedI32(82))\n\
             let constructorPair = {types_ns}.{constructor_pair}.makeConstructorPair(constructorPairPayload)\n\
             let constructorPairOut = {types_ns}.readConstructorPairValue(constructorPair)\n\
             print(constructorPairOut._0.value, constructorPairOut._1.value)\n\
             let projectorPair = {types_ns}.makeProjectorPairValue(constructorPairOut)\n\
             let projectorPairOut = {types_ns}.{projector_pair}.readProjectorPair(projectorPair)\n\
             print(projectorPairOut._0.value, projectorPairOut._1.value)\n\
             let constructorGeneric = {types_ns}.{constructor_generic}.makeConstructorGeneric(KioRunnerSelectedI32(85))\n\
             let constructorGenericOut = {types_ns}.readConstructorGenericValue(constructorGeneric)\n\
             print(constructorGenericOut.value)\n\
             let projectorGeneric = {types_ns}.makeProjectorGenericValue(KioRunnerSelectedI32(86))\n\
             let projectorGenericOut = {types_ns}.{projector_generic}.readProjectorGeneric(projectorGeneric)\n\
             print(projectorGenericOut.value)\n\
             let packed = {types_ns}.{packed_function}.makePackedFunction({{ value in KioRunnerSelectedI32(value._0.value + value._1.value) }})\n\
             let unpacked = {types_ns}.{packed_function}.readPackedFunction(packed)\n\
             print(unpacked(constructorPairOut).value)\n\
             let existential = {types_ns}.makeExistentialUnitValue()\n\
             let existentialUnitContinuation = {existential_unit_continuation}<StubHost, KioRunnerSelectedI32>(KioRunnerExistentialUnitContinuation())\n\
             let existentialOut = {types_ns}.{existential_unit}.readExistentialUnit(existential).call(existentialUnitContinuation)\n\
             print(existentialOut.value)\n\
             let existentialEmpty = {types_ns}.makeExistentialEmptyValue()\n\
             let existentialEmptyContinuation = {existential_empty_continuation}<StubHost, KioRunnerSelectedI32>(KioRunnerExistentialEmptyContinuation())\n\
             let existentialEmptyOut = {types_ns}.{existential_empty}.readExistentialEmpty(existentialEmpty).call(existentialEmptyContinuation)\n\
             print(existentialEmptyOut.value)\n\
             let recursiveBoth = {types_ns}.{recursive_both}.makeRecursiveBoth({types_ns}.recursiveBothBasePayload())\n\
             print({types_ns}.recursiveBothPayloadIsBase({types_ns}.{recursive_both}.readRecursiveBoth(recursiveBoth)).value)\n\
             let recursiveConstructor = {types_ns}.{recursive_constructor}.makeRecursiveConstructor({types_ns}.recursiveConstructorBasePayload())\n\
             print({types_ns}.recursiveConstructorPayloadIsBase({types_ns}.readRecursiveConstructorValue(recursiveConstructor)).value)\n\
             let recursiveProjector = {types_ns}.makeRecursiveProjectorBase()\n\
             print({types_ns}.recursiveProjectorPayloadIsBase({types_ns}.{recursive_projector}.readRecursiveProjector(recursiveProjector)).value)\n\
             let counts = KioRunnerVisibilityCounts()\n\
             let spread = {constructor_spread_arg}<StubHost>(KioRunnerConstructorSpread(counts: counts))\n\
             let constructed = {types_ns}.{constructor_spread}.makeConstructorSpread(spread)\n\
             precondition({types_ns}.invokeConstructorSpreadI32(constructed, KioRunnerSelectedI32(101)).value == 104)\n\
             precondition({types_ns}.invokeConstructorSpreadUnit(constructed, KioRunnerSelectedI32(102)).value == 105)\n\
             precondition(counts.inputs == [101, 102])\n\
             let projected = {types_ns}.{projector_spread}.readProjectorSpread({types_ns}.makeProjectorSpreadValue())\n\
             precondition(projected.call(Product(_0: KioRunnerSelectedI32(111), _1: KioRunnerSelectedI32(1))).value == 111)\n\
             precondition(projected.call(Product(_0: KioRunnerSelectedI32(112), _1: KioUnit())).value == 112)\n\
             let openSpread = {existential_spread_continuation}<StubHost, KioRunnerSelectedI32>(KioRunnerOpenSpread(counts: counts))\n\
             let openRecursive = {recursive_existential_continuation}<StubHost, KioRunnerSelectedI32>(KioRunnerOpenRecursive(counts: counts))\n\
             precondition({types_ns}.{existential_spread}.readExistentialSpread({types_ns}.makeExistentialSpreadValue()).call(openSpread).value == 91)\n\
             precondition({types_ns}.{recursive_existential_function}.readRecursiveExistentialFunction({types_ns}.makeRecursiveExistentialFunctionValue()).call(openRecursive).value == 92)\n\
             precondition(counts.spreadOpens == 1 && counts.recursiveOpens == 1)\n"
        ),
        // `make(String, I32, I32) -> Outer` retains exact `Outer` and `Inner`
        // carriers. Project each through its public type handle, then read the
        // positional and bare-newtype semantic fields.
        ExportDriver::CompoundInputOnce => format!(
            r#"let direct = {main_ns}.direct()
print(direct._0)
print(direct._1)
let callback = {main_ns}.callback({{
    print("callback")
    return Exp_testapi_main__callback_arg0_cbret<StubHost>(_0: 9, _1: "callback-value")
}})
print(callback._0)
print(callback._1)
let outer = {main_ns}.KioType_Outer.unOuter({main_ns}.echoOuter({main_ns}.makeOuter("nest", 11, 13)))
let inner = {main_ns}.KioType_Inner.unInner(outer.Inner)
print(outer._0)
print(inner._0)
print(inner._1)
for choice in [{main_ns}.first(17), {main_ns}.middle(19, 23), {main_ns}.last("choice", 29, 31)] {{
    switch {main_ns}.KioType_Choice.unChoice({main_ns}.echoChoice(choice)) {{
    case ._0(let value):
        print(value)
    case .Inner(let value):
        let inner = {main_ns}.KioType_Inner.unInner(value)
        print(inner._0)
        print(inner._1)
    case .Outer(let value):
        let outer = {main_ns}.KioType_Outer.unOuter(value)
        let inner = {main_ns}.KioType_Inner.unInner(outer.Inner)
        print(outer._0)
        print(inner._0)
        print(inner._1)
    }}
}}
print({main_ns}.echoText("atomic"))
"#
        ),
        ExportDriver::NestedProductRoundtrip => format!(
            "let o = {main_ns}.make(\"nest\", 7, 9)\n\
             let outer = {main_ns}.KioType_Outer.get(o)\n\
             let inner = {main_ns}.KioType_Inner.get(outer.Inner)\n\
             print(outer._0)\n\
             print(inner._0)\n\
             print(inner._1)\n"
        ),
        ExportDriver::CurriedFacade => {
            format!("print({main_ns}.pick(\"ku\", \"rz\"))\nprint({main_ns}.last(1, 2, 3))\n")
        }
        ExportDriver::NestedCurriedRoundtrip => {
            let api = module("api");
            format!(
                "let join: (String) -> (String) -> String = {{ left in {{ right in left + \"/\" + right }} }}\n\
                 let viaHost = {api}.viaHost(join)\n\
                 print(\"via host:\", viaHost(\"env-left\")(\"env-right\"))\n\
                 let roundExport = {api}.roundExport(join)\n\
                 print(\"round export:\", roundExport(\"export-left\")(\"export-right\"))\n"
            )
        }
        ExportDriver::HostSubstitutedUnitCallback => {
            let api = module("api");
            format!("print({api}.viaHost({{ (_unit: KioUnit) -> String in \"callback\" }}))\n")
        }
        ExportDriver::ReturnedForallCallByValue => format!(
            "{main_ns}.main()\n\
             {main_ns}.main()\n\
             precondition(returnedForallFailed, \"produce did not fail\")\n\
             print(\"caught\")\n"
        ),
        ExportDriver::PublicWordNames => format!(
            "let wordApi = {root_ns}.wordApi\n\
             print(wordApi.readWord())\n\
             print(wordApi.KioItem__ureadWord())\n\
             print(wordApi.KioItem_readWord_u())\n\
             print(wordApi.KioItem__ureadWord_u())\n\
             print(wordApi.KioItem_readWord_u_u())\n\
             let wordNodes = wordApi.KioModule_wordNodes\n\
             let boxed = wordNodes.KioType__uWordBox_u_u.wrapWord(55)\n\
             print(wordNodes.KioType__uWordBox_u_u.unwrapWord(boxed))\n\
             print(wordNodes.keepWord(66))\n\
             let otherNodes = wordApi.KioModule_otherNodes\n\
             let pair = wordApi.keepPair(Exp_KioItem_wordApi__keepPair_arg0<StubHost>(_WordBox__: wordNodes.KioType__uWordBox_u_u.wrapWord(Int32(77)), KioQualified_wordApi_sotherNodes___uWordBox_u_u: otherNodes.KioType__uWordBox_u_u.wrapWord(Int32(88))))\n\
             print(wordNodes.KioType__uWordBox_u_u.unwrapWord(pair._WordBox__))\n\
             print(otherNodes.KioType__uWordBox_u_u.unwrapWord(pair.KioQualified_wordApi_sotherNodes___uWordBox_u_u))\n"
        ),
        ExportDriver::FacadeSelectorCollisions => {
            let api = module("api");
            let foo_module = module("foo");
            let foo_bar = module("foo_bar");
            let i = module("i");
            let host = module("host");
            let mod_api_value = module("mod_api_value");
            let bar_module = facade_module_selector("bar", false);
            let child_module = facade_module_selector("child", false);
            let child_type = facade_type_selector("Child");
            format!(
                "{api}.pkg()\n\
                 {api}.value()\n\
                 {host}.value()\n\
                 {mod_api_value}.value()\n\
                 print({foo_module}.{bar_module}.value(9, 1))\n\
                 print({foo_bar}.value(10, 2))\n\
                 print({i}.value(41, 1))\n\
                 {api}.child()\n\
                 print(\"api.child function\")\n\
                 {api}.{child_module}.value()\n\
                 print(\"api/child module\")\n\
                 let child = {api}.{child_type}.makeChild(30)\n\
                 _ = {api}.{child_type}.readChild(child)\n\
                 print(\"api.Child type\")\n"
            )
        }
        ExportDriver::ModuleAliasScopeCollision => {
            let a = module("a");
            let b = module("b");
            format!("_ = {a}.consume({a}.make())\n_ = {b}.consume({b}.make())\n")
        }
        ExportDriver::WideCallable => {
            let arguments = (0..WIDE_CALLABLE_SLOT_COUNT)
                .map(|index| format!("Int32({index})"))
                .collect::<Vec<_>>()
                .join(", ");
            let callback_fields = (0..WIDE_CALLABLE_SLOT_COUNT)
                .map(|index| format!("_{index}: Int32({index})"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "let out = {main_ns}.select({arguments})\nprint(out._0)\nprint(out._1)\nprint(out._2)\nlet callbackArgs = Exp_testapi_main__makeSelect_ret_cbarg0<StubHost>({callback_fields})\nlet callbackOut = {main_ns}.makeSelect()(callbackArgs)\nprint(callbackOut._0)\nprint(callbackOut._1)\nprint(callbackOut._2)\n"
            )
        }
        // `apply_via[K][R](f: K -> R, x: K) -> R` remains a Swift generic
        // method. Its value arguments infer both exact type parameters.
        ExportDriver::PolyCallbackRoundtrip => format!(
            "print({main_ns}.applyVia({{ v in \"via: \" + v }}, \"apply\"))\n\
             print({main_ns}.applyVia({{ v in v + 8 }}, 7))\n"
        ),
        ExportDriver::TypeRoundtrip => format!(
            "let payload = {pair_arg}(_0: \"export-type-left\", _1: \"export-type-right\")\n\
             let boxed = {types_ns}.{pair}.mkPair(payload)\n\
             let out = {types_ns}.{pair}.unPair(boxed)\n\
             print(out._0)\n\
             print(out._1)\n"
        ),
        ExportDriver::Main { .. } | ExportDriver::Coexist => return None,
    };
    Some(body)
}

/// Render one `StubHost` method: the Swift signature + a canonical body.
fn render_method(
    m: &TraitMethod,
    binding: &HostFnBinding,
    kind: &CanonicalKind,
    host_types: &[HostTypeBinding],
) -> String {
    let sig = render_method_sig(m);
    let body = render_swift_body_for_binding(binding, kind, m, host_types);
    format!("func {sig} {{\n{body}\n}}\n")
}

fn swift_role_into_native(
    role: HostRoleRef,
    expression: &str,
    host_types: &[HostTypeBinding],
) -> String {
    match role.resolve(host_types).fixture {
        HostTypeFixture::Role(_) => expression.to_owned(),
        HostTypeFixture::SelectedRole(_) => format!("({expression}).value"),
        other => unreachable!("role reference resolved to non-role fixture {other:?}"),
    }
}

fn swift_role_from_native(
    role: HostRoleRef,
    expression: &str,
    host_types: &[HostTypeBinding],
) -> String {
    let binding = role.resolve(host_types);
    match binding.fixture {
        HostTypeFixture::Role(_) => expression.to_owned(),
        HostTypeFixture::SelectedRole(_) => {
            format!("{}({expression})", swift_selected_role_type(binding))
        }
        other => unreachable!("role reference resolved to non-role fixture {other:?}"),
    }
}

fn render_selected_role_body(
    body: HostFnBodyKind,
    host_types: &[HostTypeBinding],
) -> Option<String> {
    let selected = |role: HostRoleRef| {
        matches!(
            role.resolve(host_types).fixture,
            HostTypeFixture::SelectedRole(_)
        )
    };
    match body {
        HostFnBodyKind::Print { string } if selected(string) => Some(format!(
            "print({}, terminator: \"\")",
            swift_role_into_native(string, "arg0", host_types)
        )),
        HostFnBodyKind::Eprint { string } if selected(string) => Some(format!(
            "FileHandle.standardError.write(Data({}.utf8))",
            swift_role_into_native(string, "arg0", host_types)
        )),
        HostFnBodyKind::PrintI32 { value } if selected(value) => Some(format!(
            "print(String({}), terminator: \"\")",
            swift_role_into_native(value, "arg0", host_types)
        )),
        HostFnBodyKind::NumericToString { value, string }
            if selected(value) || selected(string) =>
        {
            let value_expr = swift_role_into_native(value, "arg0", host_types);
            let rendered = match value.fixture {
                RoleFixture::F32 => format!(
                    "let s = String(Double({value_expr}))\n\treturn {}",
                    swift_role_from_native(
                        string,
                        "s.hasSuffix(\".0\") ? String(s.dropLast(2)) : s",
                        host_types
                    )
                ),
                RoleFixture::F64 => format!(
                    "let s = String({value_expr})\n\treturn {}",
                    swift_role_from_native(
                        string,
                        "s.hasSuffix(\".0\") ? String(s.dropLast(2)) : s",
                        host_types
                    )
                ),
                _ => format!(
                    "return {}",
                    swift_role_from_native(string, &format!("String({value_expr})"), host_types)
                ),
            };
            Some(rendered)
        }
        HostFnBodyKind::Arithmetic { operation, number } if selected(number) => {
            let lhs = swift_role_into_native(number, "arg0", host_types);
            let rhs = swift_role_into_native(number, "arg1", host_types);
            let operator = match operation {
                "add" => "&+",
                "sub" => "&-",
                "mul" => "&*",
                "div" => "/",
                "mod" => "%",
                other => unreachable!("unknown integer arithmetic operation `{other}`"),
            };
            Some(format!(
                "return {}",
                swift_role_from_native(number, &format!("{lhs} {operator} {rhs}"), host_types)
            ))
        }
        HostFnBodyKind::MakeToken { value_i32, .. } if selected(value_i32) => Some(format!(
            "return {}",
            swift_role_into_native(value_i32, "arg0", host_types)
        )),
        HostFnBodyKind::TokenValue { value_i32, .. } if selected(value_i32) => Some(format!(
            "return {}",
            swift_role_from_native(value_i32, "arg0", host_types)
        )),
        _ => None,
    }
}

/// Render the Swift body for `m`, honoring the binding's bespoke body
/// before the canonical-kind dispatch.
fn render_swift_body_for_binding(
    binding: &HostFnBinding,
    kind: &CanonicalKind,
    m: &TraitMethod,
    host_types: &[HostTypeBinding],
) -> String {
    if let Some(body) = render_selected_role_body(binding.body, host_types) {
        return body;
    }
    match binding.body {
        HostFnBodyKind::CallStep { .. } => {
            let payload = swift_param_type_name(&m.arg_types[0]).unwrap_or_default();
            format!("return arg0({payload}(_0: arg1, _1: \"compound-callback\", _2: true))")
        }
        HostFnBodyKind::MakePairCallback { .. } => "return arg0(arg1)._0".to_owned(),
        HostFnBodyKind::MakeStep { .. } => {
            "return { (n: Int32) -> Int32 in n + arg0 }".to_owned()
        }
        HostFnBodyKind::BoxMake { .. } => format!("return {}(arg0)", m.ret_type),
        HostFnBodyKind::BoxGet { .. } => format!("return arg0.value(as: {}.self)", m.ret_type),
        HostFnBodyKind::MakeToken { .. } | HostFnBodyKind::TokenValue { .. } => {
            "return arg0".to_owned()
        }
        // A polymorphic-function newtype crosses as its exact nominal carrier;
        // hand that exact value back.
        HostFnBodyKind::RoundFunctor
        | HostFnBodyKind::RoundPicker
        | HostFnBodyKind::RoundPolyThunk
        | HostFnBodyKind::RoundPolyUnitSlot => "return arg0".to_owned(),
        HostFnBodyKind::ObservePacked { .. } => "state.observations += 1; return state.observe!(arg0)".to_owned(),
        HostFnBodyKind::StagedSecond { .. } => "return arg1".to_owned(),
        HostFnBodyKind::NestedCurriedRoundtrip { .. } => {
            "print(\"round host probe:\", arg0(\"host-left\")(\"host-right\"))\nreturn arg0"
                .to_owned()
        }
        HostFnBodyKind::InvokeSubstitutedUnitCallback { .. } => {
            "return \"host/\" + arg0(KioUnit())".to_owned()
        }
        HostFnBodyKind::ReturnedForallUnit => {
            format!(
                "if returnedForallProduced {{\n    print(\"throw\")\n    returnedForallFailed = true\n}} else {{\n    returnedForallProduced = true\n    print(\"produce\")\n}}\nreturn {}(KioRunnerReturnedForall())",
                m.ret_type
            )
        }
        HostFnBodyKind::TraceUnit { text } => {
            format!("if !returnedForallFailed {{ print({text:?}) }}")
        }
        HostFnBodyKind::StagedUnitCall => {
            "print(\"staged Unit host call\")".to_owned()
        }
        HostFnBodyKind::ApplyPoly { .. } => {
            "return arg0.call(\"rank-n\\n\")".to_owned()
        }
        HostFnBodyKind::MakePairStructural { .. } => {
            let ret = swift_param_type_name(&m.ret_type).unwrap_or_default();
            format!("return {ret}(_0: arg0, _1: arg1)")
        }
        HostFnBodyKind::ProducePair { .. } => {
            let ret = swift_param_type_name(&m.ret_type).unwrap_or_default();
            format!("print(\"direct\")\nreturn {ret}(_0: 7, _1: \"direct-value\")")
        }
        HostFnBodyKind::SumToString { .. } => {
            "switch arg0 {\n\tcase ._0(let n):\n\t\treturn String(n)\n\tcase ._1(let s):\n\t\treturn s\n\t}".to_owned()
        }
        HostFnBodyKind::UnreachableI32Print { .. } => {
            "fatalError(\"unreachable host fixture\")".to_owned()
        }
        _ => render_swift_body(kind, m),
    }
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

/// The emitted Swift type a host method's shaped slot resolves to, pulled
/// from the slot's rendered type (`Env_…` / a `(…) -> …` closure
/// whose leg is the alias).
fn swift_param_type_name(ty: &str) -> Option<String> {
    let start = ty.find("Env_")?;
    let rest = &ty[start..];
    let base_end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .unwrap_or(rest.len());
    let end = if rest[base_end..].starts_with("<StubHost>") {
        base_end + "<StubHost>".len()
    } else {
        base_end
    };
    Some(rest[..end].to_owned())
}

/// Render the exact Swift method signature for `m`. Params are `_ arg0: T`,
/// method binders remain generic, and a unit return (`()`) has no return
/// clause.
fn render_method_sig(m: &TraitMethod) -> String {
    let mut params = Vec::with_capacity(m.arg_types.len());
    for (i, ty) in m.arg_types.iter().enumerate() {
        let rendered = ty.to_owned();
        // A function-typed Swift parameter must be `@escaping` when it can
        // outlive the call. Generated top-level closure types start with `(`
        // and contain ` -> `; a named Product can contain that arrow only in
        // a nested generic argument and must not receive the parameter-only
        // attribute.
        let rendered = if rendered.starts_with('(') && rendered.contains(" -> ") {
            format!("@escaping {rendered}")
        } else {
            rendered
        };
        params.push(format!("_ arg{i}: {rendered}"));
    }
    let ret = &m.ret_type;
    let ret_clause = if ret == "()" {
        String::new()
    } else {
        format!(" -> {ret}")
    };
    let generic = if m.type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", m.type_params.join(", "))
    };
    format!("{}{generic}({}){ret_clause}", m.name, params.join(", "))
}

/// Render a Swift body for one canonical kind. Sum-shaped returns are
/// built `<EnumAlias>._<k>(payload)` through the emitted ffi alias.
fn render_swift_body(kind: &CanonicalKind, m: &TraitMethod) -> String {
    match kind {
        CanonicalKind::Print => "print(arg0, terminator: \"\")".to_owned(),
        CanonicalKind::PrintI32 => "print(String(arg0), terminator: \"\")".to_owned(),
        CanonicalKind::Eprint => "FileHandle.standardError.write(Data(arg0.utf8))".to_owned(),
        CanonicalKind::Exit => "exit(Int32(clampExit(Int64(arg0))))".to_owned(),
        CanonicalKind::ReadAsciiLine => render_read_ascii_line(m),
        CanonicalKind::StringLen => "return Int32(Array(arg0.utf8).count)".to_owned(),
        CanonicalKind::StringSlice => render_string_slice(),
        CanonicalKind::StringCodeAt => render_string_code_at(m),
        CanonicalKind::StringConcat => "return arg0 + arg1".to_owned(),
        CanonicalKind::StringEq => "return arg0 == arg1".to_owned(),
        CanonicalKind::StringToInt => render_string_to_int(m),
        CanonicalKind::BoolToString => "return arg0 ? \"true\" : \"false\"".to_owned(),
        CanonicalKind::NumericToString { kind } => numeric_to_string_body(kind),
        CanonicalKind::Arith { op, .. } => arith_body(op, true),
        CanonicalKind::FloatArith { op, .. } => arith_body(op, false),
        CanonicalKind::Cmp { cmp, .. } => cmp_body(cmp),
        CanonicalKind::Loop => render_loop_body(m),
        CanonicalKind::Array(op) => render_array_body(op, m),
        CanonicalKind::Custom => format!(
            "fatalError(\"kio-test-runner-swift: no canonical impl for host fn `{}`\")",
            m.name
        ),
        CanonicalKind::MakeScalar => render_make_scalar_body(),
        CanonicalKind::ScalarOf { .. } => "return arg0".to_owned(),
        CanonicalKind::ScalarAs { kind } => render_scalar_as_body(kind, m),
        CanonicalKind::ScalarIsTrue => {
            "if let b = arg0 as? Bool {\n\t\treturn b\n\t}\n\treturn false".to_owned()
        }
    }
}

/// The sum enum-alias for a sum-typed ffi alias. A sum
/// is a native `enum`; build arm `k` as `<alias>._<k>(payload)`.
fn sum_alias(alias: &str) -> &str {
    alias
}

fn render_make_scalar_body() -> String {
    "switch arg1 {\n\
     \tcase \"I32\", \"Int\":\n\
     \t\tguard let n = Int32(arg0) else { fatalError(\"make_scalar: bad i32\") }\n\
     \t\treturn n\n\
     \tcase \"F64\", \"F32\":\n\
     \t\tguard let f = Double(arg0) else { fatalError(\"make_scalar: bad f64\") }\n\
     \t\treturn f\n\
     \tcase \"String\", \"Str\":\n\
     \t\treturn arg0\n\
     \tcase \"Bool\":\n\
     \t\treturn arg0 == \"t\"\n\
     \tdefault:\n\
     \t\tfatalError(\"make_scalar: unknown representation key \" + arg1)\n\
     \t}"
    .to_owned()
}

fn render_scalar_as_body(kind: &str, m: &TraitMethod) -> String {
    let base = sum_alias(&m.ret_type);
    let swift_ty = match kind {
        "i32" => "Int32",
        "f64" => "Double",
        "str" => "String",
        "bool" => "Bool",
        other => return format!("fatalError(\"scalar_as_{other}: unsupported kind\")"),
    };
    // `. | <Kind>`: arm `_0` is the absent unit value, `_1` the present value.
    format!(
        "if let v = arg0 as? {swift_ty} {{\n\t\treturn {base}._1(v)\n\t}}\n\treturn {base}._0(KioUnit())"
    )
}

fn arith_body(op: &str, wrapping: bool) -> String {
    // Kio integer arithmetic wraps on overflow; Swift's `+`/`-`/`*` trap.
    // Use the wrapping operators for fixed-width integers, the plain ones
    // for floats (which have no `&`-prefixed form).
    let (add, sub, mul) = if wrapping {
        ("&+", "&-", "&*")
    } else {
        ("+", "-", "*")
    };
    match op {
        "add" => format!("return arg0 {add} arg1"),
        "sub" => format!("return arg0 {sub} arg1"),
        "mul" => format!("return arg0 {mul} arg1"),
        "div" => "return arg0 / arg1".to_owned(),
        "mod" => "return arg0 % arg1".to_owned(),
        _ => "return arg0".to_owned(),
    }
}

fn cmp_body(cmp: &str) -> String {
    let s = match cmp {
        "eq" => "==",
        "lt" => "<",
        "leq" | "le" => "<=",
        "gt" => ">",
        "geq" | "ge" => ">=",
        _ => "==",
    };
    format!("return arg0 {s} arg1")
}

fn numeric_to_string_body(kind: &str) -> String {
    // Swift's `String(_:)` for a floating value is the shortest round-trip
    // repr but appends `.0` to whole numbers; kio's `'g'`-style output does
    // not, so strip a trailing `.0` on the float arms.
    match kind {
        "f32" => {
            "let s = String(Double(arg0))\n\treturn s.hasSuffix(\".0\") ? String(s.dropLast(2)) : s"
                .to_owned()
        }
        "f64" => "let s = String(arg0)\n\treturn s.hasSuffix(\".0\") ? String(s.dropLast(2)) : s"
            .to_owned(),
        _ => "return String(arg0)".to_owned(),
    }
}

fn render_string_slice() -> String {
    "let b = Array(arg0.utf8)\n\tif arg1 < 0 || arg1 > arg2 || Int(arg2) > b.count {\n\t\tFileHandle.standardError.write(Data(\"string_slice: invalid range\\n\".utf8))\n\t\texit(1)\n\t}\n\treturn String(decoding: b[Int(arg1)..<Int(arg2)], as: UTF8.self)".to_owned()
}

fn render_string_code_at(m: &TraitMethod) -> String {
    let base = sum_alias(&m.ret_type);
    format!(
        "let b = Array(arg0.utf8)\n\tif arg1 < 0 || Int(arg1) >= b.count {{\n\t\treturn {base}._1(KioUnit())\n\t}}\n\treturn {base}._0(Int32(b[Int(arg1)]))"
    )
}

fn render_string_to_int(m: &TraitMethod) -> String {
    let base = sum_alias(&m.ret_type);
    format!(
        "guard let n = Int32(arg0) else {{\n\t\treturn {base}._1(KioUnit())\n\t}}\n\treturn {base}._0(n)"
    )
}

fn render_read_ascii_line(m: &TraitMethod) -> String {
    let base = sum_alias(&m.ret_type);
    format!(
        "guard let line = kioReadLine() else {{\n\t\treturn {base}._1(KioUnit())\n\t}}\n\treturn {base}._0(line)"
    )
}

/// `loop`: drive the step callback until it returns the exit arm. The
/// step's `s | r` return is the `arg0_cbret` sum; arm `_0` continues, `_1`
/// exits.
fn render_loop_body(_m: &TraitMethod) -> String {
    "var state = arg1\n\twhile true {\n\t\tswitch arg0(state) {\n\t\tcase ._0(let next):\n\t\t\tstate = next\n\t\tcase ._1(let result):\n\t\t\treturn result\n\t\t}\n\t}"
        .to_owned()
}

fn render_array_body(op: &ArrayOp, m: &TraitMethod) -> String {
    let element = m
        .type_params
        .first()
        .expect("an Array host method has one exact element binder");
    let native = format!("KioArray<{element}>");
    let recv = format!("let a: {native} = arg0.value()\n\t");
    let base = sum_alias(&m.ret_type);
    match op {
        ArrayOp::MakeEmpty => format!("return {}({native}())", m.ret_type),
        ArrayOp::MakeFilled => format!(
            "if arg0 < 0 {{ fatalError(\"array_make_filled: negative size\") }}\n\tlet a = {native}()\n\tvar i: Int32 = 0\n\twhile i < arg0 {{ a.v.append(arg1); i += 1 }}\n\treturn {}(a)",
            m.ret_type
        ),
        ArrayOp::Len => format!("{recv}return Int32(a.v.count)"),
        ArrayOp::Get => format!(
            "{recv}if arg1 < 0 || Int(arg1) >= a.v.count {{ fatalError(\"array_get: out of bounds\") }}\n\treturn a.v[Int(arg1)]"
        ),
        ArrayOp::Set => format!(
            "{recv}if arg1 < 0 || Int(arg1) >= a.v.count {{ fatalError(\"array_set: out of bounds\") }}\n\ta.v[Int(arg1)] = arg2"
        ),
        ArrayOp::Push => format!("{recv}a.v.append(arg1)"),
        ArrayOp::PopBack => format!(
            "{recv}if a.v.isEmpty {{ return {base}._1(KioUnit()) }}\n\tlet last = a.v.removeLast()\n\treturn {base}._0(last)"
        ),
        ArrayOp::Swap => format!(
            "{recv}let n = a.v.count\n\tif arg1 < 0 || Int(arg1) >= n || arg2 < 0 || Int(arg2) >= n {{ fatalError(\"array_swap: out of bounds\") }}\n\ta.v.swapAt(Int(arg1), Int(arg2))"
        ),
        ArrayOp::Clear => format!("{recv}a.v.removeAll()"),
        ArrayOp::Clone => format!(
            "{recv}let c = {native}()\n\tc.v = a.v\n\treturn {}(c)",
            m.ret_type
        ),
    }
}

/// The runner's backing for the canonical `host type Array[T];`: a
/// reference cell (reference semantics matching JS arrays).
const KIO_ARRAY_DECL: &str = "final class KioArray<T> { var v: [T] = [] }\n\n";

/// Stdin line reader backing `read_ascii_line` (ASCII-validated).
const STDIN_READER_DECL: &str = "func kioReadLine() -> String? {\n\tguard let line = readLine(strippingNewline: true) else { return nil }\n\tfor b in line.utf8 {\n\t\tif b > 127 {\n\t\t\tFileHandle.standardError.write(Data(\"read_ascii_line: non-ASCII input\\n\".utf8))\n\t\t\texit(1)\n\t\t}\n\t}\n\treturn line\n}\n\n";

/// Clamp an exit code into `0..=125` per `specs/exit-codes.md`.
const CLAMP_EXIT_DECL: &str = "func clampExit(_ n: Int64) -> Int64 {\n\tif n < 0 { return 0 }\n\tif n > 125 { return 125 }\n\treturn n\n}\n\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coexist_swiftc_uses_the_shared_outer_observer_shape() {
        let observer = CompilerObserver::for_test("observe");
        let command = coexist_swiftc_command(&observer);
        assert_eq!(command.get_program(), "observe");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["swiftc"]);
    }

    #[test]
    fn method_signature_only_marks_top_level_closures_escaping() {
        let method = TraitMethod {
            name: "accept".to_owned(),
            type_params: Vec::new(),
            arg_types: vec![
                "(Swift.Int) -> Swift.Int".to_owned(),
                "Product<(Swift.Int) -> Swift.Int, Swift.Int>".to_owned(),
            ],
            ret_type: "()".to_owned(),
            where_clause: String::new(),
        };

        assert_eq!(
            render_method_sig(&method),
            "accept(_ arg0: @escaping (Swift.Int) -> Swift.Int, _ arg1: Product<(Swift.Int) -> Swift.Int, Swift.Int>)"
        );
    }

    #[test]
    fn main_call_uses_the_exact_declaring_module() {
        assert_eq!(swift_main_call("main"), "pkg.main.main()\n");
        assert_eq!(
            swift_main_call("testapi/main"),
            "pkg.testapi.KioModule_main.main()\n"
        );
        assert_eq!(swift_main_call("prog"), "pkg.prog.main()\n");
        assert_eq!(swift_main_call("api"), "pkg.api.main()\n");
    }

    #[test]
    fn artifact_module_is_the_verbatim_swift_handle() {
        let host = swift_host_api_for_protocol(RunnerProtocol::Empty);
        for module in ["Demo", "FooBar", "KioPkg__uFooBar"] {
            let driver = build_driver(module, &host, RunnerProtocol::Empty);
            assert!(
                driver.contains(&format!("struct StubHost: {module}Host")),
                "{driver}"
            );
            assert!(
                driver.contains(&format!("create{module}(host: StubHost())")),
                "{driver}"
            );
        }
    }

    #[test]
    fn export_driver_uses_role_framed_facade_selectors() {
        let body = render_export_driver(ExportDriver::NewtypeVisibilityFacade, "pkg.testapi", true)
            .expect("driver");
        assert!(body.contains("pkg.testapi.KioModule_types.makeA"));
        assert!(
            body.contains(
                "pkg.testapi.KioModule_types.KioType_ConstructorOnly.makeConstructorOnly"
            )
        );

        let body = render_export_driver(ExportDriver::MultilabelRoundtrip, "pkg.testapi", true)
            .expect("driver");
        assert!(body.contains("pkg.testapi.KioModule_main.KioType_A.get"));
        assert!(!body.contains("pkg.testapi.KioModule_main.A.get"));
    }

    #[test]
    fn public_word_names_driver_pins_affixes_and_nested_selectors() {
        let body =
            render_export_driver(ExportDriver::PublicWordNames, "pkg", false).expect("driver");
        for literal in [
            "wordApi.readWord()",
            "wordApi.KioItem__ureadWord()",
            "wordApi.KioItem_readWord_u()",
            "wordApi.KioItem__ureadWord_u()",
            "wordApi.KioItem_readWord_u_u()",
            "wordNodes.KioType__uWordBox_u_u.wrapWord(55)",
            "wordNodes.KioType__uWordBox_u_u.unwrapWord(boxed)",
            "wordNodes.keepWord(66)",
            "wordApi.keepPair(Exp_KioItem_wordApi__keepPair_arg0<StubHost>(",
            "unwrapWord(pair._WordBox__)",
            "unwrapWord(pair.KioQualified_wordApi_sotherNodes___uWordBox_u_u)",
        ] {
            assert!(body.contains(literal), "{body}");
        }
    }

    #[test]
    fn multilabel_driver_constructs_and_projects_nominal_fields() {
        let body = render_export_driver(ExportDriver::MultilabelRoundtrip, "pkg.testapi", true)
            .expect("multilabel driver");

        assert!(
            body.contains(
                "A: pkg.testapi.KioModule_main.KioType_A.mk(42), B: pkg.testapi.KioModule_main.KioType_B.mk(\"shown\\n\")"
            ),
            "{body}"
        );
        assert!(
            body.contains(
                "A: pkg.testapi.KioModule_main.KioType_A.mk(88), B: pkg.testapi.KioModule_main.KioType_B.mk(\"99\")"
            ),
            "{body}"
        );
        assert!(body.contains("KioType_A.get(row.A)"), "{body}");
        assert!(body.contains("KioType_B.get(row.B)"), "{body}");
        assert!(!body.contains("A: 42"), "{body}");
        assert!(!body.contains("B: \"shown\\n\""), "{body}");
        assert!(!body.contains("print(row.A)"), "{body}");
        assert!(!body.contains("print(row.B)"), "{body}");
    }

    #[test]
    fn wide_callable_driver_preserves_the_grouped_product_parameter() {
        let body = render_export_driver(ExportDriver::WideCallable, "pkg.testapi", true)
            .expect("wide callable driver");

        assert!(
            body.contains("let callbackArgs = Exp_testapi_main__makeSelect_ret_cbarg0<StubHost>("),
            "{body}"
        );
        assert!(
            body.contains("pkg.testapi.KioModule_main.makeSelect()(callbackArgs)"),
            "{body}"
        );
        assert_eq!(body.matches("Int32(").count(), WIDE_CALLABLE_SLOT_COUNT * 2);
        assert!(!body.contains("makeSelect()(Int32("), "{body}");
    }

    #[test]
    fn rejects_removed_main_entry_option() {
        assert_eq!(run(&["--main-entry=function".to_owned()]), EXIT_USAGE);
    }

    #[test]
    fn role_assoc_inventory_comes_from_the_exact_protocol_contract() {
        let print_host = swift_host_api_for_protocol(RunnerProtocol::TestApiPrintLogicBool);
        assert!(print_host.assoc_types().any(|item| {
            item.name == "Later_bool"
                && item.role == "bool"
                && item.boundary_name.as_deref() == Some("logic__LaterBool")
        }));
        let driver = build_driver("Demo", &print_host, RunnerProtocol::TestApiPrintLogicBool);
        assert!(driver.contains("typealias logic__LaterBool = Bool"));
    }

    #[test]
    fn selected_role_fixture_drives_the_distinct_i32_host_type() {
        let protocol = RunnerProtocol::NewtypeVisibilityFacade;
        let contract = protocol.contract();
        assert_eq!(
            contract.host_types[0].fixture,
            HostTypeFixture::SelectedRole(RoleFixture::I32)
        );

        let api = swift_host_api_for_protocol(protocol);
        let assoc = api
            .assoc_types()
            .next()
            .expect("selected I32 contract binding");
        let binding = &contract.host_types[0];
        let selected = swift_selected_role_type(binding);
        assert_eq!(
            swift_assoc_fixture_type(contract, assoc),
            Some(selected.clone())
        );

        let driver = build_driver("Demo", &api, protocol);
        assert!(driver.contains(&swift_selected_role_decl(binding, RoleFixture::I32)));
        assert!(driver.contains(&format!(" = {selected}")));

        let selected = HostRoleRef::new("testapi", "I32", RoleFixture::I32);
        let ordinary = HostRoleRef::new("testapi", "Int", RoleFixture::I32);
        let exact_types = [
            HostTypeBinding::selected_role("testapi", "I32", RoleFixture::I32),
            HostTypeBinding::role("testapi", "Int", RoleFixture::I32),
        ];
        assert_eq!(
            swift_role_ref_type(selected, &exact_types),
            swift_selected_role_type(&exact_types[0])
        );
        assert_eq!(
            swift_role_ref_type(ordinary, &exact_types),
            "Int32".to_owned()
        );
        let binding = HostFnBinding {
            module: "testapi/io",
            leaf: "print_i32",
            body: HostFnBodyKind::PrintI32 { value: selected },
        };
        assert_eq!(
            swift_method(&binding, &exact_types).arg_types,
            [swift_selected_role_type(&exact_types[0])]
        );
    }

    #[test]
    fn selected_role_markers_preserve_each_exact_host_type_identity() {
        let bindings = RunnerProtocol::HostTypeRoundtrip.contract().host_types;
        let selected = bindings
            .iter()
            .filter(|binding| matches!(binding.fixture, HostTypeFixture::SelectedRole(_)))
            .map(swift_selected_role_type)
            .collect::<Vec<_>>();

        assert_eq!(selected.len(), 3);
        assert_ne!(selected[0], selected[1]);
        assert_ne!(selected[1], selected[2]);
        let api = swift_host_api_for_protocol(RunnerProtocol::HostTypeRoundtrip);
        let driver = build_driver("Demo", &api, RunnerProtocol::HostTypeRoundtrip);
        for marker in selected {
            assert!(driver.contains(&format!("struct {marker}")), "{driver}");
            assert!(driver.contains(&format!(" = {marker}")), "{driver}");
        }
    }

    #[test]
    fn empty_protocol_uses_only_its_declared_contract_inventory() {
        let api = swift_host_api_for_protocol(RunnerProtocol::Empty);
        assert_eq!(
            api.types.len(),
            RunnerProtocol::Empty.contract().host_types.len()
        );
        assert_eq!(
            api.functions.len(),
            RunnerProtocol::Empty.contract().host_fns.len()
        );
        let driver = build_driver("Demo", &api, RunnerProtocol::Empty);
        assert!(!driver.contains("__KioRunnerEmptyHostType"));
    }

    #[test]
    fn host_type_roundtrip_binds_exact_token_member() {
        let api = swift_host_api_for_protocol(RunnerProtocol::HostTypeRoundtrip);
        let driver = build_driver("Demo", &api, RunnerProtocol::HostTypeRoundtrip);
        assert!(driver.contains("typealias testapi__Token = Int32"));
        let make_token = api
            .methods()
            .find(|method| method.name == "testapi_opaque__makeToken")
            .expect("make_token fixture");
        assert_eq!(make_token.arg_types[0], "KioRunnerSelected_testapi__I32");
        assert_eq!(make_token.ret_type, "testapi__Token");
        let token_value = api
            .methods()
            .find(|method| method.name == "testapi_opaque__tokenValue")
            .expect("token_value fixture");
        assert_eq!(token_value.arg_types[0], "testapi__Token");
        assert_eq!(token_value.ret_type, "KioRunnerSelected_testapi__I32");
    }

    #[test]
    fn existential_newtype_driver_uses_exact_callable_carriers() {
        let protocol = RunnerProtocol::NewtypeVisibilityFacade;
        let api = swift_host_api_for_protocol(protocol);
        let driver = build_driver("Demo", &api, protocol);
        let unit = export_newtype_member_alias(
            "testapi/types",
            "Existential_unit",
            "read_existential_unit",
            "ret_cbarg0",
        );
        let empty = export_newtype_member_alias(
            "testapi/types",
            "Existential_empty",
            "read_existential_empty",
            "ret_cbarg0",
        );

        assert!(
            driver.contains(&format!(
                "struct KioRunnerExistentialUnitContinuation: {unit}Implementation"
            )),
            "{driver}"
        );
        assert!(
            driver.contains(&format!(
                "struct KioRunnerExistentialEmptyContinuation: {empty}Implementation"
            )),
            "{driver}"
        );
        assert!(driver.contains("readExistentialUnit(existential).call("));
        assert!(driver.contains("readExistentialEmpty(existentialEmpty).call("));
        assert!(!driver.contains("readExistentialUnit(existential, "));
    }

    #[test]
    fn returned_forall_host_uses_the_exact_typed_carrier_wrapper() {
        let protocol = RunnerProtocol::ReturnedForallCallByValue;
        let contract = protocol.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::ReturnedForallUnit))
            .expect("returned-forall binding");
        let method = swift_method(binding, contract.host_types);
        assert_eq!(method.ret_type, "Env_testapi_main__produce_ret<StubHost>");

        let api = swift_host_api_for_protocol(protocol);
        let driver = build_driver("Demo", &api, protocol);
        assert!(
            driver.contains(
                "struct KioRunnerReturnedForall: Env_testapi_main__produce_retImplementation"
            ),
            "{driver}"
        );
        assert!(
            driver.contains("func call<KioType_0>(_ type: KioType_0.Type) -> KioType_0"),
            "{driver}"
        );
        assert!(
            driver.contains(
                "return Env_testapi_main__produce_ret<StubHost>(KioRunnerReturnedForall())"
            ),
            "{driver}"
        );
    }

    #[test]
    fn interleaved_stage_host_method_omits_phantom_generic_slots() {
        let protocol = RunnerProtocol::HostInterleavedStageRoundtrip;
        let contract = protocol.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::StagedSecond { .. }))
            .expect("interleaved-stage host binding");
        let method = swift_method(binding, contract.host_types);

        assert!(method.type_params.is_empty());
        assert_eq!(method.arg_types, ["String", "String"]);
        assert_eq!(method.ret_type, "String");
        assert_eq!(
            render_method_sig(&method),
            "testapi_arith__staged(_ arg0: String, _ arg1: String) -> String"
        );

        let driver = build_driver("Demo", &swift_host_api_for_protocol(protocol), protocol);
        assert!(
            driver.contains("func testapi_arith__staged(_ arg0: String, _ arg1: String) -> String"),
            "{driver}"
        );
        assert!(
            !driver.contains("func testapi_arith__staged<a, b>"),
            "{driver}"
        );
    }

    #[test]
    fn staged_unit_host_method_keeps_its_exact_generic_slot() {
        let protocol = RunnerProtocol::HostStagedUnitCall;
        let contract = protocol.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::StagedUnitCall))
            .expect("staged-Unit host binding");
        let method = swift_method(binding, contract.host_types);

        assert_eq!(method.type_params, ["KioType_0"]);
        assert_eq!(method.arg_types, ["KioType_0"]);
        assert_eq!(method.ret_type, "()");
        assert_eq!(
            render_method_sig(&method),
            "testapi_main__staged<KioType_0>(_ arg0: KioType_0)"
        );

        let driver = build_driver("Demo", &swift_host_api_for_protocol(protocol), protocol);
        assert!(
            driver.contains("func testapi_main__staged<KioType_0>(_ arg0: KioType_0)"),
            "{driver}"
        );
        assert!(
            !driver.contains("func testapi_main__staged(_ arg0: Any)"),
            "{driver}"
        );
    }

    #[test]
    fn facade_aliases_keep_exact_host_and_callable_parameters() {
        let text_contract = RunnerProtocol::TestApiTextRootInt.contract();
        let string_code_at_binding = text_contract
            .host_fns
            .iter()
            .find(|binding| binding.leaf == "string_code_at")
            .expect("text protocol string_code_at");
        let string_code_at =
            swift_canonical_method(string_code_at_binding, text_contract.host_types);
        assert_eq!(
            string_code_at.ret_type,
            "Env_testapi_text__stringCodeAt_ret<StubHost>"
        );

        let compute_contract = RunnerProtocol::TestApiComputeRoot.contract();
        let loop_binding = compute_contract
            .host_fns
            .iter()
            .find(|binding| binding.leaf == "loop")
            .expect("compute protocol loop");
        let loop_method = swift_canonical_method(loop_binding, compute_contract.host_types);
        assert!(loop_method.arg_types[0].contains("Env_testapi_iter__loop_arg0_cbret"));
        assert_eq!(
            loop_method.arg_types[0],
            "(s) -> Env_testapi_iter__loop_arg0_cbret<s, r>"
        );

        let array_contract = RunnerProtocol::TestApiArray.contract();
        let array_pop_binding = array_contract
            .host_fns
            .iter()
            .find(|binding| {
                matches!(
                    binding.body,
                    HostFnBodyKind::Array {
                        operation: "pop-back",
                        ..
                    }
                )
            })
            .expect("array pop protocol binding");
        let array_pop = swift_canonical_method(array_pop_binding, array_contract.host_types);
        assert_eq!(array_pop.ret_type, "Env_testapi_array__arrayPopBack_ret<t>");

        let type_roundtrip = render_export_driver(ExportDriver::TypeRoundtrip, "pkg.testapi", true)
            .expect("type-roundtrip export driver");
        let pair_arg = export_newtype_member_alias("testapi/types", "Pair", "mk_pair", "arg0");
        assert!(type_roundtrip.contains(&format!("{pair_arg}(")));
        assert!(!type_roundtrip.contains(&format!("{pair_arg}<StubHost>")));
    }

    #[test]
    fn loop_signature_keeps_the_exact_function_identity() {
        let binding = HostFnBinding {
            module: "misleading/module",
            leaf: "not_loop",
            body: HostFnBodyKind::Loop,
        };

        let method = swift_canonical_method(&binding, &[]);
        let member = swift_host_member(binding.module, binding.leaf);
        assert_eq!(method.name, member);
        assert!(method.where_clause.is_empty());
        assert_eq!(
            method.arg_types[0],
            format!("(s) -> Env_{member}_arg0_cbret<s, r>")
        );
    }

    #[test]
    fn array_methods_use_the_exact_parameterized_host_carrier() {
        let api = swift_host_api_for_protocol(RunnerProtocol::TestApiArray);
        let pop_binding = RunnerProtocol::TestApiArray
            .contract()
            .host_fns
            .iter()
            .find(|binding| {
                matches!(
                    binding.body,
                    HostFnBodyKind::Array {
                        operation: "pop-back",
                        ..
                    }
                )
            })
            .expect("array pop protocol binding");
        assert!(matches!(
            canonical_kind(pop_binding.body),
            CanonicalKind::Array(ArrayOp::PopBack)
        ));
        let driver = build_driver("Demo", &api, RunnerProtocol::TestApiArray);
        assert!(driver.contains(KIO_ARRAY_DECL));
        assert!(!driver.contains("typealias testapi__Array"));
        assert!(driver.contains("KioHostType_testapi__Array<t>"));
        assert!(driver.contains("let a: KioArray<t> = arg0.value()"));
    }
}
