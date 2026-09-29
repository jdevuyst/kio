//! Background analysis worker and debounce scheduler.
//!
//! `didChange` notifications arrive at typing speed — tens per second
//! during a fast burst. Running `analyze_workspace_at_with_overlay`
//! synchronously on each one would freeze the LSP for the duration of
//! the type-check; the cost compounds across packages and across
//! editors that send a notification per keystroke.
//!
//! Two-stage decoupling:
//!
//! 1. **Debounce scheduler.** A dedicated thread tracks one pending
//!    deadline (the per-package "ready to analyze again" time). Each
//!    `didChange` arrival resets the deadline to `now + DEBOUNCE`.
//!    When the deadline elapses with no further edits, the scheduler
//!    posts an "analyze package P with this overlay" message to the
//!    worker. Bursts coalesce to one analysis.
//! 2. **Worker thread.** Receives "analyze package P" messages,
//!    runs the typer against a cloned overlay snapshot, posts the
//!    result (a [`WorkerResult`]) to the main thread's notification
//!    channel. The main thread publishes diagnostics on receipt.
//!
//! Threads and channel ownership:
//!
//! - **Main thread.** Owns the [`ServerState`]; receives `didChange`
//!   events; schedules work via [`Scheduler::schedule`]; receives
//!   `WorkerResult`s and publishes diagnostics.
//! - **Debounce thread.** Sleeps until the next deadline; sends a
//!   `WorkRequest` to the worker thread when it fires.
//! - **Worker thread.** Receives `WorkRequest`s; runs analysis;
//!   sends `WorkerResult`s back to the main thread.
//!
//! Shutdown: every thread reads a sentinel `Stop` variant on its
//! input channel and exits its loop; the main thread sends both
//! sentinels and joins the threads.
//!
//! Other LSP requests (hover, goto-definition, ...) read the published
//! snapshot directly; they don't drive the worker.

// `lsp_types::Uri` carries a `Cell` for internal parse-result caching,
// which trips clippy's `mutable_key_type` lint when used as a map / set
// key. See `state.rs` for the equivalent suppression and rationale.
#![allow(clippy::mutable_key_type)]

use crossbeam_channel::{self as mpsc, Receiver, RecvTimeoutError, Sender};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use lsp_types::Uri;

use crate::cmd::check::{
    AnalysisFailure, LspAnalysis, LspUserElaboratorMemos,
    analyze_module_at_with_overlay_lsp_cancellable_with_memos,
    analyze_workspace_at_with_overlay_lsp_cancellable_with_memos,
};
use crate::lsp::cancel::CancellationToken;
use crate::lsp::snapshot::{Snapshot, SnapshotId, SnapshotIdGen};
use crate::package_collection::SourceOverlay;

/// How long to wait after the last edit before reanalysing. Matched
/// to rust-analyzer's default; revisit if user reports say it feels
/// laggy or jittery.
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// A scheduled analysis request the worker should run. The
/// scheduler builds this from the latest pending state, then sends
/// it on the work channel.
pub struct WorkRequest {
    /// Package root the worker passes to `analyze_workspace_at`.
    pub package_root: PathBuf,
    /// Per-file overlay snapshot, already canonicalized to the keys
    /// the walker uses. The worker hands this to
    /// `analyze_workspace_at_with_overlay`.
    pub overlay: SourceOverlay,
    /// Identity, versions and canonical overlay paths captured at scheduling.
    /// The worker forwards the same snapshot into its result.
    pub snapshot: Snapshot,
    /// Whether this request came from background diagnostics or from a
    /// foreground typed editor request that needed a fresher snapshot.
    pub priority: WorkPriority,
    /// Canonical file to prioritize for foreground typed requests.
    /// Background diagnostics leave this empty and analyze the full
    /// package after the debounce window.
    pub focus: Option<FocusedAnalysis>,
    /// Cooperative cancellation flag. Newer scheduled work cancels
    /// superseded tokens so in-flight analysis can stop at phase
    /// boundaries instead of running to completion only to be dropped.
    pub(crate) cancel_token: CancellationToken,
}

/// File-level focus for a foreground analysis request.
#[derive(Debug, Clone)]
pub struct FocusedAnalysis {
    pub uri: Uri,
    pub file_path: PathBuf,
}

/// Analysis scheduling lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkPriority {
    Background,
    Foreground,
}

impl WorkPriority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Foreground => "foreground",
        }
    }
}

/// One completed analysis. The main thread receives this on the
/// result channel and publishes the carried diagnostics.
#[derive(Debug)]
pub struct WorkerResult {
    /// The exact `WorkRequest.snapshot` the worker received.
    pub snapshot: Snapshot,
    /// The `(uri, version)` pairs covered by the original overlay,
    /// forwarded so the publish layer can stamp them onto each
    /// `PublishDiagnostics`.
    pub overlay_versions: BTreeMap<Uri, i32>,
    /// Package root the analysis ran against, so the main thread
    /// can resolve diagnostic paths to URIs.
    pub package_root: PathBuf,
    /// Full-package or focused-module analysis result.
    pub outcome: WorkerOutcome,
}

/// The worker can produce either a full diagnostic analysis or a
/// focused typed shard for a single file.
#[derive(Debug)]
pub enum WorkerOutcome {
    Full(Result<LspAnalysis, AnalysisFailure>),
    Focused {
        uri: Uri,
        file_path: PathBuf,
        outcome: Result<LspAnalysis, AnalysisFailure>,
    },
}

impl WorkerOutcome {
    #[cfg(test)]
    pub fn is_ok(&self) -> bool {
        match self {
            Self::Full(outcome) | Self::Focused { outcome, .. } => outcome.is_ok(),
        }
    }

