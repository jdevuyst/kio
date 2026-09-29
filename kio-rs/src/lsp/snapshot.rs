//! Snapshot identity and freshness queries.
//!
//! Each analysis the worker thread runs is tagged with a [`Snapshot`]:
//! a monotonic [`SnapshotId`] plus the per-URI document versions the
//! analysis saw. The main thread stores the most-recent snapshot per
//! package and uses it to:
//!
//! 1. Decide whether to publish a worker result. A snapshot that's
//!    older than what's already published (the main thread received
//!    them out of order) gets dropped.
//! 2. Serve read-only LSP requests such as hover, goto-definition,
//!    and document symbols. Those requests ask "is your latest
//!    snapshot ≥ document version V?" and accept a slightly-stale
//!    answer if not — blocking on a fresh analysis per keystroke
//!    would add latency the LSP can't afford.
//!
//! `SnapshotId` is a monotonic counter (`u64` — practically unbounded).
//! It's allocated by [`SnapshotIdGen`], a thread-safe counter the main
//! thread and the worker share to ensure id allocation is the unique
//! source of ordering. The id alone disambiguates two snapshots; the
//! per-URI version map is what answers "does this cover document V?"
//! queries.

// `lsp_types::Uri` carries a `Cell` for internal parse-result caching,
// which trips clippy's `mutable_key_type` lint when used as a map / set
// key. See `state.rs` for the equivalent suppression and rationale.
#![allow(clippy::mutable_key_type)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lsp_types::Uri;

/// Identity tag for a single completed analysis. Allocated from
/// [`SnapshotIdGen`] in monotonically-increasing order so the main
/// thread can spot out-of-order worker results and discard a stale
/// one if a newer snapshot has already been published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SnapshotId(u64);

