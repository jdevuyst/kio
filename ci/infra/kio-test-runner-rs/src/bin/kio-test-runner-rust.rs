//! `kio-test-runner-rust` — pointed at a `kio build rust` output
//! directory, runs the emitted crate's `main` and reports the exit
//! code.
//!
//! The crate layout the emitter produces (see
//! `specs/backends/rust.md` § Output layout) is `Cargo.toml` +
//! `src/lib.rs` + `src/host.rs` + `src/shapes.rs` + `src/ffi.rs` — a
//! self-contained library targeting `std` only. The runner skips
//! Cargo entirely: two `rustc` invocations and one process spawn are
//! faster and lighter on disk than `cargo run`'s build-script /
//! lib-then-bin / per-crate-`target/` pipeline. Cargo dependence
//! would force a per-case ~50–200 MB `target/` directory, which
//! the harness's per-(case, impl) scratch tree blows the disk on
//! at corpus scale.
//!
//! ## The runner consumes an exact protocol contract
//!
//! The host API the runner implements comes from the **protocol**
//! (`--protocol <name>`, see [`protocol`]). [`RunnerProtocol::contract`]
//! fixes the exact host types and functions, their native fixtures and
//! bodies, the export driver, and whether execution compiles, constructs,
//! or invokes the package. Artifact identity is supplied independently by
//! the corpus harness. The runner does not inspect `Cargo.toml`,
//! `src/host.rs`, or another emitted file to discover semantic members,
//! signatures, roles, fixtures, or exports. Emitted stable FFI aliases remain
//! ordinary Rust type paths used by the protocol-owned driver; rustc checks
//! that those independent expectations match the artifact.
//!
//! Pipeline (the [`TestRunner`] implementation):
//!
//!  1. `host_api` — projects the selected exact protocol contract into
//!     Rust trait members and associated-type fixtures. Shaped slots (sum
//!     returns, the `loop` step callback, and roundtrip payload shapes)
//!     are named through the emitted
//!     crate's `ffi` boundary aliases (`crate::ffi::env::<member>::<leaf>`
//!     / `crate::ffi::exp::<member>::<leaf>`), never by scraping the
//!     emitted source for signatures or shapes.
//!  2. `execute_artifact` — synthesize a `StubHost` whose
//!     `impl <Handle>Host for StubHost` maps each method to a canonical
//!     body. Each body is selected by the corresponding
//!     [`protocol::HostFnBodyKind`] in the contract and rendered to Rust.
//!  3. `rustc <case>/src/lib.rs --crate-type=rlib
//!     --crate-name=<name> --edition=2024 <profile flags>
//!     --out-dir <tmp>` — builds the package as an
//!     rlib under the independently supplied artifact namespace; rustc finds
//!     the sibling `host.rs` /
//!     `shapes.rs` / `ffi.rs` modules automatically. `<profile flags>`
//!     are the `--profile`-selected optimization
//!     level (see [`opt_profile`]): `unoptimized` (`-C opt-level=0`,
//!     fastest compile), the `default` `-C opt-level=1` (mild
//!     optimization at a fraction of `-O`'s compile cost, fast enough at
//!     runtime that goldens don't drag), or `optimized`
//!     (`-C opt-level=2`, most opt-sensitive coverage). Every profile
//!     keeps `-C debuginfo=0`.
//!  4. Write the synthesized driver to `<tmp>/driver.rs` and
//!     `rustc <tmp>/driver.rs --extern <name>=<tmp>/lib<name>.rlib
//!     --crate-type=bin --edition=2024 <profile flags> -o <tmp>/bin`.
//!  5. Spawn `<tmp>/bin`; capture its exit code.
//!  6. Clean up `<tmp>` and return.
//!
//! Exits 0 on success, 1 on rustc failure / runtime error; the CLI
//! tier is 2 per `specs/exit-codes.md`. A module call to host
//! `exit(n)` propagates `n` through the spawned process's exit code
//! (clamped to 0..=125 per `specs/exit-codes.md`).
//!
//! The harness supplies `KIO_TEST_RUNNER_BUILD_CACHE_DIR` as the
//! reusable build-artifact cache root. If
//! `KIO_TEST_RUNNER_COMPILER_WRAPPER` is set, cache misses compile
//! via `<wrapper> rustc ...`; cache keys still use the real rustc
//! identity. `KIO_TEST_RUNNER_CACHE_DISABLE=1` ignores those cache
//! variables and uses a fresh temp cache for one invocation.
//!
//! ## Arrays
//!
//! The `array` protocol exposes a polymorphic `Array(t)` host
//! environment type plus ten primitives (`array_make_empty`,
//! `array_make_filled`, `array_len`, `array_get`, `array_set`,
//! `array_push`, `array_pop_back`, `array_swap`, `array_clear`,
//! `array_clone`) that goldens like `exec_sieve_of_eratosthenes` and
//! `exec_insertion_sort` consume. The JS runner backs the type with a
//! plain `Array`; the Rust runner selects one invariant
//! `Rc<RefCell<Vec<KioStoredValue>>>` storage type for the declaration.
//! Its `PartialEq` is `Rc::ptr_eq`, matching JS reference identity.
//! Generated declaration-owned carriers keep the applied marker and use
//! `KioValue<A>` to pack and unpack each element. The Rust emitter therefore
//! exposes one nullary `ArrayStorage` associated type rather than a GAT; see
//! `specs/backends/rust.md` § FFI surface > Non-atomic host types.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

#[path = "../shared/artifact_identity.rs"]
mod artifact_identity;
#[path = "../shared/build_cache/mod.rs"]
mod build_cache;
#[path = "../shared/canonical.rs"]
#[allow(dead_code)]
mod canonical;
#[path = "../shared/compiler_observer.rs"]
mod compiler_observer;
use kio_ci_scheduler as compiler_admission;
#[path = "../shared/host_api.rs"]
#[allow(dead_code)]
mod host_api;
#[path = "../shared/opt_profile.rs"]
mod opt_profile;
#[path = "../shared/path_display.rs"]
mod path_display;
#[path = "../shared/protocol.rs"]
#[allow(dead_code)]
mod protocol;
#[path = "../rust/rlib_cache/mod.rs"]
mod rlib_cache;
#[path = "../shared/runner.rs"]
mod runner;

use artifact_identity::{ArtifactIdentity, ArtifactIdentityArgs, pascal_case};
use canonical::{ArrayOp, CanonicalKind};
use compiler_observer::CompilerObserver;
use host_api::{AssocType, HostApi, TraitMethod};
use opt_profile::OptProfile;
use protocol::{
    ExportDriver, HostFnBinding, HostFnBodyKind, HostTypeBinding, HostTypeFixture,
    HostTypeIdentity, ProtocolContract, ProtocolExecution, RoleFixture, RunnerProtocol,
    WIDE_CALLABLE_SLOT_COUNT,
};
use rlib_cache::{
    BinInput, RlibCache, RlibInput, collect_crate_files, default_target_triple, rustc_identity,
};
use runner::{EXIT_RUNTIME_FAILURE, EXIT_USAGE, TestRunner};