    #[cfg(test)]
    pub fn is_err(&self) -> bool {
        match self {
            Self::Full(outcome) | Self::Focused { outcome, .. } => outcome.is_err(),
        }
    }

    fn timing_scope(&self) -> &'static str {
        match self {
            Self::Full(_) => "full",
            Self::Focused { .. } => "focused",
        }
    }

    fn timing_outcome(&self) -> (&'static str, usize) {
        let result = match self {
            Self::Full(outcome) | Self::Focused { outcome, .. } => outcome,
        };
        match result {
            Ok(_) => ("ok", 0),
            Err(failure) => ("error", failure.errors.len()),
        }
    }
}

/// Sentinel sent on the work channel to ask the worker to drain
/// any in-flight analysis and exit.
enum WorkMessage {
    Work(Box<WorkRequest>),
    Stop,
}

/// Sentinel sent on the scheduler input channel to ask the
/// debounce thread to exit.
enum ScheduleMessage {
    Pending(Box<WorkRequest>, Instant),
    CancelOlderOrEqual(SnapshotId),
    Stop,
}

/// Public entry point for the main thread: a [`Scheduler`] wraps the
/// debounce thread's input channel and exposes [`Scheduler::schedule`]
/// for the `didChange` handler. The matched output channel that
/// delivers [`WorkerResult`]s back to the main thread is returned
/// alongside it.
///
/// The two background threads (debounce and worker) start immediately;
/// they exit when [`Scheduler::shutdown`] is called.
pub struct Scheduler {
    /// Sender into the debounce thread's input channel.
    schedule_tx: Sender<ScheduleMessage>,
    /// Sender directly into the worker thread. Foreground requests use
    /// this to bypass the debounce window while also asking the
    /// debounce thread to drop older pending work.
    work_tx: Sender<WorkMessage>,
    /// Debounce interval [`Self::schedule`] stamps onto each pending
    /// request. The debounce thread reads it off the message rather
    /// than holding its own copy — that way tests configuring a
    /// shorter interval at construction get exactly that interval
    /// per-schedule.
    debounce: Duration,
    /// Handles for the two background threads, so [`Self::shutdown`]
    /// can join them.
    debounce_handle: Option<JoinHandle<()>>,
    worker_handle: Option<JoinHandle<()>>,
    /// Counter for snapshot ids, shared with the worker.
    id_gen: SnapshotIdGen,
    cancellations: CancellationRegistry,
}

#[derive(Clone, Default)]
struct CancellationRegistry {
    entries: Arc<Mutex<BTreeMap<SnapshotId, CancellationEntry>>>,
}

#[derive(Clone)]
struct CancellationEntry {
    token: CancellationToken,
    focus_file: Option<PathBuf>,
}

impl CancellationRegistry {
    fn register(&self, req: &WorkRequest) {
        let mut entries = self.entries.lock().expect("cancellation registry lock");
        entries.insert(
            req.snapshot.id(),
            CancellationEntry {
                token: req.cancel_token.clone(),
                focus_file: req.focus.as_ref().map(|focus| focus.file_path.clone()),
            },
        );
    }

    fn cancel_older_than(&self, snapshot_id: SnapshotId) {
        let mut entries = self.entries.lock().expect("cancellation registry lock");
        let mut cancelled = Vec::new();
        for (id, entry) in entries.iter() {
            if *id < snapshot_id {
                entry.token.cancel();
                cancelled.push(*id);
            }
        }
        for id in cancelled {
            entries.remove(&id);
        }
    }

    fn cancel_older_focused(&self, snapshot_id: SnapshotId, focus_file: &PathBuf) {
        let mut entries = self.entries.lock().expect("cancellation registry lock");
        let mut cancelled = Vec::new();
        for (id, entry) in entries.iter() {
            if *id < snapshot_id && entry.focus_file.as_ref() == Some(focus_file) {
                entry.token.cancel();
                cancelled.push(*id);
            }
        }
        for id in cancelled {
            entries.remove(&id);
        }
    }

    fn complete(&self, snapshot_id: SnapshotId) {
        self.entries
            .lock()
            .expect("cancellation registry lock")
            .remove(&snapshot_id);
    }
}

impl Scheduler {
    /// Spawn the debounce thread + worker thread. Returns the
    /// scheduler the main thread uses to post work, plus the
    /// receiver that delivers completed [`WorkerResult`]s.
    pub fn spawn() -> (Self, Receiver<WorkerResult>) {
        Self::spawn_with_debounce(DEBOUNCE)
    }

    /// As [`Self::spawn`], but with a configurable debounce interval.
    /// Tests use this to keep the debounce short (so unit / integration
    /// runs don't add 200ms per edit).
    pub fn spawn_with_debounce(debounce: Duration) -> (Self, Receiver<WorkerResult>) {
        let (schedule_tx, schedule_rx) = mpsc::unbounded::<ScheduleMessage>();
        let (work_tx, work_rx) = mpsc::unbounded::<WorkMessage>();
        let (result_tx, result_rx) = mpsc::unbounded::<WorkerResult>();
        let id_gen = SnapshotIdGen::new();
        let cancellations = CancellationRegistry::default();

        let debounce_handle = thread::Builder::new()
            .name("kio-lsp-debounce".to_owned())
            .spawn({
                let work_tx = work_tx.clone();
                move || run_debounce(schedule_rx, work_tx)
            })
            .expect("spawn debounce thread");
        let worker_handle = thread::Builder::new()
            .name("kio-lsp-worker".to_owned())
            .spawn({
                let cancellations = cancellations.clone();
                move || run_worker(work_rx, result_tx, cancellations)
            })
            .expect("spawn worker thread");

        (
            Self {
                schedule_tx,
                work_tx,
                debounce,
                debounce_handle: Some(debounce_handle),
                worker_handle: Some(worker_handle),
                id_gen,
                cancellations,
            },
            result_rx,
        )
    }

