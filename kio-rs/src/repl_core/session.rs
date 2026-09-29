//! `kio repl` session state — the loaded-module set, the current
//! pointer, and the cached typed analysis.
//!
//! ## Model
//!
//! A REPL session inspects the modules in a directory (a `*.pkg.kio`
//! package file is optional). The directory's module tree is
//! type-checked **as a whole** on every refresh (the REPL
//! reuses the LSP's [`crate::cmd::check::analyze_workspace_at_with_overlay_lsp`]
//! analysis), so "loading" a module is not a compilation event — it
//! is a *focus* decision. [`Session`] tracks:
//!
//! - **Loaded modules** — the subset of the directory's modules the
//!   user has brought into view. Each entry is [`Explicit`] (named
//!   in a `:load`) or [`Implicit`] (pulled in because some explicit
//!   module's `import` clause depends on it). An implicit entry records
//!   the explicit module that pulled it in.
//! - **Current module** — the most recently `:load`ed explicit
//!   module. Meta-commands resolve bare names through its view;
//!   `:mods` marks it. `None` right after `kio repl` opens or after
//!   `:reset`.
//! - **Analysis** — the most recent successful whole-tree
//!   typecheck. Holds the position index the type / reference
//!   queries read.
//!
//! ## Why whole-tree analysis
//!
//! `:load X/a/b` could in principle type just `X/a/b` and its
//! `import`-closure. But the directory's module tree is already a coherent unit the
//! LSP analyzer types in one pass, and Kio's open-world property
//! means a module's meaning never depends on which *other* modules
//! are loaded. Typing the whole package and filtering the loaded
//! set in the session is simpler and cannot disagree with `kio
//! check`. The `[Explicit]` / `[Implicit]` bookkeeping is then
//! purely a presentation concern (`:mods`, `:unload` cascades).

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::ast::{ImportKind, Module, Surface};
use crate::cmd::check::{AnalysisFailure, LspAnalysis, analyze_workspace_at_with_overlay_lsp};
use crate::error::Error;
use crate::package_collection::SourceOverlay;
use crate::pass::resolve::LocatedError;
use crate::span::Span;

/// How a module came to be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadKind {
    /// The user named this module in a `:load`.
    Explicit,
    /// Pulled in because an explicit module's `import` closure depends
    /// on it. Carries the slash path of the explicit module that
    /// (transitively) pulled it in — `:mods` shows this as
    /// `X/dep (via X/a/b)`, and `:unload` of the referent
    /// cascade-removes this entry when nothing else needs it.
    Implicit { via: String },
}

/// One loaded module: its slash path, the file it was read from,
/// the parsed Surface AST, and how it was loaded.
#[derive(Debug, Clone)]
pub struct LoadedModule {
    /// Slash module path (`X/a/b`).
    pub path: String,
    /// Absolute path to the `.kio` file.
    pub file_path: PathBuf,
    /// The parsed Surface module. Re-parsed on every refresh so
    /// `:source` / `:doc` / `:ls` see the file's current contents.
    pub module: Module<Surface>,
    /// Whether the user loaded this explicitly or it was an
    /// implicit `import`-closure dependency.
    pub kind: LoadKind,
    /// Monotonic load sequence number. The most-recently-loaded
    /// **explicit** module is the current module; on `:unload` of
    /// the current module the highest-`seq` remaining explicit
    /// module takes over.
    pub seq: u64,
}

impl LoadedModule {
    /// Whether this entry is an explicit load.
    pub fn is_explicit(&self) -> bool {
        self.kind == LoadKind::Explicit
    }
}

/// Cheap "did this file's content change?" key for the auto-reload
/// path. mtime is checked first (a `stat` per file, no I/O of the
/// contents); when mtime moves but content is the same — coarse
/// filesystem clocks, atomic-rename saves that re-stamp mtime,
/// formatter passes that overwrite a file with identical bytes — the
/// hash compare second-guesses it and a phantom reload is skipped.
///
/// `content_hash` uses [`std::collections::hash_map::DefaultHasher`];
/// cryptographic strength is unnecessary because the worst-case
/// failure mode is a missed reload of a file whose hash happens to
/// collide with its previous version, which is astronomically
/// unlikely for ordinary `.kio` source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    mtime: SystemTime,
    content_hash: u64,
}

