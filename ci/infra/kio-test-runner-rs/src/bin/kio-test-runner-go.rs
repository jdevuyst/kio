//! `kio-test-runner-go` — pointed at a `kio build go` output directory,
//! compiles the emitted Go package together with a synthesized driver
//! and reports the exit code.
//!
//! The package layout the emitter produces (see `specs/backends/go.md`
//! § Output layout) is a flat Go package directory: `pkg.go`,
//! `host.go`, `shapes.go`, `ffi.go`, `kio_runtime.go` — each declaring
//! the package's namespace as its `package` clause. The runner drops a
//! `go.mod` and a `main` driver beside them in a temp build dir and
//! runs `go build` + the binary.
//!
//! The corpus harness supplies the source package name and target-local
//! effective namespace independently. The selected
//! protocol is the sole semantic authority: it supplies the exact host types
//! and functions, their native fixtures and bodies, the export driver, and
//! whether the artifact is compiled, constructed, or invoked. The runner
//! never reads emitted source to discover or filter members. It renders the
//! contract as a `StubHost` and links that against the emitted package; any
//! FFI drift therefore fails at `go build`.
//!
//! Every flat boundary slot, plus recursively reached callback and
//! structural slots, is named through the emitted package's `ffi.go`
//! aliases (`<ns>.Env_<member>_<slot>` /
//! `<ns>.Exp_<member>_<slot>`). Products use those aliases as generic
//! flat structs with named fields. Sums use the alias-local
//! `New<alias>_<k>` constructors and inspect `Case()` through the
//! per-arm `<alias>_<k>` case aliases, never by scraping emitted source
//! or naming facade-global helpers.
//!
//! Pipeline (the [`TestRunner`] implementation):
//!
//!  1. `host_api` — renders the protocol's exact [`HostApi`] for Go.
//!  2. `execute_artifact` — synthesize the Go `StubHost` + `main`
//!     driver, assemble the build tree's file set (the emitted package
//!     `.go` files + the driver + the `go.mod` wiring), resolve the
//!     binary through the shared content-addressed build cache
//!     ([`go_cache`], a one-level adapter over [`build_cache`]) —
//!     `go build -trimpath` on a miss, a cache hit on a warm run — then
//!     spawn it and capture the exit.
//!
//! The harness supplies `KIO_TEST_RUNNER_BUILD_CACHE_DIR` as the
//! reusable build-artifact cache root; the binary is keyed by `{go
//! identity, build flags, all build-tree source bytes}`. `-trimpath`
//! makes the artifact path-neutral so a populated cache stays valid
//! across worktrees. `KIO_TEST_RUNNER_CACHE_DISABLE=1` uses a fresh temp
//! cache for one invocation.
//!
//! Exits 0 on success, 1 on a `go build` / runtime error; the CLI tier
//! is 2 per `specs/exit-codes.md`. A module call to host `exit(n)`
//! propagates `n` through the spawned process's exit code (clamped to
//! 0..=125 per `specs/exit-codes.md`).

use std::env;
use std::fmt::Write as _;
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
#[path = "../go/bin_cache/mod.rs"]
mod go_cache;
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

use artifact_identity::{ArtifactIdentity, ArtifactIdentityArgs, pascal_case};
use canonical::{ArrayOp, CanonicalKind};
use compiler_observer::CompilerObserver;
use go_cache::{GoBuildTree, GoCache, go_identity};
use host_api::{
    AssocType, HostApi, TraitMethod, go_host_member, go_nested_module_selector,
    go_type_handle_selector,
};
use opt_profile::OptProfile;
use protocol::{
    ExportDriver, HostFnBinding, HostFnBodyKind, HostRoleRef, HostTypeBinding, HostTypeFixture,
    HostTypeIdentity, ProtocolContract, ProtocolExecution, RoleFixture, RunnerProtocol,
    WIDE_CALLABLE_SLOT_COUNT,
};
use runner::{EXIT_RUNTIME_FAILURE, EXIT_USAGE, TestRunner};
use runner_cache_env::RunnerCacheConfig;

const USAGE: &str = "\
Usage: kio-test-runner-go [--protocol <name>] [--profile <name>] <output-dir>

Compile and run the Go package emitted by `kio build go` and report
the exit code.

Arguments:
  <output-dir>      Directory containing the emitted Go package
                    (`pkg.go`, `host.go`, `shapes.go`, `ffi.go`,
                    `kio_runtime.go`) per `specs/backends/go.md`. The
                    runner copies the files into a temp build dir
                    alongside a synthesized driver. Artifact identity
                    is supplied independently by the corpus harness.

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
                    Optional Rust compiler cache wrapper. The Go runner
                    ignores it because sccache does not wrap `go`.

  KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER
                    Optional internal debug executable placed outermost
                    around each actual `go build`. The value is one
                    opaque executable, not shell syntax.

  KIO_TEST_RUNNER_CACHE_DISABLE
                    Optional. Set to 1 to disable persistent runner
                    cache behavior and compiler wrappers; the runner
                    uses a fresh temporary cache for the invocation. The
                    debug compiler observer remains active. Empty, unset,
                    or 0 means enabled; any other value is an error.

  KIO_TEST_RUNNER_PROFILE
                    Optional. Optimization profile (`unoptimized`,
                    `default`, `optimized`). Validated for parity with
                    the other runners but a no-op for Go, which has no
                    optimization levels. Defaults to `default`.

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
                    `coexist` takes exactly two <output-dir> arguments
                    and hosts both packages in one driver program.
  --profile <name>
                    Optimization profile: `unoptimized`, `default`, or
                    `optimized`. A no-op for Go (no optimization levels);
                    accepted for cross-runner parity.
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
    let coexist = matches!(
        protocol.contract().execution,
        ProtocolExecution::Invoke(ExportDriver::Coexist)
    );
    let (dir, coexist_second) = match (coexist, positional.as_slice()) {
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
    let expected_packages = if coexist { 2 } else { 1 };
    let identities = match identity_args.resolve("go", expected_packages) {
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

    // Validate `--profile` / KIO_TEST_RUNNER_PROFILE for parity with the
    // other runners, then discard it: Go has no optimization levels
    // (`go build` takes none), so the profile is a no-op here — the
    // emitted binary and its cache key are profile-independent.
    if let Err(e) = OptProfile::resolve(profile_override) {
        eprintln!("error: {e}");
        return EXIT_USAGE;
    }

    let r = GoRunner {
        cache_config,
        compiler_observer,
        compiler_admission,
        protocol,
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

struct GoRunner {
    cache_config: RunnerCacheConfig,
    compiler_observer: CompilerObserver,
    compiler_admission: compiler_admission::CompilerAdmission,
    protocol: RunnerProtocol,
    identities: Vec<ArtifactIdentity>,
}

impl TestRunner for GoRunner {
    fn host_api(&self) -> HostApi {
        let ns = self.identities[0].namespace.as_str();
        let qualifier = go_pkg_qualifier(ns);
        let selection = GoHostBindingSelection::new(self.protocol.contract());
        go_host_api_for_protocol(self.protocol, &qualifier, &selection)
    }

    fn execute_artifact(
        &self,
        output_dir: &Path,
        host: &HostApi,
        protocol: RunnerProtocol,
    ) -> Result<i32, String> {
        let ns = self.identities[0].namespace.as_str();
        let qual = go_pkg_qualifier(ns);
        let driver = build_driver(host, protocol, ns, &qual);

        // Assemble the build tree's file set in memory: the emitted
        // package `.go` files under `<ns>/`, the synthesized driver
        // under `driver/`, and the two `go.mod`s wiring them. This same
        // set both defines the cache key and is what `produce` writes to
        // disk before `go build` — so the keyed and compiled bytes
        // coincide. The driver `package main` lives in `driver/`,
        // importing the emitted package via a `replace` directive.
        let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        collect_go_files(output_dir, ns, &mut files)?;
        files.push((
            PathBuf::from("go.mod"),
            format!(
                "module kiodriver\n\n{GO_DIRECTIVE}\n\nrequire {GO_MODULE_ROOT}/{ns} v0.0.0\n\nreplace {GO_MODULE_ROOT}/{ns} => ./{ns}\n"
            )
            .into_bytes(),
        ));
        files.push((
            PathBuf::from(format!("{ns}/go.mod")),
            format!("module {GO_MODULE_ROOT}/{ns}\n\n{GO_DIRECTIVE}\n").into_bytes(),
        ));
        files.push((PathBuf::from("driver/main.go"), driver.into_bytes()));
        files.sort_by(|a, b| a.0.cmp(&b.0));

        self.compile_and_run(files, output_dir)
    }
}

impl GoRunner {
    /// The `coexist` protocol's two-artifact execution: one driver
    /// program hosting both packages (`shared/protocol.rs` § The
    /// coexist protocol). Each package's `.go` files sit under their
    /// own module dir wired by `require` + `replace`; the driver
    /// imports both — the maximal-collision shape the namespace rule
    /// exists for — and interleaves calls through per-package prefixing
    /// hosts.
    fn run_coexist(&self, dir_a: &Path, dir_b: &Path) -> Result<i32, String> {
        let ns_a = self.identities[0].namespace.as_str();
        let ns_b = self.identities[1].namespace.as_str();
        if ns_a == ns_b {
            return Err(format!(
                "coexist requires two distinct package namespaces; both artifacts are `{ns_a}`"
            ));
        }
        let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        collect_go_files(dir_a, ns_a, &mut files)?;
        collect_go_files(dir_b, ns_b, &mut files)?;
        files.push((
            PathBuf::from("go.mod"),
            format!(
                "module kiodriver\n\n{GO_DIRECTIVE}\n\nrequire (\n\t{GO_MODULE_ROOT}/{ns_a} v0.0.0\n\t{GO_MODULE_ROOT}/{ns_b} v0.0.0\n)\n\nreplace {GO_MODULE_ROOT}/{ns_a} => ./{ns_a}\n\nreplace {GO_MODULE_ROOT}/{ns_b} => ./{ns_b}\n"
            )
            .into_bytes(),
        ));
        for ns in [ns_a, ns_b] {
            files.push((
                PathBuf::from(format!("{ns}/go.mod")),
                format!("module {GO_MODULE_ROOT}/{ns}\n\n{GO_DIRECTIVE}\n").into_bytes(),
            ));
        }
        files.push((
            PathBuf::from("driver/main.go"),
            build_coexist_driver(ns_a, ns_b).into_bytes(),
        ));
        files.sort_by(|a, b| a.0.cmp(&b.0));

        self.compile_and_run(files, dir_a)
    }

    /// Resolve the assembled build tree through the shared cache and
    /// run the produced binary. Shared by the one-artifact path and the
    /// coexist path; `context_dir` only labels errors.
    fn compile_and_run(
        &self,
        files: Vec<(PathBuf, Vec<u8>)>,
        context_dir: &Path,
    ) -> Result<i32, String> {
        // Resolve the binary through the shared cache. A warm hit skips
        // `go build` entirely; a miss compiles into a staging tempdir and
        // atomic-renames into place.
        //
        // `compiler_wrapper` (typically `sccache`) is deliberately
        // *not* threaded into the `go` invocation: sccache wraps
        // C/C++/rustc-shaped compilers and rejects `go` (it passes `-E`,
        // which `go` doesn't accept). Like the JS runner, the go runner
        // ignores the wrapper. The shared build cache is the runner's
        // acceleration layer here.
        let disabled_cache_temp;
        let (cache_dir, max_bytes, cache_label) = match &self.cache_config {
            RunnerCacheConfig::Persistent {
                cache_dir,
                compiler_wrapper: _,
                max_bytes,
            } => (cache_dir.clone(), *max_bytes, "go build cache"),
            RunnerCacheConfig::Disabled => {
                disabled_cache_temp = tempfile::TempDir::new()
                    .map_err(|e| format!("cannot create disabled-cache tempdir: {e}"))?;
                (
                    disabled_cache_temp.path().to_path_buf(),
                    None,
                    "disabled-cache temp go cache",
                )
            }
        };
        let cache = GoCache::open(
            cache_dir.clone(),
            None,
            self.compiler_observer.clone(),
            max_bytes,
            self.compiler_admission.clone(),
        )
        .map_err(|e| format!("cannot open {cache_label} at {}: {e}", cache_dir.display()))?;

        let go_id = go_identity(&cache_dir).map_err(|e| format!("probing go identity: {e}"))?;
        let tree = GoBuildTree {
            go_identity: go_id,
            go_directive: GO_DIRECTIVE.to_owned(),
            files,
            build_package: "driver".to_owned(),
        };

        let bin_path = cache
            .get_or_compile_bin(&tree)
            .map_err(|e| format!("building Go bin for {}: {e}", context_dir.display()))?;

        let status = Command::new(&bin_path)
            .status()
            .map_err(|e| format!("spawning driver bin {}: {e}", bin_path.display()))?;
        Ok(status.code().unwrap_or(EXIT_RUNTIME_FAILURE))
    }
}

/// The `go 1.NN` directive the runner writes into both `go.mod`s. Folded
/// into the cache key (see the go adapter's `build_flags`) since it can
/// affect codegen.
const GO_DIRECTIVE: &str = "go 1.26";

/// The module-path root for the synthesized `go.mod`s. A dotted first
/// element can never collide with a standard-library import path, which
/// a bare `module <ns>` did for stdlib-named namespaces (`runtime`,
/// `sort`, ...).
const GO_MODULE_ROOT: &str = "kio.local";

/// The Go source-level qualifier the driver binds the artifact package
/// to: normally the namespace itself (spelled as an explicit import
/// alias equal to the package's own name), suffixed only when the
/// namespace collides with a package the driver itself imports -- the
/// import path is unaffected.
fn go_pkg_qualifier(ns: &str) -> String {
    const DRIVER_IMPORTS: &[&str] = &["big", "bufio", "fmt", "os", "strconv", "strings"];
    if DRIVER_IMPORTS.contains(&ns) {
        format!("{ns}_pkg")
    } else {
        ns.to_owned()
    }
}

/// The `coexist` driver: import both packages, each bound to its own
/// name (their branded names never collide — the ergonomics the
/// protocol witnesses),
/// instantiate each against a host whose `print` prefixes the package's
/// namespace, and interleave the calls first → second → first.
fn build_coexist_driver(ns_a: &str, ns_b: &str) -> String {
    let (qual_a, qual_b) = (go_pkg_qualifier(ns_a), go_pkg_qualifier(ns_b));
    let selection = GoHostBindingSelection::new(RunnerProtocol::Coexist.contract());
    let mut out = String::new();
    out.push_str("package main\n\nimport (\n\t\"fmt\"\n\n");
    out.push_str(&format!(
        "\t{qual_a} \"{GO_MODULE_ROOT}/{ns_a}\"\n\t{qual_b} \"{GO_MODULE_ROOT}/{ns_b}\"\n)\n\n"
    ));
    for (tag, prefix) in [("A", "first"), ("B", "second")] {
        out.push_str(&format!(
            "type host{tag} struct{{}}\n\nfunc (host{tag}) Greeter__print(arg0 string) {{\n\tfmt.Print(\"{prefix}: \" + arg0)\n}}\n\n"
        ));
        selection.render_role_adapters_for(&format!("host{tag}"), &mut out);
    }
    let (handle_a, handle_b) = (pascal_case(ns_a), pascal_case(ns_b));
    let root_arguments = selection.root_arguments();
    out.push_str("func main() {\n");
    out.push_str(&format!(
        "\tpa := {qual_a}.Create{handle_a}{root_arguments}(hostA{{}})\n"
    ));
    out.push_str(&format!(
        "\tpb := {qual_b}.Create{handle_b}{root_arguments}(hostB{{}})\n"
    ));
    // The structural-facade half of the witness: both packages export the
    // same `pair() -> (I32 & String)`, so both artifacts instantiate the
    // same generic positional product shape inside distinct package
    // namespaces; reading `.F0` / `.F1` from both proves they coexist.
    out.push_str(
        "\tpa.Greeter.KioModule_main.Main()\n\tpb.Greeter.KioModule_main.Main()\n\tpa.Greeter.KioModule_main.Main()\n\tqa := pa.Greeter.KioModule_main.Pair()\n\tfmt.Println(\"first pair:\", qa.F0, qa.F1)\n\tqb := pb.Greeter.KioModule_main.Pair()\n\tfmt.Println(\"second pair:\", qb.F0, qb.F1)\n\tqa = pa.Greeter.KioModule_main.Pair()\n\tfmt.Println(\"first pair:\", qa.F0, qa.F1)\n}\n",
    );
    out
}

/// Collect the emitted Go package's `*.go` files into `files`, each at
/// relative path `<rel_prefix>/<name>`. The runner reads them only as
/// opaque bytes to compile — never to discover the host API.
fn collect_go_files(
    src: &Path,
    rel_prefix: &str,
    files: &mut Vec<(PathBuf, Vec<u8>)>,
) -> Result<(), String> {
    let entries = fs::read_dir(src).map_err(|e| format!("reading {}: {e}", src.display()))?;
    let mut copied = 0usize;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().is_some_and(|x| x == "go") {
            let name = path.file_name().unwrap();
            let bytes = fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
            files.push((PathBuf::from(rel_prefix).join(name), bytes));
            copied += 1;
        }
    }
    if copied == 0 {
        return Err(format!("no .go files in {}", src.display()));
    }
    Ok(())
}

// =========================================================================
// Host API construction (Go-typed).
// =========================================================================

struct GoHostBindingSelection<'a> {
    contract: ProtocolContract,
    roots: Vec<&'a HostTypeBinding>,
}

impl<'a> GoHostBindingSelection<'a> {
    fn new(contract: ProtocolContract) -> Self {
        let mut roots = contract
            .host_types
            .iter()
            .filter(|binding| binding.type_arity == 0)
            .collect::<Vec<_>>();
        roots.sort_by(|left, right| {
            left.module
                .split('/')
                .cmp(right.module.split('/'))
                .then_with(|| left.leaf.cmp(right.leaf))
        });
        Self { contract, roots }
    }

    fn root_arguments(&self) -> String {
        if self.roots.is_empty() {
            return String::new();
        }
        format!(
            "[{}]",
            self.roots
                .iter()
                .map(|binding| go_runner_host_type(binding))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    fn binding(&self, identity: HostTypeIdentity) -> &HostTypeBinding {
        self.contract
            .host_types
            .iter()
            .find(|binding| binding.module == identity.module && binding.leaf == identity.leaf)
            .unwrap_or_else(|| {
                unreachable!(
                    "Go protocol references undeclared host type `{}/{}`",
                    identity.module, identity.leaf
                )
            })
    }

    fn host_type(&self, identity: HostTypeIdentity) -> String {
        let binding = self.binding(identity);
        assert_eq!(
            binding.type_arity, 0,
            "a direct Go exact host binding is nullary"
        );
        go_runner_host_type(binding)
    }

    fn role_type(&self, role: HostRoleRef) -> String {
        go_runner_host_type(role.resolve(self.contract.host_types))
    }

    fn role_into_native(&self, role: HostRoleRef, expression: &str) -> String {
        match role.resolve(self.contract.host_types).fixture {
            HostTypeFixture::SelectedRole(fixture) => {
                format!("{}({expression})", go_native_role_type(fixture))
            }
            HostTypeFixture::Role(_) => expression.to_owned(),
            other => unreachable!("role reference resolved to non-role fixture {other:?}"),
        }
    }

    fn role_from_native(&self, role: HostRoleRef, expression: &str) -> String {
        match role.resolve(self.contract.host_types).fixture {
            HostTypeFixture::SelectedRole(_) => {
                format!("{}({expression})", self.role_type(role))
            }
            HostTypeFixture::Role(_) => expression.to_owned(),
            other => unreachable!("role reference resolved to non-role fixture {other:?}"),
        }
    }

    fn render_selected_types(&self, out: &mut String) {
        for binding in &self.roots {
            if let HostTypeFixture::SelectedRole(role) = binding.fixture {
                writeln!(
                    out,
                    "type {} {}\n",
                    go_runner_selected_type(binding),
                    go_native_role_type(role)
                )
                .expect("writing to String cannot fail");
            }
        }
    }

    fn render_role_adapters(&self, out: &mut String) {
        self.render_role_adapters_for("StubHost", out);
    }

    fn render_role_adapters_for(&self, receiver: &str, out: &mut String) {
        for binding in &self.roots {
            let role = match binding.fixture {
                HostTypeFixture::Role(role) | HostTypeFixture::SelectedRole(role) => role,
                _ => continue,
            };
            let adapter = go_role_adapter_identity(binding.module, binding.leaf);
            let selected = go_runner_host_type(binding);
            let native = go_native_role_type(role);
            let into_native = if selected == native {
                "value".to_owned()
            } else {
                format!("{native}(value)")
            };
            let from_native = if selected == native {
                "value".to_owned()
            } else {
                format!("{selected}(value)")
            };
            writeln!(
                out,
                "func ({receiver}) KioHostIn_{adapter}(value {selected}) {native} {{ return {into_native} }}"
            )
            .expect("writing to String cannot fail");
            writeln!(
                out,
                "func ({receiver}) KioHostOut_{adapter}(value {native}) {selected} {{ return {from_native} }}\n"
            )
            .expect("writing to String cannot fail");
        }
    }
}

fn go_role_adapter_identity(module: &str, leaf: &str) -> String {
    let components = module
        .split('/')
        .chain(std::iter::once(leaf))
        .map(host_api::host_name_core)
        .collect::<Vec<_>>();
    if module.is_empty()
        || module.split('/').next() == Some("V1")
        || components.iter().any(|component| {
            component.is_empty() || !component.bytes().all(|b| b.is_ascii_alphanumeric())
        })
    {
        go_host_type_frame(module, leaf)
    } else {
        components.join("_")
    }
}

fn go_host_type_frame(module: &str, leaf: &str) -> String {
    let segments = if module.is_empty() {
        Vec::new()
    } else {
        module.split('/').collect::<Vec<_>>()
    };
    let mut frame = format!("V1_M{}_", segments.len());
    for segment in segments {
        let segment = go_escape_identity_component(segment);
        frame.push('C');
        frame.push_str(&segment.len().to_string());
        frame.push('_');
        frame.push_str(&segment);
    }
    let leaf = go_escape_identity_component(leaf);
    frame.push('N');
    frame.push_str(&leaf.len().to_string());
    frame.push('_');
    frame.push_str(&leaf);
    frame
}

fn go_escape_identity_component(component: &str) -> String {
    let component = host_api::host_name_core(component);
    let mut escaped = String::new();
    for byte in component.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => escaped.push(char::from(byte)),
            b'_' => escaped.push_str("_u"),
            _ => write!(&mut escaped, "_x{byte:02x}").expect("writing to String cannot fail"),
        }
    }
    escaped
}

fn go_runner_selected_type(binding: &HostTypeBinding) -> String {
    format!(
        "KioHostFixture_{}",
        go_host_type_frame(binding.module, binding.leaf)
    )
}

fn go_runner_host_type(binding: &HostTypeBinding) -> String {
    match binding.fixture {
        HostTypeFixture::Role(role) => go_native_role_type(role).to_owned(),
        HostTypeFixture::SelectedRole(_) => go_runner_selected_type(binding),
        HostTypeFixture::Token | HostTypeFixture::Scalar => "any".to_owned(),
        HostTypeFixture::Array | HostTypeFixture::Box => {
            unreachable!("a parameterized Go host fixture is not a package-root type argument")
        }
    }
}

fn go_native_role_type(role: RoleFixture) -> &'static str {
    match role {
        RoleFixture::I8 => "int8",
        RoleFixture::I16 => "int16",
        RoleFixture::I32 => "int32",
        RoleFixture::I64 => "int64",
        RoleFixture::I128 | RoleFixture::U128 => "*big.Int",
        RoleFixture::U8 => "uint8",
        RoleFixture::U16 => "uint16",
        RoleFixture::U32 => "uint32",
        RoleFixture::U64 => "uint64",
        RoleFixture::F32 => "float32",
        RoleFixture::F64 => "float64",
        RoleFixture::Bool => "bool",
        RoleFixture::String => "string",
    }
}

