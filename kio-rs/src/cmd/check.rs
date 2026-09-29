//! Implementation of `kio check`.
//!
//! Walks the current working directory, finds every `*.kio` source
//! file, and routes each to the matching parser entry point: modules
//! and the package's `<name>.pkg.kio` are parsed and checked here. Prints a
//! `<path>:<line>:<col>: <message>` diagnostic to stderr. On success,
//! exits 0 with no output.
//!
//! The bulk lives in [`compile_package`], which `kio build` also calls to
//! reach a typed [`Package`] before invoking a backend.
//!
//! Both binaries route through this module: `kio` dispatches via
//! [`crate::pass::full::FullPipeline`] (Surface → Desugared → Lowered →
//! Prime), `kio-prime` via [`crate::prime::pipeline::PrimePipeline`]
//! (Surface → Prime, rejecting surface-only forms in the lowering
//! pass). [`compile_package_with`] is the generic entry point;
//! `prime_only` selects the impl.

// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(â¦)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
use crate::path_display::{DisplayPath, display_path_from_root};
#[cfg(feature = "parallel")]
use rayon::prelude::*;
#[cfg(feature = "lsp")]
use std::collections::HashSet;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use std::collections::BTreeMap;

#[cfg(test)]
mod typed_cache_work_counters {
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub(super) struct Counts {
        pub(super) fingerprints: usize,
        pub(super) reachability_roots: usize,
        pub(super) keys: usize,
    }

    thread_local! {
        static COUNTS: Cell<Counts> = const { Cell::new(Counts {
            fingerprints: 0,
            reachability_roots: 0,
            keys: 0,
        }) };
    }

    #[cfg(feature = "surface")]
    pub(super) fn reset() {
        COUNTS.set(Counts::default());
    }

    #[cfg(feature = "surface")]
    pub(super) fn snapshot() -> Counts {
        COUNTS.get()
    }

    pub(super) fn record_fingerprint() {
        COUNTS.with(|cell| {
            let mut counts = cell.get();
            counts.fingerprints += 1;
            cell.set(counts);
        });
    }

    pub(super) fn record_reachability_root() {
        COUNTS.with(|cell| {
            let mut counts = cell.get();
            counts.reachability_roots += 1;
            cell.set(counts);
        });
    }

    pub(super) fn record_key() {
        COUNTS.with(|cell| {
            let mut counts = cell.get();
            counts.keys += 1;
            cell.set(counts);
        });
    }
}

#[cfg(all(test, feature = "surface", feature = "lsp"))]
mod lsp_callable_target_work_counters {
    use std::cell::Cell;

    thread_local! {
        static SCHEMES: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn reset() {
        SCHEMES.set(0);
    }

    pub(super) fn record_scheme() {
        SCHEMES.set(SCHEMES.get() + 1);
    }

    pub(super) fn snapshot() -> usize {
        SCHEMES.get()
    }
}

#[cfg(all(test, feature = "surface", feature = "lsp"))]
mod lsp_declaration_site_work_counters {
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(super) struct Counts {
        pub(super) selective_probes: usize,
        pub(super) declaration_probes: usize,
        pub(super) provider_index_entries: usize,
        pub(super) provider_lookups: usize,
        pub(super) provider_tokenizations: usize,
        pub(super) generated_label_map_builds: usize,
    }

    thread_local! {
        static COUNTS: Cell<Counts> = const { Cell::new(Counts {
            selective_probes: 0,
            declaration_probes: 0,
            provider_index_entries: 0,
            provider_lookups: 0,
            provider_tokenizations: 0,
            generated_label_map_builds: 0,
        }) };
    }

    pub(super) fn reset() {
        COUNTS.set(Counts::default());
    }

    fn update(f: impl FnOnce(&mut Counts)) {
        COUNTS.with(|counts| {
            let mut current = counts.get();
            f(&mut current);
            counts.set(current);
        });
    }

    pub(super) fn record_selective_probe() {
        update(|counts| counts.selective_probes += 1);
    }

    pub(super) fn record_declaration_probe() {
        update(|counts| counts.declaration_probes += 1);
    }

    pub(super) fn record_provider_index_entry() {
        update(|counts| counts.provider_index_entries += 1);
    }

    pub(super) fn record_provider_lookup() {
        update(|counts| counts.provider_lookups += 1);
    }

    pub(super) fn record_provider_tokenization() {
        update(|counts| counts.provider_tokenizations += 1);
    }

    pub(super) fn record_generated_label_map_build() {
        update(|counts| counts.generated_label_map_builds += 1);
    }

    pub(super) fn snapshot() -> Counts {
        COUNTS.get()
    }
}

#[cfg(test)]
mod package_check_cache_work_counters {
    use std::cell::Cell;

    thread_local! {
        static SOURCE_HASHES: Cell<usize> = const { Cell::new(0) };
    }

    #[cfg(feature = "surface")]
    pub(super) fn reset() {
        SOURCE_HASHES.set(0);
    }

    #[cfg(feature = "surface")]
    pub(super) fn snapshot() -> usize {
        SOURCE_HASHES.get()
    }

    pub(super) fn record_source_hash() {
        SOURCE_HASHES.with(|count| count.set(count.get() + 1));
    }
}

use crate::ast::{Module, Prime, Surface};
use crate::cache::keys::{
    DeclaredModulePath, PackageModuleKey, PackageName, PipelineTag, SourceHash, SurfaceFingerprint,
    TypedModuleDependency,
};
use crate::error::Error;
#[cfg(feature = "lsp")]
use crate::error::{Fix, FixEdit};
use crate::exit_code::ExitCode;
use crate::package_collection::{self, PackageCollection, PackageEntry, PackageKey, ParsedPackage};
#[cfg(feature = "surface")]
use crate::pass::full::FullPipeline;
use crate::pass::resolve::{LocatedError, ModuleEntry, Package, PackageFileEntry, ResolvePhase};
use crate::pipeline::Pipeline;
#[cfg(feature = "prime")]
use crate::prime::pipeline::PrimePipeline;

const HELP_TEMPLATE: &str = "\
Usage: kio check [<module>...]

Typecheck the current package.

Walks the cwd's Kio source tree (parse → use resolution → name
resolution → type check) and exits 0 if everything checks.

With no positional argument, the whole package is checked. With one or
more <module> selectors, the package is still checked but the selectors
are validated against its module set. A selector is either a module
path (`pkg/utils/string`) or a filename path (`src/utils.kio`).

Exit codes (per {base}/specs/exit-codes.md): 0 on success; the matching
category code (`1x` parse / use / type / …) on failure; 2 on CLI usage
error (unknown selector, or no .kio files in the current directory).

Options:
  -h, --help    Show this help and exit.

See {base}/specs/cli.md#kio-check-module for full command behavior.";

pub fn run(args: &[String], prime_only: bool) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "{}",
            HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }
    // Validate unknown-flag-looking args up front; positional
    // selectors are accepted but flags aren't (mirrors `kio test`).
    for a in args {
        if a.starts_with('-') {
            eprintln!("error: unknown flag for `kio check`: {a}");
            return ExitCode::Usage;
        }
    }
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return ExitCode::Internal;
        }
    };
    let selectors: Vec<crate::cmd::module_selector::Selector> = args
        .iter()
        .map(|a| crate::cmd::module_selector::parse(a, &cwd))
        .collect();
    // With no positional selector, a directory containing zero `.kio`
    // files is a usage error — there is nothing to check. With explicit
    // selectors a missing file is the separate unknown-selector path.
    if selectors.is_empty()
        && let Err(code) = error_if_no_kio_files(&cwd)
    {
        return code;
    }
    // `kio check` only needs the exit code — it discards the typed
    // workspace. `skip_ok = true` lets the orchestrator skip
    // re-typechecking packages whose package-check cache is
    // still valid (see `compile_workspace_with`); we gate that on
    // [`cache::policy::caches_enabled`] so the package-check
    // cache is consulted only when the process didn't opt out via
    // the `--no-cache` CLI flag. With selectors we force `skip_ok =
    // false` so the typed root package is always materialized for
    // selector validation against its module set.
    //
    // We also force `skip_ok = false` when the cwd holds a `*.sig.kio`
    // changelog: the contract-staleness advisory below needs the typed
    // root materialized to compare the live surface against the sealed
    // contract. A package with no sig keeps the cache fast path
    // (open-world: it builds / checks identically).
    #[cfg(feature = "surface")]
    let has_sig = selectors.is_empty() && cwd_has_sig_file(&cwd);
    #[cfg(not(feature = "surface"))]
    let has_sig = false;
    let skip_ok = selectors.is_empty() && crate::cache::policy::caches_enabled() && !has_sig;
    // Dependencies are materialized at `kio dep fetch`/`update` and committed;
    // analysis walks their re-rooted modules as ordinary source. `check` does
    // not re-materialize.
    let typed = match compile_workspace(prime_only, skip_ok) {
        Ok(typed) => typed,
        Err(code) => return code,
    };
    if !selectors.is_empty() {
        let root = typed
            .root_package
            .expect("compile_workspace passes skip_ok = false when selectors are present");
        let available: Vec<(String, PathBuf)> = root
            .modules()
            .map(|(path_str, entry)| (path_str.to_owned(), entry.file_path.clone()))
            .collect();
        if crate::cmd::module_selector::resolve_against_modules(&available, &selectors).is_err() {
            return ExitCode::Usage;
        }
        return ExitCode::Success;
    }

    // Contract-staleness advisory (no-selector path): if the root
    // package has a `*.sig.kio` and the live surface breaks the sealed
    // contract (unrecorded), warn on stderr without changing the exit
    // code. Silent when there is no sig, the drift is compatible, or the
    // break is already recorded.
    #[cfg(feature = "surface")]
    {
        if let Some(root) = typed.root_package.as_ref() {
            let pkg_dir = root
                .package_file()
                .and_then(|f| f.file_path.parent())
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| cwd.clone());
            crate::cmd::sig::warn_if_unrecorded_breaking(root, &pkg_dir);
        }
    }
    ExitCode::Success
}

/// Whether the cwd subtree holds any `*.sig.kio` changelog. Used to opt
/// the `kio check` no-selector path out of the package-check cache so
/// the contract-staleness advisory has the typed root to compare. A
/// discovery hiccup degrades to `false` (keep the cache fast path) — the
/// advisory is best-effort, never a gate.
#[cfg(feature = "surface")]
fn cwd_has_sig_file(cwd: &Path) -> bool {
    let Ok(roots) = package_collection::discover_package_roots(cwd) else {
        return false;
    };
    roots
        .iter()
        .any(|r| r.dir.join(format!("{}.sig.kio", r.name)).is_file())
}

/// Error when `cwd` holds zero `.kio` files. Both `kio check` and
/// `kio test` call this on their no-selector path so a truly-empty
/// directory exits at the CLI-usage tier (code 2) with a clear
/// message, distinct from the "source present but no modules / no
/// equivs" path. A directory-scan I/O error is not treated as empty —
/// the caller proceeds and the real walk surfaces the genuine
/// diagnostic.
pub(crate) fn error_if_no_kio_files(cwd: &Path) -> Result<(), ExitCode> {
    if let Ok(false) = package_collection::has_kio_files(cwd) {
        eprintln!("error: no .kio files found in the current directory");
        return Err(ExitCode::Usage);
    }
    Ok(())
}

/// Run the full check pipeline (parse → package build → use resolution
/// → cycle check → in-body resolution → type check) over the cwd's
/// source tree. On success returns the typed [`Package`] at the
/// [`Prime`](crate::ast::Prime) phase — elaborator-position elaborations
/// substituted into the AST and validated as Kio' — ready to feed to a
/// backend; on failure prints the diagnostic to stderr and returns the
/// matching exit code.
///
/// `prime_only` selects which [`Pipeline`] impl drives the lowering
/// and typer halves. When `false`, the kio binary's [`FullPipeline`]
/// runs the full-Kio path through `desugar` + `label_elab` +
/// `typecheck_full`. When `true`, the kio-prime binary's
/// [`PrimePipeline`] runs the Kio'-only path through `prime::lower`
/// + `prime::typer`.
pub fn compile_package(prime_only: bool) -> Result<Package<Prime>, ExitCode> {
    // `skip_ok = false`: this entry point's contract is a fully
    // typed root package, so it never skips.
    compile_workspace(prime_only, false).map(|w| {
        w.root_package
            .expect("compile_package passes skip_ok = false, so the root is always typechecked")
    })
}

/// Package-collection-aware compile entry. Returns the typed root
/// [`Package`] alongside any additional packages the collection
/// contains.
///
/// `skip_ok` controls cache-driven typecheck skipping: `kio check`
/// passes `true` (it discards the collection, so a skipped package
/// producing no `Package<Prime>` is harmless); `kio build` passes
/// `false` (it consumes every package's typed form for codegen).
pub fn compile_workspace(
    prime_only: bool,
    skip_ok: bool,
) -> Result<TypedPackageCollection, ExitCode> {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return Err(ExitCode::Internal);
        }
    };
    compile_workspace_at(&cwd, prime_only, skip_ok)
}

/// As [`compile_workspace`], but takes the workspace root directory
/// as an explicit argument instead of consulting the process's
/// current working directory.
///
/// The CLI entry points still read `cwd` and call
/// [`compile_workspace`]; this variant exists so in-process callers
/// (notably `kio doc`'s per-snippet validator) can run multiple
/// concurrent compiles against per-call scratch directories without
/// serializing on process-global `cwd` state.
pub fn compile_workspace_at(
    root: &Path,
    prime_only: bool,
    skip_ok: bool,
) -> Result<TypedPackageCollection, ExitCode> {
    match prime_only {
        #[cfg(feature = "prime")]
        true => compile_workspace_with_at::<PrimePipeline>(root, skip_ok),
        #[cfg(feature = "surface")]
        false => compile_workspace_with_at::<FullPipeline>(root, skip_ok),
        #[cfg(not(all(feature = "surface", feature = "prime")))]
        _ => panic!(
            "compile_workspace_at(prime_only={prime_only}) called in a build that doesn't \
             include the matching feature — the binaries are gated by `required-features` \
             so reaching this branch implies the lib API was called with the wrong flag"
        ),
    }
}

/// As [`compile_workspace_at`], but on failure renders the diagnostic
/// into `buf` (with a trailing newline) instead of printing to stderr.
/// Used by the multi-package fan-out so each package's typecheck
/// diagnostics can be buffered and replayed in input order
/// (`cmd::package_fanout`).
pub fn compile_workspace_at_buffered(
    root: &Path,
    prime_only: bool,
    skip_ok: bool,
    buf: &mut String,
) -> Result<TypedPackageCollection, ExitCode> {
    match prime_only {
        #[cfg(feature = "prime")]
        true => compile_workspace_with_at_buffered::<PrimePipeline>(root, skip_ok, buf),
        #[cfg(feature = "surface")]
        false => compile_workspace_with_at_buffered::<FullPipeline>(root, skip_ok, buf),
        #[cfg(not(all(feature = "surface", feature = "prime")))]
        _ => panic!(
            "compile_workspace_at_buffered(prime_only={prime_only}) called in a build that \
             doesn't include the matching feature — the binaries are gated by \
             `required-features` so reaching this branch implies the lib API was called with \
             the wrong flag"
        ),
    }
}

/// In-process variant of [`compile_workspace_at`]: same pipeline,
/// but failures return the structured [`AnalysisFailure`] instead of
/// printing to stderr. Used by [`crate::lsp`] to publish diagnostics
/// over the LSP protocol.
#[cfg(feature = "surface")]
pub fn analyze_workspace_at(
    root: &Path,
    skip_ok: bool,
) -> Result<TypedPackageCollection, AnalysisFailure> {
    analyze_workspace_with_at::<FullPipeline>(
        root,
        skip_ok,
        &package_collection::SourceOverlay::empty(),
    )
}

/// As [`analyze_workspace_at`], but reads each source through
/// `overlay` first ([`package_collection::SourceOverlay::read`]) before
/// falling back to disk. Used by the LSP server to analyze against
/// the editor's authoritative buffer rather than disk for any URI
/// the editor has open.
#[cfg(feature = "surface")]
pub fn analyze_workspace_at_with_overlay(
    root: &Path,
    skip_ok: bool,
    overlay: &package_collection::SourceOverlay,
) -> Result<TypedPackageCollection, AnalysisFailure> {
    analyze_workspace_with_at::<FullPipeline>(root, skip_ok, overlay)
}

/// LSP-specific analysis result produced by
/// [`analyze_workspace_at_with_overlay_lsp`]. Holds the position-keyed
/// type and binder index from the most-recent successful typecheck,
/// plus the file-path → module-path map the LSP handlers need to
/// translate cursor positions into index lookups.
///
/// Only exists when the `full` and `lsp` features are both active —
/// the `PositionIndex` is a `typecheck_full` (full-only) type and
/// hover/goto/references are LSP-only features.
#[cfg(all(feature = "surface", feature = "lsp"))]
#[derive(Debug)]
pub struct LspAnalysis {
    /// The merged position index from the last successful typecheck.
    /// Keyed by `(module_path, Span)` — same semantics as
    /// [`crate::pass::typecheck_full::PositionIndex`].
    pub position_index: crate::pass::typecheck_full::PositionIndex,
    /// Canonical file path → slash module-path string.
    /// Built from the lowered workspace's `Package::modules()` entries;
    /// the LSP hover / definition / references handlers use this to
    /// convert a URI into the module path key required by the index.
    pub file_to_module: BTreeMap<PathBuf, String>,
    /// Per-file source text accumulated during the analysis (same map
    /// the diagnostic path builds). The LSP position-conversion layer
    /// uses this to build a [`crate::lsp::positions::LineIndex`] for
    /// files that don't have an open overlay document.
    pub sources: HashMap<PathBuf, String>,
    /// Per-file label declaration/reuse lookup derived from the analysis's
    /// surface modules. Closed-document requests query this snapshot instead
    /// of reparsing `sources` at cursor frequency.
    pub label_reuse_indexes:
        HashMap<PathBuf, std::sync::Arc<crate::lsp::label_reuse::LabelReuseIndex>>,
    /// Generated nominal type identities minted by `labels` declarations.
    /// Rename uses this surface provenance to refuse an ordinary identifier
    /// rename that could not update the coupled lowercase label spellings.
    pub(crate) generated_label_nominals: HashSet<(String, String)>,
    /// The root package's source-spelled `Lowered` AST. Source-facing LSP
    /// requests format declarations from this tree; synthesized facts live in
    /// the position index.
    pub root_package_lowered: Package<crate::ast::Lowered>,
    /// LSP-only warnings produced from the clean typed snapshot.
    /// These do not participate in the batch compiler's exit-code
    /// contract; the LSP publishes them as `DiagnosticSeverity::WARNING`
    /// after a successful full-package analysis.
    pub warnings: Vec<LspWarning>,
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_label_reuse_indexes(
    parsed: &package_collection::ParsedPackageCollection,
    surface_tokens: &HashMap<PathBuf, Vec<crate::tokens::ClassifiedToken>>,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Option<HashMap<PathBuf, std::sync::Arc<crate::lsp::label_reuse::LabelReuseIndex>>> {
    let mut indexes = HashMap::new();
    for package in parsed.packages.values() {
        for (path, module) in &package.modules {
            if cancel_token.is_cancelled() {
                return None;
            }
            let Some(tokens) = surface_tokens.get(path) else {
                continue;
            };
            indexes.insert(
                path.clone(),
                std::sync::Arc::new(
                    crate::lsp::label_reuse::LabelReuseIndex::from_module_with_tokens(
                        module, tokens,
                    ),
                ),
            );
        }
    }
    Some(indexes)
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_generated_label_nominals(
    parsed: &package_collection::ParsedPackageCollection,
    only_files: Option<&HashSet<PathBuf>>,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Option<HashSet<(String, String)>> {
    let mut generated = HashSet::new();
    for package in parsed.packages.values() {
        for (file_path, module) in &package.modules {
            if cancel_token.is_cancelled() {
                return None;
            }
            if only_files.is_some_and(|files| !files.contains(file_path)) {
                continue;
            }
            let module_path = module_path_key(&module.path);
            lsp_for_each_surface_labels(module, |labels| {
                for entry in labels
                    .entries
                    .iter()
                    .filter(|entry| !entry.is_reuse_marker())
                {
                    generated.insert((
                        module_path.clone(),
                        crate::ast::mint_label_newtype_name(&entry.name),
                    ));
                }
            });
        }
    }
    Some(generated)
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_for_each_surface_labels<'a>(
    module: &'a Module<Surface>,
    mut visit: impl FnMut(&'a crate::ast::Labels<Surface>),
) {
    for item in &module.items {
        match item {
            crate::ast::Item::Labels(labels, _) => visit(labels),
            crate::ast::Item::TypeRecGroup(group) => {
                for member in &group.members {
                    if let crate::ast::TypeRecMember::Labels(labels, _) = member {
                        visit(labels);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_surface_tokens(
    parsed: &package_collection::ParsedPackageCollection,
    surface_modules: &BTreeMap<PackageKey, Vec<(PathBuf, Module<Surface>)>>,
    only_files: Option<&HashSet<PathBuf>>,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Option<HashMap<PathBuf, Vec<crate::tokens::ClassifiedToken>>> {
    let mut by_file = HashMap::new();
    for (package_key, package) in &parsed.packages {
        let forced_by_file: HashMap<_, _> = surface_modules
            .get(package_key)
            .into_iter()
            .flatten()
            .map(|(path, module)| (path.clone(), module))
            .collect();
        for (path, cached_module) in &package.modules {
            if cancel_token.is_cancelled() {
                return None;
            }
            if only_files.is_some_and(|files| !files.contains(path)) {
                continue;
            }
            let module = forced_by_file.get(path).copied().unwrap_or(cached_module);
            let source = package
                .sources
                .get(path)
                .expect("every parsed LSP module retains its source");
            let tokens = crate::tokens::dump_module(source, module)
                .expect("a parsed LSP module remains lexically classifiable");
            by_file.insert(path.clone(), tokens);
        }
    }
    Some(by_file)
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_record_label_import_binders(
    parsed: &package_collection::ParsedPackageCollection,
    surface_tokens: &HashMap<PathBuf, Vec<crate::tokens::ClassifiedToken>>,
    file_to_module: &BTreeMap<PathBuf, String>,
    position_index: &mut crate::pass::typecheck_full::PositionIndex,
    label_reuse_indexes: &mut HashMap<
        PathBuf,
        std::sync::Arc<crate::lsp::label_reuse::LabelReuseIndex>,
    >,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Option<()> {
    use crate::ast::{ImportItem, ImportKind, mint_label_newtype_name};
    use crate::pass::typecheck_full::ResolvedBinder;

    for package in parsed.packages.values() {
        if cancel_token.is_cancelled() {
            return None;
        }
        let providers: HashMap<String, &PathBuf> = package
            .modules
            .iter()
            .map(|(file_path, module)| (module_path_key(&module.path), file_path))
            .collect();
        for (consumer_file, module) in &package.modules {
            if cancel_token.is_cancelled() {
                return None;
            }
            let Some(consumer_module) = file_to_module.get(consumer_file) else {
                continue;
            };
            let Some(tokens) = surface_tokens.get(consumer_file) else {
                continue;
            };
            let mut local_labels = HashMap::new();
            lsp_for_each_surface_labels(module, |labels| {
                for entry in labels
                    .entries
                    .iter()
                    .filter(|entry| !entry.is_reuse_marker())
                {
                    local_labels.insert(
                        entry.name.clone(),
                        ResolvedBinder::Newtype {
                            module_path: consumer_module.clone(),
                            name: mint_label_newtype_name(&entry.name),
                        },
                    );
                }
            });
            let mut imported_labels = HashMap::new();
            let mut qualified_label_modules = HashMap::new();
            for import_ in &module.imports {
                match &import_.kind {
                    ImportKind::Selective { items, from } => {
                        let Some(provider_file) = providers.get(&module_path_key(from)) else {
                            continue;
                        };
                        let Some(provider_module) = file_to_module.get(provider_file.as_path())
                        else {
                            continue;
                        };
                        for item in items {
                            let ImportItem::Label { name, span, .. } = item else {
                                continue;
                            };
                            let binder = ResolvedBinder::Newtype {
                                module_path: provider_module.clone(),
                                name: mint_label_newtype_name(name),
                            };
                            position_index.record_binder(consumer_module, *span, binder.clone());
                            imported_labels.insert(name.as_str(), binder);
                        }
                    }
                    ImportKind::Qualified { path, alias } => {
                        let Some(provider_file) = providers.get(&module_path_key(path)) else {
                            continue;
                        };
                        let Some(provider_module) = file_to_module.get(provider_file.as_path())
                        else {
                            continue;
                        };
                        qualified_label_modules.insert(alias.as_str(), provider_module.clone());
                    }
                    ImportKind::Intrinsics | ImportKind::Comptime => {}
                }
            }
            if local_labels.is_empty()
                && imported_labels.is_empty()
                && qualified_label_modules.is_empty()
            {
                continue;
            }
            let Some(source) = package.sources.get(consumer_file) else {
                continue;
            };
            for (index, token) in tokens.iter().enumerate() {
                if cancel_token.is_cancelled() {
                    return None;
                }
                if !matches!(
                    token.kind,
                    crate::tokens::TokenKind::EntityNameLabel
                        | crate::tokens::TokenKind::EntityNameLabelReference
                        | crate::tokens::TokenKind::EntityNameQualifiedLabelReference
                ) {
                    continue;
                }
                let Some(name) = source.get(token.span.start as usize..token.span.end as usize)
                else {
                    continue;
                };
                let binder = if matches!(
                    token.kind,
                    crate::tokens::TokenKind::EntityNameQualifiedLabelReference
                ) {
                    // Qualification is an AST-derived token property. Walk
                    // classified tokens only to recover the written alias;
                    // comments are first-class tokens and cannot erase the
                    // parsed `alias.label` relationship.
                    let mut before = tokens[..index].iter().rev().filter(|token| {
                        !matches!(
                            token.kind,
                            crate::tokens::TokenKind::CommentLine
                                | crate::tokens::TokenKind::CommentDoc
                        )
                    });
                    let Some(dot) = before.next() else {
                        continue;
                    };
                    if source.get(dot.span.start as usize..dot.span.end as usize) != Some(".") {
                        continue;
                    }
                    let Some(qualifier) = before.next().and_then(|token| {
                        source.get(token.span.start as usize..token.span.end as usize)
                    }) else {
                        continue;
                    };
                    let Some(provider_module) = qualified_label_modules.get(qualifier) else {
                        continue;
                    };
                    ResolvedBinder::Newtype {
                        module_path: provider_module.clone(),
                        name: mint_label_newtype_name(name),
                    }
                } else {
                    let Some(binder) = local_labels.get(name).or_else(|| imported_labels.get(name))
                    else {
                        continue;
                    };
                    binder.clone()
                };
                if let Some(index) = label_reuse_indexes.get_mut(consumer_file) {
                    std::sync::Arc::make_mut(index).protect_label_span(token.span);
                }
                if matches!(token.kind, crate::tokens::TokenKind::EntityNameLabel)
                    && local_labels.contains_key(name)
                {
                    if let ResolvedBinder::Newtype {
                        module_path,
                        name: newtype,
                    } = &binder
                    {
                        for member in ["mk", "get"] {
                            position_index.record_declaration_site_only(
                                consumer_module,
                                token.span,
                                &ResolvedBinder::NewtypeMember {
                                    module_path: module_path.clone(),
                                    newtype: newtype.clone(),
                                    member: member.to_owned(),
                                },
                            );
                        }
                    }
                    position_index.record_binder_declaration(consumer_module, token.span, binder);
                } else {
                    position_index.record_binder(consumer_module, token.span, binder);
                }
            }
        }
    }
    Some(())
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_record_rec_group_positions(
    surface_modules: &BTreeMap<PackageKey, Vec<(PathBuf, Module<Surface>)>>,
    lowered: &PackageCollection<crate::ast::Lowered>,
    sources: &HashMap<PathBuf, String>,
    file_to_module: &BTreeMap<PathBuf, String>,
    position_index: &mut crate::pass::typecheck_full::PositionIndex,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Option<()> {
    use crate::ast::{Item, Type};
    use crate::pass::typecheck_full::ResolvedBinder;

    for (package_key, modules) in surface_modules {
        if cancel_token.is_cancelled() {
            return None;
        }
        let Some(lowered_package) = lowered.packages.get(package_key) else {
            continue;
        };
        for (file_path, module) in modules {
            if cancel_token.is_cancelled() {
                return None;
            }
            let Some(module_path) = file_to_module.get(file_path) else {
                continue;
            };
            let Some(source) = sources.get(file_path) else {
                continue;
            };
            let Some(lowered_module) = lowered_package.package.module(module_path) else {
                continue;
            };
            let Ok((_, members)) = crate::tokens::dump_module_with_rec_facts(source, module) else {
                continue;
            };
            let member_types: HashMap<&str, Type<crate::ast::Lowered>> = lowered_module
                .module
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::FnDef(def) if members.iter().any(|member| member.name == def.name) => {
                        Some((
                            def.name.as_str(),
                            def.sig.signature_ty(def.ret.clone(), def.meta.span),
                        ))
                    }
                    _ => None,
                })
                .collect();
            let mut record = |name: &str, span, ty: &Type<crate::ast::Lowered>| {
                position_index.record_type(module_path, span, ty.clone());
                position_index.record_binder(
                    module_path,
                    span,
                    ResolvedBinder::Fn {
                        module_path: module_path.clone(),
                        name: name.to_owned(),
                    },
                );
            };
            for member in &members {
                if let Some(ty) = member_types.get(member.name.as_str()) {
                    record(&member.name, member.name_span, ty);
                }
                for (name, span) in &member.calls {
                    if let Some(ty) = member_types.get(name.as_str()) {
                        record(name, *span, ty);
                    }
                }
            }
        }
    }
    Some(())
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_retain_source_written_binders(
    surface_tokens: &HashMap<PathBuf, Vec<crate::tokens::ClassifiedToken>>,
    sources: &HashMap<PathBuf, String>,
    file_to_module: &BTreeMap<PathBuf, String>,
    position_index: &mut crate::pass::typecheck_full::PositionIndex,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Option<()> {
    use crate::pass::typecheck_full::ResolvedBinder;

    let file_by_module: HashMap<_, _> = file_to_module
        .iter()
        .map(|(file_path, module_path)| (module_path.as_str(), file_path))
        .collect();
    let mut declaration_span_remaps = HashMap::new();
    for ((module_path, span), binder) in position_index.declaration_binders_iter() {
        if cancel_token.is_cancelled() {
            return None;
        }
        let name = match binder {
            ResolvedBinder::Local { name, .. } | ResolvedBinder::TypeParam { name, .. } => name,
            _ => continue,
        };
        let Some(file_path) = file_by_module.get(module_path.as_str()) else {
            continue;
        };
        let Some(source) = sources.get(*file_path) else {
            continue;
        };
        if source.get(span.start as usize..span.end as usize) == Some(name) {
            continue;
        }
        let Some(tokens) = surface_tokens.get(*file_path) else {
            continue;
        };
        if let Some(exact) = lsp_named_token_span(
            tokens,
            source,
            *span,
            name,
            crate::tokens::TokenKind::VariableParameter,
        ) {
            declaration_span_remaps.insert((module_path.clone(), *span), exact);
        }
    }
    position_index.remap_local_declaration_spans(&declaration_span_remaps);

    // A placeholder family has one written stem declaration, while its
    // resolved slots have separate hygienic identities. Retain only positions
    // tied to that exact parser-confirmed introduction.
    let placeholder_stems_by_module: HashMap<_, HashMap<_, _>> = file_to_module
        .iter()
        .filter_map(|(file_path, module_path)| {
            let source = sources.get(file_path)?;
            let tokens = surface_tokens.get(file_path)?;
            let stems = tokens
                .iter()
                .filter_map(|token| {
                    let span = token.span;
                    if token.kind != crate::tokens::TokenKind::VariableParameter
                        || span.start == 0
                        || source.as_bytes().get(span.start as usize - 1) != Some(&b'.')
                        || source.as_bytes().get(span.end as usize) != Some(&b'.')
                    {
                        return None;
                    }
                    Some((span, &source[span.start as usize..span.end as usize]))
                })
                .collect();
            Some((module_path.as_str(), stems))
        })
        .collect();

    let label_spans_by_module: HashMap<_, HashSet<_>> = file_to_module
        .iter()
        .filter_map(|(file_path, module_path)| {
            surface_tokens.get(file_path).map(|tokens| {
                let spans = tokens
                    .iter()
                    .filter(|token| {
                        matches!(
                            token.kind,
                            crate::tokens::TokenKind::EntityNameLabel
                                | crate::tokens::TokenKind::EntityNameLabelReference
                                | crate::tokens::TokenKind::EntityNameQualifiedLabelReference
                        )
                    })
                    .map(|token| token.span)
                    .collect();
                (module_path.as_str(), spans)
            })
        })
        .collect();
    let source_by_module: HashMap<&str, &str> = file_to_module
        .iter()
        .filter_map(|(file_path, module_path)| {
            sources
                .get(file_path)
                .map(|source| (module_path.as_str(), source.as_str()))
        })
        .collect();
    let retained = position_index.try_retain_binders(|module_path, span, binder| {
        if cancel_token.is_cancelled() {
            return None;
        }
        if label_spans_by_module
            .get(module_path)
            .is_some_and(|spans| spans.contains(&span))
        {
            return Some(false);
        }
        let Some(source) = source_by_module.get(module_path) else {
            return Some(true);
        };
        if let ResolvedBinder::Local {
            decl_span: Some(declaration),
            ..
        } = binder
            && let Some(stem) = placeholder_stems_by_module
                .get(module_path)
                .and_then(|stems| stems.get(declaration))
        {
            let numbered = source
                .get(span.start as usize..span.end as usize)
                .and_then(|written| written.strip_prefix(*stem))
                .is_some_and(|digits| {
                    !digits.is_empty()
                        && !digits.starts_with('0')
                        && digits.bytes().all(|byte| byte.is_ascii_digit())
                });
            return Some(numbered);
        }
        let written_name = match binder {
            ResolvedBinder::Local { name, .. }
            | ResolvedBinder::TypeParam { name, .. }
            | ResolvedBinder::Fn { name, .. }
            | ResolvedBinder::BlockLabel { name, .. }
            | ResolvedBinder::HostEnvFn { name, .. }
            | ResolvedBinder::Intrinsic { name }
            | ResolvedBinder::TypeAlias { name, .. }
            | ResolvedBinder::HostType { name, .. }
            | ResolvedBinder::Newtype { name, .. } => name,
            ResolvedBinder::NewtypeMember { member, .. }
            | ResolvedBinder::QualifiedImportMember { member, .. } => member,
            ResolvedBinder::QualifiedImport { alias } => alias,
        };
        Some(
            source
                .get(span.start as usize..span.end as usize)
                .is_some_and(|written| written == written_name),
        )
    });
    if !retained {
        return None;
    }
    // The written stem declares a family, not one of its implicit slots.
    // Keep each use's identity and exact goto target, but do not publish the
    // last slot's overwritten declaration type or binder at the shared stem.
    position_index.remove_local_declaration_metadata(
        placeholder_stems_by_module
            .iter()
            .flat_map(|(module_path, stems)| stems.keys().map(move |span| (*module_path, *span))),
    );
    Some(())
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_named_token_span(
    tokens: &[crate::tokens::ClassifiedToken],
    source: &str,
    within: crate::span::Span,
    name: &str,
    kind: crate::tokens::TokenKind,
) -> Option<crate::span::Span> {
    let first = tokens.partition_point(|token| token.span.start < within.start);
    tokens[first..].iter().find_map(|token| {
        if token.span.start >= within.end {
            return None;
        }
        (token.kind == kind
            && token.span.end <= within.end
            && source.get(token.span.start as usize..token.span.end as usize) == Some(name))
        .then_some(token.span)
    })
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_importable_name_binder(
    entry: &crate::pass::resolve::ModuleEntry<crate::ast::Lowered>,
    module_path: &str,
    name: &str,
) -> Option<crate::pass::typecheck_full::ResolvedBinder> {
    use crate::ast::Item;
    use crate::pass::typecheck_full::ResolvedBinder;

    let item_id = entry.scope.lookup(name)?;
    #[cfg(test)]
    lsp_declaration_site_work_counters::record_selective_probe();
    let declaration = crate::pass::resolve::declaration_by_id(&entry.module, item_id)?;
    if let Some(def) = declaration.fn_def().filter(|def| def.name == name) {
        return Some(ResolvedBinder::Fn {
            module_path: module_path.to_owned(),
            name: def.name.clone(),
        });
    }
    if let Some(def) = declaration.type_alias().filter(|def| def.name == name) {
        return Some(ResolvedBinder::TypeAlias {
            module_path: module_path.to_owned(),
            name: def.name.clone(),
        });
    }
    if let Some(def) = declaration.newtype().filter(|def| def.name == name) {
        return Some(ResolvedBinder::Newtype {
            module_path: module_path.to_owned(),
            name: def.name.clone(),
        });
    }
    if let Some(def) = declaration.host_type().filter(|def| def.name == name) {
        return Some(ResolvedBinder::HostType {
            module_path: module_path.to_owned(),
            name: def.name.clone(),
        });
    }
    if let Some(def) = declaration.host_fn().filter(|def| def.name == name) {
        return Some(ResolvedBinder::HostEnvFn {
            module_path: module_path.to_owned(),
            name: def.name.clone(),
        });
    }
    match entry.module.items.get(item_id.item_index())? {
        Item::RecGroup(group, _) => group
            .members
            .iter()
            .find(|def| def.name == name)
            .map(|def| ResolvedBinder::Fn {
                module_path: module_path.to_owned(),
                name: def.name.clone(),
            }),
        Item::FnDef(_)
        | Item::TypeRecGroup(_)
        | Item::Newtype(_)
        | Item::HostFn(_)
        | Item::TypeAlias(_)
        | Item::LiteralAlias(_, _)
        | Item::Labels(_, _)
        | Item::LabelForward(_, _)
        | Item::Equiv(_, _)
        | Item::Elaborator(_, _)
        | Item::Op(_, _)
        | Item::VariadicOperator(_, _)
        | Item::HostType(_) => None,
    }
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_record_surface_declaration_positions(
    parsed: &package_collection::ParsedPackageCollection,
    lowered: &PackageCollection<crate::ast::Lowered>,
    surface_tokens: &HashMap<PathBuf, Vec<crate::tokens::ClassifiedToken>>,
    file_to_module: &BTreeMap<PathBuf, String>,
    position_index: &mut crate::pass::typecheck_full::PositionIndex,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Option<()> {
    use crate::ast::{ImportItem, ImportKind, Item};
    use crate::pass::typecheck_full::ResolvedBinder;
    use crate::tokens::TokenKind;

    for (package_key, package) in &parsed.packages {
        if cancel_token.is_cancelled() {
            return None;
        }
        let lowered_package = &lowered
            .packages
            .get(package_key)
            .expect("every parsed package has a lowered LSP package")
            .package;
        // A provider map is proportional to modules, while declaration lookup
        // itself uses the resolver-built top-level index. Do not rebuild a
        // provider-wide name map merely because one selective import is open.
        let providers_by_written_path: HashMap<_, _> = package
            .modules
            .iter()
            .filter_map(|(file_path, module)| {
                let semantic_path = file_to_module.get(file_path)?;
                let lowered_module = lowered_package.module(semantic_path)?;
                Some((
                    module_path_key(&module.path),
                    (semantic_path.as_str(), lowered_module),
                ))
            })
            .collect();
        let mut import_binder_cache = HashMap::new();

        for (file_path, module) in &package.modules {
            if cancel_token.is_cancelled() {
                return None;
            }
            let Some(module_path) = file_to_module.get(file_path) else {
                continue;
            };
            let source = package
                .sources
                .get(file_path)
                .expect("every parsed LSP module retains its source");
            let Some(tokens) = surface_tokens.get(file_path) else {
                continue;
            };
            let mut record = |within, name: &str, kind, binder| {
                let span = lsp_named_token_span(tokens, source, within, name, kind)
                    .expect("a surface declaration retains its exact name token");
                position_index.record_binder_declaration(module_path, span, binder);
            };
            for item in &module.items {
                match item {
                    Item::FnDef(def) => record(
                        def.meta.span,
                        &def.name,
                        TokenKind::EntityNameFunction,
                        ResolvedBinder::Fn {
                            module_path: module_path.clone(),
                            name: def.name.clone(),
                        },
                    ),
                    Item::RecGroup(group, _) => {
                        for def in &group.members {
                            record(
                                def.meta.span,
                                &def.name,
                                TokenKind::EntityNameFunction,
                                ResolvedBinder::Fn {
                                    module_path: module_path.clone(),
                                    name: def.name.clone(),
                                },
                            );
                        }
                    }
                    Item::TypeAlias(def) => record(
                        def.meta.span,
                        &def.name,
                        TokenKind::EntityNameType,
                        ResolvedBinder::TypeAlias {
                            module_path: module_path.clone(),
                            name: def.name.clone(),
                        },
                    ),
                    Item::Newtype(def) => {
                        record(
                            def.meta.span,
                            &def.name,
                            TokenKind::EntityNameType,
                            ResolvedBinder::Newtype {
                                module_path: module_path.clone(),
                                name: def.name.clone(),
                            },
                        );
                        for member in [&def.constructor, &def.projector] {
                            record(
                                member.span,
                                &member.name,
                                TokenKind::EntityNameFunction,
                                ResolvedBinder::NewtypeMember {
                                    module_path: module_path.clone(),
                                    newtype: def.name.clone(),
                                    member: member.name.clone(),
                                },
                            );
                        }
                    }
                    Item::TypeRecGroup(group) => {
                        for member in &group.members {
                            match member {
                                crate::ast::TypeRecMember::TypeAlias(def) => record(
                                    def.meta.span,
                                    &def.name,
                                    TokenKind::EntityNameType,
                                    ResolvedBinder::TypeAlias {
                                        module_path: module_path.clone(),
                                        name: def.name.clone(),
                                    },
                                ),
                                crate::ast::TypeRecMember::Newtype(def) => {
                                    record(
                                        def.meta.span,
                                        &def.name,
                                        TokenKind::EntityNameType,
                                        ResolvedBinder::Newtype {
                                            module_path: module_path.clone(),
                                            name: def.name.clone(),
                                        },
                                    );
                                    for member in [&def.constructor, &def.projector] {
                                        record(
                                            member.span,
                                            &member.name,
                                            TokenKind::EntityNameFunction,
                                            ResolvedBinder::NewtypeMember {
                                                module_path: module_path.clone(),
                                                newtype: def.name.clone(),
                                                member: member.name.clone(),
                                            },
                                        );
                                    }
                                }
                                crate::ast::TypeRecMember::Labels(labels, _) => {
                                    if let (Some(name), Some(span)) =
                                        (&labels.type_alias_name, labels.type_alias_span)
                                    {
                                        record(
                                            span,
                                            name,
                                            TokenKind::EntityNameType,
                                            ResolvedBinder::TypeAlias {
                                                module_path: module_path.clone(),
                                                name: name.clone(),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Item::HostFn(def) => record(
                        def.meta.span,
                        &def.name,
                        TokenKind::EntityNameFunction,
                        ResolvedBinder::HostEnvFn {
                            module_path: module_path.clone(),
                            name: def.name.clone(),
                        },
                    ),
                    Item::HostType(def) => record(
                        def.meta.span,
                        &def.name,
                        TokenKind::EntityNameType,
                        ResolvedBinder::HostType {
                            module_path: module_path.clone(),
                            name: def.name.clone(),
                        },
                    ),
                    Item::Labels(labels, _) => {
                        if let (Some(name), Some(span)) =
                            (&labels.type_alias_name, labels.type_alias_span)
                        {
                            record(
                                span,
                                name,
                                TokenKind::EntityNameType,
                                ResolvedBinder::TypeAlias {
                                    module_path: module_path.clone(),
                                    name: name.clone(),
                                },
                            );
                        }
                    }
                    Item::LiteralAlias(_, _)
                    | Item::LabelForward(_, _)
                    | Item::Equiv(_, _)
                    | Item::Elaborator(_, _)
                    | Item::Op(_, _)
                    | Item::VariadicOperator(_, _) => {}
                }
            }

            for import_ in &module.imports {
                match &import_.kind {
                    ImportKind::Qualified { path, alias } => {
                        let alias_search = crate::span::Span::new(path.span.end, import_.span.end);
                        let alias_span = lsp_named_token_span(
                            tokens,
                            source,
                            alias_search,
                            alias,
                            TokenKind::EntityNameModule,
                        )
                        .expect("a qualified import retains its exact alias token");
                        position_index.record_binder_declaration(
                            module_path,
                            alias_span,
                            ResolvedBinder::QualifiedImport {
                                alias: alias.clone(),
                            },
                        );
                    }
                    ImportKind::Selective { items, from } => {
                        let provider_path = module_path_key(from);
                        let Some((provider_module_path, provider)) =
                            providers_by_written_path.get(&provider_path)
                        else {
                            continue;
                        };
                        for item in items {
                            if cancel_token.is_cancelled() {
                                return None;
                            }
                            let ImportItem::Name { name, span, .. } = item else {
                                continue;
                            };
                            let binder = import_binder_cache
                                .entry((provider_path.clone(), name.clone()))
                                .or_insert_with(|| {
                                    lsp_importable_name_binder(provider, provider_module_path, name)
                                });
                            let Some(binder) = binder.clone() else {
                                continue;
                            };
                            position_index.record_binder(module_path, *span, binder);
                        }
                    }
                    ImportKind::Intrinsics | ImportKind::Comptime => {}
                }
            }
        }
    }
    Some(())
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_top_level_declaration_query(
    entry: &crate::pass::resolve::ModuleEntry<crate::ast::Lowered>,
    binder: &crate::pass::typecheck_full::ResolvedBinder,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Option<(crate::span::Span, String, crate::tokens::TokenKind)> {
    use crate::ast::Item;
    use crate::pass::typecheck_full::ResolvedBinder;
    use crate::tokens::TokenKind;

    #[cfg(test)]
    lsp_declaration_site_work_counters::record_declaration_probe();

    match binder {
        ResolvedBinder::Fn { name, .. } => {
            let item_id = entry.scope.lookup(name)?;
            match entry.module.items.get(item_id.0 as usize)? {
                Item::FnDef(def) if def.name == *name => {
                    Some((def.meta.span, name.clone(), TokenKind::EntityNameFunction))
                }
                Item::RecGroup(group, _) => {
                    let def = group
                        .members
                        .iter()
                        .find(|def| !cancel_token.is_cancelled() && def.name == *name)?;
                    Some((def.meta.span, name.clone(), TokenKind::EntityNameFunction))
                }
                _ => None,
            }
        }
        ResolvedBinder::BlockLabel { .. } => None,
        ResolvedBinder::HostEnvFn { name, .. } => {
            let item_id = entry.scope.lookup(name)?;
            let Item::HostFn(def) = entry.module.items.get(item_id.0 as usize)? else {
                return None;
            };
            (def.name == *name)
                .then(|| (def.meta.span, name.clone(), TokenKind::EntityNameFunction))
        }
        ResolvedBinder::TypeAlias { name, .. } => {
            let item_id = entry.scope.lookup(name)?;
            let def =
                crate::pass::resolve::declaration_by_id(&entry.module, item_id)?.type_alias()?;
            (def.name == *name).then(|| (def.meta.span, name.clone(), TokenKind::EntityNameType))
        }
        ResolvedBinder::HostType { name, .. } => {
            let item_id = entry.scope.lookup(name)?;
            let def =
                crate::pass::resolve::declaration_by_id(&entry.module, item_id)?.host_type()?;
            (def.name == *name).then(|| (def.meta.span, name.clone(), TokenKind::EntityNameType))
        }
        ResolvedBinder::Newtype { name, .. } => {
            let item_id = entry.scope.lookup(name)?;
            let def = crate::pass::resolve::declaration_by_id(&entry.module, item_id)?.newtype()?;
            (def.name == *name).then(|| (def.meta.span, name.clone(), TokenKind::EntityNameType))
        }
        ResolvedBinder::NewtypeMember {
            newtype, member, ..
        } => {
            let item_id = entry.scope.lookup(newtype)?;
            let def = crate::pass::resolve::declaration_by_id(&entry.module, item_id)?.newtype()?;
            let member_def = [&def.constructor, &def.projector]
                .into_iter()
                .find(|candidate| candidate.name == *member)?;
            Some((
                member_def.span,
                member.clone(),
                TokenKind::EntityNameFunction,
            ))
        }
        ResolvedBinder::Local { .. }
        | ResolvedBinder::TypeParam { .. }
        | ResolvedBinder::Intrinsic { .. }
        | ResolvedBinder::QualifiedImport { .. }
        | ResolvedBinder::QualifiedImportMember { .. } => None,
    }
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_generated_label_newtype(
    binder: &crate::pass::typecheck_full::ResolvedBinder,
) -> Option<&str> {
    match binder {
        crate::pass::typecheck_full::ResolvedBinder::Newtype { name, .. } => Some(name),
        crate::pass::typecheck_full::ResolvedBinder::NewtypeMember { newtype, .. } => Some(newtype),
        _ => None,
    }
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_generated_label_declaration_spans(
    module: &Module<Surface>,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Option<HashMap<String, crate::span::Span>> {
    #[cfg(test)]
    lsp_declaration_site_work_counters::record_generated_label_map_build();
    let mut declarations = HashMap::new();
    lsp_for_each_surface_labels(module, |labels| {
        for entry in &labels.entries {
            if cancel_token.is_cancelled() {
                return;
            }
            if !entry.is_reuse_marker() {
                declarations.insert(
                    crate::ast::mint_label_newtype_name(&entry.name),
                    entry.name_span,
                );
            }
        }
    });
    if cancel_token.is_cancelled() {
        return None;
    }
    Some(declarations)
}

/// Focused analysis deliberately indexes occurrences only in the selected
/// shard. Definition still needs exact remote targets, so resolve the distinct
/// missing semantic identities through the provider's existing scope and
/// tokenize each referenced provider at most once. Provider positions are
/// retained only in the declaration-site map, never in the occurrence index.
#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_record_missing_declaration_sites(
    parsed: &package_collection::ParsedPackageCollection,
    lowered: &PackageCollection<crate::ast::Lowered>,
    position_index: &mut crate::pass::typecheck_full::PositionIndex,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Option<()> {
    let missing =
        position_index.try_missing_declaration_site_binders(|| cancel_token.is_cancelled())?;
    if missing.is_empty() {
        return Some(());
    }
    let mut provider_tokens = HashMap::new();
    let mut provider_label_declarations = HashMap::new();
    let mut providers = HashMap::new();

    // Build the cross-package owner index once. A focused shard can mention
    // many remote declarations, so searching every package and its surface
    // module vector independently for each binder would make this K * M in
    // the number of missing targets and workspace modules.
    for (package_key, lowered_package) in &lowered.packages {
        if cancel_token.is_cancelled() {
            return None;
        }
        let parsed_package = parsed.packages.get(package_key)?;
        let surface_by_file: HashMap<_, _> = parsed_package
            .modules
            .iter()
            .map(|(file_path, module)| (file_path.as_path(), module))
            .collect();
        for (owner, lowered_entry) in lowered_package.package.modules() {
            if cancel_token.is_cancelled() {
                return None;
            }
            #[cfg(test)]
            lsp_declaration_site_work_counters::record_provider_index_entry();
            let file_path = &lowered_entry.file_path;
            let Some(surface_module) = surface_by_file.get(file_path.as_path()).copied() else {
                continue;
            };
            let Some(source) = parsed_package.sources.get(file_path) else {
                continue;
            };
            providers
                .entry(owner)
                .or_insert((lowered_entry, file_path, surface_module, source));
        }
    }

    for binder in missing {
        if cancel_token.is_cancelled() {
            return None;
        }
        let owner = match &binder {
            crate::pass::typecheck_full::ResolvedBinder::Fn { module_path, .. }
            | crate::pass::typecheck_full::ResolvedBinder::BlockLabel { module_path, .. }
            | crate::pass::typecheck_full::ResolvedBinder::HostEnvFn { module_path, .. }
            | crate::pass::typecheck_full::ResolvedBinder::TypeAlias { module_path, .. }
            | crate::pass::typecheck_full::ResolvedBinder::HostType { module_path, .. }
            | crate::pass::typecheck_full::ResolvedBinder::Newtype { module_path, .. }
            | crate::pass::typecheck_full::ResolvedBinder::NewtypeMember { module_path, .. } => {
                module_path
            }
            crate::pass::typecheck_full::ResolvedBinder::Local { .. }
            | crate::pass::typecheck_full::ResolvedBinder::TypeParam { .. }
            | crate::pass::typecheck_full::ResolvedBinder::Intrinsic { .. }
            | crate::pass::typecheck_full::ResolvedBinder::QualifiedImport { .. }
            | crate::pass::typecheck_full::ResolvedBinder::QualifiedImportMember { .. } => {
                continue;
            }
        };

        #[cfg(test)]
        lsp_declaration_site_work_counters::record_provider_lookup();
        let Some(&(lowered_entry, file_path, surface_module, source)) =
            providers.get(owner.as_str())
        else {
            continue;
        };
        let Some((within, name, kind)) =
            lsp_top_level_declaration_query(lowered_entry, &binder, cancel_token)
        else {
            continue;
        };
        if cancel_token.is_cancelled() {
            return None;
        }

        if !provider_tokens.contains_key(file_path) {
            #[cfg(test)]
            lsp_declaration_site_work_counters::record_provider_tokenization();
            let tokens = crate::tokens::dump_module(source, surface_module)
                .expect("a parsed provider module remains lexically classifiable");
            provider_tokens.insert(file_path.clone(), tokens);
        }
        let tokens = provider_tokens
            .get(file_path)
            .expect("inserted provider tokens before lookup");
        let mut span = lsp_named_token_span(tokens, source, within, &name, kind);
        if span.is_none() {
            let Some(newtype) = lsp_generated_label_newtype(&binder) else {
                continue;
            };
            if !provider_label_declarations.contains_key(file_path) {
                let declarations =
                    lsp_generated_label_declaration_spans(surface_module, cancel_token)?;
                provider_label_declarations.insert(file_path.clone(), declarations);
            }
            span = provider_label_declarations
                .get(file_path)
                .and_then(|declarations| declarations.get(newtype))
                .copied();
        }
        if let Some(span) = span {
            position_index.record_declaration_site_only(owner, span, &binder);
        }
    }
    Some(())
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_record_block_positions(
    surface_modules: &BTreeMap<PackageKey, Vec<(PathBuf, Module<Surface>)>>,
    declarations: &BTreeMap<PackageKey, crate::pass::full::LspSourceDeclarations>,
    file_to_module: &BTreeMap<PathBuf, String>,
    position_index: &mut crate::pass::typecheck_full::PositionIndex,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Option<()> {
    for (package_key, modules) in surface_modules {
        let facts = declarations.get(package_key)?;
        let mut by_module: BTreeMap<&str, BTreeMap<&str, _>> = BTreeMap::new();
        for fact in &facts.blocks {
            if cancel_token.is_cancelled() {
                return None;
            }
            by_module
                .entry(&fact.source_module)
                .or_default()
                .insert(&fact.source_name, fact);
        }
        for (file, module) in modules {
            let declared_path = module.path.segments.join("/");
            let Some(selected) = by_module.get(declared_path.as_str()) else {
                continue;
            };
            crate::lsp::block_labels::record_module(
                module,
                file_to_module.get(file)?,
                selected,
                file_to_module,
                position_index,
                || cancel_token.is_cancelled(),
            )?;
        }
    }
    Some(())
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_record_callable_declaration_positions(
    parsed: &package_collection::ParsedPackageCollection,
    callable_declarations: &BTreeMap<PackageKey, crate::pass::full::LspSourceDeclarations>,
    lowered: &PackageCollection<crate::ast::Lowered>,
    position_index: &mut crate::pass::typecheck_full::PositionIndex,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Option<()> {
    use crate::ast::{Item, Lowered};
    use crate::pass::op_fold::{
        ResolvedBuiltinKind, ResolvedCallableTarget, ResolvedCallableTypeHead,
    };
    use crate::pass::typecheck_core::{
        InternedType, ModuleEnv, RoleResolution, comptime_scheme, intrinsic_scheme_resolved,
        newtype_member_scheme_in_module, nominal_segments_in_module, synth_env_fn_def_in_module,
        synth_top_fn_def_in_module,
    };
    use crate::pass::typecheck_full::ResolvedBinder;

    #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    enum ModuleCallableKey {
        Function {
            owner: String,
            name: String,
        },
        NewtypeMember {
            owner: String,
            newtype: String,
            member: String,
        },
    }

    for (package_key, facts) in callable_declarations {
        if cancel_token.is_cancelled() {
            return None;
        }
        let facts = &facts.callables;
        if facts.is_empty() {
            continue;
        }
        let parsed_package = parsed
            .packages
            .get(package_key)
            .expect("callable facts belong to a parsed LSP package");
        let lowered_package = &lowered
            .packages
            .get(package_key)
            .expect("every parsed package has a lowered LSP package")
            .package;
        let package_name = parsed_package
            .package_file
            .as_ref()
            .map(|entry| entry.package_name.as_str());
        let builtin_source_modules = facts
            .iter()
            .filter_map(|fact| {
                matches!(fact.target, ResolvedCallableTarget::Builtin { .. })
                    .then_some(fact.source_module.as_str())
            })
            .collect::<BTreeSet<_>>();
        let mut builtin_envs_by_module = HashMap::new();
        for source_module in builtin_source_modules {
            let source_entry = lowered_package
                .module(source_module)
                .expect("resolved callable source remains in the lowered package");
            let source_env = ModuleEnv::build(
                &source_entry.module,
                lowered_package
                    .package_file()
                    .map(|entry| &entry.package_file),
                package_name,
                Some(lowered_package),
            )
            .expect("a successfully typed LSP module retains its lexical environment");
            builtin_envs_by_module.insert(source_module.to_owned(), source_env);
        }

        let needed_module_targets = facts
            .iter()
            .filter_map(|fact| {
                let ResolvedCallableTarget::Module { owner, member, .. } = &fact.target else {
                    return None;
                };
                let owner = module_path_key(owner);
                match member.as_slice() {
                    [name] => Some(ModuleCallableKey::Function {
                        owner,
                        name: name.clone(),
                    }),
                    [newtype, member] => Some(ModuleCallableKey::NewtypeMember {
                        owner,
                        newtype: newtype.clone(),
                        member: member.clone(),
                    }),
                    [] | [_, _, ..] => {
                        unreachable!("callable resolver admits only function/member targets")
                    }
                }
            })
            .collect::<BTreeSet<_>>();
        let mut module_targets: HashMap<
            ModuleCallableKey,
            (ResolvedBinder, InternedType<Lowered>),
        > = HashMap::new();
        for key in &needed_module_targets {
            if cancel_token.is_cancelled() {
                return None;
            }
            #[cfg(test)]
            lsp_callable_target_work_counters::record_scheme();
            let (owner, name) = match key {
                ModuleCallableKey::Function { owner, name } => (owner, name),
                ModuleCallableKey::NewtypeMember { owner, newtype, .. } => (owner, newtype),
            };
            let entry = lowered_package
                .module(owner)
                .expect("resolved callable owner remains in the lowered package");
            let item_id = entry
                .scope
                .lookup(name)
                .expect("resolved callable remains in its owner scope");
            let item = entry
                .module
                .items
                .get(item_id.item_index())
                .expect("owner scope points at a top-level item");
            let (binder, ty) = match key {
                ModuleCallableKey::NewtypeMember {
                    newtype, member, ..
                } => {
                    let def = crate::pass::resolve::declaration_by_id(&entry.module, item_id)
                        .and_then(crate::pass::resolve::TopLevelDeclaration::newtype)
                        .expect("resolved callable newtype remains in its owner scope");
                    let nominal_segments =
                        nominal_segments_in_module(owner, newtype, def.meta.span);
                    let ty = newtype_member_scheme_in_module(
                        def,
                        member,
                        &nominal_segments,
                        &entry.module,
                        Some(lowered_package),
                        def.meta.span,
                    )
                    .expect("resolved callable newtype member retains its scheme")
                    .ty;
                    (
                        ResolvedBinder::NewtypeMember {
                            module_path: owner.clone(),
                            newtype: newtype.clone(),
                            member: member.clone(),
                        },
                        ty,
                    )
                }
                ModuleCallableKey::Function { name, .. } => match item {
                    Item::FnDef(def) => (
                        ResolvedBinder::Fn {
                            module_path: owner.clone(),
                            name: name.clone(),
                        },
                        synth_top_fn_def_in_module(
                            def,
                            &entry.module,
                            Some(lowered_package),
                            def.meta.span,
                        )
                        .ty,
                    ),
                    Item::RecGroup(group, _) => {
                        let mut target = None;
                        for def in &group.members {
                            if cancel_token.is_cancelled() {
                                return None;
                            }
                            if def.name == *name {
                                target = Some(def);
                                break;
                            }
                        }
                        let def = target.expect("recursive callable remains in its owner group");
                        (
                            ResolvedBinder::Fn {
                                module_path: owner.clone(),
                                name: name.clone(),
                            },
                            synth_top_fn_def_in_module(
                                def,
                                &entry.module,
                                Some(lowered_package),
                                def.meta.span,
                            )
                            .ty,
                        )
                    }
                    Item::HostFn(host) => (
                        ResolvedBinder::HostEnvFn {
                            module_path: owner.clone(),
                            name: name.clone(),
                        },
                        synth_env_fn_def_in_module(
                            host,
                            &entry.module,
                            Some(lowered_package),
                            host.meta.span,
                        )
                        .ty,
                    ),
                    _ => unreachable!("owner scope and callable resolver agree on item kind"),
                },
            };
            module_targets.insert(key.clone(), (binder, ty));
        }

        for fact in facts {
            if cancel_token.is_cancelled() {
                return None;
            }
            let source_segments = fact.source_path.segments();
            let leaf = source_segments
                .last()
                .expect("a lexical callable path is non-empty");
            match &fact.target {
                ResolvedCallableTarget::Builtin { kind, name } => {
                    debug_assert_eq!(source_segments.len(), 1);
                    let source_env = builtin_envs_by_module
                        .get(&fact.source_module)
                        .expect("built an environment for every builtin callable source");
                    // Surface-only declarations disappear before `ModuleEnv` is
                    // built, so translate the retained source position into a
                    // Lowered item cutoff rather than retaining an unstable index.
                    let lowered_item_cutoff = source_env
                        .module
                        .items
                        .partition_point(|item| item.span().start < fact.source_item_start);
                    let synth = match kind {
                        ResolvedBuiltinKind::Intrinsic => {
                            let bool_role = source_env.resolve_exact_role_at(
                                crate::ast::Role::Bool,
                                Some(lowered_item_cutoff),
                            );
                            let ambiguity = match bool_role {
                                RoleResolution::Ambiguous { first, second } => {
                                    Some(source_env.describe_exact_role_ambiguity(
                                        crate::ast::Role::Bool,
                                        first,
                                        second,
                                        Some(lowered_item_cutoff),
                                    ))
                                }
                                RoleResolution::Missing | RoleResolution::Unique(_) => None,
                            };
                            intrinsic_scheme_resolved::<Lowered>(
                                name,
                                leaf.span,
                                bool_role,
                                ambiguity.as_deref(),
                            )
                            .and_then(Result::ok)
                        }
                        ResolvedBuiltinKind::Comptime => {
                            crate::comptime::ComptimeBuiltin::from_public_name(name).and_then(
                                |builtin| {
                                    comptime_scheme::<Lowered>(
                                        builtin,
                                        leaf.span,
                                        &source_env.roles_in_scope,
                                    )
                                    .ok()
                                    .flatten()
                                },
                            )
                        }
                    };
                    if let Some(synth) = synth {
                        position_index.record_type_with_identity(
                            &fact.source_module,
                            leaf.span,
                            synth.ty,
                            HashSet::new(),
                        );
                    }
                    if matches!(kind, ResolvedBuiltinKind::Intrinsic) {
                        position_index.record_binder(
                            &fact.source_module,
                            leaf.span,
                            ResolvedBinder::Intrinsic { name: name.clone() },
                        );
                    }
                }
                ResolvedCallableTarget::Module {
                    owner,
                    member,
                    source_type,
                } => {
                    let owner_path = module_path_key(owner);

                    if source_segments.len() == member.len() + 1 {
                        let alias = &source_segments[0];
                        position_index.record_binder(
                            &fact.source_module,
                            alias.span,
                            ResolvedBinder::QualifiedImport {
                                alias: alias.name.clone(),
                            },
                        );
                    }

                    match member.as_slice() {
                        [name] => {
                            debug_assert!(matches!(source_segments.len(), 1 | 2));
                            let key = ModuleCallableKey::Function {
                                owner: owner_path,
                                name: name.clone(),
                            };
                            let (binder, ty) = module_targets
                                .get(&key)
                                .expect("resolved callable function remains in its owner module");
                            position_index.record_binder(
                                &fact.source_module,
                                leaf.span,
                                binder.clone(),
                            );
                            position_index.record_type_with_identity(
                                &fact.source_module,
                                leaf.span,
                                ty.clone(),
                                HashSet::new(),
                            );
                        }
                        [type_name, member_name] => {
                            debug_assert!(matches!(source_segments.len(), 2 | 3));
                            let type_segment = &source_segments[source_segments.len() - 2];
                            let source_type = source_type
                                .as_ref()
                                .expect("a resolved newtype member retains its source head");
                            let source_binder = match source_type {
                                ResolvedCallableTypeHead::TypeAlias { owner, name } => {
                                    ResolvedBinder::TypeAlias {
                                        module_path: module_path_key(owner),
                                        name: name.clone(),
                                    }
                                }
                                ResolvedCallableTypeHead::Newtype { owner, name } => {
                                    ResolvedBinder::Newtype {
                                        module_path: module_path_key(owner),
                                        name: name.clone(),
                                    }
                                }
                            };
                            position_index.record_binder(
                                &fact.source_module,
                                type_segment.span,
                                source_binder,
                            );
                            position_index.record_binder(
                                &fact.source_module,
                                leaf.span,
                                ResolvedBinder::NewtypeMember {
                                    module_path: owner_path.clone(),
                                    newtype: type_name.clone(),
                                    member: member_name.clone(),
                                },
                            );
                            let key = ModuleCallableKey::NewtypeMember {
                                owner: owner_path,
                                newtype: type_name.clone(),
                                member: member_name.clone(),
                            };
                            let (_, ty) = module_targets
                                .get(&key)
                                .expect("resolved callable newtype remains in its owner module");
                            position_index.record_type_with_identity(
                                &fact.source_module,
                                leaf.span,
                                ty.clone(),
                                HashSet::new(),
                            );
                        }
                        [] | [_, _, ..] => {
                            unreachable!("callable resolver admits only function/member targets")
                        }
                    }
                }
            }
        }
    }
    Some(())
}

#[cfg(all(feature = "surface", feature = "lsp"))]
#[derive(Debug, Clone)]
pub struct LspWarning {
    pub file_path: PathBuf,
    pub span: crate::span::Span,
    pub message: String,
    pub fixes: Vec<Fix>,
}

#[cfg(all(feature = "surface", feature = "lsp"))]
#[derive(Clone, Default)]
pub(crate) struct LspUserElaboratorMemos {
    revisions: crate::pass::typecheck_core::PackageTypecheckRevisionStore<
        PackageKey,
        SourceHash,
        crate::ast::Lowered,
    >,
}

#[cfg(all(feature = "surface", feature = "lsp"))]
impl LspUserElaboratorMemos {
    fn scope_for(
        &self,
        package_key: &PackageKey,
        package_fingerprint: &SourceHash,
        package: &crate::pass::alpha_normalize::AlphaNormalizedPackage<crate::ast::Lowered>,
    ) -> std::sync::Arc<crate::pass::typecheck_core::PackageTypecheckScope<crate::ast::Lowered>>
    {
        self.revisions
            .scope_for_normalized(package_key, package_fingerprint, package)
    }
}

/// LSP-specific variant of [`analyze_workspace_at_with_overlay`].
/// Runs the full pipeline and, on success, captures the
/// [`crate::pass::typecheck_full::PositionIndex`] and file-to-module map in
/// an [`LspAnalysis`]. On failure returns the normal
/// [`AnalysisFailure`].
///
/// The standard analysis path consumes the checked Lowered package into
/// Prime. This variant retains the checked position index for editor queries
/// while formatting declarations from the original source-spelled tree.
#[cfg(all(feature = "surface", feature = "lsp"))]
pub fn analyze_workspace_at_with_overlay_lsp(
    root: &Path,
    overlay: &package_collection::SourceOverlay,
) -> Result<LspAnalysis, AnalysisFailure> {
    let cancel_token = crate::lsp::cancel::CancellationToken::new();
    analyze_workspace_at_with_overlay_lsp_cancellable(root, overlay, &cancel_token)
        .expect("uncancelled LSP analysis cannot be cancelled")
}

#[cfg(all(feature = "surface", feature = "lsp"))]
pub(crate) fn analyze_workspace_at_with_overlay_lsp_cancellable(
    root: &Path,
    overlay: &package_collection::SourceOverlay,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Option<Result<LspAnalysis, AnalysisFailure>> {
    let memos = LspUserElaboratorMemos::default();
    analyze_workspace_at_with_overlay_lsp_cancellable_with_memos(
        root,
        overlay,
        cancel_token,
        &memos,
    )
}

#[cfg(all(feature = "surface", feature = "lsp"))]
pub(crate) fn analyze_workspace_at_with_overlay_lsp_cancellable_with_memos(
    root: &Path,
    overlay: &package_collection::SourceOverlay,
    cancel_token: &crate::lsp::cancel::CancellationToken,
    user_elaborator_memos: &LspUserElaboratorMemos,
) -> Option<Result<LspAnalysis, AnalysisFailure>> {
    use crate::ast::Lowered;
    use crate::pass::typecheck_full::{
        PositionIndex, check_normalized_package_collect_errors_with_scope,
    };

    if cancel_token.is_cancelled() {
        return None;
    }

    // Phase 1: walk + parse.
    let parsed_ws = match package_collection::walk_with_overlay(root, overlay) {
        Ok(w) => w,
        Err((walk_err, partial_sources)) => {
            let mut sources: HashMap<PathBuf, String> = HashMap::new();
            for (k, v) in partial_sources {
                sources.insert(k, v);
            }
            if let package_collection::WalkError::Parse {
                path, source_text, ..
            } = &walk_err
            {
                sources.insert(path.clone(), source_text.clone());
            }
            let diag = walk_err.into_located();
            return Some(Err(AnalysisFailure::from_error(diag, sources)));
        }
    };
    if cancel_token.is_cancelled() {
        return None;
    }
    let package_fingerprints = lsp_user_elaborator_package_fingerprints(&parsed_ws);

    let mut sources: HashMap<PathBuf, String> = HashMap::new();
    for pkg in parsed_ws.packages.values() {
        for (k, v) in &pkg.sources {
            sources.insert(k.clone(), v.clone());
        }
    }
    let root_pkg = parsed_ws
        .packages
        .get(&parsed_ws.root)
        .expect("walk_with_overlay inserts the root package entry");
    let missing_deps = lsp_missing_materialized_dependencies(root_pkg);
    if !missing_deps.is_empty() {
        return Some(Err(AnalysisFailure::from_errors(missing_deps, sources)));
    }
    if cancel_token.is_cancelled() {
        return None;
    }

    // Phase 2: lower + resolve (no package-check cache skip — LSP always re-checks
    // so the position index is always fresh).
    let mut lowered: BTreeMap<PackageKey, package_collection::PackageEntry<Lowered>> =
        BTreeMap::new();
    let mut callable_declarations = BTreeMap::new();
    let mut surface_modules = BTreeMap::new();
    for (key, parsed_pkg) in &parsed_ws.packages {
        if cancel_token.is_cancelled() {
            return None;
        }
        if let Err(diag) = validate_source_package_with_parse_precedence::<FullPipeline>(parsed_pkg)
        {
            return Some(Err(AnalysisFailure::from_error(diag, sources)));
        }
        let parsed_for_lowering = match force_parsed_package_bodies(parsed_pkg) {
            Ok(p) => p,
            Err(diag) => {
                return Some(Err(AnalysisFailure::from_error(diag, sources)));
            }
        };
        if cancel_token.is_cancelled() {
            return None;
        }
        let (lowered_pkg, package_callable_declarations) =
            match lower_and_resolve_full_lsp(&parsed_for_lowering, cancel_token) {
                Ok(Some(p)) => p,
                Ok(None) => return None,
                Err(diag) => {
                    return Some(Err(AnalysisFailure::from_error(diag, sources)));
                }
            };
        lowered.insert(key.clone(), lowered_pkg);
        callable_declarations.insert(key.clone(), package_callable_declarations);
        surface_modules.insert(key.clone(), parsed_for_lowering.modules);
    }
    if cancel_token.is_cancelled() {
        return None;
    }
    let lowered_workspace = PackageCollection {
        root: parsed_ws.root.clone(),
        packages: lowered,
    };

    // Build the file → module-path map before running the typer; the
    // lowered workspace has the module-path keys we need.
    let file_to_module = lsp_file_to_module(&lowered_workspace);

    // Phase 4: typecheck — collect elaborations instead of discarding
    // them, so the position index survives. The root package keeps both
    // its `Lowered` AST and its elaborations whole (the REPL `:normalize`
    // path partial-evaluates against both); any additional packages
    // contribute only their position index to the merged map.
    let mut merged_index = PositionIndex::new();
    let root_key = lowered_workspace.root.clone();
    let mut root_package_lowered = None;
    for (key, entry) in &lowered_workspace.packages {
        if cancel_token.is_cancelled() {
            return None;
        }
        let normalized = crate::pass::alpha_normalize::normalize_package(&entry.package);
        let typecheck_scope = user_elaborator_memos.scope_for(
            key,
            package_fingerprints
                .get(key)
                .expect("fingerprinted every LSP package"),
            &normalized,
        );
        let checked =
            match check_normalized_package_collect_errors_with_scope(normalized, typecheck_scope) {
                Ok(checked) => checked,
                Err(errors) => {
                    return Some(Err(AnalysisFailure::from_errors(errors, sources)));
                }
            };
        if cancel_token.is_cancelled() {
            return None;
        }
        if *key == root_key {
            merged_index.merge(checked.elaborations().position_index().clone());
            root_package_lowered = Some(entry.package.clone());
        } else {
            merged_index.merge(checked.into_elaborations().into_position_index());
        }
    }

    let root_package_lowered =
        root_package_lowered.expect("workspace always contains its root package");
    let surface_tokens = lsp_surface_tokens(&parsed_ws, &surface_modules, None, cancel_token)?;
    let mut label_reuse_indexes =
        lsp_label_reuse_indexes(&parsed_ws, &surface_tokens, cancel_token)?;
    lsp_retain_source_written_binders(
        &surface_tokens,
        &sources,
        &file_to_module,
        &mut merged_index,
        cancel_token,
    )?;
    lsp_record_label_import_binders(
        &parsed_ws,
        &surface_tokens,
        &file_to_module,
        &mut merged_index,
        &mut label_reuse_indexes,
        cancel_token,
    )?;
    lsp_record_surface_declaration_positions(
        &parsed_ws,
        &lowered_workspace,
        &surface_tokens,
        &file_to_module,
        &mut merged_index,
        cancel_token,
    )?;
    lsp_record_rec_group_positions(
        &surface_modules,
        &lowered_workspace,
        &sources,
        &file_to_module,
        &mut merged_index,
        cancel_token,
    )?;
    lsp_record_block_positions(
        &surface_modules,
        &callable_declarations,
        &file_to_module,
        &mut merged_index,
        cancel_token,
    )?;
    lsp_record_callable_declaration_positions(
        &parsed_ws,
        &callable_declarations,
        &lowered_workspace,
        &mut merged_index,
        cancel_token,
    )?;
    if cancel_token.is_cancelled() {
        return None;
    }
    let warnings = lsp_unused_binding_warnings(
        &root_package_lowered,
        &file_to_module,
        &sources,
        &merged_index,
    );

    Some(Ok(LspAnalysis {
        position_index: merged_index,
        file_to_module,
        sources,
        label_reuse_indexes,
        generated_label_nominals: lsp_generated_label_nominals(&parsed_ws, None, cancel_token)?,
        root_package_lowered,
        warnings,
    }))
}

/// Force/lower the focused module's already-selected, sorted closure without
/// letting worker completion order affect diagnostics or publish a partial
/// package. Cancellation prevents queued work from starting; the caller checks
/// every result before merging a complete green batch, and its later token
/// check prevents a concurrently cancelled request from being published.
#[cfg(all(feature = "surface", feature = "lsp"))]
fn collect_cancellable_focused_forces<T, E>(
    force_targets: &[PackageModuleKey],
    cancel_token: &crate::lsp::cancel::CancellationToken,
    force: impl Fn(&PackageModuleKey) -> Result<T, E> + Send + Sync,
) -> Option<Vec<Result<T, E>>>
where
    T: Send,
    E: Send,
{
    let results: Vec<Option<Result<T, E>>> = crate::maybe_par_iter!(force_targets)
        .map(|module_path| {
            if cancel_token.is_cancelled() {
                None
            } else {
                Some(force(module_path))
            }
        })
        .collect();
    if cancel_token.is_cancelled() || results.iter().any(Option::is_none) {
        return None;
    }
    Some(
        results
            .into_iter()
            .map(|result| result.expect("checked every focused force result for cancellation"))
            .collect(),
    )
}

/// LSP-focused variant of [`analyze_workspace_at_with_overlay_lsp`].
/// It walks and resolves the package graph as usual, but forces and
/// typechecks only the module that owns `focus_file`. Typed requests
/// that need single-module facts (hover, definition, completion
/// details) can use this as a fast freshness shard while full-package
/// diagnostics are still debounced or blocked on sibling bodies.
#[cfg(all(feature = "surface", feature = "lsp"))]
pub fn analyze_module_at_with_overlay_lsp(
    root: &Path,
    overlay: &package_collection::SourceOverlay,
    focus_file: &Path,
) -> Result<LspAnalysis, AnalysisFailure> {
    let cancel_token = crate::lsp::cancel::CancellationToken::new();
    analyze_module_at_with_overlay_lsp_cancellable(root, overlay, focus_file, &cancel_token)
        .expect("uncancelled focused LSP analysis cannot be cancelled")
}

#[cfg(all(feature = "surface", feature = "lsp"))]
pub(crate) fn analyze_module_at_with_overlay_lsp_cancellable(
    root: &Path,
    overlay: &package_collection::SourceOverlay,
    focus_file: &Path,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Option<Result<LspAnalysis, AnalysisFailure>> {
    let memos = LspUserElaboratorMemos::default();
    analyze_module_at_with_overlay_lsp_cancellable_with_memos(
        root,
        overlay,
        focus_file,
        cancel_token,
        &memos,
    )
}

#[cfg(all(feature = "surface", feature = "lsp"))]
pub(crate) fn analyze_module_at_with_overlay_lsp_cancellable_with_memos(
    root: &Path,
    overlay: &package_collection::SourceOverlay,
    focus_file: &Path,
    cancel_token: &crate::lsp::cancel::CancellationToken,
    user_elaborator_memos: &LspUserElaboratorMemos,
) -> Option<Result<LspAnalysis, AnalysisFailure>> {
    use crate::ast::Lowered;
    use crate::pass::typecheck_full::PositionIndex;

    if cancel_token.is_cancelled() {
        return None;
    }

    let parsed_ws = match package_collection::walk_with_overlay(root, overlay) {
        Ok(w) => w,
        Err((walk_err, partial_sources)) => {
            let mut sources: HashMap<PathBuf, String> = HashMap::new();
            for (k, v) in partial_sources {
                sources.insert(k, v);
            }
            if let package_collection::WalkError::Parse {
                path, source_text, ..
            } = &walk_err
            {
                sources.insert(path.clone(), source_text.clone());
            }
            let diag = walk_err.into_located();
            return Some(Err(AnalysisFailure::from_error(diag, sources)));
        }
    };
    if cancel_token.is_cancelled() {
        return None;
    }
    let package_fingerprints = lsp_user_elaborator_package_fingerprints(&parsed_ws);

    let mut sources: HashMap<PathBuf, String> = HashMap::new();
    for pkg in parsed_ws.packages.values() {
        for (k, v) in &pkg.sources {
            sources.insert(k.clone(), v.clone());
        }
    }
    let root_pkg = parsed_ws
        .packages
        .get(&parsed_ws.root)
        .expect("walk_with_overlay inserts the root package entry");
    let missing_deps = lsp_missing_materialized_dependencies(root_pkg);
    if !missing_deps.is_empty() {
        return Some(Err(AnalysisFailure::from_errors(missing_deps, sources)));
    }
    if cancel_token.is_cancelled() {
        return None;
    }

    let focus_canonical = std::fs::canonicalize(focus_file).unwrap_or_else(|_| focus_file.into());
    let cache_state = TypedCacheState::disabled(FullPipeline::CACHE_TAG);
    let mut lowered_workspace = PackageCollection {
        root: parsed_ws.root.clone(),
        packages: BTreeMap::new(),
    };
    let mut summaries: BTreeMap<
        PackageKey,
        (
            crate::pass::full::FullLoweringContext,
            PackageSummary<Lowered>,
        ),
    > = BTreeMap::new();
    let mut target: Option<(PackageKey, PackageModuleKey)> = None;

    for (key, parsed_pkg) in &parsed_ws.packages {
        if cancel_token.is_cancelled() {
            return None;
        }
        let (context, summary) =
            match build_package_summary::<FullPipeline>(key, parsed_pkg, &cache_state) {
                Ok(value) => value,
                Err(diag) => {
                    return Some(Err(AnalysisFailure::from_error(diag, sources.clone())));
                }
            };
        if cancel_token.is_cancelled() {
            return None;
        }

        let package_name = parsed_pkg
            .package_file
            .as_ref()
            .map(|e| PackageName::new(e.package_name.clone()));
        for (file_path, module) in &parsed_pkg.modules {
            if std::fs::canonicalize(file_path).unwrap_or_else(|_| file_path.clone())
                == focus_canonical
            {
                target = Some((
                    key.clone(),
                    package_storage_module_path(module, package_name.as_ref()),
                ));
                break;
            }
        }

        lowered_workspace
            .packages
            .insert(key.clone(), summary.lowered.clone());
        summaries.insert(key.clone(), (context, summary));
    }

    let Some((target_key, target_module)) = target else {
        return analyze_workspace_at_with_overlay_lsp_cancellable_with_memos(
            root,
            overlay,
            cancel_token,
            user_elaborator_memos,
        );
    };
    if cancel_token.is_cancelled() {
        return None;
    }

    let parsed_pkg = parsed_ws
        .packages
        .get(&target_key)
        .expect("target key came from parsed workspace");
    let (context, summary) = summaries
        .get_mut(&target_key)
        .expect("target key has a package summary");
    let package_name = parsed_pkg
        .package_file
        .as_ref()
        .map(|e| PackageName::new(e.package_name.clone()));
    let callable_declarations = BTreeMap::from([(
        target_key.clone(),
        context.try_lsp_source_declarations(Some(target_module.as_str()), || {
            cancel_token.is_cancelled()
        })?,
    )]);
    let mut surface_modules = BTreeMap::new();
    if let Some((file_path, module)) = parsed_pkg.modules.iter().find(|(_, module)| {
        package_storage_module_path(module, package_name.as_ref()) == target_module
    }) {
        let forced = match parsed_pkg.lazy_modules.get(file_path) {
            Some(lazy) => lazy.force_all().ok(),
            None => Some(module.clone()),
        };
        if let Some(forced) = forced {
            surface_modules.insert(target_key.clone(), vec![(file_path.clone(), forced)]);
        }
    }
    let force_targets: Vec<_> =
        same_package_dependency_closure(&target_module, parsed_pkg, package_name.as_ref())
            .into_iter()
            .collect();
    let forced_results =
        collect_cancellable_focused_forces(&force_targets, cancel_token, |module_path| {
            force_and_lower_module::<FullPipeline>(
                parsed_pkg,
                context,
                &summary.modules,
                module_path,
            )
        })?;
    if forced_results.iter().any(Result::is_err) {
        let diag = forced_results
            .into_iter()
            .find_map(Result::err)
            .expect("found a focused force error before selecting the first error");
        return Some(Err(AnalysisFailure::from_error(diag, sources.clone())));
    }
    for forced in forced_results {
        let forced = forced.expect("focused force errors returned before package update");
        summary
            .lowered
            .package
            .replace_module(forced.module_path.into_string(), forced.entry);
    }
    if cancel_token.is_cancelled() {
        return None;
    }
    let normalized = crate::pass::alpha_normalize::normalize_package(&summary.lowered.package);
    lowered_workspace
        .packages
        .insert(target_key.clone(), summary.lowered.clone());
    let entry = normalized
        .package()
        .module(target_module.as_str())
        .expect("focused module was forced into the package");
    let typecheck_scope = user_elaborator_memos.scope_for(
        &target_key,
        package_fingerprints
            .get(&target_key)
            .expect("fingerprinted every LSP package"),
        &normalized,
    );
    let typed = crate::pass::full::typecheck_module_collect_elaborations_with_typecheck_scope(
        target_module.as_str(),
        entry,
        normalized.package(),
        typecheck_scope,
    );
    let typed = match typed {
        Ok(typed) => typed,
        Err(errors) => return Some(Err(AnalysisFailure::from_errors(errors, sources.clone()))),
    };
    if cancel_token.is_cancelled() {
        return None;
    }

    let mut position_index = PositionIndex::new();
    position_index.merge(typed.elaborations.position_index().clone());

    let file_to_module = lsp_file_to_module(&lowered_workspace);
    let focused_files = surface_modules
        .values()
        .flatten()
        .map(|(file_path, _)| file_path.clone())
        .collect::<HashSet<_>>();
    let surface_tokens = lsp_surface_tokens(
        &parsed_ws,
        &surface_modules,
        Some(&focused_files),
        cancel_token,
    )?;
    let mut label_reuse_indexes =
        lsp_label_reuse_indexes(&parsed_ws, &surface_tokens, cancel_token)?;
    lsp_retain_source_written_binders(
        &surface_tokens,
        &sources,
        &file_to_module,
        &mut position_index,
        cancel_token,
    )?;
    lsp_record_label_import_binders(
        &parsed_ws,
        &surface_tokens,
        &file_to_module,
        &mut position_index,
        &mut label_reuse_indexes,
        cancel_token,
    )?;
    lsp_record_surface_declaration_positions(
        &parsed_ws,
        &lowered_workspace,
        &surface_tokens,
        &file_to_module,
        &mut position_index,
        cancel_token,
    )?;
    lsp_record_rec_group_positions(
        &surface_modules,
        &lowered_workspace,
        &sources,
        &file_to_module,
        &mut position_index,
        cancel_token,
    )?;
    lsp_record_callable_declaration_positions(
        &parsed_ws,
        &callable_declarations,
        &lowered_workspace,
        &mut position_index,
        cancel_token,
    )?;
    lsp_record_block_positions(
        &surface_modules,
        &callable_declarations,
        &file_to_module,
        &mut position_index,
        cancel_token,
    )?;
    lsp_record_missing_declaration_sites(
        &parsed_ws,
        &lowered_workspace,
        &mut position_index,
        cancel_token,
    )?;

    let root_key = lowered_workspace.root.clone();
    let root_package_lowered = lowered_workspace
        .packages
        .get(&root_key)
        .expect("workspace always contains its root package")
        .package
        .clone();
    Some(Ok(LspAnalysis {
        position_index,
        file_to_module,
        sources,
        label_reuse_indexes,
        generated_label_nominals: lsp_generated_label_nominals(
            &parsed_ws,
            Some(&focused_files),
            cancel_token,
        )?,
        root_package_lowered,
        warnings: Vec::new(),
    }))
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_missing_materialized_dependencies(parsed: &ParsedPackage) -> Vec<LocatedError> {
    parsed
        .dep_files
        .iter()
        .filter_map(|(dep_file_path, dependency)| {
            let dep_dir = parsed.root_dir.join(&dependency.name);
            let has_materialized_module = parsed.modules.iter().any(|(file_path, module)| {
                module
                    .path
                    .segments
                    .first()
                    .is_some_and(|segment| segment.name == dependency.name)
                    && file_path.starts_with(&dep_dir)
            });
            if has_materialized_module {
                return None;
            }
            let source_span = match &dependency.source.origin {
                crate::ast::SourceOrigin::Path { path_span, .. } => *path_span,
                crate::ast::SourceOrigin::Git(source) => source.url_span,
            };
            Some(LocatedError {
                file_path: dep_file_path.clone(),
                error: Error::dep(
                    source_span,
                    format!(
                        "dependency `{}` is declared but not materialized",
                        dependency.name
                    ),
                )
                .with_help(format!(
                    "run `kio dep fetch {}` to materialize the dependency",
                    dependency.name
                )),
            })
        })
        .collect()
}

#[cfg(all(feature = "surface", feature = "lsp"))]
#[derive(Debug, Clone)]
struct UnusedLocalDecl {
    file_path: PathBuf,
    module_path: String,
    name: String,
    decl_span: crate::span::Span,
    name_span: crate::span::Span,
    kind: UnusedLocalDeclKind,
}

#[cfg(all(feature = "surface", feature = "lsp"))]
#[derive(Debug, Clone)]
enum UnusedLocalDeclKind {
    Param,
    Let {
        let_span: crate::span::Span,
        body_span: crate::span::Span,
        remove_safe: bool,
    },
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_unused_binding_warnings(
    package: &Package<crate::ast::Lowered>,
    file_to_module: &BTreeMap<PathBuf, String>,
    sources: &HashMap<PathBuf, String>,
    position_index: &crate::pass::typecheck_full::PositionIndex,
) -> Vec<LspWarning> {
    use crate::pass::typecheck_full::ResolvedBinder;

    let used_decls: HashSet<(String, crate::span::Span)> = position_index
        .binders_iter()
        .filter_map(|((module_path, use_span), binder)| match binder {
            ResolvedBinder::Local {
                decl_span: Some(decl_span),
                ..
            } if use_span != decl_span => Some((module_path.clone(), *decl_span)),
            _ => None,
        })
        .collect();

    let module_to_file: BTreeMap<&str, &PathBuf> = file_to_module
        .iter()
        .map(|(file_path, module_path)| (module_path.as_str(), file_path))
        .collect();
    let mut warnings = Vec::new();
    for (module_path, entry) in package.modules() {
        let Some(file_path) = module_to_file.get(module_path) else {
            continue;
        };
        let Some(source) = sources.get(file_path.as_path()) else {
            continue;
        };
        let mut declarations = Vec::new();
        for item in &entry.module.items {
            match item {
                crate::ast::Item::FnDef(def) => {
                    collect_unused_fn_def_decls(
                        source,
                        file_path,
                        module_path,
                        def,
                        &mut declarations,
                    );
                }
                crate::ast::Item::RecGroup(group, _) => {
                    for member in &group.members {
                        collect_unused_fn_def_decls(
                            source,
                            file_path,
                            module_path,
                            member,
                            &mut declarations,
                        );
                    }
                }
                _ => {}
            }
        }
        declarations.sort_by(|a, b| {
            a.module_path
                .cmp(&b.module_path)
                .then_with(|| a.decl_span.start.cmp(&b.decl_span.start))
                .then_with(|| a.decl_span.end.cmp(&b.decl_span.end))
                .then_with(|| decl_name_span_len(a).cmp(&decl_name_span_len(b)))
        });
        declarations.dedup_by(|a, b| a.module_path == b.module_path && a.decl_span == b.decl_span);
        for decl in declarations {
            if used_decls.contains(&(decl.module_path.clone(), decl.decl_span)) {
                continue;
            }
            let mut fixes = vec![Fix::machine_applicable(
                format!("Prefix `{}` with `_`", decl.name),
                vec![FixEdit::new(decl.name_span, format!("_{}", decl.name))],
            )];
            if let UnusedLocalDeclKind::Let {
                let_span,
                body_span,
                remove_safe,
            } = decl.kind
                && remove_safe
                && let Some(body_source) = span_source(source, body_span)
            {
                fixes.push(Fix::maybe_incorrect(
                    format!("Remove unused `let {}`", decl.name),
                    vec![FixEdit::new(let_span, body_source.to_owned())],
                ));
            }
            warnings.push(LspWarning {
                file_path: decl.file_path,
                span: decl.name_span,
                message: format!("unused binding `{}`", decl.name),
                fixes,
            });
        }
    }
    warnings.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then_with(|| a.span.start.cmp(&b.span.start))
            .then_with(|| a.span.end.cmp(&b.span.end))
    });
    warnings
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn decl_name_span_len(decl: &UnusedLocalDecl) -> u32 {
    decl.name_span.end.saturating_sub(decl.name_span.start)
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn collect_unused_fn_def_decls(
    source: &str,
    file_path: &Path,
    module_path: &str,
    def: &crate::ast::FnDef<crate::ast::Lowered>,
    declarations: &mut Vec<UnusedLocalDecl>,
) {
    collect_unused_signature_decls(source, file_path, module_path, &def.sig, declarations);
    collect_unused_expr_decls(source, file_path, module_path, &def.body, declarations);
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn collect_unused_signature_decls(
    source: &str,
    file_path: &Path,
    module_path: &str,
    sig: &crate::ast::Signature<crate::ast::Lowered>,
    declarations: &mut Vec<UnusedLocalDecl>,
) {
    for param in &sig.params {
        let crate::ast::SignatureParam::Value(param) = param else {
            continue;
        };
        if param.name.starts_with('_') {
            continue;
        }
        let Some(name_span) = param_name_span(source, &param.name, param.meta.span) else {
            continue;
        };
        declarations.push(UnusedLocalDecl {
            file_path: file_path.to_path_buf(),
            module_path: module_path.to_owned(),
            name: param.name.clone(),
            decl_span: name_span,
            name_span,
            kind: UnusedLocalDeclKind::Param,
        });
    }
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn collect_unused_expr_decls(
    source: &str,
    file_path: &Path,
    module_path: &str,
    expr: &crate::ast::Expr<crate::ast::Lowered>,
    declarations: &mut Vec<UnusedLocalDecl>,
) {
    use crate::ast::{ElaboratorCall, Expr};

    match expr {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Call { callee, args, .. } => {
            collect_unused_expr_decls(source, file_path, module_path, callee, declarations);
            collect_unused_call_arg_decls(source, file_path, module_path, args, declarations);
        }
        Expr::FnExpr { sig, body, .. } => {
            collect_unused_signature_decls(source, file_path, module_path, sig, declarations);
            collect_unused_expr_decls(source, file_path, module_path, body, declarations);
        }
        Expr::Let {
            name,
            name_span,
            value,
            body,
            meta,
            ..
        } => {
            // A `let` the user wrote spells its name at its name span. One a
            // lowering pass minted does not — its span points at whatever user
            // text the pass was rewriting — so the source text at the span is a
            // structural test for "did a human write this binding?", where the
            // underscore convention is only a naming discipline a new lowering
            // can forget. It is also what makes the quickfixes safe: their edits
            // are derived from this span, so a span that isn't the name would
            // rewrite whatever *is* there — a whole `fn`, in the case that
            // prompted this.
            if !name.starts_with('_') && span_source(source, *name_span) == Some(name.as_str()) {
                let remove_safe = span_between(source, meta.span.start, body.span().start)
                    .is_some_and(|prefix| !prefix.contains("//"));
                declarations.push(UnusedLocalDecl {
                    file_path: file_path.to_path_buf(),
                    module_path: module_path.to_owned(),
                    name: name.clone(),
                    decl_span: *name_span,
                    name_span: *name_span,
                    kind: UnusedLocalDeclKind::Let {
                        let_span: meta.span,
                        body_span: body.span(),
                        remove_safe,
                    },
                });
            }
            collect_unused_expr_decls(source, file_path, module_path, value, declarations);
            collect_unused_expr_decls(source, file_path, module_path, body, declarations);
        }
        Expr::Seq { value, body, .. } => {
            collect_unused_expr_decls(source, file_path, module_path, value, declarations);
            collect_unused_expr_decls(source, file_path, module_path, body, declarations);
        }
        Expr::Elaborator { call, .. } => match call {
            ElaboratorCall::FieldAccess { receiver, .. } => {
                collect_unused_expr_decls(source, file_path, module_path, receiver, declarations);
            }
            ElaboratorCall::FieldUpdate {
                receiver, updates, ..
            } => {
                collect_unused_expr_decls(source, file_path, module_path, receiver, declarations);
                for update in updates {
                    collect_unused_expr_decls(
                        source,
                        file_path,
                        module_path,
                        &update.value,
                        declarations,
                    );
                }
            }
        },
        Expr::RecQuote { plan, .. } => {
            for expression in plan.expressions() {
                collect_unused_expr_decls(source, file_path, module_path, expression, declarations);
            }
        }
        Expr::RecOrder { plan, .. } => {
            collect_unused_expr_decls(source, file_path, module_path, &plan.value, declarations);
            collect_unused_expr_decls(source, file_path, module_path, &plan.body, declarations);
        }
        Expr::UserElaborator { args, .. } => {
            collect_unused_call_arg_decls(source, file_path, module_path, args, declarations);
        }
        Expr::Ufcs { receiver, args, .. } => {
            collect_unused_expr_decls(source, file_path, module_path, receiver, declarations);
            collect_unused_call_arg_decls(source, file_path, module_path, args, declarations);
        }
        Expr::Path { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => {}
        Expr::RecCall { ext, .. }
        | Expr::RowLet { ext, .. }
        | Expr::Tuple { ext, .. }
        | Expr::FnPlaceholder { ext, .. }
        | Expr::LabelValue { ext, .. }
        | Expr::OpChain { ext, .. }
        | Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
        | Expr::EnrichedRecord { ext, .. }
        | Expr::EnrichedFieldGet { ext, .. }
        | Expr::LowHostCall { ext, .. }
        | Expr::LowModuleCall { ext, .. }
        | Expr::LowQualifiedModuleCall { ext, .. }
        | Expr::LowQualifiedNewtypeMember { ext, .. }
        | Expr::LowNewtypeCtor { ext, .. }
        | Expr::LowNewtypeProj { ext, .. }
        | Expr::LowClosureCall { ext, .. }
        | Expr::LowIndirectCall { ext, .. }
        | Expr::LowTypeApplication { ext, .. }
        | Expr::LowAbsurdCall { ext, .. }
        | Expr::LowCpsProjectorApply { ext, .. }
        | Expr::LowBoundRef { ext, .. }
        | Expr::LowHostFnValueRef { ext, .. }
        | Expr::LowModuleFnValueRef { ext, .. } => match *ext {},
    }
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn collect_unused_call_arg_decls(
    source: &str,
    file_path: &Path,
    module_path: &str,
    args: &[crate::ast::CallArg<crate::ast::Lowered>],
    declarations: &mut Vec<UnusedLocalDecl>,
) {
    for arg in args {
        if let crate::ast::CallArg::Value(value) = arg {
            collect_unused_expr_decls(source, file_path, module_path, value, declarations);
        }
    }
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn param_name_span(source: &str, name: &str, span: crate::span::Span) -> Option<crate::span::Span> {
    let start = usize::try_from(span.start).ok()?;
    let end = start.checked_add(name.len())?;
    let slice = source.get(start..end)?;
    (slice == name).then_some(crate::span::Span::new(span.start, end.try_into().ok()?))
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn span_source(source: &str, span: crate::span::Span) -> Option<&str> {
    source.get(usize::try_from(span.start).ok()?..usize::try_from(span.end).ok()?)
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn span_between(source: &str, start: u32, end: u32) -> Option<&str> {
    source.get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_user_elaborator_package_fingerprints(
    parsed_ws: &package_collection::ParsedPackageCollection,
) -> BTreeMap<PackageKey, SourceHash> {
    parsed_ws
        .packages
        .iter()
        .map(|(key, parsed_pkg)| {
            let source_hash = crate::cache::package_check::source_hash(&parsed_pkg.sources);
            let mut h = blake3::Hasher::new();
            write_fingerprint_part(&mut h, b"lsp-user-elaborator-package");
            write_fingerprint_part(&mut h, source_hash.as_str().as_bytes());
            (
                key.clone(),
                SourceHash::new(h.finalize().to_hex().to_string()),
            )
        })
        .collect()
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lsp_file_to_module<P: crate::ast::Phase>(
    workspace: &PackageCollection<P>,
) -> BTreeMap<PathBuf, String> {
    let mut file_to_module = BTreeMap::new();
    for entry in workspace.packages.values() {
        for (module_path, mod_entry) in entry.package.modules() {
            let file_path = std::fs::canonicalize(&mod_entry.file_path)
                .unwrap_or_else(|_| mod_entry.file_path.clone());
            file_to_module.insert(file_path, module_path.to_owned());
        }
    }
    file_to_module
}

/// Result of [`analyze_root_typed_with_overlay`]: the root package's
/// fully-substituted and Prime-validated [`Package<Prime>`] paired
/// with the position index from the same typecheck.
///
/// The `kio repl` expression-query path consumes both: the typed
/// `Package<Prime>` carries the elaborated Kio' AST a query renders,
/// and the [`crate::pass::typecheck_full::PositionIndex`] carries the
/// per-node synthesized types `:t <expr>` reports.
#[cfg(all(feature = "surface", feature = "lsp"))]
pub struct RootTypedAnalysis {
    /// The root package, type-checked with every elaboration
    /// substituted in and validated as Prime — the same
    /// `Package<Prime>` the `kio build` path produces.
    pub root_package: Package<Prime>,
    /// The checked, normalization-bearing Lowered root package. The REPL
    /// consumes evaluator artifacts only through this proof-bearing pair.
    #[cfg(feature = "repl-core")]
    pub(crate) checked_lowered: crate::pass::typecheck_full::CheckedLoweredPackage,
    /// The position-keyed index from the root package's typecheck:
    /// per-expression-node synthesized types keyed by
    /// `(module_path, Span)`.
    pub position_index: crate::pass::typecheck_full::PositionIndex,
}

/// Analyze the package at `root` with an overlay, returning the root
/// package's typed AST *and* its position index from one typecheck.
///
/// Like [`analyze_workspace_at_with_overlay_lsp`], but keeps the
/// substituted and validated [`Package<Prime>`] rather than discarding
/// it — the `kio repl` expression-query path needs the elaborated AST to
/// render an expression's Kio' form, and the position index to report
/// its type. Both come from the single typecheck this runs.
///
/// `overlay` redirects per-file reads to in-memory text (the REPL
/// populates it with a synthetic module body wrapping the queried
/// expression); an empty overlay reads every file from disk.
#[cfg(all(feature = "surface", feature = "lsp"))]
pub fn analyze_root_typed_with_overlay(
    root: &Path,
    overlay: &package_collection::SourceOverlay,
) -> Result<RootTypedAnalysis, AnalysisFailure> {
    use crate::ast::Lowered;
    use crate::pass::typecheck_full::check_package_preserving_normalization_collect_errors;

    // Phase 1: walk + parse.
    let parsed_ws = match package_collection::walk_with_overlay(root, overlay) {
        Ok(w) => w,
        Err((walk_err, partial_sources)) => {
            let mut sources: HashMap<PathBuf, String> = HashMap::new();
            for (k, v) in partial_sources {
                sources.insert(k, v);
            }
            if let package_collection::WalkError::Parse {
                path, source_text, ..
            } = &walk_err
            {
                sources.insert(path.clone(), source_text.clone());
            }
            let diag = walk_err.into_located();
            return Err(AnalysisFailure::from_error(diag, sources));
        }
    };

    let mut sources: HashMap<PathBuf, String> = HashMap::new();
    for pkg in parsed_ws.packages.values() {
        for (k, v) in &pkg.sources {
            sources.insert(k.clone(), v.clone());
        }
    }

    // Phase 2: lower + resolve.
    let mut lowered: BTreeMap<PackageKey, package_collection::PackageEntry<Lowered>> =
        BTreeMap::new();
    for (key, parsed_pkg) in &parsed_ws.packages {
        if let Err(diag) = validate_source_package_with_parse_precedence::<FullPipeline>(parsed_pkg)
        {
            return Err(AnalysisFailure::from_error(diag, sources));
        }
        let parsed_for_lowering = match force_parsed_package_bodies(parsed_pkg) {
            Ok(p) => p,
            Err(diag) => {
                return Err(AnalysisFailure::from_error(diag, sources));
            }
        };
        let lowered_pkg = match lower_and_resolve::<FullPipeline>(&parsed_for_lowering) {
            Ok(p) => p,
            Err(diag) => {
                return Err(AnalysisFailure::from_error(diag, sources));
            }
        };
        lowered.insert(key.clone(), lowered_pkg);
    }
    let lowered_workspace = PackageCollection {
        root: parsed_ws.root.clone(),
        packages: lowered,
    };

    // Phase 4: typecheck every package; the root package keeps both
    // its substituted and validated `Package<Prime>` and its position
    // index.
    let root_key = lowered_workspace.root.clone();
    let mut root_typed: Option<RootTypedAnalysis> = None;
    for (key, entry) in &lowered_workspace.packages {
        let checked = match check_package_preserving_normalization_collect_errors(&entry.package) {
            Ok(checked) => checked,
            Err(errors) => {
                return Err(AnalysisFailure::from_errors(errors, sources));
            }
        };
        let validated = match crate::pass::typecheck_full::validate_substituted_package(
            checked.substituted(),
        ) {
            Ok(package) => package,
            Err(error) => {
                return Err(AnalysisFailure::from_error(error, sources));
            }
        };
        if *key == root_key {
            let root_package = validated;
            let position_index = checked.elaborations().position_index().clone();
            root_typed = Some(RootTypedAnalysis {
                root_package,
                #[cfg(feature = "repl-core")]
                checked_lowered: checked,
                position_index,
            });
        }
    }

    Ok(root_typed.expect("workspace always contains its root package"))
}

/// Typed package produced by [`compile_workspace_with`].
///
/// `root_package` is `Option` because the `kio check` path
/// (`skip_ok = true`) skips re-typechecking a package whose
/// package-check cache is still valid. The `kio build` path passes
/// `skip_ok = false`, so it always sees `Some`.
pub struct TypedPackageCollection {
    pub root_key: PackageKey,
    pub root_package: Option<Package<Prime>>,
}

/// Structured analysis failure returned by [`analyze_workspace_at`]
/// and the `*_inner` variants of the workspace pipeline. Holds the
/// [`LocatedError`] values the pipeline produced at the earliest
/// failing phase plus the per-file source map the pipeline accumulated
/// up to that point.
///
/// CLI entry points wrap this through [`report_located`] (which
/// prints the primary diagnostic to stderr and returns the matching
/// exit code). In-process callers — notably the LSP server in
/// [`crate::lsp`] — consume every `LocatedError`: each maps to one LSP
/// `Diagnostic`, and the source map serves both the primary and exact related
/// files' `BytePos → Position` conversion.
#[derive(Debug)]
pub struct AnalysisFailure {
    pub errors: Vec<LocatedError>,
    pub sources: Box<HashMap<PathBuf, String>>,
}

impl AnalysisFailure {
    pub fn from_error(error: LocatedError, sources: HashMap<PathBuf, String>) -> Self {
        Self {
            errors: vec![error],
            sources: Box::new(sources),
        }
    }

    pub fn from_errors(errors: Vec<LocatedError>, sources: HashMap<PathBuf, String>) -> Self {
        let errors = canonical_analysis_errors(errors);
        Self {
            errors,
            sources: Box::new(sources),
        }
    }

    pub fn primary_error(&self) -> &LocatedError {
        self.errors
            .first()
            .expect("AnalysisFailure requires at least one located error")
    }
}

struct PackageStageResult<T> {
    key: PackageKey,
    result: Result<T, Vec<LocatedError>>,
}

enum PackageTypecheckOutcome {
    Skipped,
    Checked(Box<Package<Prime>>),
}

struct PackagePipelineOutcome<P: crate::ast::Phase> {
    lowered: Option<PackageEntry<P>>,
    typechecked: PackageTypecheckOutcome,
}

struct FrontendTiming {
    prepare: Duration,
    forced_modules: usize,
    typed_hits: usize,
    typed_misses: usize,
    package_check_lookup: Duration,
    typed_cache_lookup: Duration,
    typed_cache_store: Duration,
    lower_resolve: Duration,
    pipeline_typecheck: Duration,
    prime_validation: Duration,
    package_check_skipped: bool,
    import_body_type: Duration,
    summary_levels: usize,
    summary_max_width: usize,
    user_elaborator_timing: crate::pass::typecheck_core::UserElaboratorTimingSnapshot,
}

impl FrontendTiming {
    fn typecheck_total(&self) -> Duration {
        self.pipeline_typecheck + self.prime_validation
    }
}

struct FrontendWorkspaceTiming {
    walk: Duration,
    source_map: Duration,
    package_cache_state: Duration,
    typed_cache_state: Duration,
    packages: usize,
}

fn frontend_timing_log_enabled() -> bool {
    crate::timing::frontend_enabled()
}

fn log_frontend_workspace_timing_line(root_key: &PackageKey, timing: FrontendWorkspaceTiming) {
    eprintln!(
        "frontend-workspace-timing: {} walk_ms={:.3} source_map_ms={:.3} \
         package_cache_state_ms={:.3} typed_cache_state_ms={:.3} packages={}",
        root_key.package_name,
        duration_ms(timing.walk),
        duration_ms(timing.source_map),
        duration_ms(timing.package_cache_state),
        duration_ms(timing.typed_cache_state),
        timing.packages,
    );
}

fn frontend_timing_line(package_key: &PackageKey, timing: &FrontendTiming) -> String {
    format!(
        "frontend-timing: {} prepare_ms={:.3} forced_modules={} typed_hits={} \
         typed_misses={} package_check_lookup_ms={:.3} typed_cache_lookup_ms={:.3} \
         typed_cache_store_ms={:.3} lower_resolve_ms={:.3} typecheck_ms={:.3} \
         pipeline_typecheck_ms={:.3} prime_validation_ms={:.3} \
         package_check_skipped={} import_body_type_ms={:.3} summary_levels={} summary_max_width={} \
         user_elaborator_prepared_eval_ms={:.3} user_elaborator_prepared_hits={} \
         user_elaborator_prepared_misses={} user_elaborator_prepared_first_writes={} \
         user_elaborator_artifact_hits={} user_elaborator_artifact_misses={} \
         user_elaborator_artifact_first_writes={} \
         user_elaborator_template_eval_ms={:.3} \
         user_elaborator_template_replay_ms={:.3} \
         user_elaborator_template_memo_total_ms={:.3} \
         user_elaborator_template_batch_obligations={} user_elaborator_template_batch_unique={} \
         user_elaborator_template_batch_key_ms={:.3} user_elaborator_template_batch_compute_ms={:.3} \
         user_elaborator_template_batch_replay_loop_ms={:.3} \
         user_elaborator_template_batch_duplicates={}{}",
        package_key.package_name,
        duration_ms(timing.prepare),
        timing.forced_modules,
        timing.typed_hits,
        timing.typed_misses,
        duration_ms(timing.package_check_lookup),
        duration_ms(timing.typed_cache_lookup),
        duration_ms(timing.typed_cache_store),
        duration_ms(timing.lower_resolve),
        duration_ms(timing.typecheck_total()),
        duration_ms(timing.pipeline_typecheck),
        duration_ms(timing.prime_validation),
        usize::from(timing.package_check_skipped),
        duration_ms(timing.import_body_type),
        timing.summary_levels,
        timing.summary_max_width,
        duration_ms(timing.user_elaborator_timing.prepared_eval),
        timing.user_elaborator_timing.prepared_hits,
        timing.user_elaborator_timing.prepared_misses,
        timing.user_elaborator_timing.prepared_first_writes,
        timing.user_elaborator_timing.artifact_hits,
        timing.user_elaborator_timing.artifact_misses,
        timing.user_elaborator_timing.artifact_first_writes,
        duration_ms(timing.user_elaborator_timing.template_eval),
        duration_ms(timing.user_elaborator_timing.template_replay),
        duration_ms(timing.user_elaborator_timing.template_memo_total),
        timing.user_elaborator_timing.template_batch_obligations,
        timing.user_elaborator_timing.template_batch_unique,
        duration_ms(timing.user_elaborator_timing.template_batch_key),
        duration_ms(timing.user_elaborator_timing.template_batch_compute),
        duration_ms(timing.user_elaborator_timing.template_batch_replay_loop),
        timing
            .user_elaborator_timing
            .template_batch_obligations
            .saturating_sub(timing.user_elaborator_timing.template_batch_unique),
        user_elaborator_eval_timing_fields(&timing.user_elaborator_timing),
    )
}

fn log_frontend_timing_line(package_key: &PackageKey, timing: FrontendTiming) {
    eprintln!("{}", frontend_timing_line(package_key, &timing));
}

fn log_frontend_package_store_timing_line(package_key: &PackageKey, duration: Duration) {
    eprintln!(
        "frontend-package-cache-store-timing: {} package_check_store_ms={:.3}",
        package_key.package_name,
        duration_ms(duration),
    );
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

#[cfg(feature = "surface")]
fn user_elaborator_eval_timing_fields(
    timing: &crate::pass::typecheck_core::UserElaboratorTimingSnapshot,
) -> String {
    let eval = &timing.eval;
    format!(
        " user_elaborator_eval_root_ms={:.3} user_elaborator_eval_root_calls={} \
         user_elaborator_eval_expr_visits={} user_elaborator_eval_apply_ms={:.3} \
         user_elaborator_eval_apply_calls={} user_elaborator_eval_apply_generic_ms={:.3} \
         user_elaborator_eval_apply_generic_calls={} user_elaborator_eval_apply_atom_ms={:.3} \
         user_elaborator_eval_apply_atom_calls={} user_elaborator_eval_apply_fn_ms={:.3} \
         user_elaborator_eval_apply_fn_calls={} user_elaborator_eval_closure_apply_ms={:.3} \
         user_elaborator_eval_closure_apply_calls={} user_elaborator_eval_comptime_ms={:.3} \
         user_elaborator_eval_comptime_calls={} user_elaborator_eval_comptime_type_view_ms={:.3} \
         user_elaborator_eval_comptime_type_view_calls={} user_elaborator_eval_comptime_type_equal_ms={:.3} \
         user_elaborator_eval_comptime_type_equal_calls={} user_elaborator_eval_comptime_type_fold_ms={:.3} \
         user_elaborator_eval_comptime_type_fold_calls={} user_elaborator_eval_comptime_term_ms={:.3} \
         user_elaborator_eval_comptime_term_calls={} user_elaborator_eval_comptime_term_let_ms={:.3} \
         user_elaborator_eval_comptime_term_let_calls={} user_elaborator_eval_comptime_term_fn_ms={:.3} \
         user_elaborator_eval_comptime_term_fn_calls={} user_elaborator_eval_comptime_term_call_ms={:.3} \
         user_elaborator_eval_comptime_term_call_calls={} user_elaborator_eval_comptime_term_type_ms={:.3} \
         user_elaborator_eval_comptime_term_type_calls={} user_elaborator_eval_comptime_term_unit_ms={:.3} \
         user_elaborator_eval_comptime_term_unit_calls={} user_elaborator_eval_comptime_intrinsic_pair_ms={:.3} \
         user_elaborator_eval_comptime_intrinsic_pair_calls={} user_elaborator_eval_comptime_intrinsic_projection_ms={:.3} \
         user_elaborator_eval_comptime_intrinsic_projection_calls={} user_elaborator_eval_comptime_intrinsic_injection_ms={:.3} \
         user_elaborator_eval_comptime_intrinsic_injection_calls={} user_elaborator_eval_comptime_intrinsic_either_ms={:.3} \
         user_elaborator_eval_comptime_intrinsic_either_calls={} user_elaborator_eval_comptime_intrinsic_other_ms={:.3} \
         user_elaborator_eval_comptime_intrinsic_other_calls={} user_elaborator_eval_comptime_structural_recur_ms={:.3} \
         user_elaborator_eval_comptime_structural_recur_calls={} user_elaborator_eval_comptime_other_ms={:.3} \
         user_elaborator_eval_comptime_other_calls={} user_elaborator_eval_type_unfold_ms={:.3} \
         user_elaborator_eval_type_unfold_calls={} user_elaborator_eval_type_equiv_ms={:.3} \
         user_elaborator_eval_type_equiv_calls={} user_elaborator_eval_nf_eq_ms={:.3} \
         user_elaborator_eval_nf_eq_calls={} user_elaborator_eval_eta_contract_ms={:.3} \
         user_elaborator_eval_eta_contract_calls={} user_elaborator_eval_env_clones={} \
         user_elaborator_eval_closure_builds={} user_elaborator_eval_arg_vecs={} \
         user_elaborator_eval_arg_values={} user_elaborator_eval_capture_vecs={} \
         user_elaborator_eval_capture_values={} user_elaborator_eval_exact_memo_hits={} \
         user_elaborator_eval_exact_memo_misses={} user_elaborator_eval_exact_memo_key_skips={} \
         user_elaborator_eval_exact_memo_stores={} user_elaborator_eval_hot_functions={}",
        duration_ms(eval.root_eval),
        eval.root_eval_calls,
        eval.expr_visits,
        duration_ms(eval.apply),
        eval.apply_calls,
        duration_ms(eval.apply_generic),
        eval.apply_generic_calls,
        duration_ms(eval.apply_atom),
        eval.apply_atom_calls,
        duration_ms(eval.apply_fn),
        eval.apply_fn_calls,
        duration_ms(eval.closure_apply),
        eval.closure_apply_calls,
        duration_ms(eval.reflection),
        eval.reflection_calls,
        duration_ms(eval.reflection_type_view),
        eval.reflection_type_view_calls,
        duration_ms(eval.reflection_type_equal),
        eval.reflection_type_equal_calls,
        duration_ms(eval.reflection_type_fold),
        eval.reflection_type_fold_calls,
        duration_ms(eval.reflection_term),
        eval.reflection_term_calls,
        duration_ms(eval.reflection_term_let),
        eval.reflection_term_let_calls,
        duration_ms(eval.reflection_term_fn),
        eval.reflection_term_fn_calls,
        duration_ms(eval.reflection_term_call),
        eval.reflection_term_call_calls,
        duration_ms(eval.reflection_term_type),
        eval.reflection_term_type_calls,
        duration_ms(eval.reflection_term_unit),
        eval.reflection_term_unit_calls,
        duration_ms(eval.reflection_intrinsic_pair),
        eval.reflection_intrinsic_pair_calls,
        duration_ms(eval.reflection_intrinsic_projection),
        eval.reflection_intrinsic_projection_calls,
        duration_ms(eval.reflection_intrinsic_injection),
        eval.reflection_intrinsic_injection_calls,
        duration_ms(eval.reflection_intrinsic_either),
        eval.reflection_intrinsic_either_calls,
        duration_ms(eval.reflection_intrinsic_other),
        eval.reflection_intrinsic_other_calls,
        duration_ms(eval.reflection_structural_recur),
        eval.reflection_structural_recur_calls,
        duration_ms(eval.reflection_other),
        eval.reflection_other_calls,
        duration_ms(eval.type_unfold),
        eval.type_unfold_calls,
        duration_ms(eval.type_equiv),
        eval.type_equiv_calls,
        duration_ms(eval.nf_eq),
        eval.nf_eq_calls,
        duration_ms(eval.eta_contract),
        eval.eta_contract_calls,
        eval.env_clones,
        eval.closure_builds,
        eval.arg_vec_builds,
        eval.arg_vec_values,
        eval.capture_vec_builds,
        eval.capture_vec_values,
        eval.exact_call_memo_hits,
        eval.exact_call_memo_misses,
        eval.exact_call_memo_key_skips,
        eval.exact_call_memo_stores,
        format_eval_hot_functions(&eval.function_hotspots),
    )
}

#[cfg(feature = "surface")]
fn format_eval_hot_functions(hotspots: &[crate::normalization::EvalFunctionHotspot]) -> String {
    if hotspots.is_empty() {
        return "-".to_string();
    }
    hotspots
        .iter()
        .take(12)
        .map(|hotspot| {
            format!(
                "{}:{:.3}:{}",
                sanitize_timing_token(&hotspot.label),
                duration_ms(hotspot.duration),
                hotspot.calls
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(feature = "surface")]
fn sanitize_timing_token(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            ' ' | '\t' | '\n' | '\r' | ',' | '=' => '_',
            _ => ch,
        })
        .collect()
}

#[cfg(not(feature = "surface"))]
fn user_elaborator_eval_timing_fields(
    _timing: &crate::pass::typecheck_core::UserElaboratorTimingSnapshot,
) -> &'static str {
    ""
}

fn one_stage_error(error: LocatedError) -> Vec<LocatedError> {
    vec![error]
}

fn canonical_analysis_errors(mut errors: Vec<LocatedError>) -> Vec<LocatedError> {
    assert!(
        !errors.is_empty(),
        "AnalysisFailure requires at least one located error"
    );
    let earliest = errors
        .iter()
        .map(|error| error.error.exit_code().as_i32())
        .min()
        .expect("errors is non-empty");
    errors.retain(|error| error.error.exit_code().as_i32() == earliest);
    errors.sort_by(|a, b| {
        let (a_span, a_message) = a.error.diag();
        let (b_span, b_message) = b.error.diag();
        a.file_path
            .cmp(&b.file_path)
            .then_with(|| a_span.start.cmp(&b_span.start))
            .then_with(|| a_span.end.cmp(&b_span.end))
            .then_with(|| a_message.cmp(b_message))
    });
    errors
}

fn package_stage_errors<T>(results: &[PackageStageResult<T>]) -> Vec<LocatedError> {
    let mut errors: Vec<_> = results
        .iter()
        .flat_map(|result| {
            result.result.as_ref().err().into_iter().flat_map(|errors| {
                errors.iter().map(|error| {
                    let (span, _) = error.error.diag();
                    (
                        result.key.clone(),
                        error.file_path.clone(),
                        span,
                        LocatedError {
                            file_path: error.file_path.clone(),
                            error: error.error.clone(),
                        },
                    )
                })
            })
        })
        .collect();
    errors.sort_by(|a, b| {
        a.0.package_name
            .cmp(&b.0.package_name)
            .then_with(|| a.0.canonical_dir.cmp(&b.0.canonical_dir))
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.start.cmp(&b.2.start))
            .then_with(|| a.2.end.cmp(&b.2.end))
    });
    errors.into_iter().map(|(_, _, _, error)| error).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> PackageKey {
        PackageKey {
            canonical_dir: PathBuf::from(format!("/{name}")),
            package_name: name.to_owned(),
        }
    }

    #[test]
    fn frontend_timing_formats_pipeline_and_prime_validation_split() {
        let timing = FrontendTiming {
            prepare: Duration::ZERO,
            forced_modules: 0,
            typed_hits: 0,
            typed_misses: 0,
            package_check_lookup: Duration::ZERO,
            typed_cache_lookup: Duration::ZERO,
            typed_cache_store: Duration::ZERO,
            lower_resolve: Duration::ZERO,
            pipeline_typecheck: Duration::from_micros(1_250),
            prime_validation: Duration::from_micros(2_500),
            package_check_skipped: false,
            import_body_type: Duration::ZERO,
            summary_levels: 0,
            summary_max_width: 0,
            user_elaborator_timing: Default::default(),
        };

        let line = frontend_timing_line(&key("pkg"), &timing);
        assert!(
            line.contains(
                "typecheck_ms=3.750 pipeline_typecheck_ms=1.250 prime_validation_ms=2.500"
            ),
            "the aggregate and exact stage split should remain adjacent and comparable: {line}"
        );
    }

    #[cfg(feature = "prime")]
    #[test]
    fn scheduled_package_assembly_reestablishes_alpha_normalization() {
        let parsed = crate::pass::parser::parse(
            "module main;
             fn outer[A](seed: A) -> . {
                 let inner = .[A](value: A) { value };
                 ()
             }",
        )
        .expect("parse");
        let prime = crate::prime::lower::lower_module(parsed).expect("lower Kio'");
        let package = Package::build(
            Path::new(""),
            vec![(PathBuf::from("main.kio"), prime)],
            None,
        )
        .expect("build package");
        let normalized = crate::pass::alpha_normalize::normalize_package(&package);
        let (typed_modules, package_file) = package.into_parts();

        let assembled =
            assemble_scheduled_typecheck_package(normalized, typed_modules, package_file);
        let assembled_module = assembled
            .package()
            .module("main")
            .expect("main module")
            .module
            .clone();
        let [crate::ast::Item::FnDef(outer)] = assembled_module.items.as_slice() else {
            panic!("expected outer function");
        };
        let crate::ast::Expr::Let { value, .. } = &outer.body else {
            panic!("expected let body");
        };
        let crate::ast::Expr::FnExpr { sig, .. } = value.as_ref() else {
            panic!("expected inner function");
        };
        let [crate::ast::SignatureParam::Type(inner), ..] = sig.params.as_slice() else {
            panic!("expected inner type binder");
        };
        assert_eq!(inner.name, "A_n2");

        let repeated = assembled.renormalize_after(|package| package);
        assert_eq!(
            repeated
                .package()
                .module("main")
                .expect("main module")
                .module,
            assembled_module
        );
    }

    #[cfg(feature = "prime")]
    #[test]
    fn scheduled_typed_module_entry_indexes_the_transformed_module() {
        let parsed =
            crate::pass::parser::parse("module main; fn keep() -> . { () }").expect("parse module");
        let module = crate::prime::lower::lower_module(parsed).expect("lower Kio'");
        let package = Package::build(
            Path::new(""),
            vec![(PathBuf::from("main.kio"), module)],
            None,
        )
        .expect("build package");
        let original = package.module("main").expect("main module");
        let mut transformed = original.module.clone();
        transformed.imports.push(crate::ast::Import {
            trailing_trivia: Vec::new(),
            kind: crate::ast::ImportKind::Qualified {
                path: crate::ast::ModulePath {
                    segments: vec![crate::ast::PathSegment::new(
                        "provider".to_owned(),
                        crate::span::Span::new(0, 0),
                    )],
                    span: crate::span::Span::new(0, 0),
                },
                alias: "_q0".to_owned(),
            },
            span: crate::span::Span::new(0, 0),
            leading_trivia: Vec::new(),
        });
        let typed = scheduled_typed_module_entry(original.file_path.clone(), transformed);
        let qualified = crate::pass::resolve::qualify_type_segments_in_entry(
            &[
                crate::ast::PathSegment::new("_q0".to_owned(), crate::span::Span::new(0, 0)),
                crate::ast::PathSegment::new("I32".to_owned(), crate::span::Span::new(0, 0)),
            ],
            &typed,
            &std::collections::HashMap::new(),
        );
        assert_eq!(
            qualified
                .iter()
                .map(crate::ast::PathSegment::as_str)
                .collect::<Vec<_>>(),
            vec!["provider", "I32"],
            "the scheduled entry must derive its scope from the transformed Prime module"
        );
    }

    #[cfg(all(feature = "surface", feature = "prime"))]
    #[test]
    fn scheduled_package_assembly_indexes_generated_prime_imports() {
        let temp = tempfile::tempdir().expect("temporary package");
        std::fs::write(temp.path().join("root.pkg.kio"), "package root;")
            .expect("write package file");
        std::fs::create_dir(temp.path().join("pkg")).expect("create module directory");
        std::fs::write(
            temp.path().join("pkg/refl.kio"),
            "module pkg/refl; \
             import __comptime__; \
             pub pure fn make_rep_impl(ct: __Comptime__, _rep_type: __Type__, ctor: __Checked_term__, _source: __Type__) -> __Checked_term__ { \
               __term_call__(ct, __term_type__(ct, ctor), ctor, __term_unit__(ct)) \
             } \
             pub type Rep_payload = .; \
             pub newtype Rep : Rep_payload { pub constructor mk_rep; pub projector un_rep; }; \
             pub elab make_rep : [T] . -> Rep { captures (Rep, Rep.mk_rep); impl make_rep_impl; }; \
             fn keep() -> . { () }",
        )
        .expect("write elaborator module");
        std::fs::write(
            temp.path().join("pkg/main.kio"),
            "module pkg/main; \
             import pkg/refl(Rep); \
             import pkg/refl(make_rep); \
             fn main() -> Rep { make_rep!(., ()) }",
        )
        .expect("write consumer module");

        let typed = compile_workspace_at(temp.path(), false, false)
            .expect("the scheduled full pipeline compiles the package")
            .root_package
            .expect("skip_ok=false returns the typed root package");
        let main = typed.module("pkg/main").expect("main module");
        let generated_alias = main
            .module
            .imports
            .iter()
            .find_map(|usage| match &usage.kind {
                crate::ast::ImportKind::Qualified { path, alias }
                    if path
                        .segments
                        .iter()
                        .map(crate::ast::PathSegment::as_str)
                        .eq(["pkg", "refl"]) =>
                {
                    Some(alias.as_str())
                }
                _ => None,
            })
            .expect("capture replay injects an exact provider import");
        let qualified = crate::pass::resolve::qualify_type_segments_in_entry(
            &[
                crate::ast::PathSegment::new(
                    generated_alias.to_owned(),
                    crate::span::Span::new(0, 0),
                ),
                crate::ast::PathSegment::new("Rep".to_owned(), crate::span::Span::new(0, 0)),
            ],
            main,
            &std::collections::HashMap::new(),
        );
        assert_eq!(
            qualified
                .iter()
                .map(crate::ast::PathSegment::as_str)
                .collect::<Vec<_>>(),
            vec!["pkg", "refl", "Rep"],
            "the scheduler must derive the scope from the transformed Prime module"
        );
    }

    /// `run` rejects unknown flags before any disk work, mirroring
    /// `kio test`. Positional selectors are accepted (per
    /// `specs/cli.md` § `kio check`).
    #[test]
    fn run_with_unknown_flag_returns_usage_exit() {
        assert_eq!(run(&["--bogus".to_owned()], false), ExitCode::Usage);
    }

    /// Help flags route through `Success` without touching disk.
    #[test]
    fn run_with_help_returns_success() {
        assert_eq!(run(&["-h".to_owned()], false), ExitCode::Success);
        assert_eq!(run(&["--help".to_owned()], false), ExitCode::Success);
    }

    /// A directory with zero `.kio` files is a usage error on the
    /// no-selector path; a directory with at least one `.kio` file is
    /// not flagged here.
    #[test]
    fn error_if_no_kio_files_flags_only_the_empty_directory() {
        let empty = tempfile::tempdir().expect("tempdir");
        assert_eq!(error_if_no_kio_files(empty.path()), Err(ExitCode::Usage));

        let nonempty = tempfile::tempdir().expect("tempdir");
        std::fs::write(nonempty.path().join("a.kio"), "module a;\n").expect("write module");
        assert_eq!(error_if_no_kio_files(nonempty.path()), Ok(()));
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    fn parsed_package_with_dependency(materialized: bool) -> ParsedPackage {
        let root_dir = PathBuf::from("/pkg");
        let dep_file = crate::ast::DependencyFile {
            name: "foo".to_owned(),
            source: crate::ast::SourceBlock {
                trailing_trivia: Vec::new(),
                origin: crate::ast::SourceOrigin::Path {
                    path: "../foo/foo.pkg.kio".to_owned(),
                    path_span: crate::span::Span::new(32, 52),
                    path_leading_trivia: Vec::new(),
                },
                span: crate::span::Span::new(21, 55),
                leading_trivia: Vec::new(),
            },
            rehost: Vec::new(),
            retype: Vec::new(),
            meta: crate::ast::Meta::new(crate::span::Span::new(0, 56)),
        };
        let modules = if materialized {
            vec![(
                root_dir.join("foo/lib.kio"),
                crate::pass::parser::parse("module foo/lib;\n").expect("parse module"),
            )]
        } else {
            Vec::new()
        };
        ParsedPackage {
            root_dir,
            modules,
            lazy_modules: BTreeMap::new(),
            package_file: None,
            dep_files: vec![(PathBuf::from("/pkg/foo.dep.kio"), dep_file)],
            sources: BTreeMap::new(),
        }
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_missing_materialized_dependency_reports_dep_error() {
        let errors = lsp_missing_materialized_dependencies(&parsed_package_with_dependency(false));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].file_path, PathBuf::from("/pkg/foo.dep.kio"));
        let (span, message) = errors[0].error.diag();
        assert_eq!(span, crate::span::Span::new(32, 52));
        assert_eq!(message, "dependency `foo` is declared but not materialized");
        assert_eq!(
            errors[0].error.diagnostic().help(),
            Some("run `kio dep fetch foo` to materialize the dependency")
        );
        assert_eq!(errors[0].error.exit_code(), ExitCode::Dep);
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_missing_materialized_dependency_accepts_materialized_module() {
        let errors = lsp_missing_materialized_dependencies(&parsed_package_with_dependency(true));
        assert!(
            errors.is_empty(),
            "materialized foo module should suppress diagnostic"
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    fn analyze_lsp_source(source: &str) -> LspAnalysis {
        analyze_lsp_fixture(source).1
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    fn analyze_lsp_fixture(source: &str) -> (tempfile::TempDir, LspAnalysis, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("pkg")).expect("create package dir");
        std::fs::write(dir.path().join("pkg.kio"), "module pkg;\n").expect("write root module");
        std::fs::write(
            dir.path().join("pkg.pkg.kio"),
            "package pkg;\n\nbridge {\n  pkg;\n  pkg/**;\n}\n",
        )
        .expect("write package file");
        if source.contains("import sequence(do)") {
            std::fs::write(
                dir.path().join("sequence.kio"),
                include_str!("../../../test-data/poc/elab/workdir/sequence.kio"),
            )
            .expect("write sequence provider");
        }
        let source_path = dir.path().join("pkg/main.kio");
        std::fs::write(&source_path, source).expect("write source");
        let analysis = analyze_workspace_at_with_overlay_lsp(
            dir.path(),
            &crate::package_collection::SourceOverlay::empty(),
        )
        .expect("analysis succeeds");
        let source_path = std::fs::canonicalize(source_path).expect("canonical source path");
        (dir, analysis, source_path)
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    const COPIED_RECEIVER_SOURCE: &str = concat!(
        "module pkg/main; import sequence(do);\n",
        "newtype Left : . { constructor left; projector unleft; };\n",
        "newtype Right : . { constructor right; projector unright; };\n",
        "newtype Box[A] : A { constructor box; projector unbox; };\n",
        "fn pure[A](value: A) -> Box(A) { Box.box(value) }\n",
        "fn choose_second[A](first: A, second: Right) -> Box(Right) {\n",
        "  do! (.[A][B](value: Box(A), next: A -> Box(B)) -> Box(B) {\n",
        "    next(Box.unbox(value))\n",
        "  }) {\n",
        "    let first_value <- pure(first);\n",
        "    let second_value <- pure(second);\n",
        "    pure(second_value)\n",
        "  }\n",
        "}\n",
    );

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn copied_receiver_hover_and_inlay_use_the_written_type_binder() {
        use crate::lsp::positions::LineIndex;
        use lsp_types::{InlayHintLabel, Position, Range};

        let (_dir, analysis, path) = analyze_lsp_fixture(COPIED_RECEIVER_SOURCE);
        let uri = crate::lsp::diagnostics::path_to_uri(&path, _dir.path()).expect("file URI");
        let start = COPIED_RECEIVER_SOURCE.find("Box.unbox(value)").unwrap() as u32;
        let lines = LineIndex::new(COPIED_RECEIVER_SOURCE);
        let value = lines.to_position(start + 10);
        let hover = crate::lsp::hover::handle_hover(
            &uri,
            &Position::new(value.line, value.character),
            &analysis,
            None,
        )
        .expect("written value has a type");
        let lsp_types::HoverContents::Markup(contents) = hover.contents else {
            panic!("hover is markup");
        };
        assert_eq!(contents.value, "```kio\nBox(A)\n```");
        let hints = crate::lsp::inlay_hints::handle_inlay_hints(
            &uri,
            &Range::new(Position::new(0, 0), Position::new(u32::MAX, 0)),
            &analysis,
            None,
        )
        .expect("typed hints");
        let end = lines.to_position(start + 9);
        let projector_hints = hints
            .iter()
            .filter(|hint| hint.position == Position::new(end.line, end.character))
            .collect::<Vec<_>>();
        assert_eq!(projector_hints.len(), 1);
        assert!(
            matches!(&projector_hints[0].label, InlayHintLabel::String(label) if label == "[A]")
        );
        let value_span = crate::span::Span::new(start + 10, start + 15);
        let checked_type = analysis
            .position_index
            .type_at("pkg/main", value_span)
            .expect("written value has a checked type");
        let crate::ast::Type::Path { segments, args, .. } = checked_type else {
            panic!("written value must retain its Box type: {checked_type:?}");
        };
        assert_eq!(
            segments.last().map(|segment| segment.name.as_str()),
            Some("Box")
        );
        let [crate::ast::Type::Path { segments, args, .. }] = args.as_slice() else {
            panic!("written Box must retain one type binder: {checked_type:?}");
        };
        assert!(args.is_empty());
        let [binder] = segments.as_slice() else {
            panic!("written Box binder must remain local: {checked_type:?}");
        };
        let binders = analysis
            .position_index
            .type_binders_at("pkg/main", value_span)
            .expect("written value has a checked binder scope");
        assert!(
            binder.name != "A" && binders.contains(&binder.name),
            "source presentation must leave the written value's checked binder intact: {checked_type:?}"
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn copied_receiver_has_no_inlay_at_a_generated_application() {
        use crate::lsp::positions::LineIndex;
        use lsp_types::{Position, Range};

        let (_dir, analysis, path) = analyze_lsp_fixture(COPIED_RECEIVER_SOURCE);
        let uri = crate::lsp::diagnostics::path_to_uri(&path, _dir.path()).expect("file URI");
        let hints = crate::lsp::inlay_hints::handle_inlay_hints(
            &uri,
            &Range::new(Position::new(0, 0), Position::new(u32::MAX, 0)),
            &analysis,
            None,
        )
        .expect("typed hints");
        let end = LineIndex::new(COPIED_RECEIVER_SOURCE)
            .to_position(COPIED_RECEIVER_SOURCE.find("}) {").unwrap() as u32 + 1);
        assert!(
            !hints
                .iter()
                .any(|hint| hint.position == Position::new(end.line, end.character)),
            "the written receiver is a lambda, not a call: {hints:?}"
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn written_prefix_ufcs_and_immediate_lambda_hints_retain_all_slots() {
        use crate::lsp::positions::LineIndex;
        use lsp_types::{InlayHintLabel, Position, Range};
        let source = concat!(
            "module pkg/main;\n",
            "fn pair[X][Y](x: X, y: Y) -> X { x }\n",
            "fn id[X](x: X) -> X { x }\n",
            "fn choose[X](left: X, right: X) -> X { left }\n",
            "op _ + _ { impl choose; };\n",
            "newtype Pack <X>: X { constructor pack; projector unpack; };\n",
            "fn controls[A](value: A) -> A {\n",
            "  let first = pair(value, value);\n",
            "  let second = value.>id;\n",
            "  let third = () + ();\n",
            "  (.[X](x: X) -> X { x })(value)\n",
            "}\n",
            "fn existential() -> . {\n",
            "  let .(<Hidden> payload) = Pack.unpack(Pack.pack(()));\n",
            "  ()\n",
            "}\n",
        );
        let (dir, analysis, path) = analyze_lsp_fixture(source);
        let uri = crate::lsp::diagnostics::path_to_uri(&path, dir.path()).expect("file URI");
        let hints = crate::lsp::inlay_hints::handle_inlay_hints(
            &uri,
            &Range::new(Position::new(0, 0), Position::new(u32::MAX, 0)),
            &analysis,
            None,
        )
        .expect("typed hints");
        let lines = LineIndex::new(source);
        for (marker, offset, expected) in [
            ("pair(value", 4, "[A, A]"),
            ("value.>id", 9, "[A]"),
            ("{ x })(value)", 5, "[A]"),
            ("() + ()", 7, "[.]"),
            ("Pack.pack(())", 9, "[.]"),
        ] {
            let end = lines.to_position((source.find(marker).unwrap() + offset) as u32);
            let labels = hints
                .iter()
                .filter(|hint| hint.position == Position::new(end.line, end.character))
                .map(|hint| match &hint.label {
                    InlayHintLabel::String(value) => value.as_str(),
                    _ => panic!("string hint"),
                })
                .collect::<Vec<_>>();
            assert_eq!(labels, [expected], "written {marker}: {hints:?}");
        }
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn nested_do_preserves_written_receiver_factory_hints() {
        use crate::lsp::positions::LineIndex;
        use lsp_types::{InlayHintLabel, Position, Range};
        let source = concat!(
            "module pkg/main; import sequence(do);\n",
            "newtype Box[A]: A { constructor box; projector unbox; };\n",
            "fn pure[A](value: A) -> Box(A) { Box.box(value) }\n",
            "fn factory[X](value: X) -> [A][B](Box(A) & (A -> Box(B))) -> Box(B) {\n",
            "  .[A][B](boxed: Box(A), next: A -> Box(B)) -> Box(B) { next(Box.unbox(boxed)) }\n",
            "}\n",
            "fn run() -> Box(.) {\n",
            "  do! factory(()) {\n",
            "    let first <- pure(());\n",
            "    do! factory(()) {\n",
            "      pure(());\n",
            "      let second <- pure(first);\n",
            "      pure(second)\n",
            "    }\n",
            "  }\n",
            "}\n",
        );
        let (dir, analysis, path) = analyze_lsp_fixture(source);
        let uri = crate::lsp::diagnostics::path_to_uri(&path, dir.path()).expect("file URI");
        let hints = crate::lsp::inlay_hints::handle_inlay_hints(
            &uri,
            &Range::new(Position::new(0, 0), Position::new(u32::MAX, 0)),
            &analysis,
            None,
        )
        .expect("typed hints");
        let lines = LineIndex::new(source);
        for (start, receiver) in source.match_indices("factory(())") {
            let written = lines.to_position((start + "factory".len()) as u32);
            let generated = lines.to_position((start + receiver.len()) as u32);
            let written_hints = hints
                .iter()
                .filter(|hint| hint.position == Position::new(written.line, written.character))
                .collect::<Vec<_>>();
            assert_eq!(written_hints.len(), 1, "{hints:?}");
            assert!(
                matches!(&written_hints[0].label, InlayHintLabel::String(label) if label == "[.]")
            );
            assert!(!hints.iter().any(|hint|
                hint.position == Position::new(generated.line, generated.character)
            ), "generated do receiver call: {hints:?}");
        }
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_analysis_indexes_unused_callable_declaration_target_segments() {
        use crate::pass::typecheck_core::write_type;
        use crate::pass::typecheck_full::ResolvedBinder;

        lsp_callable_target_work_counters::reset();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("pkg")).expect("create package dir");
        std::fs::write(
            dir.path().join("pkg.pkg.kio"),
            "package pkg;\n\nbridge {\n  pkg;\n  pkg/**;\n}\n",
        )
        .expect("write package file");
        std::fs::write(dir.path().join("pkg.kio"), "module pkg;\n").expect("write root module");
        std::fs::write(
            dir.path().join("pkg/provider.kio"),
            concat!(
                "module pkg/provider;\n",
                "\n",
                "import __comptime__;\n",
                "\n",
                "pub fn combine(left: ., right: .) -> . { left }\n",
                "pub fn empty() -> . { () }\n",
                "pub fn push(left: ., right: .) -> . { left }\n",
                "pub fn finish(value: .) -> . { value }\n",
                "pub pure fn elab_impl(ct: __Comptime__) -> __Checked_term__ {\n",
                "  __term_unit__(ct)\n",
                "}\n",
            ),
        )
        .expect("write provider module");
        let role_source = concat!(
            "module pkg/roles;\n",
            "\n",
            "import __intrinsics__;\n",
            "\n",
            "op _ ^ _ { impl __fst__; };\n",
            "host type Early_bool role(bool);\n",
            "op _ %% _ { impl __if_then_else__; };\n",
            "host type Later_bool role(bool);\n",
            "op _ ** _ { impl __pair__; };\n",
        );
        std::fs::write(dir.path().join("pkg/roles.kio"), role_source)
            .expect("write role-order module");
        let source = concat!(
            "module pkg/main;\n",
            "\n",
            "import pkg/provider as helper;\n",
            "import __intrinsics__;\n",
            "import __comptime__;\n",
            "\n",
            "op _ + _ { impl helper.combine; };\n",
            "op _ - _ { impl helper.combine; };\n",
            "op _ ** _ { impl __pair__; };\n",
            "varop [* *] {\n",
            "  foldr helper.push helper.empty;\n",
            "  finalize helper.finish;\n",
            "};\n",
            "varop [+ +] {\n",
            "  foldr __term_call__ __term_unit__;\n",
            "  finalize __term_type__;\n",
            "};\n",
            "elab unit : . -> . { impl helper.elab_impl; };\n",
            "\n",
            "pub fn run() -> . { let _combined = () + (); [* (), () *] }\n",
        );
        let source_path = dir.path().join("pkg/main.kio");
        std::fs::write(&source_path, source).expect("write consumer module");

        let analysis = analyze_workspace_at_with_overlay_lsp(
            dir.path(),
            &crate::package_collection::SourceOverlay::empty(),
        )
        .expect("analysis succeeds without using any declaration");
        let module_path = analysis
            .file_to_module
            .get(&std::fs::canonicalize(source_path).expect("canonical consumer path"))
            .expect("consumer module path");

        let mut declaration_leaf_spans = HashMap::new();
        for target in ["combine", "empty", "push", "finish"] {
            let written = format!("helper.{target}");
            let mut leaf_spans = Vec::new();
            for (path_start, _) in source.match_indices(&written) {
                let qualifier_span =
                    crate::span::Span::new(path_start as u32, (path_start + "helper".len()) as u32);
                let leaf_start = path_start + "helper.".len();
                let leaf_span =
                    crate::span::Span::new(leaf_start as u32, (leaf_start + target.len()) as u32);
                leaf_spans.push(leaf_span);

                assert!(
                    matches!(
                        analysis.position_index.binder_at(module_path, qualifier_span),
                        Some(ResolvedBinder::QualifiedImport { alias }) if alias == "helper"
                    ),
                    "`{written}` must index its qualifier at the qualifier's exact source span"
                );
                assert!(
                    matches!(
                        analysis.position_index.binder_at(module_path, leaf_span),
                        Some(ResolvedBinder::Fn { module_path, name })
                            if module_path == "pkg/provider" && name == target
                    ),
                    "`{written}` must index its leaf as the resolved provider function"
                );
                assert!(
                    analysis
                        .position_index
                        .type_at(module_path, leaf_span)
                        .is_some(),
                    "`{written}` must expose its callable type at the leaf's exact source span"
                );
            }
            assert!(!leaf_spans.is_empty(), "callable declaration target");
            declaration_leaf_spans.insert(target, leaf_spans);
        }

        let elaborator_path = "helper.elab_impl";
        let elaborator_start = source
            .find(elaborator_path)
            .expect("elaborator implementation path");
        let elaborator_leaf_start = elaborator_start + "helper.".len();
        let elaborator_leaf_span = crate::span::Span::new(
            elaborator_leaf_start as u32,
            (elaborator_leaf_start + "elab_impl".len()) as u32,
        );
        assert!(
            matches!(
                analysis.position_index.binder_at(module_path, elaborator_leaf_span),
                Some(ResolvedBinder::Fn { module_path, name })
                    if module_path == "pkg/provider" && name == "elab_impl"
            ),
            "the elaborator leaf must share the provider function's semantic identity"
        );
        let elaborator_path_span = crate::span::Span::new(
            elaborator_start as u32,
            (elaborator_start + elaborator_path.len()) as u32,
        );
        assert!(
            analysis
                .position_index
                .type_at(module_path, elaborator_path_span)
                .is_some(),
            "the ordinary elaborator declaration typecheck retains its whole-path type"
        );

        let intrinsic_start = source.find("__pair__").expect("intrinsic target");
        let intrinsic_span = crate::span::Span::new(
            intrinsic_start as u32,
            (intrinsic_start + "__pair__".len()) as u32,
        );
        assert!(
            matches!(
                analysis.position_index.binder_at(module_path, intrinsic_span),
                Some(ResolvedBinder::Intrinsic { name }) if name == "__pair__"
            ),
            "an intrinsic declaration target must retain ordinary intrinsic identity"
        );
        assert!(
            analysis
                .position_index
                .type_at(module_path, intrinsic_span)
                .is_some(),
            "an intrinsic declaration target must expose its exact-leaf hover type"
        );
        assert_eq!(
            lsp_callable_target_work_counters::snapshot(),
            5,
            "repeated declaration targets must synthesize one scheme per semantic identity"
        );

        for target in ["__term_unit__", "__term_call__", "__term_type__"] {
            let start = source.find(target).expect("compile-time helper target");
            let span = crate::span::Span::new(start as u32, (start + target.len()) as u32);
            assert!(
                analysis
                    .position_index
                    .binder_at(module_path, span)
                    .is_none(),
                "`{target}` must match ordinary compile-time helper paths, which publish no binder identity"
            );
            assert!(
                analysis.position_index.type_at(module_path, span).is_some(),
                "`{target}` must expose its exact-leaf hover type"
            );
        }

        let conditional_start = role_source
            .find("__if_then_else__")
            .expect("role-sensitive intrinsic target");
        let conditional_span = crate::span::Span::new(
            conditional_start as u32,
            (conditional_start + "__if_then_else__".len()) as u32,
        );
        assert!(matches!(
            analysis
                .position_index
                .binder_at("pkg/roles", conditional_span),
            Some(ResolvedBinder::Intrinsic { name }) if name == "__if_then_else__"
        ));
        let conditional_ty = analysis
            .position_index
            .type_at("pkg/roles", conditional_span)
            .expect("the role-sensitive intrinsic has a source-order hover type");
        let mut rendered_conditional_ty = String::new();
        write_type(conditional_ty, &mut rendered_conditional_ty);
        assert!(
            rendered_conditional_ty.contains("Early_bool")
                && !rendered_conditional_ty.contains("Later_bool"),
            "the intrinsic target must use the exact role view at its declaration, got `{rendered_conditional_ty}`"
        );

        let pair_start = role_source
            .find("__pair__")
            .expect("role-independent intrinsic target");
        let pair_span =
            crate::span::Span::new(pair_start as u32, (pair_start + "__pair__".len()) as u32);
        assert!(matches!(
            analysis.position_index.binder_at("pkg/roles", pair_span),
            Some(ResolvedBinder::Intrinsic { name }) if name == "__pair__"
        ));
        assert!(
            analysis
                .position_index
                .type_at("pkg/roles", pair_span)
                .is_some(),
            "a role-independent intrinsic remains hoverable after role ambiguity"
        );

        for ((entry_module, span), binder) in analysis.position_index.binders_iter() {
            if entry_module != module_path {
                continue;
            }
            let ResolvedBinder::Fn {
                module_path: owner,
                name,
            } = binder
            else {
                continue;
            };
            if owner != "pkg/provider" {
                continue;
            }
            let Some(expected_spans) = declaration_leaf_spans.get(name.as_str()) else {
                continue;
            };
            assert!(
                expected_spans.contains(span),
                "synthetic fixed/variadic operator calls must not publish helper references"
            );
        }
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_op_and_fold_identity_alias_heads_keep_alias_and_terminal_member_identities() {
        use crate::lsp::positions::LineIndex;
        use lsp_types::{GotoDefinitionResponse, Position, Range};

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("pkg")).expect("create package dir");
        std::fs::write(
            dir.path().join("pkg.pkg.kio"),
            "package pkg;\n\nbridge { pkg; pkg/**; }\n",
        )
        .expect("write package file");
        std::fs::write(dir.path().join("pkg.kio"), "module pkg;\n").expect("write root module");
        let provider = concat!(
            "module pkg/provider;\n",
            "rec {\n",
            "  pub newtype Tag : . | Peer { pub constructor make; pub projector open; };\n",
            "  pub newtype Peer : . | Tag { pub constructor make_peer; pub projector open_peer; };\n",
            "}\n",
        );
        let provider_path = dir.path().join("pkg/provider.kio");
        std::fs::write(&provider_path, provider).expect("write provider");
        let source = concat!(
            "module pkg/main;\n",
            "import pkg/provider as imported;\n",
            "type Tag = imported.Tag;\n",
            "op _ + _ { impl Tag.make; };\n",
            "varop [* *] { foldr Tag.open Tag.make; };\n",
        );
        let source_path = dir.path().join("pkg/main.kio");
        std::fs::write(&source_path, source).expect("write source");

        let analysis = analyze_workspace_at_with_overlay_lsp(
            dir.path(),
            &crate::package_collection::SourceOverlay::empty(),
        )
        .expect("identity-alias callable declarations analyze");
        let source_path = std::fs::canonicalize(source_path).expect("canonical source");
        let provider_path = std::fs::canonicalize(provider_path).expect("canonical provider");
        let source_uri =
            crate::lsp::diagnostics::path_to_uri(&source_path, dir.path()).expect("source URI");
        let provider_uri =
            crate::lsp::diagnostics::path_to_uri(&provider_path, dir.path()).expect("provider URI");
        let source_lines = LineIndex::new(source);
        let provider_lines = LineIndex::new(provider);
        let to_range = |lines: &LineIndex, start: usize, name: &str| {
            let range = lines.to_range(crate::span::Span::new(
                start as u32,
                (start + name.len()) as u32,
            ));
            Range {
                start: Position {
                    line: range.start.line,
                    character: range.start.character,
                },
                end: Position {
                    line: range.end.line,
                    character: range.end.character,
                },
            }
        };
        let position = |lines: &LineIndex, start: usize| {
            let point = lines.to_position(start as u32);
            Position {
                line: point.line,
                character: point.character,
            }
        };

        let alias_decl = source.find("type Tag").expect("alias declaration") + "type ".len();
        let terminal_use =
            source.find("imported.Tag").expect("terminal type use") + "imported.".len();
        let op_head = source.find("impl Tag.make").expect("op alias head") + "impl ".len();
        let base_head = source
            .find("foldr Tag.open Tag.make")
            .expect("fold base head")
            + "foldr Tag.open ".len();
        let step_head = source.find("foldr Tag.open").expect("fold step head") + "foldr ".len();
        let op_member = op_head + "Tag.".len();
        let base_member = base_head + "Tag.".len();
        let terminal_decl =
            provider.find("newtype Tag").expect("terminal declaration") + "newtype ".len();
        let make_decl = provider
            .find("constructor make")
            .expect("constructor declaration")
            + "constructor ".len();

        let alias_definition = crate::lsp::definition::handle_definition(
            &source_uri,
            &position(&source_lines, op_head),
            &analysis,
            None,
            dir.path(),
        )
        .expect("op alias head definition");
        assert_eq!(
            alias_definition,
            GotoDefinitionResponse::Scalar(lsp_types::Location {
                uri: source_uri.clone(),
                range: to_range(&source_lines, alias_decl, "Tag"),
            })
        );
        let member_definition = crate::lsp::definition::handle_definition(
            &source_uri,
            &position(&source_lines, op_member),
            &analysis,
            None,
            dir.path(),
        )
        .expect("op terminal member definition");
        assert_eq!(
            member_definition,
            GotoDefinitionResponse::Scalar(lsp_types::Location {
                uri: provider_uri.clone(),
                range: to_range(&provider_lines, make_decl, "make"),
            })
        );

        let alias_refs = crate::lsp::references::handle_references(
            &source_uri,
            &position(&source_lines, op_head),
            true,
            &analysis,
            None,
            dir.path(),
        )
        .expect("alias references");
        let alias_ranges = alias_refs
            .iter()
            .filter(|location| location.uri == source_uri)
            .map(|location| location.range)
            .collect::<Vec<_>>();
        for start in [alias_decl, op_head, base_head, step_head] {
            assert!(alias_ranges.contains(&to_range(&source_lines, start, "Tag")));
        }
        assert!(!alias_ranges.contains(&to_range(&source_lines, terminal_use, "Tag")));
        assert!(!alias_refs.iter().any(|location| {
            location.uri == provider_uri
                && location.range == to_range(&provider_lines, terminal_decl, "Tag")
        }));

        let member_refs = crate::lsp::references::handle_references(
            &source_uri,
            &position(&source_lines, op_member),
            true,
            &analysis,
            None,
            dir.path(),
        )
        .expect("terminal member references");
        assert!(member_refs.iter().any(|location| {
            location.uri == provider_uri
                && location.range == to_range(&provider_lines, make_decl, "make")
        }));
        for start in [op_member, base_member] {
            assert!(member_refs.iter().any(|location| {
                location.uri == source_uri
                    && location.range == to_range(&source_lines, start, "make")
            }));
        }

        let alias_edit = crate::lsp::rename::handle_rename(
            &source_uri,
            &position(&source_lines, op_head),
            "Renamed",
            &analysis,
            None,
            dir.path(),
        )
        .expect("alias rename succeeds")
        .expect("alias rename edits");
        assert_eq!(
            alias_edit
                .changes
                .as_ref()
                .expect("plain alias edits")
                .len(),
            1
        );
        let alias_edits = alias_edit
            .changes
            .as_ref()
            .expect("plain alias edits")
            .get(&source_uri)
            .expect("source alias edits");
        assert_eq!(alias_edits.len(), 4);
        assert!(alias_edits.iter().all(|edit| edit.new_text == "Renamed"));
        assert!(
            !alias_edits
                .iter()
                .any(|edit| edit.range == to_range(&source_lines, terminal_use, "Tag"))
        );

        let member_edit = crate::lsp::rename::handle_rename(
            &source_uri,
            &position(&source_lines, op_member),
            "construct",
            &analysis,
            None,
            dir.path(),
        )
        .expect("member rename succeeds")
        .expect("member rename edits");
        assert_eq!(
            member_edit
                .changes
                .as_ref()
                .expect("plain member edits")
                .get(&source_uri)
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(
            member_edit
                .changes
                .as_ref()
                .expect("plain member edits")
                .get(&provider_uri)
                .map(Vec::len),
            Some(1)
        );
        assert!(
            member_edit
                .changes
                .as_ref()
                .expect("plain member edits")
                .values()
                .flatten()
                .all(|edit| edit.new_text == "construct")
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_top_level_identity_covers_declarations_imports_and_callable_targets() {
        use crate::lsp::positions::LineIndex;
        use crate::pass::typecheck_full::ResolvedBinder;
        use lsp_types::{Position, Range};

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("pkg.pkg.kio"),
            "package pkg;\n\nbridge { provider; consumer; }\n",
        )
        .expect("write package file");
        let provider = concat!(
            "module provider;\n",
            "\n",
            "host fn loop[S][R](step: S -> S | R, state: S) -> R;\n",
            "host fn supplied(value: .) -> .;\n",
            "\n",
            "pub fn combine(left: ., right: .) -> . { left }\n",
            "\n",
            "rec(loop) {\n",
            "  pub fn first(value: .) -> . { rec second(value) };\n",
            "  fn second(value: .) -> . { rec first(value) }\n",
            "}\n",
            "\n",
            "pub newtype Wrap : . { pub constructor mk; pub projector get; };\n",
            "fn keep_wrap(value: Wrap) -> Wrap { value }\n",
            "labels { mk: ., get: . };\n",
            "fn label_forms(mk: ., row: Get) -> Mk {\n",
            "  let _seen = row.?{get};\n",
            "  {mk}\n",
            "}\n",
        );
        let provider_path = dir.path().join("provider.kio");
        std::fs::write(&provider_path, provider).expect("write provider");
        let consumer = concat!(
            "module consumer;\n",
            "\n",
            "import provider(combine, first, Wrap, supplied);\n",
            "op _ + _ { impl combine; };\n",
            "\n",
            "pub fn call() -> . { combine((), ()) }\n",
        );
        let consumer_path = dir.path().join("consumer.kio");
        std::fs::write(&consumer_path, consumer).expect("write consumer");
        for index in 0..32 {
            let module = format!("noise{index:02}");
            std::fs::write(
                dir.path().join(format!("{module}.kio")),
                format!("module {module};\n\nfn keep() -> . {{ () }}\n"),
            )
            .expect("write unrelated provider-index control");
        }

        let analysis = analyze_workspace_at_with_overlay_lsp(
            dir.path(),
            &package_collection::SourceOverlay::empty(),
        )
        .expect("analysis succeeds");
        let provider_path = std::fs::canonicalize(provider_path).expect("canonical provider");
        let consumer_path = std::fs::canonicalize(consumer_path).expect("canonical consumer");
        let provider_module = analysis
            .file_to_module
            .get(&provider_path)
            .expect("provider module");
        let consumer_module = analysis
            .file_to_module
            .get(&consumer_path)
            .expect("consumer module");
        assert_eq!(provider_module, "provider");
        assert_eq!(consumer_module, "consumer");

        let name_span = |source: &str, needle: &str, prefix_len: usize, name: &str| {
            let start = source.find(needle).expect("named source token") + prefix_len;
            crate::span::Span::new(start as u32, (start + name.len()) as u32)
        };
        for (span, expected) in [
            (
                name_span(provider, "host fn loop", "host fn ".len(), "loop"),
                ResolvedBinder::HostEnvFn {
                    module_path: "provider".to_owned(),
                    name: "loop".to_owned(),
                },
            ),
            (
                name_span(provider, "host fn supplied", "host fn ".len(), "supplied"),
                ResolvedBinder::HostEnvFn {
                    module_path: "provider".to_owned(),
                    name: "supplied".to_owned(),
                },
            ),
            (
                name_span(provider, "fn combine", "fn ".len(), "combine"),
                ResolvedBinder::Fn {
                    module_path: "provider".to_owned(),
                    name: "combine".to_owned(),
                },
            ),
            (
                name_span(provider, "fn first", "fn ".len(), "first"),
                ResolvedBinder::Fn {
                    module_path: "provider".to_owned(),
                    name: "first".to_owned(),
                },
            ),
            (
                name_span(provider, "fn second", "fn ".len(), "second"),
                ResolvedBinder::Fn {
                    module_path: "provider".to_owned(),
                    name: "second".to_owned(),
                },
            ),
            (
                name_span(provider, "newtype Wrap", "newtype ".len(), "Wrap"),
                ResolvedBinder::Newtype {
                    module_path: "provider".to_owned(),
                    name: "Wrap".to_owned(),
                },
            ),
            (
                name_span(provider, "constructor mk", "constructor ".len(), "mk"),
                ResolvedBinder::NewtypeMember {
                    module_path: "provider".to_owned(),
                    newtype: "Wrap".to_owned(),
                    member: "mk".to_owned(),
                },
            ),
            (
                name_span(provider, "projector get", "projector ".len(), "get"),
                ResolvedBinder::NewtypeMember {
                    module_path: "provider".to_owned(),
                    newtype: "Wrap".to_owned(),
                    member: "get".to_owned(),
                },
            ),
        ] {
            let actual = analysis
                .position_index
                .binder_at(provider_module, span)
                .unwrap_or_else(|| panic!("missing declaration binder at {span:?}"));
            assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
        }

        let parsed_provider = crate::pass::parser::parse(provider).expect("parse provider");
        let label_spans = crate::tokens::dump_module(provider, &parsed_provider)
            .expect("classify provider")
            .into_iter()
            .filter(|token| {
                matches!(
                    token.kind,
                    crate::tokens::TokenKind::EntityNameLabel
                        | crate::tokens::TokenKind::EntityNameLabelReference
                        | crate::tokens::TokenKind::EntityNameQualifiedLabelReference
                )
            })
            .map(|token| token.span)
            .collect::<Vec<_>>();
        for span in &label_spans {
            let written = &provider[span.start as usize..span.end as usize];
            let expected = crate::ast::mint_label_newtype_name(written);
            assert!(matches!(
                analysis.position_index.binder_at(provider_module, *span),
                Some(ResolvedBinder::Newtype { module_path, name })
                    if module_path == "provider" && name == &expected
            ));
        }

        let focused = analyze_module_at_with_overlay_lsp(
            dir.path(),
            &package_collection::SourceOverlay::empty(),
            &provider_path,
        )
        .expect("focused provider analysis succeeds");
        assert_eq!(
            focused.label_reuse_indexes.keys().collect::<Vec<_>>(),
            vec![&provider_path],
            "focused enrichment must classify only the requested source file"
        );
        assert!(
            focused
                .position_index
                .binders_iter()
                .all(|((module, _), _)| module == provider_module),
            "focused enrichment must not publish sibling-module positions"
        );
        for span in &label_spans {
            let written = &provider[span.start as usize..span.end as usize];
            let expected = crate::ast::mint_label_newtype_name(written);
            assert!(matches!(
                focused.position_index.binder_at(provider_module, *span),
                Some(ResolvedBinder::Newtype { module_path, name })
                    if module_path == "provider" && name == &expected
            ));
        }

        lsp_declaration_site_work_counters::reset();
        let focused_consumer = analyze_module_at_with_overlay_lsp(
            dir.path(),
            &package_collection::SourceOverlay::empty(),
            &consumer_path,
        )
        .expect("focused consumer analysis succeeds");
        assert!(
            focused_consumer
                .position_index
                .binders_iter()
                .all(|((module, _), _)| module == consumer_module),
            "focused analysis must retain remote targets without publishing provider occurrences"
        );
        let import_start = consumer.find("combine").expect("selective function import");
        let import_span =
            crate::span::Span::new(import_start as u32, (import_start + "combine".len()) as u32);
        let imported = focused_consumer
            .position_index
            .binder_at(consumer_module, import_span)
            .expect("focused selective import retains semantic identity");
        assert!(matches!(
            imported,
            ResolvedBinder::Fn { module_path, name }
                if module_path == provider_module && name == "combine"
        ));
        let expected_combine_span = name_span(provider, "fn combine", "fn ".len(), "combine");
        assert_eq!(
            focused_consumer.position_index.declaration_site(imported),
            Some((provider_module.as_str(), expected_combine_span)),
            "focused and full analysis must resolve the same exact provider declaration"
        );
        assert_eq!(
            lsp_declaration_site_work_counters::snapshot(),
            lsp_declaration_site_work_counters::Counts {
                selective_probes: 4,
                declaration_probes: 4,
                provider_index_entries: 34,
                provider_lookups: 4,
                provider_tokenizations: 1,
                generated_label_map_builds: 0,
            },
            "provider lookup indexes modules once, resolves each target once, and tokenizes one provider once"
        );
        let provider_uri =
            crate::lsp::diagnostics::path_to_uri(&provider_path, dir.path()).expect("provider URI");
        let provider_lines = LineIndex::new(provider);
        for start in [
            provider.find("{get}").expect("field-access label") + 1,
            provider.rfind("{mk}").expect("record-shorthand label") + 1,
        ] {
            let cursor = provider_lines.to_position(start as u32);
            let position = Position {
                line: cursor.line,
                character: cursor.character,
            };
            assert!(
                crate::lsp::rename::handle_prepare_rename(
                    &provider_uri,
                    &position,
                    &analysis,
                    None,
                )
                .is_none(),
                "label syntax must not prepare an ordinary rename"
            );
            assert!(
                crate::lsp::rename::handle_rename(
                    &provider_uri,
                    &position,
                    "renamed",
                    &analysis,
                    None,
                    dir.path(),
                )
                .is_err(),
                "label syntax must reject an ordinary rename"
            );
        }

        let local_label_start = provider.rfind("{mk}").expect("local label use") + 1;
        let local_label_cursor = provider_lines.to_position(local_label_start as u32);
        let local_label_position = Position {
            line: local_label_cursor.line,
            character: local_label_cursor.character,
        };
        assert!(
            crate::lsp::definition::handle_definition(
                &provider_uri,
                &local_label_position,
                &analysis,
                None,
                dir.path(),
            )
            .is_some(),
            "a local label expression use must retain navigation identity"
        );
        assert!(
            crate::lsp::references::handle_references(
                &provider_uri,
                &local_label_position,
                true,
                &analysis,
                None,
                dir.path(),
            )
            .is_some_and(|locations| locations.len() >= 2),
            "a local label expression use must retain its declaration/reference set"
        );

        let local_label_without_cursor = crate::lsp::references::handle_references(
            &provider_uri,
            &local_label_position,
            false,
            &analysis,
            None,
            dir.path(),
        )
        .expect("local label references without declaration");
        let label_declaration_start = provider.find("mk:").expect("local label declaration");
        let expected_declaration = provider_lines.to_range(crate::span::Span::new(
            label_declaration_start as u32,
            (label_declaration_start + "mk".len()) as u32,
        ));
        let expected_use = provider_lines.to_range(crate::span::Span::new(
            local_label_start as u32,
            (local_label_start + "mk".len()) as u32,
        ));
        let to_lsp_range = |range: crate::lsp::positions::LspRange| Range {
            start: Position {
                line: range.start.line,
                character: range.start.character,
            },
            end: Position {
                line: range.end.line,
                character: range.end.character,
            },
        };
        let expected_declaration = to_lsp_range(expected_declaration);
        let expected_use = to_lsp_range(expected_use);
        assert!(
            local_label_without_cursor
                .iter()
                .all(|location| location.range != expected_declaration),
            "includeDeclaration=false must omit the label declaration"
        );
        assert!(
            local_label_without_cursor
                .iter()
                .any(|location| location.range == expected_use),
            "includeDeclaration=false must preserve the queried label use"
        );
        let label_declaration_cursor = provider_lines.to_position(label_declaration_start as u32);
        let from_label_declaration = crate::lsp::references::handle_references(
            &provider_uri,
            &Position {
                line: label_declaration_cursor.line,
                character: label_declaration_cursor.character,
            },
            true,
            &analysis,
            None,
            dir.path(),
        )
        .expect("references queried from a label declaration");
        assert!(
            from_label_declaration
                .iter()
                .any(|location| location.range == expected_declaration),
            "a declaration query must include the label declaration"
        );
        assert!(
            from_label_declaration
                .iter()
                .any(|location| location.range == expected_use),
            "a declaration query must include ordinary expression uses"
        );

        let wrap_start =
            provider.find("newtype Wrap").expect("newtype declaration") + "newtype ".len();
        let wrap_cursor = provider_lines.to_position(wrap_start as u32);
        let wrap_position = Position {
            line: wrap_cursor.line,
            character: wrap_cursor.character,
        };
        assert!(
            crate::lsp::rename::handle_prepare_rename(
                &provider_uri,
                &wrap_position,
                &analysis,
                None,
            )
            .is_some(),
            "newtype declarations with indexed type positions must prepare rename"
        );
        let wrap_edit = crate::lsp::rename::handle_rename(
            &provider_uri,
            &wrap_position,
            "Renamed",
            &analysis,
            None,
            dir.path(),
        )
        .expect("newtype rename succeeds")
        .expect("newtype rename produces edits");
        let wrap_consumer_uri =
            crate::lsp::diagnostics::path_to_uri(&consumer_path, dir.path()).expect("consumer URI");
        assert_eq!(
            wrap_edit
                .changes
                .as_ref()
                .expect("plain newtype edits")
                .get(&provider_uri)
                .map(Vec::len),
            Some(3)
        );
        assert_eq!(
            wrap_edit
                .changes
                .as_ref()
                .expect("plain newtype edits")
                .get(&wrap_consumer_uri)
                .map(Vec::len),
            Some(1)
        );
        assert!(
            wrap_edit
                .changes
                .as_ref()
                .expect("plain newtype edits")
                .values()
                .flatten()
                .all(|edit| edit.new_text == "Renamed")
        );

        let import_start = consumer
            .find("import provider(combine")
            .expect("selective import")
            + "import provider(".len();
        let imported = [
            (
                "combine",
                import_start,
                ResolvedBinder::Fn {
                    module_path: "provider".to_owned(),
                    name: "combine".to_owned(),
                },
            ),
            (
                "first",
                consumer.find("first").expect("first import"),
                ResolvedBinder::Fn {
                    module_path: "provider".to_owned(),
                    name: "first".to_owned(),
                },
            ),
            (
                "Wrap",
                consumer.find("Wrap").expect("Wrap import"),
                ResolvedBinder::Newtype {
                    module_path: "provider".to_owned(),
                    name: "Wrap".to_owned(),
                },
            ),
            (
                "supplied",
                consumer.find("supplied").expect("supplied import"),
                ResolvedBinder::HostEnvFn {
                    module_path: "provider".to_owned(),
                    name: "supplied".to_owned(),
                },
            ),
        ];
        for (name, start, expected) in imported {
            let span = crate::span::Span::new(start as u32, (start + name.len()) as u32);
            let actual = analysis
                .position_index
                .binder_at(consumer_module, span)
                .unwrap_or_else(|| panic!("missing selective import binder for {name}"));
            assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
        }

        let target_start = consumer.find("impl combine").expect("operator target") + "impl ".len();
        let target_span =
            crate::span::Span::new(target_start as u32, (target_start + "combine".len()) as u32);
        let call_start = consumer.rfind("combine").expect("ordinary call");
        let call_span =
            crate::span::Span::new(call_start as u32, (call_start + "combine".len()) as u32);
        for span in [target_span, call_span] {
            assert!(matches!(
                analysis.position_index.binder_at(consumer_module, span),
                Some(ResolvedBinder::Fn { module_path, name })
                    if module_path == "provider" && name == "combine"
            ));
        }

        let uri =
            crate::lsp::diagnostics::path_to_uri(&consumer_path, dir.path()).expect("consumer URI");
        let line_index = LineIndex::new(consumer);
        let cursor = line_index.to_position(call_span.start + 1);
        let edit = crate::lsp::rename::handle_rename(
            &uri,
            &Position {
                line: cursor.line,
                character: cursor.character,
            },
            "joined",
            &analysis,
            None,
            dir.path(),
        )
        .expect("rename succeeds")
        .expect("rename produces edits");
        let to_range = |source: &str, span| {
            let range = LineIndex::new(source).to_range(span);
            Range {
                start: Position {
                    line: range.start.line,
                    character: range.start.character,
                },
                end: Position {
                    line: range.end.line,
                    character: range.end.character,
                },
            }
        };
        assert_eq!(
            edit.changes
                .as_ref()
                .expect("plain workspace changes")
                .get(&provider_uri),
            Some(&vec![lsp_types::TextEdit {
                range: to_range(
                    provider,
                    name_span(provider, "fn combine", "fn ".len(), "combine")
                ),
                new_text: "joined".to_owned(),
            }])
        );
        let mut consumer_edits = edit
            .changes
            .as_ref()
            .expect("plain workspace changes")
            .get(&uri)
            .expect("consumer edits")
            .to_vec();
        consumer_edits.sort_by_key(|edit| (edit.range.start.line, edit.range.start.character));
        let mut expected_consumer_edits = [
            crate::span::Span::new(import_start as u32, (import_start + "combine".len()) as u32),
            target_span,
            call_span,
        ]
        .into_iter()
        .map(|span| lsp_types::TextEdit {
            range: to_range(consumer, span),
            new_text: "joined".to_owned(),
        })
        .collect::<Vec<_>>();
        expected_consumer_edits
            .sort_by_key(|edit| (edit.range.start.line, edit.range.start.character));
        assert_eq!(consumer_edits, expected_consumer_edits);
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_references_exclude_semantic_top_level_declarations() {
        use crate::lsp::positions::LineIndex;
        use lsp_types::{Position, Range};

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("pkg.pkg.kio"),
            "package pkg;\n\nbridge { provider; consumer; }\n",
        )
        .expect("write package file");
        let provider = concat!(
            "module provider;\n",
            "\n",
            "pub fn combine(left: ., right: .) -> . { left }\n",
            "pub newtype Wrap : . { pub constructor mk; pub projector get; };\n",
            "labels { mark: . };\n",
            "fn mark_value(mark: .) -> Mark { {mark} }\n",
        );
        let provider_path = dir.path().join("provider.kio");
        std::fs::write(&provider_path, provider).expect("write provider");
        let consumer = concat!(
            "module consumer;\n",
            "\n",
            "import provider(combine, Wrap);\n",
            "pub fn call() -> . { combine((), ()) }\n",
            "pub fn call_again() -> . { combine((), ()) }\n",
            "pub fn make_wrap() -> Wrap { Wrap.mk(()) }\n",
            "pub fn make_wrap_again() -> Wrap { Wrap.mk(()) }\n",
        );
        let consumer_path = dir.path().join("consumer.kio");
        std::fs::write(&consumer_path, consumer).expect("write consumer");

        let analysis = analyze_workspace_at_with_overlay_lsp(
            dir.path(),
            &package_collection::SourceOverlay::empty(),
        )
        .expect("analysis succeeds");
        let provider_path = std::fs::canonicalize(provider_path).expect("canonical provider");
        let consumer_path = std::fs::canonicalize(consumer_path).expect("canonical consumer");
        let provider_uri =
            crate::lsp::diagnostics::path_to_uri(&provider_path, dir.path()).expect("provider URI");
        let consumer_uri =
            crate::lsp::diagnostics::path_to_uri(&consumer_path, dir.path()).expect("consumer URI");

        let references_at = |request_uri: &lsp_types::Uri,
                             source: &str,
                             start: usize,
                             include_declaration: bool| {
            let cursor = LineIndex::new(source).to_position(start as u32 + 1);
            crate::lsp::references::handle_references(
                request_uri,
                &Position {
                    line: cursor.line,
                    character: cursor.character,
                },
                include_declaration,
                &analysis,
                None,
                dir.path(),
            )
            .expect("semantic references")
        };
        let range = |source: &str, start: usize, name: &str| {
            let range = LineIndex::new(source).to_range(crate::span::Span::new(
                start as u32,
                (start + name.len()) as u32,
            ));
            Range {
                start: Position {
                    line: range.start.line,
                    character: range.start.character,
                },
                end: Position {
                    line: range.end.line,
                    character: range.end.character,
                },
            }
        };
        let assert_declaration_policy =
            |declaration_start: usize,
             use_start: usize,
             name: &str,
             expected_uses: usize,
             request_uri: &lsp_types::Uri,
             request_source: &str| {
                let declaration_range = range(provider, declaration_start, name);
                let use_range = range(request_source, use_start, name);

                let without = references_at(request_uri, request_source, use_start, false);
                assert_eq!(without.len(), expected_uses);
                assert!(without.iter().all(|location| {
                    location.uri != provider_uri || location.range != declaration_range
                }));
                assert!(without
                    .iter()
                    .any(|location| location.uri == *request_uri && location.range == use_range));

                let with = references_at(request_uri, request_source, use_start, true);
                assert_eq!(with.len(), expected_uses + 1);
                assert!(with.iter().any(|location| {
                    location.uri == provider_uri && location.range == declaration_range
                }));

                let from_declaration =
                    references_at(&provider_uri, provider, declaration_start, false);
                assert_eq!(from_declaration.len(), expected_uses);
                assert!(from_declaration.iter().all(|location| {
                    location.uri != provider_uri || location.range != declaration_range
                }));
            };

        let combine_declaration = provider.find("combine").expect("combine declaration");
        let combine_use = consumer.rfind("combine").expect("combine use");
        assert_declaration_policy(
            combine_declaration,
            combine_use,
            "combine",
            3,
            &consumer_uri,
            consumer,
        );

        let mk_declaration =
            provider.find("constructor mk").expect("mk declaration") + "constructor ".len();
        let mk_use = consumer.rfind("Wrap.mk").expect("mk use") + "Wrap.".len();
        assert_declaration_policy(mk_declaration, mk_use, "mk", 2, &consumer_uri, consumer);

        let mark_declaration = provider.find("mark:").expect("label declaration");
        let mark_use = provider.rfind("{mark}").expect("label use") + 1;
        assert_declaration_policy(
            mark_declaration,
            mark_use,
            "mark",
            2,
            &provider_uri,
            provider,
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_qualified_import_alias_references_use_the_exact_alias_token() {
        use crate::lsp::positions::LineIndex;
        use lsp_types::{Position, Range};

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("pkg.pkg.kio"),
            "package pkg;\n\nbridge { as; first; second; }\n",
        )
        .expect("write package file");
        std::fs::write(
            dir.path().join("as.kio"),
            "module as;\n\npub fn keep(value: .) -> . { value }\n",
        )
        .expect("write provider");
        let first = concat!(
            "module first;\n",
            "\n",
            "import as as as;\n",
            "fn one() -> . { as.keep(()) }\n",
            "fn two() -> . { as.keep(()) }\n",
        );
        let first_path = dir.path().join("first.kio");
        std::fs::write(&first_path, first).expect("write first consumer");
        let second = concat!(
            "module second;\n",
            "\n",
            "import as as model;\n",
            "fn unrelated() -> . { model.keep(()) }\n",
        );
        let second_path = dir.path().join("second.kio");
        std::fs::write(&second_path, second).expect("write second consumer");

        let analysis = analyze_workspace_at_with_overlay_lsp(
            dir.path(),
            &package_collection::SourceOverlay::empty(),
        )
        .expect("analysis succeeds");
        let first_path = std::fs::canonicalize(first_path).expect("canonical first consumer");
        let second_path = std::fs::canonicalize(second_path).expect("canonical second consumer");
        let first_uri =
            crate::lsp::diagnostics::path_to_uri(&first_path, dir.path()).expect("first URI");
        let second_uri =
            crate::lsp::diagnostics::path_to_uri(&second_path, dir.path()).expect("second URI");
        let line_index = LineIndex::new(first);
        let alias = "as";
        let alias_declaration =
            first.find("import as as as").expect("alias declaration") + "import as as ".len();
        let first_use = first.find("as.keep").expect("first alias use");
        let second_use = first.rfind("as.keep").expect("second alias use");
        let range_at = |start: usize| {
            let range = line_index.to_range(crate::span::Span::new(
                start as u32,
                (start + alias.len()) as u32,
            ));
            Range {
                start: Position {
                    line: range.start.line,
                    character: range.start.character,
                },
                end: Position {
                    line: range.end.line,
                    character: range.end.character,
                },
            }
        };
        let declaration_range = range_at(alias_declaration);
        let expected_uses = [range_at(first_use), range_at(second_use)];
        let references_at = |start: usize, include_declaration: bool| {
            let cursor = line_index.to_position(start as u32 + 1);
            crate::lsp::references::handle_references(
                &first_uri,
                &Position {
                    line: cursor.line,
                    character: cursor.character,
                },
                include_declaration,
                &analysis,
                None,
                dir.path(),
            )
            .expect("qualified import alias references")
        };

        for start in [alias_declaration, first_use] {
            let with_declaration = references_at(start, true);
            assert_eq!(with_declaration.len(), 3);
            assert!(
                with_declaration
                    .iter()
                    .all(|location| location.uri == first_uri)
            );
            assert_eq!(with_declaration[0].range, declaration_range);
            assert_eq!(with_declaration[1].range, expected_uses[0]);
            assert_eq!(with_declaration[2].range, expected_uses[1]);

            let without_declaration = references_at(start, false);
            assert_eq!(without_declaration.len(), 2);
            assert!(
                without_declaration
                    .iter()
                    .all(|location| location.uri == first_uri)
            );
            assert_eq!(without_declaration[0].range, expected_uses[0]);
            assert_eq!(without_declaration[1].range, expected_uses[1]);
        }

        let unrelated_alias = second.find("model.keep").expect("unrelated alias use");
        let unrelated_cursor = LineIndex::new(second).to_position(unrelated_alias as u32 + 1);
        let unrelated = crate::lsp::references::handle_references(
            &second_uri,
            &Position {
                line: unrelated_cursor.line,
                character: unrelated_cursor.character,
            },
            true,
            &analysis,
            None,
            dir.path(),
        )
        .expect("distinct alias has its own references");
        assert_eq!(unrelated.len(), 2);
        assert!(unrelated.iter().all(|location| location.uri == second_uri));

        let declaration_cursor = line_index.to_position(alias_declaration as u32 + 1);
        let declaration_position = Position {
            line: declaration_cursor.line,
            character: declaration_cursor.character,
        };
        assert!(
            crate::lsp::definition::handle_definition(
                &first_uri,
                &declaration_position,
                &analysis,
                None,
                dir.path(),
            )
            .is_none(),
            "qualified import aliases remain non-navigable by contract"
        );
        assert!(
            crate::lsp::rename::handle_prepare_rename(
                &first_uri,
                &declaration_position,
                &analysis,
                None,
            )
            .is_none(),
            "reference identity must not make qualified aliases renameable"
        );
        assert!(
            crate::lsp::rename::handle_rename(
                &first_uri,
                &declaration_position,
                "renamed",
                &analysis,
                None,
                dir.path(),
            )
            .is_err(),
            "qualified import aliases remain unsafe to rename"
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_generated_label_callable_members_have_exact_full_and_focused_definitions() {
        use crate::lsp::positions::LineIndex;
        use crate::pass::typecheck_full::ResolvedBinder;
        use lsp_types::{GotoDefinitionResponse, Position, Range};

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("pkg.pkg.kio"),
            "package pkg;\n\nbridge { provider; consumer; }\n",
        )
        .expect("write package file");
        let provider = "module provider;\n\npub labels { item: . };\n";
        let provider_path = dir.path().join("provider.kio");
        std::fs::write(&provider_path, provider).expect("write provider");
        let consumer = concat!(
            "module consumer;\n",
            "\n",
            "import provider as p;\n",
            "import provider(Item);\n",
            "op + _ { impl p.Item.mk; };\n",
            "op - _ { impl Item.get; };\n",
        );
        let consumer_path = dir.path().join("consumer.kio");
        std::fs::write(&consumer_path, consumer).expect("write consumer");

        let full = analyze_workspace_at_with_overlay_lsp(
            dir.path(),
            &package_collection::SourceOverlay::empty(),
        )
        .expect("full analysis succeeds");
        lsp_declaration_site_work_counters::reset();
        let focused = analyze_module_at_with_overlay_lsp(
            dir.path(),
            &package_collection::SourceOverlay::empty(),
            &consumer_path,
        )
        .expect("focused consumer analysis succeeds");
        let focused_work = lsp_declaration_site_work_counters::snapshot();
        assert_eq!(focused_work.provider_tokenizations, 1);
        assert_eq!(focused_work.generated_label_map_builds, 1);

        let provider_path = std::fs::canonicalize(provider_path).expect("canonical provider");
        let consumer_path = std::fs::canonicalize(consumer_path).expect("canonical consumer");
        let provider_module = full
            .file_to_module
            .get(&provider_path)
            .expect("provider module");
        let consumer_module = full
            .file_to_module
            .get(&consumer_path)
            .expect("consumer module");
        assert_eq!(provider_module, "provider");
        assert_eq!(consumer_module, "consumer");
        assert!(
            focused
                .position_index
                .binders_iter()
                .all(|((module, _), _)| module == consumer_module)
        );

        let span = |start: usize, name: &str| {
            crate::span::Span::new(start as u32, (start + name.len()) as u32)
        };
        let label_start = provider.find("item:").expect("label declaration");
        let label_span = span(label_start, "item");
        let qualified_start = consumer.find("p.Item.mk").expect("qualified target");
        let qualified_spans = [
            (
                span(qualified_start, "p"),
                ResolvedBinder::QualifiedImport {
                    alias: "p".to_owned(),
                },
            ),
            (
                span(qualified_start + "p.".len(), "Item"),
                ResolvedBinder::Newtype {
                    module_path: "provider".to_owned(),
                    name: "Item".to_owned(),
                },
            ),
            (
                span(qualified_start + "p.Item.".len(), "mk"),
                ResolvedBinder::NewtypeMember {
                    module_path: "provider".to_owned(),
                    newtype: "Item".to_owned(),
                    member: "mk".to_owned(),
                },
            ),
        ];
        let selective_import_start = consumer
            .find("import provider(Item)")
            .expect("selective import")
            + "import provider(".len();
        let selective_start = consumer.find("Item.get").expect("selective target");
        let selective_head = ResolvedBinder::Newtype {
            module_path: "provider".to_owned(),
            name: "Item".to_owned(),
        };
        let selective_member = ResolvedBinder::NewtypeMember {
            module_path: "provider".to_owned(),
            newtype: "Item".to_owned(),
            member: "get".to_owned(),
        };
        let selective_head_span = span(selective_start, "Item");
        let selective_member_span = span(selective_start + "Item.".len(), "get");
        let assert_binder =
            |analysis: &LspAnalysis, segment_span: crate::span::Span, expected: &ResolvedBinder| {
                let actual = analysis
                    .position_index
                    .binder_at(consumer_module, segment_span)
                    .unwrap_or_else(|| panic!("missing binder at {segment_span:?}"));
                assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
            };

        for analysis in [&full, &focused] {
            for (segment_span, expected) in &qualified_spans {
                assert_binder(analysis, *segment_span, expected);
            }
            assert_binder(
                analysis,
                span(selective_import_start, "Item"),
                &selective_head,
            );
            assert_binder(analysis, selective_head_span, &selective_head);
            assert_binder(analysis, selective_member_span, &selective_member);
            for member in [&qualified_spans[2].1, &selective_member] {
                assert_eq!(
                    analysis.position_index.declaration_site(member),
                    Some((provider_module.as_str(), label_span)),
                    "generated members point to their source-written label declaration"
                );
            }
        }
        assert!(matches!(
            full.position_index.binder_at(provider_module, label_span),
            Some(ResolvedBinder::Newtype { module_path, name })
                if module_path == "provider" && name == "Item"
        ));

        let consumer_uri =
            crate::lsp::diagnostics::path_to_uri(&consumer_path, dir.path()).expect("consumer URI");
        let provider_uri =
            crate::lsp::diagnostics::path_to_uri(&provider_path, dir.path()).expect("provider URI");
        let consumer_lines = LineIndex::new(consumer);
        let provider_lines = LineIndex::new(provider);
        let expected = provider_lines.to_range(label_span);
        let expected = Range {
            start: Position {
                line: expected.start.line,
                character: expected.start.character,
            },
            end: Position {
                line: expected.end.line,
                character: expected.end.character,
            },
        };
        for analysis in [&full, &focused] {
            for member_start in [
                qualified_start + "p.Item.".len(),
                selective_start + "Item.".len(),
            ] {
                let cursor = consumer_lines.to_position(member_start as u32);
                let response = crate::lsp::definition::handle_definition(
                    &consumer_uri,
                    &Position {
                        line: cursor.line,
                        character: cursor.character,
                    },
                    analysis,
                    None,
                    dir.path(),
                )
                .expect("generated callable member resolves to its label declaration");
                let GotoDefinitionResponse::Scalar(location) = response else {
                    panic!("expected a scalar definition response")
                };
                assert_eq!(location.uri, provider_uri);
                assert_eq!(location.range, expected);
            }
        }

        for (member_start, member_name) in [
            (qualified_start + "p.Item.".len(), "mk"),
            (selective_start + "Item.".len(), "get"),
        ] {
            let cursor = consumer_lines.to_position(member_start as u32);
            let position = Position {
                line: cursor.line,
                character: cursor.character,
            };
            let member_span = span(member_start, member_name);
            let member_range = consumer_lines.to_range(member_span);
            let member_range = Range {
                start: Position {
                    line: member_range.start.line,
                    character: member_range.start.character,
                },
                end: Position {
                    line: member_range.end.line,
                    character: member_range.end.character,
                },
            };

            let full_with = crate::lsp::references::handle_references(
                &consumer_uri,
                &position,
                true,
                &full,
                None,
                dir.path(),
            )
            .expect("full generated-member references");
            assert_eq!(full_with.len(), 2);
            assert_eq!(full_with[0].uri, consumer_uri);
            assert_eq!(full_with[0].range, member_range);
            assert_eq!(full_with[1].uri, provider_uri);
            assert_eq!(full_with[1].range, expected);

            let full_without = crate::lsp::references::handle_references(
                &consumer_uri,
                &position,
                false,
                &full,
                None,
                dir.path(),
            )
            .expect("full generated-member uses");
            assert_eq!(full_without.len(), 1);
            assert_eq!(full_without[0].uri, consumer_uri);
            assert_eq!(full_without[0].range, member_range);

            let focused_with = crate::lsp::references::handle_references(
                &consumer_uri,
                &position,
                true,
                &focused,
                None,
                dir.path(),
            )
            .expect("focused generated-member references");
            assert_eq!(focused_with.len(), 1);
            assert_eq!(focused_with[0].uri, consumer_uri);
            assert_eq!(focused_with[0].range, member_range);
        }
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_recursive_labels_keep_named_alias_and_generated_nominal_declarations() {
        use crate::lsp::positions::LineIndex;
        use lsp_types::{GotoDefinitionResponse, Position, Range};

        let source = concat!(
            "module pkg/main;\n",
            "pub rec labels { item: Item };\n",
            "pub rec labels Tree = { branch: Tree };\n",
            "op + _ { impl Item.mk; };\n",
            "op - _ { impl Branch.mk; };\n",
        );
        let (dir, analysis, source_path) = analyze_lsp_fixture(source);
        let uri =
            crate::lsp::diagnostics::path_to_uri(&source_path, dir.path()).expect("source URI");
        let lines = LineIndex::new(source);
        let position = |start: usize| {
            let point = lines.to_position(start as u32);
            Position {
                line: point.line,
                character: point.character,
            }
        };
        let range = |start: usize, name: &str| {
            let range = lines.to_range(crate::span::Span::new(
                start as u32,
                (start + name.len()) as u32,
            ));
            Range {
                start: Position {
                    line: range.start.line,
                    character: range.start.character,
                },
                end: Position {
                    line: range.end.line,
                    character: range.end.character,
                },
            }
        };

        for (label, generated, target) in [
            ("item", "Item", "impl Item.mk"),
            ("branch", "Branch", "impl Branch.mk"),
        ] {
            let declaration = source
                .find(&format!("{label}:"))
                .expect("label declaration");
            let head = source.find(target).expect("generated nominal target") + "impl ".len();
            let member = head + generated.len() + 1;
            let expected = GotoDefinitionResponse::Scalar(lsp_types::Location {
                uri: uri.clone(),
                range: range(declaration, label),
            });
            assert_eq!(
                crate::lsp::definition::handle_definition(
                    &uri,
                    &position(head),
                    &analysis,
                    None,
                    dir.path(),
                ),
                Some(expected.clone()),
                "generated nominal head must resolve to its recursive label entry"
            );
            assert_eq!(
                crate::lsp::definition::handle_definition(
                    &uri,
                    &position(member),
                    &analysis,
                    None,
                    dir.path(),
                ),
                Some(expected),
                "generated mk member must resolve to its recursive label entry"
            );
            let references = crate::lsp::references::handle_references(
                &uri,
                &position(head),
                true,
                &analysis,
                None,
                dir.path(),
            )
            .expect("generated nominal references");
            assert!(
                references
                    .iter()
                    .any(|location| location.range == range(declaration, label))
            );
            assert!(
                references
                    .iter()
                    .any(|location| location.range == range(head, generated))
            );
            assert!(
                crate::lsp::rename::handle_prepare_rename(&uri, &position(head), &analysis, None,)
                    .is_none(),
                "generated nominal rename must not separate it from `{label}`"
            );
        }

        let tree_declaration =
            source.find("labels Tree").expect("named labels alias") + "labels ".len();
        let tree_use = source.find("branch: Tree").expect("named alias use") + "branch: ".len();
        assert_eq!(
            crate::lsp::definition::handle_definition(
                &uri,
                &position(tree_use),
                &analysis,
                None,
                dir.path(),
            ),
            Some(GotoDefinitionResponse::Scalar(lsp_types::Location {
                uri: uri.clone(),
                range: range(tree_declaration, "Tree"),
            }))
        );
        let edit = crate::lsp::rename::handle_rename(
            &uri,
            &position(tree_use),
            "Forest",
            &analysis,
            None,
            dir.path(),
        )
        .expect("named labels alias rename succeeds")
        .expect("named labels alias edits");
        let edits = edit
            .changes
            .as_ref()
            .expect("plain rename edits")
            .get(&uri)
            .expect("same-module Tree edits");
        assert_eq!(edits.len(), 2);
        assert!(edits.iter().all(|edit| edit.new_text == "Forest"));
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_references_use_exact_local_and_type_parameter_declarations() {
        use crate::lsp::positions::LineIndex;
        use lsp_types::{Position, Range};

        let source = concat!(
            "module pkg/main;\n",
            "fn apply[A][*F](f: F(A), value: A) -> A { value }\n",
            "newtype Pack[A] <E> : A & E { constructor mk_pack; projector un_pack; };\n",
        );
        let (dir, analysis, source_path) = analyze_lsp_fixture(source);
        let uri =
            crate::lsp::diagnostics::path_to_uri(&source_path, dir.path()).expect("source URI");
        let line_index = LineIndex::new(source);
        let range = |start: usize, name: &str| {
            let range = line_index.to_range(crate::span::Span::new(
                start as u32,
                (start + name.len()) as u32,
            ));
            Range {
                start: Position {
                    line: range.start.line,
                    character: range.start.character,
                },
                end: Position {
                    line: range.end.line,
                    character: range.end.character,
                },
            }
        };
        let references_at = |start: usize, include_declaration: bool| {
            let cursor = line_index.to_position(start as u32);
            crate::lsp::references::handle_references(
                &uri,
                &Position {
                    line: cursor.line,
                    character: cursor.character,
                },
                include_declaration,
                &analysis,
                None,
                dir.path(),
            )
            .expect("binder references")
            .into_iter()
            .map(|location| location.range)
            .collect::<Vec<_>>()
        };

        let a_decl = source.find("[A]").expect("A declaration") + 1;
        let a_use = source.find("F(A)").expect("A use") + 2;
        let a_decl_range = range(a_decl, "A");
        assert!(references_at(a_decl, true).contains(&a_decl_range));
        let a_without_decl = references_at(a_decl, false);
        assert!(!a_without_decl.contains(&a_decl_range));
        assert!(a_without_decl.contains(&range(a_use, "A")));
        assert!(references_at(a_use, true).contains(&a_decl_range));
        let a_without_use_cursor = references_at(a_use, false);
        assert!(!a_without_use_cursor.contains(&a_decl_range));
        assert!(a_without_use_cursor.contains(&range(a_use, "A")));

        let f_decl = source.find("[*F]").expect("F declaration") + 2;
        let f_use = source.find("F(A)").expect("F use");
        let f_decl_range = range(f_decl, "F");
        assert!(references_at(f_decl, true).contains(&f_decl_range));
        assert!(!references_at(f_decl, false).contains(&f_decl_range));
        assert!(references_at(f_use, true).contains(&f_decl_range));
        let f_without_use_cursor = references_at(f_use, false);
        assert!(!f_without_use_cursor.contains(&f_decl_range));
        assert!(f_without_use_cursor.contains(&range(f_use, "F")));

        let e_decl = source.find("<E>").expect("E declaration") + 1;
        let e_use = source.find("A & E").expect("E use") + "A & ".len();
        let e_decl_range = range(e_decl, "E");
        assert!(references_at(e_decl, true).contains(&e_decl_range));
        assert!(!references_at(e_decl, false).contains(&e_decl_range));
        assert!(references_at(e_use, true).contains(&e_decl_range));
        let e_without_use_cursor = references_at(e_use, false);
        assert!(!e_without_use_cursor.contains(&e_decl_range));
        assert!(e_without_use_cursor.contains(&range(e_use, "E")));

        let value_decl = source.find("value: A").expect("value declaration");
        let value_use = source.rfind("value").expect("value use");
        let value_decl_range = range(value_decl, "value");
        assert!(references_at(value_decl, true).contains(&value_decl_range));
        let value_without_decl = references_at(value_decl, false);
        assert!(!value_without_decl.contains(&value_decl_range));
        assert!(value_without_decl.contains(&range(value_use, "value")));
        assert!(references_at(value_use, true).contains(&value_decl_range));
        let value_without_use_cursor = references_at(value_use, false);
        assert!(!value_without_use_cursor.contains(&value_decl_range));
        assert!(value_without_use_cursor.contains(&range(value_use, "value")));
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_rec_calls_keep_surface_member_identity_and_spans() {
        use crate::lsp::positions::LineIndex;
        use lsp_types::{HoverContents, PrepareRenameResponse, Range};

        let source = concat!(
            "module pkg/main;\n",
            "host fn loop[S][R](step: S -> S | R, state: S) -> R;\n",
            "rec(loop) {\n",
            "  fn first(value: .) -> . { rec second(value) };\n",
            "  fn second(value: .) -> . { rec first(value) }\n",
            "}\n",
        );
        let (dir, analysis, source_path) = analyze_lsp_fixture(source);
        let uri =
            crate::lsp::diagnostics::path_to_uri(&source_path, dir.path()).expect("source URI");
        let line_index = LineIndex::new(source);
        let call_start = source.find("rec second").expect("recursive call") + "rec ".len();
        let declaration_start = source.find("fn second").expect("member declaration") + "fn ".len();
        let call_span = crate::span::Span::new(call_start as u32, call_start as u32 + 6);
        let declaration_span =
            crate::span::Span::new(declaration_start as u32, declaration_start as u32 + 6);
        let cursor = line_index.to_position(call_span.start + 1);
        let position = lsp_types::Position {
            line: cursor.line,
            character: cursor.character,
        };
        let as_range = |span| {
            let range = line_index.to_range(span);
            Range {
                start: lsp_types::Position {
                    line: range.start.line,
                    character: range.start.character,
                },
                end: lsp_types::Position {
                    line: range.end.line,
                    character: range.end.character,
                },
            }
        };

        let hover = crate::lsp::hover::handle_hover(&uri, &position, &analysis, None)
            .expect("rec member hover");
        assert_eq!(hover.range, Some(as_range(call_span)));
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("hover uses Markdown")
        };
        assert!(markup.value.contains(". -> ."), "{}", markup.value);
        assert!(!markup.value.contains("Rec_state"), "{}", markup.value);

        let references = crate::lsp::references::handle_references(
            &uri,
            &position,
            true,
            &analysis,
            None,
            dir.path(),
        )
        .expect("rec member references");
        let mut ranges = references
            .into_iter()
            .map(|location| location.range)
            .collect::<Vec<_>>();
        ranges.sort_by_key(|range| (range.start.line, range.start.character));
        assert_eq!(
            ranges,
            vec![as_range(call_span), as_range(declaration_span)]
        );

        let prepared = crate::lsp::rename::handle_prepare_rename(&uri, &position, &analysis, None)
            .expect("rec member prepareRename");
        let PrepareRenameResponse::RangeWithPlaceholder { range, placeholder } = prepared else {
            panic!("prepareRename returns a placeholder")
        };
        assert_eq!(range, as_range(call_span));
        assert_eq!(placeholder, "second");

        let edit = crate::lsp::rename::handle_rename(
            &uri,
            &position,
            "renamed",
            &analysis,
            None,
            dir.path(),
        )
        .expect("rename succeeds")
        .expect("rename produces edits");
        let edits = edit
            .changes
            .expect("plain workspace changes")
            .into_values()
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(edits.len(), 2);
        assert!(edits.iter().all(|edit| edit.new_text == "renamed"));
        assert!(edits.iter().any(|edit| edit.range == as_range(call_span)));
        assert!(
            edits
                .iter()
                .any(|edit| edit.range == as_range(declaration_span))
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_rec_call_inside_operator_operand_keeps_surface_member_position() {
        use crate::pass::typecheck_full::ResolvedBinder;

        let source = concat!(
            "module pkg/main;\n",
            "host fn loop[S][R](step: S -> S | R, state: S) -> R;\n",
            "fn choose(left: ., right: .) -> . { left }\n",
            "op _ + _ { impl choose; };\n",
            "rec(loop) {\n",
            "  fn first(value: .) -> . { rec(cont) second(value) + value };\n",
            "  fn second(value: .) -> . { rec first(value) }\n",
            "}\n",
        );
        let (_, analysis, _) = analyze_lsp_fixture(source);
        let call_start = source
            .find("rec(cont) second")
            .expect("mode-qualified recursive call")
            + "rec(cont) ".len();
        let call_span = crate::span::Span::new(call_start as u32, call_start as u32 + 6);

        assert!(
            analysis
                .position_index
                .type_at("pkg/main", call_span)
                .is_some(),
            "recursive call in operator operand has no recorded type"
        );
        assert!(matches!(
            analysis.position_index.binder_at("pkg/main", call_span),
            Some(ResolvedBinder::Fn { module_path, name })
                if module_path == "pkg/main" && name == "second"
        ));
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_analysis_keeps_complete_overlay_paths_without_disk_files() {
        let root = PathBuf::from("/kio-wasm/demo");
        let mut files = BTreeMap::new();
        files.insert(
            root.join("demo.pkg.kio"),
            "package demo;\n\nbridge { main; }\n".to_owned(),
        );
        files.insert(
            root.join("main.kio"),
            "module main;\n\npub fn run() -> . { () }\n".to_owned(),
        );
        let overlay = package_collection::SourceOverlay::complete(root.clone(), files);

        let analysis =
            analyze_workspace_at_with_overlay_lsp(&root, &overlay).expect("analysis succeeds");

        assert_eq!(
            analysis.file_to_module.get(&root.join("main.kio")),
            Some(&"main".to_owned())
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_analysis_indexes_selective_label_import_identity() {
        use crate::lsp::positions::LineIndex;
        use crate::pass::typecheck_full::ResolvedBinder;
        use lsp_types::Position;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("pkg.pkg.kio"),
            "package pkg;\n\nbridge { origin; consumer; }\n",
        )
        .expect("write package file");
        let origin = "module origin;\n\npub labels { item: . };\n";
        let origin_file = dir.path().join("origin.kio");
        std::fs::write(&origin_file, origin).expect("write origin");
        let consumer = concat!(
            "module consumer;\n\n",
            "import origin(Item, {item});\n\n",
            "pub fn make() -> Item { {item=} }\n",
            "pub fn unwrap(value: Item) -> . { Item.get(value) }\n",
        );
        std::fs::write(dir.path().join("consumer.kio"), consumer).expect("write consumer");

        let analysis = analyze_workspace_at_with_overlay_lsp(
            dir.path(),
            &package_collection::SourceOverlay::empty(),
        )
        .expect("analysis succeeds");
        let import_start = consumer.find("{item}").expect("label import") + 1;
        let import_span = crate::span::Span::new(import_start as u32, import_start as u32 + 4);
        assert!(
            analysis
                .position_index
                .binders_iter()
                .any(|((module, span), binder)| {
                    module == "consumer"
                        && *span == import_span
                        && matches!(
                            binder,
                            ResolvedBinder::Newtype { module_path, name }
                                if module_path == "origin" && name == "Item"
                        )
                })
        );
        let use_start = consumer.rfind("{item=").expect("label use") + 1;
        let use_span = crate::span::Span::new(use_start as u32, use_start as u32 + 4);
        assert!(
            analysis
                .position_index
                .binders_iter()
                .any(|((module, span), binder)| {
                    module == "consumer"
                        && *span == use_span
                        && matches!(
                            binder,
                            ResolvedBinder::Newtype { module_path, name }
                                if module_path == "origin" && name == "Item"
                        )
                }),
            "surface label uses keep the selected provider identity"
        );
        let consumer_file = std::fs::canonicalize(dir.path().join("consumer.kio"))
            .expect("canonical consumer path");
        let origin_file = std::fs::canonicalize(origin_file).expect("canonical origin path");
        assert!(
            analysis
                .label_reuse_indexes
                .get(&consumer_file)
                .and_then(|index| index.label_at(use_span.start))
                .is_some(),
            "imported label uses are protected from ordinary rename"
        );

        assert!(
            analysis
                .generated_label_nominals
                .contains(&("origin".to_owned(), "Item".to_owned()))
        );
        let consumer_uri =
            crate::lsp::diagnostics::path_to_uri(&consumer_file, dir.path()).expect("consumer URI");
        let origin_uri =
            crate::lsp::diagnostics::path_to_uri(&origin_file, dir.path()).expect("origin URI");
        let line_index = LineIndex::new(consumer);
        for (description, offset) in [
            (
                "generated label nominal",
                consumer.find("Item,").expect("nominal import"),
            ),
            (
                "generated label member",
                consumer.find("Item.get").expect("generated member") + "Item.".len(),
            ),
        ] {
            let cursor = line_index.to_position(offset as u32);
            let position = Position {
                line: cursor.line,
                character: cursor.character,
            };
            assert!(
                crate::lsp::rename::handle_prepare_rename(
                    &consumer_uri,
                    &position,
                    &analysis,
                    None,
                )
                .is_none(),
                "{description} must not prepare an incomplete rename"
            );
            assert!(
                crate::lsp::rename::handle_rename(
                    &consumer_uri,
                    &position,
                    "Renamed",
                    &analysis,
                    None,
                    dir.path(),
                )
                .is_err(),
                "{description} must refuse a rename that cannot update the source label"
            );
        }

        let origin_lines = LineIndex::new(origin);
        let declaration_start = origin.find("item:").expect("provider label declaration");
        let declaration_cursor = origin_lines.to_position(declaration_start as u32);
        let declaration_position = Position {
            line: declaration_cursor.line,
            character: declaration_cursor.character,
        };
        let references = crate::lsp::references::handle_references(
            &origin_uri,
            &declaration_position,
            false,
            &analysis,
            None,
            dir.path(),
        )
        .expect("provider label references");
        let consumer_ranges = references
            .iter()
            .filter(|location| location.uri == consumer_uri)
            .map(|location| location.range)
            .collect::<Vec<_>>();
        let consumer_range = |span| {
            let range = line_index.to_range(span);
            lsp_types::Range {
                start: Position {
                    line: range.start.line,
                    character: range.start.character,
                },
                end: Position {
                    line: range.end.line,
                    character: range.end.character,
                },
            }
        };
        assert!(
            consumer_ranges.contains(&consumer_range(import_span)),
            "a provider declaration query must include the selective label import"
        );
        assert!(
            consumer_ranges.contains(&consumer_range(use_span)),
            "a provider declaration query must include the consumer label use"
        );
        assert!(
            references.iter().all(|location| location.uri != origin_uri),
            "includeDeclaration=false must omit the provider label declaration"
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_label_binder_repair_preserves_comment_separated_qualification() {
        use crate::pass::typecheck_full::ResolvedBinder;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("pkg.pkg.kio"),
            "package pkg;\n\nbridge { imported; qualified; consumer; }\n",
        )
        .expect("write package file");
        std::fs::write(
            dir.path().join("imported.kio"),
            "module imported;\n\npub labels { item: . };\n",
        )
        .expect("write imported provider");
        std::fs::write(
            dir.path().join("qualified.kio"),
            "module qualified;\n\npub labels { item: . };\n",
        )
        .expect("write qualified provider");
        let consumer = concat!(
            "module consumer;\n",
            "\n",
            "import imported(Item, {item});\n",
            "import qualified as item;\n",
            "\n",
            "pub fn make() -> item.Item { {item. // keep qualification through trivia\n",
            "item=} }\n",
        );
        std::fs::write(dir.path().join("consumer.kio"), consumer).expect("write consumer");

        let parsed_consumer = crate::pass::parser::parse(consumer).expect("parse consumer");
        let qualified_leaf = consumer.rfind("item=").expect("qualified label use") as u32;
        let classified = crate::tokens::dump_module(consumer, &parsed_consumer)
            .expect("classify consumer")
            .into_iter()
            .find(|token| token.span.start == qualified_leaf)
            .expect("qualified label token");
        assert!(
            matches!(
                classified.kind,
                crate::tokens::TokenKind::EntityNameQualifiedLabelReference
            ),
            "parsed label qualification was lost: {:?}",
            classified.kind
        );

        let analysis = analyze_workspace_at_with_overlay_lsp(
            dir.path(),
            &package_collection::SourceOverlay::empty(),
        )
        .expect("analysis succeeds");
        let use_start = consumer.rfind("item=").expect("qualified label use");
        let use_span = crate::span::Span::new(use_start as u32, use_start as u32 + 4);
        let binders_at_use: Vec<_> = analysis
            .position_index
            .binders_iter()
            .filter(|((module, span), _)| module == "consumer" && *span == use_span)
            .map(|(_, binder)| format!("{binder:?}"))
            .collect();
        assert!(
            analysis
                .position_index
                .binders_iter()
                .any(|((module, span), binder)| {
                    module == "consumer"
                        && *span == use_span
                        && matches!(
                            binder,
                            ResolvedBinder::Newtype { module_path, name }
                                if module_path == "qualified" && name == "Item"
                        )
                }),
            "the parsed qualified path must not be rebound to imported.Item: {binders_at_use:?}"
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_unused_warning_uses_decl_span_for_shadowed_lets() {
        let source = concat!(
            "module pkg/main;\n",
            "\n",
            "pub fn run() -> . {\n",
            "  let x = ();\n",
            "  let x = ();\n",
            "  x\n",
            "}\n",
        );
        let analysis = analyze_lsp_source(source);
        assert_eq!(analysis.warnings.len(), 1, "{:?}", analysis.warnings);
        let outer_x = source.find("let x").expect("outer let") + "let ".len();
        let warning = &analysis.warnings[0];
        assert_eq!(
            warning.span,
            crate::span::Span::new(outer_x as u32, outer_x as u32 + 1)
        );
        assert_eq!(warning.message, "unused binding `x`");
        assert_eq!(warning.fixes.len(), 2);
        assert_eq!(warning.fixes[0].edits[0].span, warning.span);
        assert_eq!(warning.fixes[0].edits[0].replacement, "_x");
        assert_eq!(
            warning.fixes[1].applicability,
            crate::error::Applicability::MaybeIncorrect
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_unused_parameter_warning_has_only_prefix_fix() {
        let source = concat!(
            "module pkg/main;\n",
            "\n",
            "pub fn run(x: .) -> . {\n",
            "  ()\n",
            "}\n",
        );
        let analysis = analyze_lsp_source(source);
        assert_eq!(analysis.warnings.len(), 1, "{:?}", analysis.warnings);
        let x = source.find("x: .").expect("parameter x");
        let warning = &analysis.warnings[0];
        assert_eq!(warning.span, crate::span::Span::new(x as u32, x as u32 + 1));
        assert_eq!(warning.message, "unused binding `x`");
        assert_eq!(warning.fixes.len(), 1);
        assert_eq!(warning.fixes[0].edits[0].replacement, "_x");
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_unused_parameter_warning_covers_rec_group_member() {
        let source = concat!(
            "module pkg/main;\n",
            "\n",
            "host fn loop[S][R](step: S -> S | R, state: S) -> R;\n",
            "\n",
            "rec(loop) pub fn run(x: .) -> . {\n",
            "  ()\n",
            "}\n",
        );
        let analysis = analyze_lsp_source(source);
        assert_eq!(analysis.warnings.len(), 1, "{:?}", analysis.warnings);
        let x = source.find("x: .").expect("parameter x");
        let warning = &analysis.warnings[0];
        assert_eq!(warning.span, crate::span::Span::new(x as u32, x as u32 + 1));
        assert_eq!(warning.message, "unused binding `x`");
        assert_eq!(warning.fixes.len(), 1);
        assert_eq!(warning.fixes[0].edits[0].replacement, "_x");
    }

    fn stage_error(package: &str, path: &str, span: crate::span::Span) -> PackageStageResult<()> {
        PackageStageResult {
            key: key(package),
            result: Err(vec![LocatedError {
                file_path: PathBuf::from(path),
                error: Error::type_(span, package),
            }]),
        }
    }

    #[test]
    fn package_stage_errors_keep_all_ordered_by_package_path_then_span() {
        let results = vec![
            stage_error("z", "/z/main.kio", crate::span::Span::new(1, 2)),
            stage_error("a", "/a/z.kio", crate::span::Span::new(1, 2)),
            stage_error("a", "/a/a.kio", crate::span::Span::new(5, 6)),
            stage_error("a", "/a/a.kio", crate::span::Span::new(3, 4)),
        ];

        let errors = package_stage_errors(&results);

        assert_eq!(errors.len(), 4);
        assert_eq!(errors[0].file_path, PathBuf::from("/a/a.kio"));
        assert_eq!(errors[0].error.diag().0, crate::span::Span::new(3, 4));
        assert_eq!(errors[1].file_path, PathBuf::from("/a/a.kio"));
        assert_eq!(errors[1].error.diag().0, crate::span::Span::new(5, 6));
    }

    #[test]
    fn analysis_failure_keeps_only_earliest_exit_category() {
        let errors = vec![
            LocatedError {
                file_path: PathBuf::from("/pkg/type.kio"),
                error: Error::type_(crate::span::Span::new(1, 2), "type"),
            },
            LocatedError {
                file_path: PathBuf::from("/pkg/use.kio"),
                error: Error::import(crate::span::Span::new(3, 4), "use"),
            },
            LocatedError {
                file_path: PathBuf::from("/pkg/type2.kio"),
                error: Error::type_(crate::span::Span::new(5, 6), "type2"),
            },
        ];

        let failure = AnalysisFailure::from_errors(errors, HashMap::new());

        assert_eq!(failure.errors.len(), 1);
        assert_eq!(failure.errors[0].file_path, PathBuf::from("/pkg/use.kio"));
    }

    #[cfg(feature = "surface")]
    #[test]
    fn analysis_collects_independent_fn_body_errors_in_one_module() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("pkg")).expect("create module dir");
        std::fs::write(
            dir.path().join("pkg.pkg.kio"),
            "package pkg;\n\nbridge { main; }\n",
        )
        .expect("write package file");
        std::fs::write(
            dir.path().join("main.kio"),
            "module main;\n\npub fn a() -> . { 1 }\n\npub fn b() -> . { 2 }\n",
        )
        .expect("write module");

        let failure = match analyze_workspace_at(dir.path(), false) {
            Ok(_) => panic!("analysis should fail"),
            Err(failure) => failure,
        };

        assert_eq!(failure.errors.len(), 2);
        assert!(
            failure
                .errors
                .iter()
                .all(|error| error.error.exit_code() == ExitCode::Type)
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_analysis_collects_independent_fn_body_errors_in_one_module() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("pkg")).expect("create module dir");
        std::fs::write(
            dir.path().join("pkg.pkg.kio"),
            "package pkg;\n\nbridge { main; }\n",
        )
        .expect("write package file");
        std::fs::write(
            dir.path().join("main.kio"),
            "module main;\n\npub fn a() -> . { 1 }\n\npub fn b() -> . { 2 }\n",
        )
        .expect("write module");

        let failure = match analyze_workspace_at_with_overlay_lsp(
            dir.path(),
            &package_collection::SourceOverlay::empty(),
        ) {
            Ok(_) => panic!("analysis should fail"),
            Err(failure) => failure,
        };

        assert_eq!(failure.errors.len(), 2);
        assert!(
            failure
                .errors
                .iter()
                .all(|error| error.error.exit_code() == ExitCode::Type)
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    fn write_lsp_user_elaborator_package(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir.join("pkg")).expect("create module dir");
        std::fs::write(
            dir.join("pkg.pkg.kio"),
            "package pkg;\n\n\
             bridge { main; bad; elaborators; }\n",
        )
        .expect("write package file");
        std::fs::write(
            dir.join("elaborators.kio"),
            "module elaborators;\n\n\
             import __intrinsics__;\n\n\
             import __comptime__;\n\n\
             pub type Optional_type = __Type__ | .;\n\n\
             pub pure fn id_impl(ct: __Comptime__, _source: __Type__, value: __Checked_term__, target: Optional_type) -> __Checked_term__ {\n\
               __either__(__Type__, ., __Checked_term__, target, .(_target: __Type__) -> __Checked_term__ {\n\
                 value\n\
               }, .() -> __Checked_term__ { __elab_error__(ct, \"elaborator needs a target\") })\n\
             }\n\n\
             pub pure fn fail_impl(ct: __Comptime__, _source: __Type__, _value: __Checked_term__, _target: Optional_type) -> __Checked_term__ {\n\
               __elab_error__(ct, \"bad elaborator ran\")\n\
             }\n\n\
             pub elab id_user : [Source] Source -> [Target] Target { impl id_impl; };\n\n\
             pub elab fail_user : [Source] Source -> [Target] Target { impl fail_impl; };\n",
        )
        .expect("write elaborator package");
        let main_path = dir.join("main.kio");
        std::fs::write(
            &main_path,
            "module main;\n\n\
             import elaborators(id_user);\n\n\
             pub fn run() -> . { id_user!(., ()) }\n",
        )
        .expect("write main");
        std::fs::write(
            dir.join("bad.kio"),
            "module bad;\n\n\
             import elaborators(fail_user);\n\n\
             pub fn bad() -> . { fail_user!(., ()) }\n",
        )
        .expect("write bad");
        main_path
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_focused_analysis_does_not_evaluate_sibling_user_elaborator() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main_path = write_lsp_user_elaborator_package(dir.path());
        let overlay = package_collection::SourceOverlay::empty();
        let cancel = crate::lsp::cancel::CancellationToken::new();

        let focused = analyze_module_at_with_overlay_lsp_cancellable(
            dir.path(),
            &overlay,
            &main_path,
            &cancel,
        )
        .expect("focused analysis should not be cancelled");
        assert!(
            focused.is_ok(),
            "focused analysis should not force the bad sibling elaborator: {focused:?}"
        );

        let full = analyze_workspace_at_with_overlay_lsp(dir.path(), &overlay)
            .expect_err("full analysis should evaluate the bad sibling elaborator");
        assert!(
            full.errors.iter().any(|error| {
                let (_, message) = error.error.diag();
                message.contains("bad elaborator ran")
            }),
            "full analysis should expose the sibling elaborator failure: {full:?}"
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp", feature = "parallel"))]
    #[test]
    fn focused_force_collection_overlaps_work_and_preserves_input_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .expect("build focused-force test pool");
        let targets: Vec<_> = ["a", "b", "c", "d"]
            .into_iter()
            .map(PackageModuleKey::new)
            .collect();
        let cancel = crate::lsp::cancel::CancellationToken::new();
        let active = AtomicUsize::new(0);
        let max_active = AtomicUsize::new(0);

        let results = pool
            .install(|| {
                collect_cancellable_focused_forces(&targets, &cancel, |target| {
                    let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(now_active, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(
                        if target.as_str() == "a" { 40 } else { 20 },
                    ));
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok::<_, &'static str>(target.as_str().to_owned())
                })
            })
            .expect("uncancelled collection should complete");

        assert!(
            max_active.load(Ordering::SeqCst) > 1,
            "independent focused module forces should overlap"
        );
        assert_eq!(
            results.into_iter().collect::<Result<Vec<_>, _>>(),
            Ok(vec![
                "a".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
                "d".to_owned()
            ]),
            "completion order must not change deterministic collection order"
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn focused_force_collection_cancels_before_queued_work() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let targets: Vec<_> = ["a", "b", "c"]
            .into_iter()
            .map(PackageModuleKey::new)
            .collect();
        let cancel = crate::lsp::cancel::CancellationToken::new();
        let calls = AtomicUsize::new(0);
        let run = || {
            collect_cancellable_focused_forces(&targets, &cancel, |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                cancel.cancel();
                Ok::<_, &'static str>(())
            })
        };
        #[cfg(feature = "parallel")]
        let result = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("build single-thread cancellation pool")
            .install(run);
        #[cfg(not(feature = "parallel"))]
        let result = run();

        assert!(result.is_none(), "cancelled work must not be published");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "cancellation should prevent queued module work from starting"
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn focused_force_collection_keeps_first_error_in_input_order() {
        let targets: Vec<_> = ["a", "b", "c"]
            .into_iter()
            .map(PackageModuleKey::new)
            .collect();
        let cancel = crate::lsp::cancel::CancellationToken::new();
        let results =
            collect_cancellable_focused_forces(&targets, &cancel, |target| match target.as_str() {
                "a" => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    Err("first")
                }
                "b" => Err("second"),
                _ => Ok(()),
            })
            .expect("uncancelled collection should complete");

        assert_eq!(
            results.into_iter().find_map(Result::err),
            Some("first"),
            "worker completion order must not change the serial first error"
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_focused_force_reports_the_same_first_module_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("pkg")).expect("create module dir");
        std::fs::write(
            dir.path().join("pkg.pkg.kio"),
            "package pkg;\n\nbridge { main; alpha; beta; }\n",
        )
        .expect("write package file");
        std::fs::write(
            dir.path().join("alpha.kio"),
            "module alpha;\n\npub fn alpha_value() -> . { missing_alpha }\n",
        )
        .expect("write alpha module");
        std::fs::write(
            dir.path().join("beta.kio"),
            "module beta;\n\npub fn beta_value() -> . { missing_beta }\n",
        )
        .expect("write beta module");
        let main_path = dir.path().join("main.kio");
        std::fs::write(
            &main_path,
            "module main;\n\n\
             import beta(beta_value);\n\n\
             import alpha(alpha_value);\n\n\
             pub fn run() -> . { alpha_value() }\n",
        )
        .expect("write main module");

        let failure = analyze_module_at_with_overlay_lsp_cancellable(
            dir.path(),
            &package_collection::SourceOverlay::empty(),
            &main_path,
            &crate::lsp::cancel::CancellationToken::new(),
        )
        .expect("focused analysis should not be cancelled")
        .expect_err("both imported module bodies should fail resolution");

        assert_eq!(failure.errors.len(), 1);
        let first = failure.primary_error();
        let canonical_root = dir.path().canonicalize().expect("canonical fixture root");
        assert_eq!(first.file_path, canonical_root.join("alpha.kio"));
        let (_, message) = first.error.diag();
        assert!(
            message.contains("missing_alpha"),
            "lexically first module error should remain primary: {message}"
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_user_elaborator_memo_survives_repeated_same_snapshot_analysis() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main_path = write_lsp_user_elaborator_package(dir.path());
        let overlay = package_collection::SourceOverlay::empty();
        let cancel = crate::lsp::cancel::CancellationToken::new();
        let memos = LspUserElaboratorMemos::default();

        for _ in 0..2 {
            let result = analyze_module_at_with_overlay_lsp_cancellable_with_memos(
                dir.path(),
                &overlay,
                &main_path,
                &cancel,
                &memos,
            )
            .expect("focused analysis should not be cancelled");
            assert!(
                result.is_ok(),
                "focused analysis should succeed: {result:?}"
            );
        }

        let snapshot = memos
            .revisions
            .only_memo()
            .expect("expected one package memo entry")
            .snapshot();
        assert_eq!(
            snapshot.user_elaborator.misses, 1,
            "first focused request should compute the user-elaborator template once"
        );
        assert_eq!(
            snapshot.user_elaborator.hits, 1,
            "second identical focused request should reuse the user-elaborator template memo"
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_missing_focus_fallback_reuses_worker_memos() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_lsp_user_elaborator_package(dir.path());
        std::fs::write(
            dir.path().join("bad.kio"),
            "module bad;\n\npub fn bad() -> . { () }\n",
        )
        .expect("replace failing sibling");
        let missing_focus = dir.path().join("missing.kio");
        let overlay = package_collection::SourceOverlay::empty();
        let cancel = crate::lsp::cancel::CancellationToken::new();
        let memos = LspUserElaboratorMemos::default();

        for _ in 0..2 {
            let result = analyze_module_at_with_overlay_lsp_cancellable_with_memos(
                dir.path(),
                &overlay,
                &missing_focus,
                &cancel,
                &memos,
            )
            .expect("fallback analysis should not be cancelled");
            assert!(
                result.is_ok(),
                "fallback analysis should succeed: {result:?}"
            );
        }

        let snapshot = memos
            .revisions
            .only_memo()
            .expect("fallback should populate the worker's package memo")
            .snapshot();
        assert_eq!(snapshot.user_elaborator.misses, 1);
        assert_eq!(snapshot.user_elaborator.hits, 1);
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_user_elaborator_memo_invalidates_after_provider_edit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main_path = write_lsp_user_elaborator_package(dir.path());
        let overlay = package_collection::SourceOverlay::empty();
        let cancel = crate::lsp::cancel::CancellationToken::new();
        let memos = LspUserElaboratorMemos::default();

        let first = analyze_module_at_with_overlay_lsp_cancellable_with_memos(
            dir.path(),
            &overlay,
            &main_path,
            &cancel,
            &memos,
        )
        .expect("focused analysis should not be cancelled");
        assert!(first.is_ok(), "baseline analysis should succeed: {first:?}");

        let provider_path = dir.path().join("elaborators.kio");
        let provider = std::fs::read_to_string(&provider_path).expect("read elaborator provider");
        let edited = provider.replace(
            "\nvalue\n",
            "\n__elab_error__(ct, \"edited elaborator ran\")\n",
        );
        assert_ne!(
            edited, provider,
            "provider edit should replace the branch body"
        );
        std::fs::write(provider_path, edited).expect("edit elaborator provider");

        let second = analyze_module_at_with_overlay_lsp_cancellable_with_memos(
            dir.path(),
            &overlay,
            &main_path,
            &cancel,
            &memos,
        )
        .expect("focused analysis should not be cancelled")
        .expect_err("edited elaborator should reject the call");
        assert!(
            second.errors.iter().any(|error| {
                let (_, message) = error.error.diag();
                message.contains("edited elaborator ran")
            }),
            "the edited provider, not the cached baseline, must be evaluated: {second:?}"
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn lsp_user_elaborator_fingerprint_ignores_unrelated_package_source() {
        fn key(name: &str) -> PackageKey {
            PackageKey {
                canonical_dir: PathBuf::from(name),
                package_name: name.to_owned(),
            }
        }

        fn package(path: &str, source: &str) -> package_collection::ParsedPackage {
            package_collection::ParsedPackage {
                root_dir: PathBuf::from(path),
                modules: Vec::new(),
                lazy_modules: BTreeMap::new(),
                package_file: None,
                dep_files: Vec::new(),
                sources: BTreeMap::from([(PathBuf::from(path), source.to_owned())]),
            }
        }

        let a = key("a");
        let b = key("b");
        let baseline = package_collection::ParsedPackageCollection {
            root: a.clone(),
            packages: BTreeMap::from([
                (a.clone(), package("a/main.kio", "module a/main;\n")),
                (b.clone(), package("b/main.kio", "module b/main;\n")),
            ]),
        };
        let edited_b = package_collection::ParsedPackageCollection {
            root: a.clone(),
            packages: BTreeMap::from([
                (a.clone(), package("a/main.kio", "module a/main;\n")),
                (
                    b.clone(),
                    package("b/main.kio", "module b/main;\n\nfn x() -> . { () }\n"),
                ),
            ]),
        };

        let baseline_fingerprints = lsp_user_elaborator_package_fingerprints(&baseline);
        let edited_fingerprints = lsp_user_elaborator_package_fingerprints(&edited_b);

        assert_eq!(baseline_fingerprints.get(&a), edited_fingerprints.get(&a));
        assert_ne!(baseline_fingerprints.get(&b), edited_fingerprints.get(&b));
    }
}

/// Generic check pipeline. The driver glue (cwd walk, parse, package
/// assembly, resolve, cycle check, in-body resolution, typer call)
/// is uniform; the [`Pipeline`] trait controls the lowering pass and
/// the typer entry point.
pub fn compile_package_with<P: Pipeline>() -> Result<Package<Prime>, ExitCode>
where
    P::LoweredPhase: ResolvePhase,
    P::LoweredPhase: crate::ast::Phase<TypeLabelSugar = crate::ast::Never, ExprResolved = ()>,
    P::LoweredPhase: Clone,
    P::LoweredPhase: serde::Serialize + serde::de::DeserializeOwned,
    Module<P::LoweredPhase>: Send,
    PackageEntry<P::LoweredPhase>: Send + Sync,
    Package<P::LoweredPhase>: Sync,
    PackageCollection<P::LoweredPhase>: Sync,
    crate::ast::Type<P::LoweredPhase>: Send + Sync,
    <crate::ast::Prime as crate::ast::Phase>::FnPurity:
        crate::ast::PhaseBridge<<P::LoweredPhase as crate::ast::Phase>::FnPurity>,
{
    // `skip_ok = false`: a fully typed root package is the contract.
    compile_workspace_with::<P>(false).map(|w| {
        w.root_package.expect(
            "compile_package_with passes skip_ok = false, so the root is always typechecked",
        )
    })
}

/// PackageCollection-aware compile pipeline. Walks `cwd`, parses each local
/// package source file, runs it through the [`Pipeline`] impl,
/// validates bridge admission, then typechecks each package.
///
/// `kio check` runs this with `skip_ok = true` and discards
/// everything past the root; `kio build` runs it with
/// `skip_ok = false` and emits each package.
///
/// When `skip_ok` is set, Phase 4 consults each package's
/// package-check cache: a package whose whole-source content
/// hash is unchanged skips re-typechecking entirely (it produces no
/// `Package<Prime>` — fine, the `kio check` caller discards the
/// workspace). Caches are (re)written after a green typecheck for
/// every non-skipped package.
pub fn compile_workspace_with<P: Pipeline>(
    skip_ok: bool,
) -> Result<TypedPackageCollection, ExitCode>
where
    P::LoweredPhase: ResolvePhase,
    P::LoweredPhase: crate::ast::Phase<TypeLabelSugar = crate::ast::Never, ExprResolved = ()>,
    P::LoweredPhase: Clone,
    P::LoweredPhase: serde::Serialize + serde::de::DeserializeOwned,
    Module<P::LoweredPhase>: Send,
    PackageEntry<P::LoweredPhase>: Send + Sync,
    Package<P::LoweredPhase>: Sync,
    PackageCollection<P::LoweredPhase>: Sync,
    crate::ast::Type<P::LoweredPhase>: Send + Sync,
    <crate::ast::Prime as crate::ast::Phase>::FnPurity:
        crate::ast::PhaseBridge<<P::LoweredPhase as crate::ast::Phase>::FnPurity>,
{
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return Err(ExitCode::Internal);
        }
    };
    compile_workspace_with_at::<P>(&cwd, skip_ok)
}

/// As [`compile_workspace_with`], but takes the workspace root
/// directory as an explicit argument. See [`compile_workspace_at`]
/// for the motivating use case.
///
/// Wraps [`analyze_workspace_with_at`] (which returns the structured
/// [`AnalysisFailure`] in-process callers consume) with the CLI's
/// print-and-return-exit-code shape.
pub fn compile_workspace_with_at<P: Pipeline>(
    root: &Path,
    skip_ok: bool,
) -> Result<TypedPackageCollection, ExitCode>
where
    P::LoweredPhase: ResolvePhase,
    P::LoweredPhase: crate::ast::Phase<TypeLabelSugar = crate::ast::Never, ExprResolved = ()>,
    P::LoweredPhase: Clone,
    P::LoweredPhase: serde::Serialize + serde::de::DeserializeOwned,
    Module<P::LoweredPhase>: Send,
    PackageEntry<P::LoweredPhase>: Send + Sync,
    Package<P::LoweredPhase>: Sync,
    PackageCollection<P::LoweredPhase>: Sync,
    crate::ast::Type<P::LoweredPhase>: Send + Sync,
    <crate::ast::Prime as crate::ast::Phase>::FnPurity:
        crate::ast::PhaseBridge<<P::LoweredPhase as crate::ast::Phase>::FnPurity>,
{
    analyze_workspace_with_at::<P>(root, skip_ok, &package_collection::SourceOverlay::empty())
        .map_err(|fail| report_analysis_failure(&fail))
}

/// As [`compile_workspace_with_at`], but renders the failure diagnostic
/// into `buf` rather than printing to stderr (`cmd::package_fanout`).
pub fn compile_workspace_with_at_buffered<P: Pipeline>(
    root: &Path,
    skip_ok: bool,
    buf: &mut String,
) -> Result<TypedPackageCollection, ExitCode>
where
    P::LoweredPhase: ResolvePhase,
    P::LoweredPhase: crate::ast::Phase<TypeLabelSugar = crate::ast::Never, ExprResolved = ()>,
    P::LoweredPhase: Clone,
    P::LoweredPhase: serde::Serialize + serde::de::DeserializeOwned,
    Module<P::LoweredPhase>: Send,
    PackageEntry<P::LoweredPhase>: Send + Sync,
    Package<P::LoweredPhase>: Sync,
    PackageCollection<P::LoweredPhase>: Sync,
    crate::ast::Type<P::LoweredPhase>: Send + Sync,
    <crate::ast::Prime as crate::ast::Phase>::FnPurity:
        crate::ast::PhaseBridge<<P::LoweredPhase as crate::ast::Phase>::FnPurity>,
{
    analyze_workspace_with_at::<P>(root, skip_ok, &package_collection::SourceOverlay::empty())
        .map_err(|fail| render_analysis_failure(&fail, buf))
}

/// In-process variant of [`compile_workspace_with_at`]: same pipeline
/// (parse → lower → resolve → typecheck), but failures return a
/// structured [`AnalysisFailure`] instead of printing the diagnostic
/// to stderr. The [`crate::lsp`] server consumes this to surface
/// type-error squiggles through `publishDiagnostics`.
///
/// `overlay` redirects per-file reads to in-memory text the caller
/// holds. Non-LSP callers pass `&SourceOverlay::empty()` so every read
/// falls through to disk; the LSP server populates the overlay with
/// the editor's buffer for every open URI before calling.
pub fn analyze_workspace_with_at<P: Pipeline>(
    root: &Path,
    skip_ok: bool,
    overlay: &package_collection::SourceOverlay,
) -> Result<TypedPackageCollection, AnalysisFailure>
where
    P::LoweredPhase: ResolvePhase,
    P::LoweredPhase: crate::ast::Phase<TypeLabelSugar = crate::ast::Never, ExprResolved = ()>,
    P::LoweredPhase: Clone,
    P::LoweredPhase: serde::Serialize + serde::de::DeserializeOwned,
    Module<P::LoweredPhase>: Send,
    PackageEntry<P::LoweredPhase>: Send + Sync,
    Package<P::LoweredPhase>: Sync,
    PackageCollection<P::LoweredPhase>: Sync,
    crate::ast::Type<P::LoweredPhase>: Send + Sync,
    <crate::ast::Prime as crate::ast::Phase>::FnPurity:
        crate::ast::PhaseBridge<<P::LoweredPhase as crate::ast::Phase>::FnPurity>,
{
    // Phase 1: collect and parse every local package source file.
    let log_frontend_timing = frontend_timing_log_enabled();
    let workspace_walk_start = log_frontend_timing.then(Instant::now);
    let parsed_ws = match package_collection::walk_with_overlay(root, overlay) {
        Ok(w) => w,
        Err((walk_err, partial_sources)) => {
            let mut sources: HashMap<PathBuf, String> = HashMap::new();
            for (k, v) in partial_sources {
                sources.insert(k, v);
            }
            // For parse failures the walker hadn't yet recorded the
            // failing file's source into `partial_sources` (the
            // insert happens *after* a successful parse), but the
            // error variant carries the source text on hand. Pull
            // it into the diagnostic source map so the LSP layer
            // (and the printing path) can build a `LineIndex`
            // against the real source rather than an empty string.
            if let package_collection::WalkError::Parse {
                path, source_text, ..
            } = &walk_err
            {
                sources.insert(path.clone(), source_text.clone());
            }
            let diag = walk_err.into_located();
            return Err(AnalysisFailure::from_error(diag, sources));
        }
    };
    let workspace_walk_elapsed = workspace_walk_start
        .map(|start| start.elapsed())
        .unwrap_or(Duration::ZERO);
    analyze_parsed_workspace_with::<P>(&parsed_ws, skip_ok, workspace_walk_elapsed)
}

/// Analyze a workspace that has already been walked and parsed — the
/// post-walk half of [`analyze_workspace_with_at`] (lower → resolve →
/// bridge-admit → typecheck, with the package-check cache). `kio test`
/// walks once to census `equiv` blocks, then hands the same parsed
/// workspace here for the no-equiv fast path so the walk is not
/// repeated. `walk_elapsed` is threaded through only for the frontend
/// timing probe.
pub(crate) fn analyze_parsed_workspace_with<P: Pipeline>(
    parsed_ws: &package_collection::ParsedPackageCollection,
    skip_ok: bool,
    walk_elapsed: Duration,
) -> Result<TypedPackageCollection, AnalysisFailure>
where
    P::LoweredPhase: ResolvePhase,
    P::LoweredPhase: crate::ast::Phase<TypeLabelSugar = crate::ast::Never, ExprResolved = ()>,
    P::LoweredPhase: Clone,
    P::LoweredPhase: serde::Serialize + serde::de::DeserializeOwned,
    Module<P::LoweredPhase>: Send,
    PackageEntry<P::LoweredPhase>: Send + Sync,
    Package<P::LoweredPhase>: Sync,
    PackageCollection<P::LoweredPhase>: Sync,
    crate::ast::Type<P::LoweredPhase>: Send + Sync,
    <crate::ast::Prime as crate::ast::Phase>::FnPurity:
        crate::ast::PhaseBridge<<P::LoweredPhase as crate::ast::Phase>::FnPurity>,
{
    let log_frontend_timing = frontend_timing_log_enabled();
    // Collect a global source map for diagnostic rendering.
    let source_map_start = log_frontend_timing.then(Instant::now);
    let mut sources: HashMap<PathBuf, String> = HashMap::new();
    for pkg in parsed_ws.packages.values() {
        for (k, v) in &pkg.sources {
            sources.insert(k.clone(), v.clone());
        }
    }
    let source_map_elapsed = source_map_start
        .map(|start| start.elapsed())
        .unwrap_or(Duration::ZERO);

    // Phase 1d: package-check cache. Compute each cache-enabled package's
    // whole-source hash once. When `skip_ok`, a package is
    // skip-eligible iff one on-disk entry validates against that
    // fingerprint and carries the lowered package-file summary.
    let package_cache_state_start = log_frontend_timing.then(Instant::now);
    let cache_state = PackageCheckCacheState::compute(parsed_ws, P::CACHE_TAG);
    let package_cache_state_elapsed = package_cache_state_start
        .map(|start| start.elapsed())
        .unwrap_or(Duration::ZERO);
    let typed_cache_state_start = log_frontend_timing.then(Instant::now);
    let typed_cache_state = TypedCacheState::compute(parsed_ws, P::CACHE_TAG);
    let typed_cache_state_elapsed = typed_cache_state_start
        .map(|start| start.elapsed())
        .unwrap_or(Duration::ZERO);
    let cache_reads_enabled = skip_ok && crate::cache::policy::caches_enabled();

    if log_frontend_timing {
        log_frontend_workspace_timing_line(
            &parsed_ws.root,
            FrontendWorkspaceTiming {
                walk: walk_elapsed,
                source_map: source_map_elapsed,
                package_cache_state: package_cache_state_elapsed,
                typed_cache_state: typed_cache_state_elapsed,
                packages: parsed_ws.packages.len(),
            },
        );
    }
    let mut lowered_workspace = PackageCollection {
        root: parsed_ws.root.clone(),
        packages: BTreeMap::new(),
    };

    // Phase 2–5: lower, resolve, validate bridge admission, and
    // typecheck each local package entry.
    let log_status = std::env::var("KIO_DEBUG_PACKAGE_CHECK_CACHE")
        .map(|v| v == "1")
        .unwrap_or(false);
    let root_key = parsed_ws.root.clone();
    let mut typed_by_package: BTreeMap<PackageKey, Package<Prime>> = BTreeMap::new();
    let mut package_check_skipped: BTreeSet<PackageKey> = BTreeSet::new();
    let package_keys: Vec<PackageKey> = parsed_ws.packages.keys().cloned().collect();
    let package_results: Vec<PackageStageResult<PackagePipelineOutcome<P::LoweredPhase>>> =
        crate::maybe_par_iter!(&package_keys)
            .map(|key| {
                let parsed_pkg = parsed_ws
                    .packages
                    .get(key)
                    .expect("package key came from ParsedPackageCollection::packages");
                let result: Result<PackagePipelineOutcome<P::LoweredPhase>, Vec<LocatedError>> =
                    (|| {
                        let package_check_lookup_start = Instant::now();
                        let package_check_hit = if cache_reads_enabled {
                            cache_state.load_entry::<P::LoweredPhase>(key)
                        } else {
                            None
                        };
                        let package_check_lookup = package_check_lookup_start.elapsed();
                        let package_check_skipped = package_check_hit.is_some();
                        if package_check_skipped && *key == root_key {
                            if log_frontend_timing {
                                log_frontend_timing_line(
                                    key,
                                    FrontendTiming {
                                        prepare: Duration::ZERO,
                                        forced_modules: 0,
                                        typed_hits: 0,
                                        typed_misses: 0,
                                        package_check_lookup,
                                        typed_cache_lookup: Duration::ZERO,
                                        typed_cache_store: Duration::ZERO,
                                        lower_resolve: Duration::ZERO,
                                        pipeline_typecheck: Duration::ZERO,
                                        prime_validation: Duration::ZERO,
                                        package_check_skipped: true,
                                        import_body_type: Duration::ZERO,
                                        summary_levels: 0,
                                        summary_max_width: 0,
                                        user_elaborator_timing: Default::default(),
                                    },
                                );
                            }
                            return Ok(PackagePipelineOutcome {
                                lowered: None,
                                typechecked: PackageTypecheckOutcome::Skipped,
                            });
                        }
                        if let Some(entry) = package_check_hit {
                            if log_frontend_timing {
                                log_frontend_timing_line(
                                    key,
                                    FrontendTiming {
                                        prepare: Duration::ZERO,
                                        forced_modules: 0,
                                        typed_hits: 0,
                                        typed_misses: 0,
                                        package_check_lookup,
                                        typed_cache_lookup: Duration::ZERO,
                                        typed_cache_store: Duration::ZERO,
                                        lower_resolve: Duration::ZERO,
                                        pipeline_typecheck: Duration::ZERO,
                                        prime_validation: Duration::ZERO,
                                        package_check_skipped: true,
                                        import_body_type: Duration::ZERO,
                                        summary_levels: 0,
                                        summary_max_width: 0,
                                        user_elaborator_timing: Default::default(),
                                    },
                                );
                            }
                            return Ok(PackagePipelineOutcome {
                                lowered: Some(PackageEntry {
                                    root_dir: parsed_pkg.root_dir.clone(),
                                    package: Package::<P::LoweredPhase>::from_parts(
                                        BTreeMap::new(),
                                        entry.package_file,
                                    ),
                                }),
                                typechecked: PackageTypecheckOutcome::Skipped,
                            });
                        }
                        let summary_start = Instant::now();
                        let (lowering_context, mut summary) =
                            build_package_summary::<P>(key, parsed_pkg, &typed_cache_state)
                                .map_err(one_stage_error)?;
                        let summary_elapsed = summary_start.elapsed();
                        let typed_hit_count = summary.typed_hit_count;
                        let typed_miss_count = summary.typed_miss_count;
                        let typed_cache_lookup = summary.typed_cache_lookup;
                        let summary_levels = summary.levels.len();
                        let summary_max_width =
                            summary.levels.iter().map(Vec::len).max().unwrap_or(0);
                        let lower_start = Instant::now();
                        let mut lowered = summary.lowered.clone();
                        let lower_elapsed = lower_start.elapsed();
                        let pipeline_typecheck_start = Instant::now();
                        let (
                            pending_typechecked,
                            forced_modules,
                            typed_cache_store,
                            user_elaborator_timing,
                        ) = if package_check_skipped {
                            (None, 0, Duration::ZERO, Default::default())
                        } else {
                            let checked = typecheck_package_with_summary_scheduler::<P>(
                                key,
                                parsed_pkg,
                                &lowering_context,
                                &mut summary,
                                &typed_cache_state,
                            )?;
                            lowered = summary.lowered;
                            (
                                Some(checked.package),
                                checked.forced_modules,
                                checked.typed_cache_store,
                                checked.user_elaborator_timing,
                            )
                        };
                        let pipeline_typecheck = pipeline_typecheck_start.elapsed();
                        let import_elapsed = Duration::ZERO;
                        let mut prime_validation = Duration::ZERO;
                        let typechecked = if let Some(package) = pending_typechecked {
                            let validate_start = Instant::now();
                            let validated = crate::prime::typer::check_normalized_package(package)
                                .map_err(one_stage_error)?;
                            prime_validation = validate_start.elapsed();
                            PackageTypecheckOutcome::Checked(Box::new(validated))
                        } else {
                            PackageTypecheckOutcome::Skipped
                        };
                        if log_frontend_timing {
                            log_frontend_timing_line(
                                key,
                                FrontendTiming {
                                    prepare: summary_elapsed,
                                    forced_modules,
                                    typed_hits: typed_hit_count,
                                    typed_misses: typed_miss_count,
                                    package_check_lookup,
                                    typed_cache_lookup,
                                    typed_cache_store,
                                    lower_resolve: lower_elapsed,
                                    pipeline_typecheck,
                                    prime_validation,
                                    package_check_skipped,
                                    import_body_type: import_elapsed,
                                    summary_levels,
                                    summary_max_width,
                                    user_elaborator_timing,
                                },
                            );
                        }
                        Ok(PackagePipelineOutcome {
                            lowered: Some(lowered),
                            typechecked,
                        })
                    })();
                PackageStageResult {
                    key: key.clone(),
                    result,
                }
            })
            .collect();
    let errors = package_stage_errors(&package_results);
    if !errors.is_empty() {
        return Err(AnalysisFailure::from_errors(errors, sources));
    }
    for result in package_results {
        let outcome = result
            .result
            .expect("package pipeline errors returned before workspace insertion");
        match outcome.typechecked {
            PackageTypecheckOutcome::Skipped => {
                package_check_skipped.insert(result.key.clone());
                if log_status {
                    eprintln!(
                        "package-check-cache hit: {} (typecheck skipped)",
                        result.key.package_name
                    );
                }
            }
            PackageTypecheckOutcome::Checked(package) => {
                typed_by_package.insert(result.key.clone(), *package);
            }
        }
        if let Some(lowered) = outcome.lowered {
            lowered_workspace.packages.insert(result.key, lowered);
        }
    }

    // Phase 4b: write each non-skipped package's package-check
    // cache. The whole workspace typechecked green above (any
    // failure returned early), so every cache file written here
    // reflects a known-good public surface — partial caches never
    // land. Skipped packages already have a valid on-disk cache,
    // so they're left alone. `KIO_DEBUG_PACKAGE_CHECK_CACHE=1` logs
    // each write's hit / miss / first-write status.
    //
    // Gated on [`cache::policy::caches_enabled`]: when the operator
    // hasn't opted in to the Kio-semantic caches, the write loop
    // is skipped entirely so the cache stays cold (no stale entries
    // accumulate on disk between development edits).
    if crate::cache::policy::caches_enabled() {
        let package_cache_store_keys: Vec<PackageKey> =
            parsed_ws.packages.keys().cloned().collect();
        let mut package_cache_store_results: Vec<_> =
            crate::maybe_par_iter!(&package_cache_store_keys)
                .filter(|key| !package_check_skipped.contains(*key))
                .filter_map(|key| {
                    let lowered = lowered_workspace.packages.get(key)?;
                    let cache = cache_state.cache(key);
                    if !cache.is_enabled() {
                        return None;
                    }
                    let cache_key = cache_state.cache_key(key);
                    let status = log_status.then(|| match cache.entry_path(&cache_key) {
                        Some(path) if path.exists() => "miss (re-checked)",
                        _ => "first-write",
                    });
                    let entry = cache_state
                        .entry_for::<P::LoweredPhase>(key, lowered.package.package_file().cloned());
                    let store_start = Instant::now();
                    let store_result = cache.store(&cache_key, &entry);
                    let duration = store_start.elapsed();
                    let warning = store_result.err().map(|e| {
                        let path = cache
                            .entry_path(&cache_key)
                            .map(|p| DisplayPath(&p).to_string())
                            .unwrap_or_else(|| "<disabled>".to_owned());
                        format!("warning: could not write package-check cache `{path}`: {e}")
                    });
                    Some(PackageCheckStoreResult {
                        key: key.clone(),
                        status,
                        duration,
                        warning,
                    })
                })
                .collect();
        package_cache_store_results.sort_by(|a, b| a.key.cmp(&b.key));
        for result in package_cache_store_results {
            if let Some(status) = result.status {
                eprintln!("package-check-cache {status}: {}", result.key.package_name);
            }
            if log_frontend_timing {
                log_frontend_package_store_timing_line(&result.key, result.duration);
            }
            if let Some(warning) = result.warning {
                eprintln!("{warning}");
            }
        }
    }

    // Phase 6: hand the typed collection over to the caller. When
    // `skip_ok` skipped the root, `root_package` is `None` — the
    // `kio check` caller discards the collection, so that's fine;
    // `kio build` never skips, so it always sees `Some`.
    let root_package = typed_by_package.remove(&root_key);
    Ok(TypedPackageCollection {
        root_key,
        root_package,
    })
}

/// Per-run package-check-cache state: each cache-enabled package's current
/// whole-source content hash, computed once from the parsed workspace. Drives
/// both package-skip decisions and the cache writes after a green workspace
/// check.
struct PackageCheckCacheState {
    /// Each cache-enabled package's current whole-source content hash. Moves on
    /// any source edit — `pub` or not, signature or body.
    source_hashes: BTreeMap<PackageKey, SourceHash>,
    /// Active semantic cache roots. Missing entries are disabled.
    cache_roots: crate::cache::roots::SemanticCacheRoots,
    /// Pipeline-specific infix for the cache filename
    /// ([`Pipeline::CACHE_TAG`]) — keeps the `kio` and
    /// `kio-prime` binaries from sharing a cache file.
    cache_tag: PipelineTag,
}

struct TypedCacheState {
    cache_roots: crate::cache::roots::SemanticCacheRoots,
    debug_shared_root: Option<PathBuf>,
    module_deps: BTreeMap<(PackageKey, PackageModuleKey), Vec<TypedModuleDependency>>,
    cache_tag: PipelineTag,
}

impl TypedCacheState {
    #[cfg(feature = "surface")]
    fn disabled(cache_tag: &'static str) -> Self {
        Self {
            cache_roots: crate::cache::roots::SemanticCacheRoots::empty(),
            debug_shared_root: None,
            module_deps: BTreeMap::new(),
            cache_tag: PipelineTag::new(cache_tag),
        }
    }

    fn compute(
        parsed_ws: &package_collection::ParsedPackageCollection,
        cache_tag: &'static str,
    ) -> Self {
        Self::compute_with_policy(parsed_ws, cache_tag, crate::cache::policy::caches_enabled())
    }

    fn compute_with_policy(
        parsed_ws: &package_collection::ParsedPackageCollection,
        cache_tag: &'static str,
        caches_enabled: bool,
    ) -> Self {
        let cache_tag = PipelineTag::new(cache_tag);
        let cache_roots = crate::cache::roots::SemanticCacheRoots::compute(parsed_ws);
        if !caches_enabled {
            return Self {
                cache_roots: crate::cache::roots::SemanticCacheRoots::empty(),
                debug_shared_root: None,
                module_deps: BTreeMap::new(),
                cache_tag,
            };
        }
        let debug_shared_root = std::env::var_os("KIO_DEBUG_TYPED_CACHE_ROOT")
            .filter(|root| !root.is_empty())
            .map(PathBuf::from)
            .filter(|root| root.is_absolute());
        let mut module_deps = BTreeMap::new();
        // Package name, build configuration, and bridge globs do not enter a
        // regular module's lowering or typing environment. The current package
        // boundary is rebuilt and validated in `build_package_summary` before
        // any typed-module lookup, so only module-typing dependencies belong in
        // this semantic key.
        for (key, parsed_pkg) in &parsed_ws.packages {
            if cache_roots.root_for(key).is_none() {
                continue;
            }
            let same_package_surfaces = same_package_public_surfaces(parsed_pkg);
            let same_package_elaborator_impls = same_package_elaborator_impl_surfaces(parsed_pkg);
            let same_package_known_modules: BTreeSet<PackageModuleKey> =
                same_package_surfaces.keys().cloned().collect();
            let same_package_elaborator_known_modules: BTreeSet<PackageModuleKey> =
                same_package_elaborator_impls.keys().cloned().collect();
            let package_name = parsed_pkg
                .package_file
                .as_ref()
                .map(|e| PackageName::new(e.package_name.clone()));
            for (_, module) in &parsed_pkg.modules {
                let module_path = package_storage_module_path(module, package_name.as_ref());
                let mut deps = Vec::new();
                collect_same_package_import_fingerprints(
                    module,
                    package_name.as_ref(),
                    &same_package_surfaces,
                    &same_package_elaborator_impls,
                    &same_package_known_modules,
                    &same_package_elaborator_known_modules,
                    &mut deps,
                );
                deps.sort_by_key(TypedModuleDependency::label);
                module_deps.insert((key.clone(), module_path), deps);
            }
        }

        Self {
            cache_roots,
            debug_shared_root,
            module_deps,
            cache_tag,
        }
    }

    fn cache_for(&self, key: &PackageKey) -> crate::cache::typed::TypedModuleCache {
        if !crate::cache::policy::caches_enabled() {
            return crate::cache::typed::TypedModuleCache::disabled();
        }
        let Some(package_root) = self.cache_roots.root_for(key) else {
            return crate::cache::typed::TypedModuleCache::disabled();
        };
        let root = self.debug_shared_root.as_deref().unwrap_or(package_root);
        crate::cache::typed::TypedModuleCache::open(root.to_path_buf())
            .unwrap_or_else(|_| crate::cache::typed::TypedModuleCache::disabled())
    }

    fn key_for_active_cache(
        &self,
        cache: &crate::cache::typed::TypedModuleCache,
        package_key: &PackageKey,
        parsed_pkg: &ParsedPackage,
        module_path: &PackageModuleKey,
        file_path: &Path,
    ) -> Option<crate::cache::typed::TypedModuleCacheKey> {
        if !cache.is_enabled() {
            return None;
        }
        #[cfg(test)]
        typed_cache_work_counters::record_key();
        let source = parsed_pkg
            .sources
            .get(file_path)
            .expect("ParsedPackage::sources contains every module file path")
            .as_bytes();
        let deps = self
            .module_deps
            .get(&(package_key.clone(), module_path.clone()))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        Some(crate::cache::typed::TypedModuleCacheKey::new(
            PackageName::from_package_key(package_key),
            module_path.clone(),
            self.cache_tag,
            source,
            deps,
        ))
    }
}

struct ModuleSummary {
    file_path: PathBuf,
    source_index: usize,
    typed_hit: Option<Module<Prime>>,
    typed_miss: bool,
}

struct PackageSummary<P: crate::ast::Phase> {
    lowered: PackageEntry<P>,
    modules: BTreeMap<PackageModuleKey, ModuleSummary>,
    levels: Vec<Vec<PackageModuleKey>>,
    typed_hit_count: usize,
    typed_miss_count: usize,
    typed_cache_lookup: Duration,
}

struct ForcedLoweredModule<P: crate::ast::Phase> {
    module_path: PackageModuleKey,
    entry: ModuleEntry<P>,
}

struct ScheduledTypecheckResult {
    package: crate::pass::alpha_normalize::AlphaNormalizedPackage<Prime>,
    forced_modules: usize,
    typed_cache_store: Duration,
    user_elaborator_timing: crate::pass::typecheck_core::UserElaboratorTimingSnapshot,
}

fn assemble_scheduled_typecheck_package<P>(
    normalized: crate::pass::alpha_normalize::AlphaNormalizedPackage<P>,
    typed_modules: BTreeMap<String, ModuleEntry<Prime>>,
    package_file: Option<crate::pass::resolve::PackageFileEntry<Prime>>,
) -> crate::pass::alpha_normalize::AlphaNormalizedPackage<Prime>
where
    P: crate::pass::visit_mut::TypecheckVisitPhase,
{
    normalized.renormalize_after(|_| Package::<Prime>::from_parts(typed_modules, package_file))
}

fn scheduled_typed_module_entry(file_path: PathBuf, module: Module<Prime>) -> ModuleEntry<Prime> {
    // A module typecheck may inject Prime-qualified imports and generated
    // aliases, whether it ran locally or arrived through the typed cache.
    // Build the one derived index from that exact transformed module.
    let scope = crate::pass::resolve::TopLevelScope::build(&module).unwrap_or_else(|error| {
        unreachable!("scheduled typechecking produced an invalid top-level scope: {error:?}")
    });
    ModuleEntry {
        file_path,
        module,
        scope,
    }
}

struct PackageCheckStoreResult {
    key: PackageKey,
    status: Option<&'static str>,
    duration: Duration,
    warning: Option<String>,
}

fn validate_source_package_with_parse_precedence<P: Pipeline>(
    parsed_pkg: &ParsedPackage,
) -> Result<(), LocatedError> {
    P::validate_source_package(&parsed_pkg.root_dir, &parsed_pkg.modules)
        .map_err(|error| parsed_pkg.prefer_deferred_body_error(error))
}

fn build_package_summary<P: Pipeline>(
    package_key: &PackageKey,
    parsed_pkg: &ParsedPackage,
    cache_state: &TypedCacheState,
) -> Result<(P::LoweringContext, PackageSummary<P::LoweredPhase>), LocatedError>
where
    P::LoweredPhase: ResolvePhase,
    P::LoweredPhase: crate::ast::Phase<TypeLabelSugar = crate::ast::Never, ExprResolved = ()>,
    P::LoweredPhase: Clone,
{
    validate_source_package_with_parse_precedence::<P>(parsed_pkg)?;
    let prepared_pkg = if parsed_pkg.package_file.is_none() {
        Some(force_parsed_package_bodies(parsed_pkg)?)
    } else {
        None
    };
    let parsed_pkg = prepared_pkg.as_ref().unwrap_or(parsed_pkg);

    let package_name = parsed_pkg
        .package_file
        .as_ref()
        .map(|e| PackageName::new(e.package_name.clone()));
    let context = P::lowering_context_named(
        &parsed_pkg.modules,
        parsed_pkg.package_file.as_ref().map(|e| &e.package_file),
        package_name.as_ref().map(PackageName::as_str),
    )?;

    let mut lowered_modules = Vec::with_capacity(parsed_pkg.modules.len());
    for (file_path, module) in &parsed_pkg.modules {
        let lowered = P::lower_module_with_context(&context, file_path.clone(), module.clone())?;
        lowered_modules.push((file_path.clone(), lowered));
    }

    // A package-less module tree (no `*.pkg.kio`) is admitted here and
    // analysed for its own sake — typechecked and resolved against its
    // `import`-closure, with no package contract (env/bridge/export).
    // Modules exist independently of packages, so `kio check` / `kio test`
    // (and `kio repl` / `kio lsp`, which share this path) work on a bare
    // module tree as well as a package. `kio build` is unaffected: it
    // REQUIRES a package and rejects a package-less tree at its own
    // Phase-0 marker check (`cmd/build.rs`), before this shared path is
    // ever reached.

    let package_file_entry = match (
        &parsed_pkg.package_file,
        P::lowered_package_file_from_context(&context),
    ) {
        (Some(prev), Some(package_file)) => Some(PackageFileEntry {
            file_path: prev.file_path.clone(),
            package_name: prev.package_name.clone(),
            package_file,
        }),
        _ => None,
    };
    let package = Package::<P::LoweredPhase>::build(
        &parsed_pkg.root_dir,
        lowered_modules,
        package_file_entry,
    )?;
    package.resolve_imports()?;
    package.check_no_value_cycles()?;
    package.check_binding_origins()?;

    let levels = package
        .value_import_topo_levels()
        .into_iter()
        .map(|level| {
            level
                .into_iter()
                .map(|(module_path, _)| PackageModuleKey::new(module_path.to_owned()))
                .collect()
        })
        .collect();

    let cache = cache_state.cache_for(package_key);
    let mut modules = BTreeMap::new();
    let mut typed_hit_count = 0;
    let mut typed_miss_count = 0;
    let mut typed_cache_lookup = Duration::ZERO;
    for (source_index, (file_path, module)) in parsed_pkg.modules.iter().enumerate() {
        let module_path = package_storage_module_path(module, package_name.as_ref());
        let mut typed_hit = None;
        let mut typed_miss = false;
        if let Some(key) = cache_state.key_for_active_cache(
            &cache,
            package_key,
            parsed_pkg,
            &module_path,
            file_path,
        ) {
            let lookup_start = Instant::now();
            typed_hit = cache.lookup(&key);
            typed_cache_lookup += lookup_start.elapsed();
            if typed_hit.is_some() {
                typed_hit_count += 1;
            } else {
                typed_miss = true;
                typed_miss_count += 1;
            }
        }
        modules.insert(
            module_path,
            ModuleSummary {
                file_path: file_path.clone(),
                source_index,
                typed_hit,
                typed_miss,
            },
        );
    }

    Ok((
        context,
        PackageSummary {
            lowered: PackageEntry {
                root_dir: parsed_pkg.root_dir.clone(),
                package,
            },
            modules,
            levels,
            typed_hit_count,
            typed_miss_count,
            typed_cache_lookup,
        },
    ))
}

fn typecheck_package_with_summary_scheduler<P: Pipeline>(
    package_key: &PackageKey,
    parsed_pkg: &ParsedPackage,
    context: &P::LoweringContext,
    summary: &mut PackageSummary<P::LoweredPhase>,
    cache_state: &TypedCacheState,
) -> Result<ScheduledTypecheckResult, Vec<LocatedError>>
where
    P::LoweredPhase: ResolvePhase,
    P::LoweredPhase: crate::ast::Phase<TypeLabelSugar = crate::ast::Never, ExprResolved = ()>,
    P::LoweredPhase: Clone,
    Module<P::LoweredPhase>: Send,
    Package<P::LoweredPhase>: Sync,
    crate::ast::Type<P::LoweredPhase>: Send + Sync,
{
    let cache = cache_state.cache_for(package_key);
    let mut typed_modules: BTreeMap<String, ModuleEntry<Prime>> = BTreeMap::new();
    let mut typed_cache_store = Duration::ZERO;
    // A typed-cache hit skips a module's own typechecking, so the
    // optimization normally leaves that module's body un-forced and
    // un-lowered in `summary.lowered.package`. But a cache-*miss* module
    // is retyped, and retyping evaluates any user elaborator it imports
    // against the lowered body of the *defining* module. If that defining
    // module is a cache hit, its un-forced placeholder body would be
    // evaluated instead of the real elaborator, silently degrading the
    // result. So any module a cache-miss module transitively depends on
    // must also be force-lowered (its typed result still comes from the
    // cache; only its lowered body is needed). When every module hits,
    // this set is empty and the warm fast path is unaffected. Materialize the
    // complete set before constructing the typecheck scope: its content memos
    // and evaluator artifacts describe one immutable lowered package snapshot.
    let force_for_miss_consumers =
        force_lower_targets_for_miss_consumers(parsed_pkg, &summary.modules);
    let mut forced_modules = 0;
    for level in &summary.levels {
        let force_targets: Vec<PackageModuleKey> = level
            .iter()
            .filter(|module_path| {
                summary
                    .modules
                    .get(*module_path)
                    .is_some_and(|module| module.typed_hit.is_none())
                    || force_for_miss_consumers.contains(*module_path)
            })
            .cloned()
            .collect();
        let forced_results: Vec<PackageStageResult<ForcedLoweredModule<P::LoweredPhase>>> =
            crate::maybe_par_iter!(force_targets)
                .map(|module_path| {
                    let result = force_and_lower_module::<P>(
                        parsed_pkg,
                        context,
                        &summary.modules,
                        module_path,
                    )
                    .map_err(one_stage_error);
                    PackageStageResult {
                        key: package_key.clone(),
                        result,
                    }
                })
                .collect();
        let errors = package_stage_errors(&forced_results);
        if !errors.is_empty() {
            return Err(errors);
        }
        forced_modules += forced_results.len();
        for result in forced_results {
            let module = result
                .result
                .expect("module errors returned before package update");
            summary
                .lowered
                .package
                .replace_module(module.module_path.into_string(), module.entry);
        }
    }

    let normalized = crate::pass::alpha_normalize::normalize_package(&summary.lowered.package);
    summary.lowered.package = normalized.package().clone();
    let typecheck_scope =
        crate::pass::typecheck_core::PackageTypecheckScope::fresh_for_normalized_package(
            &normalized,
            std::sync::Arc::new(crate::pass::typecheck_core::TypeInterner::default()),
        );
    for level in &summary.levels {
        let typecheck_results: Vec<
            PackageStageResult<(PackageModuleKey, ModuleEntry<Prime>, Duration)>,
        > = crate::maybe_par_iter!(level)
            .map(|module_path| {
                let result: Result<
                    (PackageModuleKey, ModuleEntry<Prime>, Duration),
                    Vec<LocatedError>,
                > = (|| {
                    let module_summary = summary
                        .modules
                        .get(module_path)
                        .expect("summary level path came from PackageSummary::modules");
                    let mut store_elapsed = Duration::ZERO;
                    let typed = match module_summary.typed_hit.clone() {
                        Some(cached) => cached,
                        None => {
                            let entry = normalized
                                .package()
                                .module(module_path.as_str())
                                .expect("forced module was inserted into package");
                            let typed = P::typecheck_module_with_typecheck_scope(
                                module_path.as_str(),
                                entry,
                                normalized.package(),
                                typecheck_scope.clone(),
                            )?;
                            if module_summary.typed_miss
                                && let Some(key) = cache_state.key_for_active_cache(
                                    &cache,
                                    package_key,
                                    parsed_pkg,
                                    module_path,
                                    &entry.file_path,
                                )
                            {
                                let store_start = Instant::now();
                                cache.store(&key, &typed);
                                store_elapsed += store_start.elapsed();
                            }
                            typed
                        }
                    };
                    Ok((
                        module_path.clone(),
                        scheduled_typed_module_entry(module_summary.file_path.clone(), typed),
                        store_elapsed,
                    ))
                })();
                PackageStageResult {
                    key: package_key.clone(),
                    result,
                }
            })
            .collect();
        let errors = package_stage_errors(&typecheck_results);
        if !errors.is_empty() {
            return Err(errors);
        }
        for result in typecheck_results {
            let (module_path, entry, store_elapsed) = result
                .result
                .expect("typecheck errors returned before typed package update");
            typed_cache_store += store_elapsed;
            typed_modules.insert(module_path.into_string(), entry);
        }
    }
    let package_file = P::typecheck_package_file(normalized.package()).map_err(one_stage_error)?;
    Ok(ScheduledTypecheckResult {
        package: assemble_scheduled_typecheck_package(normalized, typed_modules, package_file),
        forced_modules,
        typed_cache_store,
        user_elaborator_timing: typecheck_scope.user_elaborator_timing_snapshot(),
    })
}

fn force_and_lower_module<P: Pipeline>(
    parsed_pkg: &ParsedPackage,
    context: &P::LoweringContext,
    summaries: &BTreeMap<PackageModuleKey, ModuleSummary>,
    module_path: &PackageModuleKey,
) -> Result<ForcedLoweredModule<P::LoweredPhase>, LocatedError>
where
    P::LoweredPhase: ResolvePhase,
    P::LoweredPhase: crate::ast::Phase<TypeLabelSugar = crate::ast::Never, ExprResolved = ()>,
    P::LoweredPhase: Clone,
{
    let summary = summaries
        .get(module_path)
        .expect("force target came from package summary");
    let surface = match parsed_pkg.lazy_modules.get(&summary.file_path) {
        Some(lazy) => lazy.force_all().map_err(|error| LocatedError {
            file_path: summary.file_path.clone(),
            error,
        })?,
        None => parsed_pkg
            .modules
            .get(summary.source_index)
            .map(|(_, module)| module.clone())
            .expect("module summary source index came from ParsedPackage::modules"),
    };
    let lowered = P::lower_module_with_context(context, summary.file_path.clone(), surface)?;
    if let Err(error) = crate::pass::resolve::Resolver::check_module(&lowered) {
        return Err(LocatedError {
            file_path: summary.file_path.clone(),
            error,
        });
    }
    let scope =
        crate::pass::resolve::TopLevelScope::build(&lowered).map_err(|error| LocatedError {
            file_path: summary.file_path.clone(),
            error,
        })?;
    Ok(ForcedLoweredModule {
        module_path: module_path.clone(),
        entry: ModuleEntry {
            file_path: summary.file_path.clone(),
            module: lowered,
            scope,
        },
    })
}

/// Modules that must be force-lowered because a typed-cache *miss*
/// module transitively depends on them. A cache hit otherwise leaves a
/// module's body un-forced; that is safe only when nobody re-evaluates
/// that body. A retyped (miss) consumer does re-evaluate an imported
/// elaborator's defining-module body at compile time, so every module
/// reachable from a miss must carry its real lowered body even when its
/// own typed result is served from cache. Empty when nothing misses.
fn force_lower_targets_for_miss_consumers(
    parsed_pkg: &ParsedPackage,
    modules: &BTreeMap<PackageModuleKey, ModuleSummary>,
) -> BTreeSet<PackageModuleKey> {
    let package_name = parsed_pkg
        .package_file
        .as_ref()
        .map(|e| PackageName::new(e.package_name.clone()));
    let mut targets = BTreeSet::new();
    for (module_key, summary) in modules {
        if !summary.typed_miss {
            continue;
        }
        for dep in same_package_dependency_closure(module_key, parsed_pkg, package_name.as_ref()) {
            targets.insert(dep);
        }
    }
    targets
}

fn package_storage_module_path(
    module: &Module<Surface>,
    package_name: Option<&PackageName>,
) -> PackageModuleKey {
    let declared = declared_module_path(&module.path);
    package_storage_module_key(&declared, package_name)
}

fn package_storage_module_key(
    declared: &DeclaredModulePath,
    package_name: Option<&PackageName>,
) -> PackageModuleKey {
    PackageModuleKey::from_declared(declared, package_name)
}

fn materialize_parsed_package_modules(
    parsed_pkg: &ParsedPackage,
    materialize: impl Fn(&crate::pass::parser::LazyModule) -> Result<Module<Surface>, Error> + Sync,
) -> Result<ParsedPackage, LocatedError> {
    let prepared_results: Vec<Result<(PathBuf, Module<Surface>), LocatedError>> =
        crate::maybe_par_iter!(&parsed_pkg.modules)
            .map(
                |(file_path, module)| match parsed_pkg.lazy_modules.get(file_path) {
                    Some(lazy) => materialize(lazy)
                        .map(|forced| (file_path.clone(), forced))
                        .map_err(|error| LocatedError {
                            file_path: file_path.clone(),
                            error,
                        }),
                    None => Ok((file_path.clone(), module.clone())),
                },
            )
            .collect();
    let mut modules = Vec::with_capacity(prepared_results.len());
    for result in prepared_results {
        modules.push(result?);
    }
    Ok(ParsedPackage {
        root_dir: parsed_pkg.root_dir.clone(),
        modules,
        lazy_modules: parsed_pkg.lazy_modules.clone(),
        package_file: parsed_pkg.package_file.clone(),
        dep_files: parsed_pkg.dep_files.clone(),
        sources: parsed_pkg.sources.clone(),
    })
}

fn force_parsed_package_bodies(parsed_pkg: &ParsedPackage) -> Result<ParsedPackage, LocatedError> {
    materialize_parsed_package_modules(parsed_pkg, crate::pass::parser::LazyModule::force_all)
}

fn same_package_public_surfaces(
    parsed_pkg: &ParsedPackage,
) -> BTreeMap<PackageModuleKey, SurfaceFingerprint> {
    #[cfg(test)]
    typed_cache_work_counters::record_fingerprint();
    let package_name = parsed_pkg
        .package_file
        .as_ref()
        .map(|e| PackageName::new(e.package_name.clone()));
    let mut direct_fingerprints = BTreeMap::new();
    for (_, module) in &parsed_pkg.modules {
        direct_fingerprints.insert(
            package_storage_module_path(module, package_name.as_ref()),
            public_surface_fingerprint(module),
        );
    }
    let known_modules: BTreeSet<PackageModuleKey> = direct_fingerprints.keys().cloned().collect();
    let mut deps_by_module: BTreeMap<PackageModuleKey, Vec<PackageModuleKey>> = BTreeMap::new();
    for (_, module) in &parsed_pkg.modules {
        let module_path = package_storage_module_path(module, package_name.as_ref());
        deps_by_module.insert(
            module_path,
            same_package_import_targets(module, package_name.as_ref(), &known_modules),
        );
    }

    compose_transitive_same_package_fingerprints(
        &direct_fingerprints,
        &deps_by_module,
        "same-package-public-surface",
    )
}

fn same_package_elaborator_impl_surfaces(
    parsed_pkg: &ParsedPackage,
) -> BTreeMap<PackageModuleKey, SurfaceFingerprint> {
    #[cfg(test)]
    typed_cache_work_counters::record_fingerprint();
    let package_name = parsed_pkg
        .package_file
        .as_ref()
        .map(|e| PackageName::new(e.package_name.clone()));
    let mut direct_fingerprints = BTreeMap::new();
    for (file_path, module) in &parsed_pkg.modules {
        let source = parsed_pkg
            .sources
            .get(file_path)
            .expect("ParsedPackage::sources contains every module file path");
        direct_fingerprints.insert(
            package_storage_module_path(module, package_name.as_ref()),
            source_fingerprint(source),
        );
    }
    let known_modules: BTreeSet<PackageModuleKey> = direct_fingerprints.keys().cloned().collect();
    let mut deps_by_module: BTreeMap<PackageModuleKey, Vec<PackageModuleKey>> = BTreeMap::new();
    for (_, module) in &parsed_pkg.modules {
        let module_path = package_storage_module_path(module, package_name.as_ref());
        deps_by_module.insert(
            module_path,
            same_package_import_targets(module, package_name.as_ref(), &known_modules),
        );
    }

    compose_transitive_same_package_fingerprints(
        &direct_fingerprints,
        &deps_by_module,
        "same-package-elaborator-impl",
    )
}

fn public_surface_fingerprint(module: &Module<Surface>) -> SurfaceFingerprint {
    let entries = public_surface_entries(module);
    let mut h = blake3::Hasher::new();
    write_fingerprint_part(&mut h, b"module-public-surface");
    for entry in &entries {
        write_fingerprint_part(&mut h, entry.as_bytes());
    }
    finish_surface_fingerprint(h)
}

fn source_fingerprint(source: &str) -> SurfaceFingerprint {
    let mut h = blake3::Hasher::new();
    write_fingerprint_part(&mut h, b"module-source");
    write_fingerprint_part(&mut h, source.as_bytes());
    finish_surface_fingerprint(h)
}

fn compose_transitive_same_package_fingerprints(
    direct_fingerprints: &BTreeMap<PackageModuleKey, SurfaceFingerprint>,
    deps_by_module: &BTreeMap<PackageModuleKey, Vec<PackageModuleKey>>,
    context_tag: &str,
) -> BTreeMap<PackageModuleKey, SurfaceFingerprint> {
    direct_fingerprints
        .iter()
        .map(|(module_path, direct)| {
            let mut h = blake3::Hasher::new();
            write_fingerprint_part(&mut h, context_tag.as_bytes());
            write_fingerprint_part(&mut h, b"module");
            write_fingerprint_part(&mut h, module_path.as_str().as_bytes());
            write_fingerprint_part(&mut h, b"direct");
            write_fingerprint_part(&mut h, direct.as_str().as_bytes());

            let reachable = transitive_same_package_deps(module_path, deps_by_module);
            for dep_path in reachable {
                if &dep_path == module_path {
                    continue;
                }
                let Some(dep_fingerprint) = direct_fingerprints.get(&dep_path) else {
                    continue;
                };
                write_fingerprint_part(&mut h, b"dep");
                write_fingerprint_part(&mut h, dep_path.as_str().as_bytes());
                write_fingerprint_part(&mut h, dep_fingerprint.as_str().as_bytes());
            }

            (module_path.clone(), finish_surface_fingerprint(h))
        })
        .collect()
}

/// Computes one root's exact dependency closure without retaining a partial
/// result while that root is in progress. One traversal costs
/// `O((V + E) * log V)` time and `O(V)` scratch. Composing fingerprints for
/// all `V` modules therefore costs `O(V * (V + E) * log V)` time. The
/// per-root traversal keeps cyclic graphs traversal-order independent without
/// introducing an unmeasured SCC index or another persistent graph form.
fn transitive_same_package_deps(
    module_path: &PackageModuleKey,
    deps_by_module: &BTreeMap<PackageModuleKey, Vec<PackageModuleKey>>,
) -> BTreeSet<PackageModuleKey> {
    #[cfg(test)]
    typed_cache_work_counters::record_reachability_root();

    let mut visited = BTreeSet::from([module_path.clone()]);
    let mut pending = vec![module_path.clone()];
    while let Some(current) = pending.pop() {
        if let Some(deps) = deps_by_module.get(&current) {
            for dep in deps {
                if visited.insert(dep.clone()) {
                    pending.push(dep.clone());
                }
            }
        }
    }
    visited.remove(module_path);
    visited
}

fn write_fingerprint_part(h: &mut blake3::Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

fn finish_surface_fingerprint(h: blake3::Hasher) -> SurfaceFingerprint {
    SurfaceFingerprint::new(h.finalize().to_hex().to_string())
}

#[cfg(test)]
mod typed_cache_dependency_fingerprint_tests {
    use super::*;

    fn module_key(name: &str) -> PackageModuleKey {
        PackageModuleKey::new(name.to_owned())
    }

    fn fingerprint(value: &str) -> SurfaceFingerprint {
        SurfaceFingerprint::new(value.to_owned())
    }

    #[cfg(feature = "surface")]
    fn surface_fingerprint(source: &str) -> SurfaceFingerprint {
        let module = crate::pass::parser::parse(source).expect("module source parses");
        public_surface_fingerprint(&module)
    }

    #[cfg(feature = "surface")]
    fn cache_policy_fixture(
        root: &Path,
        package_cache_enabled: bool,
    ) -> (
        package_collection::ParsedPackageCollection,
        PackageKey,
        PackageModuleKey,
        PathBuf,
    ) {
        let package_path = root.join("pkg.pkg.kio");
        let module_path = root.join("main.kio");
        let cache = if package_cache_enabled {
            "cache \"out/cache/\";"
        } else {
            "cache ();"
        };
        let package_source = format!("package pkg; build {{ {cache} }} bridge {{ main; }}");
        let module_source = "module main; pub fn run() -> . { () }";
        let package_file = crate::pass::parser::parse_package_file(&package_source, Some("pkg"))
            .expect("package source parses");
        let module = crate::pass::parser::parse(module_source).expect("module source parses");
        let module_key = package_storage_module_path(&module, Some(&PackageName::new("pkg")));
        let key = PackageKey {
            canonical_dir: root.to_path_buf(),
            package_name: "pkg".to_owned(),
        };
        let package = ParsedPackage {
            root_dir: root.to_path_buf(),
            modules: vec![(module_path.clone(), module)],
            lazy_modules: BTreeMap::new(),
            package_file: Some(PackageFileEntry {
                file_path: package_path.clone(),
                package_name: "pkg".to_owned(),
                package_file,
            }),
            dep_files: Vec::new(),
            sources: BTreeMap::from([
                (package_path, package_source),
                (module_path.clone(), module_source.to_owned()),
            ]),
        };
        (
            package_collection::ParsedPackageCollection {
                root: key.clone(),
                packages: BTreeMap::from([(key.clone(), package)]),
            },
            key,
            module_key,
            module_path,
        )
    }

    #[cfg(feature = "surface")]
    #[test]
    fn globally_disabled_typed_cache_does_no_dependency_or_key_work() {
        let root = tempfile::tempdir().expect("temporary package root");
        let (workspace, package_key, module_key, module_path) =
            cache_policy_fixture(root.path(), true);
        typed_cache_work_counters::reset();

        let state = TypedCacheState::compute_with_policy(&workspace, "test", false);
        let parsed_pkg = workspace
            .packages
            .get(&package_key)
            .expect("fixture package");
        let cache = state.cache_for(&package_key);
        assert!(!cache.is_enabled());
        assert!(
            state
                .key_for_active_cache(&cache, &package_key, parsed_pkg, &module_key, &module_path,)
                .is_none()
        );
        assert!(state.module_deps.is_empty());
        assert_eq!(
            typed_cache_work_counters::snapshot(),
            typed_cache_work_counters::Counts::default()
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn package_disabled_typed_cache_does_no_dependency_or_key_work() {
        let root = tempfile::tempdir().expect("temporary package root");
        let (workspace, package_key, module_key, module_path) =
            cache_policy_fixture(root.path(), false);
        typed_cache_work_counters::reset();

        let state = TypedCacheState::compute_with_policy(&workspace, "test", true);
        let parsed_pkg = workspace
            .packages
            .get(&package_key)
            .expect("fixture package");
        let cache = state.cache_for(&package_key);
        assert!(!cache.is_enabled());
        assert!(
            state
                .key_for_active_cache(&cache, &package_key, parsed_pkg, &module_key, &module_path,)
                .is_none()
        );
        assert!(state.module_deps.is_empty());
        assert_eq!(
            typed_cache_work_counters::snapshot(),
            typed_cache_work_counters::Counts::default()
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn enabled_typed_cache_prepares_dependencies_and_key() {
        let root = tempfile::tempdir().expect("temporary package root");
        let (workspace, package_key, module_key, module_path) =
            cache_policy_fixture(root.path(), true);
        typed_cache_work_counters::reset();

        let state = TypedCacheState::compute_with_policy(&workspace, "test", true);
        let parsed_pkg = workspace
            .packages
            .get(&package_key)
            .expect("fixture package");
        let cache = state.cache_for(&package_key);
        assert!(cache.is_enabled());
        assert!(
            state
                .key_for_active_cache(&cache, &package_key, parsed_pkg, &module_key, &module_path,)
                .is_some()
        );
        let counts = typed_cache_work_counters::snapshot();
        assert!(counts.fingerprints > 0);
        assert!(counts.reachability_roots > 0);
        assert_eq!(counts.keys, 1);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn globally_disabled_package_check_cache_skips_source_hash_work() {
        let root = tempfile::tempdir().expect("temporary package root");
        let (workspace, package_key, _, _) = cache_policy_fixture(root.path(), true);
        package_check_cache_work_counters::reset();

        let state = PackageCheckCacheState::compute_with_policy(&workspace, "test", false);

        assert!(state.cache_root(&package_key).is_none());
        assert!(state.source_hashes.is_empty());
        assert_eq!(package_check_cache_work_counters::snapshot(), 0);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn package_disabled_package_check_cache_skips_source_hash_work() {
        let root = tempfile::tempdir().expect("temporary package root");
        let (workspace, package_key, _, _) = cache_policy_fixture(root.path(), false);
        package_check_cache_work_counters::reset();

        let state = PackageCheckCacheState::compute_with_policy(&workspace, "test", true);

        assert!(state.cache_root(&package_key).is_none());
        assert!(state.source_hashes.is_empty());
        assert_eq!(package_check_cache_work_counters::snapshot(), 0);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn enabled_package_check_cache_hashes_active_package_sources() {
        let root = tempfile::tempdir().expect("temporary package root");
        let (workspace, package_key, _, _) = cache_policy_fixture(root.path(), true);
        package_check_cache_work_counters::reset();

        let state = PackageCheckCacheState::compute_with_policy(&workspace, "test", true);

        assert!(state.cache_root(&package_key).is_some());
        assert!(state.source_hashes.contains_key(&package_key));
        assert_eq!(package_check_cache_work_counters::snapshot(), 1);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn forwarding_surface_fingerprint_tracks_target_and_visibility() {
        let baseline = surface_fingerprint("module provider; pub type {field} = {origin.first};");
        assert_ne!(
            baseline,
            surface_fingerprint("module provider; pub type {field} = {origin.second};")
        );
        assert_ne!(
            baseline,
            surface_fingerprint("module provider; pub(provider) type {field} = {origin.first};")
        );
        assert_eq!(
            baseline,
            surface_fingerprint(
                "module provider;\n/// A label.\npub type {field} = {origin.first};"
            )
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn ordinary_function_bodies_do_not_change_the_public_surface_fingerprint() {
        let baseline = surface_fingerprint(
            "module provider; \
             fn helper() -> . { () } \
             pub fn run() -> . { helper() }",
        );
        let body_edit = surface_fingerprint(
            "module provider; \
             fn helper() -> . { let ignored = (); ignored } \
             pub fn run() -> . { let ignored = (); helper() }",
        );
        let signature_edit = surface_fingerprint(
            "module provider; \
             fn helper() -> . { () } \
             pub fn run(value: .) -> . { value }",
        );

        assert_eq!(baseline, body_edit);
        assert_ne!(baseline, signature_edit);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn fold_public_surface_fingerprint_tracks_shape_and_callables() {
        let baseline = surface_fingerprint(
            "module provider; \
             pub varop [% %] { foldr add zero; };",
        );
        let changed_shape = surface_fingerprint(
            "module provider; \
             pub varop [! !] { foldr add zero; };",
        );
        let changed_callable = surface_fingerprint(
            "module provider; \
             pub varop [% %] { foldr sub zero; };",
        );

        assert_ne!(baseline, changed_shape);
        assert_ne!(baseline, changed_callable);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn callable_segment_spans_do_not_enter_public_surface_fingerprints() {
        let compact = surface_fingerprint(
            "module provider; \
             pub op _ + _ { impl helpers.add; }; \
             pub varop [* *] { \
               foldr helpers.push helpers.empty; finalize helpers.finish; \
             };",
        );
        let shifted = surface_fingerprint(
            "module provider;\n\n\
             pub op _ + _ {\n  impl helpers.add;\n};\n\n\
             pub varop [* *] {\n\
               foldr helpers.push\n\
                 helpers.empty;\n\
               finalize helpers.finish;\n\
             };",
        );

        assert_eq!(compact, shifted);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn fold_public_surface_fingerprint_tracks_mode() {
        let fold_fingerprint_and_spans = |source: &str| {
            let module = crate::pass::parser::parse(source).expect("module source parses");
            let crate::ast::Item::VariadicOperator(fold, _) = &module.items[0] else {
                panic!("expected variadic operator declaration");
            };
            (
                public_surface_fingerprint(&module),
                (
                    fold.meta.span,
                    fold.spec.initializer.span(),
                    fold.spec.step.span(),
                ),
            )
        };

        // Padding the shorter mode keywords keeps all declaration and callable
        // spans equal. Fingerprint differences therefore isolate the mode.
        let (left, left_spans) =
            fold_fingerprint_and_spans("module provider; pub varop [* *] { foldl  push seed; };");
        let (right, right_spans) =
            fold_fingerprint_and_spans("module provider; pub varop [* *] { foldr  push seed; };");
        let (right_one, right_one_spans) =
            fold_fingerprint_and_spans("module provider; pub varop [* *] { foldr1 push seed; };");
        assert_eq!(left_spans, right_one_spans);
        assert_eq!(right_spans, right_one_spans);

        assert_ne!(left, right);
        assert_ne!(left, right_one);
        assert_ne!(right, right_one);

        let importing_consumer_fingerprint = |provider_fingerprint: SurfaceFingerprint| {
            let consumer = module_key("consumer");
            let provider = module_key("provider");
            let mut direct = BTreeMap::new();
            direct.insert(consumer.clone(), fingerprint("unchanged-consumer"));
            direct.insert(provider.clone(), provider_fingerprint);
            let mut deps = BTreeMap::new();
            deps.insert(consumer.clone(), vec![provider.clone()]);
            deps.insert(provider, Vec::new());
            compose_transitive_same_package_fingerprints(&direct, &deps, "typed-module")
                .get(&consumer)
                .expect("consumer fingerprint")
                .clone()
        };

        let left_consumer = importing_consumer_fingerprint(left);
        let right_consumer = importing_consumer_fingerprint(right);
        let right_one_consumer = importing_consumer_fingerprint(right_one);
        assert_ne!(left_consumer, right_consumer);
        assert_ne!(left_consumer, right_one_consumer);
        assert_ne!(right_consumer, right_one_consumer);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn same_package_dependencies_come_only_from_import_clauses() {
        let module = crate::pass::parser::parse(
            "module owner; \
             import ops/core as ops; \
             import bases as bases; \
             import steps/push as steps; \
             import finishes as finishes; \
             import unused as unused; \
             op _ + _ { impl ops.apply; }; \
             varop [* *] { \
               foldr steps.apply bases.empty; \
               finalize finishes.done; \
             };",
        )
        .expect("fixed and variadic operator declarations parse");
        let known_modules = ["bases", "finishes", "ops/core", "steps/push", "unused"]
            .into_iter()
            .map(module_key)
            .collect();

        let actual = same_package_import_targets(&module, None, &known_modules);
        let expected = ["bases", "finishes", "ops/core", "steps/push", "unused"]
            .into_iter()
            .map(module_key)
            .collect::<Vec<_>>();

        assert_eq!(actual, expected);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn elaborator_module_retains_every_qualified_import_as_an_impl_dependency() {
        let module = crate::pass::parser::parse(
            "module owner; \
             import helpers/used as used; \
             import helpers/unused as unused; \
             elab demo : . -> . { impl used.build; };",
        )
        .expect("qualified implementation target parses");
        let known_modules = ["helpers/used", "helpers/unused"]
            .into_iter()
            .map(module_key)
            .collect();

        let actual = same_package_elaborator_import_targets(&module, None, &known_modules);

        assert_eq!(
            actual,
            vec![module_key("helpers/unused"), module_key("helpers/used")]
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn module_without_elaborators_does_not_retain_qualified_impl_dependencies() {
        let module = crate::pass::parser::parse(
            "module owner; \
             import helpers/selected(helper); \
             import helpers/qualified as qualified; \
             pure fn local(value: .) -> . { helper(value) }",
        )
        .expect("ordinary helper module parses");
        let known_modules = ["helpers/selected", "helpers/qualified"]
            .into_iter()
            .map(module_key)
            .collect();

        let actual = same_package_elaborator_import_targets(&module, None, &known_modules);

        assert_eq!(actual, vec![module_key("helpers/selected")]);
    }

    #[test]
    fn transitive_composition_tracks_reachable_dependency_changes() {
        let a = module_key("a");
        let b = module_key("b");
        let c = module_key("c");
        let d = module_key("d");

        let mut direct = BTreeMap::new();
        direct.insert(a.clone(), fingerprint("a1"));
        direct.insert(b.clone(), fingerprint("b1"));
        direct.insert(c.clone(), fingerprint("c1"));
        direct.insert(d.clone(), fingerprint("d1"));

        let mut deps = BTreeMap::new();
        deps.insert(a.clone(), vec![b.clone()]);
        deps.insert(b.clone(), vec![c.clone()]);
        deps.insert(c.clone(), Vec::new());
        deps.insert(d.clone(), Vec::new());

        let baseline = compose_transitive_same_package_fingerprints(&direct, &deps, "test-context");

        let mut changed_transitive = direct.clone();
        changed_transitive.insert(c.clone(), fingerprint("c2"));
        let changed_transitive = compose_transitive_same_package_fingerprints(
            &changed_transitive,
            &deps,
            "test-context",
        );

        assert_ne!(baseline.get(&a), changed_transitive.get(&a));
        assert_ne!(baseline.get(&b), changed_transitive.get(&b));
        assert_ne!(baseline.get(&c), changed_transitive.get(&c));
        assert_eq!(baseline.get(&d), changed_transitive.get(&d));

        let mut changed_unrelated = direct.clone();
        changed_unrelated.insert(d.clone(), fingerprint("d2"));
        let changed_unrelated =
            compose_transitive_same_package_fingerprints(&changed_unrelated, &deps, "test-context");

        assert_eq!(baseline.get(&a), changed_unrelated.get(&a));
        assert_eq!(baseline.get(&b), changed_unrelated.get(&b));
        assert_eq!(baseline.get(&c), changed_unrelated.get(&c));
        assert_ne!(baseline.get(&d), changed_unrelated.get(&d));
    }

    #[test]
    fn every_root_of_a_three_cycle_has_the_complete_reachable_set() {
        let a = module_key("a");
        let b = module_key("b");
        let c = module_key("c");

        let mut deps = BTreeMap::new();
        deps.insert(a.clone(), vec![b.clone()]);
        deps.insert(b.clone(), vec![c.clone()]);
        deps.insert(c.clone(), vec![a.clone()]);

        for (root, expected) in [
            (a.clone(), BTreeSet::from([b.clone(), c.clone()])),
            (b.clone(), BTreeSet::from([a.clone(), c.clone()])),
            (c.clone(), BTreeSet::from([a.clone(), b.clone()])),
        ] {
            assert_eq!(transitive_same_package_deps(&root, &deps), expected);
        }

        let direct = BTreeMap::from([
            (a.clone(), fingerprint("a1")),
            (b.clone(), fingerprint("b1")),
            (c.clone(), fingerprint("c1")),
        ]);
        let baseline = compose_transitive_same_package_fingerprints(&direct, &deps, "cycle");
        for changed_module in [&a, &b, &c] {
            let mut changed = direct.clone();
            changed.insert(changed_module.clone(), fingerprint("changed"));
            let changed = compose_transitive_same_package_fingerprints(&changed, &deps, "cycle");
            for root in [&a, &b, &c] {
                assert_ne!(
                    baseline.get(root),
                    changed.get(root),
                    "{root:?} must include {changed_module:?} in its cycle closure"
                );
            }
        }
    }

    #[cfg(feature = "surface")]
    #[test]
    fn parsed_mixed_cycle_moves_downstream_fingerprints_after_member_edit() {
        fn package(b_source: &str) -> ParsedPackage {
            let sources = [
                ("a.kio", "module a; import b(run); pub fn run() -> . { () }"),
                ("b.kio", b_source),
                (
                    "c.kio",
                    "module c; import a(run); pub fn step(left: ., right: .) -> . { left }",
                ),
                (
                    "d.kio",
                    "module d; import c(step); pub fn run() -> . { () }",
                ),
                ("e.kio", "module e; pub fn run() -> . { () }"),
            ];
            let mut modules = Vec::new();
            let mut source_map = BTreeMap::new();
            for (path, source) in sources {
                let path = PathBuf::from(path);
                modules.push((
                    path.clone(),
                    crate::pass::parser::parse(source).expect("module source parses"),
                ));
                source_map.insert(path, source.to_owned());
            }
            ParsedPackage {
                root_dir: PathBuf::new(),
                modules,
                lazy_modules: BTreeMap::new(),
                package_file: None,
                dep_files: Vec::new(),
                sources: source_map,
            }
        }

        let baseline = same_package_public_surfaces(&package(
            "module b; import c as c; pub op _ + _ { impl c.step; }; pub fn run() -> . { () }",
        ));
        let changed = same_package_public_surfaces(&package(
            "module b; import c as c; pub op _ + _ { impl c.step; }; pub fn run(value: .) -> . { value }",
        ));

        for module in ["a", "b", "c", "d"] {
            let module = module_key(module);
            assert_ne!(
                baseline.get(&module),
                changed.get(&module),
                "{module:?} must include the edited b surface through the a -> b -> c -> a cycle"
            );
        }
        let unrelated = module_key("e");
        assert_eq!(baseline.get(&unrelated), changed.get(&unrelated));
    }

    #[cfg(feature = "surface")]
    #[test]
    fn elaborator_impl_fingerprint_tracks_imported_helper_source() {
        fn parsed_module(path: &str, source: &str) -> (PathBuf, Module<Surface>) {
            let module_file =
                crate::pass::parser::parse_module_file(source).expect("module source parses");
            (PathBuf::from(path), module_file.module)
        }

        fn package(elaborator_source: &str, helper_source: &str) -> ParsedPackage {
            let elaborator_path = PathBuf::from("elaborators.kio");
            let helper_path = PathBuf::from("elaborator_util.kio");
            let modules = vec![
                parsed_module(
                    elaborator_path.to_str().expect("utf8 path"),
                    elaborator_source,
                ),
                parsed_module(helper_path.to_str().expect("utf8 path"), helper_source),
            ];
            ParsedPackage {
                root_dir: PathBuf::new(),
                modules,
                lazy_modules: BTreeMap::new(),
                package_file: None,
                dep_files: Vec::new(),
                sources: BTreeMap::from([
                    (elaborator_path, elaborator_source.to_owned()),
                    (helper_path, helper_source.to_owned()),
                ]),
            }
        }

        let elaborator_source = "module elaborators;\n\
            import elaborator_util(helper);\n\
            pub elab poc : [A] A -> [B] B { impl helper; };\n";
        let helper_source = "module elaborator_util;\n\
            pub pure fn helper[A](value: A)[B] -> B { value }\n";
        let baseline =
            same_package_elaborator_impl_surfaces(&package(elaborator_source, helper_source));
        let changed_helper = same_package_elaborator_impl_surfaces(&package(
            elaborator_source,
            "module elaborator_util;\n\
             pub pure fn helper[A](value: A)[B] -> B { helper(A, value, B) }\n",
        ));
        let changed_declaration = same_package_elaborator_impl_surfaces(&package(
            "module elaborators;\n\
             import elaborator_util(helper);\n\
             pub elab poc : [A] A -> [B] B { captures helper; impl helper; };\n",
            helper_source,
        ));

        let elaborators = module_key("elaborators");
        assert_ne!(baseline.get(&elaborators), changed_helper.get(&elaborators));
        assert_ne!(
            baseline.get(&elaborators),
            changed_declaration.get(&elaborators)
        );

        let qualified_elaborator_source = "module elaborators;\n\
            import elaborator_util as util;\n\
            pub elab poc : [A] A -> [B] B { impl util.helper; };\n";
        let qualified_baseline = same_package_elaborator_impl_surfaces(&package(
            qualified_elaborator_source,
            helper_source,
        ));
        let qualified_changed_helper = same_package_elaborator_impl_surfaces(&package(
            qualified_elaborator_source,
            "module elaborator_util;\n\
             pub pure fn helper[A](value: A)[B] -> B { helper(A, value, B) }\n",
        ));
        assert_ne!(
            qualified_baseline.get(&elaborators),
            qualified_changed_helper.get(&elaborators),
            "a qualified elaborator implementation target must retain its helper body dependency"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn elaborator_schedule_change_keeps_the_lazy_surface_but_invalidates_the_impl() {
        fn package(source: &str) -> ParsedPackage {
            let path = PathBuf::from("elaborators.kio");
            let module_file = crate::pass::parser::parse_module_file_lazy(source)
                .expect("lazy elaborator source parses");
            ParsedPackage {
                root_dir: PathBuf::new(),
                modules: vec![(path.clone(), module_file.module)],
                lazy_modules: BTreeMap::new(),
                package_file: None,
                dep_files: Vec::new(),
                sources: BTreeMap::from([(path, source.to_owned())]),
            }
        }

        let late = package(
            "module elaborators;\n\
             pub elab poc : [A] A -> [B] B { impl helper; };\n",
        );
        let fills = package(
            "module elaborators;\n\
             pub elab poc : [A] A -> [B] B { impl(fills) helper; };\n",
        );
        let retargeted = package(
            "module elaborators;\n\
             pub elab poc : [A] A -> [B] B { impl other_helper; };\n",
        );
        let module = module_key("elaborators");
        let late_public = same_package_public_surfaces(&late);
        let fills_public = same_package_public_surfaces(&fills);
        let retargeted_public = same_package_public_surfaces(&retargeted);
        assert_eq!(
            late_public.get(&module),
            fills_public.get(&module),
            "the lazy public header must not classify the deferred implementation"
        );
        assert_eq!(
            late_public.get(&module),
            retargeted_public.get(&module),
            "the implementation target must not enter the public header fingerprint"
        );

        let late_impl = same_package_elaborator_impl_surfaces(&late);
        let fills_impl = same_package_elaborator_impl_surfaces(&fills);
        let retargeted_impl = same_package_elaborator_impl_surfaces(&retargeted);
        assert_ne!(
            late_impl.get(&module),
            fills_impl.get(&module),
            "the complete source fingerprint must invalidate consumers when the schedule changes"
        );
        assert_ne!(
            late_impl.get(&module),
            retargeted_impl.get(&module),
            "the complete source fingerprint must invalidate consumers when the target changes"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn trailing_descriptors_participate_in_lazy_public_surface_fingerprints() {
        fn surface(entries: &str) -> BTreeMap<PackageModuleKey, SurfaceFingerprint> {
            let source = format!(
                "module elaborators; pub elab choose : [R] (. -> R) -> (. -> R) -> R {{ {entries} impl helper; }}"
            );
            let path = PathBuf::from("elaborators.kio");
            let parsed = crate::pass::parser::parse_module_file_lazy(&source).unwrap();
            same_package_public_surfaces(&ParsedPackage {
                root_dir: PathBuf::new(),
                modules: vec![(path.clone(), parsed.module)],
                lazy_modules: BTreeMap::new(),
                package_file: None,
                dep_files: Vec::new(),
                sources: BTreeMap::from([(path, source)]),
            })
        }
        let baseline = surface("trailing thunk; trailing product else;");
        for changed in [
            "",
            "trailing product; trailing product else;",
            "trailing thunk; trailing product otherwise;",
            "trailing product else; trailing thunk;",
        ] {
            assert_ne!(baseline, surface(changed), "{changed}");
        }
        assert_eq!(
            baseline,
            surface("trailing thunk; // descriptor comment\ntrailing product else;")
        );
    }
}

fn collect_same_package_import_fingerprints(
    module: &Module<Surface>,
    package_name: Option<&PackageName>,
    public_surfaces: &BTreeMap<PackageModuleKey, SurfaceFingerprint>,
    elaborator_impl_surfaces: &BTreeMap<PackageModuleKey, SurfaceFingerprint>,
    public_known_modules: &BTreeSet<PackageModuleKey>,
    elaborator_known_modules: &BTreeSet<PackageModuleKey>,
    deps: &mut Vec<TypedModuleDependency>,
) {
    for target_key in same_package_import_targets(module, package_name, public_known_modules) {
        if let Some(hash) = public_surfaces.get(&target_key) {
            deps.push(TypedModuleDependency::same_package_module(
                target_key,
                hash.clone(),
            ));
        }
    }
    for target_key in
        same_package_elaborator_import_targets(module, package_name, elaborator_known_modules)
    {
        if let Some(hash) = elaborator_impl_surfaces.get(&target_key) {
            deps.push(TypedModuleDependency::same_package_elaborator_module(
                target_key,
                hash.clone(),
            ));
        }
    }
}

fn same_package_import_targets(
    module: &Module<Surface>,
    package_name: Option<&PackageName>,
    known_modules: &BTreeSet<PackageModuleKey>,
) -> Vec<PackageModuleKey> {
    let mut targets = BTreeSet::new();
    for u in &module.imports {
        let target = match &u.kind {
            crate::ast::ImportKind::Selective { from, .. } => from,
            crate::ast::ImportKind::Qualified { path, .. } => path,
            crate::ast::ImportKind::Intrinsics | crate::ast::ImportKind::Comptime => continue,
        };
        let declared = declared_module_path(target);
        let target_key = package_storage_module_key(&declared, package_name);
        if known_modules.contains(&target_key) {
            targets.insert(target_key);
        }
    }
    targets.into_iter().collect()
}

fn same_package_dependency_closure(
    root: &PackageModuleKey,
    parsed_pkg: &ParsedPackage,
    package_name: Option<&PackageName>,
) -> BTreeSet<PackageModuleKey> {
    let mut known_modules = BTreeSet::new();
    let mut deps_by_module = BTreeMap::new();
    for (_, module) in &parsed_pkg.modules {
        let declared = declared_module_path(&module.path);
        let module_key = package_storage_module_key(&declared, package_name);
        known_modules.insert(module_key.clone());
    }
    for (_, module) in &parsed_pkg.modules {
        let declared = declared_module_path(&module.path);
        let module_key = package_storage_module_key(&declared, package_name);
        deps_by_module.insert(
            module_key,
            same_package_import_targets(module, package_name, &known_modules),
        );
    }
    let mut closure = transitive_same_package_deps(root, &deps_by_module);
    closure.insert(root.clone());
    closure
}

fn same_package_elaborator_import_targets(
    module: &Module<Surface>,
    package_name: Option<&PackageName>,
    known_modules: &BTreeSet<PackageModuleKey>,
) -> Vec<PackageModuleKey> {
    // Function bodies remain lazy here, so a selectively imported name may
    // be an elaborator called from a body we have not parsed yet. A local
    // elaborator implementation may also reach any qualified import through
    // its ordinary pure helper graph. Retain every qualified import when the
    // module declares an elaborator; narrowing that set requires a complete
    // body-level call-graph analysis, not just inspection of the impl path.
    let declares_elaborator = module
        .items
        .iter()
        .any(|item| matches!(item, crate::ast::Item::Elaborator(_, _)));
    let mut targets = BTreeSet::new();
    for u in &module.imports {
        let target = match &u.kind {
            crate::ast::ImportKind::Selective { from, .. } => from,
            crate::ast::ImportKind::Qualified { path, .. } if declares_elaborator => path,
            crate::ast::ImportKind::Qualified { .. }
            | crate::ast::ImportKind::Intrinsics
            | crate::ast::ImportKind::Comptime => continue,
        };
        let declared = declared_module_path(target);
        let target_key = package_storage_module_key(&declared, package_name);
        if known_modules.contains(&target_key) {
            targets.insert(target_key);
        }
    }
    targets.into_iter().collect()
}

fn public_surface_entries(module: &Module<Surface>) -> Vec<String> {
    let mut entries = Vec::new();
    for item in &module.items {
        match item {
            crate::ast::Item::FnDef(d) => entries.push(format!(
                "fn:{}:{}",
                d.name,
                crate::pretty::pretty_item_signature(item)
            )),
            crate::ast::Item::RecGroup(g, _) => {
                for d in &g.members {
                    entries.push(format!(
                        "fn:{}:{}",
                        d.name,
                        crate::pretty::pretty_item_signature(&crate::ast::Item::FnDef(d.clone()))
                    ));
                }
            }
            crate::ast::Item::TypeRecGroup(group) => entries.push(format!(
                "type_rec_group:{}:{}",
                group
                    .members
                    .iter()
                    .map(|member| match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => alias.name.as_str(),
                        crate::ast::TypeRecMember::Newtype(newtype) => newtype.name.as_str(),
                        crate::ast::TypeRecMember::Labels(labels, _) => {
                            labels.type_alias_name.as_deref().unwrap_or("")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(","),
                crate::pretty::pretty_item_signature(item)
            )),
            crate::ast::Item::TypeAlias(a) => entries.push(format!(
                "type:{}:{}",
                a.name,
                crate::pretty::pretty_item_signature(item)
            )),
            crate::ast::Item::LiteralAlias(l, _) => entries.push(format!(
                "literal:{}:{}",
                l.name,
                crate::pretty::pretty_item_signature(item)
            )),
            crate::ast::Item::Newtype(d) => entries.push(format!(
                "newtype:{}:{}",
                d.name,
                crate::pretty::pretty_item_signature(item)
            )),
            crate::ast::Item::Labels(d, _) => entries.push(format!(
                "labels:{}:{}",
                d.type_alias_name.as_deref().unwrap_or(""),
                crate::pretty::pretty_item_signature(item)
            )),
            crate::ast::Item::LabelForward(forward, _) => entries.push(format!(
                "label_forward:{}:{}",
                forward.name,
                crate::pretty::pretty_item_signature(item)
            )),
            crate::ast::Item::Op(op, _) if op.vis.is_pub() => entries.push(format!(
                "op:{}:{}",
                crate::ast::OperatorDispatchKey::from_body(&op.body).render(),
                crate::pretty::pretty_item_signature(item)
            )),
            crate::ast::Item::VariadicOperator(fold, _) if fold.vis.is_pub() => {
                entries.push(format!(
                    "fold:{}:{}",
                    crate::ast::OperatorDispatchKey::from_variadic(fold).render(),
                    crate::pretty::pretty_item_signature(item)
                ))
            }
            crate::ast::Item::Elaborator(s, _) if s.vis.is_pub() => entries.push(format!(
                "elaborator:{}:{}",
                s.name,
                crate::pretty::pretty_elaborator_public_signature(s)
            )),
            // Host items are always public and part of the contract
            // surface, so they contribute to the public fingerprint.
            crate::ast::Item::HostType(h) => entries.push(format!(
                "host_type:{}:{}",
                h.name,
                crate::pretty::pretty_item_signature(item)
            )),
            crate::ast::Item::HostFn(h) => entries.push(format!(
                "host_fn:{}:{}",
                h.name,
                crate::pretty::pretty_item_signature(item)
            )),
            crate::ast::Item::Equiv(_, _)
            | crate::ast::Item::Op(_, _)
            | crate::ast::Item::VariadicOperator(_, _)
            | crate::ast::Item::Elaborator(_, _) => {}
        }
    }
    entries.sort();
    entries
}

impl PackageCheckCacheState {
    /// Compute source hashes for cache-enabled packages in the parsed
    /// workspace. `cache_tag` is the running pipeline's [`Pipeline::CACHE_TAG`].
    fn compute(
        parsed_ws: &package_collection::ParsedPackageCollection,
        cache_tag: &'static str,
    ) -> Self {
        Self::compute_with_policy(parsed_ws, cache_tag, crate::cache::policy::caches_enabled())
    }

    fn compute_with_policy(
        parsed_ws: &package_collection::ParsedPackageCollection,
        cache_tag: &'static str,
        caches_enabled: bool,
    ) -> Self {
        let cache_tag = PipelineTag::new(cache_tag);
        if !caches_enabled {
            return Self {
                source_hashes: BTreeMap::new(),
                cache_roots: crate::cache::roots::SemanticCacheRoots::empty(),
                cache_tag,
            };
        }
        let cache_roots = crate::cache::roots::SemanticCacheRoots::compute(parsed_ws);
        let mut source_hashes = BTreeMap::new();
        for (key, parsed_pkg) in &parsed_ws.packages {
            if cache_roots.root_for(key).is_none() {
                continue;
            }
            #[cfg(test)]
            package_check_cache_work_counters::record_source_hash();
            source_hashes.insert(
                key.clone(),
                crate::cache::package_check::source_hash(&parsed_pkg.sources),
            );
        }
        Self {
            source_hashes,
            cache_roots,
            cache_tag,
        }
    }

    fn cache_root(&self, key: &PackageKey) -> Option<&Path> {
        self.cache_roots.root_for(key)
    }

    fn cache_key(&self, key: &PackageKey) -> crate::cache::package_check::PackageCheckCacheKey {
        crate::cache::package_check::PackageCheckCacheKey::new(
            PackageName::from_package_key(key),
            self.cache_tag,
            self.source_hash(key),
        )
    }

    fn cache(&self, key: &PackageKey) -> crate::cache::package_check::PackageCheckCache {
        let Some(root) = self.cache_root(key) else {
            return crate::cache::package_check::PackageCheckCache::disabled();
        };
        crate::cache::package_check::PackageCheckCache::open(root.to_path_buf())
            .unwrap_or_else(|_| crate::cache::package_check::PackageCheckCache::disabled())
    }

    fn load_entry<P>(
        &self,
        key: &PackageKey,
    ) -> Option<crate::cache::package_check::PackageCheckEntry<P>>
    where
        P: crate::ast::Phase + serde::Serialize + serde::de::DeserializeOwned,
    {
        self.cache_root(key)?;
        let cache = self.cache(key);
        if !cache.is_enabled() {
            return None;
        }
        cache.lookup(&self.cache_key(key)).ok().flatten()
    }

    fn entry_for<P>(
        &self,
        _key: &PackageKey,
        package_file: Option<crate::pass::resolve::PackageFileEntry<P>>,
    ) -> crate::cache::package_check::PackageCheckEntry<P>
    where
        P: crate::ast::Phase + serde::Serialize,
    {
        crate::cache::package_check::PackageCheckEntry { package_file }
    }

    /// A cache-enabled package's current source hash. Every package with an
    /// active cache root was hashed by `compute`, so the lookup never misses.
    fn source_hash(&self, key: &PackageKey) -> SourceHash {
        self.source_hashes
            .get(key)
            .expect("compute() hashed every package with an active cache root")
            .clone()
    }
}

fn declared_module_path(path: &crate::ast::ModulePath) -> DeclaredModulePath {
    DeclaredModulePath::new(module_path_key(path))
}

fn module_path_key(path: &crate::ast::ModulePath) -> String {
    path.segments
        .iter()
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lower_and_resolve<P: Pipeline>(
    parsed: &ParsedPackage,
) -> Result<PackageEntry<P::LoweredPhase>, LocatedError>
where
    P::LoweredPhase: ResolvePhase,
    P::LoweredPhase: crate::ast::Phase<ExprResolved = ()> + Clone,
{
    let modules = parsed.modules.clone();
    let package_file_ast = parsed.package_file.as_ref().map(|e| e.package_file.clone());
    let package_name = parsed
        .package_file
        .as_ref()
        .map(|e| e.package_name.as_str());
    let (lowered_modules, lowered_package_file) =
        P::lower_package_named(modules, package_file_ast, package_name)?;

    resolve_lowered_package(parsed, lowered_modules, lowered_package_file)
}

#[cfg(all(feature = "surface", feature = "lsp"))]
type FullLspResolvedPackage = (
    PackageEntry<crate::ast::Lowered>,
    crate::pass::full::LspSourceDeclarations,
);

#[cfg(all(feature = "surface", feature = "lsp"))]
fn lower_and_resolve_full_lsp(
    parsed: &ParsedPackage,
    cancel_token: &crate::lsp::cancel::CancellationToken,
) -> Result<Option<FullLspResolvedPackage>, LocatedError> {
    let modules = parsed.modules.clone();
    let package_file_ast = parsed.package_file.as_ref().map(|e| e.package_file.clone());
    let package_name = parsed
        .package_file
        .as_ref()
        .map(|e| e.package_name.as_str());
    // The caller has already run source-package validation so it can preserve
    // parse-error precedence. Retain the exact lowering scope's callable
    // projection instead of rebuilding and revalidating that scope for LSP.
    let Some((lowered_modules, lowered_package_file, callable_declarations)) =
        FullPipeline::lower_projected_package_named_for_lsp(
            modules,
            package_file_ast,
            package_name,
            || cancel_token.is_cancelled(),
        )?
    else {
        return Ok(None);
    };
    if cancel_token.is_cancelled() {
        return Ok(None);
    }
    let package = resolve_lowered_package(parsed, lowered_modules, lowered_package_file)?;
    Ok(Some((package, callable_declarations)))
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn resolve_lowered_package<P>(
    parsed: &ParsedPackage,
    lowered_modules: Vec<(PathBuf, Module<P>)>,
    lowered_package_file: Option<crate::ast::PackageFile<P>>,
) -> Result<PackageEntry<P>, LocatedError>
where
    P: ResolvePhase + crate::ast::Phase<ExprResolved = ()> + Clone,
{
    // A package-less module tree (no `*.pkg.kio`) is admitted here: the
    // module tree is analysed for its own sake — typechecked and
    // resolved against its `import`-closure — without a package contract.
    // `Package::build` already accepts `None` for the package file (see
    // below). This is the analysis surface `kio repl` and `kio lsp`
    // operate on; `kio check` / `kio build` still require a package
    // file via their own marker check (see `build_package_summary`).
    // The package contract (`env` / `bridge` / `export`) is what a
    // *package file* declares; module-tree analysis needs none of it.

    let package_file_entry = match (&parsed.package_file, lowered_package_file) {
        (Some(prev), Some(package_file)) => Some(PackageFileEntry {
            file_path: prev.file_path.clone(),
            package_name: prev.package_name.clone(),
            package_file,
        }),
        _ => None,
    };
    let package = Package::<P>::build(&parsed.root_dir, lowered_modules, package_file_entry)?;
    package.resolve_imports()?;
    package.check_no_value_cycles()?;
    package.check_in_body_resolution()?;
    Ok(PackageEntry {
        root_dir: parsed.root_dir.clone(),
        package,
    })
}

/// Project a dependency package's **contract surface** by lowering only
/// its *signatures*: every function body (and `rec`-group member body) is
/// blanked to `()` before lowering, and the surface-only auxiliary
/// declarations that contribute no contract entry are dropped. The digest
/// is therefore a pure function of the dependency's declared signature
/// surface, computed **without a function-body typecheck** — so it is
/// identical on `kio` (which lowers via [`FullPipeline`]) and `kio-prime`
/// (via [`PrimePipeline`]), even when the dependency's bodies use surface
/// forms the surface-less `kio-prime` lowering would otherwise reject
/// (see `specs/versioning.md` § Git-dependency contract gate and
/// [`strip_module_to_signature_surface`]).
#[cfg(feature = "cli")]
pub(crate) fn contract_snapshot_via_lower<P: Pipeline>(
    parsed: &ParsedPackage,
) -> Result<crate::sig::ContractSnapshot, LocatedError>
where
    P::LoweredPhase: ResolvePhase
        + crate::pass::resolve::ExportContractPhase
        + crate::pass::typecheck_core::TyperPhase,
    P::LoweredPhase: crate::ast::Phase<ExprResolved = (), FnPurity = crate::ast::Purity> + Clone,
{
    let mut modules = parsed.modules.clone();
    for (_, module) in &mut modules {
        normalize_projected_parameter_patterns(module);
    }
    let rec_header_errors = projected_rec_header_errors(&modules);
    if let Some(error) = rec_header_errors
        .iter()
        .find(|error| matches!(error.error, Error::Parse(_)))
    {
        return Err(LocatedError {
            file_path: error.file_path.clone(),
            error: error.error.clone(),
        });
    }
    crate::pass::surface_registry::validate_package(&parsed.root_dir, &parsed.modules)
        .map_err(|error| parsed.prefer_deferred_body_error(error))?;
    crate::pass::surface_registry::filter_unsealed_contract_imports(&mut modules);
    for (file_path, module) in modules.iter_mut() {
        strip_module_to_signature_surface(module).map_err(|error| LocatedError {
            file_path: file_path.clone(),
            error,
        })?;
    }
    let package_file_ast = parsed.package_file.as_ref().map(|e| e.package_file.clone());
    let package_name = parsed
        .package_file
        .as_ref()
        .map(|e| e.package_name.as_str());
    let (lowered_modules, lowered_package_file) =
        P::lower_projected_package_named(modules, package_file_ast, package_name)?;
    let package_file_entry = match (&parsed.package_file, lowered_package_file) {
        (Some(prev), Some(package_file)) => Some(PackageFileEntry {
            file_path: prev.file_path.clone(),
            package_name: prev.package_name.clone(),
            package_file,
        }),
        _ => None,
    };
    let (package, mut name_errors) = Package::<P::LoweredPhase>::build_deferring_contract_checks(
        &parsed.root_dir,
        lowered_modules,
        package_file_entry,
    )?;
    package.resolve_imports()?;
    package.check_no_value_cycles()?;
    if let Err(error) = package.check_binding_origins() {
        name_errors.push(error);
    }
    name_errors.extend(
        rec_header_errors
            .iter()
            .filter(|error| matches!(error.error, Error::NameRes(_)))
            .map(|error| LocatedError {
                file_path: error.file_path.clone(),
                error: error.error.clone(),
            }),
    );
    if let Some(error) = earliest_projected_error(parsed, name_errors) {
        return Err(error);
    }
    package.check_in_body_resolution()?;
    let mut type_errors: Vec<LocatedError> = rec_header_errors
        .iter()
        .filter(|error| matches!(error.error, Error::Type(_)))
        .map(|error| LocatedError {
            file_path: error.file_path.clone(),
            error: error.error.clone(),
        })
        .collect();
    if let Err(error) = crate::pass::typecheck_core::check_package_signatures(&package) {
        if matches!(
            &error.error,
            Error::Parse(_) | Error::Import(_) | Error::NameRes(_)
        ) {
            return Err(error);
        }
        if matches!(&error.error, Error::Type(_)) {
            type_errors.push(error);
        } else if type_errors.is_empty() {
            return Err(error);
        }
    }
    if let Some(error) = earliest_projected_error(parsed, type_errors) {
        return Err(error);
    }
    package.validate_bridge_contract()?;
    Ok(crate::sig::ContractSnapshot::from_package(&package))
}

#[cfg(feature = "cli")]
fn earliest_projected_error(
    parsed: &ParsedPackage,
    errors: Vec<LocatedError>,
) -> Option<LocatedError> {
    errors.into_iter().min_by_key(|error| {
        let module_index = parsed
            .modules
            .iter()
            .position(|(file_path, _)| file_path == &error.file_path)
            .unwrap_or(usize::MAX);
        (module_index, error.error.diagnostic().span.start)
    })
}

#[cfg(feature = "cli")]
fn projected_rec_header_errors(modules: &[(PathBuf, Module<Surface>)]) -> Vec<LocatedError> {
    let mut errors = Vec::new();
    for (file_path, module) in modules {
        for item in &module.items {
            let crate::ast::Item::RecGroup(group, _) = item else {
                continue;
            };
            for error in crate::pass::rec_headers::validate_all(group) {
                errors.push(LocatedError {
                    file_path: file_path.clone(),
                    error,
                });
            }
        }
    }
    errors
}

#[cfg(feature = "cli")]
fn normalize_projected_parameter_patterns(module: &mut Module<Surface>) {
    for item in &mut module.items {
        match item {
            crate::ast::Item::FnDef(def) => {
                normalize_projected_signature_patterns(&mut def.sig);
            }
            crate::ast::Item::RecGroup(group, _) => {
                for member in &mut group.members {
                    normalize_projected_signature_patterns(&mut member.sig);
                }
            }
            _ => {}
        }
    }
}

#[cfg(feature = "cli")]
fn normalize_projected_signature_patterns(signature: &mut crate::ast::Signature<Surface>) {
    for param in &mut signature.params {
        let crate::ast::SignatureParam::Value(param) = param else {
            continue;
        };
        if let Some(pattern) = param.pattern.take() {
            param.ty = Some(pattern.outer_type());
        }
    }
}

/// Reduce a parsed Surface module to the **signature surface** a
/// dependency's contract is projected from, so it can be lowered for a
/// contract digest without lowering or typechecking function bodies.
///
/// - Function definitions keep their signature; their body is blanked to
///   `()` (the contract reads only `sig` / `ret`). Surface parameter patterns
///   are replaced by the product type they declare, which is their complete
///   contribution to the signature.
/// - A `rec` group is surface-only *as an item*, but its members are
///   ordinary `fn` definitions whose signatures belong in the contract, so
///   each member is lifted out as a top-level `fn` (body blanked). The
///   recursion structure is irrelevant once bodies are gone.
/// - Host types, host fns, and type aliases carry the contract directly.
///   Public newtypes are reduced to the same four-state host surface recorded
///   in a signature file; private newtypes pass through for visibility checks.
/// - The surface-only auxiliary declarations (`equiv`, `op`, `varop`,
///   `literal`, `elab`) contribute no contract entry (see
///   `sig::surface::item_is_exported`), and `kio-prime`'s lowering rejects
///   them, so they are dropped: the result is the same contract on both
///   binaries.
/// - `labels` is the one surface form that would enter the contract only
///   *after* surface label-elaboration (as newtypes). Computing that
///   without the surface frontend is exactly what `kio-prime` cannot do, so
///   `labels` is dropped here too: an unsealed dependency's live contract
///   covers its explicitly-declared signatures, and a dependency that
///   exposes `labels` in its public interface must be **sealed** (ship a
///   `<pkg>.sig.kio`) to freeze them into its contract. See
///   `specs/versioning.md` § Git-dependency contract gate.
#[cfg(feature = "cli")]
fn strip_module_to_signature_surface(
    module: &mut crate::ast::Module<Surface>,
) -> Result<(), Error> {
    use crate::ast::{Expr, Item, Meta};
    let blank_body = || Expr::Unit {
        occurrence: Default::default(),
        meta: Meta::new(crate::span::Span::new(0, 0)),
    };
    let strip_fn = |d: &mut crate::ast::FnDef<Surface>| {
        normalize_projected_signature_patterns(&mut d.sig);
        d.body = blank_body();
    };
    let mut kept = Vec::with_capacity(module.items.len());
    for item in std::mem::take(&mut module.items) {
        match item {
            Item::FnDef(mut d) => {
                strip_fn(&mut d);
                kept.push(Item::FnDef(d));
            }
            Item::RecGroup(group, _) => {
                for mut member in group.members {
                    strip_fn(&mut member);
                    kept.push(Item::FnDef(member));
                }
            }
            Item::TypeRecGroup(group) => {
                let members = group
                    .members
                    .into_iter()
                    .filter_map(|member| match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => {
                            Some(crate::ast::TypeRecMember::TypeAlias(alias))
                        }
                        crate::ast::TypeRecMember::Newtype(newtype) => {
                            let newtype = if newtype.is_host_exported() {
                                crate::sig::project_newtype_declaration(newtype)
                            } else {
                                newtype
                            };
                            Some(crate::ast::TypeRecMember::Newtype(newtype))
                        }
                        crate::ast::TypeRecMember::Labels(_, _) => None,
                    })
                    .collect();
                kept.push(Item::TypeRecGroup(crate::ast::TypeRecGroup {
                    members,
                    doc: group.doc,
                    source_layout: group.source_layout,
                    rec_span: group.rec_span,
                    open_brace_span: group.open_brace_span,
                    close_brace_span: group.close_brace_span,
                    deferred_rec_labels_diagnostic: group.deferred_rec_labels_diagnostic,
                    meta: group.meta,
                }));
            }
            Item::TypeAlias(alias) => {
                kept.push(Item::TypeAlias(alias));
            }
            Item::Newtype(newtype) => {
                let newtype = if newtype.is_host_exported() {
                    crate::sig::project_newtype_declaration(newtype)
                } else {
                    newtype
                };
                kept.push(Item::Newtype(newtype));
            }
            keep @ (Item::HostType(_) | Item::HostFn(_)) => {
                kept.push(keep);
            }
            Item::Labels(..)
            | Item::LabelForward(..)
            | Item::Equiv(..)
            | Item::Elaborator(..)
            | Item::LiteralAlias(..)
            | Item::Op(..)
            | Item::VariadicOperator(..) => {}
        }
    }
    module.items = kept;

    let mut analyses =
        crate::pass::resolve::projected_surface_type_rec_analyses(module)?.into_iter();
    let mut partitioned = Vec::with_capacity(module.items.len());
    for item in std::mem::take(&mut module.items) {
        let Item::TypeRecGroup(group) = item else {
            partitioned.push(item);
            continue;
        };
        let (analyzed_span, analysis) = analyses
            .next()
            .expect("one projection analysis per recursive group");
        debug_assert_eq!(analyzed_span, group.meta.span);
        let analysis = analysis.ok_or_else(|| {
            Error::internal(
                group.meta.span,
                "a labels-free contract projection must have a representable recursive partition",
            )
        })?;
        partitioned.extend(crate::pass::resolve::emit_type_rec_partition(
            group, &analysis,
        ));
    }
    debug_assert!(analyses.next().is_none());
    module.items = partitioned;
    Ok(())
}

/// Side map (path → original source text) needed by
/// `report_located` to render `<file>:<line>:<col>: <message>`
/// diagnostics for any error that escapes after parsing.
pub type SourceMap = HashMap<PathBuf, String>;

#[cfg(feature = "surface")]
pub(crate) struct PreparedTestPackage<P: crate::pass::visit_mut::TypecheckVisitPhase> {
    pub(crate) package: crate::pass::alpha_normalize::AlphaNormalizedPackage<P>,
    pub(crate) sources: SourceMap,
}

/// Walk and parse the workspace rooted at `cwd`, reporting any walk /
/// parse failure to stderr and mapping it to its exit code. `kio test`
/// walks once (to census `equiv` blocks) before choosing the no-equiv
/// fast path or the discharge path, so the walk is exposed on its own.
#[cfg(feature = "surface")]
pub(crate) fn walk_or_report(
    cwd: &Path,
) -> Result<package_collection::ParsedPackageCollection, ExitCode> {
    match package_collection::walk(cwd) {
        Ok(workspace) => Ok(workspace),
        Err((walk_err, partial_sources)) => {
            let mut sources: HashMap<PathBuf, String> = HashMap::new();
            for (path, source) in partial_sources {
                sources.insert(path, source);
            }
            if let package_collection::WalkError::Parse {
                path, source_text, ..
            } = &walk_err
            {
                sources.insert(path.clone(), source_text.clone());
            }
            let diag = walk_err.into_located();
            Err(report_located(&sources, diag))
        }
    }
}

/// Lower and resolve an already-walked workspace into the root
/// package's typed-but-unsubstituted [`Lowered`](crate::ast::Lowered)
/// AST for `kio test`'s discharge path — deliberately stopping before
/// the `Lowered → Prime` substitution so `Item::Equiv` survives for the
/// runner to read (unlike [`analyze_parsed_workspace_with`], which
/// substitutes and validates). Paired with [`walk_or_report`] so
/// `kio test` walks once for both the equiv census and this step.
#[cfg(feature = "surface")]
pub(crate) fn prepare_resolved_package_from_walked<P: Pipeline>(
    parsed_ws: &package_collection::ParsedPackageCollection,
) -> Result<PreparedTestPackage<P::LoweredPhase>, ExitCode>
where
    P::LoweredPhase: ResolvePhase,
    P::LoweredPhase:
        crate::ast::Phase<TypeLabelSugar = crate::ast::Never, ExprResolved = ()> + Clone,
{
    let mut sources: HashMap<PathBuf, String> = HashMap::new();
    for pkg in parsed_ws.packages.values() {
        for (path, source) in &pkg.sources {
            sources.insert(path.clone(), source.clone());
        }
    }

    let cache_state = TypedCacheState::disabled(P::CACHE_TAG);
    let mut root_summary: Option<(P::LoweringContext, PackageSummary<P::LoweredPhase>)> = None;

    // Every package is lowered so a build error anywhere in the workspace
    // surfaces, but only the root's summary is kept — `kio test`
    // discharges the root package.
    for (key, parsed_pkg) in &parsed_ws.packages {
        let (context, summary) = match build_package_summary::<P>(key, parsed_pkg, &cache_state) {
            Ok(value) => value,
            Err(diag) => return Err(report_located(&sources, diag)),
        };
        if key == &parsed_ws.root {
            root_summary = Some((context, summary));
        }
    }

    let (context, mut summary) = root_summary.expect("workspace always contains its root package");
    let parsed_root = parsed_ws
        .packages
        .get(&parsed_ws.root)
        .expect("workspace always contains its root package");
    let module_paths: Vec<PackageModuleKey> = summary.modules.keys().cloned().collect();
    for module_path in module_paths {
        let forced = match force_and_lower_module::<P>(
            parsed_root,
            &context,
            &summary.modules,
            &module_path,
        ) {
            Ok(forced) => forced,
            Err(diag) => return Err(report_located(&sources, diag)),
        };
        summary
            .lowered
            .package
            .replace_module(forced.module_path.into_string(), forced.entry);
    }

    Ok(PreparedTestPackage {
        package: crate::pass::alpha_normalize::normalize_package(&summary.lowered.package),
        sources,
    })
}

#[cfg(feature = "surface")]
pub(crate) fn report_located(sources: &HashMap<PathBuf, String>, diag: LocatedError) -> ExitCode {
    let source = sources
        .get(&diag.file_path)
        .map(|s| s.as_str())
        .unwrap_or("");
    eprint_error_with_sources(&diag.file_path, source, sources, &diag.error);
    diag.error.exit_code()
}

pub(crate) fn report_analysis_failure(failure: &AnalysisFailure) -> ExitCode {
    let diag = failure.primary_error();
    let source = failure
        .sources
        .get(&diag.file_path)
        .map(|s| s.as_str())
        .unwrap_or("");
    eprint_error_with_sources(&diag.file_path, source, &failure.sources, &diag.error);
    diag.error.exit_code()
}

/// As [`report_analysis_failure`], but renders the diagnostic into a
/// `String` (with a trailing newline, matching the `eprintln!` form)
/// rather than printing to stderr. Used by the multi-package fan-out so
/// each package's diagnostics can be buffered and replayed in input
/// order (`cmd::package_fanout`).
pub(crate) fn render_analysis_failure(failure: &AnalysisFailure, buf: &mut String) -> ExitCode {
    let diag = failure.primary_error();
    let source = failure
        .sources
        .get(&diag.file_path)
        .map(|s| s.as_str())
        .unwrap_or("");
    let color = crate::diagnostic::ColorMode::for_stderr();
    let display_root = diagnostic_display_root();
    buf.push_str(&crate::diagnostic::render_with_sources(
        &diag.file_path,
        source,
        &failure.sources,
        display_root.as_deref(),
        &diag.error,
        color,
    ));
    buf.push('\n');
    diag.error.exit_code()
}

pub(crate) fn eprint_error(path: &Path, source: &str, err: &Error) {
    let color = crate::diagnostic::ColorMode::for_stderr();
    eprintln!(
        "{}",
        crate::diagnostic::render(&display_path(path), source, err, color)
    );
}

fn eprint_error_with_sources(
    path: &Path,
    source: &str,
    sources: &HashMap<PathBuf, String>,
    err: &Error,
) {
    let color = crate::diagnostic::ColorMode::for_stderr();
    let display_root = diagnostic_display_root();
    eprintln!(
        "{}",
        crate::diagnostic::render_with_sources(
            path,
            source,
            sources,
            display_root.as_deref(),
            err,
            color,
        )
    );
}

/// As [`eprint_error`], but renders into `buf` (with a trailing newline,
/// matching the `eprintln!` form) rather than printing to stderr. Used by
/// `cmd::sig`, which captures diagnostics into a buffer.
#[cfg(feature = "surface")]
pub(crate) fn render_error(path: &Path, source: &str, err: &Error, buf: &mut String) {
    let color = crate::diagnostic::ColorMode::for_stderr();
    buf.push_str(&crate::diagnostic::render(
        &display_path(path),
        source,
        err,
        color,
    ));
    buf.push('\n');
}

/// The local name of every dependency declared at the package rooted at
/// `root` (the leading segment under which each dependency's modules are
/// re-rooted), printing any diagnostic to stderr and returning the
/// matching exit code on failure. `kio test` uses this to recognize which
/// of the package's modules came from a dependency, so it can scope its
/// equiv run to the consumer's own modules. Reading the dependency
/// declarations is a command-level step; the analysis pipeline never does.
#[cfg(feature = "surface")]
pub(crate) fn dependency_local_names_or_report(
    root: &Path,
) -> Result<std::collections::BTreeSet<String>, ExitCode> {
    package_collection::dependency_local_names(root).map_err(|diag| {
        let source = std::fs::read_to_string(&diag.file_path).unwrap_or_default();
        eprint_error(&diag.file_path, &source, &diag.error);
        diag.error.exit_code()
    })
}

fn diagnostic_display_root() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| std::fs::canonicalize(&cwd).ok().or(Some(cwd)))
}

fn display_path(path: &Path) -> String {
    let display_root = diagnostic_display_root();
    display_path_from_root(path, display_root.as_deref())
}

#[cfg(test)]
mod analysis_failure_render_tests {
    use super::*;
    use crate::pass::resolve::LocatedError;
    use crate::span::Span;

    #[test]
    fn analysis_failure_renderer_uses_the_exact_related_source() {
        let caller_path = PathBuf::from("caller.kio");
        let provider_path = PathBuf::from("provider.kio");
        let caller = "fn use(value: Wrap(_)) { value }\n";
        let provider = "pub type Wrap[T] = [A] T -> A;\n";
        let hole = caller.find('_').expect("hole") as u32;
        let binder = provider.find('A').expect("binder") as u32;
        let failure = AnalysisFailure::from_error(
            LocatedError {
                file_path: caller_path.clone(),
                error: Error::type_(Span::new(hole, hole + 1), "invalid placeholder")
                    .with_secondary_in_file(
                        provider_path.clone(),
                        Span::new(binder, binder + 1),
                        "provider binder",
                    ),
            },
            HashMap::from([
                (caller_path, caller.to_owned()),
                (provider_path, provider.to_owned()),
            ]),
        );

        let mut rendered = String::new();
        assert_eq!(
            render_analysis_failure(&failure, &mut rendered),
            ExitCode::Type
        );
        assert!(rendered.contains("provider.kio:1:"), "{rendered}");
        assert!(
            rendered.contains("pub type Wrap[T] = [A] T -> A;"),
            "{rendered}"
        );
        assert!(rendered.contains("provider binder"), "{rendered}");
    }

    #[test]
    fn analysis_failure_same_file_route_matches_the_compatibility_renderer() {
        let path = PathBuf::from("caller.kio");
        let source = "fn caller() { bad }\n";
        let error = Error::type_(Span::new(14, 17), "invalid value")
            .with_secondary(Span::new(3, 9), "defined here");
        let failure = AnalysisFailure::from_error(
            LocatedError {
                file_path: path.clone(),
                error: error.clone(),
            },
            HashMap::from([(path.clone(), source.to_owned())]),
        );
        let mut rendered = String::new();
        render_analysis_failure(&failure, &mut rendered);
        let expected = format!(
            "{}\n",
            crate::diagnostic::render(
                &display_path(&path),
                source,
                &error,
                crate::diagnostic::ColorMode::for_stderr(),
            )
        );
        assert_eq!(rendered, expected);
    }
}

#[cfg(all(test, feature = "prime", feature = "cli"))]
mod prime_only_contract_surface_tests {
    use super::*;

    #[test]
    fn projected_recursive_group_repartitions_without_surface_frontend() {
        let mut module = crate::pass::parser::parse(
            "module api; \
             rec { \
               pub newtype Public : Private { constructor make; projector read; }; \
               newtype Private : Public { constructor make_private; projector read_private; }; \
             }",
        )
        .expect("parse Kio'-shaped module");
        strip_module_to_signature_surface(&mut module)
            .expect("Prime-only contract projection repartitions the group");

        assert!(
            module
                .items
                .iter()
                .all(|item| !matches!(item, crate::ast::Item::TypeRecGroup(_))),
            "erasing the public opaque payload makes the group acyclic"
        );
        let names = module
            .items
            .iter()
            .filter_map(|item| match item {
                crate::ast::Item::Newtype(newtype) => Some(newtype.name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["Public", "Private"]);
    }
}

#[cfg(all(test, feature = "surface", feature = "prime", feature = "cli"))]
mod contract_surface_tests {
    use super::*;
    use crate::error::Error;
    use crate::pass::full::FullPipeline;
    use crate::prime::pipeline::PrimePipeline;

    fn projected_contract_located_error(module_src: &str) -> LocatedError {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("dep.pkg.kio"),
            "package dep;\n\nbridge {\n  api;\n}\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("api.kio"), module_src).unwrap();
        let parsed = crate::package_collection::walk(tmp.path()).expect("walk dep package");
        let dep = parsed.packages.get(&parsed.root).expect("root package");
        contract_snapshot_via_lower::<FullPipeline>(dep)
            .expect_err("invalid projected signature must be rejected")
    }

    fn projected_contract_error(module_src: &str) -> Error {
        projected_contract_located_error(module_src).error
    }

    fn projected_errors_for_modules(bridge: &str, modules: &[(&str, &str)]) -> [LocatedError; 2] {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("dep.pkg.kio"),
            format!("package dep;\n\nbridge {{\n{bridge}\n}}\n"),
        )
        .unwrap();
        for (file, source) in modules {
            std::fs::write(tmp.path().join(file), source).unwrap();
        }
        let parsed = crate::package_collection::walk(tmp.path()).expect("walk dep package");
        let dep = parsed.packages.get(&parsed.root).expect("root package");
        [
            contract_snapshot_via_lower::<FullPipeline>(dep)
                .expect_err("full projection must reject the invalid surface package"),
            contract_snapshot_via_lower::<PrimePipeline>(dep)
                .expect_err("prime projection must reject the invalid surface package"),
        ]
    }

    fn projected_contracts_for_modules(
        modules: &[(&str, &str)],
    ) -> [Result<crate::sig::ContractSnapshot, LocatedError>; 2] {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("dep.pkg.kio"),
            "package dep;\n\nbridge {\n  **;\n}\n",
        )
        .unwrap();
        for (file, source) in modules {
            std::fs::write(tmp.path().join(file), source).unwrap();
        }
        let parsed = crate::package_collection::walk(tmp.path()).expect("walk dep package");
        let dep = parsed.packages.get(&parsed.root).expect("root package");
        [
            contract_snapshot_via_lower::<FullPipeline>(dep),
            contract_snapshot_via_lower::<PrimePipeline>(dep),
        ]
    }

    fn assert_projected_contracts_match(modules: &[(&str, &str)]) {
        let [full, prime] = projected_contracts_for_modules(modules);
        let full = full.expect("full projection accepts the signature surface");
        let prime = prime.expect("Prime projection accepts the signature surface");
        assert_eq!(
            crate::sig::contract_digest(&full),
            crate::sig::contract_digest(&prime)
        );
        assert!(
            full.items.values().any(|entry| entry.name.leaf == "expose"),
            "the compared contract must contain the exported API signature"
        );
    }

    #[test]
    fn projected_contract_accepts_host_types_in_pure_function_signatures() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("dep.pkg.kio"),
            "package dep;\n\nbridge {\n  api;\n}\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("api.kio"),
            "module api;\n\
             host type String role(str);\n\
             pub pure fn identity(value: String) -> String { value }\n",
        )
        .unwrap();
        let parsed = crate::package_collection::walk(tmp.path()).expect("walk dep package");
        let dep = parsed.packages.get(&parsed.root).expect("root package");
        let full = contract_snapshot_via_lower::<FullPipeline>(dep)
            .expect("full projection accepts the pure function signature");
        let prime = contract_snapshot_via_lower::<PrimePipeline>(dep)
            .expect("Prime projection accepts the pure function signature");
        assert_eq!(
            crate::sig::contract_digest(&full),
            crate::sig::contract_digest(&prime)
        );
    }

    #[test]
    fn projected_contract_validates_kind_and_arity() {
        let error = projected_contract_error(
            "module api;\n\
             pub type Box[A] = A;\n\
             pub fn bad(value: Box(., .)) -> . { () }\n",
        );
        assert!(
            matches!(error, Error::Type(_)),
            "contract signature must reject an over-applied type: {error:?}"
        );
    }

    #[test]
    fn body_only_surface_imports_do_not_obstruct_projected_contracts() {
        let cases = [
            (
                "literal",
                "module provider; pub literal zero = 0;",
                "import provider(zero);",
            ),
            (
                "operator",
                "module provider; pub fn add(left: ., right: .) -> . { () } pub op _ + _ { impl add; };",
                "import provider(op _ + _);",
            ),
            (
                "variadic operator",
                "module provider; pub fn nil() -> . { () } pub fn cons(head: ., tail: .) -> . { () } pub varop [* *] { foldr cons nil; };",
                "import provider(varop [* *]);",
            ),
            (
                "labels",
                "module provider; pub labels Choice = { tag: . } | { other: . };",
                "import provider({tag}, Tag, Choice);",
            ),
            (
                "label forward",
                "module provider; pub labels { original: . }; pub type {tag} = {original};",
                "import provider({tag});",
            ),
            (
                "elaborator",
                "module provider; \
                 import __comptime__; \
                 pub pure fn identity_impl( \
                   _ct: __Comptime__, \
                   _source: __Type__, \
                   value: __Checked_term__ \
                 ) -> __Checked_term__ { value } \
                 pub elab identity : [A] A -> A { impl identity_impl; };",
                "import provider(identity);",
            ),
        ];
        for (_, provider, import) in cases {
            let api = format!("module api; {import} pub fn expose[A](value: A) -> A {{ value }}");
            assert_projected_contracts_match(&[
                ("provider.kio", provider),
                ("api.kio", api.as_str()),
            ]);
        }
    }

    #[test]
    fn projected_mixed_import_keeps_ordinary_signature_items() {
        assert_projected_contracts_match(&[
            (
                "provider.kio",
                "module provider; \
                 host type Host; \
                 host fn host_id(value: Host) -> Host; \
                 pub type Payload = .; \
                 pub newtype Wrapped : Payload { pub constructor wrap; pub projector unwrap; }; \
                 pub fn identity(value: Payload) -> Payload { value } \
                 pub literal zero = 0;",
            ),
            (
                "api.kio",
                "module api; \
                 import provider(Host, host_id, identity, Payload, Wrapped, zero); \
                 pub fn expose(value: Payload) -> Payload { value }",
            ),
        ]);
    }

    #[test]
    fn projected_label_import_can_be_shadowed_by_signature_binder() {
        assert_projected_contracts_match(&[
            ("provider.kio", "module provider; pub labels { tag: . };"),
            (
                "api.kio",
                "module api; import provider(Tag); pub fn expose[Tag](value: Tag) -> Tag { value }",
            ),
        ]);
    }

    #[test]
    fn projected_label_identity_in_signature_still_requires_sealed_contract() {
        for result in projected_contracts_for_modules(&[
            ("provider.kio", "module provider; pub labels { tag: . };"),
            (
                "api.kio",
                "module api; import provider(Tag); pub fn expose(value: Tag) -> Tag { value }",
            ),
        ]) {
            let error =
                result.expect_err("labels-generated identity is not in an unsealed contract");
            assert!(matches!(error.error, Error::NameRes(_)), "got: {error:?}");
        }
    }

    #[test]
    fn projected_opaque_newtype_does_not_retain_label_generated_payload() {
        let [full, prime] = projected_contracts_for_modules(&[(
            "api.kio",
            "module api; \
             labels { hidden: . }; \
             pub newtype Token : Hidden { constructor make_token; projector read_token; };",
        )]);
        let full = full.expect("full projection hides an opaque newtype payload");
        let prime = prime.expect("Prime projection hides an opaque newtype payload");
        assert_eq!(
            crate::sig::contract_digest(&full),
            crate::sig::contract_digest(&prime)
        );
        assert!(
            full.items.values().any(|entry| {
                entry.name.leaf == "Token"
                    && matches!(
                        entry.kind,
                        crate::sig::ContractKind::Newtype {
                            surface: crate::sig::PublicNewtypeSurface::Opaque,
                            ..
                        }
                    )
            }),
            "the public nominal identity remains in the contract"
        );
        assert!(
            full.items.values().all(|entry| entry.name.leaf != "Hidden"),
            "the private label-generated payload must not enter the contract"
        );
    }

    #[test]
    fn projected_opaque_newtype_recomputes_its_recursive_group_partition() {
        let [full, prime] = projected_contracts_for_modules(&[(
            "api.kio",
            "module api; \
             rec { \
               pub newtype Public : Private { constructor make; projector read; }; \
               newtype Private : Public { constructor make_private; projector read_private; }; \
             }",
        )]);
        let full = full.expect("full projection repartitions the now-acyclic group");
        let prime = prime.expect("Prime projection repartitions the now-acyclic group");
        assert_eq!(
            crate::sig::contract_digest(&full),
            crate::sig::contract_digest(&prime)
        );
        assert_eq!(full.items.len(), 1, "only the public opaque entry survives");
        assert!(full.items.values().any(|entry| {
            entry.name.leaf == "Public"
                && matches!(
                    entry.kind,
                    crate::sig::ContractKind::Newtype {
                        surface: crate::sig::PublicNewtypeSurface::Opaque,
                        ..
                    }
                )
        }));
    }

    #[test]
    fn projected_opaque_singleton_newtype_drops_redundant_recursion() {
        let [full, prime] = projected_contracts_for_modules(&[(
            "api.kio",
            "module api; \
             pub rec newtype Loop : Loop { constructor make; projector read; };",
        )]);
        let full = full.expect("full projection drops the now-redundant singleton marker");
        let prime = prime.expect("Prime projection drops the now-redundant singleton marker");
        assert_eq!(
            crate::sig::contract_digest(&full),
            crate::sig::contract_digest(&prime)
        );
        assert_eq!(full.items.len(), 1);
        assert!(full.items.values().any(|entry| entry.name.leaf == "Loop"));
    }

    #[test]
    fn projected_auxiliary_import_validation_precedes_filtering() {
        let cases = [
            (
                "missing",
                vec![(
                    "api.kio",
                    "module api; import missing(absent); pub fn expose() -> . { () }",
                )],
            ),
            (
                "private",
                vec![
                    ("provider.kio", "module provider; literal hidden = 0;"),
                    (
                        "api.kio",
                        "module api; import provider(hidden); pub fn expose() -> . { () }",
                    ),
                ],
            ),
            (
                "cycle",
                vec![
                    (
                        "a.kio",
                        "module a; import b(b_marker); pub literal a_marker = 0; pub fn expose() -> . { () }",
                    ),
                    (
                        "b.kio",
                        "module b; import a(a_marker); pub literal b_marker = 0;",
                    ),
                ],
            ),
        ];
        for (name, modules) in cases {
            for result in projected_contracts_for_modules(&modules) {
                let error = result.expect_err("the original written import graph is invalid");
                assert!(matches!(error.error, Error::Import(_)), "{name}: {error:?}");
            }
        }
    }

    #[test]
    fn projected_contract_validates_recursive_headers() {
        let error = projected_contract_error(
            "module api;\n\
             rec(loop) {\n\
               pub fn left[A](value: A) -> A { rec right(A, value) };\n\
               pub fn right[A, B](value: A) -> A { rec left(A, value) }\n\
             }\n",
        );
        assert!(
            matches!(error, Error::Parse(_)),
            "contract projection must apply ordinary rec-header validation: {error:?}"
        );
    }

    #[test]
    fn projected_rec_parse_error_precedes_name_and_type_errors() {
        let error = projected_contract_error(
            "module api;\n\
             rec(loop) {\n\
               pub fn same[*F](value: .) -> . { value };\n\
               pub fn same[A, B](value: .) -> . { value }\n\
             }\n",
        );
        assert!(
            matches!(error, Error::Parse(_)),
            "rec-header Parse errors must precede Name and Type errors: {error:?}"
        );
    }

    #[test]
    fn projected_rec_type_error_respects_type_source_order() {
        let error = projected_contract_error(
            "module api;\n\
             type Box[A] = A;\n\
             pub fn earlier(value: Box(., .)) -> . { () }\n\
             rec(loop) {\n\
               pub fn later[*F](value: .) -> . { value }\n\
             }\n",
        );
        let diagnostic = error.diagnostic();
        assert!(
            matches!(error, Error::Type(_))
                && !diagnostic.message.contains("`rec(loop)` can only pack"),
            "the earlier ordinary Type error must precede the later rec-header Type error: \
             {error:?}"
        );
    }

    #[test]
    fn projected_contract_reports_name_before_private_type_closure() {
        let error = projected_contract_error(
            "module api;\n\
             pub fn expose(value: Hidden) -> Hidden { value }\n\
             type Hidden = .;\n",
        );
        assert!(
            matches!(error, Error::NameRes(_)),
            "source-order name resolution must precede bridge closure: {error:?}"
        );
    }

    #[test]
    fn projected_contract_reports_import_before_name_and_bridge() {
        let error = projected_contract_error(
            "module api;\n\
             import absent(Missing);\n\
             pub fn expose(value: Hidden) -> Hidden { value }\n\
             type Hidden = .;\n",
        );
        assert!(
            matches!(error, Error::Import(_)),
            "use validation must precede source-order and bridge errors: {error:?}"
        );
    }

    #[test]
    fn projected_contract_reports_import_before_duplicate_declaration() {
        let error = projected_contract_error(
            "module api;\n\
             import absent(Missing);\n\
             type Duplicate = .;\n\
             type Duplicate = .;\n\
             pub fn expose(value: Duplicate) -> Duplicate { value }\n",
        );
        assert!(
            matches!(error, Error::Import(_)),
            "use validation must precede top-level duplicate-name errors: {error:?}"
        );
    }

    #[test]
    fn projected_contract_reports_import_cycle_before_name_errors() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("dep.pkg.kio"),
            "package dep;\n\nbridge {\n  a;\n}\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("a.kio"),
            "module a;\n\
             import b as b;\n\
             pub fn expose(value: Missing) -> Missing { value }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("b.kio"),
            "module b;\n\
             import a as a;\n\
             pub type Present = .;\n",
        )
        .unwrap();
        let parsed = crate::package_collection::walk(tmp.path()).expect("walk dep package");
        let dep = parsed.packages.get(&parsed.root).expect("root package");
        let error = contract_snapshot_via_lower::<FullPipeline>(dep)
            .expect_err("the projected dependency must reject its value-import cycle")
            .error;
        assert!(
            matches!(error, Error::Import(_)),
            "value-import cycles must precede signature Name errors: {error:?}"
        );
    }

    #[test]
    fn projected_patterns_accept_host_types_in_pure_function_signatures() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("dep.pkg.kio"),
            "package dep;\n\nbridge {\n  api;\n}\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("api.kio"),
            "module api;\n\
             host type String role(str);\n\
             pub pure fn bad((text: String, _: .)) -> . { () }\n",
        )
        .unwrap();
        let parsed = crate::package_collection::walk(tmp.path()).expect("walk dep package");
        let dep = parsed.packages.get(&parsed.root).expect("root package");

        let full = contract_snapshot_via_lower::<FullPipeline>(dep)
            .expect("full projection accepts the host-typed pattern");
        let prime = contract_snapshot_via_lower::<PrimePipeline>(dep)
            .expect("Prime projection accepts the host-typed pattern");
        assert_eq!(
            crate::sig::contract_digest(&full),
            crate::sig::contract_digest(&prime)
        );
    }

    #[test]
    fn projected_rec_pattern_headers_match_full_and_prime_pipelines() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("dep.pkg.kio"),
            "package dep;\n\nbridge {\n  api;\n}\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("api.kio"),
            "module api;\n\
             rec(loop) { pub fn first[A]((value: A, _: A)) -> A { value } }\n",
        )
        .unwrap();
        let parsed = crate::package_collection::walk(tmp.path()).expect("walk dep package");
        let dep = parsed.packages.get(&parsed.root).expect("root package");
        let full = contract_snapshot_via_lower::<FullPipeline>(dep)
            .expect("full projection accepts the annotated rec pattern");
        let prime = contract_snapshot_via_lower::<PrimePipeline>(dep)
            .expect("prime projection accepts the annotated rec pattern");
        assert_eq!(
            crate::sig::contract_digest(&full),
            crate::sig::contract_digest(&prime)
        );
    }

    #[test]
    fn projected_contract_reports_type_before_private_type_closure() {
        let error = projected_contract_error(
            "module api;\n\
             type Box[A] = A;\n\
             type Hidden = .;\n\
             pub fn expose(value: Box(., .)) -> Hidden { () }\n",
        );
        assert!(
            matches!(error, Error::Type(_)),
            "kind checking must precede bridge closure: {error:?}"
        );
    }

    #[test]
    fn projected_contract_rejects_private_reachable_type() {
        let source = "module api;\n\
                      type Hidden = .;\n\
                      pub fn expose(value: Hidden) -> Hidden { value }\n";
        let located = projected_contract_located_error(source);
        let Error::Type(error) = &located.error else {
            panic!(
                "a private reachable type must fail declaration-signature visibility: {:?}",
                located.error
            );
        };
        assert_eq!(
            error.message,
            "type alias `Hidden` must be at least as visible as function `expose`"
        );
        assert_eq!(
            error.help(),
            Some("give type alias `Hidden` visibility equal to or wider than function `expose`")
        );
        let hidden_start = source.find("value: Hidden").expect("parameter type") as u32 + 7;
        assert_eq!(
            error.span,
            crate::span::Span::new(hidden_start, hidden_start + 6)
        );
        assert_eq!(located.file_path.file_name().unwrap(), "api.kio");
        assert_eq!(
            &source[error.span.start as usize..error.span.end as usize],
            "Hidden"
        );
    }

    #[test]
    fn projected_contract_binders_ignore_same_named_private_nominal() {
        let snapshot = |module_src: &str| {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::write(
                tmp.path().join("dep.pkg.kio"),
                "package dep;\n\nbridge {\n  api;\n}\n",
            )
            .unwrap();
            std::fs::write(tmp.path().join("api.kio"), module_src).unwrap();
            let parsed = crate::package_collection::walk(tmp.path()).expect("walk dep package");
            let dep = parsed.packages.get(&parsed.root).expect("root package");
            contract_snapshot_via_lower::<FullPipeline>(dep).expect("valid projected contract")
        };
        let before = snapshot(
            "module api;\n\
             pub fn id[A](value: A) -> A { value }\n\
             pub type Identity[A] = A;\n",
        );
        let after = snapshot(
            "module api;\n\
             pub fn id[A](value: A) -> A { value }\n\
             pub type Identity[A] = A;\n\
             type A = .;\n",
        );
        assert_eq!(
            crate::sig::contract_digest(&before),
            crate::sig::contract_digest(&after),
            "an unrelated private nominal cannot capture written contract binders"
        );
    }

    #[test]
    fn unbridged_type_closure_points_to_referring_signature() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("dep.pkg.kio"),
            "package dep;\n\nbridge {\n  api;\n}\n",
        )
        .unwrap();
        let api_source = "module api;\n\
                          import types(Hidden);\n\
                          pub fn expose(value: Hidden) -> Hidden { value }\n";
        std::fs::write(tmp.path().join("api.kio"), api_source).unwrap();
        std::fs::write(
            tmp.path().join("types.kio"),
            "module types;\n\
             pub type Hidden = .;\n",
        )
        .unwrap();
        let parsed = crate::package_collection::walk(tmp.path()).expect("walk dep package");
        let dep = parsed.packages.get(&parsed.root).expect("root package");
        let located = contract_snapshot_via_lower::<FullPipeline>(dep)
            .expect_err("unbridged reachable type must be rejected");
        let (span, _) = located.error.diag();
        assert!(matches!(located.error, Error::Bridge(_)));
        assert_eq!(located.file_path.file_name().unwrap(), "api.kio");
        assert_eq!(
            &api_source[span.start as usize..span.end as usize],
            "Hidden"
        );
    }

    /// An unsealed dependency whose function bodies and parameter spellings use
    /// surface forms must yield the
    /// **identical** contract digest on `kio` (FullPipeline) and `kio-prime`
    /// (PrimePipeline). The contract is the signature surface, so bodies
    /// never enter it; before the fix the kio-prime path ran a full lowering
    /// and rejected the surface body with "is not part of Kio'".
    #[test]
    fn surface_fn_dep_contract_is_identical_across_pipelines() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("dep.pkg.kio"),
            "package dep;\n\nbridge {\n  api;\n}\n",
        )
        .unwrap();
        // `api` exports a `pub fn` whose body is a surface `if`/`else` — the
        // exact shape `kio-prime`'s lowering rejects in a full compile.
        std::fs::write(
            root.join("api.kio"),
            "module api;\n\
             host type Bool role(bool);\n\
             host type I32 role(i32);\n\
             pub fn pick(b: Bool, x: I32, y: I32) -> I32 { if b { x } else { y } }\n\
             pub fn first[A][B]((value: A, _: B)) -> A { value }\n",
        )
        .unwrap();

        let parsed = crate::package_collection::walk(root).expect("walk dep package");
        let dep = parsed.packages.get(&parsed.root).expect("root package");

        // The OLD path computed the contract via a full kio-prime *compile*
        // (`compile_workspace_at_buffered(prime_only = true)`), which forces
        // and rejects the surface `if`/`else` body.
        let mut buf = String::new();
        assert!(
            compile_workspace_at_buffered(root, true, false, &mut buf).is_err(),
            "a full kio-prime compile rejects the surface body: {buf}"
        );

        // The fix computes the contract from signatures only, so it succeeds
        // identically on both pipelines.
        let full = contract_snapshot_via_lower::<FullPipeline>(dep)
            .expect("kio (FullPipeline) computes the contract from signatures");
        let prime = contract_snapshot_via_lower::<PrimePipeline>(dep)
            .expect("kio-prime (PrimePipeline) computes the same contract");

        assert_eq!(
            crate::sig::contract_digest(&full),
            crate::sig::contract_digest(&prime),
            "the unsealed contract digest must be identical across kio and kio-prime"
        );
        // The exported fn signature is actually captured (not an empty
        // contract that would trivially match).
        assert!(
            full.items.values().any(|e| e.name.leaf == "pick")
                && full.items.values().any(|e| e.name.leaf == "first"),
            "the contract carries both exported signatures: {:?}",
            full.items.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn contract_snapshot_rejects_distinct_origins_for_one_signature_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("dep.pkg.kio"),
            "package dep;\n\nbridge {\n  first;\n  second;\n  api;\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("first.kio"), "module first; pub type T = .;\n").unwrap();
        std::fs::write(root.join("second.kio"), "module second; pub type T = .;\n").unwrap();
        std::fs::write(
            root.join("api.kio"),
            "module api;\nimport first(T);\nimport second(T);\npub fn expose(value: T) -> T { value }\n",
        )
        .unwrap();

        let parsed = crate::package_collection::walk(root).expect("walk dep package");
        let dep = parsed.packages.get(&parsed.root).expect("root package");
        for result in [
            contract_snapshot_via_lower::<FullPipeline>(dep),
            contract_snapshot_via_lower::<PrimePipeline>(dep),
        ] {
            let err = result.expect_err("a contract name must have one source identity");
            assert!(
                matches!(err.error, crate::error::Error::NameRes(_)),
                "got: {:?}",
                err.error
            );
        }
    }

    #[test]
    fn contract_snapshot_validates_consumed_surface_registry_origins() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("dep.pkg.kio"),
            "package dep;\n\nbridge {\n  first;\n  second;\n  api;\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("first.kio"),
            "module first; pub literal pick = 1;\n",
        )
        .unwrap();
        std::fs::write(
            root.join("second.kio"),
            "module second; pub literal pick = 2;\n",
        )
        .unwrap();
        std::fs::write(
            root.join("api.kio"),
            "module api;\nimport first(pick);\nimport second(pick);\npub fn expose() -> . { () }\n",
        )
        .unwrap();

        let parsed = crate::package_collection::walk(root).expect("walk dep package");
        let dep = parsed.packages.get(&parsed.root).expect("root package");
        for error in [
            contract_snapshot_via_lower::<FullPipeline>(dep)
                .expect_err("full projection must validate source before stripping literals"),
            contract_snapshot_via_lower::<PrimePipeline>(dep)
                .expect_err("prime projection must validate source before stripping literals"),
        ] {
            assert!(
                matches!(error.error, crate::error::Error::NameRes(_)),
                "got: {:?}",
                error.error
            );
        }
    }

    #[test]
    fn projected_elaborator_origins_match_across_pipelines_and_import_orders() {
        let elaborator =
            "pub elab same : [Source] Source -> [Target] Target { impl implementation; };";
        for (first_item, second_item) in [
            (elaborator, elaborator),
            ("pub fn same() -> . { () }", elaborator),
        ] {
            for api in [
                "module api; import first(same); import second(same); pub fn expose() -> . { () }",
                "module api; import second(same); import first(same); pub fn expose() -> . { () }",
            ] {
                for error in projected_errors_for_modules(
                    "  api;",
                    &[
                        ("first.kio", &format!("module first; {first_item}")),
                        ("second.kio", &format!("module second; {second_item}")),
                        ("api.kio", api),
                    ],
                ) {
                    assert!(
                        matches!(error.error, Error::NameRes(_))
                            && error.error.diagnostic().message.contains("binding `same`"),
                        "got: {:?}",
                        error.error
                    );
                }
            }
        }
    }

    #[test]
    fn projected_label_nominal_collisions_match_across_pipelines_and_orders() {
        for api in [
            "module api; import labels(Item); import types(Item); pub fn expose() -> . { () }",
            "module api; import types(Item); import labels(Item); pub fn expose() -> . { () }",
        ] {
            for error in projected_errors_for_modules(
                "  api;",
                &[
                    ("labels.kio", "module labels; pub labels { item: . };"),
                    ("types.kio", "module types; pub type Item = .;"),
                    ("api.kio", api),
                ],
            ) {
                assert!(
                    matches!(error.error, Error::NameRes(_))
                        && error.error.diagnostic().message.contains("binding `Item`"),
                    "got: {:?}",
                    error.error
                );
            }
        }

        for api in [
            "module api; pub type Item = .; pub labels { item: . };",
            "module api; pub labels { item: . }; pub type Item = .;",
        ] {
            for error in projected_errors_for_modules("  api;", &[("api.kio", api)]) {
                assert!(
                    matches!(error.error, Error::NameRes(_))
                        && error
                            .error
                            .diagnostic()
                            .message
                            .contains("duplicate top-level declaration `Item`"),
                    "got: {:?}",
                    error.error
                );
            }
        }
    }

    #[test]
    fn projected_import_failures_precede_consumed_origin_errors_in_both_pipelines() {
        let elaborator =
            "pub elab same : [Source] Source -> [Target] Target { impl implementation; };";
        for error in projected_errors_for_modules(
            "  api;",
            &[
                ("first.kio", &format!("module first; {elaborator}")),
                ("second.kio", &format!("module second; {elaborator}")),
                (
                    "api.kio",
                    "module api; import first(same); import second(same); import missing(absent); pub fn expose() -> . { () }",
                ),
            ],
        ) {
            assert!(
                matches!(error.error, Error::Import(_)),
                "got: {:?}",
                error.error
            );
        }
    }

    #[test]
    fn projected_import_cycles_precede_consumed_origin_errors_in_both_pipelines() {
        let elaborator =
            "pub elab same : [Source] Source -> [Target] Target { impl implementation; };";
        for error in projected_errors_for_modules(
            "  api;",
            &[
                ("first.kio", &format!("module first; {elaborator}")),
                ("second.kio", &format!("module second; {elaborator}")),
                (
                    "api.kio",
                    "module api; import cycle as cycle; import first(same); import second(same); pub fn expose() -> . { () }",
                ),
                ("cycle.kio", "module cycle; import api as api;"),
            ],
        ) {
            assert!(
                matches!(error.error, Error::Import(_)),
                "got: {:?}",
                error.error
            );
        }
    }

    /// The strip lifts `rec`-group members into the contract (their
    /// signatures are real exports) while dropping surface-only auxiliary
    /// declarations, and a `rec` body using surface forms is still handled
    /// identically across both pipelines.
    #[test]
    fn rec_group_members_reach_contract_identically() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("dep.pkg.kio"),
            "package dep;\n\nbridge {\n  api;\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("api.kio"),
            "module api;\n\
             host type I32 role(i32);\n\
             rec(loop) {\n\
               pub fn ping(n: I32) -> I32 { rec pong(n) };\n\
               pub fn pong(n: I32) -> I32 { rec ping(n) }\n\
             }\n",
        )
        .unwrap();

        let parsed = crate::package_collection::walk(root).expect("walk dep package");
        let dep = parsed.packages.get(&parsed.root).expect("root package");
        let full = contract_snapshot_via_lower::<FullPipeline>(dep).expect("kio contract");
        let prime = contract_snapshot_via_lower::<PrimePipeline>(dep).expect("kio-prime contract");

        assert_eq!(
            crate::sig::contract_digest(&full),
            crate::sig::contract_digest(&prime),
            "rec-group member signatures digest identically across pipelines"
        );
        assert!(
            full.items.values().any(|e| e.name.leaf == "ping")
                && full.items.values().any(|e| e.name.leaf == "pong"),
            "both rec-group members reach the contract surface"
        );
    }
}