    /// Allocate the next snapshot id. The main thread allocates ids
    /// when scheduling rather than the worker, so the scheduler can
    /// tag each pending request with its eventual id — useful for
    /// future cancellation work (the unallocated request can drop
    /// without leaving a gap).
    pub fn next_snapshot_id(&self) -> SnapshotId {
        self.id_gen.next()
    }

    /// Post a `WorkRequest`. The debounce thread coalesces successive
    /// posts within the configured debounce window into a single
    /// worker dispatch; the most-recently-posted request wins (it
    /// carries the freshest overlay).
    pub fn schedule(&self, req: WorkRequest) {
        let deadline = Instant::now() + self.debounce;
        let snapshot_id = req.snapshot.id();
        self.cancellations.register(&req);
        self.cancellations.cancel_older_than(snapshot_id);
        // Send-failure here means the debounce thread has already
        // dropped its receiver — the LSP main loop is shutting
        // down or has shut down. Ignore: there's nothing to schedule
        // against and no consumer for the result.
        if self
            .schedule_tx
            .send(ScheduleMessage::Pending(Box::new(req), deadline))
            .is_err()
        {
            self.cancellations.complete(snapshot_id);
        }
    }

    /// Post a foreground request directly to the worker and cancel
    /// any debounced request that is no fresher than it.
    pub fn schedule_immediate(&self, req: WorkRequest) {
        self.schedule_immediate_inner(req, true);
    }

    /// Post a foreground focused request directly to the worker while
    /// keeping the debounced full diagnostics request alive.
    pub fn schedule_immediate_preserving_debounce(&self, req: WorkRequest) {
        self.schedule_immediate_inner(req, false);
    }

    fn schedule_immediate_inner(&self, req: WorkRequest, cancel_pending: bool) {
        let snapshot_id = req.snapshot.id();
        self.cancellations.register(&req);
        if cancel_pending {
            self.cancellations.cancel_older_than(snapshot_id);
            let _ = self
                .schedule_tx
                .send(ScheduleMessage::CancelOlderOrEqual(snapshot_id));
        } else if let Some(focus) = &req.focus {
            self.cancellations
                .cancel_older_focused(snapshot_id, &focus.file_path);
        }
        if self.work_tx.send(WorkMessage::Work(Box::new(req))).is_err() {
            self.cancellations.complete(snapshot_id);
        }
    }

    /// Drain the worker queue and join the background threads.
    /// Called from the LSP main loop's `shutdown` path.
    pub fn shutdown(&mut self) {
        // Send stop sentinels. The receivers may already be gone if
        // the thread crashed; ignore send errors.
        let _ = self.schedule_tx.send(ScheduleMessage::Stop);
        if let Some(h) = self.debounce_handle.take() {
            let _ = h.join();
        }
        if let Some(h) = self.worker_handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        // The main loop's shutdown path should call `Self::shutdown`
        // explicitly. This fallback covers the panic case so the
        // background threads don't leak.
        if self.debounce_handle.is_some() || self.worker_handle.is_some() {
            self.shutdown();
        }
    }
}