/// The `coexist` protocol's two-artifact execution (`shared/protocol.rs`
/// § The coexist protocol): compile each emitted crate to an rlib under
/// its own published crate name, link one driver against both — the
/// maximal-collision shape the namespace rule exists for — and interleave the calls
/// through per-package prefixing hosts. One golden exercises this
/// protocol, so it compiles cold with direct `rustc` invocations rather
/// than threading a two-rlib shape through the one-rlib bin cache.
fn run_coexist(
    contract: ProtocolContract,
    dir_a: &Path,
    identity_a: &ArtifactIdentity,
    dir_b: &Path,
    identity_b: &ArtifactIdentity,
    compiler_observer: &CompilerObserver,
    compiler_admission: &compiler_admission::CompilerAdmission,
) -> Result<i32, String> {
    let (name_a, name_b) = (&identity_a.namespace, &identity_b.namespace);
    if name_a == name_b {
        return Err(format!(
            "coexist requires two distinct package namespaces; both artifacts are `{name_a}`"
        ));
    }
    let build =
        tempfile::TempDir::new().map_err(|e| format!("cannot create build tempdir: {e}"))?;
    let rustc = |args: &[&str]| -> Result<(), String> {
        let mut command = coexist_rustc_command(compiler_observer);
        command.args(args).current_dir(build.path());
        let admitted = compiler_admission
            .acquire_for(&mut command)
            .map_err(|e| format!("acquiring compiler admission: {e}"))?;
        let out = admitted
            .output()
            .map_err(|e| format!("spawning rustc: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "rustc {} failed:\n{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    };
    for (name, dir) in [(name_a, dir_a), (name_b, dir_b)] {
        let lib_rs = dir.join("src/lib.rs");
        rustc(&[
            "--edition",
            "2024",
            "--crate-type",
            "rlib",
            "--crate-name",
            name,
            lib_rs.to_str().ok_or("non-utf8 artifact path")?,
            "-o",
            &format!("lib{name}.rlib"),
        ])?;
    }
    assert_eq!(
        contract.execution,
        ProtocolExecution::Invoke(ExportDriver::Coexist)
    );
    let coexist_host = host_api_for_contract(contract);
    let mut driver = String::new();
    for (tag, prefix, name) in [
        ("A", "first", name_a.as_str()),
        ("B", "second", name_b.as_str()),
    ] {
        let mut host_impl = String::new();
        for (assoc, binding) in coexist_host.assoc_types().zip(contract.host_types) {
            let HostTypeFixture::Role(role) = binding.fixture else {
                unreachable!("coexist contract contains a non-role host type")
            };
            host_impl.push_str(&format!(
                "    type {} = {};\n",
                assoc
                    .boundary_name
                    .as_deref()
                    .expect("exact coexist host type has a Rust member"),
                rust_native_for_fixture(role)
            ));
        }
        host_impl.push('\n');
        for (method, binding) in coexist_host.methods().zip(contract.host_fns) {
            let body = match binding.body {
                HostFnBodyKind::Print { .. } => format!("print!(\"{prefix}: {{arg0}}\");"),
                _ => rewrite_crate_paths(&render_rust_body_for_binding(binding, method), name),
            };
            host_impl.push_str(&format!(
                "    {} {{\n        {body}\n    }}\n",
                render_method_sig(method, name)
            ));
        }
        driver.push_str(&format!(
            "#[derive(Clone)]\nstruct Host{tag};\nimpl {name}::host::{h}Host for Host{tag} {{\n{host_impl}}}\n\n",
            h = pascal_case(name),
        ));
    }
    // The facade-shell half of the witness: both packages export the same
    // `pair() -> (I32 & String)`, so both crates expose the same canonical
    // binary `Product` facade; reading `._0` / `._1` from both in one
    // program proves the independent crates coexist.
    driver.push_str(&format!(
        "fn main() {{\n    let pa = {name_a}::create_{factory_a}(HostA);\n    let pb = {name_b}::create_{factory_b}(HostB);\n    pa.greeter.main.main();\n    pb.greeter.main.main();\n    pa.greeter.main.main();\n    let qa = pa.greeter.main.pair();\n    println!(\"first pair: {{}} {{}}\", qa._0, qa._1);\n    let qb = pb.greeter.main.pair();\n    println!(\"second pair: {{}} {{}}\", qb._0, qb._1);\n    let qa = pa.greeter.main.pair();\n    println!(\"first pair: {{}} {{}}\", qa._0, qa._1);\n}}\n",
        factory_a = artifact_identity::value_brand(name_a),
        factory_b = artifact_identity::value_brand(name_b),
    ));
    fs::write(build.path().join("driver.rs"), driver)
        .map_err(|e| format!("writing driver: {e}"))?;
    rustc(&[
        "--edition",
        "2024",
        "driver.rs",
        "--extern",
        &format!("{name_a}=lib{name_a}.rlib"),
        "--extern",
        &format!("{name_b}=lib{name_b}.rlib"),
        "-o",
        "driver",
    ])?;
    let status = Command::new(build.path().join("driver"))
        .status()
        .map_err(|e| format!("spawning coexist driver: {e}"))?;
    Ok(status.code().unwrap_or(EXIT_RUNTIME_FAILURE))
}

fn coexist_rustc_command(compiler_observer: &CompilerObserver) -> Command {
    compiler_observer.command(std::ffi::OsStr::new("rustc"), None)
}

const USAGE: &str = "\
Usage: kio-test-runner-rust [--protocol <name>] [--profile <name>] <output-dir>

Compile and run the Cargo crate emitted by `kio build rust` and
report the exit code.

Arguments:
  <output-dir>      Directory containing the emitted Cargo crate
                    (`Cargo.toml`, `src/lib.rs`, `src/host.rs`,
                    `src/shapes.rs`, `src/ffi.rs`) per
                    `specs/backends/rust.md`. The runner compiles
                    `src/lib.rs` directly with rustc. Artifact identity
                    is supplied independently by the corpus harness.

Environment:
  KIO_TEST_RUNNER_BUILD_CACHE_DIR
                    Required. Directory where reusable runner-built
                    artifacts live. Must persist across invocations
                    for warm-cache benefits, unless
                    KIO_TEST_RUNNER_CACHE_DISABLE=1.

  KIO_TEST_RUNNER_BUILD_CACHE_SIZE
                    Optional. Maximum persistent cache size. Accepts
                    bytes or K/M/G/T suffixes.

  KIO_TEST_RUNNER_COMPILER_WRAPPER
                    Optional. Command prepended to compatible compiler
                    invocations, e.g. `sccache`.

  KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER
                    Optional internal debug executable placed outermost
                    around each actual native compile. The value is one
                    opaque executable, not shell syntax.

  KIO_TEST_RUNNER_CACHE_DISABLE
                    Optional. Set to 1 to disable all persistent
                    runner cache behavior and compiler wrappers. The
                    debug compiler observer remains active. Empty, unset,
                    or 0 means enabled; any other value is an error.

  KIO_TEST_RUNNER_PROFILE
                    Optional. Optimization profile for the compile:
                    `unoptimized` (-C opt-level=0), `default`
                    (-C opt-level=1), or `optimized` (-C opt-level=2).
                    Defaults to `default`. --profile overrides it.

Other options:
  --package-name <name>
                    Kio source package name supplied by the corpus harness.
                    Repeat exactly twice for `--protocol coexist`.
  --artifact-namespace <namespace>
                    Effective namespace of the preceding package
                    artifact.
  --protocol <name>
                    Host/Kio interaction protocol to run. Defaults to
                    `empty-main` — the exact empty-host contract that
                    instantiates the package and invokes exported `main`.
                    See the runner README for the protocol catalogue.
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
                    Ok(protocol) => protocol,
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
                    Ok(protocol) => protocol,
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

    let is_coexist = matches!(
        protocol.contract().execution,
        ProtocolExecution::Invoke(ExportDriver::Coexist)
    );
    let expected_packages = if is_coexist { 2 } else { 1 };
    let identities = match identity_args.resolve("rust", expected_packages) {
        Ok(identities) => identities,
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
    let compiler_observer = match CompilerObserver::from_env() {
        Ok(observer) => observer,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    // The coexist protocol is the one two-artifact protocol: exactly two
    // positional output dirs; every other protocol takes exactly one.
    if is_coexist {
        return match positional.as_slice() {
            [a, b] => match run_coexist(
                protocol.contract(),
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

    let cache_disabled = match cache_disable_from_env() {
        Ok(disabled) => disabled,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    let cache = if cache_disabled {
        RunnerCache::Disabled
    } else {
        let cache_dir = match env::var_os("KIO_TEST_RUNNER_BUILD_CACHE_DIR") {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            Some(_) => {
                eprintln!("error: KIO_TEST_RUNNER_BUILD_CACHE_DIR must not be empty");
                return EXIT_USAGE;
            }
            None => {
                eprintln!("error: KIO_TEST_RUNNER_BUILD_CACHE_DIR is required");
                eprintln!();
                eprintln!("{USAGE}");
                return EXIT_USAGE;
            }
        };
        let max_bytes = match cache_size_from_env() {
            Ok(max_bytes) => max_bytes,
            Err(e) => {
                eprintln!("error: {e}");
                return EXIT_USAGE;
            }
        };
        let compiler_wrapper = match env::var_os("KIO_TEST_RUNNER_COMPILER_WRAPPER") {
            Some(w) if !w.is_empty() => Some(w),
            _ => None,
        };
        RunnerCache::Persistent {
            cache_dir,
            compiler_wrapper,
            max_bytes,
        }
    };

    let profile = match OptProfile::resolve(profile_override) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };
    let runner = RustRunner {
        cache,
        compiler_observer,
        compiler_admission,
        protocol,
        profile,
        identity: identities.into_iter().next().unwrap(),
    };
    runner.run(dir, protocol)
}

fn cache_disable_from_env() -> Result<bool, String> {
    match env::var("KIO_TEST_RUNNER_CACHE_DISABLE") {
        Ok(value) => parse_cache_disable_value(&value),
        Err(env::VarError::NotPresent) => Ok(false),
        Err(env::VarError::NotUnicode(_)) => {
            Err("KIO_TEST_RUNNER_CACHE_DISABLE must be valid Unicode".to_owned())
        }
    }
}

fn parse_cache_disable_value(value: &str) -> Result<bool, String> {
    match value {
        "" | "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(format!(
            "KIO_TEST_RUNNER_CACHE_DISABLE must be 1, 0, or empty; got {value:?}"
        )),
    }
}

fn cache_size_from_env() -> Result<Option<u64>, String> {
    match env::var("KIO_TEST_RUNNER_BUILD_CACHE_SIZE") {
        Ok(value) if value.is_empty() => Ok(None),
        Ok(value) => parse_cache_size_literal(&value).map(Some),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => {
            Err("KIO_TEST_RUNNER_BUILD_CACHE_SIZE must be valid Unicode".to_owned())
        }
    }
}

fn parse_cache_size_literal(value: &str) -> Result<u64, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("KIO_TEST_RUNNER_BUILD_CACHE_SIZE must not be blank".to_owned());
    }
    let upper = trimmed.to_ascii_uppercase();
    let (digits, multiplier) = match upper.as_str() {
        s if s.ends_with("KB") => (&trimmed[..trimmed.len() - 2], 1024_u64),
        s if s.ends_with('K') => (&trimmed[..trimmed.len() - 1], 1024_u64),
        s if s.ends_with("MB") => (&trimmed[..trimmed.len() - 2], 1024_u64.pow(2)),
        s if s.ends_with('M') => (&trimmed[..trimmed.len() - 1], 1024_u64.pow(2)),
        s if s.ends_with("GB") => (&trimmed[..trimmed.len() - 2], 1024_u64.pow(3)),
        s if s.ends_with('G') => (&trimmed[..trimmed.len() - 1], 1024_u64.pow(3)),
        s if s.ends_with("TB") => (&trimmed[..trimmed.len() - 2], 1024_u64.pow(4)),
        s if s.ends_with('T') => (&trimmed[..trimmed.len() - 1], 1024_u64.pow(4)),
        _ => (trimmed, 1_u64),
    };
    let base = digits.trim().parse::<u64>().map_err(|_| {
        format!("KIO_TEST_RUNNER_BUILD_CACHE_SIZE must be a byte count with optional K/M/G/T suffix; got {value:?}")
    })?;
    if base == 0 {
        return Err("KIO_TEST_RUNNER_BUILD_CACHE_SIZE must be greater than zero".to_owned());
    }
    base.checked_mul(multiplier)
        .ok_or_else(|| format!("KIO_TEST_RUNNER_BUILD_CACHE_SIZE is too large: {value:?}"))
}

/// The Rust backend's `TestRunner` implementation.
///
/// Holds the runner build-cache policy and the selected protocol.
/// Persistent cache mode uses the harness-supplied cache root;
/// disabled mode uses a per-invocation temp root. The protocol is
/// carried here so `host_api` — whose trait signature takes no
/// protocol — can return the protocol's fixed [`HostApi`].
struct RustRunner {
    cache: RunnerCache,
    compiler_observer: CompilerObserver,
    compiler_admission: compiler_admission::CompilerAdmission,
    protocol: RunnerProtocol,
    identity: ArtifactIdentity,
    /// The `--profile`-selected optimization level (see [`opt_profile`]).
    /// Feeds the rustc flags on a compile miss and the rlib/bin cache
    /// key, so a `default`-built artifact is never served for an
    /// `optimized` request.
    profile: OptProfile,
}

enum RunnerCache {
    Persistent {
        cache_dir: PathBuf,
        compiler_wrapper: Option<std::ffi::OsString>,
        max_bytes: Option<u64>,
    },
    Disabled,
}

impl TestRunner for RustRunner {
    fn host_api(&self) -> HostApi {
        host_api_for_protocol(self.protocol)
    }

    fn execute_artifact(
        &self,
        output_dir: &Path,
        host: &HostApi,
        protocol: RunnerProtocol,
    ) -> Result<i32, String> {
        let crate_name = self.identity.namespace.as_str();

        let disabled_cache_temp;
        let (cache_dir, compiler_wrapper, max_bytes, cache_label) = match &self.cache {
            RunnerCache::Persistent {
                cache_dir,
                compiler_wrapper,
                max_bytes,
            } => (
                cache_dir.clone(),
                compiler_wrapper.clone(),
                *max_bytes,
                "rlib cache",
            ),
            RunnerCache::Disabled => {
                disabled_cache_temp = tempfile::TempDir::new()
                    .map_err(|e| format!("cannot create disabled-cache tempdir: {e}"))?;
                (
                    disabled_cache_temp.path().to_path_buf(),
                    None,
                    None,
                    "disabled-cache temp rlib cache",
                )
            }
        };
        // Observer policy is deliberately outside the cache-mode branch: a
        // disabled cache removes the acceleration wrapper, not debug
        // observation of the resulting cold compiler invocation.
        let cache = RlibCache::open(
            cache_dir.clone(),
            compiler_wrapper,
            self.compiler_observer.clone(),
            max_bytes,
            self.compiler_admission.clone(),
        )
        .map_err(|e| format!("cannot open {cache_label} at {}: {e}", cache_dir.display()))?;

        let rustc_path = PathBuf::from("rustc");
        let rustc_id =
            rustc_identity(&rustc_path).map_err(|e| format!("probing rustc identity: {e}"))?;
        let target_triple = default_target_triple(&rustc_path)
            .map_err(|e| format!("probing rustc host triple: {e}"))?;

        let crate_files = collect_crate_files(output_dir)
            .map_err(|e| format!("walking emitted crate at {}: {e}", output_dir.display()))?;

        let rlib_input = RlibInput {
            rustc_identity: rustc_id.clone(),
            target_triple: target_triple.clone(),
            edition: "2024".into(),
            profile: self.profile,
            crate_name: crate_name.to_owned(),
            crate_files,
        };

        if matches!(
            protocol.contract().execution,
            ProtocolExecution::CompileOnly
        ) {
            cache
                .get_or_compile_rlib(&rlib_input, output_dir)
                .map_err(|e| format!("building Rust rlib for {}: {e}", output_dir.display()))?;
            return Ok(0);
        }

        let driver_src = build_driver_for_protocol(crate_name, host, protocol)?;

        // Driver source lands in its own tempdir so the rustc
        // invocation has a stable on-disk path to read. The
        // driver's bytes feed the bin cache key directly; the file
        // location is incidental.
        let driver_dir =
            tempfile::TempDir::new().map_err(|e| format!("cannot create driver tempdir: {e}"))?;
        let driver_path = driver_dir.path().join("driver.rs");
        fs::write(&driver_path, driver_src.as_bytes())
            .map_err(|e| format!("cannot write driver at {}: {e}", driver_path.display()))?;

        // The bin key chains the rlib key; compute it locally so
        // the bin input is complete before the cache call
        // (matches what `get_or_compile_bin` does internally).
        let rlib_key = rlib_cache::rlib_key(&rlib_input);
        let bin_input = BinInput {
            rustc_identity: rustc_id,
            target_triple,
            edition: "2024".into(),
            profile: self.profile,
            rlib_key,
            crate_name: crate_name.to_owned(),
            driver_source: driver_src.into_bytes(),
            linker_flags: vec![("--extern".into(), crate_name.to_owned())],
        };

        let bin_path = cache
            .get_or_compile_bin(&rlib_input, output_dir, &driver_path, &bin_input)
            .map_err(|e| format!("building Rust bin for {}: {e}", output_dir.display()))?;

        // Spawn the bin. Its stdout / stderr are inherited so the
        // harness's diff comparator sees them on its own pipes;
        // the exit code is propagated as the runner's exit code.
        let status = Command::new(&bin_path)
            .status()
            .map_err(|e| format!("spawning driver bin {}: {e}", bin_path.display()))?;
        Ok(status.code().unwrap_or(EXIT_RUNTIME_FAILURE))
    }
}

/// Project the protocol registry's exact Rust host boundary.
///
/// The registry is the only semantic input. In particular, this function
/// never widens a contract to a tier-wide candidate inventory and never
/// consults an emitted trait to recover the member set.
fn host_api_for_protocol(protocol: RunnerProtocol) -> HostApi {
    host_api_for_contract(protocol.contract())
}

fn host_api_for_contract(contract: ProtocolContract) -> HostApi {
    host_api::project_host_api(
        contract,
        |binding| AssocType {
            name: binding.leaf.to_owned(),
            boundary_name: Some(host_api::rust_host_member(binding.module, binding.leaf)),
            role: match binding.fixture {
                HostTypeFixture::Role(role) | HostTypeFixture::SelectedRole(role) => {
                    role.role().to_owned()
                }
                _ => String::new(),
            },
            type_params: (0..binding.type_arity)
                .map(|index| match index {
                    0 => "t".to_owned(),
                    1 => "u".to_owned(),
                    2 => "v".to_owned(),
                    _ => format!("t{index}"),
                })
                .collect(),
        },
        |binding| rust_host_method(binding, contract.host_types),
    )
}

fn rust_host_method(binding: &HostFnBinding, host_types: &[HostTypeBinding]) -> TraitMethod {
    let member = host_api::rust_host_member(binding.module, binding.leaf);
    let ffi = |leaf: &str| format!("crate::ffi::env::{member}::{leaf}");
    let ffi_for_host = |leaf: &str| format!("{}<Self>", ffi(leaf));
    let assoc = |identity: HostTypeIdentity, expected_fixture: HostTypeFixture| {
        let binding = host_types
            .iter()
            .find(|binding| binding.module == identity.module && binding.leaf == identity.leaf)
            .unwrap_or_else(|| {
                panic!(
                    "protocol host fn requires undeclared host type `{}/{}`",
                    identity.module, identity.leaf
                )
            });
        assert_eq!(
            binding.fixture, expected_fixture,
            "protocol host type `{}/{}` has the wrong native fixture",
            identity.module, identity.leaf
        );
        format!(
            "Self::{}",
            host_api::rust_host_member(binding.module, binding.leaf)
        )
    };
    let method = |args: &[&str], ret: &str| TraitMethod {
        name: member.clone(),
        type_params: Vec::new(),
        arg_types: args.iter().map(|arg| (*arg).to_owned()).collect(),
        ret_type: ret.to_owned(),
        where_clause: String::new(),
    };
    let role =
        |role, borrow_string| protocol::rust_native_role_type(role, host_types, borrow_string);
    let marker_facade = |marker: &str| format!("<{marker} as crate::shapes::KioType>::Facade");
    let ffi_use = |leaf: &str, arguments: &[&str]| {
        if arguments.is_empty() {
            ffi(leaf)
        } else {
            format!("{}<{}>", ffi(leaf), arguments.join(", "))
        }
    };

    match binding.body {
        HostFnBodyKind::UnreachableI32Print { i32 } => method(&[role(i32, false)], "()"),
        HostFnBodyKind::MakeToken { value_i32, token } => method(
            &[role(value_i32, false)],
            &assoc(token, HostTypeFixture::Token),
        ),
        HostFnBodyKind::TokenValue { token, value_i32 } => method(
            &[&assoc(token, HostTypeFixture::Token)],
            role(value_i32, false),
        ),
        HostFnBodyKind::BoxGet { box_type: _ } => TraitMethod {
            type_params: vec!["t".to_owned()],
            arg_types: vec![ffi_use("arg0", &["Self", "t"])],
            ret_type: marker_facade("t"),
            ..method(&[], "")
        },
        HostFnBodyKind::BoxMake { box_type: _ } => TraitMethod {
            type_params: vec!["t".to_owned()],
            arg_types: vec![marker_facade("t")],
            ret_type: ffi_use("ret", &["Self", "t"]),
            ..method(&[], "")
        },
        HostFnBodyKind::CallStep { i32, .. } => TraitMethod {
            arg_types: vec![ffi_for_host("arg0"), role(i32, false).to_owned()],
            ret_type: role(i32, false).to_owned(),
            ..method(&[], "")
        },
        HostFnBodyKind::MakePairCallback { i32, .. } => TraitMethod {
            arg_types: vec![ffi_for_host("arg0"), role(i32, false).to_owned()],
            ret_type: role(i32, false).to_owned(),
            ..method(&[], "")
        },
        HostFnBodyKind::MakeStep { i32 } => method(&[role(i32, false)], &ffi_for_host("ret")),
        HostFnBodyKind::ApplyPoly { string } => TraitMethod {
            arg_types: vec![ffi_for_host("arg0")],
            ret_type: role(string, false).to_owned(),
            ..method(&[], "")
        },
        HostFnBodyKind::MakePairStructural { i32, string } => method(
            &[role(i32, false), role(string, false)],
            &ffi_for_host("ret"),
        ),
        HostFnBodyKind::ProducePair { .. } => method(&[], &ffi_for_host("ret")),
        HostFnBodyKind::SumToString { string, .. } => {
            method(&[&ffi_for_host("arg0")], role(string, false))
        }
        HostFnBodyKind::RoundFunctor
        | HostFnBodyKind::RoundPicker
        | HostFnBodyKind::RoundPolyThunk
        | HostFnBodyKind::RoundPolyUnitSlot => {
            method(&[&ffi_for_host("arg0")], &ffi_for_host("ret"))
        }
        HostFnBodyKind::NestedCurriedRoundtrip { string: _ } => {
            method(&[&ffi_for_host("arg0")], &ffi_for_host("ret"))
        }
        HostFnBodyKind::InvokeSubstitutedUnitCallback { text } => TraitMethod {
            arg_types: vec![ffi_for_host("arg0")],
            ret_type: role(text, false).to_owned(),
            ..method(&[], "")
        },
        HostFnBodyKind::ReturnedForallUnit => method(&[], &ffi_for_host("ret")),
        HostFnBodyKind::ObservePacked { i32 } => method(&[&ffi_for_host("arg0")], role(i32, false)),
        HostFnBodyKind::TraceUnit { .. } => method(&[], "()"),
        HostFnBodyKind::StagedUnitCall => TraitMethod {
            type_params: vec!["A".to_owned(), "B".to_owned()],
            arg_types: vec![marker_facade("A")],
            ret_type: "()".to_owned(),
            where_clause: String::new(),
            name: member,
        },
        HostFnBodyKind::StagedSecond { string } => TraitMethod {
            type_params: vec!["A".to_owned(), "B".to_owned()],
            ..method(
                &[role(string, false), role(string, false)],
                role(string, false),
            )
        },
        HostFnBodyKind::Loop => TraitMethod {
            type_params: vec!["s".to_owned(), "r".to_owned()],
            arg_types: vec![ffi_use("arg0", &["s", "r"]), marker_facade("s")],
            ret_type: marker_facade("r"),
            where_clause: String::new(),
            name: member,
        },
        HostFnBodyKind::Array {
            operation, index, ..
        } => {
            let array = ffi_use("arg0", &["Self", "t"]);
            let element = marker_facade("t");
            let index = index.map(|index| role(index, false));
            let mut rendered = match operation {
                "make-empty" => method(&[], &ffi_use("ret", &["Self", "t"])),
                "make-filled" => method(
                    &[index.expect("make-filled index role"), &element],
                    &ffi_use("ret", &["Self", "t"]),
                ),
                "len" => method(&[&array], index.expect("len index role")),
                "get" => method(&[&array, index.expect("get index role")], &element),
                "set" => method(&[&array, index.expect("set index role"), &element], "()"),
                "push" => method(&[&array, &element], "()"),
                "pop-back" => method(&[&array], &ffi_use("ret", &["t"])),
                "swap" => method(
                    &[
                        &array,
                        index.expect("swap index role"),
                        index.expect("swap index role"),
                    ],
                    "()",
                ),
                "clear" => method(&[&array], "()"),
                "clone" => method(&[&array], &ffi_use("ret", &["Self", "t"])),
                other => unreachable!("unknown protocol array operation `{other}`"),
            };
            rendered.type_params.push("t".to_owned());
            rendered
        }
        _ => {
            let mut rendered = protocol::canonical_host_method(binding, host_types);
            let own_strings = |ty: String| {
                ty.replace("&SelectedString", "SelectedString")
                    .replace("&String", "String")
            };
            rendered.arg_types = rendered.arg_types.into_iter().map(own_strings).collect();
            rendered.ret_type = own_strings(rendered.ret_type);
            rendered.where_clause = own_strings(rendered.where_clause);
            rendered
        }
    }
}

/// The two canonical variant paths for a binary sum-typed FFI alias.
///
/// `alias` is an `ffi` type-alias path such as
/// `crate::ffi::env::string_code_at::ret` or
/// `crate::ffi::env::r#loop::arg0_cbret`. Exact facade-shell identity keeps
/// an anonymous structural sum distinct from a label-bearing sum, so the
/// alias expands to the canonical `Sum` facade and therefore exposes `Left`
/// and `Right`.
/// Generic arguments remain part of the alias and are rendered with expression
/// turbofish syntax so the constructor cannot drift to another instantiation.
fn sum_variant_ctors(alias: &str) -> (String, String) {
    let alias = struct_literal_expr_path(alias);
    (format!("{alias}::Left"), format!("{alias}::Right"))
}

fn right_nested_product_literal(outer: &str, product: &str, values: &[String]) -> String {
    assert!(
        values.len() >= 2,
        "a structural product literal needs at least two values"
    );
    let mut tail = values.last().expect("nonempty product values").clone();
    for value in values[1..values.len() - 1].iter().rev() {
        tail = format!("{product} {{ _0: {value}, _1: {tail} }}");
    }
    format!("{outer} {{ _0: {}, _1: {tail} }}", values[0])
}

fn right_nested_product_access(value: &str, index: usize, arity: usize) -> String {
    assert!(arity >= 2, "a structural product access needs arity >= 2");
    assert!(index < arity, "structural product slot is in range");
    if index + 1 == arity {
        format!("{value}{}", "._1".repeat(index))
    } else {
        format!("{value}{}._0", "._1".repeat(index))
    }
}

fn right_nested_sum_literal(
    outer: &str,
    sum: &str,
    index: usize,
    arity: usize,
    value: &str,
) -> String {
    assert!(arity >= 2, "a structural sum literal needs arity >= 2");
    assert!(index < arity, "structural sum arm is in range");
    if index == 0 {
        return format!("{outer}::Left({value})");
    }
    let mut nested = if index + 1 == arity {
        value.to_owned()
    } else {
        format!("{sum}::Left({value})")
    };
    for _ in 1..index {
        nested = format!("{sum}::Right({nested})");
    }
    format!("{outer}::Right({nested})")
}

fn build_driver_for_protocol(
    crate_name: &str,
    host: &HostApi,
    protocol: RunnerProtocol,
) -> Result<String, String> {
    let contract = protocol.contract();
    let main_body = match contract.execution {
        ProtocolExecution::CompileOnly => {
            return Err("compile-only protocol unexpectedly requested a Rust driver".to_owned());
        }
        ProtocolExecution::ConstructOnly => "    let _ = pkg;\n".to_owned(),
        ProtocolExecution::Invoke(driver) => {
            let export_root = match driver {
                ExportDriver::Main { module } => module.strip_suffix("/main"),
                _ => contract.testapi_conformed.then_some("testapi"),
            };
            match driver {
                ExportDriver::Main { module } => rust_main_call(module),
                ExportDriver::Coexist => {
                    unreachable!("--protocol coexist dispatches through run_coexist")
                }
                ExportDriver::NamespaceRoundtrip => {
                    // The exported `main` / `utils` modules sit at
                    // `pkg.testapi.main` / `pkg.testapi.utils` under the testapi
                    // package surface.
                    let (main_ns, utils_ns) = match &export_root {
                        Some(root) => (format!("pkg.{root}.main"), format!("pkg.{root}.utils")),
                        None => ("pkg.main".to_owned(), "pkg.utils".to_owned()),
                    };
                    format!(
                        "    println!(\"{{}}\", {main_ns}.answer());\n    \
                 println!(\"{{}}\", {utils_ns}.echo(\"namespace-utils\".to_string()));\n"
                    )
                }
                ExportDriver::CallbackRoundtrip => {
                    // The callback fns are exported in `testapi/main`
                    // (`pkg.testapi.main.apply_twice` / `.make_step`).
                    let main_ns = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg".to_owned(),
                    };
                    format!(
                        "    let callback = {crate_name}::ffi::exp::testapi_main__applyTwice::arg0::<StubHost>::new(|n| n + 3);\n    \
                 println!(\"{{}}\", {main_ns}.applyTwice(callback, 10));\n    \
                 let step = {main_ns}.makeStep(4);\n    \
                 println!(\"{{}}\", step.call(5));\n"
                    )
                }
                ExportDriver::ModuleRoundtrip => {
                    // The exported `api` module sits at `pkg.testapi.api` under
                    // the testapi package surface (module `testapi/api`).
                    let api = match &export_root {
                        Some(root) => format!("pkg.{root}.api"),
                        None => "pkg.api".to_owned(),
                    };
                    format!(
                        "    println!(\"{{}}\", {api}.tag());\n    \
                 println!(\"{{}}\", {api}.value());\n    \
                 println!(\"{{}}\", {api}.echo(\"module-echo\".to_string()));\n"
                    )
                }
                ExportDriver::MultilabelRoundtrip => {
                    // `say` is exported in `testapi/main`, reached at
                    // `pkg.testapi.main.say`. It takes the multi-label product
                    // `(a: A, b: B)`; the `exp::testapi_main__say::arg0<H>` alias
                    // names its exact generic shell. The source labels select
                    // the leaf newtypes, while the public structural product
                    // keeps its canonical positional `_0` / `_1` fields.
                    let main_ns = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg".to_owned(),
                    };
                    format!(
                        "    let payload = {crate_name}::ffi::exp::testapi_main__say::arg0::<StubHost> {{\n\
                 \x20       _0: {crate_name}::shapes::nominal::testapi::main::A::<StubHost>::mk(42),\n\
                 \x20       _1: {crate_name}::shapes::nominal::testapi::main::B::<StubHost>::mk(\"shown\\n\".to_string()),\n\
                 \x20   }};\n\
                 \x20   {main_ns}.say(payload);\n\
                 \x20   let row: {crate_name}::ffi::exp::testapi_main__echoPair::arg0<StubHost> = {crate_name}::ffi::exp::testapi_main__echoPair::arg0::<StubHost> {{\n\
                 \x20       _0: {crate_name}::shapes::nominal::testapi::main::A::<StubHost>::mk(88),\n\
                 \x20       _1: {crate_name}::shapes::nominal::testapi::main::B::<StubHost>::mk(\"99\".to_string()),\n\
                 \x20   }};\n\
                 \x20   let row = {main_ns}.echoPair(row);\n\
                 \x20   println!(\"{{}}\", {crate_name}::shapes::nominal::testapi::main::A::<StubHost>::get(row._0));\n\
                 \x20   println!(\"{{}}\", {crate_name}::shapes::nominal::testapi::main::B::<StubHost>::get(row._1));\n\
                 \x20   let a = {crate_name}::shapes::nominal::testapi::main::A::<StubHost>::mk(111);\n\
                 \x20   let a = {main_ns}.echoA(a);\n\
                 \x20   println!(\"{{}}\", {crate_name}::shapes::nominal::testapi::main::A::<StubHost>::get(a));\n"
                    )
                }
                ExportDriver::PolyRoundtrip => {
                    // The polymorphic fns are exported at the testapi root
                    // (`pkg.testapi.poly_echo` / `.keep_left`).
                    let root = match &export_root {
                        Some(r) => format!("pkg.{r}"),
                        None => "pkg".to_owned(),
                    };
                    format!(
                        "    println!(\"{{}}\", {root}.polyEcho::<{crate_name}::shapes::KioNative<String>>(\"poly-string\".to_string()));\n    \
                 println!(\"{{}}\", {root}.polyEcho::<{crate_name}::shapes::KioNative<i32>>(42));\n    \
                 println!(\"{{}}\", {root}.keepLeft::<{crate_name}::shapes::KioNative<String>, {crate_name}::shapes::KioNative<i32>>(\"left\".to_string(), 99));\n"
                    )
                }
                ExportDriver::FunctorDictRoundtrip => format!(
                    r#"    use {crate_name}::shapes::{{KioNative, KioFn1, KioTypeConstructor1, Product}};
    use {crate_name}::ffi::exp::testapi_types__Functor::fmap_ret_impl as _;
    type BoxHead = {crate_name}::shapes::KioNewtypeConstructor_testapi_stypes__Box_P0<StubHost>;
    type IntegerBox = {crate_name}::shapes::nominal::testapi::types::Box<StubHost, KioNative<i32>>;
    type TextBox = {crate_name}::shapes::nominal::testapi::types::Box<StubHost, KioNative<String>>;
    type Functor = {crate_name}::shapes::nominal::testapi::types::Functor<StubHost, BoxHead>;
    let integers = ::std::rc::Rc::new(::std::cell::RefCell::new(Vec::new()));
    let texts = ::std::rc::Rc::new(::std::cell::RefCell::new(Vec::new()));
    let events = integers.clone();
    let to_text = KioFn1::<KioNative<i32>, KioNative<String>>::new(move |value| {{ events.borrow_mut().push(value); format!("v:{{value}}") }});
    let events = texts.clone();
    let to_integer = KioFn1::<KioNative<String>, KioNative<i32>>::new(move |value| {{ events.borrow_mut().push(value.clone()); value.len() as i32 }});
    let dict = pkg.testapi.main.echoFunctor(pkg.testapi.main.boxFunctor());
    let first = pkg.testapi.main.applyFunctor::<KioNative<i32>, KioNative<String>>(dict.clone(), to_text.clone(), IntegerBox::mkBox(Product {{ _0: 42, _1: () }}));
    assert_eq!(TextBox::unBox(first)._0, "v:42");
    let second = pkg.testapi.main.applyFunctor::<KioNative<String>, KioNative<i32>>(dict.clone(), to_integer.clone(), TextBox::mkBox(Product {{ _0: "apple".to_owned(), _1: () }}));
    assert_eq!(IntegerBox::unBox(second)._0, 5);
    let map = Functor::fmap(dict);
    let third = map.apply::<KioNative<i32>, KioNative<String>>(Product {{ _0: to_text, _1: BoxHead::lift::<KioNative<i32>>(IntegerBox::mkBox(Product {{ _0: 7, _1: () }})) }});
    assert_eq!(TextBox::unBox(BoxHead::project::<KioNative<String>>(third))._0, "v:7");
    let fourth = map.apply::<KioNative<String>, KioNative<i32>>(Product {{ _0: to_integer, _1: BoxHead::lift::<KioNative<String>>(TextBox::mkBox(Product {{ _0: "pear".to_owned(), _1: () }})) }});
    assert_eq!(IntegerBox::unBox(BoxHead::project::<KioNative<i32>>(fourth))._0, 4);
    assert_eq!(*integers.borrow(), [42, 7]);
    assert_eq!(*texts.borrow(), ["apple", "pear"]);
    println!("functor dictionary ok");
"#
                ),
                ExportDriver::RustCallbackAliases => {
                    format!(
                        r#"    use {crate_name}::shapes::{{KioNative, KioType}};
    use {crate_name}::ffi::exp::{{testapi_main__Packed as packed, testapi_main__accept as accept, testapi_main__callback as callback}};
    use accept::ret_impl as _;
    use callback::ret_cbret_impl as _;
    struct Open(::std::rc::Rc<::std::cell::Cell<usize>>);
    impl packed::open_continuation_impl<StubHost, KioNative<i32>> for Open {{
        fn apply<U: KioType>(&self, value: packed::open_continuation_cbarg<StubHost, KioNative<i32>, U>) -> i32 {{
            fn accepts_payload<U: KioType, V: packed::open_continuation_cbarg_impl<StubHost, KioNative<i32>, U>>(_value: &V) {{}}
            accepts_payload::<U, _>(&value);
            self.0.set(self.0.get() + 1);
            91
        }}
    }}
    let opens = ::std::rc::Rc::new(::std::cell::Cell::new(0));
    let continuation = packed::open_continuation::<StubHost, KioNative<i32>>::new(Open(opens.clone()));
    let opened = {crate_name}::shapes::nominal::testapi::main::Packed::<StubHost>::open(pkg.testapi.main.makePacked(), continuation);
    assert_eq!(opened, 91);
    assert_eq!(opens.get(), 1);
    struct Identity;
    impl accept::arg0_impl<StubHost> for Identity {{
        fn apply<A: KioType>(&self, value: accept::arg0_cbarg<A>) -> accept::arg0_cbret<A> {{ value }}
    }}
    let echoed = pkg.testapi.main.accept(accept::arg0::<StubHost>::new(Identity));
    let integer: accept::ret_cbarg<KioNative<i32>> = 37;
    let integer: accept::ret_cbret<KioNative<i32>> = echoed.apply::<KioNative<i32>>(integer);
    assert_eq!(integer, 37);
    let text: accept::ret_cbarg<KioNative<String>> = "alias".to_owned();
    let text: accept::ret_cbret<KioNative<String>> = echoed.apply::<KioNative<String>>(text);
    assert_eq!(text, "alias");
    struct ReturnedIdentity;
    impl callback::ret_cbret_impl<StubHost> for ReturnedIdentity {{
        fn apply<A: KioType>(&self, value: callback::ret_cbret_cbarg<A>) -> callback::ret_cbret_cbret<A> {{ value }}
    }}
    let constructed = callback::ret_cbret::<StubHost>::new(ReturnedIdentity);
    assert_eq!(constructed.apply::<KioNative<i32>>(83), 83);
    let produced: callback::ret_cbret<StubHost> = pkg.testapi.main.callback().call();
    assert_eq!(produced.apply::<KioNative<i32>>(83), 83);
    println!("callback aliases ok");
"#
                    )
                }
                ExportDriver::CallableSlotsRoundtrip => {
                    let main = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg".to_owned(),
                    };
                    let exp = format!("{crate_name}::ffi::exp::testapi_main");
                    let shapes = format!("{crate_name}::shapes");
                    format!(
                        "    let product_calls = ::std::rc::Rc::new(::std::cell::Cell::new(0));\n\
                         \x20   let count = product_calls.clone();\n\
                         \x20   let product: {exp}__applyProduct::arg0<StubHost> = {exp}__applyProduct::arg0::<StubHost> {{ _0: {shapes}::KioFn1::new(move |value| {{ count.set(count.get() + 1); value + 5 }}), _1: 11 }};\n\
                         \x20   assert_eq!({main}.applyProduct(product.clone()), 16);\n\
                         \x20   let echoed_product = {main}.echoProduct({exp}__echoProduct::arg0::<StubHost> {{ _0: product._0, _1: 17 }});\n\
                         \x20   assert_eq!(echoed_product._1, 17);\n\
                         \x20   assert_eq!(echoed_product._0.call(echoed_product._1), 22);\n\
                         \x20   let made_product = {main}.makeProduct(23);\n\
                         \x20   assert_eq!(made_product._1, 23);\n\
                         \x20   assert_eq!(made_product._0.call(29), 29);\n\
                         \x20   let sum_calls = ::std::rc::Rc::new(::std::cell::Cell::new(0));\n\
                         \x20   let count = sum_calls.clone();\n\
                         \x20   let sum: {exp}__applySum::arg0<StubHost> = {exp}__applySum::arg0::<StubHost>::Left({shapes}::KioFn1::new(move |value| {{ count.set(count.get() + 1); value + 7 }}));\n\
                         \x20   assert_eq!({main}.applySum(sum.clone(), 31), 38);\n\
                         \x20   match {main}.echoSum(sum) {{ {shapes}::Sum::Left(step) => assert_eq!(step.call(37), 44), _ => panic!(\"callable sum arm changed\") }}\n\
                         \x20   match {main}.makeCallableSum() {{ {shapes}::Sum::Left(step) => assert_eq!(step.call(41), 41), _ => panic!(\"callable sum arm changed\") }}\n\
                         \x20   let scalar = {main}.makeScalarSum(97);\n\
                         \x20   match &scalar {{ {shapes}::Sum::Right(value) => assert_eq!(*value, 97), _ => panic!(\"scalar sum arm changed\") }}\n\
                         \x20   assert_eq!({main}.applySum(scalar.clone(), 43), 97);\n\
                         \x20   match {main}.echoSum(scalar) {{ {shapes}::Sum::Right(value) => assert_eq!(value, 97), _ => panic!(\"scalar sum arm changed\") }}\n\
                         \x20   assert_eq!(product_calls.get(), 2);\n\
                         \x20   assert_eq!(sum_calls.get(), 2);\n\
                         \x20   println!(\"callable slots ok\");\n"
                    )
                }
                ExportDriver::ScalarRoundtrip => {
                    let main = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg".to_owned(),
                    };
                    format!(
                        "    let signed = -1208925819614629174706299_i128;\n\
                         \x20   let unsigned = 2417851639229258349412391_u128;\n\
                         \x20   assert_eq!({main}.echoI128(signed), signed);\n\
                         \x20   assert_eq!({main}.echoU128(unsigned), unsigned);\n\
                         \x20   for value in [1.5_f32, -2.25_f32] {{ assert_eq!({main}.echoF32(value), value); }}\n\
                         \x20   for value in [1.0000000000000002_f64, -3.125_f64] {{ assert_eq!({main}.echoF64(value), value); }}\n\
                         \x20   println!(\"scalar payloads ok\");\n"
                    )
                }
                ExportDriver::HostOwnedRoundtrip => {
                    let main = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg".to_owned(),
                    };
                    let box_type = format!("{crate_name}::shapes::KioHostType_testapi__Box");
                    let native = format!("{crate_name}::shapes::KioNative");
                    let value = format!("{crate_name}::shapes::KioValue");
                    format!(
                        "    for token in [7_i32, 19_i32] {{ assert_eq!({main}.echoToken(token), token); }}\n\
                         \x20   let integer = {box_type}::<StubHost, {native}<i32>>::from_storage(__BoxStorage(::std::rc::Rc::new({value}::<{native}<i32>>::pack(42).into_stored())));\n\
                         \x20   let integer = {main}.echoBox::<{native}<i32>>(integer).into_storage();\n\
                         \x20   assert_eq!({value}::<{native}<i32>>::from_stored(integer.0.as_ref().clone()).unpack(), 42);\n\
                         \x20   let text = {box_type}::<StubHost, {native}<String>>::from_storage(__BoxStorage(::std::rc::Rc::new({value}::<{native}<String>>::pack(\"box-value\".to_owned()).into_stored())));\n\
                         \x20   let text = {main}.echoBox::<{native}<String>>(text).into_storage();\n\
                         \x20   assert_eq!({value}::<{native}<String>>::from_stored(text.0.as_ref().clone()).unpack(), \"box-value\");\n\
                         \x20   println!(\"host-owned payloads ok\");\n"
                    )
                }
                ExportDriver::StructuralRoundtrip => {
                    // `pair_swap` / `dispatch_left` are exported in `testapi/main`,
                    // reached at `pkg.testapi.main.*`. `pair_swap` takes / returns
                    // a `(I32 & String)` product; `dispatch_left` takes a
                    // `I32 | String` sum. Name both through their `exp` aliases —
                    // the product alias is a struct the driver fills with
                    // positional `_0` / `_1` fields; the sum is built through the
                    // canonical `Left` / `Right` variants through the `arg0`
                    // type alias. Wider products and sums are right-nested.
                    let main_ns = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg".to_owned(),
                    };
                    let wide_alias =
                        format!("{crate_name}::ffi::exp::testapi_main__rotate::arg0::<StubHost>");
                    let product = format!("{crate_name}::shapes::Product");
                    let wide = right_nested_product_literal(
                        &wide_alias,
                        &product,
                        &(1..=12).map(|value| value.to_string()).collect::<Vec<_>>(),
                    );
                    let rotated_0 = right_nested_product_access("rotated", 0, 12);
                    let rotated_1 = right_nested_product_access("rotated", 1, 12);
                    let rotated_11 = right_nested_product_access("rotated", 11, 12);
                    let classify_alias =
                        format!("{crate_name}::ffi::exp::testapi_main__classify::arg0::<StubHost>");
                    let sum = format!("{crate_name}::shapes::Sum");
                    let classify_0 = right_nested_sum_literal(&classify_alias, &sum, 0, 10, "1i8");
                    let classify_4 = right_nested_sum_literal(&classify_alias, &sum, 4, 10, "5u8");
                    let classify_9 = right_nested_sum_literal(
                        &classify_alias,
                        &sum,
                        9,
                        10,
                        "\"ten\".to_string()",
                    );
                    let samples = [
                        (0, "-101i8"),
                        (1, "-12345i16"),
                        (2, "-123456789i32"),
                        (3, "-9007199254740993i64"),
                        (4, "201u8"),
                        (5, "54321u16"),
                        (6, "3456789012u32"),
                        (7, "18014398509481987u64"),
                        (8, "false"),
                        (8, "true"),
                        (9, "\"sum-value\".to_string()"),
                    ]
                    .into_iter()
                    .map(|(arm, value)| {
                        right_nested_sum_literal(&classify_alias, &sum, arm, 10, value)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                    let payload_arms = (0..10)
                        .map(|arm| {
                            let pattern =
                                right_nested_sum_literal(&classify_alias, &sum, arm, 10, "value");
                            format!("            {pattern} => value.to_string(),\n")
                        })
                        .collect::<String>();
                    format!(
                        "    let p = {crate_name}::ffi::exp::testapi_main__pairSwap::arg0::<StubHost> {{ _0: 42, _1: \"hello\".to_string() }};\n\
                 \x20   let q = {main_ns}.pairSwap(p);\n\
                 \x20   println!(\"{{}} {{}}\", q._0, q._1);\n\
                 \x20   println!(\"{{}}\", {main_ns}.dispatchLeft({crate_name}::ffi::exp::testapi_main__dispatchLeft::arg0::<StubHost>::Left(7)));\n\
                 \x20   println!(\"{{}}\", {main_ns}.dispatchLeft({crate_name}::ffi::exp::testapi_main__dispatchLeft::arg0::<StubHost>::Right(\"from-sum\".to_string())));\n\
                 \x20   let wide = {wide};\n\
                 \x20   let rotated = {main_ns}.rotate(wide);\n\
                 \x20   println!(\"{{}} {{}} {{}}\", {rotated_0}, {rotated_1}, {rotated_11});\n\
                 \x20   println!(\"{{}}\", {main_ns}.classify({classify_0}));\n\
                 \x20   println!(\"{{}}\", {main_ns}.classify({classify_4}));\n\
                 \x20   println!(\"{{}}\", {main_ns}.classify({classify_9}));\n\
                 \x20   let first = {main_ns}.chooseFirst();\n\
                 \x20   println!(\"{{}}\", {main_ns}.classify(first));\n\
                 \x20   let middle = {main_ns}.chooseMiddle();\n\
                 \x20   println!(\"{{}}\", {main_ns}.classify(middle));\n\
                 \x20   let last = {main_ns}.chooseLast();\n\
                 \x20   println!(\"{{}}\", {main_ns}.classify(last));\n\
                 \x20   for sample in [{samples}] {{\n\
                 \x20       let returned = {main_ns}.echoSum(sample);\n\
                 \x20       let payload = match &returned {{\n{payload_arms}\
                 \x20       }};\n\
                 \x20       println!(\"{{}} {{}}\", {main_ns}.classify(returned), payload);\n\
                 \x20   }}\n"
                    )
                }
                ExportDriver::PositionalProductRoundtrip => {
                    // Flat (not testapi-conformed) golden. `make_pair(I32,
                    // String) -> I32 & String` is exported at the bulk-flat
                    // `pkg.main`. Call it and read the returned product back
                    // through its positional `_0` / `_1` fields — the
                    // spec-mandated structural-product FFI field names. The
                    // return type is named through the `exp::main__make_pair::ret`
                    // alias so the runner references the exact generic shell
                    // without re-deriving its semantic identity.
                    let main_ns = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg.main".to_owned(),
                    };
                    format!(
                        "    let q: {crate_name}::ffi::exp::main__makePair::ret<StubHost> = {main_ns}.makePair(7, \"hello\".to_string());\n\
                 \x20   println!(\"{{}} {{}}\", q._0, q._1);\n"
                    )
                }
                ExportDriver::NewtypeSumRoundtrip => {
                    // `pack(I32, String) -> Tagged` (Out) and `first_or(I32,
                    // Tagged) -> I32` (In) exported in `testapi/main`, where
                    // `Tagged = Pr | .` and `Pr` wraps `(I32 & String)`. Chaining
                    // them round-trips a sum-arm-over-a-newtype-over-a-product
                    // across the boundary; the recovered first field prints `7`.
                    let main_ns = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg.main".to_owned(),
                    };
                    format!(
                        "    println!(\"{{}}\", {main_ns}.firstOr(0, {main_ns}.pack(7, \"hi\".to_string())));\n"
                    )
                }
                ExportDriver::CurriedFacade => {
                    let main_ns = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg.main".to_owned(),
                    };
                    format!(
                        "    println!(\"{{}}\", {main_ns}.pick(\"ku\".to_string(), \"rz\".to_string()));\n\
                 \x20   println!(\"{{}}\", {main_ns}.last(1, 2, 3));\n"
                    )
                }
                // `apply_via[K][R](f: K -> R, x: K) -> R` in `testapi/main` keeps
                // `K` / `R` as real generics on the Rust facade, so passing a host
                // closure exercises the boundary wrapper's erase/downcast threading
                // at two driver-chosen instantiations.
                ExportDriver::PolyCallbackRoundtrip => {
                    let main_ns = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg.main".to_owned(),
                    };
                    format!(
                        "    let string_step = {crate_name}::shapes::KioFn1::<{crate_name}::shapes::KioNative<String>, {crate_name}::shapes::KioNative<String>>::new(|s| format!(\"via: {{s}}\"));\n\
                 \x20   println!(\"{{}}\", {main_ns}.applyVia::<{crate_name}::shapes::KioNative<String>, {crate_name}::shapes::KioNative<String>>(string_step, \"apply\".to_string()));\n\
                 \x20   let int_step = {crate_name}::shapes::KioFn1::<{crate_name}::shapes::KioNative<i32>, {crate_name}::shapes::KioNative<i32>>::new(|n| n + 8);\n\
                 \x20   println!(\"{{}}\", {main_ns}.applyVia::<{crate_name}::shapes::KioNative<i32>, {crate_name}::shapes::KioNative<i32>>(int_step, 7));\n"
                    )
                }
                ExportDriver::NestedCurriedRoundtrip => {
                    let api = match &export_root {
                        Some(r) => format!("pkg.{r}.api"),
                        None => "pkg.api".to_owned(),
                    };
                    format!(
                        "    let join = {crate_name}::ffi::exp::testapi_api__viaHost::arg0::<StubHost>::new(|left| {{\n\
                                 {crate_name}::ffi::exp::testapi_api__viaHost::arg0_cbret::<StubHost>::new(move |right| format!(\"{{left}}/{{right}}\"))\n\
                             }});\n\
                             let via_host = {api}.viaHost(join.clone());\n\
                             let via_host_second = via_host.call(\"env-left\".to_string());\n\
                             println!(\"via host: {{}}\", via_host_second.call(\"env-right\".to_string()));\n\
                             let round_export = {api}.roundExport(join);\n\
                             let round_export_second = round_export.call(\"export-left\".to_string());\n\
                             println!(\"round export: {{}}\", round_export_second.call(\"export-right\".to_string()));\n"
                    )
                }
                ExportDriver::HostSubstitutedUnitCallback => {
                    let api = match &export_root {
                        Some(r) => format!("pkg.{r}.api"),
                        None => "pkg.api".to_owned(),
                    };
                    format!(
                        "    let callback = {crate_name}::ffi::exp::testapi_api__viaHost::arg0::<StubHost>::new(|()| \"callback\".to_string());\n\
                         \x20   println!(\"{{}}\", {api}.viaHost(callback));\n"
                    )
                }
                ExportDriver::ReturnedForallCallByValue => {
                    let main_ns = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg.main".to_owned(),
                    };
                    format!(
                        "    {main_ns}.main();\n\
                         \x20   match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {main_ns}.main())) {{\n\
                         \x20       Err(payload) if payload.downcast_ref::<&str>() == Some(&\"produce failed\") => println!(\"caught\"),\n\
                         \x20       Err(payload) => ::std::panic::resume_unwind(payload),\n\
                         \x20       Ok(()) => panic!(\"produce did not panic\"),\n\
                         \x20   }}\n"
                    )
                }
                ExportDriver::FacadeSelectorCollisions => {
                    let root = match &export_root {
                        Some(r) => format!("pkg.{r}"),
                        None => "pkg".to_owned(),
                    };
                    let api = format!("{root}.api");
                    format!(
                        "    {api}.pkg();\n\
                             {api}.value();\n\
                             {root}.host.value();\n\
                             {root}.modApiValue.value();\n\
                             println!(\"{{}}\", {root}.foo.bar.value(9, 1));\n\
                             println!(\"{{}}\", {root}.fooBar.value(10, 2));\n\
                             println!(\"{{}}\", {root}.i.value(41, 1));\n\
                             {api}.child();\n\
                             println!(\"api.child function\");\n\
                             {api}.child.value();\n\
                             println!(\"api/child module\");\n\
                             let child = {crate_name}::shapes::nominal::testapi::api::Child::<StubHost>::makeChild(30);\n\
                             {crate_name}::shapes::nominal::testapi::api::Child::<StubHost>::readChild(child);\n\
                             println!(\"api.Child type\");\n"
                    )
                }
                ExportDriver::ModuleAliasScopeCollision => {
                    let root = export_root.unwrap_or("");
                    let a = if root.is_empty() {
                        "pkg.a".to_owned()
                    } else {
                        format!("pkg.{root}.a")
                    };
                    let b = if root.is_empty() {
                        "pkg.b".to_owned()
                    } else {
                        format!("pkg.{root}.b")
                    };
                    format!(
                        "    let a_value = {a}.make();\n\
                             {a}.consume(a_value);\n\
                             let b_value = {b}.make();\n\
                             {b}.consume(b_value);\n"
                    )
                }
                ExportDriver::NewtypeScalarRoundtrip => {
                    // `bump(Wrap) -> Wrap` exported in `testapi/main`, where
                    // `Wrap` is a bare scalar-payload newtype (`newtype Wrap :
                    // I32`). Build a `Wrap` through its public ctor, round-trip it
                    // through the export, and read the payload back through its
                    // public projector — the value survives both the In (param)
                    // and Out (return) newtype conversions across the FFI.
                    let main_ns = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg.main".to_owned(),
                    };
                    format!(
                        "    let w = {crate_name}::shapes::nominal::testapi::main::Wrap::<StubHost>::mkWrap(7);\n\
                 \x20   let out = {main_ns}.bump(w);\n\
                 \x20   println!(\"{{}}\", {crate_name}::shapes::nominal::testapi::main::Wrap::<StubHost>::unWrap(out));\n"
                    )
                }
                ExportDriver::NewtypeIgnoredArgumentRoundtrip => {
                    let main_ns = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg.main".to_owned(),
                    };
                    format!("    println!(\"{{}}\", {main_ns}.toI32({main_ns}.fromI32(7)));\n")
                }
                ExportDriver::RecursiveNewtypeBoundary => {
                    let main_ns = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg.main".to_owned(),
                    };
                    let root = format!("{crate_name}::shapes::nominal::main::Root::<StubHost>");
                    format!(
                        "    let payload = {main_ns}.basePayload();\n\
                         \x20   let root = {root}::makeRoot(payload);\n\
                         \x20   let kept = {main_ns}.keep(root);\n\
                         \x20   let projected = {root}::readRoot(kept);\n\
                         \x20   println!(\"{{}}\", {main_ns}.acceptPayload(projected));\n"
                    )
                }
                ExportDriver::NewtypeVisibilityFacade => {
                    let mut body = format!(
                        "    let input_a: SelectedI32 = SelectedI32(11);\n    \
             let a: {crate_name}::shapes::nominal::testapi::types::OpaqueA<StubHost> = pkg.testapi.types.makeA(input_a);\n    \
             let out_a: SelectedI32 = pkg.testapi.types.readA(a);\n    \
             println!(\"{{}}\", out_a.0);\n    \
             let input_b: SelectedI32 = SelectedI32(22);\n    \
             let b: {crate_name}::shapes::nominal::testapi::types::OpaqueB<StubHost> = pkg.testapi.types.makeB(input_b);\n    \
             let out_b: SelectedI32 = pkg.testapi.types.readB(b);\n    \
             println!(\"{{}}\", out_b.0);\n    \
             let c: {crate_name}::shapes::nominal::testapi::types::ConstructorOnly<StubHost> = {crate_name}::shapes::nominal::testapi::types::ConstructorOnly::<StubHost>::makeConstructorOnly(SelectedI32(33));\n    \
             let out_c: SelectedI32 = pkg.testapi.types.readConstructorOnlyValue(c);\n    \
             println!(\"{{}}\", out_c.0);\n    \
             let p: {crate_name}::shapes::nominal::testapi::types::ProjectorOnly<StubHost> = pkg.testapi.types.makeProjectorOnlyValue(SelectedI32(44));\n    \
             let out_p: SelectedI32 = {crate_name}::shapes::nominal::testapi::types::ProjectorOnly::<StubHost>::readProjectorOnly(p);\n    \
             println!(\"{{}}\", out_p.0);\n    \
             let both: {crate_name}::shapes::nominal::testapi::types::BothPublic<StubHost> = {crate_name}::shapes::nominal::testapi::types::BothPublic::<StubHost>::makeBothPublic(SelectedI32(55));\n    \
             let out_both: SelectedI32 = {crate_name}::shapes::nominal::testapi::types::BothPublic::<StubHost>::readBothPublic(both);\n    \
             println!(\"{{}}\", out_both.0);\n    \
             let left = pkg.testapi.left.make(SelectedI32(66));\n    \
             let left_payload: SelectedI32 = pkg.testapi.left.read(left);\n    \
             let left_reboxed = {crate_name}::shapes::nominal::testapi::left::Shared::<StubHost>::makeShared(left_payload);\n    \
             let out_left: SelectedI32 = {crate_name}::shapes::nominal::testapi::left::Shared::<StubHost>::readShared(left_reboxed);\n    \
             println!(\"{{}}\", out_left.0);\n    \
             let right = pkg.testapi.right.make(SelectedI32(77));\n    \
             let right_payload: SelectedI32 = pkg.testapi.right.read(right);\n    \
             let right_reboxed = {crate_name}::shapes::nominal::testapi::right::Shared::<StubHost>::makeShared(right_payload);\n    \
             let out_right: SelectedI32 = {crate_name}::shapes::nominal::testapi::right::Shared::<StubHost>::readShared(right_reboxed);\n    \
             println!(\"{{}}\", out_right.0);\n    \
             let constructor_pair = {crate_name}::shapes::nominal::testapi::types::ConstructorPair::<StubHost>::makeConstructorPair({crate_name}::shapes::Product {{ _0: SelectedI32(81), _1: SelectedI32(82) }});\n    \
             let constructor_pair_out = pkg.testapi.types.readConstructorPairValue(constructor_pair);\n    \
             println!(\"{{}} {{}}\", constructor_pair_out._0.0, constructor_pair_out._1.0);\n    \
             let projector_pair = pkg.testapi.types.makeProjectorPairValue({crate_name}::shapes::Product {{ _0: constructor_pair_out._0.clone(), _1: constructor_pair_out._1.clone() }});\n    \
             let projector_pair_out = {crate_name}::shapes::nominal::testapi::types::ProjectorPair::<StubHost>::readProjectorPair(projector_pair);\n    \
             println!(\"{{}} {{}}\", projector_pair_out._0.0, projector_pair_out._1.0);\n    \
             let constructor_generic = {crate_name}::shapes::nominal::testapi::types::ConstructorGeneric::<StubHost, {crate_name}::shapes::KioNative<SelectedI32>>::makeConstructorGeneric(SelectedI32(85));\n    \
             let constructor_generic_out: SelectedI32 = pkg.testapi.types.readConstructorGenericValue(constructor_generic);\n    \
             println!(\"{{}}\", constructor_generic_out.0);\n    \
             let projector_generic = pkg.testapi.types.makeProjectorGenericValue(SelectedI32(86));\n    \
             let projector_generic_out: SelectedI32 = {crate_name}::shapes::nominal::testapi::types::ProjectorGeneric::<StubHost, {crate_name}::shapes::KioNative<SelectedI32>>::readProjectorGeneric(projector_generic);\n    \
             println!(\"{{}}\", projector_generic_out.0);\n    \
             let packed_fn = {crate_name}::ffi::exp::testapi_types__PackedFunction::makePackedFunction_arg0::<StubHost>::new(|args| SelectedI32(args._0.0 + args._1.0));\n    \
             let packed = {crate_name}::shapes::nominal::testapi::types::PackedFunction::<StubHost>::makePackedFunction(packed_fn);\n    \
             let unpacked = {crate_name}::shapes::nominal::testapi::types::PackedFunction::<StubHost>::readPackedFunction(packed);\n    \
             println!(\"{{}}\", unpacked.call({crate_name}::shapes::Product {{ _0: constructor_pair_out._0, _1: constructor_pair_out._1 }}).0);\n    \
             struct Select89;\n    \
             impl {crate_name}::ffi::exp::testapi_types__ExistentialUnit::readExistentialUnit_continuation_impl<StubHost, {crate_name}::shapes::KioNative<i32>> for Select89 {{\n    \
                 fn apply<__KioHidden: {crate_name}::shapes::KioType>(&self, _value: <__KioHidden as {crate_name}::shapes::KioType>::Facade) -> i32 {{ 89 }}\n    \
             }}\n    \
             let existential = pkg.testapi.types.makeExistentialUnitValue();\n    \
             let select89 = {crate_name}::ffi::exp::testapi_types__ExistentialUnit::readExistentialUnit_continuation::<StubHost, {crate_name}::shapes::KioNative<i32>>::new(Select89);\n    \
             let existential_out: i32 = {crate_name}::shapes::nominal::testapi::types::ExistentialUnit::<StubHost>::readExistentialUnit(existential, select89);\n    \
             println!(\"{{}}\", existential_out);\n    \
             struct Select90;\n    \
             impl {crate_name}::ffi::exp::testapi_types__ExistentialEmpty::readExistentialEmpty_continuation_impl<StubHost, {crate_name}::shapes::KioNative<i32>> for Select90 {{\n    \
                 fn apply<__KioHidden: {crate_name}::shapes::KioType>(&self) -> i32 {{ 90 }}\n    \
             }}\n    \
             let existential_empty = pkg.testapi.types.makeExistentialEmptyValue();\n    \
             let select90 = {crate_name}::ffi::exp::testapi_types__ExistentialEmpty::readExistentialEmpty_continuation::<StubHost, {crate_name}::shapes::KioNative<i32>>::new(Select90);\n    \
             let existential_empty_out: i32 = {crate_name}::shapes::nominal::testapi::types::ExistentialEmpty::<StubHost>::readExistentialEmpty(existential_empty, select90);\n    \
             println!(\"{{}}\", existential_empty_out);\n    \
             let recursive_both = {crate_name}::shapes::nominal::testapi::types::RecursiveBoth::<StubHost>::makeRecursiveBoth(pkg.testapi.types.recursiveBothBasePayload());\n    \
             println!(\"{{}}\", pkg.testapi.types.recursiveBothPayloadIsBase({crate_name}::shapes::nominal::testapi::types::RecursiveBoth::<StubHost>::readRecursiveBoth(recursive_both)).0);\n    \
             let recursive_constructor = {crate_name}::shapes::nominal::testapi::types::RecursiveConstructor::<StubHost>::makeRecursiveConstructor(pkg.testapi.types.recursiveConstructorBasePayload());\n    \
             println!(\"{{}}\", pkg.testapi.types.recursiveConstructorPayloadIsBase(pkg.testapi.types.readRecursiveConstructorValue(recursive_constructor)).0);\n    \
             let recursive_projector = pkg.testapi.types.makeRecursiveProjectorBase();\n    \
             println!(\"{{}}\", pkg.testapi.types.recursiveProjectorPayloadIsBase({crate_name}::shapes::nominal::testapi::types::RecursiveProjector::<StubHost>::readRecursiveProjector(recursive_projector)).0);\n"
                    );
                    body.push_str(&format!(
                        r#"    use {crate_name}::shapes::{{KioNative, KioType, KioUnit, Product}};
    use {crate_name}::ffi::exp::testapi_types__ProjectorSpread::readProjectorSpread_ret_impl as _;
    struct Spread(::std::rc::Rc<::std::cell::RefCell<Vec<i32>>>);
    impl {crate_name}::ffi::exp::testapi_types__ConstructorSpread::makeConstructorSpread_arg0_impl<StubHost> for Spread {{
        fn apply<A: KioType>(&self, value: Product<SelectedI32, A::Facade>) -> SelectedI32 {{ self.0.borrow_mut().push(value._0.0); SelectedI32(value._0.0 + 3) }}
    }}
    let inputs = ::std::rc::Rc::new(::std::cell::RefCell::new(Vec::new()));
    let callback = {crate_name}::ffi::exp::testapi_types__ConstructorSpread::makeConstructorSpread_arg0::<StubHost>::new(Spread(inputs.clone()));
    let constructed = {crate_name}::shapes::nominal::testapi::types::ConstructorSpread::<StubHost>::makeConstructorSpread(callback);
    assert_eq!(pkg.testapi.types.invokeConstructorSpreadI32(constructed.clone(), SelectedI32(101)).0, 104);
    assert_eq!(pkg.testapi.types.invokeConstructorSpreadUnit(constructed, SelectedI32(102)).0, 105);
    assert_eq!(*inputs.borrow(), [101, 102]);
    let projected = {crate_name}::shapes::nominal::testapi::types::ProjectorSpread::<StubHost>::readProjectorSpread(pkg.testapi.types.makeProjectorSpreadValue());
    assert_eq!(projected.apply::<KioNative<SelectedI32>>(Product {{ _0: SelectedI32(111), _1: SelectedI32(1) }}).0, 111);
    assert_eq!(projected.apply::<KioUnit>(Product {{ _0: SelectedI32(112), _1: () }}).0, 112);
    struct OpenSpread(::std::rc::Rc<::std::cell::Cell<usize>>);
    impl {crate_name}::ffi::exp::testapi_types__ExistentialSpread::readExistentialSpread_continuation_impl<StubHost, KioNative<i32>> for OpenSpread {{
        fn apply<U: KioType>(&self, _value: {crate_name}::ffi::exp::testapi_types__ExistentialSpread::readExistentialSpread_continuation_cbarg<StubHost, KioNative<i32>, U>) -> i32 {{ self.0.set(self.0.get() + 1); 91 }}
    }}
    struct OpenRecursive(::std::rc::Rc<::std::cell::Cell<usize>>);
    impl {crate_name}::ffi::exp::testapi_types__RecursiveExistentialFunction::readRecursiveExistentialFunction_continuation_impl<StubHost, KioNative<i32>> for OpenRecursive {{
        fn apply<U: KioType>(&self, _value: {crate_name}::ffi::exp::testapi_types__RecursiveExistentialFunction::readRecursiveExistentialFunction_continuation_cbarg<StubHost, U>) -> i32 {{ self.0.set(self.0.get() + 1); 92 }}
    }}
    let spread_opens = ::std::rc::Rc::new(::std::cell::Cell::new(0));
    let recursive_opens = ::std::rc::Rc::new(::std::cell::Cell::new(0));
    let spread_continuation = {crate_name}::ffi::exp::testapi_types__ExistentialSpread::readExistentialSpread_continuation::<StubHost, KioNative<i32>>::new(OpenSpread(spread_opens.clone()));
    let recursive_continuation = {crate_name}::ffi::exp::testapi_types__RecursiveExistentialFunction::readRecursiveExistentialFunction_continuation::<StubHost, KioNative<i32>>::new(OpenRecursive(recursive_opens.clone()));
    assert_eq!({crate_name}::shapes::nominal::testapi::types::ExistentialSpread::<StubHost>::readExistentialSpread(pkg.testapi.types.makeExistentialSpreadValue(), spread_continuation), 91);
    assert_eq!({crate_name}::shapes::nominal::testapi::types::RecursiveExistentialFunction::<StubHost>::readRecursiveExistentialFunction(pkg.testapi.types.makeRecursiveExistentialFunctionValue(), recursive_continuation), 92);
    assert_eq!(spread_opens.get(), 1);
    assert_eq!(recursive_opens.get(), 1);
"#
                    ));
                    body
                }
                ExportDriver::HostExistentialRoundtrip =>
                    "    let result = pkg.testapi.main.exercise();\n    assert_eq!((result._0, result._1), (37, 83));\n    EXISTENTIAL_COUNTS.with(|counts| assert_eq!(counts.get(), (2, 2)));\n    println!(\"existential host opening ok\");\n"
                .to_owned(),
                ExportDriver::PublicWordNames => format!(
                    "    println!(\"{{}}\", pkg.wordApi.readWord());\n    \
             println!(\"{{}}\", pkg.wordApi._readWord());\n    \
             println!(\"{{}}\", pkg.wordApi.readWord_());\n    \
             println!(\"{{}}\", pkg.wordApi._readWord_());\n    \
             println!(\"{{}}\", pkg.wordApi.readWord__());\n    \
             let boxed = {crate_name}::shapes::nominal::wordApi::wordNodes::_WordBox__::<StubHost, {crate_name}::shapes::KioNative<i32>>::wrapWord(55);\n    \
             println!(\"{{}}\", {crate_name}::shapes::nominal::wordApi::wordNodes::_WordBox__::<StubHost, {crate_name}::shapes::KioNative<i32>>::unwrapWord(boxed));\n    \
             println!(\"{{}}\", pkg.wordApi.wordNodes.keepWord::<{crate_name}::shapes::KioNative<i32>>(66));\n    \
             type CountValue = {crate_name}::shapes::KioHostTypeMarker_wordApi__CountValue<StubHost>;\n    \
             let pair = pkg.wordApi.keepPair({crate_name}::shapes::Product {{ _0: {crate_name}::shapes::nominal::wordApi::wordNodes::_WordBox__::<StubHost, CountValue>::wrapWord(77), _1: {crate_name}::shapes::nominal::wordApi::otherNodes::_WordBox__::<StubHost, CountValue>::wrapWord(88) }});\n    \
             println!(\"{{}}\", {crate_name}::shapes::nominal::wordApi::wordNodes::_WordBox__::<StubHost, CountValue>::unwrapWord(pair._0));\n    \
             println!(\"{{}}\", {crate_name}::shapes::nominal::wordApi::otherNodes::_WordBox__::<StubHost, CountValue>::unwrapWord(pair._1));\n"
                ),
                ExportDriver::CompoundInputOnce => format!(
                    r#"    let direct = pkg.testapi.main.direct();
    println!("{{}}\n{{}}", direct._0, direct._1);
    let callback = {crate_name}::ffi::exp::testapi_main__callback::arg0::<StubHost>::new(|| {{
        println!("callback");
        {crate_name}::shapes::Product {{ _0: 9, _1: "callback-value".to_string() }}
    }});
    let result = pkg.testapi.main.callback(callback);
    println!("{{}}\n{{}}", result._0, result._1);
    let outer = pkg.testapi.main.echoOuter(pkg.testapi.main.makeOuter("nest".to_string(), 11, 13));
    let outer = {crate_name}::shapes::nominal::testapi::main::Outer::<StubHost>::unOuter(outer);
    let inner = {crate_name}::shapes::nominal::testapi::main::Inner::<StubHost>::unInner(outer._1);
    println!("{{}}\n{{}}\n{{}}", outer._0, inner._0, inner._1);
    for choice in [pkg.testapi.main.first(17), pkg.testapi.main.middle(19, 23), pkg.testapi.main.last("choice".to_string(), 29, 31)] {{
        let choice = {crate_name}::shapes::nominal::testapi::main::Choice::<StubHost>::unChoice(pkg.testapi.main.echoChoice(choice));
        match choice {{
            {crate_name}::shapes::Sum::Left(value) => println!("{{}}", value),
            {crate_name}::shapes::Sum::Right({crate_name}::shapes::Sum::Left(value)) => {{
                let inner = {crate_name}::shapes::nominal::testapi::main::Inner::<StubHost>::unInner(value);
                println!("{{}}\n{{}}", inner._0, inner._1);
            }}
            {crate_name}::shapes::Sum::Right({crate_name}::shapes::Sum::Right(value)) => {{
                let outer = {crate_name}::shapes::nominal::testapi::main::Outer::<StubHost>::unOuter(value);
                let inner = {crate_name}::shapes::nominal::testapi::main::Inner::<StubHost>::unInner(outer._1);
                println!("{{}}\n{{}}\n{{}}", outer._0, inner._0, inner._1);
            }}
        }}
    }}
    println!("{{}}", pkg.testapi.main.echoText("atomic".to_string()));
"#
                ),
                ExportDriver::NestedProductRoundtrip => {
                    // `make(String, I32, I32) -> Outer` exported in
                    // `testapi/main`, where the label-minted `Inner` wraps
                    // `(I32 & I32)` and `Outer` wraps `(String & Inner)`. The
                    // return is named through the `exp::testapi_main__make::ret`
                    // alias; each newtype's payload product is recovered through
                    // its emitted `get` projector and read off the canonical
                    // `Product` facade's `_0` / `_1` fields. Each product nesting
                    // level opens its own binding scope in the Out conversion —
                    // the nested-binder regression shape.
                    let main_ns = match &export_root {
                        Some(r) => format!("pkg.{r}.main"),
                        None => "pkg.main".to_owned(),
                    };
                    format!(
                        "    let o: {crate_name}::ffi::exp::testapi_main__make::ret<StubHost> = {main_ns}.make(\"nest\".to_string(), 7, 9);\n\
                 \x20   let p = {crate_name}::shapes::nominal::testapi::main::Outer::<StubHost>::get(o);\n\
                 \x20   println!(\"{{}}\", p._0);\n\
                 \x20   let q = {crate_name}::shapes::nominal::testapi::main::Inner::<StubHost>::get(p._1);\n\
                 \x20   println!(\"{{}}\", q._0);\n\
                 \x20   println!(\"{{}}\", q._1);\n"
                    )
                }
                ExportDriver::TypeRoundtrip => {
                    // The `Pair` type is exported by its kio name (a stable
                    // identifier, not a structural hash). Its exact prepared
                    // constructor takes the single written product parameter,
                    // built through its exact emitted FFI alias; the projector
                    // returns the same canonical binary product facade. `Pair`
                    // lives in `testapi/types`.
                    format!(
                        "    type Pair = {crate_name}::shapes::nominal::testapi::types::Pair<StubHost, {crate_name}::shapes::KioNative<String>, {crate_name}::shapes::KioNative<String>>;\n\
                 \x20   let boxed = Pair::mkPair({crate_name}::ffi::exp::testapi_types__Pair::mkPair_arg0::<{crate_name}::shapes::KioNative<String>, {crate_name}::shapes::KioNative<String>> {{ _0: \"export-type-left\".to_string(), _1: \"export-type-right\".to_string() }});\n\
                 \x20   let out = Pair::unPair(boxed);\n\
                 \x20   println!(\"{{}}\", out._0);\n\
                 \x20   println!(\"{{}}\", out._1);\n"
                    )
                }
                ExportDriver::WideCallable => {
                    let main_ns = match &export_root {
                        Some(root) => format!("pkg.{root}.main"),
                        None => "pkg.main".to_owned(),
                    };
                    let values = (0..WIDE_CALLABLE_SLOT_COUNT)
                        .map(|value| value.to_string())
                        .collect::<Vec<_>>();
                    let arguments = values.join(", ");
                    let product = format!("{crate_name}::shapes::Product");
                    let callback_argument =
                        right_nested_product_literal(&product, &product, &values);
                    format!(
                        "    let selected = {main_ns}.select({arguments});\n\
                         \x20   println!(\"{{}}\", selected._0);\n\
                         \x20   println!(\"{{}}\", selected._1._0);\n\
                         \x20   println!(\"{{}}\", selected._1._1);\n\
                         \x20   let callback_selected = {main_ns}.makeSelect().call({callback_argument});\n\
                         \x20   println!(\"{{}}\", callback_selected._0);\n\
                         \x20   println!(\"{{}}\", callback_selected._1._0);\n\
                         \x20   println!(\"{{}}\", callback_selected._1._1);\n"
                    )
                }
            }
        }
    };
    Ok(build_driver(crate_name, host, &main_body, protocol))
}