/// The protocol's exact [`HostApi`], rendered for Go.
fn go_host_api_for_protocol(
    protocol: RunnerProtocol,
    qualifier: &str,
    selection: &GoHostBindingSelection<'_>,
) -> HostApi {
    let contract = protocol.contract();
    host_api::project_host_api(
        contract,
        |binding| AssocType {
            name: binding.leaf.to_owned(),
            boundary_name: (!binding.module.is_empty())
                .then(|| go_host_member(binding.module, binding.leaf)),
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
        |binding| go_method(binding, qualifier, selection),
    )
}

fn go_method(
    binding: &HostFnBinding,
    qualifier: &str,
    selection: &GoHostBindingSelection<'_>,
) -> TraitMethod {
    match binding.body {
        HostFnBodyKind::CallStep { i32, string, bool_ } => {
            let i32 = selection.role_type(i32);
            let callback = format!(
                "func({}, {}, {}) {i32}",
                i32,
                selection.role_type(string),
                selection.role_type(bool_)
            );
            go_method_with_types(binding, vec![callback, i32.clone()], i32)
        }
        HostFnBodyKind::MakePairCallback { i32, .. } => {
            let i32 = selection.role_type(i32);
            go_method_with_types(
                binding,
                vec![
                    env_alias(
                        qualifier,
                        binding.module,
                        binding.leaf,
                        "arg0",
                        &selection.root_arguments(),
                    ),
                    i32.clone(),
                ],
                i32,
            )
        }
        HostFnBodyKind::StagedSecond { string } => {
            let string = selection.role_type(string);
            go_method_with_types(binding, vec![string.clone(), string.clone()], string)
        }
        HostFnBodyKind::MakePairStructural { i32, string } => go_method_with_types(
            binding,
            vec![selection.role_type(i32), selection.role_type(string)],
            env_alias(
                qualifier,
                binding.module,
                binding.leaf,
                "ret",
                &selection.root_arguments(),
            ),
        ),
        HostFnBodyKind::ProducePair { .. } => go_method_with_types(
            binding,
            Vec::new(),
            env_alias(
                qualifier,
                binding.module,
                binding.leaf,
                "ret",
                &selection.root_arguments(),
            ),
        ),
        HostFnBodyKind::MakeStep { i32 } => go_method_with_types(
            binding,
            vec![selection.role_type(i32)],
            env_alias(
                qualifier,
                binding.module,
                binding.leaf,
                "ret",
                &selection.root_arguments(),
            ),
        ),
        HostFnBodyKind::ApplyPoly { string } => go_method_with_types(
            binding,
            vec![env_alias(
                qualifier,
                binding.module,
                binding.leaf,
                "arg0",
                &selection.root_arguments(),
            )],
            selection.role_type(string),
        ),
        HostFnBodyKind::SumToString { string, .. } => go_method_with_types(
            binding,
            vec![env_alias(
                qualifier,
                binding.module,
                binding.leaf,
                "arg0",
                &selection.root_arguments(),
            )],
            selection.role_type(string),
        ),
        HostFnBodyKind::BoxMake { .. }
        | HostFnBodyKind::BoxGet { .. }
        | HostFnBodyKind::RoundFunctor
        | HostFnBodyKind::RoundPicker
        | HostFnBodyKind::RoundPolyThunk
        | HostFnBodyKind::RoundPolyUnitSlot
        | HostFnBodyKind::NestedCurriedRoundtrip { .. } => {
            go_alias_method(binding, qualifier, selection, 1, true)
        }
        HostFnBodyKind::InvokeSubstitutedUnitCallback { text } => go_method_with_types(
            binding,
            vec![env_alias(
                qualifier,
                binding.module,
                binding.leaf,
                "arg0",
                &selection.root_arguments(),
            )],
            selection.role_type(text),
        ),
        HostFnBodyKind::ReturnedForallUnit => {
            go_alias_method(binding, qualifier, selection, 0, true)
        }
        HostFnBodyKind::ObservePacked { i32 } => go_method_with_types(
            binding,
            vec![env_alias(
                qualifier,
                binding.module,
                binding.leaf,
                "arg0",
                &selection.root_arguments(),
            )],
            selection.role_type(i32),
        ),
        HostFnBodyKind::TraceUnit { .. } => {
            go_alias_method(binding, qualifier, selection, 0, false)
        }
        HostFnBodyKind::StagedUnitCall => go_alias_method(binding, qualifier, selection, 1, false),
        HostFnBodyKind::MakeToken { value_i32, token } => go_method_with_types(
            binding,
            vec![selection.role_type(value_i32)],
            selection.host_type(token),
        ),
        HostFnBodyKind::TokenValue { token, value_i32 } => go_method_with_types(
            binding,
            vec![selection.host_type(token)],
            selection.role_type(value_i32),
        ),
        HostFnBodyKind::UnreachableI32Print { i32 } => {
            go_method_with_types(binding, vec![selection.role_type(i32)], "()".to_owned())
        }
        _ => go_canonical_method(binding, qualifier, selection),
    }
}

fn go_method_with_types(
    binding: &HostFnBinding,
    arg_types: Vec<String>,
    ret_type: String,
) -> TraitMethod {
    let name = if binding.module.is_empty() {
        host_api::go_export_capitalize(binding.leaf)
    } else {
        go_host_member(binding.module, binding.leaf)
    };
    TraitMethod {
        name,
        type_params: Vec::new(),
        arg_types,
        ret_type,
        where_clause: String::new(),
    }
}

/// Project one host method entirely through its stable boundary-slot aliases.
/// The protocol identity supplies the alias owner and flat value-slot count;
/// no fixture type or emitted declaration is inspected to recover a spelling.
fn go_alias_method(
    binding: &HostFnBinding,
    qualifier: &str,
    selection: &GoHostBindingSelection<'_>,
    arg_count: usize,
    has_return: bool,
) -> TraitMethod {
    let name = if binding.module.is_empty() {
        host_api::go_export_capitalize(binding.leaf)
    } else {
        go_host_member(binding.module, binding.leaf)
    };
    TraitMethod {
        name,
        type_params: Vec::new(),
        arg_types: (0..arg_count)
            .map(|index| {
                env_alias(
                    qualifier,
                    binding.module,
                    binding.leaf,
                    &format!("arg{index}"),
                    &selection.root_arguments(),
                )
            })
            .collect(),
        ret_type: if has_return {
            env_alias(
                qualifier,
                binding.module,
                binding.leaf,
                "ret",
                &selection.root_arguments(),
            )
        } else {
            "()".to_owned()
        },
        where_clause: String::new(),
    }
}

/// Build one canonical host fn's Go-typed [`TraitMethod`] for leaf
/// `leaf` declared in `module` (the namespaced testapi submodule, or
/// `None` for a bare env). Structural, function, and sum slots use the
/// emitted package's stable `ffi.go` aliases. Direct exact host roots use the
/// package handle's declaration-keyed type parameters instead.
fn go_canonical_method(
    binding: &HostFnBinding,
    qualifier: &str,
    selection: &GoHostBindingSelection<'_>,
) -> TraitMethod {
    match binding.body {
        HostFnBodyKind::Print { string } | HostFnBodyKind::Eprint { string } => {
            go_method_with_types(binding, vec![selection.role_type(string)], "()".to_owned())
        }
        HostFnBodyKind::PrintI32 { value } => {
            go_method_with_types(binding, vec![selection.role_type(value)], "()".to_owned())
        }
        HostFnBodyKind::ReadAsciiLine { .. } => {
            go_alias_method(binding, qualifier, selection, 0, true)
        }
        HostFnBodyKind::StringConcat { string } => {
            let string = selection.role_type(string);
            go_method_with_types(binding, vec![string.clone(), string.clone()], string)
        }
        HostFnBodyKind::StringEq { string, bool_ } => {
            let string = selection.role_type(string);
            go_method_with_types(
                binding,
                vec![string.clone(), string],
                selection.role_type(bool_),
            )
        }
        HostFnBodyKind::StringCodeAt { string, index } => go_method_with_types(
            binding,
            vec![selection.role_type(string), selection.role_type(index)],
            env_alias(
                qualifier,
                binding.module,
                binding.leaf,
                "ret",
                &selection.root_arguments(),
            ),
        ),
        HostFnBodyKind::Arithmetic { number, .. }
        | HostFnBodyKind::FloatArithmetic { number, .. } => {
            let number = selection.role_type(number);
            go_method_with_types(binding, vec![number.clone(), number.clone()], number)
        }
        HostFnBodyKind::Compare { number, bool_, .. } => {
            let number = selection.role_type(number);
            go_method_with_types(
                binding,
                vec![number.clone(), number],
                selection.role_type(bool_),
            )
        }
        HostFnBodyKind::Loop => go_alias_method(binding, qualifier, selection, 2, true),
        HostFnBodyKind::StringSlice { string, index } => go_method_with_types(
            binding,
            vec![
                selection.role_type(string),
                selection.role_type(index),
                selection.role_type(index),
            ],
            selection.role_type(string),
        ),
        HostFnBodyKind::Exit { status_i32 } => go_method_with_types(
            binding,
            vec![selection.role_type(status_i32)],
            env_alias(
                qualifier,
                binding.module,
                binding.leaf,
                "ret",
                &selection.root_arguments(),
            ),
        ),
        HostFnBodyKind::NumericToString { value, string } => go_method_with_types(
            binding,
            vec![selection.role_type(value)],
            selection.role_type(string),
        ),
        HostFnBodyKind::BoolToString { bool_, string } => go_method_with_types(
            binding,
            vec![selection.role_type(bool_)],
            selection.role_type(string),
        ),
        HostFnBodyKind::StringLen { string, index } => go_method_with_types(
            binding,
            vec![selection.role_type(string)],
            selection.role_type(index),
        ),
        HostFnBodyKind::StringToInt { string, .. } => go_method_with_types(
            binding,
            vec![selection.role_type(string)],
            env_alias(
                qualifier,
                binding.module,
                binding.leaf,
                "ret",
                &selection.root_arguments(),
            ),
        ),
        // dyn_load_prime's opaque-scalar surface. `Scalar` is a role-less
        // `host type` — an opaque host-owned box that erases to `any` at the
        // Go boundary (the runner boxes the native value directly; see
        // `render_go_body`). `make_scalar` / `scalar_of_*` build a `Scalar`;
        // `scalar_as_*` project a `. | <Kind>` sum (named via the ffi `ret`
        // alias); `scalar_is_true` tests an opaque bool.
        HostFnBodyKind::Array { operation, .. } => {
            go_array_method(binding, qualifier, selection, operation)
        }
        HostFnBodyKind::MakeScalar { string, scalar } => {
            let string = selection.role_type(string);
            go_method_with_types(
                binding,
                vec![string.clone(), string],
                selection.host_type(scalar),
            )
        }
        HostFnBodyKind::ScalarOf { value, scalar } => go_method_with_types(
            binding,
            vec![selection.role_type(value)],
            selection.host_type(scalar),
        ),
        HostFnBodyKind::ScalarAs { scalar, .. } => go_method_with_types(
            binding,
            vec![selection.host_type(scalar)],
            env_alias(
                qualifier,
                binding.module,
                binding.leaf,
                "ret",
                &selection.root_arguments(),
            ),
        ),
        HostFnBodyKind::ScalarIsTrue { scalar, bool_ } => go_method_with_types(
            binding,
            vec![selection.host_type(scalar)],
            selection.role_type(bool_),
        ),
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
                "bespoke Go host body `{}` reached canonical signature rendering",
                binding.leaf
            )
        }
    }
}

