//! `kio-test-runner-ts` — pointed at a `kio build ts` output
//! directory, evaluates the emitted ES module via rquickjs (Rust
//! bindings around the QuickJS C engine) and reports success or
//! failure as an exit code.
//!
//! The TypeScript backend is **pure skin**: `kio build ts` writes the JS
//! backend's `<ns>.js` byte-identical (`specs/backends/ts.md` § Output
//! layout) plus a `<ns>.d.ts` typed-skin sidecar. The runtime artifact
//! the runner executes is that `<ns>.js` — the `.d.ts` carries types
//! for a TypeScript *host* and has no bearing on running the package.
//! So this runner runs exactly the same artifact, the same way, as
//! `kio-test-runner-js`: it shares the entire JS execution engine
//! ([`js_exec`](../shared/js_exec.rs)) — the host-record builders,
//! native callables, protocol drivers, and `<ns>.js` discovery — and
//! owns only `fn main`, argv parsing, and the TS-specific `USAGE` text.
//!
//! The `.d.ts` is type-checked separately by `tsc --strict --noEmit` in
//! CI (the paired job); the runner does not consult it, honoring the
//! decoupling red lines (it reads no `.kio` source and no emitted file
//! to reconstruct the interface — it builds the host record from the
//! selected protocol alone). See
//! `ci/infra/kio-test-runner-rs/README.md` for the protocol model and
//! the canonical-body catalogue.
//!
//! Exits 0 on clean evaluation, 1 on any JS exception, 2 for a
//! malformed invocation (the CLI usage tier per `specs/exit-codes.md`).
//! A module call to `exit(n)` exits with `n` verbatim (clamped).

use std::env;
use std::path::Path;
use std::process;

// The shared modules are declared at the crate root because their
// cross-references use `crate::<mod>` paths; the JS execution engine
// (`js_exec`, shared with `kio-test-runner-js`) reaches them the same
// way. The `ts` backend's `<ns>.js` is the JS backend's, byte-identical,
// so the engine is reused verbatim.
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
Usage: kio-test-runner-ts [--protocol <name>] <output-dir>

Evaluate the ES module emitted by `kio build ts` in an embedded JS
engine. `kio build ts` writes the JS backend's `<ns>.js` byte-identical
plus a `<ns>.d.ts` typed-skin sidecar; this runner runs the `<ns>.js`
(the `.d.ts` is type-checked separately by `tsc`). It instantiates the
package via the branded `create<Handle>(host)` factory (its name derived
from the harness-supplied effective artifact namespace) with a small default
host record (see the module docs); if the returned package exposes a callable
`main`, it's invoked.

Arguments:
  <output-dir>   Directory containing the package module
                 (`<ns>.js`) emitted by `kio build ts`
                 (e.g., `out/ts/` for a target whose `out` is
                 `out/ts/`).

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
    // Match the JS bin: run the QuickJS engine on a dedicated worker
    // thread with a generous stack so deep Rust frames around the FFI
    // boundary have headroom in debug builds, and the runtime's
    // lifetime is isolated from the main-thread `process::exit` paths.
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
    let identities = match identity_args.resolve("ts", expected_packages) {
        Ok(identities) => identities,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    // The coexist protocol is the one two-artifact protocol: exactly two
    // positional output dirs; every other protocol takes exactly one. The
    // runtime artifact is the JS backend's byte-identical `<ns>.js`, so
    // the shared coexist execution covers the ts row; the `.d.ts` skins
    // are independently type-checked by the paired per-case `tsc` job,
    // and ES-module isolation already makes a two-import TS host
    // collision-free by construction.
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