fn rust_main_call(module: &str) -> String {
    format!("    pkg.{}.main();\n", module.replace('/', "."))
}

/// Build a `src/main.rs` driver for the emitted crate. The driver
/// links to the crate's `[lib]`, defines a `StubHost`, supplies the
/// protocol-owned associated-type fixtures and canonical method bodies, and
/// runs the supplied main body. An exact associated type for which the
/// operational protocol owns no fixture is deliberately left absent so rustc
/// rejects the incomplete host implementation.
fn build_driver(
    crate_name: &str,
    host: &HostApi,
    main_body: &str,
    protocol: RunnerProtocol,
) -> String {
    // Branded facade names, reconstructed from the crate name the same
    // way the emitter derives them (`specs/backends/rust.md` § Output
    // layout): the host contract is the handle + `Host`, the factory is
    // `create_<ident>`. The `host` module keeps its fixed name.
    let host_trait = format!("{}Host", pascal_case(crate_name));
    let factory = format!("create_{}", artifact_identity::value_brand(crate_name));
    let mut impl_body = String::new();
    // Track whether the runner's declaration-owned invariant storage needs to
    // be emitted (only when the package declares `host type Array[T]`).
    let mut needs_array_storage = false;
    let mut needs_box_storage = false;
    let mut needs_scalar_enum = false;
    let contract = protocol.contract();
    debug_assert_eq!(host.types.len(), contract.host_types.len());
    for (at, binding) in host.assoc_types().zip(contract.host_types) {
        let decl_name = at
            .boundary_name
            .as_deref()
            .expect("exact protocol host type must carry its Rust member");
        match binding.fixture {
            HostTypeFixture::Role(role) => {
                impl_body.push_str(&format!(
                    "    type {decl_name} = {};\n",
                    rust_native_for_fixture(role)
                ));
            }
            HostTypeFixture::SelectedRole(RoleFixture::I32) => {
                impl_body.push_str(&format!("    type {decl_name} = SelectedI32;\n"));
            }
            HostTypeFixture::SelectedRole(RoleFixture::String) => {
                impl_body.push_str(&format!("    type {decl_name} = SelectedString;\n"));
            }
            HostTypeFixture::SelectedRole(role) => {
                unreachable!(
                    "Rust runner has no distinct selected-role fixture for `{}`",
                    role.role()
                );
            }
            HostTypeFixture::Array => {
                assert_eq!(
                    at.type_params.len(),
                    1,
                    "Array protocol fixture must be unary"
                );
                needs_array_storage = true;
                impl_body.push_str(&format!("    type {decl_name}Storage = __ArrayStorage;\n",));
            }
            HostTypeFixture::Box => {
                assert_eq!(
                    at.type_params.len(),
                    1,
                    "Box protocol fixture must be unary"
                );
                needs_box_storage = true;
                impl_body.push_str(&format!("    type {decl_name}Storage = __BoxStorage;\n",));
            }
            HostTypeFixture::Token => {
                assert!(
                    at.type_params.is_empty(),
                    "Token protocol fixture must be nullary"
                );
                impl_body.push_str(&format!("    type {decl_name} = i32;\n"));
            }
            HostTypeFixture::Scalar => {
                assert!(
                    at.type_params.is_empty(),
                    "Scalar protocol fixture must be nullary"
                );
                // `dyn_load_prime`'s opaque `host type Scalar` — a tagged enum
                // over the scalar shapes it boxes (i32 / str / bool / f64).
                // Its `Clone + PartialEq` satisfy the host-type intent bounds.
                needs_scalar_enum = true;
                impl_body.push_str(&format!("    type {decl_name} = __Scalar;\n"));
            }
        }
    }
    if !host.types.is_empty() && !host.functions.is_empty() {
        impl_body.push('\n');
    }
    debug_assert_eq!(host.functions.len(), contract.host_fns.len());
    for (i, (m, binding)) in host.methods().zip(contract.host_fns).enumerate() {
        let sig = render_method_sig(m, crate_name);
        let body = rewrite_crate_paths(&render_rust_body_for_binding(binding, m), crate_name);
        impl_body.push_str(&format!("    {sig} {{\n        {body}\n    }}\n"));
        if i + 1 < host.functions.len() {
            impl_body.push('\n');
        }
    }

    // The associated storage is arity-independent. Applied element types stay
    // on the generated carrier, while each cell retains only opaque tokens.
    let array_storage_decl = if needs_array_storage {
        format!(
            "\n\
             struct __ArrayStorage(::std::rc::Rc<::std::cell::RefCell<Vec<{crate_name}::shapes::KioStoredValue>>>);\n\
             \n\
             impl Clone for __ArrayStorage {{\n\
             \x20   fn clone(&self) -> Self {{ __ArrayStorage(::std::rc::Rc::clone(&self.0)) }}\n\
             }}\n\
             \n\
             impl PartialEq for __ArrayStorage {{\n\
             \x20   fn eq(&self, other: &Self) -> bool {{ ::std::rc::Rc::ptr_eq(&self.0, &other.0) }}\n\
             }}\n\
             \n\
             impl __ArrayStorage {{\n\
             \x20   fn new(v: Vec<{crate_name}::shapes::KioStoredValue>) -> Self {{ __ArrayStorage(::std::rc::Rc::new(::std::cell::RefCell::new(v))) }}\n\
             }}\n"
        )
    } else {
        String::new()
    };

    let box_storage_decl = if needs_box_storage {
        format!(
            "\n\
             struct __BoxStorage(::std::rc::Rc<{crate_name}::shapes::KioStoredValue>);\n\
             \n\
             impl Clone for __BoxStorage {{\n\
             \x20   fn clone(&self) -> Self {{ __BoxStorage(::std::rc::Rc::clone(&self.0)) }}\n\
             }}\n\
             \n\
             impl PartialEq for __BoxStorage {{\n\
             \x20   fn eq(&self, other: &Self) -> bool {{ ::std::rc::Rc::ptr_eq(&self.0, &other.0) }}\n\
             }}\n"
        )
    } else {
        String::new()
    };

    // The backing for `dyn_load_prime`'s opaque `host type Scalar`: a tagged enum
    // over the scalar shapes it boxes. `Clone + PartialEq` satisfy the
    // host-type intent bounds (`Clone + PartialEq + 'static`). The `bool`
    // arm is what `scalar_is_true` reads; the `f64` arm uses bit-equality
    // for `PartialEq` (NaN never appears in the corpus, and reference-style
    // identity is not required for a scalar).
    let scalar_enum_decl = if needs_scalar_enum {
        "\n\
         #[derive(Clone)]\n\
         enum __Scalar { I32(i32), F64(f64), Str(String), Bool(bool) }\n\
         \n\
         impl PartialEq for __Scalar {\n\
         \x20   fn eq(&self, other: &Self) -> bool {\n\
         \x20       match (self, other) {\n\
         \x20           (__Scalar::I32(a), __Scalar::I32(b)) => a == b,\n\
         \x20           (__Scalar::F64(a), __Scalar::F64(b)) => a.to_bits() == b.to_bits(),\n\
         \x20           (__Scalar::Str(a), __Scalar::Str(b)) => a == b,\n\
         \x20           (__Scalar::Bool(a), __Scalar::Bool(b)) => a == b,\n\
         \x20           _ => false,\n\
         \x20       }\n\
         \x20   }\n\
         }\n"
        .to_owned()
    } else {
        String::new()
    };

    let read_ascii_line_decl = if contract
        .host_fns
        .iter()
        .any(|binding| matches!(binding.body, HostFnBodyKind::ReadAsciiLine { .. }))
    {
        "\n\
         fn __kio_read_next_line() -> Option<String> {\n\
         \x20   let mut line = String::new();\n\
         \x20   match ::std::io::stdin().read_line(&mut line) {\n\
         \x20       Ok(0) => None,\n\
         \x20       Ok(_) => {\n\
         \x20           if line.ends_with('\\n') {\n\
         \x20               line.pop();\n\
         \x20               if line.ends_with('\\r') {\n\
         \x20                   line.pop();\n\
         \x20               }\n\
         \x20           }\n\
         \x20           if !line.is_ascii() {\n\
         \x20               eprintln!(\"read_ascii_line: non-ASCII input\");\n\
         \x20               ::std::process::exit(1);\n\
         \x20           }\n\
         \x20           Some(line)\n\
         \x20       }\n\
         \x20       Err(e) => {\n\
         \x20           eprintln!(\"read_ascii_line: reading stdin: {}\", e);\n\
         \x20           ::std::process::exit(1);\n\
         \x20       }\n\
         \x20   }\n\
         }\n"
        .to_owned()
    } else {
        String::new()
    };

    let selected_i32_decl = if contract
        .host_types
        .iter()
        .any(|binding| binding.fixture == HostTypeFixture::SelectedRole(RoleFixture::I32))
    {
        "#[derive(Clone, PartialEq)]\n\
         struct SelectedI32(i32);\n\
         \n\
         impl From<i32> for SelectedI32 {\n\
         \x20   fn from(value: i32) -> Self { Self(value) }\n\
         }\n\
         \n\
         impl SelectedI32 {\n\
         \x20   fn wrapping_add(self, other: Self) -> Self { Self(self.0.wrapping_add(other.0)) }\n\
         }\n\
         \n\
         impl ::std::fmt::Display for SelectedI32 {\n\
         \x20   fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result { self.0.fmt(f) }\n\
         }\n"
    } else {
        ""
    };

    let selected_string_decl = if contract
        .host_types
        .iter()
        .any(|binding| binding.fixture == HostTypeFixture::SelectedRole(RoleFixture::String))
    {
        "#[derive(Clone, PartialEq)]\n\
         struct SelectedString(String);\n\
         \n\
         impl From<String> for SelectedString {\n\
         \x20   fn from(value: String) -> Self { Self(value) }\n\
         }\n\
         \n\
         impl ::std::ops::Deref for SelectedString {\n\
         \x20   type Target = str;\n\
         \x20   fn deref(&self) -> &str { &self.0 }\n\
         }\n\
         \n\
         impl ::std::fmt::Display for SelectedString {\n\
         \x20   fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result { self.0.fmt(f) }\n\
         }\n"
    } else {
        ""
    };

    let (stub_host_decl, stub_host_ctor) = if contract
        .host_fns
        .iter()
        .any(|binding| matches!(binding.body, HostFnBodyKind::ReturnedForallUnit))
    {
        (
            "struct StubHost { returned_forall_produced: ::std::rc::Rc<::std::cell::Cell<bool>>, }",
            "StubHost { returned_forall_produced: ::std::rc::Rc::new(::std::cell::Cell::new(false)) }",
        )
    } else {
        ("struct StubHost;", "StubHost")
    };
    let existential_state = if protocol == RunnerProtocol::HostExistentialRoundtrip {
        "thread_local! { static EXISTENTIAL_COUNTS: ::std::cell::Cell<(usize, usize)> = const { ::std::cell::Cell::new((0, 0)) }; }\n"
    } else {
        ""
    };
    let recursion_limit_attr = if protocol == RunnerProtocol::ExportWideCallable {
        "#![recursion_limit = \"512\"]\n"
    } else {
        ""
    };

    format!(
        "// Generated by kio-test-runner-rust — do not edit by hand.\n\
         //\n\
         // Thin driver that instantiates the emitted package against a\n\
         // StubHost synthesized from the selected exact protocol contract,\n\
         // then runs that contract's fixed export driver.\n\
         \n\
         {recursion_limit_attr}\
         #![allow(unused_variables)]\n\
         #![allow(unused_imports)]\n\
         #![allow(non_camel_case_types)]\n\
         \n\
         use {crate_name}::host::{host_trait};\n\
         {selected_i32_decl}\n\
         {selected_string_decl}\n\
         {array_storage_decl}\n\
         {box_storage_decl}\n\
         {scalar_enum_decl}\n\
         {read_ascii_line_decl}\n\
         #[derive(Clone)]\n\
         {stub_host_decl}\n\
         {existential_state}\n\
         \n\
         impl {host_trait} for StubHost {{\n\
         {impl_body}\
         }}\n\
         \n\
         fn main() {{\n\
         \x20   let pkg = {crate_name}::{factory}({stub_host_ctor});\n\
         {main_body}\
         }}\n"
    )
}

