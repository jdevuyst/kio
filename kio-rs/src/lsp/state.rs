//! Per-server state for the live LSP: the open-document overlay store
//! and bookkeeping for published diagnostics.
//!
//! The overlay store holds one [`OpenDocument`] per URI the editor has
//! sent through `textDocument/didOpen`. Each entry carries:
//!
//! - the current full text (string the editor reports — authoritative
//!   over the disk copy for as long as the document is open),
//! - the client-supplied monotonic version number,
//! - a lazily-built [`LineIndex`] used for position ↔ byte-offset
//!   conversion. Edits invalidate the index; the next read rebuilds it.
//!
//! The store is the *truth*: once an editor opens a document, the
//! analysis pipeline must read its source from the overlay (not from
//! disk) for as long as it stays open. `didClose` removes the entry,
//! at which point disk becomes authoritative again.

// `lsp_types::Uri` carries a `Cell` for internal parse-result caching,
// which trips clippy's `mutable_key_type` lint when used as a map / set
// key. The cell doesn't affect `Eq` / `Hash` / `Ord` (those compare the
// URI's string form), so the lint is a false-positive for our usage —
// both the URI's clients and the upstream `lsp-types` crate use `Uri`
// in maps and sets.
#![allow(clippy::mutable_key_type)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lsp_types::{TextDocumentContentChangeEvent, Uri};

use crate::ast::{Module, Surface};
use crate::cmd::check::LspAnalysis;
use crate::error::Error;
use crate::lsp::label_reuse::LabelReuseIndex;
use crate::lsp::positions::{LineIndex, LspPosition};
use crate::lsp::snapshot::{Snapshot, SnapshotId};
use crate::lsp::util::uri_to_canonical;
use crate::package_collection::{ModuleFileParserContext, SourceOverlay};
use crate::pass::parser::LazyModule;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParseContextKey {
    Source,
    File(ModuleFileParserContext),
}

/// One open document. Holds the authoritative overlay text the editor
/// last reported, the version number the client tagged it with, and a
/// lazily-built [`LineIndex`] over the current text.
///
/// The line index is built on demand. After [`Self::set_text`] or
/// [`Self::splice`] mutates the text, the cached index drops; the next
/// [`Self::line_index`] call rebuilds it. Reads against an unmodified
/// document hit the cache.
#[derive(Debug)]
pub struct OpenDocument {
    text: String,
    /// Client-supplied document version. Each `didChange` notification
    /// carries a new version that monotonically increases per URI.
    /// `didOpen` initializes it.
    version: i32,
    /// Lazily-built index over `text`. `None` after a mutation; the
    /// next [`Self::line_index`] call populates it.
    line_index: Option<LineIndex>,
    /// Lazily-built surface parse over `text`. `None` after a mutation;
    /// the next syntax-level request populates it.
    parsed_module: Option<(ParseContextKey, Result<Module<Surface>, Error>)>,
    /// Label declarations and reuse markers derived from `parsed_module`.
    /// The nested option caches syntactically broken text as well as a
    /// successful index.
    label_reuse_index: Option<Option<Arc<LabelReuseIndex>>>,
    /// Lazily-built header/body-thunk parse over `text`. Used by
    /// syntax-level requests that do not need function bodies.
    lazy_module: Option<Result<LazyModule, Error>>,
    #[cfg(test)]
    parse_count: usize,
    #[cfg(test)]
    file_context_materialization_count: usize,
}

impl OpenDocument {
    /// Construct an open-document entry from a `didOpen` event.
    pub fn new(text: String, version: i32) -> Self {
        Self {
            text,
            version,
            line_index: None,
            parsed_module: None,
            label_reuse_index: None,
            lazy_module: None,
            #[cfg(test)]
            parse_count: 0,
            #[cfg(test)]
            file_context_materialization_count: 0,
        }
    }

    /// The current overlay text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The latest client-supplied version number.
    pub fn version(&self) -> i32 {
        self.version
    }

    /// Borrow the line index, building it lazily the first time after
    /// a mutation. The returned reference is invalidated by the next
    /// [`Self::set_text`] / [`Self::splice`] call.
    pub fn line_index(&mut self) -> &LineIndex {
        if self.line_index.is_none() {
            self.line_index = Some(LineIndex::new(&self.text));
        }
        self.line_index.as_ref().expect("just populated above")
    }

    /// Borrow a parsed surface module for the current text, parsing it
    /// lazily on first use after each edit. Returns `None` while the
    /// document is syntactically broken.
    #[cfg(test)]
    pub fn with_parsed_module<R>(
        &mut self,
        f: impl FnOnce(&Module<Surface>, &str) -> R,
    ) -> Option<R> {
        self.with_parsed_module_in_context(ParseContextKey::Source, crate::pass::parser::parse, f)
    }

    fn with_parsed_module_in_context<R>(
        &mut self,
        context: ParseContextKey,
        parse: impl FnOnce(&str) -> Result<Module<Surface>, Error>,
        f: impl FnOnce(&Module<Surface>, &str) -> R,
    ) -> Option<R> {
        if self
            .parsed_module
            .as_ref()
            .is_none_or(|(cached, _)| cached != &context)
        {
            #[cfg(test)]
            {
                self.parse_count += 1;
            }
            self.parsed_module = Some((context, parse(&self.text)));
            self.label_reuse_index = None;
        }
        let module = self.parsed_module.as_ref()?.1.as_ref().ok()?;
        Some(f(module, &self.text))
    }

    /// Return the cached label-reuse index for this document version.
    /// Building it shares the document's lazy surface parse, so cursor-level
    /// requests never parse or rebuild the index independently.
    #[cfg(test)]
    pub fn label_reuse_index(&mut self) -> Option<Arc<LabelReuseIndex>> {
        self.label_reuse_index_in_context(ParseContextKey::Source, crate::pass::parser::parse)
    }

    fn label_reuse_index_in_context(
        &mut self,
        context: ParseContextKey,
        parse: impl FnOnce(&str) -> Result<Module<Surface>, Error>,
    ) -> Option<Arc<LabelReuseIndex>> {
        let context_is_current = self
            .parsed_module
            .as_ref()
            .is_some_and(|(cached, _)| cached == &context);
        if !context_is_current {
            self.label_reuse_index = None;
        }
        if self.label_reuse_index.is_none() {
            let index = self.with_parsed_module_in_context(context, parse, |module, source| {
                Arc::new(LabelReuseIndex::from_module(module, source))
            });
            self.label_reuse_index = Some(index);
        }
        self.label_reuse_index.as_ref()?.clone()
    }

    /// Borrow a lazily parsed surface module for the current text,
    /// parsing only headers/signatures on first use after each edit.
    /// Returns `None` while the header surface is syntactically broken.
    pub fn with_lazy_module<R>(
        &mut self,
        f: impl FnOnce(&Module<Surface>, &str) -> R,
    ) -> Option<R> {
        if self.lazy_module.is_none() {
            self.lazy_module = Some(crate::pass::parser::parse_lazy(&self.text));
        }
        let module = self.lazy_module.as_ref()?.as_ref().ok()?.module();
        Some(f(module, &self.text))
    }

    fn file_parser_context(
        &mut self,
        source_path: &Path,
    ) -> Result<ModuleFileParserContext, Error> {
        if self.lazy_module.is_none() {
            self.lazy_module = Some(crate::pass::parser::parse_lazy(&self.text));
        }
        let lazy = match self.lazy_module.as_ref().expect("populated above") {
            Ok(lazy) => lazy,
            Err(error) => return Err(error.clone()),
        };
        let context = ModuleFileParserContext::from_module(source_path, lazy.module())?;
        Ok(context)
    }

    fn materialize_file_header(&mut self) -> crate::pass::parser::ModuleFile {
        #[cfg(test)]
        {
            self.file_context_materialization_count += 1;
        }
        let lazy = self
            .lazy_module
            .as_ref()
            .expect("file context requires a cached lazy parse")
            .as_ref()
            .expect("file context is built only from a valid lazy parse")
            .clone();
        crate::pass::parser::ModuleFile {
            module: lazy.module().clone(),
            lazy: Some(lazy),
        }
    }

    fn with_file_parsed_module_in_context<R>(
        &mut self,
        context: ParseContextKey,
        f: impl FnOnce(&Module<Surface>, &str) -> R,
    ) -> Option<R> {
        if self
            .parsed_module
            .as_ref()
            .is_none_or(|(cached, _)| cached != &context)
        {
            #[cfg(test)]
            {
                self.parse_count += 1;
            }
            let header = self.materialize_file_header();
            self.parsed_module = Some((context, header.force_all().map(|file| file.module)));
            self.label_reuse_index = None;
        }
        let module = self.parsed_module.as_ref()?.1.as_ref().ok()?;
        Some(f(module, &self.text))
    }

    fn file_label_reuse_index_in_context(
        &mut self,
        context: ParseContextKey,
    ) -> Option<Arc<LabelReuseIndex>> {
        let context_is_current = self
            .parsed_module
            .as_ref()
            .is_some_and(|(cached, _)| cached == &context);
        if !context_is_current {
            self.label_reuse_index = None;
        }
        if self.label_reuse_index.is_none() {
            let index = self.with_file_parsed_module_in_context(context, |module, source| {
                Arc::new(LabelReuseIndex::from_module(module, source))
            });
            self.label_reuse_index = Some(index);
        }
        self.label_reuse_index.as_ref()?.clone()
    }

    /// Replace the entire text. Used for `didChange` events whose
    /// `range` is `None` (full-document replacement) and for resetting
    /// the overlay text on `didOpen` if the URI was already open.
    pub fn set_text(&mut self, text: String, version: i32) {
        self.text = text;
        self.version = version;
        self.line_index = None;
        self.parsed_module = None;
        self.label_reuse_index = None;
        self.lazy_module = None;
    }

