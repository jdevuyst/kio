//! Exit codes per `specs/exit-codes.md`.
//!
//! Each variant maps to a distinct process exit status from the
//! spec table.
//!
//! Tests under `test-data/goldens/` assert on the numeric value via
//! `expected.exit`.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ExitCode {
    Success,
    Internal,
    Usage,
    Compile,
    Parse,
    Import,
    NameRes,
    Type,
    Elaborator,
    Totality,
    Bridge,
    /// Dependency / resolution error (non-specific) per
    /// `specs/exit-codes.md`'s reserved `3x` tier. Numerically `30`:
    /// an unresolvable remote, a locked commit unobtainable, a lock out
    /// of sync with the `*.dep.kio` set, a malformed lock, a
    /// dependency-name / local-root collision, a dependency missing or
    /// carrying a malformed `<pkg>.sig.kio`, or a tampered upgrade.
    /// Finer `31`–`39` can factor out later.
    Dep,
    Build,
    /// `kio test`: at least one `equiv` block did not verify (its
    /// `term`s reduced to ≥2 distinct normal forms, or otherwise
    /// failed cleanly). Per `specs/exit-codes.md`'s `5x` tier.
    TestFailure,
    /// `kio fmt --check`: at least one input file's canonical form
    /// differs from disk. Numerically `60` per the `6x` fmt-check
    /// tier in `specs/exit-codes.md`. Distinct from `gofmt -l` /
    /// `cargo fmt --check` (both `1`); Kio reserves `1` for the
    /// internal-error category.
    FmtDiff,
    /// `kio doc`: a Kiodoc contract violation in one of the visited
    /// markdown files, or a snippet whose `kio check` exit didn't
    /// match its declared `check_exit_code` (default `0`).
    /// Numerically `70` per the `7x` doc-check tier in
    /// `specs/exit-codes.md`; `71`–`79` are reserved for later.
    /// See `specs/kiodoc.md` for the directive contract.
    DocError,
    /// `kio sig status`: the live contract surface breaks the last
    /// sealed contract and the break is **unrecorded**. Numerically
    /// `80`, the highest-precedence state in the `8x` `kio sig status`
    /// command tier. The `1x` typecheck cascade short-circuits before
    /// any sig comparison, so a malformed package never reports `8x`.
    SigIncompatible,
    /// `kio sig status`: an unrecorded *compatible* delta — the live
    /// surface drifted from the sealed contract in a backward-compatible
    /// way that the draft does not yet record. Numerically `81`.
    SigStale,
    /// `kio sig status`: a break **is** recorded in the draft but not
    /// yet sealed (run `kio sig commit`). Numerically `82`. Outranks `81`
    /// in the status precedence (a recorded-but-unsealed break is not
    /// "clean"), but is outranked by `80` (an unrecorded break).
    SigUnsealedBreak,
}

impl ExitCode {
    pub fn as_i32(self) -> i32 {
        match self {
            ExitCode::Success => 0,
            ExitCode::Internal => 1,
            ExitCode::Usage => 2,
            ExitCode::Compile => 10,
            ExitCode::Parse => 11,
            ExitCode::Import => 12,
            ExitCode::NameRes => 13,
            ExitCode::Type => 14,
            ExitCode::Elaborator => 15,
            ExitCode::Totality => 16,
            ExitCode::Bridge => 20,
            ExitCode::Dep => 30,
            ExitCode::Build => 40,
            ExitCode::TestFailure => 50,
            ExitCode::FmtDiff => 60,
            ExitCode::DocError => 70,
            ExitCode::SigIncompatible => 80,
            ExitCode::SigStale => 81,
            ExitCode::SigUnsealedBreak => 82,
        }
    }
}