/// Debounce thread main loop. Receives `(request, deadline)` pairs;
/// each new pair replaces the pending one (typing coalesces). When
/// the current deadline elapses with no new pair, the held request
/// is forwarded to the worker.
fn run_debounce(schedule_rx: Receiver<ScheduleMessage>, work_tx: Sender<WorkMessage>) {
    let mut pending: Option<(Box<WorkRequest>, Instant)> = None;
    loop {
        let recv_result = match &pending {
            // Block forever until a message arrives.
            None => schedule_rx.recv().map_err(|_| ()),
            // Wait at most until the deadline.
            Some((_, deadline)) => {
                let now = Instant::now();
                if now >= *deadline {
                    // Deadline already elapsed — flush and continue.
                    let (req, _) = pending.take().expect("Some matched above");
                    if work_tx.send(WorkMessage::Work(req)).is_err() {
                        // Worker is gone; nothing more to do.
                        return;
                    }
                    continue;
                }
                match schedule_rx.recv_timeout(*deadline - now) {
                    Ok(msg) => Ok(msg),
                    Err(RecvTimeoutError::Timeout) => {
                        let (req, _) = pending.take().expect("Some matched above");
                        if work_tx.send(WorkMessage::Work(req)).is_err() {
                            return;
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => Err(()),
                }
            }
        };
        match recv_result {
            Ok(ScheduleMessage::Pending(req, deadline)) => {
                pending = Some((req, deadline));
            }
            Ok(ScheduleMessage::CancelOlderOrEqual(snapshot_id)) => {
                if pending
                    .as_ref()
                    .is_some_and(|(req, _)| req.snapshot.id() <= snapshot_id)
                {
                    pending = None;
                }
            }
            Ok(ScheduleMessage::Stop) => {
                // Drain any pending request — the worker should still
                // process the latest edit on shutdown so its result
                // gets a chance to publish.
                if let Some((req, _)) = pending.take() {
                    let _ = work_tx.send(WorkMessage::Work(req));
                }
                let _ = work_tx.send(WorkMessage::Stop);
                return;
            }
            Err(()) => {
                let _ = work_tx.send(WorkMessage::Stop);
                return;
            }
        }
    }
}

/// Worker thread main loop. Receives `WorkRequest`s, runs analysis,
/// posts results back. A `Stop` message exits the loop cleanly.
fn run_worker(
    work_rx: Receiver<WorkMessage>,
    result_tx: Sender<WorkerResult>,
    cancellations: CancellationRegistry,
) {
    let mut backlog = VecDeque::new();
    let user_elaborator_memos = LspUserElaboratorMemos::default();
    loop {
        let msg = match backlog.pop_front() {
            Some(msg) => Ok(msg),
            None => work_rx.recv(),
        };
        let Ok(msg) = msg else { return };
        match msg {
            WorkMessage::Work(req) => {
                let mut req = *req;
                let mut stop_after_result = false;
                while let Ok(next) = work_rx.try_recv() {
                    match next {
                        WorkMessage::Work(newer) => {
                            merge_or_defer_work(&mut req, *newer, &mut backlog, &cancellations);
                        }
                        WorkMessage::Stop => {
                            stop_after_result = true;
                            break;
                        }
                    }
                }
                let snapshot_id = req.snapshot.id();
                let result = run_one_with_memos(req, &user_elaborator_memos);
                cancellations.complete(snapshot_id);
                if let Some(result) = result
                    && result_tx.send(result).is_err()
                {
                    return;
                }
                if stop_after_result {
                    return;
                }
            }
            WorkMessage::Stop => return,
        }
    }
}

fn merge_or_defer_work(
    current: &mut WorkRequest,
    newer: WorkRequest,
    backlog: &mut VecDeque<WorkMessage>,
    cancellations: &CancellationRegistry,
) {
    if requests_substitute(current, &newer) {
        if newer.snapshot.id() > current.snapshot.id() {
            current.cancel_token.cancel();
            cancellations.complete(current.snapshot.id());
            *current = newer;
        }
        return;
    }
    if newer.priority == WorkPriority::Foreground && current.priority == WorkPriority::Background {
        let deferred = std::mem::replace(current, newer);
        backlog.push_back(WorkMessage::Work(Box::new(deferred)));
    } else {
        backlog.push_back(WorkMessage::Work(Box::new(newer)));
    }
}

fn requests_substitute(left: &WorkRequest, right: &WorkRequest) -> bool {
    match (&left.focus, &right.focus) {
        (None, None) => true,
        (Some(left), Some(right)) => left.file_path == right.file_path,
        _ => false,
    }
}

#[cfg(test)]
fn run_one(req: WorkRequest) -> Option<WorkerResult> {
    let user_elaborator_memos = LspUserElaboratorMemos::default();
    run_one_with_memos(req, &user_elaborator_memos)
}

/// Execute one [`WorkRequest`] against the overlay snapshot and wrap
/// the outcome into a [`WorkerResult`].
fn run_one_with_memos(
    req: WorkRequest,
    user_elaborator_memos: &LspUserElaboratorMemos,
) -> Option<WorkerResult> {
    let WorkRequest {
        package_root,
        overlay,
        snapshot,
        priority,
        focus,
        cancel_token,
    } = req;
    let snapshot_id = snapshot.id();

    // Use the LSP analysis variant: on success it captures the
    // PositionIndex so hover / goto-definition / references queries can
    // serve against it without re-running the typer.
    let start = Instant::now();
    if cancel_token.is_cancelled() {
        emit_cancelled_timing(snapshot_id, priority, focus.is_some(), &package_root, start);
        return None;
    }
    let outcome = match (priority, focus) {
        (WorkPriority::Foreground, Some(focus)) => {
            let Some(outcome) = analyze_module_at_with_overlay_lsp_cancellable_with_memos(
                &package_root,
                &overlay,
                &focus.file_path,
                &cancel_token,
                user_elaborator_memos,
            ) else {
                emit_cancelled_timing(snapshot_id, priority, true, &package_root, start);
                return None;
            };
            WorkerOutcome::Focused {
                outcome,
                uri: focus.uri,
                file_path: focus.file_path,
            }
        }
        _ => {
            let Some(outcome) = analyze_workspace_at_with_overlay_lsp_cancellable_with_memos(
                &package_root,
                &overlay,
                &cancel_token,
                user_elaborator_memos,
            ) else {
                emit_cancelled_timing(snapshot_id, priority, false, &package_root, start);
                return None;
            };
            WorkerOutcome::Full(outcome)
        }
    };
    if crate::timing::lsp_enabled() {
        let (outcome_label, error_count) = outcome.timing_outcome();
        eprintln!(
            "lsp-timing: analysis snapshot={} priority={} scope={} package={} outcome={} errors={} total_ms={:.3}",
            snapshot_id.raw(),
            priority.as_str(),
            outcome.timing_scope(),
            package_root.display(),
            outcome_label,
            error_count,
            start.elapsed().as_secs_f64() * 1000.0,
        );
    }

    Some(WorkerResult {
        overlay_versions: snapshot.versions().clone(),
        snapshot,
        package_root,
        outcome,
    })
}

fn emit_cancelled_timing(
    snapshot_id: SnapshotId,
    priority: WorkPriority,
    focused: bool,
    package_root: &std::path::Path,
    start: Instant,
) {
    if crate::timing::lsp_enabled() {
        eprintln!(
            "lsp-timing: analysis snapshot={} priority={} scope={} package={} outcome=cancelled errors=0 total_ms={:.3}",
            snapshot_id.raw(),
            priority.as_str(),
            if focused { "focused" } else { "full" },
            package_root.display(),
            start.elapsed().as_secs_f64() * 1000.0,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Per-test temp directory removed on drop. Mirrors the shape used
    /// by lsp_smoke.rs.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir()
                .join(format!("kio-lsp-worker-{}-{tag}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create temp dir");
            TempDir(dir)
        }

        fn path(&self) -> &PathBuf {
            &self.0
        }

        fn write(&self, rel: &str, content: &str) -> PathBuf {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).expect("create parent dir");
            }
            fs::write(&p, content).expect("write file");
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn uri(s: &str) -> Uri {
        Uri::from_str(s).expect("uri")
    }

    /// Receive a `WorkerResult` with a bounded wait, panicking on
    /// timeout so tests fail loudly rather than hang.
    fn recv_with_timeout(rx: &Receiver<WorkerResult>, timeout: Duration) -> WorkerResult {
        rx.recv_timeout(timeout)
            .expect("worker result within timeout")
    }

    #[test]
    fn worker_runs_one_analysis_against_overlay() {
        let dir = TempDir::new("one");
        let main_path = dir.write("main.kio", "module main;\n\npub fn run() -> . { () }\n");
        dir.write("pkg.pkg.kio", "package pkg;\n\nbridge { main; }\n");
        let canonical_main = fs::canonicalize(&main_path).expect("canon");
        let canonical_root = fs::canonicalize(dir.path()).expect("canon");

        let (mut scheduler, result_rx) = Scheduler::spawn_with_debounce(Duration::from_millis(10));
        let mut overlay = SourceOverlay::empty();
        overlay.insert(
            canonical_main.clone(),
            "module main;\n\npub fn run() -> . { () }\n".to_owned(),
        );
        let mut overlay_versions = BTreeMap::new();
        overlay_versions.insert(uri("file:///x.kio"), 5);

        let id = scheduler.next_snapshot_id();
        scheduler.schedule(WorkRequest {
            package_root: canonical_root,
            overlay,
            snapshot: Snapshot::new(id, overlay_versions.clone()).with_source_paths(
                BTreeMap::from([(uri("file:///x.kio"), canonical_main.clone())]),
            ),
            priority: WorkPriority::Background,
            focus: None,
            cancel_token: CancellationToken::new(),
        });

        let result = recv_with_timeout(&result_rx, Duration::from_secs(15));
        assert_eq!(result.snapshot.id(), id);
        assert!(result.outcome.is_ok(), "clean overlay should typecheck");
        assert_eq!(result.snapshot.versions(), &overlay_versions);
        assert_eq!(
            result.snapshot.source_path(&uri("file:///x.kio")),
            Some(canonical_main.as_path())
        );

        scheduler.shutdown();
    }

    #[test]
    fn worker_surfaces_type_error_against_overlay() {
        let dir = TempDir::new("type-err");
        let main_path = dir.write("main.kio", "module main;\n\npub fn run() -> . { 1 }\n");
        dir.write("pkg.pkg.kio", "package pkg;\n\nbridge { main; }\n");
        let canonical_main = fs::canonicalize(&main_path).expect("canon");
        let canonical_root = fs::canonicalize(dir.path()).expect("canon");

        let (mut scheduler, result_rx) = Scheduler::spawn_with_debounce(Duration::from_millis(10));
        let mut overlay = SourceOverlay::empty();
        // Overlay reuses the on-disk text; the type error is real.
        overlay.insert(
            canonical_main.clone(),
            "module main;\n\npub fn run() -> . { 1 }\n".to_owned(),
        );

        let id = scheduler.next_snapshot_id();
        scheduler.schedule(WorkRequest {
            package_root: canonical_root,
            overlay,
            snapshot: Snapshot::new(id, BTreeMap::new()),
            priority: WorkPriority::Background,
            focus: None,
            cancel_token: CancellationToken::new(),
        });

        let result = recv_with_timeout(&result_rx, Duration::from_secs(15));
        assert!(result.outcome.is_err(), "type error must surface");

        scheduler.shutdown();
    }

    #[test]
    fn debounce_coalesces_rapid_schedules() {
        let dir = TempDir::new("debounce");
        let main_path = dir.write("main.kio", "module main;\n\npub fn run() -> . { () }\n");
        dir.write("pkg.pkg.kio", "package pkg;\n\nbridge { main; }\n");
        let canonical_main = fs::canonicalize(&main_path).expect("canon");
        let canonical_root = fs::canonicalize(dir.path()).expect("canon");

        // Generous debounce so the burst stays inside it.
        let (mut scheduler, result_rx) = Scheduler::spawn_with_debounce(Duration::from_millis(150));

        for _ in 0..5 {
            let mut overlay = SourceOverlay::empty();
            overlay.insert(
                canonical_main.clone(),
                "module main;\n\npub fn run() -> . { () }\n".to_owned(),
            );
            let id = scheduler.next_snapshot_id();
            scheduler.schedule(WorkRequest {
                package_root: canonical_root.clone(),
                overlay,
                snapshot: Snapshot::new(id, BTreeMap::new()),
                priority: WorkPriority::Background,
                focus: None,
                cancel_token: CancellationToken::new(),
            });
            // Tight back-to-back posts; the debounce thread should
            // collapse them.
            std::thread::sleep(Duration::from_millis(10));
        }

        // Exactly one result should arrive after the debounce window.
        let result = recv_with_timeout(&result_rx, Duration::from_secs(15));
        assert!(result.outcome.is_ok());
        // No second result should follow within a short grace period
        // (give the worker time to publish a hypothetical extra).
        let extra = result_rx.recv_timeout(Duration::from_millis(300));
        assert!(
            extra.is_err(),
            "debounce should produce exactly one result for a burst; got an extra: {extra:?}",
        );

        scheduler.shutdown();
    }

    #[test]
    fn worker_drops_obsolete_queued_work_before_running() {
        let dir = TempDir::new("worker-queue");
        let main_path = dir.write("main.kio", "module main;\n\npub fn run() -> . { () }\n");
        dir.write("pkg.pkg.kio", "package pkg;\n\nbridge { main; }\n");
        let canonical_main = fs::canonicalize(&main_path).expect("canon");
        let canonical_root = fs::canonicalize(dir.path()).expect("canon");

        let (work_tx, work_rx) = mpsc::unbounded::<WorkMessage>();
        let (result_tx, result_rx) = mpsc::unbounded::<WorkerResult>();
        let ids = SnapshotIdGen::new();
        let mut expected_latest = None;
        for version in 1..=5 {
            let mut overlay = SourceOverlay::empty();
            overlay.insert(
                canonical_main.clone(),
                "module main;\n\npub fn run() -> . { () }\n".to_owned(),
            );
            let id = ids.next();
            expected_latest = Some(id);
            let mut overlay_versions = BTreeMap::new();
            overlay_versions.insert(uri("file:///tmp/main.kio"), version);
            work_tx
                .send(WorkMessage::Work(Box::new(WorkRequest {
                    package_root: canonical_root.clone(),
                    overlay,
                    snapshot: Snapshot::new(id, overlay_versions),
                    priority: WorkPriority::Background,
                    focus: None,
                    cancel_token: CancellationToken::new(),
                })))
                .expect("send work");
        }
        work_tx.send(WorkMessage::Stop).expect("send stop");

        let handle =
            thread::spawn(move || run_worker(work_rx, result_tx, CancellationRegistry::default()));
        let result = recv_with_timeout(&result_rx, Duration::from_secs(15));
        assert_eq!(result.snapshot.id(), expected_latest.expect("latest id"));
        assert!(result.outcome.is_ok(), "latest queued work should run");
        assert!(
            result_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "worker should skip older queued jobs and emit one result"
        );
        handle.join().expect("worker thread exits");
    }

    #[test]
    fn worker_prefers_highest_snapshot_id_not_receive_order() {
        let dir = TempDir::new("worker-freshness-order");
        let main_path = dir.write("main.kio", "module main;\n\npub fn run() -> . { () }\n");
        dir.write("pkg.pkg.kio", "package pkg;\n\nbridge { main; }\n");
        let canonical_main = fs::canonicalize(&main_path).expect("canon");
        let canonical_root = fs::canonicalize(dir.path()).expect("canon");

        let (work_tx, work_rx) = mpsc::unbounded::<WorkMessage>();
        let (result_tx, result_rx) = mpsc::unbounded::<WorkerResult>();
        let ids = SnapshotIdGen::new();
        let stale = ids.next();
        let fresh = ids.next();

        for (id, version, priority) in [
            (fresh, 2, WorkPriority::Foreground),
            (stale, 1, WorkPriority::Background),
        ] {
            let mut overlay = SourceOverlay::empty();
            overlay.insert(
                canonical_main.clone(),
                "module main;\n\npub fn run() -> . { () }\n".to_owned(),
            );
            let mut overlay_versions = BTreeMap::new();
            overlay_versions.insert(uri("file:///tmp/main.kio"), version);
            work_tx
                .send(WorkMessage::Work(Box::new(WorkRequest {
                    package_root: canonical_root.clone(),
                    overlay,
                    snapshot: Snapshot::new(id, overlay_versions),
                    priority,
                    focus: None,
                    cancel_token: CancellationToken::new(),
                })))
                .expect("send work");
        }
        work_tx.send(WorkMessage::Stop).expect("send stop");

        let handle =
            thread::spawn(move || run_worker(work_rx, result_tx, CancellationRegistry::default()));
        let result = recv_with_timeout(&result_rx, Duration::from_secs(15));
        assert_eq!(result.snapshot.id(), fresh);
        assert_eq!(
            result.snapshot.version(&uri("file:///tmp/main.kio")),
            Some(2)
        );
        assert!(result.outcome.is_ok(), "fresh queued work should run");
        assert!(
            result_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "worker should skip lower-id queued jobs even when they arrive later"
        );
        handle.join().expect("worker thread exits");
    }

    #[test]
    fn scheduling_new_background_work_cancels_older_tokens() {
        let dir = TempDir::new("cancel-background");
        let main_path = dir.write("main.kio", "module main;\n\npub fn run() -> . { () }\n");
        dir.write("pkg.pkg.kio", "package pkg;\n\nbridge { main; }\n");
        let canonical_main = fs::canonicalize(&main_path).expect("canon");
        let canonical_root = fs::canonicalize(dir.path()).expect("canon");

        let (mut scheduler, _result_rx) = Scheduler::spawn_with_debounce(Duration::from_secs(10));

        let old_token = CancellationToken::new();
        let old_id = scheduler.next_snapshot_id();
        let mut old_overlay = SourceOverlay::empty();
        old_overlay.insert(
            canonical_main.clone(),
            "module main;\n\npub fn run() -> . { () }\n".to_owned(),
        );
        scheduler.schedule(WorkRequest {
            package_root: canonical_root.clone(),
            overlay: old_overlay,
            snapshot: Snapshot::new(old_id, BTreeMap::from([(uri("file:///tmp/main.kio"), 1)])),
            priority: WorkPriority::Background,
            focus: None,
            cancel_token: old_token.clone(),
        });

        let new_token = CancellationToken::new();
        let new_id = scheduler.next_snapshot_id();
        let mut new_overlay = SourceOverlay::empty();
        new_overlay.insert(
            canonical_main,
            "module main;\n\npub fn run() -> . { () }\n".to_owned(),
        );
        scheduler.schedule(WorkRequest {
            package_root: canonical_root,
            overlay: new_overlay,
            snapshot: Snapshot::new(new_id, BTreeMap::from([(uri("file:///tmp/main.kio"), 2)])),
            priority: WorkPriority::Background,
            focus: None,
            cancel_token: new_token.clone(),
        });

        assert!(old_token.is_cancelled());
        assert!(!new_token.is_cancelled());

        scheduler.shutdown();
    }

    #[test]
    fn focused_foreground_preserves_full_diagnostics_token() {
        let dir = TempDir::new("focused-preserves-token");
        let main_path = dir.write("main.kio", "module main;\n\npub fn run() -> . { () }\n");
        dir.write("pkg.pkg.kio", "package pkg;\n\nbridge { main; }\n");
        let canonical_main = fs::canonicalize(&main_path).expect("canon");
        let canonical_root = fs::canonicalize(dir.path()).expect("canon");

        let (mut scheduler, _result_rx) = Scheduler::spawn_with_debounce(Duration::from_secs(10));

        let full_token = CancellationToken::new();
        let mut full_overlay = SourceOverlay::empty();
        full_overlay.insert(
            canonical_main.clone(),
            "module main;\n\npub fn run() -> . { () }\n".to_owned(),
        );
        scheduler.schedule(WorkRequest {
            package_root: canonical_root.clone(),
            overlay: full_overlay,
            snapshot: Snapshot::new(
                scheduler.next_snapshot_id(),
                BTreeMap::from([(uri("file:///tmp/main.kio"), 1)]),
            ),
            priority: WorkPriority::Background,
            focus: None,
            cancel_token: full_token.clone(),
        });

        let focused_token = CancellationToken::new();
        let mut focused_overlay = SourceOverlay::empty();
        focused_overlay.insert(
            canonical_main.clone(),
            "module main;\n\npub fn run() -> . { () }\n".to_owned(),
        );
        scheduler.schedule_immediate_preserving_debounce(WorkRequest {
            package_root: canonical_root,
            overlay: focused_overlay,
            snapshot: Snapshot::new(
                scheduler.next_snapshot_id(),
                BTreeMap::from([(uri("file:///tmp/main.kio"), 1)]),
            ),
            priority: WorkPriority::Foreground,
            focus: Some(FocusedAnalysis {
                uri: uri("file:///tmp/main.kio"),
                file_path: canonical_main,
            }),
            cancel_token: focused_token.clone(),
        });

        assert!(!full_token.is_cancelled());
        assert!(!focused_token.is_cancelled());

        scheduler.shutdown();
    }

    #[test]
    fn worker_emits_no_result_for_cancelled_request() {
        let token = CancellationToken::new();
        token.cancel();
        let result = run_one(WorkRequest {
            package_root: PathBuf::from("/tmp/kio-lsp-cancelled"),
            overlay: SourceOverlay::empty(),
            snapshot: Snapshot::new(SnapshotIdGen::new().next(), BTreeMap::new()),
            priority: WorkPriority::Background,
            focus: None,
            cancel_token: token,
        });

        assert!(result.is_none());
    }

    #[test]
    fn foreground_schedule_bypasses_and_cancels_debounce() {
        let dir = TempDir::new("foreground");
        let main_path = dir.write("main.kio", "module main;\n\npub fn run() -> . { () }\n");
        dir.write("pkg.pkg.kio", "package pkg;\n\nbridge { main; }\n");
        let canonical_main = fs::canonicalize(&main_path).expect("canon");
        let canonical_root = fs::canonicalize(dir.path()).expect("canon");

        let (mut scheduler, result_rx) = Scheduler::spawn_with_debounce(Duration::from_secs(10));

        let background_id = scheduler.next_snapshot_id();
        let mut background_overlay = SourceOverlay::empty();
        background_overlay.insert(
            canonical_main.clone(),
            "module main;\n\npub fn run() -> . { () }\n".to_owned(),
        );
        scheduler.schedule(WorkRequest {
            package_root: canonical_root.clone(),
            overlay: background_overlay,
            snapshot: Snapshot::new(
                background_id,
                BTreeMap::from([(uri("file:///tmp/main.kio"), 1)]),
            ),
            priority: WorkPriority::Background,
            focus: None,
            cancel_token: CancellationToken::new(),
        });

        let foreground_id = scheduler.next_snapshot_id();
        let mut foreground_overlay = SourceOverlay::empty();
        foreground_overlay.insert(
            canonical_main,
            "module main;\n\npub fn run() -> . { () }\n".to_owned(),
        );
        scheduler.schedule_immediate(WorkRequest {
            package_root: canonical_root,
            overlay: foreground_overlay,
            snapshot: Snapshot::new(
                foreground_id,
                BTreeMap::from([(uri("file:///tmp/main.kio"), 2)]),
            ),
            priority: WorkPriority::Foreground,
            focus: None,
            cancel_token: CancellationToken::new(),
        });

        let result = recv_with_timeout(&result_rx, Duration::from_secs(15));
        assert_eq!(result.snapshot.id(), foreground_id);
        assert_eq!(
            result.snapshot.version(&uri("file:///tmp/main.kio")),
            Some(2)
        );
        assert!(result.outcome.is_ok(), "foreground analysis should run");
        assert!(
            result_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "foreground schedule should cancel the older debounced request"
        );

        scheduler.shutdown();
        assert!(
            result_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "shutdown must not drain an older debounced request after a foreground schedule"
        );
    }

    #[test]
    fn foreground_focus_typechecks_one_module_despite_sibling_body_error() {
        let dir = TempDir::new("foreground-focus");
        let main_source = "module pkg/main;\n\npub fn run() -> . { () }\n";
        dir.write("pkg.kio", "module pkg;\n");
        let main_path = dir.write("pkg/main.kio", main_source);
        dir.write(
            "pkg/bad.kio",
            "module pkg/bad;\n\npub fn bad() -> . { 1 }\n",
        );
        dir.write("pkg.pkg.kio", "package pkg;\n\nbridge { pkg; }\n");
        let canonical_main = fs::canonicalize(&main_path).expect("canon main");
        let canonical_root = fs::canonicalize(dir.path()).expect("canon root");
        let focus_uri = uri("file:///tmp/main.kio");

        let mut overlay = SourceOverlay::empty();
        overlay.insert(canonical_main.clone(), main_source.to_owned());
        let result = run_one(WorkRequest {
            package_root: canonical_root,
            overlay,
            snapshot: Snapshot::new(
                SnapshotIdGen::new().next(),
                BTreeMap::from([(focus_uri.clone(), 1)]),
            ),
            priority: WorkPriority::Foreground,
            focus: Some(FocusedAnalysis {
                uri: focus_uri.clone(),
                file_path: canonical_main.clone(),
            }),
            cancel_token: CancellationToken::new(),
        });

        let result = result.expect("focused run should produce a result");
        match result.outcome {
            WorkerOutcome::Focused {
                uri,
                file_path,
                outcome: Ok(analysis),
            } => {
                assert_eq!(uri, focus_uri);
                assert_eq!(file_path, canonical_main);
                assert!(
                    analysis.position_index.type_count() > 0,
                    "focused analysis should capture typed positions"
                );
            }
            other => panic!("expected focused success, got {other:?}"),
        }
    }

    #[test]
    fn focused_foreground_does_not_displace_full_diagnostics_work() {
        let dir = TempDir::new("focused-keeps-full");
        let main_source = "module pkg/main;\n\npub fn run() -> . { () }\n";
        dir.write("pkg.kio", "module pkg;\n");
        let main_path = dir.write("pkg/main.kio", main_source);
        dir.write(
            "pkg/bad.kio",
            "module pkg/bad;\n\npub fn bad() -> . { 1 }\n",
        );
        dir.write("pkg.pkg.kio", "package pkg;\n\nbridge { pkg; }\n");
        let canonical_main = fs::canonicalize(&main_path).expect("canon main");
        let canonical_root = fs::canonicalize(dir.path()).expect("canon root");
        let focus_uri = uri("file:///tmp/main.kio");
        let ids = SnapshotIdGen::new();

        let (work_tx, work_rx) = mpsc::unbounded::<WorkMessage>();
        let (result_tx, result_rx) = mpsc::unbounded::<WorkerResult>();
        for (priority, focus) in [
            (WorkPriority::Background, None),
            (
                WorkPriority::Foreground,
                Some(FocusedAnalysis {
                    uri: focus_uri.clone(),
                    file_path: canonical_main.clone(),
                }),
            ),
        ] {
            let mut overlay = SourceOverlay::empty();
            overlay.insert(canonical_main.clone(), main_source.to_owned());
            work_tx
                .send(WorkMessage::Work(Box::new(WorkRequest {
                    package_root: canonical_root.clone(),
                    overlay,
                    snapshot: Snapshot::new(ids.next(), BTreeMap::from([(focus_uri.clone(), 1)])),
                    priority,
                    focus,
                    cancel_token: CancellationToken::new(),
                })))
                .expect("send work");
        }

        let handle =
            thread::spawn(move || run_worker(work_rx, result_tx, CancellationRegistry::default()));
        let first = recv_with_timeout(&result_rx, Duration::from_secs(15));
        assert!(
            matches!(
                &first.outcome,
                WorkerOutcome::Focused { outcome: Ok(_), .. }
            ),
            "foreground focused work should run first, got {first:?}"
        );
        let second = recv_with_timeout(&result_rx, Duration::from_secs(15));
        assert!(
            matches!(&second.outcome, WorkerOutcome::Full(Err(_))),
            "full diagnostics work should still run, got {second:?}"
        );

        work_tx.send(WorkMessage::Stop).expect("send stop");
        handle.join().expect("worker thread exits");
    }

    #[test]
    fn shutdown_drains_pending_then_exits() {
        let dir = TempDir::new("shutdown");
        let main_path = dir.write("main.kio", "module main;\n\npub fn run() -> . { () }\n");
        dir.write("pkg.pkg.kio", "package pkg;\n\nbridge { main; }\n");
        let canonical_main = fs::canonicalize(&main_path).expect("canon");
        let canonical_root = fs::canonicalize(dir.path()).expect("canon");

        let (mut scheduler, result_rx) = Scheduler::spawn_with_debounce(Duration::from_secs(10));
        // Schedule one request with a long debounce, then immediately
        // shut down. The pending request should still get processed
        // before the worker exits.
        let mut overlay = SourceOverlay::empty();
        overlay.insert(
            canonical_main.clone(),
            "module main;\n\npub fn run() -> . { () }\n".to_owned(),
        );
        let id = scheduler.next_snapshot_id();
        scheduler.schedule(WorkRequest {
            package_root: canonical_root,
            overlay,
            snapshot: Snapshot::new(id, BTreeMap::new()),
            priority: WorkPriority::Background,
            focus: None,
            cancel_token: CancellationToken::new(),
        });

        scheduler.shutdown();
        // After shutdown, the pending result should still arrive.
        let result = result_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("pending analysis should drain on shutdown");
        assert_eq!(result.snapshot.id(), id);
    }

    #[test]
    fn ids_are_monotonically_increasing() {
        let (mut scheduler, _result_rx) = Scheduler::spawn_with_debounce(Duration::from_millis(10));
        let a = scheduler.next_snapshot_id();
        let b = scheduler.next_snapshot_id();
        let c = scheduler.next_snapshot_id();
        assert!(a < b);
        assert!(b < c);
        scheduler.shutdown();
    }
}
