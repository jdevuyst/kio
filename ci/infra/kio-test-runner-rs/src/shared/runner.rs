//! The `TestRunner` trait — the per-backend contract each runner
//! bin implements.
//!
//! The trait is deliberately small: each per-backend bin still owns
//! its `fn main`, its argv parsing, and its main loop. What the
//! trait pins is *what backend-specific work the runner does* and
//! how its outputs are shaped. The shared crate's bias is for the
//! per-backend bin to remain readable on its own — the trait is a
//! checklist for "a new backend's runner does these N things,"
//! not a generic framework swallowing the bin's `fn main`.
//!
//! `compile_driver` and `spawn` are merged into a single
//! `execute_artifact` op because every per-backend bin today either
//! spawns a fresh process (Rust) or evaluates in-process (JS) —
//! splitting compile and spawn into distinct trait methods adds
//! ceremony without enabling reuse.

use std::path::Path;

use crate::host_api::{self, HostApi};
use crate::protocol::RunnerProtocol;

/// The exit-code tier the runner uses for its own malformed-invocation
/// errors — `2`, per `specs/exit-codes.md` (CLI usage tier).
pub const EXIT_USAGE: i32 = 2;

/// The exit-code tier the runner uses for a runtime failure while
/// executing the artifact (a JS exception, a `rustc` failure, a
/// missing `main`, etc.) — `1`, per `specs/exit-codes.md`.
pub const EXIT_RUNTIME_FAILURE: i32 = 1;

/// A per-backend test runner.
///
/// Implementations live in the per-backend bin (`ci/infra/kio-test-
/// runner-rs/src/bin/kio-test-runner-<backend>.rs`). The harness
/// constructs an instance once per process invocation and drives
/// it through `run`, the default-method entry point that orders
/// the per-backend ops.
///
/// ## Lifetime / setup
///
/// The trait carries no associated data lifetimes — implementations
/// own their own state (a tempdir handle, an embedded interpreter
/// context, a cache handle). The runner is constructed once per
/// process invocation; `fn main` in the per-backend bin builds it
/// from argv and calls [`TestRunner::run`].
pub trait TestRunner {
    /// Project the host API the runner will implement from its protocol.
    ///
    /// The complete host inventory, every signature, and every native
    /// fixture come from the selected exact **protocol**, never from
    /// emitted source or case-specific semantic metadata. The method
    /// deliberately receives no artifact path: artifact loading and
    /// validation belong to [`Self::execute_artifact`], not semantic
    /// host-contract construction.
    fn host_api(&self) -> HostApi;

    /// Synthesize a default implementation of the host API and
    /// execute the artifact under it, returning the process /
    /// evaluation exit code.
    ///
    /// This merges what the spec's plan calls
    /// `synth_stub_impl + compile_driver + spawn` into one op
    /// because the three steps share state every per-backend
    /// runner today threads through together (the driver's
    /// source bytes feed both the rustc invocation and the
    /// emitter-spawned process; the JS evaluator builds the
    /// host record and immediately calls the package factory
    /// against it). This shape matches the current two bins without forcing
    /// artificial separation.
    ///
    /// `output_dir` is the `kio build <target>` output directory;
    /// `host_api` is the result of [`Self::host_api`].
    fn execute_artifact(
        &self,
        output_dir: &Path,
        host_api: &HostApi,
        protocol: RunnerProtocol,
    ) -> Result<i32, String>;

    /// Top-level entry the per-backend bin's `fn main` calls.
    ///
    /// The default impl wires the per-backend ops together in the
    /// canonical order. A per-backend bin overrides only when the
    /// ordering itself is backend-specific.
    ///
    /// Errors map to exit code `1` (runtime failure) per
    /// `specs/exit-codes.md`. The CLI usage tier (`2`) is the
    /// per-bin `fn main`'s responsibility, since it owns argv
    /// parsing.
    fn run(&self, output_dir: &Path, protocol: RunnerProtocol) -> i32 {
        let host_api = self.host_api();
        host_api::assert_host_api(protocol.contract(), &host_api);
        match self.execute_artifact(output_dir, &host_api, protocol) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: executing artifact: {e}");
                EXIT_RUNTIME_FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake `TestRunner` whose `host_api` returns a fixed shape
    /// and whose `execute_artifact` returns a configurable exit code.
    /// Used to exercise the default `run` impl.
    struct FakeRunner {
        host: HostApi,
        exit: i32,
        exec_err: Option<String>,
    }

    impl FakeRunner {
        fn new(host: HostApi, exit: i32) -> Self {
            Self {
                host,
                exit,
                exec_err: None,
            }
        }
        fn with_exec_err(mut self, e: &str) -> Self {
            self.exec_err = Some(e.to_owned());
            self
        }
    }

    impl TestRunner for FakeRunner {
        fn host_api(&self) -> HostApi {
            self.host.clone()
        }
        fn execute_artifact(
            &self,
            _: &Path,
            _: &HostApi,
            _: RunnerProtocol,
        ) -> Result<i32, String> {
            if let Some(e) = &self.exec_err {
                Err(e.clone())
            } else {
                Ok(self.exit)
            }
        }
    }

    fn dummy_host() -> HostApi {
        host_api::dynamic_host_api(RunnerProtocol::Empty.contract())
    }

    #[test]
    fn run_propagates_exit_code_on_happy_path() {
        let runner = FakeRunner::new(dummy_host(), 0);
        assert_eq!(runner.run(Path::new("/dev/null"), RunnerProtocol::Empty), 0);

        let runner = FakeRunner::new(dummy_host(), 42);
        assert_eq!(
            runner.run(Path::new("/dev/null"), RunnerProtocol::Empty),
            42
        );
    }

    #[test]
    fn run_returns_runtime_failure_on_execute_artifact_err() {
        let runner = FakeRunner::new(dummy_host(), 0).with_exec_err("synthetic exec error");
        assert_eq!(
            runner.run(Path::new("/dev/null"), RunnerProtocol::Empty),
            EXIT_RUNTIME_FAILURE
        );
    }
}