    /// Apply one LSP `TextDocumentContentChangeEvent`. The event is
    /// either a full-text replacement (`range` is `None`) or an
    /// incremental edit (`range` is `Some` — the LSP positions get
    /// converted to byte offsets via the current [`LineIndex`] before
    /// splicing).
    ///
    /// `new_version` is the document version after this edit applies.
    /// In a multi-edit `didChange` payload, LSP says each event's
    /// positions are relative to the *post-previous-edit* document
    /// state — applying them in order with intermediate version bumps
    /// matches that contract; the per-event version is the same as
    /// the payload-level new version (LSP doesn't carry per-event
    /// versions). Callers passing a multi-edit batch reuse the
    /// notification's `text_document.version` for every event in the
    /// batch, which is what LSP specifies.
    pub fn apply_change(
        &mut self,
        change: &TextDocumentContentChangeEvent,
        new_version: i32,
    ) -> Result<(), SpliceError> {
        match change.range {
            None => {
                // Full-document replacement.
                self.set_text(change.text.clone(), new_version);
                Ok(())
            }
            Some(range) => {
                // Convert LSP positions to byte offsets against the
                // *current* line index, then splice. Building the
                // index here also caches it for the next edit in the
                // same notification (subsequent edits invalidate it
                // via the splice).
                let idx = self.line_index();
                let start = idx.position_to_offset(LspPosition {
                    line: range.start.line,
                    character: range.start.character,
                }) as usize;
                let end = idx.position_to_offset(LspPosition {
                    line: range.end.line,
                    character: range.end.character,
                }) as usize;
                self.splice(start, end, &change.text, new_version)
            }
        }
    }

    /// Apply an in-place splice: replace bytes in `start..end` with
    /// `replacement`. The caller has already converted any LSP
    /// `Position` range to byte offsets via [`Self::line_index`]. The
    /// byte offsets must be on UTF-8 character boundaries — the caller
    /// is responsible for that, since LSP positions snap to UTF-16
    /// code-unit boundaries which always coincide with code-point
    /// boundaries. The version bumps to `new_version`.
    ///
    /// Returns `Err` if the byte range is out of bounds or splits a
    /// UTF-8 character; the caller surfaces the malformed event to the
    /// editor (today: dropped on the floor with a stderr warning;
    /// `didChange` events that don't apply should be rare in practice).
    pub fn splice(
        &mut self,
        start: usize,
        end: usize,
        replacement: &str,
        new_version: i32,
    ) -> Result<(), SpliceError> {
        if start > end || end > self.text.len() {
            return Err(SpliceError::OutOfBounds {
                start,
                end,
                len: self.text.len(),
            });
        }
        if !self.text.is_char_boundary(start) || !self.text.is_char_boundary(end) {
            return Err(SpliceError::NotCharBoundary { start, end });
        }
        // `replace_range` is the idiomatic O(len + replacement.len())
        // splice on a String.
        self.text.replace_range(start..end, replacement);
        self.version = new_version;
        self.line_index = None;
        self.parsed_module = None;
        self.label_reuse_index = None;
        self.lazy_module = None;
        Ok(())
    }
}

/// Errors from [`OpenDocument::splice`] when an edit's byte offsets
/// don't fit the current text. Surfaces as a stderr warning from the
/// LSP server — a well-behaved editor never sends one.
#[derive(Debug)]
pub enum SpliceError {
    OutOfBounds {
        start: usize,
        end: usize,
        len: usize,
    },
    NotCharBoundary {
        start: usize,
        end: usize,
    },
}

impl std::fmt::Display for SpliceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpliceError::OutOfBounds { start, end, len } => write!(
                f,
                "didChange edit range {start}..{end} out of bounds for document length {len}"
            ),
            SpliceError::NotCharBoundary { start, end } => write!(
                f,
                "didChange edit range {start}..{end} does not fall on UTF-8 character boundaries"
            ),
        }
    }
}

impl std::error::Error for SpliceError {}

/// A successful analysis plus the snapshot identity it was computed
/// from. Typed LSP requests can use the analysis as a stale-good
/// fallback while newer analysis is pending; mutating requests can
/// inspect the snapshot before producing edits.
#[derive(Debug)]
pub struct StoredAnalysis {
    snapshot: Snapshot,
    analysis: LspAnalysis,
}

impl StoredAnalysis {
    pub fn new(snapshot: Snapshot, analysis: LspAnalysis) -> Self {
        Self { snapshot, analysis }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn analysis(&self) -> &LspAnalysis {
        &self.analysis
    }
}

/// One open document's focused-analysis lifecycle. URI identity survives
/// canonical-path changes on disk, while `file_path` remains the exact
/// analysis key captured when the current focused request was scheduled.
#[derive(Debug)]
struct FocusedAnalysisState {
    file_path: PathBuf,
    freshness_watermark: SnapshotId,
    analysis: Option<StoredAnalysis>,
}

/// Per-server state for the LSP. Holds the overlay store, bookkeeping
/// for published diagnostics, and the most-recently-successful analysis
/// results used by hover / goto-definition / references queries.
#[derive(Debug, Default)]
pub struct ServerState {
    pub(super) completion_snippets: bool,
    /// Overlay store: one entry per URI the editor has opened (and
    /// not yet closed). The entry's text overrides disk for analysis;
    /// the entry's version is reported back via
    /// `publishDiagnostics.version`.
    documents: BTreeMap<Uri, OpenDocument>,
    /// URIs we have published a non-empty diagnostic set to. When the
    /// next reanalysis pass yields no diagnostic for one of these
    /// URIs, the server publishes an empty `Vec<Diagnostic>` to clear
    /// the stale set. LSP replaces published diagnostics wholesale
    /// per URI, so this set is the only piece of state we need to
    /// track to discharge the "clear stale diagnostics" rule.
    published_with_diagnostics: BTreeSet<Uri>,
    /// Most-recently-successful analysis per package root. Keyed by
    /// the canonicalized package root path the worker analyzed. Hover
    /// / goto-definition / references queries read from this; they
    /// never block on a fresh analysis (serve the stale snapshot
    /// instead, matching rust-analyzer's behavior).
    analyses: BTreeMap<PathBuf, StoredAnalysis>,
    /// Focused scheduling and stale-good state keyed by the open document's
    /// stable URI. Each entry binds the canonical file identity captured at
    /// scheduling time, the latest freshness watermark, and an optional typed
    /// shard. Closing or reopening the URI drops the whole lifecycle. A
    /// successful full analysis may drop only the shard while retaining the
    /// watermark until close.
    focused_by_uri: BTreeMap<Uri, FocusedAnalysisState>,
    /// Latest full-analysis snapshot id scheduled per package root.
    /// Older full worker results are obsolete: a newer edit has
    /// already requested fresher diagnostics.
    latest_scheduled: BTreeMap<PathBuf, SnapshotId>,
}

impl ServerState {
    /// Construct an empty server state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the editor opened a document, seeding the overlay
    /// with the initial text and version the `didOpen` event carries.
    /// If a document with the same URI was already open (an editor
    /// quirk — LSP says clients must `didClose` first, but we tolerate
    /// re-opens), the existing entry's text is replaced.
    pub fn open(&mut self, uri: Uri, text: String, version: i32) {
        self.focused_by_uri.remove(&uri);
        match self.documents.get_mut(&uri) {
            Some(doc) => doc.set_text(text, version),
            None => {
                self.documents.insert(uri, OpenDocument::new(text, version));
            }
        }
    }

    /// Record that the editor closed a document. Removes its overlay,
    /// focused typed shard, and focused freshness watermark;
    /// subsequent analysis reads source from disk for this URI.
    pub fn close(&mut self, uri: &Uri) {
        self.focused_by_uri.remove(uri);
        self.documents.remove(uri);
    }

    /// Borrow the overlay entry for `uri`, if any.
    pub fn document(&self, uri: &Uri) -> Option<&OpenDocument> {
        self.documents.get(uri)
    }

    /// Mutably borrow the overlay entry for `uri`, if any. Used by the
    /// `didChange` handler to apply incremental edits.
    pub fn document_mut(&mut self, uri: &Uri) -> Option<&mut OpenDocument> {
        self.documents.get_mut(uri)
    }

    /// All URIs the editor currently has open.
    pub fn open_documents(&self) -> impl Iterator<Item = &Uri> {
        self.documents.keys()
    }

    /// Snapshot the overlay store as a `(URI, text, version)` triple
    /// list. The worker thread receives this snapshot and runs
    /// analysis against it (overlay text overrides disk for open URIs).
    /// Cloning the text into the snapshot is the price of decoupling
    /// the worker from the main thread's overlay store; today's
    /// overlay sizes (a handful of open files, kilobytes each) make
    /// the copy cheap.
    pub fn overlay_snapshot(&self) -> Vec<(Uri, String, i32)> {
        self.documents
            .iter()
            .map(|(u, d)| (u.clone(), d.text.clone(), d.version))
            .collect()
    }

    pub fn source_overlay(&self) -> SourceOverlay {
        let mut overlay = SourceOverlay::empty();
        for (uri, document) in &self.documents {
            if let Some(path) = uri_to_canonical(uri) {
                overlay.insert(path, document.text.clone());
            }
        }
        overlay
    }

    /// Use the cached parsed module for an open document. Returns
    /// `None` when the document is not open or does not parse.
    pub fn with_overlay_parsed_module<R>(
        &mut self,
        uri: &Uri,
        f: impl FnOnce(&Module<Surface>, &str) -> R,
    ) -> Option<R> {
        let context = ParseContextKey::Source;
        self.documents.get_mut(uri)?.with_parsed_module_in_context(
            context,
            |source| crate::pass::parser::parse_module_file(source).map(|file| file.module),
            f,
        )
    }