/// The REPL session: loaded modules, the current pointer, the
/// package root, and the cached analysis.
pub struct Session {
    /// Absolute path to the package root (the directory holding
    /// `<X>.pkg.kio`).
    package_root: PathBuf,
    /// Source overlay used for analysis. Empty for disk-backed terminal
    /// sessions; complete for browser/in-memory sessions.
    source_overlay: SourceOverlay,
    /// Loaded modules keyed by slash path.
    loaded: BTreeMap<String, LoadedModule>,
    /// Slash path of the current module, or `None` when no explicit
    /// module is loaded.
    current: Option<String>,
    /// Next load sequence number.
    next_seq: u64,
    /// Most recent successful whole-tree analysis.
    analysis: Option<LspAnalysis>,
    /// Monotonic counter for naming the synthetic functions the
    /// expression-query path appends to a module overlay. Per-session
    /// (not per-turn) so a synthetic name is unique across the whole
    /// session — handy when debugging an overlay's typecheck.
    next_expr_counter: u64,
    /// Per-`.kio`-file fingerprint as of the last successful
    /// [`Self::refresh`]. The auto-reload path consults this from
    /// [`Self::detect_source_changes`] so a watcher event that
    /// doesn't correspond to a real content change (a touch, an
    /// atomic re-write of identical bytes) is filtered out rather
    /// than driving a redundant re-typecheck. Keyed by the same path
    /// shape `analysis.sources` carries.
    fingerprints: BTreeMap<PathBuf, FileFingerprint>,
}

impl Session {
    /// Open a fresh session rooted at `package_root`. No modules are
    /// loaded; no current module.
    pub fn new(package_root: PathBuf) -> Self {
        Self {
            package_root,
            source_overlay: SourceOverlay::empty(),
            loaded: BTreeMap::new(),
            current: None,
            next_seq: 0,
            analysis: None,
            next_expr_counter: 0,
            fingerprints: BTreeMap::new(),
        }
    }

    /// Open a session over a complete in-memory source tree.
    pub fn new_in_memory(package_root: PathBuf, files: BTreeMap<PathBuf, String>) -> Self {
        Self {
            package_root: package_root.clone(),
            source_overlay: SourceOverlay::complete(package_root, files),
            loaded: BTreeMap::new(),
            current: None,
            next_seq: 0,
            analysis: None,
            next_expr_counter: 0,
            fingerprints: BTreeMap::new(),
        }
    }

    /// Hand out the next starting value for the expression query's
    /// collision-free synthetic-function name search.
    pub fn next_expr_counter(&mut self) -> u64 {
        let n = self.next_expr_counter;
        self.next_expr_counter += 1;
        n
    }

    /// The package root directory.
    pub fn package_root(&self) -> &Path {
        &self.package_root
    }

    pub fn source_overlay(&self) -> &SourceOverlay {
        &self.source_overlay
    }

    /// The current module's slash path, if any.
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// The cached analysis, if a successful typecheck has run.
    pub fn analysis(&self) -> Option<&LspAnalysis> {
        self.analysis.as_ref()
    }

    /// Look up a loaded module by slash path.
    pub fn module(&self, path: &str) -> Option<&LoadedModule> {
        self.loaded.get(path)
    }

    /// Iterate loaded modules in load order (lowest `seq` first).
    pub fn modules_in_load_order(&self) -> Vec<&LoadedModule> {
        let mut v: Vec<&LoadedModule> = self.loaded.values().collect();
        v.sort_by_key(|m| m.seq);
        v
    }

    /// Iterate loaded modules by slash path (the `BTreeMap` order).
    pub fn loaded_paths(&self) -> impl Iterator<Item = &str> {
        self.loaded.keys().map(String::as_str)
    }

    /// Whether any module is loaded.
    pub fn is_empty(&self) -> bool {
        self.loaded.is_empty()
    }

    /// Drop every loaded module and clear the current pointer
    /// (`:reset`). The cached analysis is dropped too — a later
    /// `:load` rebuilds it. The on-disk history file is untouched
    /// (that is the line reader's concern).
    pub fn reset(&mut self) {
        self.loaded.clear();
        self.current = None;
        self.analysis = None;
        self.fingerprints.clear();
    }

