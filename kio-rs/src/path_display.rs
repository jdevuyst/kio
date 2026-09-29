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
//! Drop-in replacement at call sites: every `path.display()` becomes
//! `DisplayPath(&path)`. The wrapper accepts anything that's
//! `AsRef<Path>` — `Path`, `PathBuf`, `&Path`, `&PathBuf` — so the
//! call site doesn't need to track `&` levels. It stores a borrow
//! and only allocates inside `Display::fmt` on Windows when a
//! backslash is present in the path's UTF-8 lossy rendering.

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

/// Render `path` relative to an already-selected display root when possible.
///
/// This helper is deliberately lexical and performs no filesystem access.
/// Callers that need a canonical working-directory root discover it once at
/// their command boundary, then reuse it for every path in one diagnostic.
pub fn display_path_from_root(path: &Path, display_root: Option<&Path>) -> String {
    let displayed = display_root
        .and_then(|root| path.strip_prefix(root).ok())
        .unwrap_or(path);
    DisplayPath(displayed).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_path_from_root_is_purely_lexical() {
        assert_eq!(
            display_path_from_root(
                Path::new("/workspace/src/main.kio"),
                Some(Path::new("/workspace"))
            ),
            "src/main.kio"
        );
        assert_eq!(
            display_path_from_root(
                Path::new("/outside/main.kio"),
                Some(Path::new("/workspace"))
            ),
            "/outside/main.kio"
        );
    }
}