    /// Use the cached parsed module for an ordinary URI-backed file. Its
    /// lexical path is validated against its declared module path before the
    /// syntax cache is consulted.
    pub(crate) fn with_overlay_parsed_module_for_file<R>(
        &mut self,
        uri: &Uri,
        source_path: &Path,
        f: impl FnOnce(&Module<Surface>, &str) -> R,
    ) -> Option<R> {
        let file_context = self
            .documents
            .get_mut(uri)?
            .file_parser_context(source_path)
            .ok()?;
        let context = ParseContextKey::File(file_context);
        self.documents
            .get_mut(uri)?
            .with_file_parsed_module_in_context(context, f)
    }

    /// Return the cached label-reuse index for an open document.
    pub fn overlay_label_reuse_index(&mut self, uri: &Uri) -> Option<Arc<LabelReuseIndex>> {
        let context = ParseContextKey::Source;
        self.documents
            .get_mut(uri)?
            .label_reuse_index_in_context(context, |source| {
                crate::pass::parser::parse_module_file(source).map(|file| file.module)
            })
    }

    /// Return the cached label-reuse index for an ordinary URI-backed file.
    pub(crate) fn overlay_label_reuse_index_for_file(
        &mut self,
        uri: &Uri,
        source_path: &Path,
    ) -> Option<Arc<LabelReuseIndex>> {
        let file_context = self
            .documents
            .get_mut(uri)?
            .file_parser_context(source_path)
            .ok()?;
        let context = ParseContextKey::File(file_context);
        self.documents
            .get_mut(uri)?
            .file_label_reuse_index_in_context(context)
    }

    /// Use the cached lazy parse for an open document. Returns `None`
    /// when the document is not open or its header/signature surface
    /// does not parse.
    pub fn with_overlay_lazy_module<R>(
        &mut self,
        uri: &Uri,
        f: impl FnOnce(&Module<Surface>, &str) -> R,
    ) -> Option<R> {
        self.documents.get_mut(uri)?.with_lazy_module(f)
    }

    /// Record that we just published a non-empty diagnostic set for
    /// `uri`. The next reanalysis pass uses this list to decide which
    /// URIs to clear.
    pub fn mark_published(&mut self, uri: Uri) {
        self.published_with_diagnostics.insert(uri);
    }

    /// Take and clear the set of URIs we have previously published
    /// diagnostics for. The caller iterates the returned set and
    /// publishes an empty `Vec<Diagnostic>` to any URI that the new
    /// analysis did not produce a diagnostic for.
    pub fn drain_published(&mut self) -> BTreeSet<Uri> {
        std::mem::take(&mut self.published_with_diagnostics)
    }

    /// Record the newest analysis request scheduled for `package_root`.
    /// Completed worker results older than this request are ignored by
    /// the publish layer.
    pub fn mark_scheduled(&mut self, package_root: &Path, snapshot_id: SnapshotId) {
        self.latest_scheduled
            .insert(package_root.to_path_buf(), snapshot_id);
    }

    /// Record the newest focused analysis request for `uri` and its current
    /// canonical `file_path`. A changed canonical identity starts a fresh
    /// lifecycle and cannot reuse the old path's shard.
    pub fn mark_focused_scheduled(&mut self, uri: &Uri, file_path: &Path, snapshot_id: SnapshotId) {
        if !self.documents.contains_key(uri) {
            unreachable!("focused work was scheduled for a document that is not open");
        }
        match self.focused_by_uri.get_mut(uri) {
            Some(focused) if focused.file_path == file_path => {
                focused.freshness_watermark = snapshot_id;
            }
            _ => {
                self.focused_by_uri.insert(
                    uri.clone(),
                    FocusedAnalysisState {
                        file_path: file_path.to_path_buf(),
                        freshness_watermark: snapshot_id,
                        analysis: None,
                    },
                );
            }
        }
    }

    /// Whether a full worker result is older than the newest scheduled
    /// full snapshot for `package_root`.
    pub fn worker_result_is_obsolete(&self, package_root: &Path, snapshot_id: SnapshotId) -> bool {
        self.latest_scheduled
            .get(package_root)
            .is_some_and(|latest| snapshot_id < *latest)
    }

    /// Whether a focused worker result is older than the newest
    /// focused request for the same file, no longer covers the
    /// editor's current document version, or arrived after close.
    pub fn focused_worker_result_is_obsolete(
        &self,
        file_path: &Path,
        uri: &Uri,
        snapshot: &Snapshot,
    ) -> bool {
        let Some(document) = self.document(uri) else {
            return true;
        };
        let Some(focused) = self.focused_by_uri.get(uri) else {
            return true;
        };
        focused.file_path != file_path
            || snapshot.id() < focused.freshness_watermark
            || !snapshot.covers(uri, document.version())
    }

    /// Store a completed [`LspAnalysis`] for `package_root`, paired with
    /// the snapshot it was computed from. The worker calls this when a
    /// clean typecheck completes; subsequent hover / goto-definition /
    /// references queries read from it. A successful full snapshot advances
    /// each focused lifecycle whose file and current open-document version it
    /// covers, so an older in-flight focused result cannot resurrect state;
    /// covered focused shards no newer than that full snapshot are no longer
    /// needed.
    pub fn store_analysis(
        &mut self,
        package_root: &Path,
        snapshot: Snapshot,
        analysis: LspAnalysis,
    ) {
        let snapshot_id = snapshot.id();
        let documents = &self.documents;
        for (uri, focused) in &mut self.focused_by_uri {
            if !analysis.file_to_module.contains_key(&focused.file_path)
                || !documents
                    .get(uri)
                    .is_some_and(|document| snapshot.covers(uri, document.version()))
            {
                continue;
            }
            focused.freshness_watermark = focused.freshness_watermark.max(snapshot_id);
            if focused
                .analysis
                .as_ref()
                .is_some_and(|stored| stored.snapshot().id() <= snapshot_id)
            {
                focused.analysis = None;
            }
        }
        self.analyses.insert(
            package_root.to_path_buf(),
            StoredAnalysis::new(snapshot, analysis),
        );
    }

    /// Store a focused single-file analysis shard for an open document.
    pub fn store_focused_analysis(
        &mut self,
        uri: &Uri,
        file_path: &Path,
        snapshot: Snapshot,
        analysis: LspAnalysis,
    ) {
        let Some(focused) = self.focused_by_uri.get_mut(uri) else {
            unreachable!("focused result was accepted without a current open-document request");
        };
        if focused.file_path != file_path || snapshot.id() < focused.freshness_watermark {
            unreachable!(
                "focused result was accepted for a different file identity or after supersession"
            );
        }
        focused.analysis = Some(StoredAnalysis::new(snapshot, analysis));
    }

    /// Return a reference to the most-recent analysis for the package
    /// that contains `file_path`, if one exists. `file_path` must be
    /// the canonical absolute path to the source file.
    ///
    /// Implementation: the analysis's `file_to_module` map contains an
    /// entry for every canonical source path in the package — look in
    /// each stored analysis for one that covers `file_path`.
    pub fn analysis_for_file(&self, file_path: &std::path::Path) -> Option<&LspAnalysis> {
        self.analysis_entry_for_file(file_path)
            .map(StoredAnalysis::analysis)
    }

    /// Declaration-catalog lookup for a new or untitled consumer. Callers
    /// validate the selected provider's snapshot before using its entries.
    pub(crate) fn analysis_for_package_root(&self, root: &Path) -> Option<&LspAnalysis> {
        self.analyses.get(root).map(StoredAnalysis::analysis)
    }

    /// Return the stored analysis entry for the package containing
    /// `file_path`, including the snapshot metadata.
    pub fn analysis_entry_for_file(&self, file_path: &std::path::Path) -> Option<&StoredAnalysis> {
        self.analyses
            .values()
            .find(|a| a.analysis.file_to_module.contains_key(file_path))
    }

    /// Return the best typed data for a single-file request. A fresh
    /// full-package analysis wins. If the full analysis is stale but a
    /// focused shard covers the open document version, use the focused
    /// shard. An open document never falls back to an older version: a
    /// failed current analysis must not let stale binder identities authorize
    /// navigation or edits in changed source.
    pub fn best_analysis_for_file(
        &self,
        file_path: &std::path::Path,
        uri: &Uri,
    ) -> Option<&LspAnalysis> {
        self.best_analysis_entry_for_file(file_path, uri)
            .map(StoredAnalysis::analysis)
    }

    /// As [`Self::best_analysis_for_file`], but includes the snapshot
    /// metadata.
    pub fn best_analysis_entry_for_file(
        &self,
        file_path: &std::path::Path,
        uri: &Uri,
    ) -> Option<&StoredAnalysis> {
        let full = self.analysis_entry_for_file(file_path);
        if full.is_some_and(|entry| self.entry_covers_uri(entry, uri)) {
            return full;
        }
        self.focused_by_uri
            .get(uri)
            .filter(|focused| focused.file_path == file_path)
            .and_then(|focused| focused.analysis.as_ref())
            .filter(|entry| self.entry_covers_uri(entry, uri))
    }