    /// Re-run the whole-tree analysis and re-parse every loaded
    /// module's source from disk.
    ///
    /// Used by `:load` (after staging new modules) and by the
    /// auto-reload path (after a watched file changes). On success
    /// the cached analysis and every loaded module's AST reflect the
    /// current file contents; on failure the analysis is left as-is
    /// and the error is returned for the caller to print — the
    /// session keeps the prior version, per the auto-reload contract.
    ///
    /// Also rebuilds the [`Self::fingerprints`] cache from the fresh
    /// analysis's source map — the auto-reload path's content-change
    /// detector reads it back on the next watcher tick.
    pub fn refresh(&mut self) -> Result<(), AnalysisFailure> {
        let analysis =
            analyze_workspace_at_with_overlay_lsp(&self.package_root, &self.source_overlay)?;
        // Re-parse every loaded module from the analysis's source map
        // so `:source` / `:doc` / `:ls` see current contents.
        let mut reparsed = Vec::with_capacity(self.loaded.len());
        for (path, entry) in &self.loaded {
            let Some(source) = analysis_source(&analysis, &entry.file_path) else {
                return Err(AnalysisFailure::from_error(
                    LocatedError {
                        file_path: entry.file_path.clone(),
                        error: Error::import(
                            Span::new(0, 0),
                            format!("cannot read source for loaded module `{path}`"),
                        ),
                    },
                    analysis.sources.clone(),
                ));
            };
            let module = crate::pass::parser::parse(source).map_err(|error| {
                AnalysisFailure::from_error(
                    LocatedError {
                        file_path: entry.file_path.clone(),
                        error,
                    },
                    analysis.sources.clone(),
                )
            })?;
            reparsed.push((path.clone(), module));
        }
        for (path, module) in reparsed {
            self.loaded
                .get_mut(&path)
                .expect("reparsed loaded module remains present")
                .module = module;
        }
        // Refresh the fingerprint cache from the analysis's source
        // map — the source text is in hand, only the mtime needs a
        // `stat`. A file whose mtime can't be read drops out of the
        // cache (a subsequent change-check then treats it as changed
        // and forces a refresh, which is the right failure mode).
        let mut next: BTreeMap<PathBuf, FileFingerprint> = BTreeMap::new();
        for (path, source) in &analysis.sources {
            if let Some(fp) = fingerprint_for(path, source) {
                next.insert(path.clone(), fp);
            }
        }
        self.fingerprints = next;
        self.analysis = Some(analysis);
        Ok(())
    }