fn rust_native_for_fixture(role: RoleFixture) -> &'static str {
    match role {
        RoleFixture::Bool => "bool",
        RoleFixture::I8 => "i8",
        RoleFixture::I16 => "i16",
        RoleFixture::I32 => "i32",
        RoleFixture::I64 => "i64",
        RoleFixture::I128 => "i128",
        RoleFixture::U8 => "u8",
        RoleFixture::U16 => "u16",
        RoleFixture::U32 => "u32",
        RoleFixture::U64 => "u64",
        RoleFixture::U128 => "u128",
        RoleFixture::F32 => "f32",
        RoleFixture::F64 => "f64",
        RoleFixture::String => "String",
    }
}

/// Render a method signature for the impl block. Args are named
/// `arg0`, `arg1`, … (the emitter uses these names already; we
/// echo the convention so the canonical-body table can refer to
/// them positionally). The receiver is `&self`. Method-level kind-`*`
/// parameters re-appear as representation markers after the name
/// (`fn id<t: KioType>(...)`).
///
/// `crate::shapes::…` references in the original signature point
/// at the emitted library crate; rewrite them to
/// `<crate_name>::shapes::…` so they resolve from the driver crate
/// the impl block lives in.
fn render_method_sig(m: &TraitMethod, crate_name: &str) -> String {
    let mut params = String::from("&self");
    for (i, ty) in m.arg_types.iter().enumerate() {
        let ty = rewrite_crate_paths(ty, crate_name);
        params.push_str(&format!(", arg{i}: {ty}"));
    }
    let ret_text = rewrite_crate_paths(&m.ret_type, crate_name);
    let ret = if m.ret_type == "()" {
        String::new()
    } else {
        format!(" -> {ret_text}")
    };
    // Each source kind-`*` binder is a `KioType` marker. Function slots use
    // the concrete `KioFnN` facade, so they add no synthetic closure type
    // parameter and no `Fn` where-clause.
    let type_params = if m.type_params.is_empty() {
        String::new()
    } else {
        let bounded: Vec<String> = m
            .type_params
            .iter()
            .map(|n| format!("{n}: {crate_name}::shapes::KioType"))
            .collect();
        format!("<{}>", bounded.join(", "))
    };
    let where_clause = if m.where_clause.is_empty() {
        String::new()
    } else {
        format!(
            " where {}",
            rewrite_crate_paths(&m.where_clause, crate_name)
        )
    };
    format!("fn {}{type_params}({params}){ret}{where_clause}", m.name)
}