    /// Optional completion metadata requires the captured dependency snapshot,
    /// not just the consumer's version. Keep this stricter choice separate
    /// from the navigation and provider-first import-completion routes.
    pub(crate) fn completion_analysis_for_file(
        &self,
        file_path: &Path,
        uri: &Uri,
    ) -> Option<&LspAnalysis> {
        let open_sources: BTreeMap<_, _> = self
            .documents
            .iter()
            .map(|(uri, document)| (uri, (document, uri_to_canonical(uri))))
            .collect();
        let current =
            |entry: &StoredAnalysis| self.entry_sources_are_current(entry, uri, &open_sources);
        let full = self
            .analyses
            .iter()
            .find(|(_, entry)| entry.analysis.file_to_module.contains_key(file_path));
        if let Some((root, entry)) = full
            && !self.worker_result_is_obsolete(root, entry.snapshot.id())
            && current(entry)
        {
            return Some(entry.analysis());
        }
        self.focused_by_uri
            .get(uri)
            .filter(|focused| focused.file_path == file_path)
            .and_then(|focused| {
                focused.analysis.as_ref().filter(|entry| {
                    entry.snapshot.id() >= focused.freshness_watermark && current(entry)
                })
            })
            .map(StoredAnalysis::analysis)
    }

    fn entry_sources_are_current(
        &self,
        entry: &StoredAnalysis,
        uri: &Uri,
        open_sources: &BTreeMap<&Uri, (&OpenDocument, Option<PathBuf>)>,
    ) -> bool {
        self.entry_covers_uri(entry, uri)
            && !self.latest_scheduled.iter().any(|(root, latest)| {
                entry.snapshot.id() < *latest
                    && entry
                        .analysis
                        .sources
                        .keys()
                        .any(|path| path.starts_with(root))
            })
            && entry.snapshot.versions().iter().all(|(uri, version)| {
                let Some(path) = entry.snapshot.source_path(uri) else {
                    return false;
                };
                let Some(source) = entry.analysis.sources.get(path) else {
                    return true;
                };
                open_sources
                    .get(uri)
                    .is_some_and(|(document, current_path)| {
                        current_path.as_deref() == Some(path)
                            && document.version() == *version
                            && document.text() == source
                    })
            })
            && open_sources.iter().all(|(uri, (document, path))| {
                let Some(path) = path else {
                    return true;
                };
                entry.analysis.sources.get(path).is_none_or(|source| {
                    entry.snapshot.version(uri) == Some(document.version())
                        && entry.snapshot.source_path(uri) == Some(path.as_path())
                        && source == document.text()
                })
            })
    }

    /// A continuation-label identity depends on its selected provider descriptor.
    /// Check this exact entry while retaining ordinary stale-good navigation.
    pub(crate) fn block_label_query_is_current(
        &self,
        entry: &StoredAnalysis,
        uri: &Uri,
        position: &lsp_types::Position,
    ) -> bool {
        let Some(path) = uri_to_canonical(uri) else {
            return false;
        };
        let analysis = entry.analysis();
        let Some(module) = analysis.file_to_module.get(&path) else {
            return false;
        };
        let Some(source) = analysis.sources.get(&path) else {
            return false;
        };
        let offset = LineIndex::new(source).position_to_offset(LspPosition {
            line: position.line,
            character: position.character,
        });
        let binder = crate::lsp::util::smallest_containing_span(
            offset,
            analysis
                .position_index
                .binders_iter()
                .filter(|((owner, _), _)| owner == module)
                .map(|((_, span), binder)| (*span, binder)),
        );
        if !matches!(
            binder,
            Some((
                _,
                crate::pass::typecheck_full::ResolvedBinder::BlockLabel { .. }
            ))
        ) {
            return true;
        }
        let open_sources = self
            .documents
            .iter()
            .map(|(uri, document)| (uri, (document, uri_to_canonical(uri))))
            .collect();
        self.entry_sources_are_current(entry, uri, &open_sources)
    }

    /// Return a full-package analysis only when it covers the exact current
    /// open-document version. Cross-file requests cannot use a focused shard,
    /// but they also cannot use an older full snapshot after an invalid edit.
    pub fn current_full_analysis_entry_for_file(
        &self,
        file_path: &std::path::Path,
        uri: &Uri,
    ) -> Option<&StoredAnalysis> {
        self.analysis_entry_for_file(file_path)
            .filter(|entry| self.entry_covers_uri(entry, uri))
    }

    pub fn current_full_analysis_for_file(
        &self,
        file_path: &std::path::Path,
        uri: &Uri,
    ) -> Option<&LspAnalysis> {
        self.current_full_analysis_entry_for_file(file_path, uri)
            .map(StoredAnalysis::analysis)
    }

    fn entry_covers_uri(&self, entry: &StoredAnalysis, uri: &Uri) -> bool {
        match self.document(uri).map(OpenDocument::version) {
            Some(version) => entry.snapshot().covers(uri, version),
            // A closed document is disk-backed. An analysis that observed an
            // open overlay cannot certify the source after that overlay closes.
            None => entry.snapshot().version(uri).is_none(),
        }
    }
}

#[cfg(test)]
mod tests {

    const IMPORT_GRAMMAR_LSP_CONSUMER: &str = "module app/main;
        import syntax(op _ + _, op _ => _, varop [% %]);
        labels { field: . };
        pub fn run(a: ., b: .) -> . { [% a => (a + b), b => a %] }";

    fn import_grammar_assert_lsp_ast(module: &Module, operator: &str) {
        use crate::ast::{Expr, ImportItem, ImportKind, Item, OpChainKind, OperatorGrammar};
        let ImportKind::Selective { items, .. } = &module.imports[0].kind else {
            panic!("selective")
        };
        assert!(items.iter().any(|item| matches!(item,
            ImportItem::OperatorPattern { grammar: OperatorGrammar::Fixed(pattern), .. }
            if crate::ast::OperatorDispatchKey::from_pattern(pattern).leading_run == [operator])));
        assert_eq!(module.items.len(), 2);
        let Item::FnDef(run) = &module.items[1] else {
            panic!("run")
        };
        let Expr::OpChain {
            kind: OpChainKind::Variadic { elements, .. },
            ..
        } = &run.body
        else {
            panic!("variadic")
        };
        assert_eq!(elements.len(), 2);
        let Expr::OpChain {
            kind: OpChainKind::Normal { slots: pair, .. },
            ..
        } = &elements[0]
        else {
            panic!("pair element")
        };
        let Expr::OpChain {
            kind: OpChainKind::Normal { pattern, slots },
            ..
        } = &pair[1]
        else {
            panic!("fixed operator body")
        };
        assert_eq!(
            crate::ast::OperatorDispatchKey::from_pattern(pattern).leading_run,
            [operator]
        );
        assert_eq!(slots.len(), 2);
    }

    fn import_grammar_request_syntax(
        state: &mut ServerState,
        consumer: &Uri,
        file: bool,
        operator: &str,
    ) {
        let path = absolute_test_path("/workspace/app/main.kio");
        if file {
            assert!(
                state
                    .with_overlay_parsed_module_for_file(consumer, &path, |m, _| {
                        import_grammar_assert_lsp_ast(m, operator)
                    })
                    .is_some()
            );
            assert!(
                state
                    .overlay_label_reuse_index_for_file(consumer, &path)
                    .is_some()
            );
        } else {
            assert!(
                state
                    .with_overlay_parsed_module(consumer, |m, _| import_grammar_assert_lsp_ast(
                        m, operator
                    ))
                    .is_some()
            );
            assert!(state.overlay_label_reuse_index(consumer).is_some());
        }
        let source = state.document(consumer).unwrap().text().to_owned();
        let formatted = crate::cmd::fmt::format_source("", &source).unwrap();
        import_grammar_assert_lsp_ast(&crate::pass::parser::parse(&formatted).unwrap(), operator);
    }

    fn import_grammar_provider(clause: &str, marker: &str, extra: &str) -> String {
        format!(
            "module syntax;
            pub fn plus(a: ., b: .) -> . {{ a }}
            pub fn base() -> . {{ () }}
            pub fn entry(a: ., b: .) -> . & . {{ (a, b) }}
            pub fn seed(a: ., b: .) -> . {{ b }}
            pub fn step(a: ., b: ., c: .) -> . {{ a }}
            pub fn other((a: ., b: .), c: .) -> . {{ c }}
            pub fn finish(a: .) -> . {{ a }}
            pub op _ + _ {{ impl plus; }};
            pub op _ => _ {{ impl entry; }};
            pub varop [{marker} {marker}] {{ {clause} }};
            {extra}"
        )
    }

    fn import_grammar_analyze(
        state: &ServerState,
        file: bool,
    ) -> Result<LspAnalysis, crate::cmd::check::AnalysisFailure> {
        let root = absolute_test_path("/workspace");
        let mut files = BTreeMap::new();
        files.insert(
            root.join("proof.pkg.kio"),
            "package proof; bridge { app/main; }".into(),
        );
        for uri in state.open_documents() {
            if let Some(path) = uri_to_canonical(uri) {
                files.insert(path, state.document(uri).unwrap().text().to_owned());
            }
        }
        let overlay = SourceOverlay::complete(root.clone(), files);
        if file {
            crate::cmd::check::analyze_module_at_with_overlay_lsp(
                &root,
                &overlay,
                &root.join("app/main.kio"),
            )
        } else {
            crate::cmd::check::analyze_workspace_at_with_overlay_lsp(&root, &overlay)
        }
    }