    /// Walk the package's known `.kio` files and report which (if
    /// any) have changed since the last successful [`Self::refresh`].
    /// Returns the set of slash module paths whose source changed.
    ///
    /// **Two-layer detection.** Each file is checked first by
    /// [`std::fs::metadata`] mtime — equality with the cached
    /// fingerprint short-circuits to "unchanged" without touching the
    /// contents. When mtime moves (or the file is missing from the
    /// cache, or `stat` fails) the file's bytes are read and hashed
    /// against the cached content hash; equality there refreshes the
    /// cached mtime in place so the next tick is cheap, and the file
    /// is treated as unchanged. The two layers between them filter
    /// out (a) bare touches, (b) editor saves that bump mtime without
    /// changing bytes (formatter passes, atomic-rename of identical
    /// content), and (c) coarse-mtime races where two saves within
    /// one filesystem-clock tick can otherwise be missed.
    ///
    /// **Coverage.** The file set is the analysis's `sources` map —
    /// every `.kio` file the last successful walk found in the
    /// package, not just the currently-loaded subset. A new file that
    /// appears after the last refresh shows up here (it has no cache
    /// entry, so it's treated as changed); a tracked file that
    /// disappears from disk is also reported (forces the refresh,
    /// which then surfaces the missing-file error through the normal
    /// diagnostic path). An empty analysis (no successful refresh
    /// yet) returns an empty set.
    pub fn detect_source_changes(&mut self) -> BTreeSet<String> {
        let Some(analysis) = self.analysis.as_ref() else {
            return BTreeSet::new();
        };

        let mut changed_files: BTreeSet<PathBuf> = BTreeSet::new();
        let mut refreshed_mtimes: Vec<(PathBuf, SystemTime)> = Vec::new();

        // Phase 1: enumerate the known package `.kio` files. The
        // analysis source map is the authoritative file set — every
        // file the last successful walk found, keyed by the same path
        // shape the fingerprint cache uses.
        for path in analysis.sources.keys() {
            let cached = self.fingerprints.get(path).copied();
            let on_disk_mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();

            match (cached, on_disk_mtime) {
                // Cache hit and mtime matches — skip without I/O.
                (Some(fp), Some(mt)) if mt == fp.mtime => {}
                // Cache hit but mtime moved — fall back to content
                // hash. If content is unchanged, refresh the cached
                // mtime so the next tick is cheap again.
                (Some(fp), Some(mt)) => {
                    let new_hash = std::fs::read(path).ok().map(|bytes| hash_bytes(&bytes));
                    if new_hash == Some(fp.content_hash) {
                        refreshed_mtimes.push((path.clone(), mt));
                    } else {
                        changed_files.insert(path.clone());
                    }
                }
                // Cache hit but file unreadable — treat as changed so
                // the refresh surfaces the missing-file error.
                (Some(_), None) => {
                    changed_files.insert(path.clone());
                }
                // No cache entry — a file the previous refresh
                // didn't know about. Treat as changed.
                (None, _) => {
                    changed_files.insert(path.clone());
                }
            }
        }

        // Apply the in-place mtime refreshes recorded above.
        for (path, mt) in refreshed_mtimes {
            if let Some(fp) = self.fingerprints.get_mut(&path) {
                fp.mtime = mt;
            }
        }

        // Phase 2: map changed file paths to slash module paths.
        // `file_to_module` is keyed by canonical paths; the analysis
        // `sources` map may carry either shape. Tolerate the split.
        let mut changed_modules = BTreeSet::new();
        for path in &changed_files {
            if let Some(module_path) = module_path_for_file(analysis, path) {
                changed_modules.insert(module_path);
            }
        }
        changed_modules
    }

    /// Resolve a slash module path to its `.kio` file within the
    /// package, using the analysis's `file_to_module` map.
    ///
    /// Returns `None` when no analysis is cached or the path names no
    /// module in the package.
    pub fn module_file(&self, module_path: &str) -> Option<PathBuf> {
        let analysis = self.analysis.as_ref()?;
        analysis
            .file_to_module
            .iter()
            .find(|(_, mp)| mp.as_str() == module_path)
            .map(|(fp, _)| fp.clone())
    }

    /// Every module path the package defines (whether loaded or not).
    /// Empty when no analysis is cached.
    pub fn package_module_paths(&self) -> Vec<String> {
        match &self.analysis {
            Some(a) => {
                let mut v: Vec<String> = a.file_to_module.values().cloned().collect();
                v.sort();
                v.dedup();
                v
            }
            None => Vec::new(),
        }
    }

    /// Source text of a file from the cached analysis's source map.
    pub fn source_of(&self, file_path: &Path) -> Option<&str> {
        let analysis = self.analysis.as_ref()?;
        analysis_source(analysis, file_path)
    }