/// Rewrite every standalone `crate::` path prefix to
/// `<crate_name>::`. The trait emitter writes shape references as
/// `crate::shapes::Product` (where `crate` is the emitted library);
/// inside the driver crate (the runner's own `src/main.rs`),
/// `crate` resolves to the driver, so we have to swap the prefix to
/// the emitted library crate name.
fn rewrite_crate_paths(ty: &str, crate_name: &str) -> String {
    // Token-level replacement so we don't accidentally hit a
    // substring inside an identifier. The path token always
    // appears as a top-level prefix or after `<`, `,`, ` `, or
    // `(` — Rust's path syntax doesn't admit it in any other
    // position.
    let mut out = String::with_capacity(ty.len());
    let bytes = ty.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let starts_here = i == 0 || matches!(bytes[i - 1], b'<' | b',' | b' ' | b'(' | b'>' | b'&');
        if starts_here && ty[i..].starts_with("crate::") {
            out.push_str(crate_name);
            out.push_str("::");
            i += "crate::".len();
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn struct_literal_expr_path(type_path: &str) -> String {
    match type_path.find('<') {
        Some(generic_start) if !type_path[..generic_start].ends_with("::") => {
            format!(
                "{}::{}",
                &type_path[..generic_start],
                &type_path[generic_start..]
            )
        }
        _ => type_path.to_owned(),
    }
}

/// Render a Rust expression body for one exact protocol binding.
///
/// Standard protocol bodies map directly to the shared [`CanonicalKind`]
/// renderer. Protocol-owned custom bodies stay explicit here, including their
/// stable FFI alias use; an unknown custom tag is an internal registry/adapter
/// mismatch rather than a runtime fallback.
///
/// The [`TraitMethod`] supplies the already-projected Rust signature for bodies
/// whose expression needs one of its types —
fn render_rust_body_for_binding(binding: &HostFnBinding, m: &TraitMethod) -> String {
    if matches!(binding.body, HostFnBodyKind::Loop) {
        let member = host_api::rust_host_member(binding.module, binding.leaf);
        let sum_path = format!("crate::ffi::env::{member}::arg0_cbret<s, r>");
        let (v0, v1) = sum_variant_ctors(&sum_path);
        return format!(
            "let mut __s = arg1; loop {{ match arg0.call(__s) {{ \
             {v0}(v) => {{ __s = v; }} \
             {v1}(v) => return v, \
             }} }}"
        );
    }
    let kind = match binding.body {
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
        HostFnBodyKind::UnreachableI32Print { .. } => {
            return "panic!(\"unreachable i32 print fixture\")".to_owned();
        }
        HostFnBodyKind::MakeToken { .. } => {
            return if m.arg_types.first().is_some_and(|ty| ty == "SelectedI32") {
                "arg0.0".to_owned()
            } else {
                "arg0".to_owned()
            };
        }
        HostFnBodyKind::TokenValue { .. } => {
            return if m.ret_type == "SelectedI32" {
                "arg0.into()".to_owned()
            } else {
                "arg0".to_owned()
            };
        }
        HostFnBodyKind::BoxMake { .. } => {
            let marker = m
                .type_params
                .first()
                .expect("Box.make fixture has one marker binder");
            let carrier = struct_literal_expr_path(&m.ret_type);
            return format!(
                "{carrier}::from_storage(__BoxStorage(::std::rc::Rc::new(crate::shapes::KioValue::<{marker}>::pack(arg0).into_stored())))"
            );
        }
        HostFnBodyKind::BoxGet { .. } => {
            let marker = m
                .type_params
                .first()
                .expect("Box.get fixture has one marker binder");
            return format!(
                "{{ let storage = arg0.storage(); crate::shapes::KioValue::<{marker}>::from_stored(storage.0.as_ref().clone()).unpack() }}"
            );
        }
        HostFnBodyKind::CallStep { .. } => {
            let member = host_api::rust_host_member(binding.module, binding.leaf);
            let payload_ty = format!("crate::ffi::env::{member}::arg0_cbarg<Self>");
            let payload_expr_path = struct_literal_expr_path(&payload_ty);
            let payload = right_nested_product_literal(
                &payload_expr_path,
                "crate::shapes::Product",
                &[
                    "arg1".to_owned(),
                    "\"compound-callback\".to_string()".to_owned(),
                    "true".to_owned(),
                ],
            );
            return format!("{{ let payload = {payload}; arg0.call(payload) }}");
        }
        HostFnBodyKind::MakePairCallback { .. } => {
            return "{ let out = arg0.call(arg1); out._0 }".to_owned();
        }
        HostFnBodyKind::MakeStep { .. } => {
            let carrier = struct_literal_expr_path(&m.ret_type);
            return format!("{carrier}::new(move |n| n.wrapping_add(arg0))");
        }
        HostFnBodyKind::ApplyPoly { .. } => {
            let member = host_api::rust_host_member(binding.module, binding.leaf);
            return format!(
                "<_ as crate::ffi::env::{member}::arg0_impl<Self>>::apply::<crate::shapes::KioNative<String>>(&arg0, \"rank-n\\n\".to_string())"
            );
        }
        HostFnBodyKind::MakePairStructural { .. } => {
            return render_make_pair_structural_body(m);
        }
        HostFnBodyKind::ProducePair { .. } => {
            return format!(
                "{{ println!(\"direct\"); {} {{ _0: 7, _1: \"direct-value\".to_string() }} }}",
                rust_struct_literal_path(&m.ret_type)
            );
        }
        HostFnBodyKind::SumToString { .. } => return render_sum_to_string_body(m),
        HostFnBodyKind::ObservePacked { .. } => return "{
            EXISTENTIAL_COUNTS.with(|counts| { let (observations, openings) = counts.get(); counts.set((observations + 1, openings)); });
            struct Open;
            impl crate::ffi::exp::testapi_types__Packed::readPacked_continuation_impl<StubHost, crate::shapes::KioNative<i32>> for Open {
                fn apply<U: crate::shapes::KioType>(&self, payload: crate::ffi::exp::testapi_types__Packed::readPacked_continuation_cbarg<StubHost, U>) -> i32 {
                    EXISTENTIAL_COUNTS.with(|counts| { let (observations, openings) = counts.get(); counts.set((observations, openings + 1)); });
                    payload._1.call(payload._0)
                }
            }
            let continuation = crate::ffi::exp::testapi_types__Packed::readPacked_continuation::<StubHost, crate::shapes::KioNative<i32>>::new(Open);
            crate::shapes::nominal::testapi::types::Packed::<StubHost>::readPacked(arg0, continuation)
        }".to_owned(),
        HostFnBodyKind::RoundFunctor
        | HostFnBodyKind::RoundPicker
        | HostFnBodyKind::RoundPolyThunk
        | HostFnBodyKind::RoundPolyUnitSlot => {
            return "arg0".to_owned();
        }
        HostFnBodyKind::StagedSecond { .. } => return "arg1.to_string()".to_owned(),
        HostFnBodyKind::NestedCurriedRoundtrip { .. } => {
            return "{ let left = \"host-left\".to_string(); let right = \"host-right\".to_string(); let step = arg0.call(left); println!(\"round host probe: {}\", step.call(right)); arg0 }".to_owned();
        }
        HostFnBodyKind::InvokeSubstitutedUnitCallback { .. } => {
            return "format!(\"host/{}\", arg0.call(()))".to_owned();
        }
        HostFnBodyKind::ReturnedForallUnit => {
            let member = host_api::rust_host_member(binding.module, binding.leaf);
            return format!(
                "{{ \
                 if self.returned_forall_produced.replace(true) {{ \
                     println!(\"throw\"); \
                     ::std::panic::panic_any(\"produce failed\"); \
                 }} \
                 println!(\"produce\"); \
                 struct Impl; \
                 impl crate::ffi::env::{member}::ret_impl<StubHost> for Impl {{ \
                     fn apply<A: crate::shapes::KioType>(&self) -> A::Facade {{ \
                         /* `[A] A` has no total terminating inhabitant. This fixture-specific \
                         primitive is exercised only at Unit. `ret::new` first invokes it at \
                         the generated erased ingress, whose facade is `KioStoredValue`; this \
                         guard checks that convention, not that A is Unit. A facade alias can \
                         pass the guard and then deterministically panic during marker decoding. */ \
                         if ::std::any::TypeId::of::<A::Facade>() \
                             != ::std::any::TypeId::of::<crate::shapes::KioStoredValue>() \
                         {{ \
                             panic!(\"returned forall fixture called outside erased ingress\"); \
                         }} \
                         crate::shapes::KioValue::<A>::from_stored( \
                             crate::shapes::KioValue::<crate::shapes::KioUnit>::pack(()).into_stored(), \
                         ).unpack() \
                     }} \
                 }} \
                 crate::ffi::env::{member}::ret::<StubHost>::new(Impl) \
                 }}"
            );
        }
        HostFnBodyKind::TraceUnit { text } => {
            return format!("println!({text:?})");
        }
        HostFnBodyKind::StagedUnitCall => {
            return "println!(\"staged Unit host call\")".to_owned();
        }
    };
    render_rust_body(&kind, m)
}

fn render_make_pair_structural_body(m: &TraitMethod) -> String {
    format!(
        "{} {{ _0: arg0, _1: arg1.to_string() }}",
        rust_struct_literal_path(&m.ret_type)
    )
}

fn render_sum_to_string_body(m: &TraitMethod) -> String {
    let (v0, v1) = sum_variant_ctors(
        m.arg_types
            .first()
            .expect("sum-to-string fixture must have an argument"),
    );
    format!(
        "match arg0 {{ \
         {v0}(n) => n.to_string(), \
         {v1}(s) => s, \
         }}"
    )
}

fn rust_struct_literal_path(ty: &str) -> String {
    match ty.split_once('<') {
        Some((head, args)) => format!("{head}::<{args}"),
        None => ty.to_owned(),
    }
}

fn render_rust_body(kind: &CanonicalKind, m: &TraitMethod) -> String {
    match kind {
        CanonicalKind::Print => "use std::io::Write; \
             let _ = std::io::stdout().write_all(arg0.as_bytes()); \
             let _ = std::io::stdout().flush();"
            .to_owned(),
        CanonicalKind::Eprint => "use std::io::Write; \
             let _ = std::io::stderr().write_all(arg0.as_bytes()); \
             let _ = std::io::stderr().flush();"
            .to_owned(),
        // The arg's underlying type is the host's Int role. i32
        // conversion via `as` is safe across every integer role
        // (narrowing/widening preserves observability for codes
        // in 0..=125).
        CanonicalKind::Exit => "std::process::exit((arg0 as i32).clamp(0, 125));".to_owned(),
        CanonicalKind::ReadAsciiLine => render_read_ascii_line_body_rust(m),
        CanonicalKind::StringLen => render_string_len_body_rust(),
        CanonicalKind::StringSlice => render_string_slice_body_rust(),
        CanonicalKind::StringCodeAt => render_string_code_at_body_rust(m),
        CanonicalKind::StringConcat => "format!(\"{}{}\", arg0, arg1)".to_owned(),
        CanonicalKind::StringEq => "arg0 == arg1".to_owned(),
        CanonicalKind::StringToInt => render_string_to_int_body_rust(m),
        CanonicalKind::BoolToString => "arg0.to_string()".to_owned(),
        CanonicalKind::PrintI32 => "use std::io::Write; \
             let __s = arg0.to_string(); \
             let _ = std::io::stdout().write_all(__s.as_bytes()); \
             let _ = std::io::stdout().flush();"
            .to_owned(),
        // f32 promotes to f64 before formatting to match the JS
        // runner's output (JS has no f32; Number.prototype.toString
        // on a JS-promoted f32 gives the f64-precision render).
        // Rust's f32::Display picks a shorter representation, so
        // a bare `.to_string()` on f32 would diverge.
        CanonicalKind::NumericToString { kind } => {
            if kind == "f32" {
                "(arg0 as f64).to_string().into()".to_owned()
            } else {
                "arg0.to_string().into()".to_owned()
            }
        }
        CanonicalKind::Arith { op, kind } => arith_body_rust(op, kind),
        // The float (`f32` / `f64`) arithmetic families use the bare
        // IEEE-754 op (no width wrap).
        CanonicalKind::FloatArith { op, kind } => arith_body_rust(op, kind),
        // `dyn_load_prime`'s opaque-scalar surface over the `__Scalar` enum.
        CanonicalKind::MakeScalar => render_make_scalar_body_rust(),
        CanonicalKind::ScalarOf { kind } => match kind.as_str() {
            "i32" => "__Scalar::I32(arg0)".to_owned(),
            "str" => "__Scalar::Str(arg0.to_owned())".to_owned(),
            "bool" => "__Scalar::Bool(arg0)".to_owned(),
            "f64" => "__Scalar::F64(arg0)".to_owned(),
            other => format!("panic!(\"scalar_of_{other}: unsupported kind\");"),
        },
        CanonicalKind::ScalarAs { kind } => render_scalar_as_body_rust(kind, m),
        CanonicalKind::ScalarIsTrue => {
            "match arg0 { __Scalar::Bool(b) => b, _ => false }".to_owned()
        }
        CanonicalKind::Cmp { cmp, .. } => cmp_body_rust(cmp),
        CanonicalKind::Loop => unreachable!("loop bodies return before canonical rendering"),
        // The polymorphic `array_*` family uses one invariant associated
        // storage and generated application carriers. Each element crosses
        // the storage boundary through its `KioType` marker.
        CanonicalKind::Array(op) => render_array_body_rust(op, m),
        CanonicalKind::Custom => format!(
            "panic!(\"kio-test-runner-rust: no canonical impl for host fn `{name}`\");",
            name = m.name,
        ),
    }
}

fn render_read_ascii_line_body_rust(m: &TraitMethod) -> String {
    let (v0, v1) = sum_variant_ctors(&m.ret_type);
    format!(
        "match __kio_read_next_line() {{ \
         Some(line) => {v0}(line), \
         None => {v1}(()), \
         }}"
    )
}

fn render_string_to_int_body_rust(m: &TraitMethod) -> String {
    let (v0, v1) = sum_variant_ctors(&m.ret_type);
    format!(
        "match arg0.parse::<i32>() {{ \
         Ok(n) => {v0}(n), \
         Err(_) => {v1}(()), \
         }}"
    )
}

fn render_string_len_body_rust() -> String {
    "arg0.len() as i32".to_owned()
}

fn render_string_slice_body_rust() -> String {
    "{ let __len = arg0.len() as i32; \
     if arg1 < 0 || arg1 > arg2 || arg2 > __len { \
     eprintln!(\"string_slice: invalid range [{}, {}) for len {}\", arg1, arg2, __len); std::process::exit(1); } \
     arg0[arg1 as usize..arg2 as usize].to_string() }"
        .to_owned()
}

fn render_string_code_at_body_rust(m: &TraitMethod) -> String {
    let (v0, v1) = sum_variant_ctors(&m.ret_type);
    format!(
        "if arg1 < 0 || (arg1 as usize) >= arg0.len() {{ \
         {v1}(()) \
         }} else {{ \
         {v0}(arg0.as_bytes()[arg1 as usize] as i32) \
         }}"
    )
}

/// `make_scalar(text, representation) -> __Scalar`. The fixture host maps an
/// exact host-type descriptor to this representation key (`I32` / `Int` →
/// i32, `F64` / `F32` → f64, `String` / `Str` → str, `Bool` → bool),
/// then parses the literal text. Both arguments are owned `String` values.
fn render_make_scalar_body_rust() -> String {
    "match arg1.as_str() { \
     \"I32\" | \"Int\" => __Scalar::I32(arg0.parse::<i32>().expect(\"make_scalar: bad i32\")), \
     \"F64\" | \"F32\" => __Scalar::F64(arg0.parse::<f64>().expect(\"make_scalar: bad f64\")), \
     \"String\" | \"Str\" => __Scalar::Str(arg0.to_owned()), \
     \"Bool\" => __Scalar::Bool(arg0 == \"t\"), \
     other => panic!(\"make_scalar: unknown representation key {}\", other), \
     }"
    .to_owned()
}

/// `scalar_as_<kind>(s) -> . | <Kind>`. Project the typed value out of the
/// `__Scalar`, or the unit arm on a shape mismatch. The sum is `. | T`
/// (`Left` is unit, `Right` is `T`), so the present value uses `Right` and
/// the absent `()` uses `Left`.
fn render_scalar_as_body_rust(kind: &str, m: &TraitMethod) -> String {
    let (v0, v1) = sum_variant_ctors(&m.ret_type);
    let (variant, bind) = match kind {
        "i32" => ("__Scalar::I32", "v"),
        "f64" => ("__Scalar::F64", "v"),
        "str" => ("__Scalar::Str", "v"),
        "bool" => ("__Scalar::Bool", "v"),
        other => return format!("panic!(\"scalar_as_{other}: unsupported kind\");"),
    };
    format!("match arg0 {{ {variant}({bind}) => {v1}({bind}), _ => {v0}(()) }}")
}

fn arith_body_rust(op: &str, kind: &str) -> String {
    // Floats use plain operators; integers use wrapping_* to
    // mirror the JS runner's BigInt.asIntN / asUintN behavior
    // (signed overflow wraps modulo 2^N rather than panicking).
    // Integer modulo uses wrapping_rem, which matches JS's `%`
    // operator on width-wrapped operands (truncated-toward-zero
    // remainder, matching Rust's `%` semantics on the underlying
    // primitive).
    let is_float = matches!(kind, "f32" | "f64");
    match (op, is_float) {
        ("add", true) => "arg0 + arg1".to_owned(),
        ("sub", true) => "arg0 - arg1".to_owned(),
        ("mul", true) => "arg0 * arg1".to_owned(),
        ("div", true) => "arg0 / arg1".to_owned(),
        ("mod", true) => "arg0 % arg1".to_owned(),
        ("add", false) => "arg0.wrapping_add(arg1)".to_owned(),
        ("sub", false) => "arg0.wrapping_sub(arg1)".to_owned(),
        ("mul", false) => "arg0.wrapping_mul(arg1)".to_owned(),
        ("div", false) => "arg0.wrapping_div(arg1)".to_owned(),
        ("mod", false) => "arg0.wrapping_rem(arg1)".to_owned(),
        _ => format!("arg0 /* {op}_{kind} */"),
    }
}

fn cmp_body_rust(cmp: &str) -> String {
    match cmp {
        "eq" => "arg0 == arg1".to_owned(),
        "lt" => "arg0 < arg1".to_owned(),
        "leq" | "le" => "arg0 <= arg1".to_owned(),
        "gt" => "arg0 > arg1".to_owned(),
        "geq" | "ge" => "arg0 >= arg1".to_owned(),
        _ => "false".to_owned(),
    }
}

/// Render one Rust expression body for an [`ArrayOp`] arm. The applied
/// carrier exposes `storage()` / `from_storage`; the storage itself retains
/// `KioStoredValue`, and `KioValue<t>` performs the typed element conversion.
fn render_array_body_rust(op: &ArrayOp, m: &TraitMethod) -> String {
    let marker = m
        .type_params
        .first()
        .expect("every Array operation has one marker binder");
    let pack =
        |value: &str| format!("crate::shapes::KioValue::<{marker}>::pack({value}).into_stored()");
    let unpack =
        |value: &str| format!("crate::shapes::KioValue::<{marker}>::from_stored({value}).unpack()");
    let carrier = || struct_literal_expr_path(&m.ret_type);
    match op {
        ArrayOp::MakeEmpty => {
            format!(
                "{}::from_storage(__ArrayStorage::new(Vec::new()))",
                carrier()
            )
        }
        ArrayOp::MakeFilled => {
            let packed = pack("arg1.clone()");
            format!(
                "if arg0 < 0 {{ panic!(\"array_make_filled: negative size {{}}\", arg0); }} \
                 {}::from_storage(__ArrayStorage::new((0..arg0).map(|_| {packed}).collect()))",
                carrier()
            )
        }
        ArrayOp::Len => "arg0.storage().0.borrow().len() as i32".to_owned(),
        ArrayOp::Get => format!(
            "{{ let __storage = arg0.storage(); let __a = __storage.0.borrow(); \
             if arg1 < 0 || (arg1 as usize) >= __a.len() {{ \
             panic!(\"array_get: index {{}} out of bounds (len {{}})\", arg1, __a.len()); }} \
             {} }}",
            unpack("__a[arg1 as usize].clone()")
        ),
        ArrayOp::Set => format!(
            "{{ let __storage = arg0.storage(); let mut __a = __storage.0.borrow_mut(); \
             if arg1 < 0 || (arg1 as usize) >= __a.len() {{ \
             panic!(\"array_set: index {{}} out of bounds (len {{}})\", arg1, __a.len()); }} \
             __a[arg1 as usize] = {}; }}",
            pack("arg2")
        ),
        ArrayOp::Push => format!("arg0.storage().0.borrow_mut().push({});", pack("arg1")),
        ArrayOp::PopBack => {
            // The `t | .` return is built through canonical `Sum` variants:
            // `Left` carries the popped value and `Right` the empty case.
            let (v0, v1) = sum_variant_ctors(&m.ret_type);
            let unpacked = unpack("v");
            format!(
                "{{ let popped = arg0.storage().0.borrow_mut().pop(); \
                 match popped {{ \
                 Some(v) => {v0}({unpacked}), \
                 None => {v1}(()), \
                 }} }}"
            )
        }
        ArrayOp::Swap => {
            "{ let __storage = arg0.storage(); let mut __a = __storage.0.borrow_mut(); \
             let __len = __a.len(); \
             if arg1 < 0 || (arg1 as usize) >= __len { \
             panic!(\"array_swap: index {} out of bounds (len {})\", arg1, __len); } \
             if arg2 < 0 || (arg2 as usize) >= __len { \
             panic!(\"array_swap: index {} out of bounds (len {})\", arg2, __len); } \
             __a.swap(arg1 as usize, arg2 as usize); }"
                .to_owned()
        }
        ArrayOp::Clear => "arg0.storage().0.borrow_mut().clear();".to_owned(),
        ArrayOp::Clone => format!(
            "{{ let __storage = arg0.storage(); let __values = __storage.0.borrow().clone(); \
             {}::from_storage(__ArrayStorage::new(__values)) }}",
            carrier()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::HostRoleRef;

    #[cfg(unix)]
    #[test]
    fn disabled_cache_still_observes_the_cold_compile_not_rustc_probes() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tempfile::TempDir::new().unwrap();
        let crate_root = fixture.path().join("crate");
        fs::create_dir_all(crate_root.join("src")).unwrap();
        fs::write(
            crate_root.join("src/lib.rs"),
            b"pub fn answer() -> i32 { 42 }\n",
        )
        .unwrap();

        let observer_path = fixture.path().join("rustc");
        fs::write(
            &observer_path,
            b"#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$(dirname \"$0\")/observer.log\"\nexec \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&observer_path, fs::Permissions::from_mode(0o755)).unwrap();

        let runner = RustRunner {
            cache: RunnerCache::Disabled,
            compiler_observer: CompilerObserver::for_test(observer_path),
            compiler_admission: compiler_admission::CompilerAdmission::disabled(),
            protocol: RunnerProtocol::CompileOnly,
            identity: ArtifactIdentity {
                namespace: "observer_fixture".to_owned(),
            },
            profile: OptProfile::Unoptimized,
        };
        assert_eq!(runner.run(&crate_root, RunnerProtocol::CompileOnly), 0);

        let observed = fs::read_to_string(fixture.path().join("observer.log")).unwrap();
        assert_eq!(observed.lines().collect::<Vec<_>>(), ["rustc"]);
    }

    #[test]
    fn coexist_rustc_uses_the_shared_outer_observer_shape() {
        let observer = CompilerObserver::for_test("observe");
        let command = coexist_rustc_command(&observer);
        assert_eq!(command.get_program(), "observe");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["rustc"]);
    }

    #[test]
    fn main_call_uses_the_exact_declaring_module() {
        assert_eq!(rust_main_call("main"), "    pkg.main.main();\n");
        assert_eq!(
            rust_main_call("testapi/main"),
            "    pkg.testapi.main.main();\n"
        );
        assert_eq!(rust_main_call("prog"), "    pkg.prog.main();\n");
        assert_eq!(rust_main_call("api"), "    pkg.api.main();\n");
    }

    #[test]
    fn cache_disable_env_accepts_only_boolean_values() {
        assert!(!parse_cache_disable_value("").unwrap());
        assert!(!parse_cache_disable_value("0").unwrap());
        assert!(parse_cache_disable_value("1").unwrap());
        assert!(parse_cache_disable_value("true").is_err());
        assert!(parse_cache_disable_value("2").is_err());
    }

    #[test]
    fn cache_size_literal_accepts_plain_bytes_and_binary_suffixes() {
        assert_eq!(parse_cache_size_literal("1024").unwrap(), 1024);
        assert_eq!(parse_cache_size_literal("1K").unwrap(), 1024);
        assert_eq!(parse_cache_size_literal("2MB").unwrap(), 2 * 1024 * 1024);
        assert_eq!(
            parse_cache_size_literal("3g").unwrap(),
            3 * 1024 * 1024 * 1024
        );
        assert_eq!(parse_cache_size_literal("4T").unwrap(), 4 * 1024_u64.pow(4));
    }

    #[test]
    fn cache_size_literal_rejects_blank_zero_and_unknown_suffixes() {
        assert!(parse_cache_size_literal("").is_err());
        assert!(parse_cache_size_literal("0").is_err());
        assert!(parse_cache_size_literal("12XB").is_err());
    }

    #[test]
    fn every_rust_host_api_is_an_exact_contract_projection() {
        for &protocol in RunnerProtocol::ALL {
            let contract = protocol.contract();
            let api = host_api_for_protocol(protocol);
            assert_eq!(api.types.len(), contract.host_types.len(), "{protocol:?}");
            assert_eq!(api.functions.len(), contract.host_fns.len(), "{protocol:?}");

            for (assoc, binding) in api.assoc_types().zip(contract.host_types) {
                assert_eq!(assoc.name, binding.leaf, "{protocol:?}");
                assert_eq!(
                    assoc.boundary_name.as_deref(),
                    Some(host_api::rust_host_member(binding.module, binding.leaf).as_str()),
                    "{protocol:?}"
                );
                assert_eq!(
                    assoc.role,
                    match binding.fixture {
                        HostTypeFixture::Role(role) | HostTypeFixture::SelectedRole(role) => {
                            role.role()
                        }
                        _ => "",
                    },
                    "{protocol:?}"
                );
                assert_eq!(assoc.type_params.len(), usize::from(binding.type_arity));
            }

            for (method, binding) in api.methods().zip(contract.host_fns) {
                assert_eq!(
                    method.name,
                    host_api::rust_host_member(binding.module, binding.leaf),
                    "{protocol:?}"
                );
                let _ = render_rust_body_for_binding(binding, method);
            }
        }
    }

    #[test]
    fn shaped_host_slots_use_stable_emitted_ffi_aliases() {
        let protocol = RunnerProtocol::HostCallbackRoundtrip;
        let contract = protocol.contract();
        let api = host_api_for_protocol(protocol);
        let (binding, call_step) = contract
            .host_fns
            .iter()
            .zip(api.methods())
            .find(|(binding, _)| matches!(binding.body, HostFnBodyKind::CallStep { .. }))
            .expect("callback contract declares call_step");
        assert!(call_step.where_clause.is_empty());
        assert_eq!(
            call_step.arg_types[0],
            "crate::ffi::env::testapi_arith__callStep::arg0<Self>"
        );
        let body = render_rust_body_for_binding(binding, call_step);
        assert!(body.contains("testapi_arith__callStep::arg0_cbarg::<Self>"));
        assert!(body.contains("_0: arg1"));
        assert!(body.contains("arg0.call(payload)"));
        assert!(!body.contains("arg0(payload)"));
        let signature = render_method_sig(call_step, "test_pkg");
        assert!(signature.contains("testapi_arith__callStep::arg0<Self>"));
        assert!(!signature.contains("impl Fn"));
        assert!(!signature.contains("Rc<dyn Fn"));
        assert!(!signature.contains(" where "));
    }

    #[test]
    fn rank_n_host_slots_use_uniform_values_and_stable_impl_aliases() {
        let protocol = RunnerProtocol::HostRanknRoundtrip;
        let contract = protocol.contract();
        let api = host_api_for_protocol(protocol);
        let (binding, apply_poly) = contract
            .host_fns
            .iter()
            .zip(api.methods())
            .find(|(binding, _)| matches!(binding.body, HostFnBodyKind::ApplyPoly { .. }))
            .expect("rank-N contract declares apply_poly");

        assert_eq!(
            apply_poly.arg_types,
            ["crate::ffi::env::testapi_arith__applyPoly::arg0<Self>"]
        );
        let signature = render_method_sig(apply_poly, "test_pkg");
        assert!(signature.contains("testapi_arith__applyPoly::arg0<Self>"));
        assert!(!signature.contains("impl "));
        assert!(!signature.contains("KioForall_"));

        let body = render_rust_body_for_binding(binding, apply_poly);
        assert!(body.contains("testapi_arith__applyPoly::arg0_impl<Self>"));
        assert!(body.contains("KioNative<String>"));
        assert!(!body.contains("KioForall_"));
    }

    #[test]
    fn returned_forall_uses_typed_value_ingress_without_raw_storage_escape() {
        let protocol = RunnerProtocol::ReturnedForallCallByValue;
        let contract = protocol.contract();
        let api = host_api_for_protocol(protocol);
        let (binding, produce) = contract
            .host_fns
            .iter()
            .zip(api.methods())
            .find(|(binding, _)| matches!(binding.body, HostFnBodyKind::ReturnedForallUnit))
            .expect("returned-forall contract declares produce");

        assert_eq!(
            produce.ret_type,
            "crate::ffi::env::testapi_main__produce::ret<Self>"
        );
        let body = render_rust_body_for_binding(binding, produce);
        assert!(body.contains("testapi_main__produce::ret_impl<StubHost>"));
        assert!(body.contains("testapi_main__produce::ret::<StubHost>::new(Impl)"));
        assert!(body.contains("TypeId::of::<A::Facade>()"));
        assert!(body.contains("TypeId::of::<crate::shapes::KioStoredValue>()"));
        assert!(body.contains("KioValue::<crate::shapes::KioUnit>::pack(())"));
        assert!(body.contains("KioValue::<A>::from_stored"));
        assert!(!body.contains("__KioErasedType"));
        assert!(!body.contains("Rc::new(())"));
        assert!(!body.contains("dyn ::std::any::Any"));
    }

    #[test]
    fn nested_curried_host_roundtrip_forwards_the_owned_callback() {
        let protocol = RunnerProtocol::NestedCurriedRoundtrip;
        let contract = protocol.contract();
        let api = host_api_for_protocol(protocol);
        let (binding, round_host) = contract
            .host_fns
            .iter()
            .zip(api.methods())
            .next()
            .expect("nested-curried contract declares round_host");

        assert!(round_host.where_clause.is_empty());
        assert_eq!(
            round_host.arg_types,
            ["crate::ffi::env::testapi_api__roundHost::arg0<Self>"]
        );
        assert_eq!(
            round_host.ret_type,
            "crate::ffi::env::testapi_api__roundHost::ret<Self>"
        );
        let body = render_rust_body_for_binding(binding, round_host);
        assert!(body.contains("arg0.call(left)"));
        assert!(body.contains("step.call(right)"));
        assert!(body.ends_with("arg0 }"));
        assert!(!body.contains("arg0("));
    }

    #[test]
    fn nested_curried_driver_uses_exact_export_callback_aliases() {
        let protocol = RunnerProtocol::NestedCurriedRoundtrip;
        let host = host_api_for_protocol(protocol);
        let driver = build_driver_for_protocol("test_pkg", &host, protocol)
            .expect("nested-curried protocol has an invocable Rust driver");

        assert!(
            driver
                .contains("test_pkg::ffi::exp::testapi_api__viaHost::arg0::<StubHost>::new(|left|"),
            "{driver}"
        );
        assert!(
            driver.contains(
                "test_pkg::ffi::exp::testapi_api__viaHost::arg0_cbret::<StubHost>::new(move |right|"
            ),
            "{driver}"
        );
        assert!(!driver.contains("KioNative<String>"), "{driver}");
    }

    #[test]
    fn host_substituted_unit_driver_uses_the_exact_export_callback_alias() {
        let protocol = RunnerProtocol::HostSubstitutedUnitCallback;
        let host = host_api_for_protocol(protocol);
        let driver = build_driver_for_protocol("test_pkg", &host, protocol)
            .expect("host-substituted Unit protocol has an invocable Rust driver");

        assert!(
            driver.contains("test_pkg::ffi::exp::testapi_api__viaHost::arg0::<StubHost>::new(|()|"),
            "{driver}"
        );
        assert!(!driver.contains("KioNative<String>"), "{driver}");
    }

    #[test]
    fn module_alias_driver_passes_complete_product_values() {
        let protocol = RunnerProtocol::ModuleAliasScopeCollision;
        let host = host_api_for_protocol(protocol);
        let driver =
            build_driver_for_protocol("fixture", &host, protocol).expect("module-alias driver");

        assert!(driver.contains("consume(a_value)"), "{driver}");
        assert!(driver.contains("consume(b_value)"), "{driver}");
        assert!(!driver.contains("consume(a_value._0"), "{driver}");
        assert!(!driver.contains("consume(b_value._0"), "{driver}");
    }

    #[test]
    fn compound_source_parameters_use_single_public_products() {
        let protocol = RunnerProtocol::ExportMultilabelRoundtrip;
        let host = host_api_for_protocol(protocol);
        let driver = build_driver_for_protocol("test_pkg", &host, protocol)
            .expect("multilabel protocol has an invocable Rust driver");
        assert!(
            driver.contains("_0: test_pkg::shapes::nominal::testapi::main::A::<StubHost>::mk(42)"),
            "{driver}"
        );
        assert!(
            driver.contains(
                "_1: test_pkg::shapes::nominal::testapi::main::B::<StubHost>::mk(\"shown\\n\".to_string())"
            ),
            "{driver}"
        );
        assert!(driver.contains("get(row._0)"), "{driver}");
        assert!(driver.contains("get(row._1)"), "{driver}");
        assert!(!driver.contains("get(row.A)"), "{driver}");
        assert!(!driver.contains("get(row.B)"), "{driver}");

        let protocol = RunnerProtocol::ExportTypeRoundtrip;
        let host = host_api_for_protocol(protocol);
        let driver = build_driver_for_protocol("test_pkg", &host, protocol)
            .expect("type-roundtrip protocol has an invocable Rust driver");
        assert!(
            driver.contains(
                "Pair::mkPair(test_pkg::ffi::exp::testapi_types__Pair::mkPair_arg0::<test_pkg::shapes::KioNative<String>, test_pkg::shapes::KioNative<String>> { _0: \"export-type-left\".to_string(), _1: \"export-type-right\".to_string() })"
            ),
            "{driver}"
        );
        assert!(
            !driver.contains("Pair::mkPair(\"export-type-left\""),
            "{driver}"
        );

        let protocol = RunnerProtocol::NewtypeVisibilityFacade;
        let host = host_api_for_protocol(protocol);
        let driver = build_driver_for_protocol("test_pkg", &host, protocol)
            .expect("newtype-visibility protocol has an invocable Rust driver");
        assert!(
            driver.contains(
                "makeConstructorPair(test_pkg::shapes::Product { _0: SelectedI32(81), _1: SelectedI32(82) })"
            ),
            "{driver}"
        );
        assert!(
            driver.contains(
                "makeProjectorPairValue(test_pkg::shapes::Product { _0: constructor_pair_out._0.clone(), _1: constructor_pair_out._1.clone() })"
            ),
            "{driver}"
        );
        assert!(
            !driver.contains("makeConstructorPair(SelectedI32(81), SelectedI32(82))"),
            "{driver}"
        );
        assert!(
            !driver.contains(
                "makeProjectorPairValue(constructor_pair_out._0.clone(), constructor_pair_out._1.clone())"
            ),
            "{driver}"
        );
    }

    #[test]
    fn interleaved_stage_host_impl_retains_phantom_type_parameters() {
        let protocol = RunnerProtocol::HostInterleavedStageRoundtrip;
        let contract = protocol.contract();
        let api = host_api_for_protocol(protocol);
        let staged = contract
            .host_fns
            .iter()
            .zip(api.methods())
            .find_map(|(binding, method)| {
                matches!(binding.body, HostFnBodyKind::StagedSecond { .. }).then_some(method)
            })
            .expect("interleaved-stage contract declares staged");

        assert_eq!(staged.type_params, ["A", "B"]);
        assert_eq!(
            render_method_sig(staged, "test_pkg"),
            "fn testapi_arith__staged<A: test_pkg::shapes::KioType, B: test_pkg::shapes::KioType>(&self, arg0: String, arg1: String) -> String"
        );
    }

    #[test]
    fn parameterized_host_storage_is_nullary_and_marker_typed() {
        let protocol = RunnerProtocol::HostGenericTypeRoundtrip;
        let host = host_api_for_protocol(protocol);
        let driver = build_driver("test_pkg", &host, "    let _ = pkg;\n", protocol);
        assert!(
            driver
                .contains("struct __BoxStorage(::std::rc::Rc<test_pkg::shapes::KioStoredValue>);")
        );
        assert!(driver.contains("type testapi__BoxStorage = __BoxStorage;"));
        assert!(driver.contains("t: test_pkg::shapes::KioType"));
        assert!(driver.contains("test_pkg::shapes::KioValue::<t>::pack(arg0).into_stored()"));
        assert!(driver.contains("test_pkg::shapes::KioValue::<t>::from_stored"));
        assert!(!driver.contains("type testapi__Box<"));
        assert!(!driver.contains("__BoxCell"));

        let protocol = RunnerProtocol::TestApiArray;
        let host = host_api_for_protocol(protocol);
        let driver = build_driver("test_pkg", &host, "    let _ = pkg;\n", protocol);
        assert!(driver.contains("type testapi__ArrayStorage = __ArrayStorage;"));
        assert!(driver.contains("Vec<test_pkg::shapes::KioStoredValue>"));
        assert!(driver.contains("t: test_pkg::shapes::KioType"));
        assert!(driver.contains("test_pkg::shapes::KioValue::<t>::pack"));
        assert!(driver.contains("test_pkg::shapes::KioValue::<t>::from_stored"));
        assert!(!driver.contains("type testapi__Array<"));
        assert!(!driver.contains("Clone + 'static"));
    }

    #[test]
    fn role_and_opaque_fixtures_render_from_contract_tags() {
        assert_eq!(rust_native_for_fixture(RoleFixture::String), "String");
        assert_eq!(rust_native_for_fixture(RoleFixture::I32), "i32");

        let protocol = RunnerProtocol::NewtypeVisibilityFacade;
        let host = host_api_for_protocol(protocol);
        let driver = build_driver_for_protocol("test_pkg", &host, protocol)
            .expect("NewtypeVisibilityFacade has an executable Rust driver");
        assert!(driver.contains("struct SelectedI32(i32);"));
        assert!(driver.contains("type testapi__I32 = SelectedI32;"));
        assert!(driver.contains("readExistentialUnit_continuation_impl<"));
        assert!(driver.contains("readExistentialUnit_continuation::<"));
        assert!(driver.contains("::new(Select89)"));
        assert!(driver.contains("readExistentialEmpty_continuation_impl<"));
        assert!(driver.contains("readExistentialEmpty_continuation::<"));
        assert!(driver.contains("::new(Select90)"));

        let protocol = RunnerProtocol::HostTypeRoundtrip;
        let host = host_api_for_protocol(protocol);
        let driver = build_driver("test_pkg", &host, "    let _ = pkg;\n", protocol);
        assert!(driver.contains("struct SelectedString(String);"));
        assert!(driver.contains("type testapi__Count = SelectedI32;"));
        assert!(driver.contains("type testapi__I32 = SelectedI32;"));
        assert!(driver.contains("type testapi__String = SelectedString;"));
        assert!(driver.contains("fn wrapping_add(self, other: Self) -> Self"));
        assert!(driver.contains("arg0.to_string().into()"));
        assert!(driver.contains("arg0.0"));
        assert!(driver.contains("arg0.into()"));
    }

    #[test]
    fn wide_callable_driver_invokes_every_exact_slot_once() {
        let protocol = RunnerProtocol::ExportWideCallable;
        let host = host_api_for_protocol(protocol);
        let driver = build_driver_for_protocol("test_pkg", &host, protocol)
            .expect("wide-callable protocol has an invocable Rust driver");

        assert_eq!(driver.matches("#![recursion_limit = \"512\"]").count(), 1);
        let call_prefix = "let selected = pkg.testapi.main.select(";
        assert_eq!(driver.matches(call_prefix).count(), 1);
        let arguments = driver
            .split_once(call_prefix)
            .and_then(|(_, rest)| rest.split_once(");"))
            .map(|(arguments, _)| arguments)
            .expect("wide-callable driver contains the exact select call")
            .split(", ")
            .collect::<Vec<_>>();
        assert_eq!(arguments.len(), WIDE_CALLABLE_SLOT_COUNT);
        assert_eq!(arguments[0], "0");
        assert_eq!(arguments[127], "127");
        assert_eq!(arguments[254], "254");
        let callback_prefix = ".makeSelect().call(";
        assert_eq!(driver.matches(callback_prefix).count(), 1);
        let callback_argument = driver
            .split_once(callback_prefix)
            .and_then(|(_, rest)| rest.split_once(");"))
            .map(|(argument, _)| argument)
            .expect("wide-callable driver contains the exact callback call");
        assert!(
            callback_argument
                .starts_with("test_pkg::shapes::Product { _0: 0, _1: test_pkg::shapes::Product {")
        );
        assert_eq!(
            callback_argument
                .matches("test_pkg::shapes::Product {")
                .count(),
            WIDE_CALLABLE_SLOT_COUNT - 1
        );
        assert!(callback_argument.contains("test_pkg::shapes::Product { _0: 253, _1: 254 }"));
        assert!(!callback_argument.starts_with("0, 1"));
        assert!(driver.contains("println!(\"{}\", selected._0);"));
        assert!(driver.contains("println!(\"{}\", selected._1._0);"));
        assert!(driver.contains("println!(\"{}\", selected._1._1);"));
        assert!(driver.contains("println!(\"{}\", callback_selected._0);"));
        assert!(driver.contains("println!(\"{}\", callback_selected._1._0);"));
        assert!(driver.contains("println!(\"{}\", callback_selected._1._1);"));

        let ordinary_protocol = RunnerProtocol::Empty;
        let ordinary_host = host_api_for_protocol(ordinary_protocol);
        let ordinary_driver = build_driver(
            "test_pkg",
            &ordinary_host,
            "    let _ = pkg;\n",
            ordinary_protocol,
        );
        assert!(!ordinary_driver.contains("#![recursion_limit"));
    }

    #[test]
    fn exact_role_identity_mutation_is_rejected_by_rustc() {
        let selected_i32 = HostRoleRef::new("testapi", "I32", RoleFixture::I32);
        let host_types = [
            HostTypeBinding::selected_role("testapi", "I32", RoleFixture::I32),
            HostTypeBinding::role("testapi", "Int", RoleFixture::I32),
        ];
        let binding = HostFnBinding {
            module: "testapi/io",
            leaf: "print_i32",
            body: HostFnBodyKind::PrintI32 {
                value: selected_i32,
            },
        };
        let method = rust_host_method(&binding, &host_types);
        assert_eq!(method.arg_types, ["SelectedI32"]);
        assert_eq!(
            protocol::rust_native_role_type(
                HostRoleRef::new("testapi", "Int", RoleFixture::I32),
                &host_types,
                false,
            ),
            "i32"
        );

        let selected_member = host_api::rust_host_member("testapi", "I32");
        let ordinary_member = host_api::rust_host_member("testapi", "Int");
        let compile = |trait_argument: &str| {
            let dir = tempfile::TempDir::new().expect("temporary compile directory");
            let source = dir.path().join("lib.rs");
            let output = dir.path().join("libfixture.rlib");
            fs::write(
                &source,
                format!(
                    "#![allow(non_camel_case_types, non_snake_case)]\n\
                     struct SelectedI32(i32);\n\
                     trait Host {{\n\
                         type {selected_member};\n\
                         type {ordinary_member};\n\
                         fn {method_name}(_value: Self::{trait_argument});\n\
                     }}\n\
                     struct StubHost;\n\
                     impl Host for StubHost {{\n\
                         type {selected_member} = SelectedI32;\n\
                         type {ordinary_member} = i32;\n\
                         fn {method_name}(_value: {impl_argument}) {{}}\n\
                     }}\n",
                    method_name = method.name,
                    impl_argument = method.arg_types[0],
                ),
            )
            .expect("write mutation fixture");
            Command::new("rustc")
                .args(["--edition", "2024", "--crate-type", "rlib"])
                .arg(&source)
                .arg("-o")
                .arg(&output)
                .output()
                .expect("run rustc mutation fixture")
        };

        let exact = compile(&selected_member);
        assert!(
            exact.status.success(),
            "exact associated member should compile:\n{}",
            String::from_utf8_lossy(&exact.stderr)
        );

        let mutated = compile(&ordinary_member);
        assert!(!mutated.status.success());
        assert!(
            String::from_utf8_lossy(&mutated.stderr).contains("incompatible type for trait"),
            "wrong associated member failed for an unexpected reason:\n{}",
            String::from_utf8_lossy(&mutated.stderr)
        );
    }

    #[test]
    fn opaque_methods_keep_exact_qualified_type_members_distinct() {
        let host_types = [
            HostTypeBinding::opaque("wrong", "Shared", 1, HostTypeFixture::Box),
            HostTypeBinding::opaque("right/path", "Shared", 1, HostTypeFixture::Box),
        ];
        let binding = HostFnBinding {
            module: "testapi/box",
            leaf: "make",
            body: HostFnBodyKind::BoxMake {
                box_type: HostTypeIdentity::new("right/path", "Shared"),
            },
        };

        let method = rust_host_method(&binding, &host_types);
        assert_eq!(
            method.ret_type,
            "crate::ffi::env::testapi_box__make::ret<Self, t>"
        );
        assert_ne!(
            host_api::rust_host_member("wrong", "Shared"),
            host_api::rust_host_member("right/path", "Shared")
        );
    }

    #[test]
    fn structural_custom_bodies_do_not_depend_on_function_leaf() {
        let i32 = HostRoleRef::new("testapi", "I32", RoleFixture::I32);
        let string = HostRoleRef::new("testapi", "String", RoleFixture::String);
        let make_pair = HostFnBinding {
            module: "renamed",
            leaf: "not_make_pair",
            body: HostFnBodyKind::MakePairStructural { i32, string },
        };
        let make_pair_method = TraitMethod {
            name: host_api::rust_host_member(make_pair.module, make_pair.leaf),
            type_params: Vec::new(),
            arg_types: Vec::new(),
            ret_type: "Pair<i32, String>".to_owned(),
            where_clause: String::new(),
        };
        assert_eq!(
            render_rust_body_for_binding(&make_pair, &make_pair_method),
            "Pair::<i32, String> { _0: arg0, _1: arg1.to_string() }"
        );

        let sum = HostFnBinding {
            module: "renamed",
            leaf: "not_sum_to_string",
            body: HostFnBodyKind::SumToString { i32, string },
        };
        let sum_method = TraitMethod {
            name: host_api::rust_host_member(sum.module, sum.leaf),
            type_params: Vec::new(),
            arg_types: vec!["crate::ffi::env::renamed__not_sum_to_string::arg0<Self>".to_owned()],
            ret_type: String::new(),
            where_clause: String::new(),
        };
        let body = render_rust_body_for_binding(&sum, &sum_method);
        assert!(body.contains("renamed__not_sum_to_string::arg0::<Self>::Left"));
        assert!(body.contains("renamed__not_sum_to_string::arg0::<Self>::Right"));
    }

    #[test]
    fn sum_variant_paths_preserve_the_complete_alias_instantiation() {
        assert_eq!(
            sum_variant_ctors("crate::ffi::env::member::ret<Self>"),
            (
                "crate::ffi::env::member::ret::<Self>::Left".to_owned(),
                "crate::ffi::env::member::ret::<Self>::Right".to_owned(),
            )
        );
        assert_eq!(
            sum_variant_ctors("crate::ffi::env::member::ret::<s, r>"),
            (
                "crate::ffi::env::member::ret::<s, r>::Left".to_owned(),
                "crate::ffi::env::member::ret::<s, r>::Right".to_owned(),
            )
        );
    }

    #[test]
    fn facade_collision_driver_cases_source_module_components() {
        let protocol = RunnerProtocol::FacadeSelectorCollisions;
        let host = host_api_for_protocol(protocol);
        let driver = build_driver_for_protocol("test_pkg", &host, protocol)
            .expect("facade-selector-collisions driver");
        assert!(
            driver.contains("pkg.testapi.modApiValue.value()"),
            "{driver}"
        );
        assert!(
            driver.contains("pkg.testapi.fooBar.value(10, 2)"),
            "{driver}"
        );
        assert!(!driver.contains("pkg.testapi.mod_api_value"), "{driver}");
        assert!(!driver.contains("pkg.testapi.foo_bar"), "{driver}");
    }

    #[test]
    fn public_word_names_driver_pins_affixes_and_nested_selectors() {
        let protocol = RunnerProtocol::PublicWordNames;
        let host = host_api_for_protocol(protocol);
        let driver = build_driver_for_protocol("test_pkg", &host, protocol)
            .expect("public word names protocol has an invocable Rust driver");
        for literal in [
            "pkg.wordApi.readWord()",
            "pkg.wordApi._readWord()",
            "pkg.wordApi.readWord_()",
            "pkg.wordApi._readWord_()",
            "pkg.wordApi.readWord__()",
            "nominal::wordApi::wordNodes::_WordBox__::<StubHost, test_pkg::shapes::KioNative<i32>>::wrapWord(55)",
            "nominal::wordApi::wordNodes::_WordBox__::<StubHost, test_pkg::shapes::KioNative<i32>>::unwrapWord(boxed)",
            "pkg.wordApi.wordNodes.keepWord::<test_pkg::shapes::KioNative<i32>>(66)",
            "pkg.wordApi.keepPair(test_pkg::shapes::Product { _0:",
            "type CountValue = test_pkg::shapes::KioHostTypeMarker_wordApi__CountValue<StubHost>;",
            "nominal::wordApi::otherNodes::_WordBox__::<StubHost, CountValue>::unwrapWord(pair._1)",
        ] {
            assert!(driver.contains(literal), "{driver}");
        }
    }

    #[test]
    fn loop_body_reads_ffi_alias_sum_path() {
        let protocol = RunnerProtocol::TestApiComputeLoop;
        let contract = protocol.contract();
        let host = host_api_for_protocol(protocol);
        let (binding, method) = contract
            .host_fns
            .iter()
            .zip(host.methods())
            .find(|(binding, _)| binding.body == HostFnBodyKind::Loop)
            .expect("compute-loop contract declares loop");
        let body = render_rust_body_for_binding(binding, method);
        assert!(body.contains("loop {"));
        assert!(body.contains("testapi_iter__loop::arg0_cbret::<s, r>::Left"));
        assert!(body.contains("testapi_iter__loop::arg0_cbret::<s, r>::Right"));
    }
}
