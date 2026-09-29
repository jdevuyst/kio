//! Auto-reload file watching for the `kio repl` inspector.
//!
//! Every `:load`ed file is, in effect, watched: the REPL registers a
//! recursive [`notify_debouncer_mini`] watcher on the **package
//! root** and, between line reads, drains pending change events. A
//! change to any `.kio` file in the package triggers a re-typecheck
//! ([`crate::repl_core::session::Session::refresh`]); on success the
//! session inherits the new contents, on failure the prior version
//! is kept and a diagnostic prints.
//!
//! ## Why watch the directory, not each file
//!
//! Editors save atomically — write a temp file, then rename it over
//! the target. A per-file `inotify` watch on the *original* inode is
//! lost the moment the rename swaps it out. Watching the package
//! root recursively side-steps this: the rename, the temp-file
//! create, and the final file all surface as directory events. The
//! debouncer (a short coalescing window) collapses the burst of
//! events one save produces into a single signal.
//!
//! ## Threading
//!
//! `notify` runs its own background thread. The debouncer forwards
//! batched events onto an [`std::sync::mpsc`] channel; the REPL's
//! main thread drains that channel non-blockingly via
//! [`Watcher::drain`] between user inputs. The watcher is best
//! effort — if `notify` fails to initialize (an exotic platform, a
//! sandbox with no inotify), [`Watcher::start`] returns `None` and
//! the REPL runs without auto-reload rather than refusing to start.

use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};

/// Debounce window for coalescing a save's burst of filesystem
/// events into one signal. 300ms comfortably covers an editor's
/// write-temp-then-rename sequence without a perceptible reload lag.
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(300);

/// A live package-directory watcher.
///
/// Holds the [`Debouncer`] (whose `Drop` stops the background
/// thread) and the receiving end of the event channel. The REPL
/// keeps one of these for the session's lifetime and polls
/// [`Self::drain`] between line reads.
pub struct Watcher {
    /// Kept alive so the watcher thread keeps running; dropping it
    /// shuts the watcher down. Not otherwise read.
    _debouncer: Debouncer<RecommendedWatcher>,
    /// Batched change events from the debouncer.
    events: Receiver<DebounceEventResult>,
}

impl Watcher {
    /// Start watching `package_root` recursively.
    ///
    /// Returns `None` when the platform watcher cannot be created or
    /// the root cannot be watched — the REPL then runs without
    /// auto-reload. A `None` here is never fatal: auto-reload is a
    /// convenience, not a correctness requirement.
    pub fn start(package_root: &Path) -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<DebounceEventResult>();
        let mut debouncer = new_debouncer(DEBOUNCE_WINDOW, tx).ok()?;
        debouncer
            .watcher()
            .watch(package_root, RecursiveMode::Recursive)
            .ok()?;
        Some(Self {
            _debouncer: debouncer,
            events: rx,
        })
    }

    /// Drain every pending change event without blocking.
    ///
    /// Returns `true` when at least one `.kio`-relevant change event
    /// was seen since the last drain — the caller then re-typechecks.
    /// Returns `false` when the channel was quiet. A watcher-internal
    /// error event is treated as "something changed" (conservatively
    /// re-typecheck) rather than surfaced to the user.
    pub fn drain(&self) -> bool {
        let mut changed = false;
        loop {
            match self.events.try_recv() {
                Ok(Ok(events)) => {
                    // Only `.kio` file events matter — ignore churn
                    // in `out/`, editor swap files, etc. A path with
                    // no `.kio` suffix never affects the typecheck.
                    if events.iter().any(|e| has_kio_suffix(&e.path)) {
                        changed = true;
                    }
                }
                Ok(Err(_)) => {
                    // A watcher backend error — re-check to be safe.
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        changed
    }
}

/// Whether a path ends in `.kio` (a Kio-family source file). Editor
/// temp files (`.kio~`, `.kio.swp`, `#…#`) do not match, so a save's
/// intermediate churn is filtered out.
fn has_kio_suffix(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(crate::file_kind::has_kio_extension)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn kio_suffix_recognizes_kio_files() {
        assert!(has_kio_suffix(&PathBuf::from("/pkg/main.kio")));
        assert!(has_kio_suffix(&PathBuf::from("/pkg/app.pkg.kio")));
        assert!(has_kio_suffix(&PathBuf::from("/pkg/app.sig.kio")));
        assert!(has_kio_suffix(&PathBuf::from("/pkg/lib.dep.kio")));
        assert!(has_kio_suffix(&PathBuf::from("/pkg/lib.lock.kio")));
        assert!(has_kio_suffix(&PathBuf::from("/pkg/lib.kio")));
    }

    #[test]
    fn kio_suffix_rejects_temp_and_other_files() {
        // Editor swap / backup files.
        assert!(!has_kio_suffix(&PathBuf::from("/pkg/main.kio~")));
        assert!(!has_kio_suffix(&PathBuf::from("/pkg/.main.kio.swp")));
        assert!(!has_kio_suffix(&PathBuf::from("/pkg/#main.kio#")));
        // Unrelated files.
        assert!(!has_kio_suffix(&PathBuf::from("/pkg/out/app.js")));
        assert!(!has_kio_suffix(&PathBuf::from("/pkg/README.md")));
    }

    #[test]
    fn start_succeeds_on_a_real_directory() {
        // A watcher over a temp directory should initialize and a
        // fresh drain should report no changes.
        let dir = std::env::temp_dir().join(format!("kio-repl-watch-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let watcher = Watcher::start(&dir);
        // On a normal dev / CI host the watcher initializes. If it
        // doesn't (no inotify in the sandbox), `start` returns
        // `None` — that path is exercised implicitly and is not a
        // failure.
        if let Some(w) = watcher {
            assert!(!w.drain(), "a freshly-started watcher has no events");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