    /// Stage a set of modules as loaded and commit them.
    ///
    /// `explicit` is the slash path the user named; `closure` is the
    /// full set of slash paths to load (the explicit module plus its
    /// `import`-closure deps). Each path is read from disk, parsed, and
    /// inserted. Already-loaded modules are updated in place:
    ///
    /// - An already-loaded module re-read by an explicit `:load` of
    ///   itself becomes (or stays) explicit and gets a fresh `seq`.
    /// - A module that was explicit and is now only an implicit dep
    ///   keeps its explicit status (a user-loaded module is never
    ///   silently demoted).
    ///
    /// After staging, `current` is set to `explicit`. The caller has
    /// already validated (via [`Self::plan_load`]) that every path
    /// resolves, so this method does not fail.
    pub fn commit_load(&mut self, explicit: &str, closure: &[StagedModule]) {
        for staged in closure {
            let is_the_explicit = staged.path == explicit;
            match self.loaded.get_mut(&staged.path) {
                Some(existing) => {
                    existing.module = staged.module.clone();
                    existing.file_path = staged.file_path.clone();
                    if is_the_explicit {
                        existing.kind = LoadKind::Explicit;
                        existing.seq = self.next_seq;
                        self.next_seq += 1;
                    } else if existing.kind != LoadKind::Explicit {
                        // Refresh the `via` referent for an implicit
                        // entry — the latest explicit load owns it.
                        existing.kind = LoadKind::Implicit {
                            via: explicit.to_owned(),
                        };
                    }
                }
                None => {
                    let kind = if is_the_explicit {
                        LoadKind::Explicit
                    } else {
                        LoadKind::Implicit {
                            via: explicit.to_owned(),
                        }
                    };
                    let seq = self.next_seq;
                    self.next_seq += 1;
                    self.loaded.insert(
                        staged.path.clone(),
                        LoadedModule {
                            path: staged.path.clone(),
                            file_path: staged.file_path.clone(),
                            module: staged.module.clone(),
                            kind,
                            seq,
                        },
                    );
                }
            }
        }
        self.current = Some(explicit.to_owned());
    }

    /// Remove `target` and every implicit dep that becomes orphaned.
    ///
    /// `target` must be a loaded explicit module — the caller checks
    /// this and the "no other explicit module references it" rule
    /// before calling. After removing `target`, every implicit entry
    /// whose `via` referent is no longer loaded **and** that no
    /// remaining explicit module's `import`-closure still reaches is
    /// dropped too.
    ///
    /// If `target` was the current module, the current pointer falls
    /// back to the highest-`seq` remaining explicit module, or to
    /// `None` when none remain.
    pub fn unload(&mut self, target: &str) {
        self.loaded.remove(target);

        // Recompute the implicit-closure of the remaining explicit
        // modules; drop any implicit entry not in it.
        let still_reachable = self.implicit_closure_of_explicits();
        self.loaded
            .retain(|path, entry| entry.is_explicit() || still_reachable.contains(path));

        if self.current.as_deref() == Some(target) {
            self.current = self
                .loaded
                .values()
                .filter(|m| m.is_explicit())
                .max_by_key(|m| m.seq)
                .map(|m| m.path.clone());
        }
    }

    /// The set of module paths reachable through the `import`-closure of
    /// every currently-loaded explicit module. Used by [`Self::unload`]
    /// to decide which implicit entries survive.
    fn implicit_closure_of_explicits(&self) -> std::collections::BTreeSet<String> {
        let mut reached = std::collections::BTreeSet::new();
        let mut frontier: Vec<String> = self
            .loaded
            .values()
            .filter(|m| m.is_explicit())
            .map(|m| m.path.clone())
            .collect();
        while let Some(path) = frontier.pop() {
            // Use the loaded entry's AST if present, else skip — a
            // dep not currently loaded contributes nothing here.
            let Some(entry) = self.loaded.get(&path) else {
                continue;
            };
            for dep in import_clause_targets(&entry.module) {
                if reached.insert(dep.clone()) {
                    frontier.push(dep);
                }
            }
        }
        reached
    }

    /// Whether any **explicit** module other than `target` has
    /// `target` in its `import`-closure (transitively). Used to reject
    /// `:unload <target>` when removing it would orphan a module
    /// another explicit load still depends on.
    pub fn explicit_referrers(&self, target: &str) -> Vec<String> {
        let mut referrers = Vec::new();
        for m in self.loaded.values() {
            if !m.is_explicit() || m.path == target {
                continue;
            }
            if self.import_closure(&m.path).contains(target) {
                referrers.push(m.path.clone());
            }
        }
        referrers.sort();
        referrers
    }

    /// The transitive `import`-closure of `start` over the *loaded*
    /// module set. `start` itself is not included.
    fn import_closure(&self, start: &str) -> std::collections::BTreeSet<String> {
        let mut reached = std::collections::BTreeSet::new();
        let mut frontier = vec![start.to_owned()];
        while let Some(path) = frontier.pop() {
            let Some(entry) = self.loaded.get(&path) else {
                continue;
            };
            for dep in import_clause_targets(&entry.module) {
                if dep != start && reached.insert(dep.clone()) {
                    frontier.push(dep);
                }
            }
        }
        reached
    }
}