impl SnapshotId {
    /// Returns the raw counter value. Exposed for diagnostics and
    /// debug-trace output; not for ordering decisions (use `Ord`).
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// Thread-safe monotonic id generator shared between the main thread
/// (queueing analyses) and the worker thread (tagging completed
/// snapshots). Implemented as an atomic counter; cloning the generator
/// shares the counter (it's an `Arc<AtomicU64>` inside).
#[derive(Debug, Clone)]
pub struct SnapshotIdGen {
    counter: Arc<AtomicU64>,
}

impl SnapshotIdGen {
    /// Construct a fresh id generator. The first allocated id is `0`.
    pub fn new() -> Self {
        Self {
            counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Allocate the next id. Monotonic across threads thanks to
    /// `Ordering::Relaxed` being sufficient for a counter that never
    /// goes backwards.
    pub fn next(&self) -> SnapshotId {
        SnapshotId(self.counter.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for SnapshotIdGen {
    fn default() -> Self {
        Self::new()
    }
}

/// A completed analysis snapshot. Carries:
///
/// - The [`SnapshotId`] allocated when the analysis was scheduled.
///   Ordering on `SnapshotId` is the canonical happens-before relation
///   between snapshots.
/// - The per-URI document versions the analysis saw. A document
///   absent from `versions` was read from disk (no overlay was open).
///   A reader asking "is this snapshot ≥ version V of URI U?" checks
///   `versions.get(U) >= Some(V)`.
///
/// The analysis payload (diagnostics, type tables, … — anything the
/// LSP serves from this snapshot) is stored by the LSP state alongside
/// this identity tag. This file owns only the identity and freshness
/// contract.
#[derive(Debug, Clone)]
pub struct Snapshot {
    id: SnapshotId,
    /// Per-URI document version the analysis observed. Only URIs that
    /// had an overlay open at scheduling time appear here; disk-source
    /// URIs are absent.
    versions: BTreeMap<Uri, i32>,
    /// Canonical overlay identities captured alongside versions at scheduling.
    /// Re-canonicalizing a URI later cannot recover a retargeted symlink's
    /// analyzed identity.
    source_paths: BTreeMap<Uri, PathBuf>,
}

impl Snapshot {
    /// Construct a snapshot tag for an analysis that ran against
    /// `versions`.
    pub fn new(id: SnapshotId, versions: BTreeMap<Uri, i32>) -> Self {
        Self {
            id,
            versions,
            source_paths: BTreeMap::new(),
        }
    }

    pub(crate) fn with_source_paths(mut self, source_paths: BTreeMap<Uri, PathBuf>) -> Self {
        assert!(
            source_paths
                .keys()
                .all(|uri| self.versions.contains_key(uri)),
            "captured source paths require matching overlay versions"
        );
        self.source_paths = source_paths;
        self
    }

    pub(crate) fn source_path(&self, uri: &Uri) -> Option<&Path> {
        self.source_paths.get(uri).map(PathBuf::as_path)
    }

    /// The allocated id. Used for ordering against other snapshots
    /// (later ids strictly happen after earlier ids).
    pub fn id(&self) -> SnapshotId {
        self.id
    }

    /// The version the snapshot saw for `uri`, if it had an overlay
    /// open. `None` means the analysis read this URI from disk (or
    /// the URI wasn't part of the analyzed package).
    pub fn version(&self, uri: &Uri) -> Option<i32> {
        self.versions.get(uri).copied()
    }

    /// Whether this snapshot's view of `uri` is at least as fresh as
    /// `requested_version`. Returns `true` when the snapshot saw a
    /// version `>= requested_version`; `false` when it saw an older
    /// version *or* didn't see the URI at all.
    ///
    /// Read-only LSP query handlers use this to decide whether to
    /// serve the stored snapshot or wait for a fresher one.
    pub fn covers(&self, uri: &Uri, requested_version: i32) -> bool {
        self.versions
            .get(uri)
            .is_some_and(|&v| v >= requested_version)
    }

    /// All `(URI, version)` pairs the snapshot saw. Used by the
    /// publish layer to set `PublishDiagnostics.version` correctly.
    pub fn versions(&self) -> &BTreeMap<Uri, i32> {
        &self.versions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn uri(s: &str) -> Uri {
        Uri::from_str(s).expect("uri")
    }

    #[test]
    fn snapshot_id_gen_allocates_monotonically() {
        let g = SnapshotIdGen::new();
        let a = g.next();
        let b = g.next();
        let c = g.next();
        assert!(a < b);
        assert!(b < c);
        assert_eq!(a.raw(), 0);
        assert_eq!(b.raw(), 1);
        assert_eq!(c.raw(), 2);
    }

    #[test]
    fn snapshot_id_gen_shared_across_clones() {
        // The generator is `Arc<AtomicU64>` under the hood, so clones
        // share the counter.
        let a = SnapshotIdGen::new();
        let b = a.clone();
        assert_eq!(a.next().raw(), 0);
        assert_eq!(b.next().raw(), 1); // b sees the post-a counter
        assert_eq!(a.next().raw(), 2);
    }

    #[test]
    fn snapshot_covers_when_version_matches_or_newer() {
        let g = SnapshotIdGen::new();
        let u = uri("file:///tmp/a.kio");
        let mut versions = BTreeMap::new();
        versions.insert(u.clone(), 5);
        let snap = Snapshot::new(g.next(), versions);
        assert!(snap.covers(&u, 5)); // exact match
        assert!(snap.covers(&u, 4)); // newer than requested
        assert!(!snap.covers(&u, 6)); // older than requested
    }

    #[test]
    fn snapshot_does_not_cover_uri_not_in_snapshot() {
        let g = SnapshotIdGen::new();
        let u = uri("file:///tmp/a.kio");
        let other = uri("file:///tmp/b.kio");
        let mut versions = BTreeMap::new();
        versions.insert(u.clone(), 3);
        let snap = Snapshot::new(g.next(), versions);
        assert!(!snap.covers(&other, 1));
    }

    #[test]
    fn snapshot_version_returns_seen_value() {
        let g = SnapshotIdGen::new();
        let u = uri("file:///tmp/a.kio");
        let mut versions = BTreeMap::new();
        versions.insert(u.clone(), 42);
        let snap = Snapshot::new(g.next(), versions);
        assert_eq!(snap.version(&u), Some(42));
        assert_eq!(snap.version(&uri("file:///tmp/missing.kio")), None);
    }

    #[test]
    fn snapshot_id_total_order() {
        let g = SnapshotIdGen::new();
        let s1 = Snapshot::new(g.next(), BTreeMap::new());
        let s2 = Snapshot::new(g.next(), BTreeMap::new());
        assert!(s1.id() < s2.id());
    }
}