    #[test]
    fn import_grammar_provider_edits_keep_syntax() {
        use crate::package_collection::import_grammar_with_denied_reads;
        for file in [false, true] {
            let mut state = ServerState::new();
            let consumer = uri("file:///workspace/app/main.kio");
            let provider = uri("file:///workspace/syntax.kio");
            state.open(consumer.clone(), IMPORT_GRAMMAR_LSP_CONSUMER.into(), 1);
            let sources = [
                (import_grammar_provider("foldl step base;", "%", ""), None),
                (
                    import_grammar_provider("foldr1 other seed; finalize finish;", "%", ""),
                    None,
                ),
                (
                    import_grammar_provider("foldl step base;", "!", ""),
                    Some("exports no operator matching"),
                ),
                (
                    import_grammar_provider(
                        "foldl step base;",
                        "%",
                        "pub fn unrelated() -> . { () }",
                    ),
                    None,
                ),
                ("module syntax; broken {".into(), Some("provider parse")),
            ];
            for (event, (source, error_kind)) in sources.iter().enumerate() {
                state.open(provider.clone(), source.clone(), event as i32 + 1);
                let (_, attempts) = import_grammar_with_denied_reads(
                    [absolute_test_path("/workspace/syntax.kio")],
                    || {
                        import_grammar_request_syntax(&mut state, &consumer, file, "+");
                        import_grammar_request_syntax(&mut state, &consumer, file, "+");
                    },
                );
                assert!(attempts.is_empty());
                let semantic = import_grammar_analyze(&state, file);
                match error_kind {
                    None => {
                        let analysis = semantic.expect("valid current provider must typecheck");
                        assert_eq!(
                            analysis
                                .sources
                                .get(&absolute_test_path("/workspace/syntax.kio")),
                            Some(source)
                        );
                    }
                    Some("provider parse") => {
                        let failure = semantic
                            .expect_err("broken provider must refresh semantic diagnostics");
                        assert!(
                            failure.errors.iter().any(|error| error.file_path
                                == absolute_test_path("/workspace/syntax.kio"))
                        );
                    }
                    Some(message) => {
                        let failure = semantic
                            .expect_err("grammar mismatch must refresh semantic diagnostics");
                        assert!(
                            failure.errors.iter().any(|error| error.file_path
                                == absolute_test_path("/workspace/app/main.kio")
                                && error.error.diag().1.contains(*message)),
                            "{:?}",
                            failure.errors
                        );
                    }
                }
                let document = state.document(&consumer).unwrap();
                assert_eq!(document.parse_count, 1, "file={file} event={event}");
                if file {
                    assert_eq!(document.file_context_materialization_count, 1);
                }
            }
        }
        eprintln!(
            "LSP provider events: routes=2 events=5 syntax_counts=[1,1,1,1,1] semantic=[ok,ok,mismatch,ok,provider_error]"
        );
    }

    #[test]
    fn import_grammar_consumer_edits_refresh_syntax() {
        for file in [false, true] {
            let mut state = ServerState::new();
            let consumer = uri("file:///workspace/app/main.kio");
            state.open(consumer.clone(), IMPORT_GRAMMAR_LSP_CONSUMER.into(), 1);
            import_grammar_request_syntax(&mut state, &consumer, file, "+");
            let changed = IMPORT_GRAMMAR_LSP_CONSUMER
                .replace("_ + _", "_ - _")
                .replace("a + b", "a - b");
            state
                .document_mut(&consumer)
                .unwrap()
                .apply_change(&full_change(&changed), 2)
                .unwrap();
            import_grammar_request_syntax(&mut state, &consumer, file, "-");
            import_grammar_request_syntax(&mut state, &consumer, file, "-");
            let document = state.document(&consumer).unwrap();
            assert_eq!(document.parse_count, 2);
            if file {
                assert_eq!(document.file_context_materialization_count, 2);
            }
        }
        eprintln!("LSP consumer events: routes=2 syntax_counts=[1,2,2] grammar_and_body=[+,-,-]");
    }

    #[test]
    fn import_grammar_missing_provider_keeps_syntax() {
        use crate::package_collection::import_grammar_with_denied_reads;
        for file in [false, true] {
            let mut state = ServerState::new();
            let consumer = uri("file:///workspace/app/main.kio");
            let provider = uri("file:///workspace/syntax.kio");
            state.open(consumer.clone(), IMPORT_GRAMMAR_LSP_CONSUMER.into(), 1);
            for present in [false, true, false] {
                if present {
                    state.open(
                        provider.clone(),
                        import_grammar_provider("foldl step base;", "%", ""),
                        1,
                    );
                } else {
                    state.close(&provider);
                }
                let (_, attempts) = import_grammar_with_denied_reads(
                    [absolute_test_path("/workspace/syntax.kio")],
                    || import_grammar_request_syntax(&mut state, &consumer, file, "+"),
                );
                assert!(attempts.is_empty());
                let result = import_grammar_analyze(&state, file);
                if present {
                    result.expect("present provider restores semantic analysis");
                } else {
                    let failure = result.expect_err("absent provider remains a semantic error");
                    assert!(
                        failure
                            .errors
                            .iter()
                            .any(|error| error.error.diag().1.contains("syntax")),
                        "{:?}",
                        failure.errors
                    );
                }
                assert_eq!(state.document(&consumer).unwrap().parse_count, 1);
            }
        }
        let mut untitled = ServerState::new();
        let consumer = uri("untitled:proof.kio");
        untitled.open(consumer.clone(), IMPORT_GRAMMAR_LSP_CONSUMER.into(), 1);
        assert!(
            untitled
                .with_overlay_parsed_module(&consumer, |module, _| import_grammar_assert_lsp_ast(
                    module, "+"
                ))
                .is_some()
        );
        assert_eq!(untitled.document(&consumer).unwrap().parse_count, 1);
        eprintln!(
            "LSP missing-provider: routes=2 syntax_counts=[1,1,1] semantic=[missing,ok,missing] untitled=1"
        );
    }
    use super::*;
    use lsp_types::{Position, Range};
    use std::str::FromStr;

    fn uri(s: &str) -> Uri {
        match s.strip_prefix("file:///") {
            Some(path) => crate::lsp::util::test_file_uri(format!("/{path}")),
            None => Uri::from_str(s).expect("test URI"),
        }
    }

    fn absolute_test_path(path: &str) -> PathBuf {
        crate::lsp::util::test_file_path(path)
    }

