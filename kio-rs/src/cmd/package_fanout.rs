//! Shared multi-package fan-out for the cwd-subtree commands.
//!
//! `kio build`, `kio sig`, and `kio doc build` each discover N package
//! roots in the cwd subtree and process them independently — own
//! workspace, own typecheck, own codegen, disjoint `out/` and
//! `*.sig.kio`, no cross-package rollback. The work is a stateless map
//! over the package directories, so it fans out across rayon like every
//! other compiler fan-out (`crate::par`).
//!
//! The one subtlety is output ordering: each per-package worker emits
//! its own diagnostics, and a naive parallel loop would interleave them
//! nondeterministically — breaking the golden contract. So a worker
//! never writes to the process streams directly; it writes into a
//! per-package [`CapturedOutput`] buffer, and [`run`] replays the
//! buffers (and the per-package banner) in input order after the
//! fan-out, exactly mirroring the in-order collect-then-print pattern at
//! `cmd::fmt`'s file fan-out and `cmd::test`'s equiv fan-out.
//!
//! Exit codes are reduced with a caller-supplied order-independent
//! combiner (`kio build`/`kio sig`'s first-failure or worst-severity
//! rules), so the chosen overall code is independent of execution order.

use crate::exit_code::ExitCode;
use crate::path_display::DisplayPath;
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// A per-package output buffer a worker writes into instead of printing
/// to the process streams. Replayed in input order after the fan-out so
/// parallel execution stays deterministic.
///
/// Use the [`out`]/[`outln`]/[`err`]/[`errln`] macros to append; they
/// route to `stdout` / `stderr` respectively and never fail (writing to
/// a `String` is infallible).
#[derive(Default)]
pub struct CapturedOutput {
    pub stdout: String,
    pub stderr: String,
}

impl CapturedOutput {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replay the captured buffers to the real process streams. Called
    /// once per package, in input order, after the fan-out.
    fn replay(&self) {
        if !self.stdout.is_empty() {
            let mut w = std::io::stdout().lock();
            let _ = w.write_all(self.stdout.as_bytes());
        }
        if !self.stderr.is_empty() {
            let mut w = std::io::stderr().lock();
            let _ = w.write_all(self.stderr.as_bytes());
        }
    }

    /// Replay the captured buffers to the real process streams. For a
    /// single-package caller (no parallel fan-out) that buffers only to
    /// share the buffered code path and replays immediately.
    pub fn replay_now(&self) {
        self.replay();
    }
}

/// Run `worker` over every package directory in parallel, replaying each
/// package's buffered output in input order and reducing the per-package
/// exit codes with `combine`.
///
/// - `banner_verb` orients the operator when more than one package is
///   processed (`"kio build"`, `"kio sig"`, `"kio doc build"`); it
///   prefixes the per-package `package i of N at <dir>` line written to
///   the captured stderr.
/// - `worker(dir, &mut CapturedOutput) -> ExitCode` does one package's
///   work, writing all its user-facing output into the buffer.
/// - `combine(acc, next) -> ExitCode` reduces the per-package codes;
///   it must be order-independent (e.g. first-failure or worst-severity)
///   since the workers run concurrently.
pub fn run<W, C>(package_dirs: &[PathBuf], banner_verb: &str, worker: W, combine: C) -> ExitCode
where
    W: Fn(&Path, &mut CapturedOutput) -> ExitCode + Sync,
    C: Fn(ExitCode, ExitCode) -> ExitCode,
{
    let multi = package_dirs.len() > 1;
    let total = package_dirs.len();

    let mut results: Vec<(usize, ExitCode, CapturedOutput)> = crate::maybe_par_iter!(package_dirs)
        .enumerate()
        .map(|(i, dir)| {
            let mut cap = CapturedOutput::new();
            if multi {
                let _ = writeln!(
                    cap.stderr,
                    "{banner_verb}: package {} of {} at {}",
                    i + 1,
                    total,
                    DisplayPath(dir)
                );
            }
            let code = worker(dir, &mut cap);
            (i, code, cap)
        })
        .collect();

    results.sort_by_key(|(i, _, _)| *i);

    let mut overall = ExitCode::Success;
    for (_, code, cap) in &results {
        cap.replay();
        overall = combine(overall, *code);
    }
    overall
}