/// One `array_*` host fn's Go-typed method. Parametric Array/element slots use
/// their prepared aliases; direct index roots use the declaration-keyed exact
/// type selected by the protocol.
fn go_array_method(
    binding: &HostFnBinding,
    qualifier: &str,
    selection: &GoHostBindingSelection<'_>,
    operation: &str,
) -> TraitMethod {
    let mut method = match operation {
        "make-empty" => go_alias_method(binding, qualifier, selection, 0, true),
        "make-filled" => go_alias_method(binding, qualifier, selection, 2, true),
        "len" | "pop-back" | "clone" => go_alias_method(binding, qualifier, selection, 1, true),
        "get" => go_alias_method(binding, qualifier, selection, 2, true),
        "set" => go_alias_method(binding, qualifier, selection, 3, false),
        "push" => go_alias_method(binding, qualifier, selection, 2, false),
        "swap" => go_alias_method(binding, qualifier, selection, 3, false),
        "clear" => go_alias_method(binding, qualifier, selection, 1, false),
        other => unreachable!("unknown protocol array operation `{other}`"),
    };
    let index = match binding.body {
        HostFnBodyKind::Array { index, .. } => index.map(|role| selection.role_type(role)),
        _ => unreachable!("array renderer received a non-array binding"),
    };
    match operation {
        "make-filled" => method.arg_types[0] = index.expect("make-filled has an index role"),
        "len" => method.ret_type = index.expect("len has an index role"),
        "get" | "set" => method.arg_types[1] = index.expect("indexed array op has an index role"),
        "swap" => {
            let index = index.expect("swap has an index role");
            method.arg_types[1] = index.clone();
            method.arg_types[2] = index;
        }
        "make-empty" | "push" | "pop-back" | "clear" | "clone" => {}
        other => unreachable!("unknown protocol array operation `{other}`"),
    }
    method
}

/// The Go `ffi.go` alias naming an env member's boundary slot:
/// `<ns>.Env_<member>_<slot>`, where `<member>` is the emitted
/// boundary method name (`go_host_member`). Every flat parameter and
/// non-Unit structural return references its prepared alias, while direct
/// exact host roots come from the protocol's declaration-keyed selection.
fn env_alias(ns: &str, module: &str, leaf: &str, slot: &str, root_arguments: &str) -> String {
    format!(
        "{ns}.Env_{}_{slot}{root_arguments}",
        go_host_member(module, leaf)
    )
}

// =========================================================================
// Driver synthesis.
// =========================================================================

/// Build the Go `main` driver: a `StubHost` satisfying the emitted
/// `Host` interface (each method a canonical body) plus a `main` that
/// instantiates the package and runs the protocol.
fn needs_array_runtime(host_types: &[HostTypeBinding]) -> bool {
    host_types
        .iter()
        .any(|binding| binding.fixture == HostTypeFixture::Array)
}

fn build_driver(host: &HostApi, protocol: RunnerProtocol, ns: &str, qual: &str) -> String {
    let contract = protocol.contract();
    let selection = GoHostBindingSelection::new(contract);
    if contract.execution == ProtocolExecution::CompileOnly {
        return format!("package main\n\nimport _ \"{GO_MODULE_ROOT}/{ns}\"\n\nfunc main() {{}}\n");
    }
    let handle = pascal_case(ns);
    let mut out = String::new();
    out.push_str("// Generated by kio-test-runner-go — do not edit by hand.\n");
    out.push_str("package main\n\n");
    out.push_str("import (\n");
    out.push_str("\t\"bufio\"\n");
    out.push_str("\t\"fmt\"\n");
    out.push_str("\t\"math/big\"\n");
    out.push_str("\t\"os\"\n");
    out.push_str("\t\"strconv\"\n");
    out.push_str("\t\"strings\"\n");
    out.push_str(&format!("\t{qual} \"{GO_MODULE_ROOT}/{ns}\"\n"));
    out.push_str(")\n\n");
    // Silence unused-import complaints — not every driver uses every pkg.
    out.push_str("var _ = bufio.NewReader\nvar _ = fmt.Sprintf\nvar _ = big.NewInt\nvar _ = os.Stdout\nvar _ = strconv.Itoa\nvar _ = strings.Repeat\n\n");

    if needs_array_runtime(contract.host_types) {
        out.push_str(KIO_ARRAY_DECL);
    }
    let needs_stdin = host
        .methods()
        .zip(contract.host_fns)
        .any(|(_, binding)| matches!(binding.body, HostFnBodyKind::ReadAsciiLine { .. }));
    if needs_stdin {
        out.push_str(STDIN_READER_DECL);
    }
    let needs_exit = host
        .methods()
        .zip(contract.host_fns)
        .any(|(_, binding)| matches!(binding.body, HostFnBodyKind::Exit { .. }));
    if needs_exit {
        out.push_str(CLAMP_EXIT_DECL);
    }
    // The 128-bit wrap helper, needed when any `*_i128` / `*_u128`
    // arithmetic method is present (those compute on big.Int and wrap).
    let needs_wrap128 = contract.host_fns.iter().any(|binding| {
        matches!(
            binding.body,
            HostFnBodyKind::Arithmetic {
                number: HostRoleRef {
                    fixture: RoleFixture::I128 | RoleFixture::U128,
                    ..
                },
                ..
            }
        )
    });
    if needs_wrap128 {
        out.push_str(WRAP128_DECL);
    }

    selection.render_selected_types(&mut out);
    if contract
        .host_fns
        .iter()
        .any(|binding| matches!(binding.body, HostFnBodyKind::ReturnedForallUnit))
    {
        out.push_str(
            "var returnedForallProduced bool\n\n\
             func runReturnedForallAgain(run func()) {\n\
             \tdefer func() {\n\
             \t\trecovered := recover()\n\
             \t\tif recovered != \"produce failed\" { panic(recovered) }\n\
             \t\tfmt.Println(\"caught\")\n\
             \t}()\n\
             \trun()\n\
             \tpanic(\"produce did not panic\")\n\
             }\n\n",
        );
    }
    if protocol == RunnerProtocol::HostExistentialRoundtrip {
        let payload = env_alias(
            qual,
            "testapi/arith",
            "observe",
            "arg0",
            &selection.root_arguments(),
        );
        out.push_str(&format!(
            "var observePacked func({payload}) int32\nvar existentialObservations int\n\n"
        ));
    }
    out.push_str("type StubHost struct{}\n\n");
    selection.render_role_adapters(&mut out);
    for (m, binding) in host.methods().zip(contract.host_fns) {
        let kind = canonical_kind(binding.body);
        out.push_str(&render_method(m, &kind, binding, qual, &selection));
        out.push('\n');
    }

    out.push_str("func main() {\n");
    out.push_str(&format!(
        "\tpkg := {qual}.Create{handle}{}(StubHost{{}})\n",
        selection.root_arguments()
    ));
    out.push_str(&render_main_body(contract, qual));
    out.push_str("\t_ = pkg\n");
    out.push_str("}\n");
    out
}

/// Render the `main` body that invokes the package for `protocol`.
///
/// A plain protocol calls the package's exported `main`. An
/// export-surface roundtrip instead drives the package's exported items
/// directly — calling each exported fn through its Go namespace struct
/// (`pkg.<Ns>.<Fn>(…)`) and printing the result — to exercise the FFI
/// export boundary the same way a host adopter would.
fn render_main_body(contract: protocol::ProtocolContract, ns: &str) -> String {
    let selection = GoHostBindingSelection::new(contract);
    let export_root = contract.testapi_conformed.then_some("testapi");
    // The exported-namespace prefix: `pkg.<Title(root)>` for a testapi
    // package, else bare `pkg`.
    let root_ns = match export_root {
        Some(root) => format!("pkg.{}", go_title(root)),
        None => "pkg".to_owned(),
    };
    match contract.execution {
        ProtocolExecution::CompileOnly | ProtocolExecution::ConstructOnly => String::new(),
        ProtocolExecution::Invoke(ExportDriver::Main { module }) => go_main_call(module),
        ProtocolExecution::Invoke(ExportDriver::Coexist) => {
            unreachable!("--protocol coexist dispatches through run_coexist")
        }
        ProtocolExecution::Invoke(driver) => {
            render_export_driver(driver, &root_ns, ns, export_root.is_some(), &selection)
                .unwrap_or_else(|| unreachable!("non-main Go export driver has no renderer"))
        }
    }
}