    fn analysis_for_files(files: &[PathBuf]) -> LspAnalysis {
        let file_to_module = files
            .iter()
            .enumerate()
            .map(|(index, path)| (path.clone(), format!("pkg/module_{index}")))
            .collect();
        let sources = files
            .iter()
            .map(|path| (path.clone(), "module pkg/main;\n".to_owned()))
            .collect();
        LspAnalysis {
            position_index: crate::pass::typecheck_full::PositionIndex::new(),
            file_to_module,
            label_reuse_indexes: crate::lsp::label_reuse::indexes_from_sources(&sources),
            sources,
            generated_label_nominals: Default::default(),
            root_package_lowered: crate::pass::resolve::Package::from_parts(BTreeMap::new(), None),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn completion_metadata_retains_captured_identity_when_uri_target_changes() {
        let consumer_uri = uri("file:///completion/main.kio");
        let provider_uri = uri("file:///completion/retargeted.kio");
        let consumer = uri_to_canonical(&consumer_uri).unwrap();
        let captured_provider = absolute_test_path("/completion/original.kio");
        let root = consumer.parent().unwrap();
        let source = "module pkg/main;\n";
        let mut state = ServerState::new();
        state.open(consumer_uri.clone(), source.into(), 1);
        state.open(provider_uri.clone(), source.into(), 1);
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();
        state.store_analysis(
            root,
            Snapshot::new(
                ids.next(),
                BTreeMap::from([(consumer_uri.clone(), 1), (provider_uri.clone(), 1)]),
            )
            .with_source_paths(BTreeMap::from([
                (consumer_uri.clone(), consumer.clone()),
                (provider_uri.clone(), captured_provider.clone()),
            ])),
            analysis_for_files(&[consumer.clone(), captured_provider]),
        );
        assert!(
            state
                .best_analysis_for_file(&consumer, &consumer_uri)
                .is_some()
        );
        assert!(
            state
                .completion_analysis_for_file(&consumer, &consumer_uri)
                .is_none(),
            "unchanged bytes/version do not authorize a different canonical provider identity"
        );
        state.close(&provider_uri);
        assert!(
            state
                .completion_analysis_for_file(&consumer, &consumer_uri)
                .is_none(),
            "closing a retargeted URI must still invalidate its captured provider overlay"
        );
    }

    #[test]
    fn completion_metadata_checks_snapshot_sources_and_focused_fallback() {
        let consumer_uri = uri("file:///completion/main.kio");
        let provider_uri = uri("file:///completion/provider.kio");
        let consumer = uri_to_canonical(&consumer_uri).unwrap();
        let provider = uri_to_canonical(&provider_uri).unwrap();
        let root = consumer.parent().unwrap();
        let source = "module pkg/main;\n";
        let analysis = || analysis_for_files(&[consumer.clone(), provider.clone()]);
        let snapshot = |id, versions: BTreeMap<Uri, i32>| {
            let paths = versions
                .keys()
                .map(|uri| (uri.clone(), uri_to_canonical(uri).unwrap()))
                .collect();
            Snapshot::new(id, versions).with_source_paths(paths)
        };
        let mut state = ServerState::new();
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();
        state.open(consumer_uri.clone(), source.into(), 1);
        state.open(provider_uri.clone(), source.into(), 1);
        state.store_analysis(
            root,
            snapshot(
                ids.next(),
                BTreeMap::from([(consumer_uri.clone(), 1), (provider_uri.clone(), 1)]),
            ),
            analysis(),
        );
        assert!(
            state
                .completion_analysis_for_file(&consumer, &consumer_uri)
                .is_some()
        );

        state.open(provider_uri.clone(), format!("{source} "), 1);
        assert!(
            state
                .completion_analysis_for_file(&consumer, &consumer_uri)
                .is_none()
        );
        assert!(
            state
                .best_analysis_for_file(&consumer, &consumer_uri)
                .is_some(),
            "generic navigation selection is unchanged"
        );
        state.close(&provider_uri);
        assert!(
            state
                .completion_analysis_for_file(&consumer, &consumer_uri)
                .is_none()
        );
        state.store_analysis(
            root,
            snapshot(ids.next(), BTreeMap::from([(consumer_uri.clone(), 1)])),
            analysis(),
        );
        assert!(
            state
                .completion_analysis_for_file(&consumer, &consumer_uri)
                .is_some(),
            "a newly analyzed disk provider is current"
        );
        state.open(provider_uri.clone(), source.into(), 2);
        assert!(
            state
                .completion_analysis_for_file(&consumer, &consumer_uri)
                .is_none(),
            "a newly opened provider cannot reuse a disk snapshot"
        );

        let focused_id = ids.next();
        state.mark_focused_scheduled(&consumer_uri, &consumer, focused_id);
        state.store_focused_analysis(
            &consumer_uri,
            &consumer,
            snapshot(
                focused_id,
                BTreeMap::from([(consumer_uri.clone(), 1), (provider_uri.clone(), 2)]),
            ),
            analysis(),
        );
        assert!(
            state
                .completion_analysis_for_file(&consumer, &consumer_uri)
                .is_some(),
            "fresh focused dependencies replace stale full metadata"
        );
        state.mark_scheduled(root, ids.next());
        assert!(
            state
                .completion_analysis_for_file(&consumer, &consumer_uri)
                .is_none(),
            "a newer package snapshot supersedes focused metadata too"
        );
    }

    fn full_change(text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.to_owned(),
        }
    }

    #[test]
    fn changed_open_document_cannot_fall_back_to_a_stale_typed_snapshot() {
        let mut state = ServerState::new();
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();
        let u = uri("file:///tmp/kio-lsp-stale-authority.kio");
        let file_path = uri_to_canonical(&u).expect("canonical file URI");
        state.open(u.clone(), "module pkg/main;\n".to_owned(), 1);
        state.store_analysis(
            Path::new("/tmp/kio-lsp-stale-authority"),
            Snapshot::new(ids.next(), BTreeMap::from([(u.clone(), 1)])),
            analysis_for_files(std::slice::from_ref(&file_path)),
        );
        assert!(state.best_analysis_for_file(&file_path, &u).is_some());
        assert!(
            state
                .current_full_analysis_for_file(&file_path, &u)
                .is_some()
        );

        state
            .document_mut(&u)
            .expect("open document")
            .apply_change(
                &full_change("module pkg/main;\nnewtype Broken : Broken;"),
                2,
            )
            .expect("apply invalid edit");

        assert!(state.best_analysis_for_file(&file_path, &u).is_none());
        assert!(
            state
                .current_full_analysis_for_file(&file_path, &u)
                .is_none()
        );
        assert!(
            state.analysis_for_file(&file_path).is_some(),
            "retaining old data for closed-file/history uses must not grant it current authority"
        );
    }

    fn range_change(
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
        text: &str,
    ) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position {
                    line: start_line,
                    character: start_char,
                },
                end: Position {
                    line: end_line,
                    character: end_char,
                },
            }),
            range_length: None,
            text: text.to_owned(),
        }
    }

    #[test]
    fn open_seeds_overlay_text_and_version() {
        let mut state = ServerState::new();
        let u = uri("file:///tmp/a.kio");
        state.open(u.clone(), "module pkg/a;\n".to_owned(), 1);
        let doc = state.document(&u).expect("doc present");
        assert_eq!(doc.text(), "module pkg/a;\n");
        assert_eq!(doc.version(), 1);
    }

    #[test]
    fn close_drops_overlay() {
        let mut state = ServerState::new();
        let u = uri("file:///tmp/a.kio");
        state.open(u.clone(), "x".to_owned(), 1);
        assert!(state.document(&u).is_some());
        state.close(&u);
        assert!(state.document(&u).is_none());
    }

    #[test]
    fn focused_retention_is_bounded_by_open_documents() {
        let mut state = ServerState::new();
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();

        for index in 0..4 {
            let u = uri(&format!("file:///tmp/kio-lsp-retention-{index}.kio"));
            let file_path = uri_to_canonical(&u).expect("canonical file URI");
            state.open(u.clone(), "module pkg/main;\n".to_owned(), 1);
            let snapshot_id = ids.next();
            state.mark_focused_scheduled(&u, &file_path, snapshot_id);
            state.store_focused_analysis(
                &u,
                &file_path,
                Snapshot::new(snapshot_id, BTreeMap::from([(u.clone(), 1)])),
                analysis_for_files(std::slice::from_ref(&file_path)),
            );

            state.close(&u);
            assert!(state.focused_by_uri.is_empty());
        }
    }

    #[test]
    fn closed_document_rejects_late_focused_result() {
        let mut state = ServerState::new();
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();
        let u = uri("file:///tmp/kio-lsp-late-focus.kio");
        let file_path = uri_to_canonical(&u).expect("canonical file URI");
        state.open(u.clone(), "module pkg/main;\n".to_owned(), 1);
        let snapshot_id = ids.next();
        state.mark_focused_scheduled(&u, &file_path, snapshot_id);
        let snapshot = Snapshot::new(snapshot_id, BTreeMap::from([(u.clone(), 1)]));

        state.close(&u);

        assert!(state.focused_worker_result_is_obsolete(&file_path, &u, &snapshot));
    }

    #[test]
    fn reopened_document_rejects_a_result_from_its_previous_open_lifetime() {
        let mut state = ServerState::new();
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();
        let u = uri("file:///tmp/kio-lsp-reopened-focus.kio");
        let file_path = uri_to_canonical(&u).expect("canonical file URI");
        state.open(u.clone(), "module pkg/old;\n".to_owned(), 1);
        let snapshot_id = ids.next();
        state.mark_focused_scheduled(&u, &file_path, snapshot_id);
        let old_snapshot = Snapshot::new(snapshot_id, BTreeMap::from([(u.clone(), 1)]));

        state.close(&u);
        state.open(u.clone(), "module pkg/new;\n".to_owned(), 1);

        assert!(state.focused_worker_result_is_obsolete(&file_path, &u, &old_snapshot));
    }

    #[test]
    fn repeated_did_open_starts_a_new_focused_lifetime() {
        let mut state = ServerState::new();
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();
        let u = uri("file:///tmp/kio-lsp-repeated-open.kio");
        let file_path = uri_to_canonical(&u).expect("canonical file URI");
        state.open(u.clone(), "module pkg/old;\n".to_owned(), 1);
        let snapshot_id = ids.next();
        state.mark_focused_scheduled(&u, &file_path, snapshot_id);
        let old_snapshot = Snapshot::new(snapshot_id, BTreeMap::from([(u.clone(), 1)]));
        state.store_focused_analysis(
            &u,
            &file_path,
            old_snapshot.clone(),
            analysis_for_files(std::slice::from_ref(&file_path)),
        );

        state.open(u.clone(), "module pkg/new;\n".to_owned(), 1);

        assert!(state.focused_by_uri.is_empty());
        assert!(state.focused_worker_result_is_obsolete(&file_path, &u, &old_snapshot));
    }

    #[test]
    fn changed_canonical_identity_starts_a_fresh_focused_lifecycle() {
        let mut state = ServerState::new();
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();
        let u = uri("file:///tmp/kio-lsp-retargeted-focus.kio");
        let old_path = absolute_test_path("/tmp/kio-lsp-old-target.kio");
        let new_path = absolute_test_path("/tmp/kio-lsp-new-target.kio");
        state.open(u.clone(), "module pkg/main;\n".to_owned(), 1);
        let old_id = ids.next();
        let old_snapshot = Snapshot::new(old_id, BTreeMap::from([(u.clone(), 1)]));
        state.mark_focused_scheduled(&u, &old_path, old_id);
        state.store_focused_analysis(
            &u,
            &old_path,
            old_snapshot.clone(),
            analysis_for_files(std::slice::from_ref(&old_path)),
        );

        let new_id = ids.next();
        let new_snapshot = Snapshot::new(new_id, BTreeMap::from([(u.clone(), 1)]));
        state.mark_focused_scheduled(&u, &new_path, new_id);

        let focused = &state.focused_by_uri[&u];
        assert_eq!(focused.file_path, new_path);
        assert_eq!(focused.freshness_watermark, new_id);
        assert!(focused.analysis.is_none());
        assert!(state.focused_worker_result_is_obsolete(&old_path, &u, &old_snapshot));
        assert!(!state.focused_worker_result_is_obsolete(&new_path, &u, &new_snapshot));
    }

    #[test]
    fn newer_same_file_focus_preserves_stale_good_and_supersedes_old_result() {
        let mut state = ServerState::new();
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();
        let u = uri("file:///tmp/kio-lsp-rescheduled-focus.kio");
        let file_path = absolute_test_path("/tmp/kio-lsp-rescheduled-focus.kio");
        state.open(u.clone(), "module pkg/main;\n".to_owned(), 1);
        let old_id = ids.next();
        let old_snapshot = Snapshot::new(old_id, BTreeMap::from([(u.clone(), 1)]));
        state.mark_focused_scheduled(&u, &file_path, old_id);
        state.store_focused_analysis(
            &u,
            &file_path,
            old_snapshot.clone(),
            analysis_for_files(std::slice::from_ref(&file_path)),
        );

        let new_id = ids.next();
        let new_snapshot = Snapshot::new(new_id, BTreeMap::from([(u.clone(), 1)]));
        state.mark_focused_scheduled(&u, &file_path, new_id);

        let focused = &state.focused_by_uri[&u];
        assert_eq!(focused.freshness_watermark, new_id);
        assert_eq!(
            focused
                .analysis
                .as_ref()
                .expect("stale-good shard retained while newer work runs")
                .snapshot()
                .id(),
            old_id
        );
        assert!(state.focused_worker_result_is_obsolete(&file_path, &u, &old_snapshot));
        assert!(!state.focused_worker_result_is_obsolete(&file_path, &u, &new_snapshot));
    }

    #[cfg(unix)]
    #[test]
    fn close_drops_focused_state_after_canonical_identity_drift() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(&first).expect("first target");
        std::fs::create_dir_all(&second).expect("second target");
        std::fs::write(first.join("main.kio"), "module pkg/first;\n").expect("first source");
        std::fs::write(second.join("main.kio"), "module pkg/second;\n").expect("second source");
        let alias = dir.path().join("alias");
        symlink(&first, &alias).expect("first symlink");
        let alias_file = alias.join("main.kio");
        let u = crate::lsp::diagnostics::path_to_uri(&alias_file, dir.path()).expect("file URI");
        let focused_path = std::fs::canonicalize(&alias_file).expect("first canonical path");
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();
        let snapshot_id = ids.next();
        let mut state = ServerState::new();
        state.open(u.clone(), "module pkg/first;\n".to_owned(), 1);
        state.mark_focused_scheduled(&u, &focused_path, snapshot_id);
        state.store_focused_analysis(
            &u,
            &focused_path,
            Snapshot::new(snapshot_id, BTreeMap::from([(u.clone(), 1)])),
            analysis_for_files(std::slice::from_ref(&focused_path)),
        );

        std::fs::remove_file(&alias).expect("remove first symlink");
        symlink(&second, &alias).expect("retarget symlink");
        state.close(&u);

        assert!(state.focused_by_uri.is_empty());
    }

    #[test]
    fn full_analysis_drops_only_covered_nonnewer_focused_shards() {
        let mut state = ServerState::new();
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();
        let older_id = ids.next();
        let unrelated_id = ids.next();
        let uncovered_id = ids.next();
        let full_id = ids.next();
        let newer_id = ids.next();
        let older = absolute_test_path("/tmp/kio-lsp-covered-older.kio");
        let newer = absolute_test_path("/tmp/kio-lsp-covered-newer.kio");
        let uncovered = absolute_test_path("/tmp/kio-lsp-uncovered-older.kio");
        let unrelated = absolute_test_path("/tmp/kio-lsp-unrelated.kio");
        let older_uri =
            crate::lsp::diagnostics::path_to_uri(&older, Path::new("/tmp")).expect("older URI");
        let newer_uri =
            crate::lsp::diagnostics::path_to_uri(&newer, Path::new("/tmp")).expect("newer URI");
        let uncovered_uri = crate::lsp::diagnostics::path_to_uri(&uncovered, Path::new("/tmp"))
            .expect("uncovered URI");
        let unrelated_uri = crate::lsp::diagnostics::path_to_uri(&unrelated, Path::new("/tmp"))
            .expect("unrelated URI");

        for (uri, path, id) in [
            (&older_uri, &older, older_id),
            (&newer_uri, &newer, newer_id),
            (&uncovered_uri, &uncovered, uncovered_id),
            (&unrelated_uri, &unrelated, unrelated_id),
        ] {
            state.open((*uri).clone(), "module pkg/main;\n".to_owned(), 1);
            state.mark_focused_scheduled(uri, path, id);
            state.store_focused_analysis(
                uri,
                path,
                Snapshot::new(id, BTreeMap::from([((*uri).clone(), 1)])),
                analysis_for_files(std::slice::from_ref(path)),
            );
        }

        state.store_analysis(
            Path::new("/tmp/kio-lsp-package"),
            Snapshot::new(
                full_id,
                BTreeMap::from([(older_uri.clone(), 1), (newer_uri.clone(), 1)]),
            ),
            analysis_for_files(&[older.clone(), newer.clone(), uncovered.clone()]),
        );

        assert!(state.focused_by_uri[&older_uri].analysis.is_none());
        assert!(state.focused_by_uri[&newer_uri].analysis.is_some());
        assert_eq!(
            state.focused_by_uri[&newer_uri].freshness_watermark,
            newer_id
        );
        assert!(state.focused_by_uri[&uncovered_uri].analysis.is_some());
        assert!(state.focused_by_uri[&unrelated_uri].analysis.is_some());
        assert_eq!(state.focused_by_uri.len(), 4);
        assert!(!state.focused_worker_result_is_obsolete(
            &uncovered,
            &uncovered_uri,
            &Snapshot::new(uncovered_id, BTreeMap::from([(uncovered_uri.clone(), 1)]),),
        ));
        assert!(!state.focused_worker_result_is_obsolete(
            &unrelated,
            &unrelated_uri,
            &Snapshot::new(unrelated_id, BTreeMap::from([(unrelated_uri.clone(), 1)]),),
        ));
    }

    #[test]
    fn successful_full_analysis_rejects_an_older_inflight_focused_result() {
        let mut state = ServerState::new();
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();
        let focused_id = ids.next();
        let full_id = ids.next();
        let file_path = absolute_test_path("/tmp/kio-lsp-covered-inflight.kio");
        let u = crate::lsp::diagnostics::path_to_uri(&file_path, Path::new("/tmp"))
            .expect("focused URI");
        state.open(u.clone(), "module pkg/main;\n".to_owned(), 1);
        state.mark_focused_scheduled(&u, &file_path, focused_id);
        let focused_snapshot = Snapshot::new(focused_id, BTreeMap::from([(u.clone(), 1)]));

        state.store_analysis(
            Path::new("/tmp/kio-lsp-package"),
            Snapshot::new(full_id, BTreeMap::from([(u.clone(), 1)])),
            analysis_for_files(std::slice::from_ref(&file_path)),
        );

        assert!(state.focused_worker_result_is_obsolete(&file_path, &u, &focused_snapshot));
    }

    #[test]
    fn re_open_replaces_text() {
        // Some editors send didOpen twice without a didClose between
        // (workspace reload). The server must tolerate this and end
        // up with the latest text.
        let mut state = ServerState::new();
        let u = uri("file:///tmp/a.kio");
        state.open(u.clone(), "first".to_owned(), 1);
        state.open(u.clone(), "second".to_owned(), 2);
        let doc = state.document(&u).expect("doc");
        assert_eq!(doc.text(), "second");
        assert_eq!(doc.version(), 2);
    }

    #[test]
    fn set_text_invalidates_line_index() {
        let mut doc = OpenDocument::new("hello\nworld".to_owned(), 1);
        // Touch line_index so it's populated.
        let _ = doc.line_index();
        assert!(doc.line_index.is_some());
        doc.set_text("new".to_owned(), 2);
        assert!(doc.line_index.is_none());
        assert_eq!(doc.text(), "new");
        assert_eq!(doc.version(), 2);
    }

    #[test]
    fn set_text_invalidates_parsed_module_cache() {
        let mut doc = OpenDocument::new("module pkg/main;\n".to_owned(), 1);
        assert!(
            doc.with_parsed_module(|module, _| module.items.len())
                .is_some()
        );

        doc.set_text("module pkg/main;\n\nfn broken() -> . { (\n".to_owned(), 2);
        assert!(doc.with_parsed_module(|_, _| ()).is_none());

        doc.set_text("module pkg/main;\n\nfn ok() -> . { () }\n".to_owned(), 3);
        assert!(
            doc.with_parsed_module(|module, _| module.items.len())
                .is_some()
        );
    }

    #[test]
    fn label_reuse_index_parses_once_per_document_version() {
        let source = "module pkg/main; fn run() -> . { () }";
        let mut doc = OpenDocument::new(source.to_owned(), 1);

        let first = doc.label_reuse_index().expect("first index");
        let second = doc.label_reuse_index().expect("cached index");
        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.binding_at(0).is_none());
        assert_eq!(doc.parse_count, 1);

        doc.set_text(
            "module pkg/main; labels { other: . }; labels Row = { other: _ };".to_owned(),
            2,
        );
        let after_edit = doc.label_reuse_index().expect("rebuilt index");
        assert!(!Arc::ptr_eq(&first, &after_edit));
        assert_eq!(doc.parse_count, 2);
    }

    #[test]
    fn splice_replaces_range_and_bumps_version() {
        let mut doc = OpenDocument::new("hello world".to_owned(), 1);
        doc.splice(6, 11, "kio", 2).expect("splice ok");
        assert_eq!(doc.text(), "hello kio");
        assert_eq!(doc.version(), 2);
        assert!(doc.line_index.is_none()); // invalidated
    }

    #[test]
    fn splice_pure_insertion() {
        let mut doc = OpenDocument::new("ab".to_owned(), 1);
        doc.splice(1, 1, "X", 2).expect("insertion ok");
        assert_eq!(doc.text(), "aXb");
        assert_eq!(doc.version(), 2);
    }

    #[test]
    fn splice_pure_deletion() {
        let mut doc = OpenDocument::new("abcde".to_owned(), 1);
        doc.splice(1, 4, "", 2).expect("deletion ok");
        assert_eq!(doc.text(), "ae");
    }

    #[test]
    fn splice_rejects_out_of_bounds() {
        let mut doc = OpenDocument::new("abc".to_owned(), 1);
        assert!(matches!(
            doc.splice(0, 100, "", 2),
            Err(SpliceError::OutOfBounds { .. })
        ));
        // Original text unchanged.
        assert_eq!(doc.text(), "abc");
        assert_eq!(doc.version(), 1);
    }

    #[test]
    fn splice_rejects_inverted_range() {
        let mut doc = OpenDocument::new("abc".to_owned(), 1);
        assert!(matches!(
            doc.splice(2, 1, "", 2),
            Err(SpliceError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn splice_rejects_non_char_boundary() {
        // `é` is two bytes in UTF-8. Splicing at byte 1 (middle of `é`)
        // must fail.
        let mut doc = OpenDocument::new("éx".to_owned(), 1);
        assert!(matches!(
            doc.splice(1, 1, "", 2),
            Err(SpliceError::NotCharBoundary { .. })
        ));
    }

    #[test]
    fn overlay_snapshot_includes_every_open_doc() {
        let mut state = ServerState::new();
        let a = uri("file:///tmp/a.kio");
        let b = uri("file:///tmp/b.kio");
        state.open(a.clone(), "A".to_owned(), 1);
        state.open(b.clone(), "B".to_owned(), 5);
        let snap = state.overlay_snapshot();
        assert_eq!(snap.len(), 2);
        // BTreeMap iteration is sorted by URI string form; assert
        // both entries are present rather than order.
        let map: std::collections::HashMap<_, _> =
            snap.into_iter().map(|(u, t, v)| (u, (t, v))).collect();
        assert_eq!(map[&a], ("A".to_owned(), 1));
        assert_eq!(map[&b], ("B".to_owned(), 5));
    }

    #[test]
    fn file_context_is_part_of_the_cached_parse_identity_without_operator_imports() {
        let mut state = ServerState::new();
        let consumer = uri("file:///workspace/a/b/c.kio");
        let source = "module a/b/c; fn run(value: .) -> . { value }";
        state.open(consumer.clone(), source.to_owned(), 1);
        let first = absolute_test_path("/workspace/a/b/c.kio");
        let second = absolute_test_path("/other/a/b/c.kio");

        assert!(
            state
                .with_overlay_parsed_module_for_file(&consumer, &first, |_, _| ())
                .is_some()
        );
        let document = state.document(&consumer).expect("consumer");
        assert_eq!(document.parse_count, 1);
        assert_eq!(document.file_context_materialization_count, 1);
        assert!(
            state
                .with_overlay_parsed_module_for_file(&consumer, &first, |_, _| ())
                .is_some()
        );
        let document = state.document(&consumer).expect("consumer");
        assert_eq!(document.parse_count, 1);
        assert_eq!(document.file_context_materialization_count, 1);
        assert!(
            state
                .overlay_label_reuse_index_for_file(&consumer, &first)
                .is_some()
        );
        let document = state.document(&consumer).expect("consumer");
        assert_eq!(document.parse_count, 1);
        assert_eq!(
            document.file_context_materialization_count, 1,
            "cache hits and label-index reuse must borrow the cached lazy syntax"
        );
        assert!(
            state
                .with_overlay_parsed_module_for_file(&consumer, &second, |_, _| ())
                .is_some()
        );
        let document = state.document(&consumer).expect("consumer");
        assert_eq!(
            document.parse_count, 2,
            "a distinct real-file context must not reuse a context-free cached parse"
        );
        assert_eq!(document.file_context_materialization_count, 2);
    }

    #[test]
    fn semantic_duplicate_import_errors_do_not_change_cached_syntax() {
        for second_provider in ["binary", "other"] {
            let mut state = ServerState::new();
            let consumer = uri("file:///workspace/app/main.kio");
            for provider in ["binary", "other"] {
                state.open(
                    uri(&format!("file:///workspace/{provider}.kio")),
                    format!("module {provider}; pub fn choose(a: ., b: .) -> . {{ a }} pub op _ ? _ {{ impl choose; }};"),
                    1,
                );
            }
            state.open(
                consumer.clone(),
                format!("module app/main; import binary(op _ ? _); import {second_provider}(op _ ? _); pub fn run(a: ., b: .) -> . {{ a ? b }}"),
                1,
            );
            assert!(
                state
                    .with_overlay_parsed_module(&consumer, |_, _| ())
                    .is_some()
            );
            let failure = import_grammar_analyze(&state, false)
                .expect_err("ordinary duplicate introductions are semantic errors");
            assert!(
                failure.errors.iter().any(|error| matches!(
                    error.error,
                    crate::error::Error::NameRes(_)
                ) && error.error.diag().1.contains("operator")),
                "{:?}",
                failure.errors
            );
            assert!(
                state
                    .with_overlay_parsed_module(&consumer, |_, _| ())
                    .is_some()
            );
            assert_eq!(state.document(&consumer).unwrap().parse_count, 1);
        }
    }

    #[test]
    fn unrelated_overlay_edits_keep_context_free_parse_cache() {
        let mut state = ServerState::new();
        let consumer = uri("file:///workspace/app/main.kio");
        state.open(
            consumer.clone(),
            "module app/main; fn run(value: .) -> . { value }".to_owned(),
            1,
        );
        assert!(
            state
                .with_overlay_parsed_module(&consumer, |_, _| ())
                .is_some()
        );

        state.open(
            uri("file:///workspace/app/unrelated.kio"),
            "module app/unrelated;".to_owned(),
            1,
        );
        assert!(
            state
                .with_overlay_parsed_module(&consumer, |_, _| ())
                .is_some()
        );
        assert_eq!(state.document(&consumer).expect("consumer").parse_count, 1);
    }

    #[test]
    fn drain_published_returns_and_clears() {
        let mut state = ServerState::new();
        let a = uri("file:///tmp/a.kio");
        let b = uri("file:///tmp/b.kio");
        state.mark_published(a.clone());
        state.mark_published(b.clone());
        let drained = state.drain_published();
        assert_eq!(drained.len(), 2);
        assert!(drained.contains(&a));
        assert!(drained.contains(&b));
        // Second drain is empty.
        assert_eq!(state.drain_published().len(), 0);
    }

    #[test]
    fn apply_change_full_replace() {
        let mut doc = OpenDocument::new("old text".to_owned(), 1);
        doc.apply_change(&full_change("brand new"), 2)
            .expect("full replace ok");
        assert_eq!(doc.text(), "brand new");
        assert_eq!(doc.version(), 2);
    }

    #[test]
    fn apply_change_incremental_replace() {
        // Replace "world" with "kio" in "hello\nworld".
        let mut doc = OpenDocument::new("hello\nworld".to_owned(), 1);
        doc.apply_change(&range_change(1, 0, 1, 5, "kio"), 2)
            .expect("incremental replace ok");
        assert_eq!(doc.text(), "hello\nkio");
        assert_eq!(doc.version(), 2);
    }

    #[test]
    fn apply_change_insertion() {
        // Insert "X" between 'a' and 'b'.
        let mut doc = OpenDocument::new("ab".to_owned(), 1);
        doc.apply_change(&range_change(0, 1, 0, 1, "X"), 2)
            .expect("insertion ok");
        assert_eq!(doc.text(), "aXb");
    }

    #[test]
    fn apply_change_deletion() {
        // Delete "bcd" from "abcde".
        let mut doc = OpenDocument::new("abcde".to_owned(), 1);
        doc.apply_change(&range_change(0, 1, 0, 4, ""), 2)
            .expect("deletion ok");
        assert_eq!(doc.text(), "ae");
    }

    #[test]
    fn apply_change_multi_line_replacement() {
        // Replace lines 0-1 ("hello\nworld") with "x".
        let mut doc = OpenDocument::new("hello\nworld\ntail".to_owned(), 1);
        doc.apply_change(&range_change(0, 0, 1, 5, "x"), 2)
            .expect("multi-line replace ok");
        assert_eq!(doc.text(), "x\ntail");
    }

    #[test]
    fn apply_change_with_inserted_newline() {
        // Insert a newline in "abcd" before 'c': "ab\ncd".
        let mut doc = OpenDocument::new("abcd".to_owned(), 1);
        doc.apply_change(&range_change(0, 2, 0, 2, "\n"), 2)
            .expect("insert newline ok");
        assert_eq!(doc.text(), "ab\ncd");
    }

    #[test]
    fn apply_change_sequence_uses_post_edit_positions() {
        // LSP says: in a multi-event payload, positions in event N+1
        // are interpreted against the document state after event N.
        // Verify by chaining two edits.
        let mut doc = OpenDocument::new("abcd".to_owned(), 1);
        // Edit 1: replace 'b' with "XY", giving "aXYcd".
        doc.apply_change(&range_change(0, 1, 0, 2, "XY"), 2)
            .expect("edit 1 ok");
        assert_eq!(doc.text(), "aXYcd");
        // Edit 2: position (0,3) is now 'c' (one past the inserted
        // "XY"). Replace 'c' with 'Z'.
        doc.apply_change(&range_change(0, 3, 0, 4, "Z"), 3)
            .expect("edit 2 ok");
        assert_eq!(doc.text(), "aXYZd");
    }

    #[test]
    fn apply_change_full_replace_with_explicit_range_none() {
        // Even with `range_length: Some(_)` and `range: None`, the
        // full-replace branch wins (the deprecated `range_length`
        // field is ignored).
        let mut doc = OpenDocument::new("old".to_owned(), 1);
        let change = TextDocumentContentChangeEvent {
            range: None,
            range_length: Some(3),
            text: "new".to_owned(),
        };
        doc.apply_change(&change, 2).expect("full replace ok");
        assert_eq!(doc.text(), "new");
    }

    #[test]
    fn open_documents_lists_open_uris() {
        let mut state = ServerState::new();
        let u = uri("file:///tmp/a.kio");
        assert_eq!(state.open_documents().count(), 0);
        state.open(u.clone(), "x".to_owned(), 1);
        assert_eq!(state.open_documents().count(), 1);
        state.close(&u);
        assert_eq!(state.open_documents().count(), 0);
    }

    #[test]
    fn scheduled_snapshot_marks_older_result_obsolete() {
        let mut state = ServerState::new();
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();
        let older = ids.next();
        let newer = ids.next();
        let package_root = PathBuf::from("/tmp/pkg");

        state.mark_scheduled(&package_root, newer);

        assert!(state.worker_result_is_obsolete(&package_root, older));
        assert!(!state.worker_result_is_obsolete(&package_root, newer));
        assert!(!state.worker_result_is_obsolete(Path::new("/tmp/other"), older));
    }
}