/// A module staged for loading: its slash path, file path, and
/// freshly-parsed AST. Produced by the `:load` planner; consumed by
/// [`Session::commit_load`].
#[derive(Debug, Clone)]
pub struct StagedModule {
    pub path: String,
    pub file_path: PathBuf,
    pub module: Module<Surface>,
}

/// The slash module-path targets of a module's `import` clauses.
///
/// Selective (`import x/y(a);`) and qualified (`import x/y as m;`)
/// clauses both name a `from` / target module path; intrinsics
/// (`import __intrinsics__;`) name no module. The `:load` planner
/// resolves each path against the package's actual module set and
/// only auto-loads modules in the current package.
pub fn import_clause_targets(module: &Module<Surface>) -> Vec<String> {
    let mut out = Vec::new();
    for u in &module.imports {
        match &u.kind {
            ImportKind::Selective { from, .. } => out.push(module_key(from)),
            ImportKind::Qualified { path, .. } => out.push(module_key(path)),
            ImportKind::Intrinsics => {}
            ImportKind::Comptime => {}
        }
    }
    out
}

fn module_key(path: &crate::ast::ModulePath) -> String {
    path.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// Look up a file's source text in an [`LspAnalysis`] source map,
/// tolerating the canonical / non-canonical path-key split the LSP
/// analysis carries.
fn analysis_source<'a>(analysis: &'a LspAnalysis, file_path: &Path) -> Option<&'a str> {
    if let Some(s) = analysis.sources.get(file_path) {
        return Some(s.as_str());
    }
    if let Ok(canonical) = std::fs::canonicalize(file_path)
        && let Some(s) = analysis.sources.get(&canonical)
    {
        return Some(s.as_str());
    }
    None
}

/// Hash a byte slice with [`std::collections::hash_map::DefaultHasher`].
/// Used by the fingerprint cache to second-guess mtime equality —
/// no cryptographic strength needed (see [`FileFingerprint`]).
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Build a [`FileFingerprint`] for `path` given its current source
/// text. `stat`s the file for its mtime; `None` when that `stat`
/// fails (the path then drops out of the cache and is treated as
/// changed on the next tick).
fn fingerprint_for(path: &Path, source: &str) -> Option<FileFingerprint> {
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    Some(FileFingerprint {
        mtime,
        content_hash: hash_bytes(source.as_bytes()),
    })
}