fn go_main_call(module: &str) -> String {
    let mut segments = module.split('/');
    let first = segments
        .next()
        .expect("a protocol main module is never empty");
    let mut access = format!("pkg.{}", go_title(first));
    for segment in segments {
        access.push('.');
        access.push_str(&go_nested_module_selector(segment));
    }
    format!("\t{access}.Main()\n")
}

/// The export-surface roundtrip drivers: each calls the package's
/// exported items through their Go namespace structs and prints the
/// results, matching the golden's `expected.stdout`. `root_ns` is the
/// exported root namespace access (`pkg.Testapi` / `pkg`). Returns `None`
/// for a non-export protocol (the caller falls back to the `main` call).
fn render_export_driver(
    driver: ExportDriver,
    root_ns: &str,
    ns: &str,
    modules_are_nested: bool,
    selection: &GoHostBindingSelection<'_>,
) -> Option<String> {
    let root_arguments = selection.root_arguments();
    let module = |source: &str| {
        let selector = if modules_are_nested {
            go_nested_module_selector(source)
        } else {
            go_title(source)
        };
        format!("{root_ns}.{selector}")
    };
    let type_handle =
        |module: &str, source: &str| format!("{module}.{}", go_type_handle_selector(source));

    let body = match driver {
        // `tag()` / `value()` / `echo` exported in `testapi/api`.
        ExportDriver::ModuleRoundtrip => {
            let api = module("api");
            format!(
                "\tfmt.Println({api}.Tag())\n\
                 \tfmt.Println({api}.Value())\n\
                 \tfmt.Println({api}.Echo(\"module-echo\"))\n"
            )
        }
        // `answer()` in `testapi/main`, `echo` in `testapi/utils`.
        ExportDriver::NamespaceRoundtrip => {
            let main = module("main");
            let utils = module("utils");
            format!(
                "\tfmt.Println({main}.Answer())\n\
                 \tfmt.Println({utils}.Echo(\"namespace-utils\"))\n"
            )
        }
        // Polymorphic fns exported at the testapi root; each returns the
        // erased `any`, asserted for printing.
        ExportDriver::PolyRoundtrip => format!(
            "\tfmt.Println({root_ns}.PolyEcho(\"poly-string\").(string))\n\
             \tfmt.Println({root_ns}.PolyEcho(int32(42)).(int32))\n\
             \tfmt.Println({root_ns}.KeepLeft(\"left\", int32(99)).(string))\n"
        ),
        // `apply_twice(step, seed)` and `make_step(delta)` in
        // `testapi/main`; the callback / returned closure are concrete
        // `func(int32) int32`.
        ExportDriver::CallbackRoundtrip => {
            let main = module("main");
            format!(
                "\tfmt.Println({main}.ApplyTwice(func(n int32) int32 {{ return n + 3 }}, 10))\n\
                 \tstep := {main}.MakeStep(4)\n\
                 \tfmt.Println(step(5))\n"
            )
        }
        // `make_pair(I32, String) -> (I32 & String)` exported in the flat
        // package's `main` module (`pkg.Main.MakePair`); read the returned
        // product's positional fields.
        ExportDriver::PositionalProductRoundtrip => {
            let main = module("main");
            format!("\tq := {main}.MakePair(7, \"hello\")\n\tfmt.Println(q.F0, q.F1)\n")
        }
        // Direct exports flatten a right-spine product domain into positional
        // Go arguments, while `echo_pair`'s product return remains a keyed
        // structural value.
        ExportDriver::MultilabelRoundtrip => {
            let main = module("main");
            let a = type_handle(&main, "A");
            format!(
                "\t{main}.Say(42, \"shown\\n\")\n\
                 \trow := {main}.EchoPair(88, \"99\")\n\
                 \tfmt.Println(row.A)\n\
                 \tfmt.Println(row.B)\n\
                 \tfmt.Println({a}.Get({main}.EchoA({a}.Mk(111))))\n"
            )
        }
        ExportDriver::RustCallbackAliases => {
            "\tpanic(\"rust-callback-aliases tests the Rust public naming contract only\")\n"
                .to_owned()
        }
        ExportDriver::HostExistentialRoundtrip => {
            let main = module("main");
            let types = module("types");
            let payload = env_alias(
                ns,
                "testapi/arith",
                "observe",
                "arg0",
                &selection.root_arguments(),
            );
            let packed = go_type_handle_selector("Packed");
            format!(
                "\topenings := 0\n\tobservePacked = func(value {payload}) int32 {{\n\t\treturn {types}.{packed}.ReadPacked(value, func(seed any, step func(any) int32) any {{ openings++; return step(seed) }}).(int32)\n\t}}\n\tresult := {main}.Exercise()\n\tif result.F0 != 37 || result.F1 != 83 || existentialObservations != 2 || openings != 2 {{ panic(\"existential host observations changed\") }}\n\tfmt.Println(\"existential host opening ok\")\n"
            )
        }
        ExportDriver::FunctorDictRoundtrip => {
            let main = module("main");
            let types = module("types");
            let box_type = type_handle(&types, "Box");
            let functor = type_handle(&types, "Functor");
            format!(
                r#"    integers := []int32{{}}
    texts := []string{{}}
    toText := func(value any) any {{ number := value.(int32); integers = append(integers, number); return fmt.Sprintf("v:%d", number) }}
    toInteger := func(value any) any {{ text := value.(string); texts = append(texts, text); return int32(len(text)) }}
    dict := {main}.EchoFunctor({main}.BoxFunctor())
    first := {main}.ApplyFunctor(dict, toText, {box_type}.MkBox(int32(42), {ns}.Unit{{}}))
    if {box_type}.UnBox(first).F0 != "v:42" {{ panic("functor integer input") }}
    second := {main}.ApplyFunctor(dict, toInteger, {box_type}.MkBox("apple", {ns}.Unit{{}}))
    if {box_type}.UnBox(second).F0 != int32(5) {{ panic("functor text input") }}
    abstractDict := func(step func(any) any, value any) any {{
        return dict(step, value.({ns}.Exp_Testapi_types__Box_mkBox_ret{root_arguments}))
    }}
    mapping := {functor}.Fmap(abstractDict)
    third := mapping(toText, {box_type}.MkBox(int32(7), {ns}.Unit{{}})).({ns}.Exp_Testapi_types__Box_unBox_arg0{root_arguments})
    if {box_type}.UnBox(third).F0 != "v:7" {{ panic("projected functor integer input") }}
    fourth := mapping(toInteger, {box_type}.MkBox("pear", {ns}.Unit{{}})).({ns}.Exp_Testapi_types__Box_unBox_arg0{root_arguments})
    if {box_type}.UnBox(fourth).F0 != int32(4) {{ panic("projected functor text input") }}
    if len(integers) != 2 || integers[0] != 42 || integers[1] != 7 || len(texts) != 2 || texts[0] != "apple" || texts[1] != "pear" {{ panic("functor callback events") }}
    fmt.Println("functor dictionary ok")
"#
            )
        }
        ExportDriver::CallableSlotsRoundtrip => {
            let main = module("main");
            let input_sum = format!("{ns}.Exp_Testapi_main__applySum_arg0{root_arguments}");
            let input_callable = sum_constructor(&input_sum, 0);
            let echo_callable = format!("{ns}.Exp_Testapi_main__echoSum_ret_0{root_arguments}");
            let echo_scalar = format!("{ns}.Exp_Testapi_main__echoSum_ret_1{root_arguments}");
            let made_callable =
                format!("{ns}.Exp_Testapi_main__makeCallableSum_ret_0{root_arguments}");
            let made_scalar = format!("{ns}.Exp_Testapi_main__makeScalarSum_ret_1{root_arguments}");
            format!(
                "\tproductCalls := 0\n\
                 \tproductStep := func(value int32) int32 {{ productCalls++; return value + 5 }}\n\
                 \tif {main}.ApplyProduct(productStep, 11) != 16 {{ panic(\"product callback input\") }}\n\
                 \techoedProduct := {main}.EchoProduct(productStep, 17)\n\
                 \tif echoedProduct.F1 != 17 || echoedProduct.F0(echoedProduct.F1) != 22 {{ panic(\"product callback roundtrip\") }}\n\
                 \tmadeProduct := {main}.MakeProduct(23)\n\
                 \tif madeProduct.F1 != 23 || madeProduct.F0(29) != 29 {{ panic(\"package product callback\") }}\n\
                 \tsumCalls := 0\n\
                 \tsumStep := func(value int32) int32 {{ sumCalls++; return value + 7 }}\n\
                 \tsum := {input_callable}(sumStep)\n\
                 \tif {main}.ApplySum(sum, 31) != 38 {{ panic(\"sum callback input\") }}\n\
                 \techoedSum := {main}.EchoSum(sum).Case().({echo_callable})\n\
                 \tif echoedSum.Value()(37) != 44 {{ panic(\"sum callback roundtrip\") }}\n\
                 \tmadeSum := {main}.MakeCallableSum().Case().({made_callable})\n\
                 \tif madeSum.Value()(41) != 41 {{ panic(\"package sum callback\") }}\n\
                 \tscalar := {main}.MakeScalarSum(97)\n\
                 \tif scalar.Case().({made_scalar}).Value() != 97 || {main}.ApplySum(scalar, 43) != 97 {{ panic(\"scalar sum input\") }}\n\
                 \tif {main}.EchoSum(scalar).Case().({echo_scalar}).Value() != 97 {{ panic(\"scalar sum roundtrip\") }}\n\
                 \tif productCalls != 2 || sumCalls != 2 {{ panic(\"callback count changed\") }}\n\
                 \tfmt.Println(\"callable slots ok\")\n"
            )
        }
        ExportDriver::ScalarRoundtrip => {
            let main = module("main");
            format!(
                "\tsigned, ok := new(big.Int).SetString(\"-1208925819614629174706299\", 10)\n\
                 \tif !ok {{ panic(\"invalid signed fixture\") }}\n\
                 \tunsigned, ok := new(big.Int).SetString(\"2417851639229258349412391\", 10)\n\
                 \tif !ok {{ panic(\"invalid unsigned fixture\") }}\n\
                 \tif {main}.EchoI128(new(big.Int).Set(signed)).Cmp(signed) != 0 || {main}.EchoU128(new(big.Int).Set(unsigned)).Cmp(unsigned) != 0 {{ panic(\"wide integer payload changed\") }}\n\
                 \tfor _, value := range []float32{{1.5, -2.25}} {{ if {main}.EchoF32(value) != value {{ panic(\"F32 payload changed\") }} }}\n\
                 \tfor _, value := range []float64{{1.0000000000000002, -3.125}} {{ if {main}.EchoF64(value) != value {{ panic(\"F64 payload changed\") }} }}\n\
                 \tfmt.Println(\"scalar payloads ok\")\n"
            )
        }
        ExportDriver::HostOwnedRoundtrip => {
            let main = module("main");
            let from_native =
                go_parametric_host_from_native(ns, HostTypeIdentity::new("testapi", "Box"));
            format!(
                "\tfor _, value := range []int32{{7, 19}} {{ if {main}.EchoToken(value).(int32) != value {{ panic(\"token payload changed\") }} }}\n\
                 \tinteger := {main}.EchoBox({from_native}(int32(42))).UnsafeNative()\n\
                 \tif integer.(int32) != 42 {{ panic(\"integer box payload changed\") }}\n\
                 \ttext := {main}.EchoBox({from_native}(\"box-value\")).UnsafeNative()\n\
                 \tif text.(string) != \"box-value\" {{ panic(\"string box payload changed\") }}\n\
                 \tfmt.Println(\"host-owned payloads ok\")\n"
            )
        }
        // `pair_swap((I32, String)) -> (String & I32)` and
        // `dispatch_left(I32 | String) -> String`, exported in
        // `testapi/main`. The product param/return read positional `F0`/`F1`
        // fields; each sum param is built through the exact slot alias's
        // local constructor (`New<alias>_<arm>`), so the driver never needs
        // a facade-global semantic-key helper name.
        ExportDriver::StructuralRoundtrip => {
            let main = module("main");
            let dispatch = format!("{ns}.Exp_Testapi_main__dispatchLeft_arg0{root_arguments}");
            let classify = format!("{ns}.Exp_Testapi_main__classify_arg0{root_arguments}");
            let dispatch_0 = sum_constructor(&dispatch, 0);
            let dispatch_1 = sum_constructor(&dispatch, 1);
            let classify_0 = sum_constructor(&classify, 0);
            let classify_4 = sum_constructor(&classify, 4);
            let classify_9 = sum_constructor(&classify, 9);
            let samples = [
                (0, "int8(-101)"),
                (1, "int16(-12345)"),
                (2, "int32(-123456789)"),
                (3, "int64(-9007199254740993)"),
                (4, "uint8(201)"),
                (5, "uint16(54321)"),
                (6, "uint32(3456789012)"),
                (7, "uint64(18014398509481987)"),
                (8, "false"),
                (8, "true"),
                (9, "\"sum-value\""),
            ]
            .into_iter()
            .map(|(arm, value)| format!("{}({value})", sum_constructor(&classify, arm)))
            .collect::<Vec<_>>()
            .join(", ");
            let payload_arms = (0..10)
                .map(|arm| {
                    format!(
                        "\t\tcase {ns}.Exp_Testapi_main__echoSum_ret_{arm}{root_arguments}:\n\
                         \t\t\tpayload = value.Value()\n"
                    )
                })
                .collect::<String>();
            format!(
                "\tq := {main}.PairSwap(42, \"hello\")\n\
                 \tfmt.Println(q.F0, q.F1)\n\
                 \tfmt.Println({main}.DispatchLeft({dispatch_0}(7)))\n\
                 \tfmt.Println({main}.DispatchLeft({dispatch_1}(\"from-sum\")))\n\
                 \trotated := {main}.Rotate(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12)\n\
                 \tfmt.Println(rotated.F0, rotated.F1, rotated.F11)\n\
                 \tfmt.Println({main}.Classify({classify_0}(int8(1))))\n\
                 \tfmt.Println({main}.Classify({classify_4}(uint8(5))))\n\
                 \tfmt.Println({main}.Classify({classify_9}(\"ten\")))\n\
                 \tfirst := {main}.ChooseFirst()\n\
                 \tfmt.Println({main}.Classify(first))\n\
                 \tmiddle := {main}.ChooseMiddle()\n\
                 \tfmt.Println({main}.Classify(middle))\n\
                 \tlast := {main}.ChooseLast()\n\
                 \tfmt.Println({main}.Classify(last))\n\
                 \tfor _, sample := range []{classify}{{{samples}}} {{\n\
                 \t\treturned := {main}.EchoSum(sample)\n\
                 \t\tvar payload any\n\
                 \t\tswitch value := returned.Case().(type) {{\n{payload_arms}\
                 \t\tdefault:\n\
                 \t\t\tpanic(\"export-structural-roundtrip: foreign sum arm\")\n\
                 \t\t}}\n\
                 \t\tfmt.Println({main}.Classify(returned), payload)\n\
                 \t}}\n"
            )
        }
        // `pack(I32, String) -> Tagged` (Out) and `first_or(I32, Tagged) ->
        // I32` (In), exported in `testapi/main`, where `Tagged = Pr | .`
        // and `Pr` is a newtype over `(I32 & String)`. Chaining them
        // round-trips a sum-arm-over-a-newtype-over-a-product across the
        // boundary; the In conversion of `Tagged` reads the `Pr` arm's typed
        // product facade rather than an erased `any` payload. The
        // recovered first field prints as `7`.
        ExportDriver::NewtypeSumRoundtrip => {
            let main = module("main");
            format!("\tfmt.Println({main}.FirstOr(0, {main}.Pack(7, \"hi\")))\n")
        }
        // `bump(Wrap) -> Wrap` exported in `testapi/main`, where `Wrap`
        // is a bare scalar-payload newtype (`newtype Wrap : I32`). The Go
        // facade exposes the bare scalar payload directly, so the
        // export takes / returns the raw `int32` payload directly
        // (`Bump(int32) int32`). Round-trip `7` through it; it prints as
        // `7`.
        ExportDriver::NewtypeScalarRoundtrip => {
            let main = module("main");
            format!("\tfmt.Println({main}.Bump(7))\n")
        }
        ExportDriver::NewtypeIgnoredArgumentRoundtrip => {
            let main = module("main");
            format!(
                "\tvar wrapped int32 = {main}.FromI32(7)\n\
                 \tfmt.Println({main}.ToI32(wrapped))\n"
            )
        }
        ExportDriver::RecursiveNewtypeBoundary => {
            let main = module("main");
            let root = type_handle(&main, "Root");
            format!(
                "\tpayload := {main}.BasePayload()\n\
                 \troot := {root}.MakeRoot(payload)\n\
                 \tkept := {main}.Keep(root)\n\
                 \tprojected := {root}.ReadRoot(kept)\n\
                 \tfmt.Println({main}.AcceptPayload(projected))\n"
            )
        }
        ExportDriver::NewtypeVisibilityFacade => {
            let i32_type = selection.host_type(HostTypeIdentity::new("testapi", "I32"));
            let types = module("types");
            let left = module("left");
            let right = module("right");
            let constructor_only = type_handle(&types, "Constructor_only");
            let projector_only = type_handle(&types, "Projector_only");
            let both_public = type_handle(&types, "Both_public");
            let left_shared = type_handle(&left, "Shared");
            let right_shared = type_handle(&right, "Shared");
            let constructor_pair = type_handle(&types, "Constructor_pair");
            let projector_pair = type_handle(&types, "Projector_pair");
            let constructor_generic = type_handle(&types, "Constructor_generic");
            let projector_generic = type_handle(&types, "Projector_generic");
            let packed_function = type_handle(&types, "Packed_function");
            let existential_unit = type_handle(&types, "Existential_unit");
            let existential_empty = type_handle(&types, "Existential_empty");
            let recursive_both = type_handle(&types, "Recursive_both");
            let recursive_constructor = type_handle(&types, "Recursive_constructor");
            let recursive_projector = type_handle(&types, "Recursive_projector");
            let constructor_spread = type_handle(&types, "Constructor_spread");
            let projector_spread = type_handle(&types, "Projector_spread");
            let existential_spread = type_handle(&types, "Existential_spread");
            let recursive_existential = type_handle(&types, "Recursive_existential_function");
            format!(
                "\tvar inputA {i32_type} = 11\n\
             \ta := {types}.MakeA(inputA)\n\
             \tvar outA {i32_type} = {types}.ReadA(a)\n\
             \tfmt.Println(outA)\n\
             \tvar inputB {i32_type} = 22\n\
             \tb := {types}.MakeB(inputB)\n\
             \tvar outB {i32_type} = {types}.ReadB(b)\n\
             \tfmt.Println(outB)\n\
             \tvar inputC {i32_type} = 33\n\
             \tc := {constructor_only}.MakeConstructorOnly(inputC)\n\
             \tvar outC {i32_type} = {types}.ReadConstructorOnlyValue(c)\n\
             \tfmt.Println(outC)\n\
             \tvar inputP {i32_type} = 44\n\
             \tp := {types}.MakeProjectorOnlyValue(inputP)\n\
             \tvar outP {i32_type} = {projector_only}.ReadProjectorOnly(p)\n\
             \tfmt.Println(outP)\n\
             \tvar inputBoth {i32_type} = 55\n\
             \tboth := {both_public}.MakeBothPublic(inputBoth)\n\
             \tvar outBoth {i32_type} = {both_public}.ReadBothPublic(both)\n\
             \tfmt.Println(outBoth)\n\
             \tvar inputLeft {i32_type} = 66\n\
             \tleftValue := {left}.Make(inputLeft)\n\
             \tvar outLeft {i32_type} = {left_shared}.ReadShared({left_shared}.MakeShared({left}.Read(leftValue)))\n\
             \tfmt.Println(outLeft)\n\
             \tvar inputRight {i32_type} = 77\n\
             \trightValue := {right}.Make(inputRight)\n\
             \tvar outRight {i32_type} = {right_shared}.ReadShared({right_shared}.MakeShared({right}.Read(rightValue)))\n\
             \tfmt.Println(outRight)\n\
             \tconstructorPairValue := {constructor_pair}.MakeConstructorPair({i32_type}(81), {i32_type}(82))\n\
             \tconstructorPairOut := {types}.ReadConstructorPairValue(constructorPairValue)\n\
             \tfmt.Println(constructorPairOut.F0, constructorPairOut.F1)\n\
             \tprojectorPairValue := {types}.MakeProjectorPairValue(constructorPairOut.F0, constructorPairOut.F1)\n\
             \tprojectorPairOut := {projector_pair}.ReadProjectorPair(projectorPairValue)\n\
             \tfmt.Println(projectorPairOut.F0, projectorPairOut.F1)\n\
             \tconstructorGenericValue := {constructor_generic}.MakeConstructorGeneric({i32_type}(85))\n\
             \tfmt.Println({types}.ReadConstructorGenericValue(constructorGenericValue).({i32_type}))\n\
             \tprojectorGenericValue := {types}.MakeProjectorGenericValue({i32_type}(86))\n\
             \tfmt.Println({projector_generic}.ReadProjectorGeneric(projectorGenericValue).({i32_type}))\n\
             \tpacked := {packed_function}.MakePackedFunction(func(left {i32_type}, right {i32_type}) {i32_type} {{ return left + right }})\n\
             \tunpacked := {packed_function}.ReadPackedFunction(packed)\n\
             \tfmt.Println(unpacked(constructorPairOut.F0, constructorPairOut.F1))\n\
             \texistential := {types}.MakeExistentialUnitValue()\n\
             \tfmt.Println({existential_unit}.ReadExistentialUnit(existential, func(_ any) any {{ return int32(89) }}).(int32))\n\
             \texistentialEmptyValue := {types}.MakeExistentialEmptyValue()\n\
             \tfmt.Println({existential_empty}.ReadExistentialEmpty(existentialEmptyValue, func() any {{ return int32(90) }}).(int32))\n\
             \trecursiveBothValue := {recursive_both}.MakeRecursiveBoth({types}.RecursiveBothBasePayload())\n\
             \tfmt.Println({types}.RecursiveBothPayloadIsBase({recursive_both}.ReadRecursiveBoth(recursiveBothValue)))\n\
             \trecursiveConstructorValue := {recursive_constructor}.MakeRecursiveConstructor({types}.RecursiveConstructorBasePayload())\n\
             \tfmt.Println({types}.RecursiveConstructorPayloadIsBase({types}.ReadRecursiveConstructorValue(recursiveConstructorValue)))\n\
             \trecursiveProjectorValue := {types}.MakeRecursiveProjectorBase()\n\
             \tfmt.Println({types}.RecursiveProjectorPayloadIsBase({recursive_projector}.ReadRecursiveProjector(recursiveProjectorValue)))\n\
             \tinputs := []{i32_type}{{}}\n\
             \tconstructed := {constructor_spread}.MakeConstructorSpread(func(seed {i32_type}, _ any) {i32_type} {{ inputs = append(inputs, seed); return seed + 3 }})\n\
             \tif {types}.InvokeConstructorSpreadI32(constructed, 101) != 104 || {types}.InvokeConstructorSpreadUnit(constructed, 102) != 105 || len(inputs) != 2 || inputs[0] != 101 || inputs[1] != 102 {{ panic(\"constructor callback changed\") }}\n\
             \tprojected := {projector_spread}.ReadProjectorSpread({types}.MakeProjectorSpreadValue())\n\
             \tif projected(111, {i32_type}(1)) != 111 || projected(112, {ns}.Unit{{}}) != 112 {{ panic(\"projector callback changed\") }}\n\
             \tspreadOpens, recursiveOpens := 0, 0\n\
             \topenedSpread := {existential_spread}.ReadExistentialSpread({types}.MakeExistentialSpreadValue(), func(_ func(any, any) any) any {{ spreadOpens++; return int32(91) }})\n\
             \topenedRecursive := {recursive_existential}.ReadRecursiveExistentialFunction({types}.MakeRecursiveExistentialFunctionValue(), func(_ func(any) {ns}.Exp_Testapi_types__makeRecursiveExistentialFunctionValue_ret{root_arguments}) any {{ recursiveOpens++; return int32(92) }})\n\
             \tif openedSpread.(int32) != 91 || openedRecursive.(int32) != 92 || spreadOpens != 1 || recursiveOpens != 1 {{ panic(\"existential continuation count changed\") }}\n"
            )
        }
        // `make(String, I32, I32) -> Outer` exported in `testapi/main`,
        // where the label-minted `Inner` wraps `(I32 & I32)` and `Outer`
        // wraps `(String & Inner)`. The Go facade instantiates one generic
        // product shell per nesting level, retaining positional `F<k>` fields
        // for the plain slots and the bare newtype name for the nested slot.
        // Read all three leaf slots; the nested named-product return is the
        // nested-binder regression shape.
        ExportDriver::CompoundInputOnce => {
            let main = module("main");
            format!(
                r#"    direct := {main}.Direct()
    fmt.Println(direct.F0)
    fmt.Println(direct.F1)
    callback := {main}.Callback(func() {ns}.Exp_Testapi_main__callback_arg0_cbret{root_arguments} {{
        fmt.Println("callback")
        return {ns}.Exp_Testapi_main__callback_arg0_cbret{root_arguments}{{F0: 9, F1: "callback-value"}}
    }})
    fmt.Println(callback.F0)
    fmt.Println(callback.F1)
    outer := {main}.EchoOuter({main}.MakeOuter("nest", 11, 13))
    fmt.Println(outer.F0)
    fmt.Println(outer.Inner.F0)
    fmt.Println(outer.Inner.F1)
    for _, choice := range []{ns}.Exp_Testapi_main__echoChoice_ret{root_arguments}{{{main}.First(17), {main}.Middle(19, 23), {main}.Last("choice", 29, 31)}} {{
        switch value := {main}.EchoChoice(choice).Case().(type) {{
        case {ns}.Exp_Testapi_main__echoChoice_ret_0{root_arguments}:
            fmt.Println(value.Value())
        case {ns}.Exp_Testapi_main__echoChoice_ret_1{root_arguments}:
            inner := value.Value()
            fmt.Println(inner.F0)
            fmt.Println(inner.F1)
        case {ns}.Exp_Testapi_main__echoChoice_ret_2{root_arguments}:
            outer := value.Value()
            fmt.Println(outer.F0)
            fmt.Println(outer.Inner.F0)
            fmt.Println(outer.Inner.F1)
        default:
            panic("compound-input-once: foreign sum arm")
        }}
    }}
    fmt.Println({main}.EchoText("atomic"))
"#
            )
        }
        ExportDriver::NestedProductRoundtrip => {
            let main = module("main");
            format!(
                "\to := {main}.Make(\"nest\", 7, 9)\n\
                 \tfmt.Println(o.F0)\n\
                 \tfmt.Println(o.Inner.F0)\n\
                 \tfmt.Println(o.Inner.F1)\n"
            )
        }
        ExportDriver::CurriedFacade => {
            let main = module("main");
            format!(
                "\tfmt.Println({main}.Pick(\"ku\", \"rz\"))\n\tfmt.Println({main}.Last(1, 2, 3))\n"
            )
        }
        ExportDriver::NestedCurriedRoundtrip => {
            let api = module("api");
            format!(
                "\tjoin := func(left string) func(string) string {{\n\
                 \t\treturn func(right string) string {{ return left + \"/\" + right }}\n\
                 \t}}\n\
                 \tviaHost := {api}.ViaHost(join)\n\
                 \tfmt.Println(\"via host:\", viaHost(\"env-left\")(\"env-right\"))\n\
                 \troundExport := {api}.RoundExport(join)\n\
                 \tfmt.Println(\"round export:\", roundExport(\"export-left\")(\"export-right\"))\n"
            )
        }
        ExportDriver::HostSubstitutedUnitCallback => {
            let api = module("api");
            format!(
                "\tfmt.Println({api}.ViaHost(func(_ {ns}.Unit) string {{ return \"callback\" }}))\n"
            )
        }
        ExportDriver::ReturnedForallCallByValue => {
            let main = module("main");
            format!(
                "\t{main}.Main()\n\
                 \trunReturnedForallAgain(func() {{ _ = {main}.Main() }})\n"
            )
        }
        ExportDriver::PublicWordNames => {
            let word_api = format!("{root_ns}.WordApi");
            let word_nodes = format!("{word_api}.KioModule_wordNodes");
            format!(
                "\tfmt.Println({word_api}.ReadWord())\n\
                 \tfmt.Println({word_api}.KioItem__ureadWord())\n\
                 \tfmt.Println({word_api}.KioItem_readWord_u())\n\
                 \tfmt.Println({word_api}.KioItem__ureadWord_u())\n\
                 \tfmt.Println({word_api}.KioItem_readWord_u_u())\n\
                 \tboxed := {word_nodes}.KioType__uWordBox_u_u.WrapWord(55)\n\
                 \tfmt.Println({word_nodes}.KioType__uWordBox_u_u.UnwrapWord(boxed))\n\
                 \tfmt.Println({word_nodes}.KeepWord(66))\n\
                 \tpair := {word_api}.KeepPair(77, 88)\n\
                 \tfmt.Println(pair.KioKey_V1_B13__uWordBox_u_u)\n\
                 \tfmt.Println(pair.KioKey_V1_Q2_M7_wordApiM10_otherNodesN13__uWordBox_u_u)\n"
            )
        }
        ExportDriver::FacadeSelectorCollisions => {
            let api = module("api");
            let foo_module = module("foo");
            let foo_bar = module("foo_bar");
            let i = module("i");
            let host = module("host");
            let mod_api_value = module("mod_api_value");
            let bar_module = go_nested_module_selector("bar");
            let child_module = format!("{api}.{}", go_nested_module_selector("child"));
            let child_type = type_handle(&api, "Child");
            format!(
                "\t{api}.Pkg()\n\
                 \t{api}.Value()\n\
                 \t{host}.Value()\n\
                 \t{mod_api_value}.Value()\n\
                 \tfmt.Println({foo_module}.{bar_module}.Value(9, 1))\n\
                 \tfmt.Println({foo_bar}.Value(10, 2))\n\
                 \tfmt.Println({i}.Value(41, 1))\n\
                 \t{api}.Child()\n\
                 \tfmt.Println(\"api.child function\")\n\
                 \t{child_module}.Value()\n\
                 \tfmt.Println(\"api/child module\")\n\
                 \tchild := {child_type}.MakeChild(30)\n\
                 \t{child_type}.ReadChild(child)\n\
                 \tfmt.Println(\"api.Child type\")\n"
            )
        }
        ExportDriver::ModuleAliasScopeCollision => {
            let a = module("a");
            let b = module("b");
            format!(
                "\taValue := {a}.Make()\n\
                 \t{a}.Consume(aValue.F0, aValue.F1)\n\
                 \tbValue := {b}.Make()\n\
                 \t{b}.Consume(bValue.F0, bValue.F1, bValue.F2)\n"
            )
        }
        ExportDriver::WideCallable => {
            let main = module("main");
            let arguments = (0..WIDE_CALLABLE_SLOT_COUNT)
                .map(|index| index.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "\tout := {main}.Select({arguments})\n\tfmt.Println(out.F0)\n\tfmt.Println(out.F1)\n\tfmt.Println(out.F2)\n\tcallbackOut := {main}.MakeSelect()({arguments})\n\tfmt.Println(callbackOut.F0)\n\tfmt.Println(callbackOut.F1)\n\tfmt.Println(callbackOut.F2)\n"
            )
        }
        // `apply_via[K][R](f: K -> R, x: K) -> R` erases to
        // `ApplyVia(func(any) any, any) any` on the Go facade; the
        // driver's closures pick the concrete instantiation and assert it
        // back out of `any`.
        ExportDriver::PolyCallbackRoundtrip => {
            let main = module("main");
            format!(
                "\tfmt.Println({main}.ApplyVia(func(v any) any {{ return \"via: \" + v.(string) }}, \"apply\").(string))\n\
                 \tfmt.Println({main}.ApplyVia(func(v any) any {{ return v.(int) + 8 }}, 7).(int))\n"
            )
        }
        // The exported `Pair` newtype lives in `testapi/types`; its
        // `pub constructor mk_pair` / `pub projector un_pair` surface as
        // identity methods on a role-tagged per-type handle under the
        // `testapi/types` module (`.MkPair` / `.UnPair`) — a transparent
        // newtype. Build the
        // `(A & B)` payload — a type-var product, so positional `F0`/`F1`
        // fields, both `any` — through the type-qualified
        // `Exp_..._Pair_mkPair_arg0` alias, round-trip it, read the fields.
        ExportDriver::TypeRoundtrip => {
            let types = module("types");
            let pair = type_handle(&types, "Pair");
            format!(
                "\tboxed := {pair}.MkPair(\"export-type-left\", \"export-type-right\")\n\
                 \tout := {pair}.UnPair(boxed)\n\
                 \tfmt.Println(out.F0)\n\
                 \tfmt.Println(out.F1)\n"
            )
        }
        ExportDriver::Main { .. } | ExportDriver::Coexist => return None,
    };
    Some(body)
}

