//! Path rendering for user-facing diagnostic output.
//!
//! `std::path::Path::display()` uses the OS-native separator (`\` on
//! Windows, `/` on Unix). Goldens, snapshot tests, and any other
//! byte-for-byte diff over diagnostic output therefore diverge by
//! host: the same compile produces `input/a.md: …` on Linux and
//! `input\a.md: …` on Windows, and a single expected-output fixture
//! cannot satisfy both.
//!
//! The [`DisplayPath`] wrapper renders a path with forward-slash
//! separators on every platform. It is intended **only** for
//! diagnostic / user-facing output — filesystem operations elsewhere
//! must still use the platform's native [`Path`] / [`PathBuf`] APIs.
//!
//! Mirrors the same-named wrapper in `kio-rs/src/path_display.rs`;
//! kept locally so the shared build cache and the per-backend runners
//! compile standalone here without a kio-rs dep.

use std::fmt;
use std::path::Path;

/// Diagnostic-output wrapper that renders a path with forward-slash
/// separators on every platform. See module-level docs.
pub struct DisplayPath<P>(pub P);

impl<P: AsRef<Path>> fmt::Display for DisplayPath<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.0.as_ref().to_string_lossy();
        // `cfg!(windows)` is a `const bool`; the dead branch is
        // eliminated at compile time, so Unix builds pay no
        // string-replacement cost and Windows-legitimate filenames
        // containing `\` aren't accidentally rewritten on Unix.
        if cfg!(windows) {
            f.write_str(&s.replace('\\', "/"))
        } else {
            f.write_str(&s)
        }
    }
}