/// Resolve a file path to its slash module path via the analysis's
/// `file_to_module` map. `file_to_module` is keyed by canonical paths
/// while the source-map / fingerprint side may carry either shape, so
/// try both the path as-given and its canonicalized form.
fn module_path_for_file(analysis: &LspAnalysis, file_path: &Path) -> Option<String> {
    if let Some(p) = analysis.file_to_module.get(file_path) {
        return Some(p.clone());
    }
    if let Ok(canonical) = std::fs::canonicalize(file_path)
        && let Some(p) = analysis.file_to_module.get(&canonical)
    {
        return Some(p.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Module<Surface> {
        crate::pass::parser::parse(src).expect("test module parses")
    }

    fn staged(path: &str, src: &str) -> StagedModule {
        StagedModule {
            path: path.to_owned(),
            file_path: PathBuf::from(format!("/tmp/{path}.kio")),
            module: parse(src),
        }
    }

    #[test]
    fn fresh_session_has_no_current_and_is_empty() {
        let s = Session::new(PathBuf::from("/tmp/pkg"));
        assert!(s.is_empty());
        assert_eq!(s.current(), None);
    }

    #[test]
    fn refresh_reparses_loaded_imported_operator_module() {
        let root = tempfile::tempdir().expect("temporary package root");
        std::fs::write(
            root.path().join("app.pkg.kio"),
            "package app;\n\nbridge {\n  app/**;\n}\n",
        )
        .expect("package file");
        let module_dir = root.path().join("app");
        std::fs::create_dir_all(&module_dir).expect("module directory");
        std::fs::write(
            module_dir.join("syntax.kio"),
            "module app/syntax;\npub fn choose(left: ., right: .) -> . { left }\npub op ? _ : _ { impl choose; };\n",
        )
        .expect("provider module");
        let main_path = module_dir.join("main.kio");
        std::fs::write(
            &main_path,
            "module app/main;\nimport app/syntax(op ? _ : _);\npub fn before() -> . { ? () : () }\n",
        )
        .expect("consumer module");

        let mut session = Session::new(root.path().to_path_buf());
        session.refresh().expect("initial analysis");
        let file_path = session.module_file("app/main").expect("consumer file");
        let source = session.source_of(&file_path).expect("consumer source");
        let module = crate::pass::parser::parse(source).expect("provider-aware initial parse");
        session.commit_load(
            "app/main",
            &[StagedModule {
                path: "app/main".to_owned(),
                file_path,
                module,
            }],
        );

        std::fs::write(
            &main_path,
            "module app/main;\nimport app/syntax(op ? _ : _);\npub fn after() -> . { ? () : () }\n",
        )
        .expect("updated consumer module");
        session.refresh().expect("refreshed analysis");
        let loaded = &session.module("app/main").expect("loaded consumer").module;
        assert!(
            loaded
                .items
                .iter()
                .any(|item| matches!(item, crate::ast::Item::FnDef(def) if def.name == "after"))
        );
        assert!(
            !loaded
                .items
                .iter()
                .any(|item| matches!(item, crate::ast::Item::FnDef(def) if def.name == "before"))
        );
    }

    #[test]
    fn commit_load_sets_current_to_explicit() {
        let mut s = Session::new(PathBuf::from("/tmp/pkg"));
        let m = staged("pkg/a", "module pkg/a;\npub fn f() -> . { () }\n");
        s.commit_load("pkg/a", &[m]);
        assert_eq!(s.current(), Some("pkg/a"));
        assert!(s.module("pkg/a").unwrap().is_explicit());
    }

    #[test]
    fn implicit_dep_records_its_referent() {
        let mut s = Session::new(PathBuf::from("/tmp/pkg"));
        let main = staged(
            "pkg/main",
            "module pkg/main;\nimport pkg/dep(f);\npub fn g() -> . { () }\n",
        );
        let dep = staged("pkg/dep", "module pkg/dep;\npub fn f() -> . { () }\n");
        s.commit_load("pkg/main", &[main, dep]);
        let dep_entry = s.module("pkg/dep").unwrap();
        assert_eq!(
            dep_entry.kind,
            LoadKind::Implicit {
                via: "pkg/main".to_owned()
            }
        );
        // The explicit module is current.
        assert_eq!(s.current(), Some("pkg/main"));
    }

    #[test]
    fn reload_of_explicit_refreshes_seq_and_keeps_current() {
        let mut s = Session::new(PathBuf::from("/tmp/pkg"));
        s.commit_load("pkg/a", &[staged("pkg/a", "module pkg/a;\n")]);
        s.commit_load("pkg/b", &[staged("pkg/b", "module pkg/b;\n")]);
        assert_eq!(s.current(), Some("pkg/b"));
        // Re-load `pkg/a`: it becomes current again.
        s.commit_load("pkg/a", &[staged("pkg/a", "module pkg/a;\n")]);
        assert_eq!(s.current(), Some("pkg/a"));
    }

    #[test]
    fn unload_current_falls_back_to_recent_explicit() {
        let mut s = Session::new(PathBuf::from("/tmp/pkg"));
        s.commit_load("pkg/a", &[staged("pkg/a", "module pkg/a;\n")]);
        s.commit_load("pkg/b", &[staged("pkg/b", "module pkg/b;\n")]);
        // Current is pkg/b; unload it -> falls back to pkg/a.
        s.unload("pkg/b");
        assert_eq!(s.current(), Some("pkg/a"));
        assert!(s.module("pkg/b").is_none());
    }

    #[test]
    fn unload_last_explicit_clears_current() {
        let mut s = Session::new(PathBuf::from("/tmp/pkg"));
        s.commit_load("pkg/a", &[staged("pkg/a", "module pkg/a;\n")]);
        s.unload("pkg/a");
        assert_eq!(s.current(), None);
        assert!(s.is_empty());
    }

    #[test]
    fn unload_cascades_orphaned_implicit_dep() {
        let mut s = Session::new(PathBuf::from("/tmp/pkg"));
        let main = staged(
            "pkg/main",
            "module pkg/main;\nimport pkg/dep(f);\npub fn g() -> . { () }\n",
        );
        let dep = staged("pkg/dep", "module pkg/dep;\npub fn f() -> . { () }\n");
        s.commit_load("pkg/main", &[main, dep]);
        // pkg/dep is an orphan once pkg/main goes.
        s.unload("pkg/main");
        assert!(s.is_empty(), "orphaned implicit dep should cascade-remove");
    }

    #[test]
    fn unload_keeps_implicit_dep_still_used_by_another_explicit() {
        let mut s = Session::new(PathBuf::from("/tmp/pkg"));
        let a = staged(
            "pkg/a",
            "module pkg/a;\nimport pkg/dep(f);\npub fn g() -> . { () }\n",
        );
        let dep = staged("pkg/dep", "module pkg/dep;\npub fn f() -> . { () }\n");
        s.commit_load("pkg/a", &[a, dep.clone()]);
        let b = staged(
            "pkg/b",
            "module pkg/b;\nimport pkg/dep(f);\npub fn h() -> . { () }\n",
        );
        s.commit_load("pkg/b", &[b, dep]);
        // Unload pkg/a — pkg/dep is still needed by pkg/b, so it stays.
        s.unload("pkg/a");
        assert!(s.module("pkg/dep").is_some());
        assert!(s.module("pkg/b").is_some());
    }

    #[test]
    fn explicit_referrers_flags_dependents() {
        let mut s = Session::new(PathBuf::from("/tmp/pkg"));
        let a = staged(
            "pkg/a",
            "module pkg/a;\nimport pkg/dep(f);\npub fn g() -> . { () }\n",
        );
        let dep = staged("pkg/dep", "module pkg/dep;\npub fn f() -> . { () }\n");
        s.commit_load("pkg/a", &[a, dep.clone()]);
        // Now load pkg/dep explicitly too.
        s.commit_load("pkg/dep", &[dep]);
        // pkg/a still refers to pkg/dep.
        assert_eq!(s.explicit_referrers("pkg/dep"), vec!["pkg/a".to_owned()]);
    }

    #[test]
    fn reset_clears_everything() {
        let mut s = Session::new(PathBuf::from("/tmp/pkg"));
        s.commit_load("pkg/a", &[staged("pkg/a", "module pkg/a;\n")]);
        s.reset();
        assert!(s.is_empty());
        assert_eq!(s.current(), None);
    }

    #[test]
    fn import_clause_targets_collects_selective_and_qualified() {
        let m = parse(
            "module pkg/main;\nimport pkg/a(f);\nimport pkg/b as b;\nimport __intrinsics__;\npub fn g() -> . { () }\n",
        );
        let targets = import_clause_targets(&m);
        assert!(targets.contains(&"pkg/a".to_owned()));
        assert!(targets.contains(&"pkg/b".to_owned()));
        // Intrinsics import names no module.
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn hash_bytes_is_stable_and_discriminates() {
        // Equal bytes hash the same way; different bytes (almost
        // certainly) don't. `DefaultHasher` is per-process but
        // identical within one session, which is all the fingerprint
        // cache needs.
        let a = hash_bytes(b"module demo/main;\npub fn run() -> . { () }\n");
        let b = hash_bytes(b"module demo/main;\npub fn run() -> . { () }\n");
        let c = hash_bytes(b"module demo/main;\npub fn run() -> . { (()) }\n");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn fingerprint_for_uses_current_mtime() {
        // Writing a file and asking for its fingerprint should yield
        // an mtime within sniffing distance of "now."
        let dir = std::env::temp_dir().join(format!("kio-fp-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("a.kio");
        let content = "module a;\n";
        std::fs::write(&path, content).expect("write");
        let fp = fingerprint_for(&path, content).expect("fingerprint exists");
        assert_eq!(fp.content_hash, hash_bytes(content.as_bytes()));
        // mtime is within the last 10 seconds — generous to cover slow
        // CI hosts.
        let age = std::time::SystemTime::now()
            .duration_since(fp.mtime)
            .unwrap_or_default();
        assert!(age.as_secs() < 10, "mtime should be recent, got {age:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