/// Render one `StubHost` method: the Go signature + a canonical body.
fn render_method(
    m: &TraitMethod,
    kind: &CanonicalKind,
    binding: &HostFnBinding,
    ns: &str,
    selection: &GoHostBindingSelection<'_>,
) -> String {
    let sig = render_method_sig(m);
    let body = render_go_body_for_binding(binding, kind, m, ns, selection);
    format!("func (StubHost) {sig} {{\n\t{body}\n}}\n")
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

/// Render the Go body for `m`, honoring the protocol's bespoke host fns
/// (`call_step`, `make_pair`, `make_step`, `apply_poly`, `box_make` /
/// `box_get`, `make_token` / `token_value`, `sum_to_string`) before the
/// canonical-kind dispatch. The bespoke bodies key on the binding's
/// structured body and mirror the Rust runner's semantics. Products are built
/// through their exact generic-struct aliases and named fields. Sums are built
/// through alias-local constructors and inspected through `Case()` plus the
/// exact positional case aliases.
fn render_go_body_for_binding(
    binding: &HostFnBinding,
    kind: &CanonicalKind,
    m: &TraitMethod,
    ns: &str,
    selection: &GoHostBindingSelection<'_>,
) -> String {
    match binding.body {
        HostFnBodyKind::Print { string } => format!(
            "os.Stdout.WriteString({})",
            selection.role_into_native(string, "arg0")
        ),
        HostFnBodyKind::Eprint { string } => format!(
            "os.Stderr.WriteString({})",
            selection.role_into_native(string, "arg0")
        ),
        HostFnBodyKind::NumericToString { value, string } => {
            let kind = match kind {
                CanonicalKind::NumericToString { kind } => kind,
                _ => unreachable!("numeric-to-string binding must retain its role kind"),
            };
            let value = selection.role_into_native(value, "arg0");
            let rendered = numeric_to_string_expression(kind, &value);
            format!("return {}", selection.role_from_native(string, &rendered))
        }
        // `call_step(step, seed)`: the callback's product domain is the
        // canonical three-slot public function facade. Reconstructing the
        // source product belongs to the generated adapter, not the host.
        HostFnBodyKind::CallStep { string, bool_, .. } => {
            let string = selection.role_from_native(string, "\"compound-callback\"");
            let bool_ = selection.role_from_native(bool_, "true");
            format!("return arg0(arg1, {string}, {bool_})")
        }
        // `make_pair(build, seed)`: apply the callback (returns `(I32 &
        // String)`) and take the first field.
        HostFnBodyKind::MakePairCallback { .. } => "return arg0(arg1).F0".to_owned(),
        // `make_step(delta)`: return an `I32 -> I32` step closure.
        HostFnBodyKind::MakeStep { .. } => {
            "return func(n int32) int32 { return n + arg0 }".to_owned()
        }
        // The package owns one declaration-keyed carrier for `Box(T)`. The
        // fixture keeps its native payload erased, but crosses the public
        // boundary through that exact carrier rather than through `any`.
        HostFnBodyKind::BoxMake { box_type } => format!(
            "return {}(arg0)",
            go_parametric_host_from_native(ns, box_type)
        ),
        HostFnBodyKind::BoxGet { .. } => "return arg0.UnsafeNative()".to_owned(),
        // `make_token(I32) -> Token`: the opaque `Token` erases to `any`,
        // so the token *is* its `I32` payload (boxed to `any` on return).
        HostFnBodyKind::MakeToken { value_i32, .. } => {
            format!("return {}", selection.role_into_native(value_i32, "arg0"))
        }
        // `token_value(Token) -> I32`: unbox the erased payload.
        HostFnBodyKind::TokenValue { value_i32, .. } => format!(
            "return {}",
            selection.role_from_native(value_i32, "arg0.(int32)")
        ),
        // A polymorphic-function newtype crosses as an erased function
        // carrier; hand the exact value back.
        HostFnBodyKind::RoundFunctor
        | HostFnBodyKind::RoundPicker
        | HostFnBodyKind::RoundPolyThunk
        | HostFnBodyKind::RoundPolyUnitSlot => "return arg0".to_owned(),
        HostFnBodyKind::ObservePacked { .. } => {
            "existentialObservations++; return observePacked(arg0)".to_owned()
        }
        HostFnBodyKind::StagedSecond { .. } => "return arg1".to_owned(),
        HostFnBodyKind::NestedCurriedRoundtrip { .. } => {
            "fmt.Println(\"round host probe:\", arg0(\"host-left\")(\"host-right\"))\n\treturn arg0"
                .to_owned()
        }
        HostFnBodyKind::InvokeSubstitutedUnitCallback { .. } => {
            format!("return \"host/\" + arg0({ns}.Unit{{}})")
        }
        HostFnBodyKind::ReturnedForallUnit => {
            format!(
                "if returnedForallProduced {{\n\t\tfmt.Println(\"throw\")\n\t\tpanic(\"produce failed\")\n\t}}\n\treturnedForallProduced = true\n\tfmt.Println(\"produce\")\n\treturn {ns}.Unit{{}}"
            )
        }
        HostFnBodyKind::TraceUnit { text } => format!("fmt.Println({text:?})"),
        HostFnBodyKind::StagedUnitCall => "fmt.Println(\"staged Unit host call\")".to_owned(),
        // `apply_poly(f)`: feed the rank-n callback an erased `String`
        // carrier and return the (erased) result as a string.
        HostFnBodyKind::ApplyPoly { .. } => "return arg0(\"rank-n\\n\").(string)".to_owned(),
        // `make_pair(n, s)`: build the `(I32 & String)` product directly.
        HostFnBodyKind::MakePairStructural { .. } => {
            let ret = env_alias(
                ns,
                binding.module,
                binding.leaf,
                "ret",
                &selection.root_arguments(),
            );
            format!("return {ret}{{F0: arg0, F1: arg1}}")
        }
        HostFnBodyKind::ProducePair { .. } => {
            let ret = env_alias(
                ns,
                binding.module,
                binding.leaf,
                "ret",
                &selection.root_arguments(),
            );
            format!("fmt.Println(\"direct\")\n\treturn {ret}{{F0: 7, F1: \"direct-value\"}}")
        }
        // `sum_to_string(v)`: inspect the `(I32 | String)` carrier's exact
        // case; arm 0 stringifies the `I32`, arm 1 is the `String`.
        HostFnBodyKind::SumToString { .. } => {
            let alias = env_alias(
                ns,
                binding.module,
                binding.leaf,
                "arg0",
                &selection.root_arguments(),
            );
            let (case_0, case_1) = sum_cases(&alias);
            format!(
                "switch c := arg0.Case().(type) {{\n\tcase {case_0}:\n\t\treturn strconv.FormatInt(int64(c.Value()), 10)\n\tcase {case_1}:\n\t\treturn c.Value()\n\tdefault:\n\t\t_ = c\n\t\tpanic(\"sum_to_string: foreign variant\")\n\t}}"
            )
        }
        HostFnBodyKind::Array { array, .. } => match kind {
            CanonicalKind::Array(operation) => {
                let from_native = go_parametric_host_from_native(ns, array);
                render_array_body(operation, m, ns, Some(&from_native))
            }
            _ => unreachable!("array binding must classify as an array operation"),
        },
        HostFnBodyKind::Loop => {
            let cbret = env_alias(
                ns,
                binding.module,
                binding.leaf,
                "arg0_cbret",
                &selection.root_arguments(),
            );
            render_loop_body(&cbret)
        }
        HostFnBodyKind::UnreachableI32Print { .. } => {
            "panic(\"unreachable host function\")".to_owned()
        }
        _ => render_go_body(kind, m, ns),
    }
}

/// Render the Go method signature for `m`. Params are `arg0`, `arg1`, …;
/// a unit return (`()`) has no return clause. Every other type string comes
/// from either an exact declaration binding or a prepared emitted alias.
fn render_method_sig(m: &TraitMethod) -> String {
    let mut params = Vec::with_capacity(m.arg_types.len());
    for (i, ty) in m.arg_types.iter().enumerate() {
        params.push(format!("arg{i} {ty}"));
    }
    let ret_clause = if m.ret_type == "()" {
        String::new()
    } else {
        format!(" {}", m.ret_type)
    };
    format!(
        "{}({}){ret_clause}",
        go_host_method_recv_name(&m.name),
        params.join(", ")
    )
}

/// The method name as it appears on the receiver (the emitted interface
/// method name). For a Go-exported package these are the mangled
/// `<Module>__<leaf>` names the emitter writes — export-capitalized (the
/// mangled slash-path's first character is upper-cased so the method is
/// Go-exported) and reachable through the interface.
fn go_host_method_recv_name(name: &str) -> &str {
    name
}

/// Render a Go expression body for one canonical kind. Mirrors the Rust
/// runner's `render_rust_body`, but emits Go and builds / matches shaped
/// returns through the emitted package's stable aliases.
fn render_go_body(kind: &CanonicalKind, m: &TraitMethod, ns: &str) -> String {
    match kind {
        CanonicalKind::Print => "os.Stdout.WriteString(arg0)".to_owned(),
        CanonicalKind::Eprint => "os.Stderr.WriteString(arg0)".to_owned(),
        CanonicalKind::Exit => {
            "os.Exit(int(clampExit(int64(arg0)))); panic(\"unreachable\")".to_owned()
        }
        CanonicalKind::ReadAsciiLine => render_read_ascii_line(m, ns),
        CanonicalKind::StringLen => "return int32(len(arg0))".to_owned(),
        CanonicalKind::StringSlice => render_string_slice(),
        CanonicalKind::StringCodeAt => render_string_code_at(m, ns),
        CanonicalKind::StringConcat => "return arg0 + arg1".to_owned(),
        CanonicalKind::StringEq => "return arg0 == arg1".to_owned(),
        CanonicalKind::StringToInt => render_string_to_int(m, ns),
        CanonicalKind::BoolToString => "return strconv.FormatBool(arg0)".to_owned(),
        CanonicalKind::PrintI32 => {
            "os.Stdout.WriteString(strconv.FormatInt(int64(arg0), 10))".to_owned()
        }
        CanonicalKind::NumericToString { kind } => numeric_to_string_body(kind),
        CanonicalKind::Arith { op, kind } => int_arith_body(op, kind),
        CanonicalKind::FloatArith { op, .. } => float_arith_body(op),
        CanonicalKind::Cmp { cmp, kind } => cmp_body(cmp, kind),
        CanonicalKind::Loop => unreachable!("loop uses its protocol-owned callback aliases"),
        CanonicalKind::Array(op) => render_array_body(op, m, ns, None),
        CanonicalKind::Custom => format!(
            "panic(\"kio-test-runner-go: no canonical impl for host fn `{}`\")",
            m.name
        ),
        // dyn_load_prime's opaque-scalar surface. The Go runner represents a
        // `Scalar` by boxing the native Go value directly in `any`
        // (`int32` / `float64` / `string` / `bool` are mutually distinct Go
        // types and no two scalar kinds share one, so the box is
        // unambiguous — the dynamic-boxing analogue of the JS runner and of
        // the Rust runner's `__Scalar` enum).
        CanonicalKind::MakeScalar => render_make_scalar_body(),
        // `scalar_of_<kind>(v) -> Scalar`: the typed param is already the
        // native Go value; boxing it to the `any` return *is* the scalar.
        CanonicalKind::ScalarOf { .. } => "return arg0".to_owned(),
        CanonicalKind::ScalarAs { kind } => render_scalar_as_body(kind, m, ns),
        // `scalar_is_true(s) -> Bool`: a bool-shaped scalar yields its
        // truth; any other shape is not true (a well-typed guest never asks).
        CanonicalKind::ScalarIsTrue => {
            "if b, ok := arg0.(bool); ok {\n\t\treturn b\n\t}\n\treturn false".to_owned()
        }
    }
}

/// `make_scalar(text, representation) -> Scalar`. The fixture host maps an
/// exact host-type descriptor to this representation key, which selects the
/// scalar shape used to parse the literal's raw text. Mirrors the Rust runner's
/// `render_make_scalar_body_rust` (`I32`/`Int` → i32, `F64`/`F32` → f64,
/// `String`/`Str` → string, `Bool` → `arg0 == "t"`).
fn render_make_scalar_body() -> String {
    "switch arg1 {\n\
     \tcase \"I32\", \"Int\":\n\
     \t\tn, err := strconv.ParseInt(arg0, 10, 32)\n\
     \t\tif err != nil {\n\t\t\tpanic(\"make_scalar: bad i32\")\n\t\t}\n\
     \t\treturn int32(n)\n\
     \tcase \"F64\", \"F32\":\n\
     \t\tf, err := strconv.ParseFloat(arg0, 64)\n\
     \t\tif err != nil {\n\t\t\tpanic(\"make_scalar: bad f64\")\n\t\t}\n\
     \t\treturn f\n\
     \tcase \"String\", \"Str\":\n\
     \t\treturn arg0\n\
     \tcase \"Bool\":\n\
     \t\treturn arg0 == \"t\"\n\
     \tdefault:\n\
     \t\tpanic(\"make_scalar: unknown representation key \" + arg1)\n\
     \t}"
    .to_owned()
}

/// `scalar_as_<kind>(s) -> . | <Kind>`. Project the boxed native value
/// out of the opaque `Scalar`, or the unit arm on a shape mismatch. The
/// sum is `. | <Kind>` — arm 0 (`_0`) is the absent unit value, arm 1 (`_1`)
/// the present value (the reverse spine order of `string_to_int`'s
/// `<Kind> | .`, so the present value is `_1` here, not `_0`).
fn render_scalar_as_body(kind: &str, m: &TraitMethod, ns: &str) -> String {
    let (new_0, new_1) = sum_constructors(&m.ret_type);
    let go_ty = match kind {
        "i32" => "int32",
        "f64" => "float64",
        "str" => "string",
        "bool" => "bool",
        other => return format!("panic(\"scalar_as_{other}: unsupported kind\")"),
    };
    format!(
        "if v, ok := arg0.({go_ty}); ok {{\n\t\treturn {new_1}(v)\n\t}}\n\treturn {new_0}({ns}.Unit{{}})"
    )
}

fn int_arith_body(op: &str, kind: &str) -> String {
    // i128 / u128 are `*big.Int` — no native operator; compute on big.Int
    // and wrap to the role's width to match the JS BigInt.asIntN /
    // asUintN model. Fixed-width Go integers already wrap on overflow.
    if kind == "i128" || kind == "u128" {
        let signed = kind == "i128";
        let bigop = match op {
            "add" => "Add",
            "sub" => "Sub",
            "mul" => "Mul",
            "div" => "Quo",
            "mod" => "Rem",
            _ => "Add",
        };
        return format!("r := new(big.Int).{bigop}(arg0, arg1)\n\treturn kioWrap128(r, {signed})");
    }
    match op {
        "add" => "return arg0 + arg1".to_owned(),
        "sub" => "return arg0 - arg1".to_owned(),
        "mul" => "return arg0 * arg1".to_owned(),
        "div" => "return arg0 / arg1".to_owned(),
        "mod" => "return arg0 % arg1".to_owned(),
        _ => "return arg0".to_owned(),
    }
}

fn float_arith_body(op: &str) -> String {
    match op {
        "add" => "return arg0 + arg1".to_owned(),
        "sub" => "return arg0 - arg1".to_owned(),
        "mul" => "return arg0 * arg1".to_owned(),
        "div" => "return arg0 / arg1".to_owned(),
        _ => "return arg0".to_owned(),
    }
}

fn cmp_body(cmp: &str, kind: &str) -> String {
    if kind == "i128" || kind == "u128" {
        // `*big.Int` comparison via `Cmp` (-1 / 0 / +1).
        let pred = match cmp {
            "eq" => "== 0",
            "lt" => "< 0",
            "leq" | "le" => "<= 0",
            "gt" => "> 0",
            "geq" | "ge" => ">= 0",
            _ => "== 0",
        };
        return format!("return arg0.Cmp(arg1) {pred}");
    }
    let g = match cmp {
        "eq" => "==",
        "lt" => "<",
        "leq" | "le" => "<=",
        "gt" => ">",
        "geq" | "ge" => ">=",
        _ => "==",
    };
    format!("return arg0 {g} arg1")
}

fn numeric_to_string_body(kind: &str) -> String {
    format!("return {}", numeric_to_string_expression(kind, "arg0"))
}

fn numeric_to_string_expression(kind: &str, value: &str) -> String {
    match kind {
        // f32 widens to f64 before formatting (the JS runner has no f32).
        "f32" => format!("strconv.FormatFloat(float64({value}), 'g', -1, 64)"),
        "f64" => format!("strconv.FormatFloat({value}, 'g', -1, 64)"),
        // i128 / u128 are `*big.Int` — format via its own decimal.
        "i128" | "u128" => format!("({value}).String()"),
        // u64 prints unsigned; the other widths fit int64.
        "u64" => format!("strconv.FormatUint(uint64({value}), 10)"),
        _ => format!("strconv.FormatInt(int64({value}), 10)"),
    }
}

fn render_string_slice() -> String {
    "if arg1 < 0 || arg1 > arg2 || int(arg2) > len(arg0) {\n\t\tfmt.Fprintf(os.Stderr, \"string_slice: invalid range [%d, %d) for len %d\\n\", arg1, arg2, len(arg0))\n\t\tos.Exit(1)\n\t}\n\treturn arg0[arg1:arg2]".to_owned()
}

fn render_string_code_at(m: &TraitMethod, ns: &str) -> String {
    // `Int | .` shaped return — `_0` = the byte value, `_1` = unit.
    let (new_0, new_1) = sum_constructors(&m.ret_type);
    format!(
        "if arg1 < 0 || int(arg1) >= len(arg0) {{\n\t\treturn {new_1}({ns}.Unit{{}})\n\t}}\n\treturn {new_0}(int32(arg0[arg1]))"
    )
}

fn render_string_to_int(m: &TraitMethod, ns: &str) -> String {
    let (new_0, new_1) = sum_constructors(&m.ret_type);
    format!(
        "n, err := strconv.ParseInt(arg0, 10, 32)\n\tif err != nil {{\n\t\treturn {new_1}({ns}.Unit{{}})\n\t}}\n\treturn {new_0}(int32(n))"
    )
}

fn render_read_ascii_line(m: &TraitMethod, ns: &str) -> String {
    let (new_0, new_1) = sum_constructors(&m.ret_type);
    format!(
        "line, ok := kioReadLine()\n\tif !ok {{\n\t\treturn {new_1}({ns}.Unit{{}})\n\t}}\n\treturn {new_0}(line)"
    )
}

/// `loop`: drive the step callback until it returns the exit arm. The
/// step's `s | r` return is the `arg0_cbret` sum; arm `_0` continues
/// (the new state), `_1` exits (the result). `cbret` is the callback
/// result's exact alias, derived from the binding identity.
fn render_loop_body(cbret: &str) -> String {
    let (case_0, case_1) = sum_cases(cbret);
    format!(
        "s := arg1\n\tfor {{\n\t\tswitch step := arg0(s).Case().(type) {{\n\t\tcase {case_0}:\n\t\t\ts = step.Value()\n\t\tcase {case_1}:\n\t\t\treturn step.Value()\n\t\tdefault:\n\t\t\t_ = step\n\t\t\tpanic(\"loop: foreign step result\")\n\t\t}}\n\t}}"
    )
}

/// The first two exact case aliases for a sum-typed boundary alias.
fn sum_cases(alias: &str) -> (String, String) {
    let (base, arguments) = split_go_type_arguments(alias);
    (
        format!("{base}_0{arguments}"),
        format!("{base}_1{arguments}"),
    )
}

/// Derive an alias-local sum constructor without naming the facade-global
/// semantic-key helper. `<qualifier>.<alias>` becomes
/// `<qualifier>.New<alias>_<index>`.
fn sum_constructor(alias: &str, index: usize) -> String {
    let (base, arguments) = split_go_type_arguments(alias);
    match base.rsplit_once('.') {
        Some((qualifier, local)) => {
            format!("{qualifier}.New{local}_{index}{arguments}")
        }
        None => format!("New{base}_{index}{arguments}"),
    }
}

fn split_go_type_arguments(alias: &str) -> (&str, &str) {
    alias
        .find('[')
        .map_or((alias, ""), |index| alias.split_at(index))
}

fn go_parametric_host_from_native(ns: &str, identity: HostTypeIdentity) -> String {
    format!(
        "{ns}.UnsafeKioHostType_{}FromNative[any]",
        go_host_type_frame(identity.module, identity.leaf)
    )
}

fn sum_constructors(alias: &str) -> (String, String) {
    (sum_constructor(alias, 0), sum_constructor(alias, 1))
}

fn render_array_body(op: &ArrayOp, m: &TraitMethod, ns: &str, from_native: Option<&str>) -> String {
    // The runner keeps Array's native payload in `*kioArray`. Exact public
    // boundaries wrap that payload in the declaration-keyed carrier; older
    // opaque-only paths can still call this helper without a wrapper.
    // `make_empty` / `make_filled` take no array receiver.
    let recv = if from_native.is_some() {
        "a := arg0.UnsafeNative().(*kioArray)\n\t"
    } else {
        "a := arg0.(*kioArray)\n\t"
    };
    let wrap = |expression: &str| match from_native {
        Some(constructor) => format!("{constructor}({expression})"),
        None => expression.to_owned(),
    };
    match op {
        ArrayOp::MakeEmpty => format!("return {}", wrap("&kioArray{}")),
        ArrayOp::MakeFilled => format!(
            "if arg0 < 0 {{ panic(\"array_make_filled: negative size\") }}\n\ta := &kioArray{{}}\n\tfor i := int32(0); i < arg0; i++ {{ a.v = append(a.v, arg1) }}\n\treturn {}",
            wrap("a")
        ),
        ArrayOp::Len => format!("{recv}return int32(len(a.v))"),
        ArrayOp::Get => format!(
            "{recv}if arg1 < 0 || int(arg1) >= len(a.v) {{ panic(\"array_get: out of bounds\") }}\n\treturn a.v[arg1]"
        ),
        ArrayOp::Set => format!(
            "{recv}if arg1 < 0 || int(arg1) >= len(a.v) {{ panic(\"array_set: out of bounds\") }}\n\ta.v[arg1] = arg2"
        ),
        ArrayOp::Push => format!("{recv}a.v = append(a.v, arg1)"),
        ArrayOp::PopBack => {
            let (new_0, new_1) = sum_constructors(&m.ret_type);
            format!(
                "{recv}if len(a.v) == 0 {{ return {new_1}({ns}.Unit{{}}) }}\n\tlast := a.v[len(a.v)-1]\n\ta.v = a.v[:len(a.v)-1]\n\treturn {new_0}(last)"
            )
        }
        ArrayOp::Swap => format!(
            "{recv}n := len(a.v)\n\tif arg1 < 0 || int(arg1) >= n || arg2 < 0 || int(arg2) >= n {{ panic(\"array_swap: out of bounds\") }}\n\ta.v[arg1], a.v[arg2] = a.v[arg2], a.v[arg1]"
        ),
        ArrayOp::Clear => format!("{recv}a.v = a.v[:0]"),
        ArrayOp::Clone => format!(
            "{recv}c := &kioArray{{}}\n\tc.v = append(c.v, a.v...)\n\treturn {}",
            wrap("c")
        ),
    }
}

/// Title-case a snake_case module/leaf segment for a Go-exported name
/// (`testapi` → `Testapi`, `my_mod` → `MyMod`). Mirrors the emitter's
/// `title_case`.
fn go_title(s: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for ch in s.chars() {
        if ch == '_' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// The runner's backing for the canonical `host type Array[T];`: a
/// pointer-identity slice cell (reference semantics matching JS arrays).
const KIO_ARRAY_DECL: &str = "type kioArray struct{ v []any }\n\n";

/// Stdin line reader backing `read_ascii_line`.
const STDIN_READER_DECL: &str = "var kioStdin = bufio.NewReader(os.Stdin)\n\nfunc kioReadLine() (string, bool) {\n\tline, err := kioStdin.ReadString('\\n')\n\tif len(line) == 0 && err != nil {\n\t\treturn \"\", false\n\t}\n\tline = strings.TrimRight(line, \"\\n\")\n\tline = strings.TrimRight(line, \"\\r\")\n\tfor i := 0; i < len(line); i++ {\n\t\tif line[i] > 127 {\n\t\t\tfmt.Fprintln(os.Stderr, \"read_ascii_line: non-ASCII input\")\n\t\t\tos.Exit(1)\n\t\t}\n\t}\n\treturn line, true\n}\n\n";

/// Clamp an exit code into `0..=125` per `specs/exit-codes.md`.
const CLAMP_EXIT_DECL: &str = "func clampExit(n int64) int64 {\n\tif n < 0 {\n\t\treturn 0\n\t}\n\tif n > 125 {\n\t\treturn 125\n\t}\n\treturn n\n}\n\n";

/// Wrap a `*big.Int` into the 128-bit two's-complement (signed) or
/// modular (unsigned) range, matching JS `BigInt.asIntN(128, …)` /
/// `asUintN(128, …)`. `signed` selects i128 vs u128.
const WRAP128_DECL: &str = "var kioMod128 = new(big.Int).Lsh(big.NewInt(1), 128)\nvar kioHalf128 = new(big.Int).Lsh(big.NewInt(1), 127)\n\nfunc kioWrap128(v *big.Int, signed bool) *big.Int {\n\tr := new(big.Int).Mod(v, kioMod128)\n\tif r.Sign() < 0 {\n\t\tr.Add(r, kioMod128)\n\t}\n\tif signed && r.Cmp(kioHalf128) >= 0 {\n\t\tr.Sub(r, kioMod128)\n\t}\n\treturn r\n}\n\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_call_uses_the_exact_declaring_module() {
        assert_eq!(go_main_call("main"), "\tpkg.Main.Main()\n");
        assert_eq!(
            go_main_call("testapi/main"),
            "\tpkg.Testapi.KioModule_main.Main()\n"
        );
        assert_eq!(go_main_call("prog"), "\tpkg.Prog.Main()\n");
        assert_eq!(go_main_call("api"), "\tpkg.Api.Main()\n");
    }

    #[test]
    fn array_runtime_detection_uses_the_exact_fixture_not_the_leaf() {
        let renamed_array = [HostTypeBinding::opaque(
            "collections/internal",
            "Vector",
            1,
            HostTypeFixture::Array,
        )];
        assert!(needs_array_runtime(&renamed_array));

        let misleading_leaf = [HostTypeBinding::opaque(
            "tokens",
            "Array",
            0,
            HostTypeFixture::Token,
        )];
        assert!(!needs_array_runtime(&misleading_leaf));
    }

    #[test]
    fn array_index_slots_use_the_exact_root_binding_not_a_missing_alias() {
        let protocol = RunnerProtocol::parse("testapi-array").expect("known protocol");
        let contract = protocol.contract();
        let selection = GoHostBindingSelection::new(contract);
        let get = contract
            .host_fns
            .iter()
            .find(|binding| {
                matches!(
                    binding.body,
                    HostFnBodyKind::Array {
                        operation: "get",
                        ..
                    }
                )
            })
            .expect("array protocol has get");
        let get = go_array_method(get, "demo", &selection, "get");
        assert_eq!(get.arg_types[1], "int32");
        assert!(!get.arg_types[1].contains("Env_"));

        let len = contract
            .host_fns
            .iter()
            .find(|binding| {
                matches!(
                    binding.body,
                    HostFnBodyKind::Array {
                        operation: "len",
                        ..
                    }
                )
            })
            .expect("array protocol has len");
        let len = go_array_method(len, "demo", &selection, "len");
        assert_eq!(len.ret_type, "int32");
        assert!(!len.ret_type.contains("Env_"));
    }

    #[test]
    fn selector_collision_host_pins_readable_and_fallback_role_adapters() {
        let protocol = RunnerProtocol::FacadeSelectorCollisions;
        let selection = GoHostBindingSelection::new(protocol.contract());
        let host = go_host_api_for_protocol(protocol, "fixture", &selection);
        let driver = build_driver(&host, protocol, "fixture", "fixture");

        assert_eq!(
            go_host_type_frame("api/foo_bar", "read_value"),
            "V1_M2_C3_apiC6_fooBarN9_readValue"
        );
        assert_eq!(
            go_role_adapter_identity("testapi/foo_bar_", "I32"),
            "V1_M2_C7_testapiC8_fooBar_uN3_I32"
        );

        assert!(
            driver.contains("func (StubHost) KioItem_testapi_sfooBar__read("),
            "{driver}"
        );
        for adapter in [
            "testapi_I32",
            "testapi_foo_bar_I32",
            "testapi_fooBar_I32",
            "testapi_i_I32",
        ] {
            assert!(
                driver.contains(&format!("func (StubHost) KioHostIn_{adapter}(")),
                "missing input adapter `{adapter}` in:\n{driver}"
            );
            assert!(
                driver.contains(&format!("func (StubHost) KioHostOut_{adapter}(")),
                "missing output adapter `{adapter}` in:\n{driver}"
            );
        }
    }

    #[test]
    fn rejects_removed_main_entry_option() {
        assert_eq!(run(&["--main-entry=function".to_owned()]), EXIT_USAGE);
    }

    #[test]
    fn loop_signature_keeps_the_exact_function_identity() {
        let binding = HostFnBinding {
            module: "misleading/module",
            leaf: "not_loop",
            body: HostFnBodyKind::Loop,
        };

        let selection = GoHostBindingSelection::new(
            RunnerProtocol::parse("testapi-print")
                .expect("known protocol")
                .contract(),
        );
        let method = go_canonical_method(&binding, "demo", &selection);
        let member = go_host_member(binding.module, binding.leaf);
        let root_arguments = selection.root_arguments();
        assert_eq!(method.name, member);
        assert_eq!(
            method.arg_types,
            vec![
                format!("demo.Env_{member}_arg0{root_arguments}"),
                format!("demo.Env_{member}_arg1{root_arguments}"),
            ]
        );
        assert_eq!(
            method.ret_type,
            format!("demo.Env_{member}_ret{root_arguments}")
        );
        assert!(method.type_params.is_empty());
        assert!(method.where_clause.is_empty());

        let body =
            render_go_body_for_binding(&binding, &CanonicalKind::Loop, &method, "demo", &selection);
        assert!(body.contains("arg0(s).Case().(type)"), "{body}");
        let cbret = format!("demo.Env_{member}_arg0_cbret{root_arguments}");
        let (case_0, case_1) = sum_cases(&cbret);
        assert!(body.contains(&format!("case {case_0}:")), "{body}");
        assert!(body.contains(&format!("case {case_1}:")), "{body}");
        assert!(body.contains("step.Value()"), "{body}");
    }

    #[test]
    fn unit_host_return_omits_clause_and_uses_the_selected_exact_type() {
        let protocol = RunnerProtocol::parse("testapi-print").expect("known protocol");
        let contract = protocol.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::Print { .. }))
            .expect("testapi-print has print");
        let selection = GoHostBindingSelection::new(contract);
        let method = go_method(binding, "demo", &selection);
        let member = go_host_member(binding.module, binding.leaf);

        assert_eq!(method.arg_types, vec!["string"]);
        assert_eq!(method.ret_type, "()");
        assert_eq!(render_method_sig(&method), format!("{member}(arg0 string)"));
    }

    #[test]
    fn bottom_host_return_keeps_erased_alias() {
        let contract = RunnerProtocol::TestApiIo.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::Exit { .. }))
            .expect("testapi-io has exit");
        let selection = GoHostBindingSelection::new(contract);
        let method = go_method(binding, "demo", &selection);
        let ret = "demo.Env_Testapi_io__exit_ret[int32, string]";

        assert_eq!(method.arg_types, vec!["int32"]);
        assert_eq!(method.ret_type, ret);
        assert_eq!(
            render_method_sig(&method),
            "Testapi_io__exit(arg0 int32) demo.Env_Testapi_io__exit_ret[int32, string]"
        );
        assert_eq!(
            render_go_body_for_binding(
                binding,
                &canonical_kind(binding.body),
                &method,
                "demo",
                &selection,
            ),
            "os.Exit(int(clampExit(int64(arg0)))); panic(\"unreachable\")"
        );
    }

    #[test]
    fn callback_product_uses_canonical_flat_exact_slots() {
        let protocol = RunnerProtocol::parse("host-callback-roundtrip").expect("known protocol");
        let contract = protocol.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::CallStep { .. }))
            .expect("host-callback-roundtrip has call_step");
        let selection = GoHostBindingSelection::new(contract);
        let method = go_method(binding, "demo", &selection);
        let body = render_go_body_for_binding(
            binding,
            &canonical_kind(binding.body),
            &method,
            "demo",
            &selection,
        );

        assert_eq!(method.arg_types[0], "func(int32, string, bool) int32");
        assert_eq!(method.arg_types[1], "int32");
        assert_eq!(method.ret_type, "int32");
        assert_eq!(body, "return arg0(arg1, \"compound-callback\", true)");
    }

    #[test]
    fn interleaved_direct_roots_use_the_exact_selected_type() {
        let protocol =
            RunnerProtocol::parse("host-interleaved-stage-roundtrip").expect("known protocol");
        let contract = protocol.contract();
        let selection = GoHostBindingSelection::new(contract);
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::StagedSecond { .. }))
            .expect("interleaved protocol has staged_second");
        let method = go_method(binding, "demo", &selection);

        assert_eq!(method.arg_types, vec!["string", "string"]);
        assert_eq!(method.ret_type, "string");
    }

    #[test]
    fn coexist_driver_selects_exact_roots_and_supplies_role_adapters() {
        let driver = build_coexist_driver("first_pkg", "second_pkg");

        assert!(
            driver.contains("first_pkg.CreateFirstPkg[int32, string](hostA{})"),
            "{driver}"
        );
        assert!(
            driver.contains("second_pkg.CreateSecondPkg[int32, string](hostB{})"),
            "{driver}"
        );
        for receiver in ["hostA", "hostB"] {
            assert!(
                driver.contains(&format!("func ({receiver}) KioHostIn_greeter_I32")),
                "{driver}"
            );
            assert!(
                driver.contains(&format!("func ({receiver}) KioHostOut_greeter_String")),
                "{driver}"
            );
        }
    }

    #[test]
    fn forall_keeps_the_root_alias_prefix() {
        let protocol =
            RunnerProtocol::parse("host-poly-function-newtype-roundtrip").expect("known protocol");
        let contract = protocol.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::RoundPicker))
            .expect("host-poly-function-newtype-roundtrip has round_picker");
        let selection = GoHostBindingSelection::new(contract);
        let method = go_method(binding, "demo", &selection);
        let member = go_host_member(binding.module, binding.leaf);
        let root_arguments = selection.root_arguments();

        assert_eq!(
            method.arg_types,
            vec![format!("demo.Env_{member}_arg0{root_arguments}")]
        );
        assert_eq!(
            method.ret_type,
            format!("demo.Env_{member}_ret{root_arguments}")
        );
        assert!(!method.arg_types[0].contains("_result"));
        assert!(!method.ret_type.contains("_result"));
    }

    #[test]
    fn sum_api_is_derived_only_from_the_exact_alias() {
        let alias = "demo.Env_Testapi_io__read_ascii_line_ret";
        assert_eq!(
            sum_cases(alias),
            (format!("{alias}_0"), format!("{alias}_1"))
        );
        assert_eq!(
            sum_constructors(alias),
            (
                "demo.NewEnv_Testapi_io__read_ascii_line_ret_0".to_owned(),
                "demo.NewEnv_Testapi_io__read_ascii_line_ret_1".to_owned(),
            )
        );
        let generic = "demo.Env_Testapi_io__read_ascii_line_ret[int32, any]";
        assert_eq!(
            sum_cases(generic),
            (
                "demo.Env_Testapi_io__read_ascii_line_ret_0[int32, any]".to_owned(),
                "demo.Env_Testapi_io__read_ascii_line_ret_1[int32, any]".to_owned(),
            )
        );
        assert_eq!(
            sum_constructors(generic),
            (
                "demo.NewEnv_Testapi_io__read_ascii_line_ret_0[int32, any]".to_owned(),
                "demo.NewEnv_Testapi_io__read_ascii_line_ret_1[int32, any]".to_owned(),
            )
        );

        let body = render_loop_body("demo.Env_Testapi_control__loop_arg0_cbret");
        assert!(body.contains(".Case().(type)"), "{body}");
        assert!(body.contains(".Value()"), "{body}");
    }

    #[test]
    fn structural_export_driver_uses_alias_local_sum_constructors() {
        let selection =
            GoHostBindingSelection::new(RunnerProtocol::ExportStructuralRoundtrip.contract());
        let root_arguments = selection.root_arguments();
        let body = render_export_driver(
            ExportDriver::StructuralRoundtrip,
            "pkg.Testapi",
            "demo",
            true,
            &selection,
        )
        .expect("structural driver");

        assert!(
            body.contains(&format!(
                "demo.NewExp_Testapi_main__dispatchLeft_arg0_0{root_arguments}(7)"
            )),
            "{body}"
        );
        assert!(
            body.contains(&format!(
                "demo.NewExp_Testapi_main__classify_arg0_9{root_arguments}(\"ten\")"
            )),
            "{body}"
        );
        assert!(body.contains(".PairSwap(42, \"hello\")"), "{body}");
        assert!(
            body.contains(".Rotate(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12)"),
            "{body}"
        );
    }

    #[test]
    fn public_word_names_driver_pins_affixes_and_nested_selectors() {
        let selection = GoHostBindingSelection::new(RunnerProtocol::PublicWordNames.contract());
        let body = render_export_driver(
            ExportDriver::PublicWordNames,
            "pkg",
            "demo",
            false,
            &selection,
        )
        .expect("driver");
        for literal in [
            "pkg.WordApi.ReadWord()",
            "pkg.WordApi.KioItem__ureadWord()",
            "pkg.WordApi.KioItem_readWord_u()",
            "pkg.WordApi.KioItem__ureadWord_u()",
            "pkg.WordApi.KioItem_readWord_u_u()",
            "KioModule_wordNodes.KioType__uWordBox_u_u.WrapWord(55)",
            "KioModule_wordNodes.KioType__uWordBox_u_u.UnwrapWord(boxed)",
            "KioModule_wordNodes.KeepWord(66)",
            "pkg.WordApi.KeepPair(77, 88)",
            "pair.KioKey_V1_B13__uWordBox_u_u",
            "pair.KioKey_V1_Q2_M7_wordApiM10_otherNodesN13__uWordBox_u_u",
        ] {
            assert!(body.contains(literal), "{body}");
        }
    }

    #[test]
    fn newtype_callback_product_uses_canonical_flat_slots() {
        let selection =
            GoHostBindingSelection::new(RunnerProtocol::NewtypeVisibilityFacade.contract());
        let selected_i32 = selection.host_type(HostTypeIdentity::new("testapi", "I32"));
        let body = render_export_driver(
            ExportDriver::NewtypeVisibilityFacade,
            "pkg.Testapi",
            "demo",
            true,
            &selection,
        )
        .expect("newtype facade driver");

        assert!(
            body.contains(&format!(
                "MakeConstructorPair({selected_i32}(81), {selected_i32}(82))"
            )),
            "{body}"
        );
        assert!(
            body.contains(&format!(
                "func(left {selected_i32}, right {selected_i32}) {selected_i32}"
            )),
            "{body}"
        );
        assert!(
            body.contains("unpacked(constructorPairOut.F0, constructorPairOut.F1)"),
            "{body}"
        );
        assert!(
            body.contains("MakeProjectorPairValue(constructorPairOut.F0, constructorPairOut.F1)"),
            "{body}"
        );
        assert!(!body.contains("_arg0_cbarg0"), "{body}");
    }
}
