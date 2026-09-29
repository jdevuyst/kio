//! `kio-test-runner-js` — pointed at a `kio build js` output
//! directory, evaluates the emitted ES module via rquickjs (Rust
//! bindings around the QuickJS C engine) and reports success or
//! failure as an exit code.
//!
//! The execution engine — the host-record builders, native callables,
//! protocol drivers, and exact `<ns>.js` loading — lives in
//! [`js_exec`](../shared/js_exec.rs), shared with `kio-test-runner-ts`
//! (the `ts` backend emits the JS backend's `<ns>.js` byte-identical, so
//! both bins run the same artifact the same way). This bin owns only
//! `fn main`, its argv parsing, and the JS-specific `USAGE` text.
//!
//! Pipeline (the engine's [`js_exec::JsRunner`] `TestRunner`
//! implementation):
//!
//! 1. `host_api` — projects the selected **protocol**'s exact
//!    host-function bindings (`--protocol <name>`); the runner reads no
//!    emitted file. The JS backend is dynamically typed, so it needs
//!    each function's qualified identity and body kind, not native type
//!    syntax.
//! 2. `execute_artifact`:
//!    1. Register the I/O / process callables (`print`, `eprint`,
//!       `exit`) on the global object so the host record's
//!       arrow-fn forwarders can reach them by name.
//!    2. Locate the package's single JS module
//!       (`<ns>.js`) in the supplied directory; declare it as
//!       an ES module via `Module::declare`, then `eval()` and
//!       synchronously drain the resulting promise.
//!    3. Build the runner's default host record and call the branded
//!       `create<Handle>(host)` factory — its name derived from the
//!       effective artifact namespace — to instantiate the package.
//!    4. If the resulting package value exposes a callable `main`,
//!       invoke it.
//!
//! The host record contains exactly the selected protocol's host
//! functions. An unknown custom body throws a loud "no canonical impl"
//! stub if called. For the full canonical-body catalogue and protocol
//! model see `ci/infra/kio-test-runner-rs/README.md`.
//!
//! Exits 0 on clean evaluation, 1 on any JS exception (parse error,
//! reference error, throw during `main`); 2 is the CLI usage tier per
//! `specs/exit-codes.md`. A module call to `exit(n)` exits with `n`
//! verbatim (clamped).
//!
//! ## Protocol-driven host bindings
//!
//! The protocol carries structured host-function bindings. The JS
//! engine uses each exact module+leaf identity and body kind but does
//! not need to render native type syntax. The Rust runner projects the
//! same structure into a typed `impl Host`; neither runner reads an
//! emitted interface.

use std::env;
use std::path::Path;
use std::process;

// The shared modules are declared at the crate root because their
// cross-references use `crate::<mod>` paths; the JS execution engine
// (`js_exec`, shared with `kio-test-runner-ts`) reaches them the same
// way.
#[path = "../shared/artifact_identity.rs"]
mod artifact_identity;
#[path = "../shared/host_api.rs"]
mod host_api;
#[path = "../shared/js_exec.rs"]
mod js_exec;
#[path = "../shared/protocol.rs"]
mod protocol;
#[path = "../shared/runner.rs"]
mod runner;

use artifact_identity::ArtifactIdentityArgs;
use js_exec::JsRunner;
use protocol::RunnerProtocol;
use runner::{EXIT_USAGE, TestRunner};

const USAGE: &str = "\
Usage: kio-test-runner-js [--protocol <name>] <output-dir>

Evaluate the ES module emitted by `kio build js` in an embedded JS
engine. The runner instantiates the package via the branded
`create<Handle>(host)` factory (its name derived from the harness-supplied
effective artifact namespace) with a small default host record (see the module docs); if the
returned package exposes a callable `main`, it's invoked.

Arguments:
  <output-dir>   Directory containing the JS package module
                 (`<ns>.js`) emitted by `kio build js`
                 (e.g., `out/js/` for a target whose `out` is
                 `out/js/`).

Options:
  --package-name <name>
                 Kio source package name supplied by the corpus harness.
                 Repeat exactly twice for `--protocol coexist`.
  --artifact-namespace <namespace>
                 Effective namespace of the preceding package artifact.
  --protocol <name>
                 Host/Kio interaction protocol to run. Defaults to
                 `empty-main` — the exact empty-host contract that
                 instantiates the package and invokes exported `main`.
                 See the runner README for the protocol catalogue.
  -h, --help      Show this help and exit.
";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    // QuickJS executes bytecode iteratively in C, so the embedded
    // engine itself doesn't blow the Rust stack. We still run on a
    // dedicated worker thread with a generous stack: it gives debug
    // builds headroom for deep Rust frames around the FFI boundary,
    // and it isolates the QuickJS runtime's lifetime from the
    // main-thread `process::exit` paths.
    let exit_code = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || run(&args))
        .expect("test-runner worker thread")
        .join()
        .expect("worker thread panicked");
    process::exit(exit_code);
}

fn run(args: &[String]) -> i32 {
    let mut positional: Vec<&str> = Vec::new();
    let mut protocol = RunnerProtocol::default();
    let mut protocol_seen = false;
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
            s if s.starts_with("--") => {
                eprintln!("error: unknown option: {s}");
                return EXIT_USAGE;
            }
            s => positional.push(s),
        }
    }

    let expected_packages = if protocol == RunnerProtocol::Coexist {
        2
    } else {
        1
    };
    let identities = match identity_args.resolve("js", expected_packages) {
        Ok(identities) => identities,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    // The coexist protocol is the one two-artifact protocol: exactly two
    // positional output dirs; every other protocol takes exactly one.
    if protocol == RunnerProtocol::Coexist {
        return match positional.as_slice() {
            [a, b] => match js_exec::run_coexist(
                Path::new(*a),
                &identities[0],
                Path::new(*b),
                &identities[1],
            ) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("error: executing coexist artifacts: {e}");
                    runner::EXIT_RUNTIME_FAILURE
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

    JsRunner::new(protocol, identities.into_iter().next().unwrap()).run(dir, protocol)
}
