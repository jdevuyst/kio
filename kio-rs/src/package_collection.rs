//! Local package collection and package-boundary views.
//!
//! The package boundary is declared by `<name>.pkg.kio` and explicit
//! bridge blocks. Checking and building use only checked-in local
//! source.

#![allow(clippy::result_large_err)] // walker errors carry diagnostic context

#[cfg(feature = "cli")]
use crate::ast::TypeParam;
use crate::ast::{
    Item, Module, OpPart, OperatorDispatchKey, Phase, Prime, Surface, Type, VariadicSpec,
};
use crate::error::Error;
use crate::pass::resolve::{LocatedError, Package, PackageFileEntry};
use crate::path_display::DisplayPath;
use crate::span::Span;
#[cfg(feature = "cli")]
use std::collections::BTreeSet;
use std::collections::{BTreeMap, HashMap};
#[cfg(feature = "cli")]
use std::ffi::OsStr;
#[cfg(feature = "cli")]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(all(feature = "cli", feature = "surface"))]
mod retype_labels;

#[cfg(feature = "cli")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceOverlay {
    files: BTreeMap<PathBuf, String>,
    complete_root: Option<PathBuf>,
}

#[cfg(all(test, feature = "cli"))]
thread_local! {
    static IMPORT_GRAMMAR_DENIED_READS: std::cell::RefCell<Option<(BTreeSet<PathBuf>, Vec<PathBuf>)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, feature = "cli"))]
fn import_grammar_observe_source_read(path: &Path) -> io::Result<()> {
    IMPORT_GRAMMAR_DENIED_READS.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some((denied, attempts)) = slot.as_mut()
            && denied.contains(path)
        {
            attempts.push(path.to_path_buf());
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "provider read denied",
            ));
        }
        Ok(())
    })
}

#[cfg(all(test, feature = "cli"))]
pub(crate) fn import_grammar_with_denied_reads<R>(
    paths: impl IntoIterator<Item = PathBuf>,
    f: impl FnOnce() -> R,
) -> (R, Vec<PathBuf>) {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            IMPORT_GRAMMAR_DENIED_READS.with(|slot| {
                slot.borrow_mut().take();
            });
        }
    }
    IMPORT_GRAMMAR_DENIED_READS.with(|slot| {
        assert!(slot.borrow().is_none(), "provider observers cannot nest");
        *slot.borrow_mut() = Some((paths.into_iter().collect(), Vec::new()));
    });
    let _clear = Clear;
    let result = f();
    let attempts = IMPORT_GRAMMAR_DENIED_READS
        .with(|slot| slot.borrow().as_ref().expect("observer active").1.clone());
    (result, attempts)
}

#[cfg(feature = "cli")]
impl SourceOverlay {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn complete(root: PathBuf, files: BTreeMap<PathBuf, String>) -> Self {
        Self {
            files,
            complete_root: Some(root),
        }
    }

    pub fn insert(&mut self, canonical_path: PathBuf, text: String) {
        self.files.insert(canonical_path, text);
    }

    pub fn get(&self, path: &Path) -> Option<&str> {
        self.files.get(path).map(String::as_str)
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    pub fn read(&self, path: &Path) -> io::Result<String> {
        #[cfg(all(test, feature = "cli"))]
        import_grammar_observe_source_read(path)?;
        if let Some(s) = self.files.get(path) {
            Ok(s.clone())
        } else if self.is_complete_for(path) || self.path_is_under_complete_root(path) {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("source overlay has no entry for {}", path.display()),
            ))
        } else {
            fs::read_to_string(path)
        }
    }

    pub fn complete_root_for(&self, cwd: &Path) -> Option<PathBuf> {
        let root = self.complete_root.as_ref()?;
        if root == cwd {
            Some(root.clone())
        } else {
            None
        }
    }

    pub fn is_complete_for(&self, root: &Path) -> bool {
        self.complete_root.as_deref() == Some(root)
    }

    fn path_is_under_complete_root(&self, path: &Path) -> bool {
        self.complete_root
            .as_ref()
            .is_some_and(|root| path.starts_with(root))
    }

    pub fn kio_files_under(&self, root: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = self
            .files
            .keys()
            .filter(|path| {
                path.starts_with(root)
                    && path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(crate::file_kind::has_kio_extension)
            })
            .cloned()
            .collect();
        files.sort();
        files
    }
}

/// Parser identity for one ordinary on-disk module file.
///
/// The selected lexical file path and its declared module path determine the
/// source root. Validation and cache identity do not depend on provider files.
#[cfg(feature = "cli")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleFileParserContext {
    source_path: PathBuf,
    source_root: PathBuf,
}

#[cfg(feature = "cli")]
impl ModuleFileParserContext {
    pub(crate) fn from_module(
        source_path: &Path,
        module: &crate::ast::Module<crate::ast::Surface>,
    ) -> Result<Self, Error> {
        Self::from_module_path(source_path, &module.path)
    }

    pub(crate) fn from_module_path(
        source_path: &Path,
        module_path: &crate::ast::ModulePath,
    ) -> Result<Self, Error> {
        if !source_path.is_absolute() {
            return Err(Error::parse(
                module_path.span,
                format!(
                    "selected module path `{}` is not absolute",
                    source_path.display()
                ),
            ));
        }
        let source_root = source_root_from_module_path(source_path, module_path)?;
        Ok(Self {
            source_path: source_path.to_path_buf(),
            source_root,
        })
    }
}

#[cfg(feature = "cli")]
fn source_root_from_module_path(
    source_path: &Path,
    module_path: &crate::ast::ModulePath,
) -> Result<PathBuf, Error> {
    let Some((last, parents)) = module_path.segments.split_last() else {
        return Err(Error::parse(
            module_path.span,
            "a module declaration must contain at least one path segment".to_owned(),
        ));
    };

    let stem_matches = source_path.extension() == Some(OsStr::new("kio"))
        && source_path.file_stem() == Some(OsStr::new(last.name.as_str()));
    let mut root = source_path.parent();
    let parents_match = parents.iter().rev().all(|segment| {
        let Some(current) = root else {
            return false;
        };
        if current.file_name() != Some(OsStr::new(segment.name.as_str())) {
            return false;
        }
        root = current.parent();
        true
    });
    if stem_matches && parents_match {
        return Ok(root.unwrap_or_else(|| Path::new("")).to_path_buf());
    }

    let declared = module_path
        .segments
        .iter()
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join("/");
    Err(Error::parse(
        module_path.span,
        format!(
            "module declaration `{declared}` does not match the selected file path `{}`; \
             the path must end in `{declared}.kio`",
            source_path.display()
        ),
    ))
}

#[cfg(feature = "lsp")]
pub(crate) fn canonicalize_with_missing_suffix(path: &Path) -> Option<PathBuf> {
    let mut cursor = path;
    let mut missing = Vec::new();
    loop {
        if let Ok(mut canonical) = fs::canonicalize(cursor) {
            for component in missing.iter().rev() {
                canonical.push(component);
            }
            return Some(canonical);
        }
        let name = cursor.file_name()?.to_os_string();
        missing.push(name);
        cursor = cursor.parent()?;
    }
}

/// Parse one selected ordinary file after validating its lexical path against
/// its own declared module path.
#[cfg(feature = "cli")]
pub(crate) fn parse_module_file_with_file_context(
    source: &str,
    source_path: &Path,
) -> Result<crate::pass::parser::ModuleFile, Error> {
    let header = crate::pass::parser::parse_module_file_lazy(source)?;
    ModuleFileParserContext::from_module(source_path, &header.module)?;
    header.force_all()
}

#[cfg(all(feature = "surface", feature = "lsp", feature = "cli"))]
pub(crate) fn find_package_root(start: &Path) -> Option<PathBuf> {
    let mut directory = if start.is_dir() {
        Some(start)
    } else {
        start.parent()
    };
    while let Some(candidate) = directory {
        let has_package_file = fs::read_dir(candidate).ok().is_some_and(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(crate::file_kind::is_package_file)
            })
        });
        if has_package_file {
            return Some(candidate.to_path_buf());
        }
        directory = candidate.parent();
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct PackageKey {
    pub canonical_dir: PathBuf,
    pub package_name: String,
}

impl PackageKey {
    pub fn label(&self) -> String {
        format!("{}:{}", DisplayPath(&self.canonical_dir), self.package_name)
    }
}

#[derive(Debug, Clone)]
pub struct PackageEntry<P: Phase = Surface> {
    pub root_dir: PathBuf,
    pub package: Package<P>,
}

#[derive(Debug, Clone)]
pub struct PackageCollection<P: Phase = Surface> {
    pub root: PackageKey,
    pub packages: BTreeMap<PackageKey, PackageEntry<P>>,
}

impl<P: Phase> PackageCollection<P> {
    pub fn root_entry(&self) -> &PackageEntry<P> {
        self.packages
            .get(&self.root)
            .expect("workspace always contains its root")
    }
}

#[derive(Clone)]
pub struct PubOpRecord {
    pub name: OperatorDispatchKey,
    pub body: PubOpRecordBody,
}

#[derive(Clone)]
pub enum PubOpRecordBody {
    Normal {
        pattern: Vec<OpPart>,
        function: crate::ast::LexicalCallablePath,
    },
    VariadicOperator {
        open: Vec<String>,
        spec: Box<VariadicSpec>,
    },
}

pub fn collect_pub_ops(modules: &[(PathBuf, Module<Surface>)]) -> Vec<PubOpRecord> {
    let mut records = Vec::new();
    for (_, module) in modules {
        for item in &module.items {
            if let Item::Op(d, _) = item
                && d.vis.is_pub()
            {
                let crate::ast::OpBody::Normal { pattern, function } = &d.body;
                records.push(PubOpRecord {
                    name: OperatorDispatchKey::from_pattern(pattern),
                    body: PubOpRecordBody::Normal {
                        pattern: pattern.clone(),
                        function: function.clone(),
                    },
                });
            } else if let Item::VariadicOperator(d, _) = item
                && d.vis.is_pub()
            {
                records.push(PubOpRecord {
                    name: OperatorDispatchKey::from_variadic(d),
                    body: PubOpRecordBody::VariadicOperator {
                        open: d.open.clone(),
                        spec: d.spec.clone(),
                    },
                });
            }
        }
    }
    records
}

#[derive(Debug)]
pub struct ParsedPackage {
    pub root_dir: PathBuf,
    pub modules: Vec<(PathBuf, Module<Surface>)>,
    pub lazy_modules: BTreeMap<PathBuf, crate::pass::parser::LazyModule>,
    pub package_file: Option<PackageFileEntry<Surface>>,
    /// Parsed `<local>.dep.kio` declarations found at this package's
    /// root. Like `<pkg>.sig.kio`, dependency files are not modules and
    /// are kept out of `modules`. Empty unless the package declares a
    /// dependency. The walk only *parses* these; nothing in resolution,
    /// typecheck, or codegen consumes them. The command-level
    /// [`materialize_dependencies`] step reads them before analysis,
    /// re-roots each dependency's modules, and writes the result to disk
    /// under the consumer's package root — after which the dependency's
    /// modules are ordinary `.kio` files the walk picks up like any
    /// hand-written module, so the compiler proper stays
    /// dependency-agnostic.
    pub dep_files: Vec<(PathBuf, crate::ast::DependencyFile<Surface>)>,
    pub sources: BTreeMap<PathBuf, String>,
}

impl ParsedPackage {
    /// Prefer an error from a deferred body over `later_error`.
    ///
    /// Package collection deliberately leaves bodies lazy. When a header-only
    /// validation has already found a later-pipeline error, this failure-path
    /// probe preserves Parse before every later pipeline tier without forcing
    /// any body on the successful, cacheable path.
    #[cfg(feature = "cli")]
    pub(crate) fn prefer_deferred_body_error(&self, later_error: LocatedError) -> LocatedError {
        for (file_path, _) in &self.modules {
            let Some(lazy) = self.lazy_modules.get(file_path) else {
                continue;
            };
            if let Err(error) = lazy.force_all() {
                return LocatedError {
                    file_path: file_path.clone(),
                    error,
                };
            }
        }
        later_error
    }
}

#[derive(Debug)]
pub struct ParsedPackageCollection {
    pub root: PackageKey,
    pub packages: BTreeMap<PackageKey, ParsedPackage>,
}

#[cfg(feature = "cli")]
pub fn walk(cwd: &Path) -> Result<ParsedPackageCollection, (WalkError, BTreeMap<PathBuf, String>)> {
    walk_with_overlay(cwd, &SourceOverlay::empty())
}

#[cfg(feature = "cli")]
pub fn walk_with_overlay(
    cwd: &Path,
    overlay: &SourceOverlay,
) -> Result<ParsedPackageCollection, (WalkError, BTreeMap<PathBuf, String>)> {
    let root = match overlay.complete_root_for(cwd) {
        Some(root) => root,
        None => canonicalize(cwd).map_err(|source| {
            (
                WalkError::CanonicalizeRoot {
                    path: cwd.to_path_buf(),
                    source,
                },
                BTreeMap::new(),
            )
        })?,
    };
    match parse_package_files(&root, overlay) {
        Ok(parsed) => {
            let package_name = parsed
                .package_file
                .as_ref()
                .map(|entry| entry.package_name.clone())
                .or_else(|| {
                    parsed.modules.first().and_then(|(_, module)| {
                        module.path.segments.first().map(|s| s.name.clone())
                    })
                })
                .unwrap_or_else(|| {
                    root.file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("pkg")
                        .to_owned()
                });
            let key = PackageKey {
                canonical_dir: root,
                package_name,
            };
            let mut packages = BTreeMap::new();
            packages.insert(key.clone(), parsed);
            Ok(ParsedPackageCollection {
                root: key,
                packages,
            })
        }
        Err(error) => Err((error, BTreeMap::new())),
    }
}

/// Report whether `cwd`'s source tree contains at least one `.kio`
/// file (including `*.pkg.kio` package files and root
/// modules). Used by `kio check` / `kio test` to distinguish a
/// truly-empty directory — which is a usage error when no positional
/// selector was given — from a directory that has source but no
/// modules or no equivs.
///
/// Returns `Err` if the directory scan itself fails (e.g. an I/O
/// error); the caller proceeds to [`walk`] in that case so the genuine
/// diagnostic surfaces rather than a misleading "no `.kio` files"
/// message.
#[cfg(feature = "cli")]
pub fn has_kio_files(cwd: &Path) -> Result<bool, WalkError> {
    let root = canonicalize(cwd).map_err(|source| WalkError::CanonicalizeRoot {
        path: cwd.to_path_buf(),
        source,
    })?;
    let mut files = Vec::new();
    collect_kio_files(&root, &root, &mut files)?;
    Ok(!files.is_empty())
}

#[cfg(feature = "cli")]
fn parse_package_files(root: &Path, overlay: &SourceOverlay) -> Result<ParsedPackage, WalkError> {
    let files = if overlay.is_complete_for(root) {
        overlay.kio_files_under(root)
    } else {
        let mut files = Vec::new();
        collect_kio_files(root, root, &mut files)?;
        files.sort();
        files
    };

    let mut package_files = Vec::new();
    let mut modules = Vec::new();
    let mut lazy_modules = BTreeMap::new();
    let mut dep_files = Vec::new();
    let mut sources = BTreeMap::new();

    for path in files {
        let source = overlay.read(&path).map_err(|source| WalkError::ReadFile {
            path: path.clone(),
            source,
        })?;
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        sources.insert(path.clone(), source.clone());

        if crate::file_kind::is_package_file(filename) {
            if path.parent() == Some(root) {
                let package_name = crate::file_kind::package_stem(filename)
                    .unwrap_or(filename)
                    .to_owned();
                let package_file =
                    crate::pass::parser::parse_package_file(&source, Some(&package_name)).map_err(
                        |error| WalkError::Parse {
                            path: path.clone(),
                            source_text: source,
                            error,
                        },
                    )?;
                package_files.push(PackageFileEntry {
                    file_path: path,
                    package_name,
                    package_file,
                });
            }
            continue;
        }

        if crate::file_kind::is_dep_file(filename) {
            if path.parent() == Some(root) {
                let stem = crate::file_kind::dep_stem(filename).unwrap_or(filename);
                let dependency_file =
                    crate::pass::parser::parse_dependency_file(&source, Some(stem)).map_err(
                        |error| WalkError::Parse {
                            path: path.clone(),
                            source_text: source,
                            error,
                        },
                    )?;
                dep_files.push((path, dependency_file));
            }
            continue;
        }

        if crate::file_kind::is_module_file(filename) {
            let parsed = crate::pass::parser::parse_module_file_lazy(&source).map_err(|error| {
                WalkError::Parse {
                    path: path.clone(),
                    source_text: source,
                    error,
                }
            })?;
            if let Some(lazy) = parsed.lazy {
                lazy_modules.insert(path.clone(), lazy);
            }
            modules.push((path.clone(), parsed.module));
        }
    }

    let package_file = match package_files.len() {
        0 => None,
        1 => package_files.pop(),
        _ => {
            return Err(WalkError::MultiplePackageFiles {
                root: root.to_path_buf(),
                paths: package_files
                    .into_iter()
                    .map(|entry| entry.file_path)
                    .collect(),
            });
        }
    };

    Ok(ParsedPackage {
        root_dir: root.to_path_buf(),
        modules,
        lazy_modules,
        package_file,
        dep_files,
        sources,
    })
}

/// Resolve every direct local `path` dependency declared at the package
/// rooted at `root`, re-root each one's module tree under the
/// dependency's **local name**, and **write the re-rooted module source
/// to disk** under the consumer's package root, so the ordinary package
/// walk consumes it like any hand-written module.
///
/// This is the whole of the compiler's dependency-awareness: it runs at
/// the command level (`kio dep fetch` / `kio dep update`), and the
/// resulting `<local>/…` subtree of canonical `.kio` modules is written
/// into the consumer's tree and committed there (the consumer ships its
/// dependency's materialized closure). `kio check` /
/// `kio build`, resolution,
/// typecheck, codegen, the LSP, the REPL, and `kio test` never touch a
/// dependency declaration — they walk files.
///
/// **Re-rooting.** A dependency is an ordinary Kio package. Each of its
/// module's declared paths gains the local name as a new leading
/// segment — `module app;` becomes `module <local>/app;` — and that
/// module's source is written to `<root>/<local>/app.kio`. The on-disk
/// path matches the re-rooted module path, so `Package::build`'s
/// FS-path-coherence check passes the materialized files exactly as it
/// passes hand-written ones (no skip, no special case). Re-rooting also
/// rewrites every *intra-dependency* module-path reference (an `import
/// <mod>(…);` / `import <mod> as …;` pointing at a sibling dependency module)
/// the same way, so a dependency whose modules import each other still
/// resolves; the `module` declaration and these import clauses are the
/// only module-path sites a regular module carries. The dependency's
/// `pub` items become importable to the consumer via
/// `import <local>/<mod>(<item>);`.
///
/// **Materialization is fetch-to-disk.** The re-rooted module is
/// pretty-printed ([`crate::pretty::pretty_module`]) — so the written
/// file is canonical Kio — and written under `<root>/<local>/` in the
/// consumer's source tree, where it is committed. Writing is
/// content-addressed:
/// a file already holding the identical bytes is left untouched, so
/// re-materializing does not churn timestamps when nothing changed.
///
/// Scope of this slice — **direct, host-free dependencies, local `path`
/// or remote `git`**:
///
/// - Two source origins are admitted. A local
///   [`crate::ast::SourceOrigin::Path`] is relative to the consumer's
///   package root and names the dependency's `*.pkg.kio` file. A remote
///   [`crate::ast::SourceOrigin::Git`] is fetched into the per-user cache
///   and checked out at the commit its `ref` resolves to (pinned in a
///   committed `<local>.lock.kio`); the checked-out tree then feeds the
///   same re-root logic. The git half lives in [`crate::git_dep`].
/// - A dependency that itself declares `*.dep.kio` files must have those
///   dependencies already materialized in its tree (each package
///   materializes its own); they re-root along with it. An un-materialized
///   nested dependency surfaces as an ordinary unresolved-module error, and
///   a diamond's copies collapse only through an explicit `retype`.
/// - **No sealing.** Every `pub` item in every dependency module is
///   importable by the consumer; the dependency's bridge exports place no
///   restriction on the consumer-visible surface.
/// - **No `host`/env composition.** A host-bearing dependency's items are
///   materialized as-is (they become part of the package); an explicit
///   `rehost` statement is the only rebinding mechanism.
///
/// Open-world: a `*.dep.kio` only *adds* an importable `<local>/…` root,
/// it never changes the meaning of an existing module. The one hazard is
/// the local name colliding with an existing local root module, which is
/// rejected here so `import <local>/…` resolves unambiguously to the
/// dependency.
#[cfg(feature = "cli")]
pub fn materialize_dependencies(root: &Path) -> Result<(), LocatedError> {
    materialize_dependencies_filtered(root, None, false).map(|_| ())
}

/// Whether materializing one dependency had to write anything, surfaced so
/// `kio dep fetch` can report `up to date` versus `fetched`. The underlying
/// write is content-addressed (a file already holding the identical bytes
/// is left untouched), so [`UpToDate`](Self::UpToDate) means the on-disk
/// re-rooted tree already matched, byte-for-byte, what re-rooting the lock's
/// pinned commit (or the `path` source) produces — the materialization was
/// genuinely redundant. [`Fetched`](Self::Fetched) means at least one module
/// file was (re)written or pruned, or the skip was bypassed by `--force`.
#[cfg(feature = "cli")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializeOutcome {
    /// The dependency's re-rooted tree on disk already matched the lock's
    /// intent exactly; nothing was written.
    UpToDate,
    /// The dependency was (re)materialized: a module was written or a stale
    /// one pruned, or `--force` re-materialized unconditionally.
    Fetched,
}

/// As [`materialize_dependencies`], but restricting the work to the
/// dependencies whose local name is in `only` when it is `Some`, and
/// returning each materialized dependency's [`MaterializeOutcome`] keyed by
/// local name. `None` materializes every declared dependency (what the
/// implicit build/check/test step does); `Some(names)` is `kio dep fetch
/// <name>...`, which materializes just the named dependencies. A name in
/// `only` that matches no declared dependency is the caller's to reject
/// before calling — this function silently materializes the intersection.
///
/// **Skip-if-already-materialized.** When `force` is `false`, a dependency
/// whose on-disk re-rooted tree already matches the lock's intent
/// byte-for-byte (and carries no stale module) is a no-op: it reports
/// [`MaterializeOutcome::UpToDate`] without rewriting. The match is exact —
/// it is computed by re-rooting the lock's pinned commit (for a `git`
/// dependency) or the `path` source and comparing the result against disk —
/// so the skip is sound: a stale lock, a partial / corrupt checkout, a
/// missing or extra file, or any byte difference falls through to a full
/// (re)materialization. When `force` is `true` the skip is bypassed and
/// every selected dependency is re-materialized unconditionally
/// (`kio dep fetch --force` — the drift-check refresh).
///
/// Like [`materialize_dependencies`], a present `<local>.lock.kio` is
/// honored (a locked git dependency uses its locked commit, no
/// re-resolution); re-pinning is `kio dep update`'s job, not fetch's.
#[cfg(feature = "cli")]
pub fn materialize_dependencies_filtered(
    root: &Path,
    only: Option<&std::collections::BTreeSet<String>>,
    force: bool,
) -> Result<BTreeMap<String, MaterializeOutcome>, LocatedError> {
    let root = canonicalize(root).map_err(|source| LocatedError {
        file_path: root.to_path_buf(),
        error: Error::internal(
            Span::new(0, 0),
            format!(
                "cannot resolve package root `{}`: {source}",
                DisplayPath(root)
            ),
        ),
    })?;
    let parsed = parse_package_files(&root, &SourceOverlay::empty()).map_err(|walk_err| {
        // A walk failure here is the same one analysis would hit; surface
        // it through the shared located-error mapping so the diagnostic
        // renders identically.
        walk_err.into_located()
    })?;
    if parsed.dep_files.is_empty() {
        return Ok(BTreeMap::new());
    }
    // Every discovered module's `(first-path-segment, file-path)` — the
    // collision check (below, per dependency) consults this. A module
    // discovered under a previously-materialized `<local>/…` tree carries
    // that local name as its first segment *and* lives under
    // `<root>/<local>/`, so the per-dependency check excludes a
    // dependency's own materialized modules by file location; a
    // hand-written root module `<root>/<local>.kio` is not under that
    // directory and so still collides.
    let local_modules: Vec<(String, PathBuf)> = parsed
        .modules
        .iter()
        .filter_map(|(file_path, module)| {
            module
                .path
                .segments
                .first()
                .map(|s| (s.name.clone(), file_path.clone()))
        })
        .collect();

    // Every `retype` declaration across all dependencies, so a chained
    // remap (A→B, B→C) can be flattened to its ultimate origin when a
    // module is materialized — the counterpart it names may itself be a
    // retype source whose `newtype` is gone by the time the consumer
    // checks. Collected up front because the chain spans
    // dependencies and the per-dependency materialization runs in an
    // arbitrary order.
    let all_retypes: Vec<crate::ast::RetypeDecl> = parsed
        .dep_files
        .iter()
        .flat_map(|(_p, dep)| dep.retype.iter().cloned())
        .collect();
    let mut retype_graph = RetypeGraph::build(&all_retypes);

    let mut retype_validation = RetypeValidationState::default();
    if parsed.dep_files.iter().any(|(_, dependency)| {
        only.is_none_or(|only| only.contains(&dependency.name)) && !dependency.retype.is_empty()
    }) {
        retype_validation.visibility_modules = Some(
            parsed
                .modules
                .iter()
                .map(|(path, module)| (path.clone(), retype_type_projection(module.clone())))
                .collect(),
        );
    }
    let mut outcomes: BTreeMap<String, MaterializeOutcome> = BTreeMap::new();
    for (dep_file_path, dependency) in &parsed.dep_files {
        if let Some(only) = only
            && !only.contains(&dependency.name)
        {
            continue;
        }
        let outcome = materialize_one_dependency(
            &root,
            dep_file_path,
            dependency,
            &local_modules,
            &mut retype_graph,
            &mut retype_validation,
            force,
        )?;
        outcomes.insert(dependency.name.clone(), outcome);
    }

    if let Some(modules) = retype_validation.visibility_modules.take() {
        #[cfg(feature = "surface")]
        let mut modules = modules;
        #[cfg(feature = "surface")]
        finalize_retyped_label_modules(&mut retype_validation, &mut modules, &mut outcomes, force);
        validate_retype_interface(
            &retype_validation.checks,
            &retype_validation.imports,
            &modules,
        )?;
    }
    retype_validation.imports.clear();
    for pending in retype_validation.pending.drain(..) {
        publish_materialized_dependency(
            &pending.dep_dir,
            &pending.local_name,
            pending.source_span,
            &pending.desired,
        )?;
    }

    // Discharge each `retype`'s congruence obligation now that every
    // dependency is materialized — the `to` counterpart (which may be
    // another dependency's module) is on disk regardless of the order the
    // loop visited the dependencies. Re-parse the full materialized tree
    // once and validate the snapshotted `from`-side payloads against it.
    // (A skipped, already-up-to-date dependency still registers its
    // congruence check above, so the obligation is discharged against the
    // on-disk tree whether or not this run rewrote it.)
    if !retype_validation.checks.is_empty() {
        let materialized = parse_package_files(&root, &SourceOverlay::empty())
            .map(|p| p.modules)
            .unwrap_or_default();
        let semantics = RetypeSemanticPackage::build(
            &retype_validation.checks,
            &retype_validation.source_modules,
            &materialized,
        )?;
        for (check, pairs) in retype_validation.checks.iter().zip(&semantics.pairs) {
            validate_retype_congruence(check, pairs, &semantics.package)?;
        }
    }
    Ok(outcomes)
}

/// Discharge one `retype`'s congruence obligation against the fully
/// materialized consumer tree: the `to` module must declare a same-named,
/// structurally-congruent counterpart for each retyped newtype
/// (`from ⊆ to`). A missing or incongruent counterpart is a dependency
/// error — a clean materialize-time diagnostic instead of a downstream
/// typecheck failure.
#[cfg(feature = "cli")]
fn validate_retype_congruence(
    check: &DeferredRetypeCheck,
    pairs: &[CanonicalRetypePair],
    package: &Package<RetypeSemanticPhase>,
) -> Result<(), LocatedError> {
    debug_assert_eq!(pairs.len(), check.from_newtypes.len());
    for (snapshot, pair) in check.from_newtypes.iter().zip(pairs) {
        let counterpart = &pair.counterpart;
        validate_retype_visibility_floor(
            check,
            snapshot,
            &counterpart.name,
            &counterpart.vis,
            &pair.counterpart_module,
        )?;
        // Binder shapes must agree: universal and existential lists retain
        // distinct roles, and each corresponding position fixes the kind
        // under which the payload is checked.
        if counterpart.type_params.len() != snapshot.declaration.type_params.len()
            || counterpart.existential_params.len() != snapshot.declaration.existential_params.len()
        {
            return Err(LocatedError {
                file_path: check.dep_file_path.clone(),
                error: Error::dep(
                    check.span,
                    format!(
                        "`retype` cannot map `{}`'s `newtype {}` onto `{}`'s: their type-parameter \
                         arities differ ({} universal / {} existential vs {} universal / {} \
                         existential)",
                        module_path_surface(&check.from),
                        snapshot.declaration.name,
                        module_path_surface(&check.to),
                        snapshot.declaration.type_params.len(),
                        snapshot.declaration.existential_params.len(),
                        counterpart.type_params.len(),
                        counterpart.existential_params.len(),
                    ),
                ),
            });
        }
        let kind_mismatch = [
            (
                "universal",
                snapshot.declaration.type_params.as_slice(),
                counterpart.type_params.as_slice(),
            ),
            (
                "existential",
                snapshot.declaration.existential_params.as_slice(),
                counterpart.existential_params.as_slice(),
            ),
        ]
        .into_iter()
        .find_map(|(binder_class, source_params, target_params)| {
            source_params
                .iter()
                .zip(target_params)
                .enumerate()
                .find_map(|(index, (source, target))| {
                    let source_kind = source.effective_kind();
                    let target_kind = target.effective_kind();
                    (source_kind != target_kind).then_some((
                        binder_class,
                        index + 1,
                        source_kind,
                        target_kind,
                    ))
                })
        });
        if let Some((binder_class, index, source_kind, target_kind)) = kind_mismatch {
            return Err(LocatedError {
                file_path: check.dep_file_path.clone(),
                error: Error::dep(
                    check.span,
                    format!(
                        "`retype` cannot map `{}`'s `newtype {}` onto `{}`'s: corresponding {} \
                         type-parameter kinds differ at binder {} ({} vs {})",
                        module_path_surface(&check.from),
                        snapshot.declaration.name,
                        module_path_surface(&check.to),
                        binder_class,
                        index,
                        source_kind,
                        target_kind,
                    ),
                ),
            });
        }
        if !canonical_retype_payloads_congruent(&pair.from, &pair.to, package) {
            return Err(LocatedError {
                file_path: check.dep_file_path.clone(),
                error: Error::dep(
                    check.span,
                    format!(
                        "`retype` cannot map `{}`'s `newtype {}` onto `{}`'s: their payloads \
                         are not structurally congruent",
                        module_path_surface(&check.from),
                        snapshot.declaration.name,
                        module_path_surface(&check.to),
                    ),
                ),
            });
        }
    }
    Ok(())
}

#[cfg(feature = "cli")]
fn validate_retype_visibility_floor(
    check: &DeferredRetypeCheck,
    snapshot: &RetypedNewtypeSnapshot,
    name: &str,
    visibility: &crate::ast::Visibility,
    owner: &crate::ast::ModulePath,
) -> Result<(), LocatedError> {
    if crate::pass::resolve::visibility_covers(
        visibility,
        owner,
        &snapshot.declaration.vis,
        &check.from,
    ) {
        return Ok(());
    }
    Err(LocatedError {
        file_path: check.dep_file_path.clone(),
        error: Error::dep(
            check.span,
            format!(
                "`retype` target `{}.{name}` is not visible everywhere the source \
                 `{}.{}` is visible; the rebound type must preserve the source \
                 declaration's importable surface",
                module_path_surface(owner),
                module_path_surface(&check.from),
                snapshot.declaration.name,
            ),
        ),
    })
}

#[cfg(feature = "cli")]
fn validate_retype_member_interface(
    check: &DeferredRetypeCheck,
    snapshot: &RetypedNewtypeSnapshot,
    counterpart: &crate::ast::Newtype<Surface>,
    owner: &crate::ast::ModulePath,
) -> Result<(), LocatedError> {
    use crate::pass::resolve::{intersect_visibility, visibility_covers};
    let source = &snapshot.declaration;
    for (role, source_member, target_member) in [
        ("constructor", &source.constructor, &counterpart.constructor),
        ("projector", &source.projector, &counterpart.projector),
    ] {
        let source_visibility = intersect_visibility(&source.vis, &source_member.vis);
        if matches!(source_visibility, crate::ast::Visibility::Private) {
            continue;
        }
        let source_path = format!(
            "{}.{}.{}",
            module_path_surface(&check.from),
            source.name,
            source_member.name,
        );
        let target_path = format!("{}.{}", module_path_surface(owner), counterpart.name,);
        let message = if target_member.name != source_member.name {
            Some(format!(
                "`retype` target `{target_path}` has {role} `{}`, but source \
                 `{source_path}` requires {role} `{}`; the rebound type must \
                 preserve its public and scoped member names",
                target_member.name, source_member.name,
            ))
        } else if !visibility_covers(
            &intersect_visibility(&counterpart.vis, &target_member.vis),
            owner,
            &source_visibility,
            &check.from,
        ) {
            Some(format!(
                "`retype` target {role} `{target_path}.{}` is not visible \
                 everywhere source {role} `{source_path}` is visible; the \
                 rebound type must preserve its public and scoped member access",
                target_member.name,
            ))
        } else {
            None
        };
        if let Some(message) = message {
            return Err(LocatedError {
                file_path: check.dep_file_path.clone(),
                error: Error::dep(check.span, message),
            });
        }
    }
    Ok(())
}

/// Check the declaration-interface obligations created by retyping. The ordinary
/// type projection supplies declaration metadata without checking retained
/// value imports whose providers are outside that projection.
#[cfg(feature = "cli")]
fn validate_retype_interface(
    checks: &[DeferredRetypeCheck],
    imports: &[RetypeImport],
    modules: &[(PathBuf, Module<Surface>)],
) -> Result<(), LocatedError> {
    let roots = checks
        .iter()
        .flat_map(|check| [&check.from, &check.to])
        .chain(
            imports
                .iter()
                .flat_map(|import| [&import.importer, &import.to]),
        )
        .map(module_path_surface)
        .collect();
    let projected = retype_semantic_projection(modules, &BTreeMap::new(), roots);
    let find_module = |path: &crate::ast::ModulePath| {
        projected
            .iter()
            .find(|(_, module)| module.path.segments == path.segments)
            .map(|(_, module)| module)
    };
    for check in checks {
        let Some(target) = find_module(&check.to) else {
            continue;
        };
        for snapshot in &check.from_newtypes {
            // Missing counterparts are diagnosed by semantic congruence,
            // which also validates their binder shapes and payloads.
            if let Some((counterpart, owner)) =
                resolve_retyped_counterpart(target, &snapshot.declaration.name, &projected)
            {
                validate_retype_label_counterpart(check, snapshot, &counterpart)?;
                let counterpart = counterpart.declaration();
                validate_retype_visibility_floor(
                    check,
                    snapshot,
                    &counterpart.name,
                    &counterpart.vis,
                    &owner.path,
                )?;
                validate_retype_member_interface(
                    check,
                    snapshot,
                    counterpart.as_ref(),
                    &owner.path,
                )?;
            }
        }
    }
    for import in imports {
        let Some(target) = find_module(&import.to) else {
            continue;
        };
        if resolve_retyped_counterpart(target, &import.name, &projected).is_none() {
            continue;
        }
        let check = checks
            .iter()
            .find(|check| check.from.segments == import.from.segments && check.span == import.span)
            .expect("each retype-created import has a retained source obligation");
        let snapshot = check
            .from_newtypes
            .iter()
            .find(|snapshot| snapshot.declaration.name == import.name)
            .expect("each retype-created import names a selected source newtype");
        let visibility = retype_head_visibility(target, &import.name);
        if !visibility.as_ref().is_some_and(|visibility| {
            crate::pass::resolve::is_visible(visibility, &import.importer)
        }) {
            return Err(LocatedError {
                file_path: check.dep_file_path.clone(),
                error: Error::dep(
                    check.span,
                    format!(
                        "`retype` target `{}.{}` is not importable from materialized module \
                         `{}`; the rebound type must be visible at its rewritten use",
                        module_path_surface(&import.to),
                        import.name,
                        module_path_surface(&import.importer),
                    ),
                ),
            });
        }
        validate_retype_visibility_floor(
            check,
            snapshot,
            &import.name,
            &visibility.expect("the importable head has declaration visibility"),
            &target.path,
        )?;
    }
    Ok(())
}

#[cfg(feature = "cli")]
fn validate_retype_label_counterpart(
    check: &DeferredRetypeCheck,
    source: &RetypedNewtypeSnapshot,
    target: &DeclaredRetypeNominal<'_>,
) -> Result<(), LocatedError> {
    if let Some(label) = &source.label
        && target.label().is_none()
    {
        return Err(LocatedError {
            file_path: check.dep_file_path.clone(),
            error: Error::dep(
                check.span,
                format!(
                    "`retype` of label `{label}` requires a label-generated counterpart for \
                     `{}.{}`, not an ordinary `newtype` declaration",
                    module_path_surface(&check.to),
                    source.declaration.name,
                ),
            ),
        });
    }
    Ok(())
}

#[cfg(feature = "cli")]
fn retype_head_visibility(module: &Module<Surface>, name: &str) -> Option<crate::ast::Visibility> {
    if let Some(nominal) = declared_retype_nominals(module)
        .into_iter()
        .find(|nominal| nominal.name() == name)
    {
        return Some(nominal.visibility().clone());
    }
    let mut visibility = None;
    for item in &module.items {
        crate::pass::resolve::for_each_item_declaration(item, |declaration| {
            if visibility.is_none()
                && let Some(alias) = declaration.type_alias().filter(|alias| alias.name == name)
            {
                visibility = Some(alias.vis.clone());
            }
        });
    }
    visibility
}

/// Resolve a retyped `newtype`'s counterpart in `to_module`: the
/// `newtype` named `name` it declares, or — when `to_module` was itself
/// retyped and now re-imports the name — the declaration reached by
/// following its `import <origin>(<name>)` chain to the origin module.
/// Returns the borrowed written nominal and its label-family classification with
/// its declaring module (needed to qualify its payload leaves). A re-import
/// cycle or a missing origin yields `None`.
#[cfg(feature = "cli")]
fn resolve_retyped_counterpart<'a>(
    to_module: &'a Module<Surface>,
    name: &str,
    all: &'a [(PathBuf, Module<Surface>)],
) -> Option<(DeclaredRetypeNominal<'a>, &'a Module<Surface>)> {
    let mut module = to_module;
    let mut seen: Vec<Vec<String>> = Vec::new();
    loop {
        if let Some(nominal) = declared_retype_nominals(module)
            .into_iter()
            .find(|nominal| nominal.name() == name)
        {
            return Some((nominal, module));
        }
        let key: Vec<String> = module
            .path
            .segments
            .iter()
            .map(|s| s.name.clone())
            .collect();
        if seen.contains(&key) {
            return None;
        }
        seen.push(key);
        let origin = module
            .imports
            .iter()
            .find_map(|u| match &u.kind {
                crate::ast::ImportKind::Selective { items, from }
                    if items
                        .iter()
                        .filter_map(crate::ast::ImportItem::as_name)
                        .any(|item| item == name) =>
                {
                    Some(from)
                }
                _ => None,
            })
            .or_else(|| identity_reexport_origin(module, name))?;
        module = all
            .iter()
            .find(|(_p, m)| m.path.segments == origin.segments)
            .map(|(_p, m)| m)?;
    }
}

#[cfg(feature = "cli")]
fn identity_reexport_origin<'a>(
    module: &'a Module<Surface>,
    name: &str,
) -> Option<&'a crate::ast::ModulePath> {
    let alias = module.items.iter().find_map(|item| match item {
        Item::TypeAlias(alias) if alias.vis.is_pub() && alias.name == name => Some(alias),
        _ => None,
    })?;
    let Type::Path { segments, args, .. } = &alias.body else {
        return None;
    };
    let [qualifier, target_name] = segments.as_slice() else {
        return None;
    };
    if target_name.as_str() != name
        || args.len() != alias.type_params.len()
        || !args.iter().zip(&alias.type_params).all(|(arg, param)| {
            matches!(arg,
                Type::Path { segments, args, .. }
                    if args.is_empty()
                        && matches!(segments.as_slice(), [segment] if segment.as_str() == param.name)
            )
        })
    {
        return None;
    }
    module
        .imports
        .iter()
        .find_map(|import_| match &import_.kind {
            crate::ast::ImportKind::Qualified { path, alias } if alias == qualifier.as_str() => {
                Some(path)
            }
            _ => None,
        })
}

/// One declared dependency, surfaced for the `kio dep` command: its
/// `<local>.dep.kio` path and the parsed declaration (local name +
/// `source { … }` origin). This is the command-level read the `kio dep`
/// subcommand uses to enumerate, filter, and re-pin dependencies; the
/// analysis pipeline never consumes it (it walks materialized files).
#[cfg(feature = "cli")]
pub struct DeclaredDependency {
    pub dep_file_path: PathBuf,
    pub dependency: crate::ast::DependencyFile<Surface>,
}

/// Read and parse every `<local>.dep.kio` declared at the package rooted
/// at `root`, returning each one's file path and parsed declaration. The
/// canonicalized order is the file-name-sorted walk order. Empty when the
/// package declares no dependencies. Used by `kio dep fetch` / `kio dep
/// update` to enumerate the dependencies a command names (or all of them)
/// and, for `update`, reach each git origin's `(url, ref)` to re-pin.
#[cfg(feature = "cli")]
pub fn read_dependency_files(root: &Path) -> Result<Vec<DeclaredDependency>, LocatedError> {
    let root = canonicalize(root).map_err(|source| LocatedError {
        file_path: root.to_path_buf(),
        error: Error::internal(
            Span::new(0, 0),
            format!(
                "cannot resolve package root `{}`: {source}",
                DisplayPath(root)
            ),
        ),
    })?;
    let parsed = parse_package_files(&root, &SourceOverlay::empty())
        .map_err(|walk_err| walk_err.into_located())?;
    Ok(parsed
        .dep_files
        .into_iter()
        .map(|(dep_file_path, dependency)| DeclaredDependency {
            dep_file_path,
            dependency,
        })
        .collect())
}

/// The local name of every `<local>.dep.kio` dependency declared at the
/// package rooted at `root`. Each name is the leading path segment under
/// which [`materialize_dependencies`] re-roots that dependency's modules,
/// so a module path whose first segment is one of these names belongs to
/// a dependency rather than to the consumer's own source.
///
/// This is a command-level read of the dependency declarations, the same
/// place [`materialize_dependencies`] reads them; the analysis pipeline
/// stays dependency-agnostic. `kio test` calls it to scope its equiv run
/// to the consumer's own modules (see [`specs/cli.md`](../../specs/cli.md)
/// § `kio test`). Returns an empty set when the package declares no
/// dependencies.
#[cfg(feature = "cli")]
pub fn dependency_local_names(
    root: &Path,
) -> Result<std::collections::BTreeSet<String>, LocatedError> {
    let root = canonicalize(root).map_err(|source| LocatedError {
        file_path: root.to_path_buf(),
        error: Error::internal(
            Span::new(0, 0),
            format!(
                "cannot resolve package root `{}`: {source}",
                DisplayPath(root)
            ),
        ),
    })?;
    let parsed = parse_package_files(&root, &SourceOverlay::empty())
        .map_err(|walk_err| walk_err.into_located())?;
    Ok(parsed
        .dep_files
        .iter()
        .map(|(_, dependency)| dependency.name.clone())
        .collect())
}

/// Every mutable visibility handle an item carries, for re-rooting each
/// `pub(P)` restriction when a dependency is materialized under the
/// consumer. An item may carry **more than one** visibility: a `newtype`'s
/// `constructor`/`projector` members and a `rec`-group's members each carry
/// their own, and each may legally be `pub(<module-path>)` (see
/// [`specs/language.md`](../../specs/language.md) § Visibility). Returning
/// them all — not just the item's own — is what keeps a member restriction
/// re-rooted alongside the module: a missed member `vis` would keep a bare
/// pre-materialization path that no longer prefixes the re-rooted module.
#[cfg(feature = "cli")]
fn item_visibilities_mut<P: Phase>(item: &mut Item<P>) -> Vec<&mut crate::ast::Visibility> {
    match item {
        Item::FnDef(d) => vec![&mut d.vis],
        Item::TypeAlias(a) => vec![&mut a.vis],
        Item::Newtype(d) => vec![&mut d.vis, &mut d.constructor.vis, &mut d.projector.vis],
        Item::Labels(d, _) => vec![&mut d.vis],
        Item::LabelForward(d, _) => vec![&mut d.vis],
        Item::Elaborator(s, _) => vec![&mut s.vis],
        Item::Op(d, _) => vec![&mut d.vis],
        Item::VariadicOperator(d, _) => vec![&mut d.vis],
        Item::LiteralAlias(l, _) => vec![&mut l.vis],
        Item::RecGroup(g, _) => g.members.iter_mut().map(|m| &mut m.vis).collect(),
        Item::TypeRecGroup(g) => g
            .members
            .iter_mut()
            .flat_map(|member| match member {
                crate::ast::TypeRecMember::TypeAlias(alias) => vec![&mut alias.vis],
                crate::ast::TypeRecMember::Newtype(newtype) => vec![
                    &mut newtype.vis,
                    &mut newtype.constructor.vis,
                    &mut newtype.projector.vis,
                ],
                crate::ast::TypeRecMember::Labels(labels, _) => vec![&mut labels.vis],
            })
            .collect(),
        Item::Equiv(_, _) | Item::HostType(_) | Item::HostFn(_) => Vec::new(),
    }
}

/// Prepend the dependency's local name as a new leading segment of a
/// module path, namespacing it under the dependency. Applied to a
/// dependency module's own declaration (`module app;` → `<local>/app`)
/// and to each intra-dependency module-path reference (`import helper(…);`
/// → `import <local>/helper(…);`) so a re-rooted module's
/// references still resolve to its sibling modules.
#[cfg(feature = "cli")]
fn reroot_module_path(path: &mut crate::ast::ModulePath, local_name: &str) {
    path.segments.insert(
        0,
        crate::ast::PathSegment {
            name: local_name.to_owned(),
            span: path.span,
        },
    );
}

#[cfg(feature = "cli")]
fn reroot_visibility(visibility: &mut crate::ast::Visibility, local_name: &str) {
    if let crate::ast::Visibility::PublicIn(path) = visibility {
        reroot_module_path(path, local_name);
    }
}

#[cfg(feature = "cli")]
fn reroot_newtype_visibilities(newtype: &mut crate::ast::Newtype<Surface>, local_name: &str) {
    reroot_visibility(&mut newtype.vis, local_name);
    reroot_visibility(&mut newtype.constructor.vis, local_name);
    reroot_visibility(&mut newtype.projector.vis, local_name);
}

/// A resolved `rehost` target: the re-rooted dependency module `from`
/// and the consumer module `to` that provides replacements for its host
/// items. The rewrite is purely local — it rebinds `from`'s own host
/// declarations onto `to` (see [`apply_rehost`]) and touches no other
/// module — so it needs only the two paths, not a cross-module census of
/// the host names involved.
#[cfg(feature = "cli")]
struct RehostTarget {
    from: crate::ast::ModulePath,
    to: crate::ast::ModulePath,
    span: crate::span::Span,
}

#[cfg(feature = "cli")]
fn validate_rehost_selectors(declarations: &[crate::ast::RehostDecl]) -> Result<(), Error> {
    let mut first_selectors = BTreeMap::new();
    for declaration in declarations {
        let key = declaration
            .from
            .segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>();
        if let Some(first_span) = first_selectors.insert(key, declaration.span) {
            let source = module_path_surface(&declaration.from);
            return Err(Error::dep(
                declaration.span,
                format!("`rehost` source module `{source}` is selected more than once"),
            )
            .with_secondary(first_span, format!("`{source}` was first selected here"))
            .with_help(
                "each dependency module may be selected by one `rehost` statement; remove \
                 the repeated selector or select disjoint modules",
            ));
        }
    }
    Ok(())
}

/// Apply a dependency's `rehost` statements to one re-rooted module.
///
/// The rewrite is **local**: it touches only the module named by a
/// `rehost <from> to <to>`'s `from`, and leaves every other module —
/// including ones that import this module's rebound items — untouched.
/// Each `host` declaration the `from` module makes is replaced by an
/// ordinary item that forwards to the consumer's provider `to`:
///
/// - a `host type T` becomes `pub type T = <p>.T` (a transparent alias to
///   the provider's type, where `<p>` is a fresh qualified import of `to`).
///   The provider type's `role(...)`, if any, is inherited by the alias at
///   typecheck time (see [`register_alias_inherited_roles`]), so a literal
///   the original host type received still resolves through the alias.
/// - a `host fn f(..) -> R` becomes `pub fn f(..) -> R { <p>.f(..) }` (a
///   one-layer forwarding wrapper).
///
/// Because the rewritten items are *exported declarations* (not imports),
/// a module that does `import <from>(T);` resolves the name through the
/// alias / wrapper with no redirect — which is why the importer-rewrite
/// the old drop-and-redirect form needed is gone. The `to` path is a
/// consumer module written as-is: this runs *after* re-rooting precisely
/// so the injected qualified import is not itself re-rooted under the
/// dependency.
#[cfg(feature = "cli")]
fn apply_rehost(module: &mut crate::ast::Module, targets: &[RehostTarget]) {
    use crate::ast::{Import, ImportKind, Item};

    // At most one `rehost` names this module as its `from`.
    let Some(target) = targets
        .iter()
        .find(|t| t.from.segments == module.path.segments)
    else {
        return;
    };
    if !module
        .items
        .iter()
        .any(|item| matches!(item, Item::HostType(_) | Item::HostFn(_)))
    {
        return;
    }

    let provider_alias = fresh_provider_alias(module, &target.to);
    for item in &mut module.items {
        match item {
            Item::HostType(h) => *item = Item::TypeAlias(host_type_to_alias(h, &provider_alias)),
            Item::HostFn(h) => *item = Item::FnDef(host_fn_to_wrapper(h, &provider_alias)),
            _ => {}
        }
    }
    module.imports.push(Import {
        trailing_trivia: Vec::new(),
        kind: ImportKind::Qualified {
            path: target.to.clone(),
            alias: provider_alias,
        },
        span: target.span,
        leading_trivia: Vec::new(),
    });
}

/// A qualified-import alias for the provider module `to` that does not
/// collide with any name already bound in `module` — a top-level
/// declaration or another import alias. The base is the provider path's
/// exact path bytes encoded as a lowercase word; on a clash an `_nN`
/// freshness word is appended until the name is free, so the injected
/// `import <to> as <alias>;` resolves unambiguously regardless of what the
/// rehosted dependency module happens to name.
#[cfg(feature = "cli")]
fn fresh_provider_alias(module: &crate::ast::Module, to: &crate::ast::ModulePath) -> String {
    use crate::ast::{ImportItem, ImportKind, Item};

    let mut bound: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for item in &module.items {
        match item {
            Item::FnDef(d) => {
                bound.insert(d.name.as_str());
            }
            Item::RecGroup(group, _) => {
                for member in &group.members {
                    bound.insert(member.name.as_str());
                }
            }
            Item::TypeRecGroup(group) => {
                for member in &group.members {
                    match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => {
                            bound.insert(alias.name.as_str());
                        }
                        crate::ast::TypeRecMember::Newtype(newtype) => {
                            bound.insert(newtype.name.as_str());
                        }
                        crate::ast::TypeRecMember::Labels(labels, _) => {
                            if let Some(name) = &labels.type_alias_name {
                                bound.insert(name.as_str());
                            }
                        }
                    }
                }
            }
            Item::TypeAlias(a) => {
                bound.insert(a.name.as_str());
            }
            Item::LiteralAlias(a, _) => {
                bound.insert(a.name.as_str());
            }
            Item::Newtype(d) => {
                bound.insert(d.name.as_str());
            }
            Item::HostType(h) => {
                bound.insert(h.name.as_str());
            }
            Item::HostFn(h) => {
                bound.insert(h.name.as_str());
            }
            Item::Elaborator(s, _) => {
                bound.insert(s.name.as_str());
            }
            // Label spellings, equiv, and fixed or variadic operator declarations
            // occupy namespaces distinct from ordinary module aliases.
            Item::Labels(_, _)
            | Item::LabelForward(_, _)
            | Item::Equiv(_, _)
            | Item::Op(_, _)
            | Item::VariadicOperator(_, _) => {}
        }
    }
    for u in &module.imports {
        match &u.kind {
            ImportKind::Selective { items, .. } => {
                for it in items {
                    if let ImportItem::Name { name, .. } = it {
                        bound.insert(name.as_str());
                    }
                }
            }
            ImportKind::Qualified { alias, .. } => {
                bound.insert(alias.as_str());
            }
            ImportKind::Intrinsics | ImportKind::Comptime => {}
        }
    }

    let path = to
        .segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("/");
    let base = format!("_rehost_{}", crate::naming::encode_name_component(&path));
    if !bound.contains(base.as_str()) {
        return base;
    }
    let mut n = 2u32;
    loop {
        let candidate = crate::naming::indexed_name(&base, n);
        if !bound.contains(candidate.as_str()) {
            return candidate;
        }
        n += 1;
    }
}

/// Rewrite a `host type T[..]` into the transparent alias
/// `pub type T[..] = <provider>.T(..)`. The alias keeps the host type's
/// own type parameters and applies them to the provider's type so a
/// parametric host type (`host type Box[T]`) rebinds correctly
/// (`pub type Box[T] = <provider>.Box(T)`).
#[cfg(feature = "cli")]
fn host_type_to_alias(h: &crate::ast::HostType, provider_alias: &str) -> crate::ast::TypeAlias {
    let span = h.meta.span;
    let args = h
        .type_params
        .iter()
        .map(|tp| crate::ast::Type::synth_path(vec![tp.name.clone()], Vec::new(), span))
        .collect();
    let body =
        crate::ast::Type::synth_path(vec![provider_alias.to_owned(), h.name.clone()], args, span);
    crate::ast::TypeAlias {
        vis: crate::ast::Visibility::Public,
        name: h.name.clone(),
        name_span: span,
        type_params: h.type_params.clone(),
        body,
        meta: crate::ast::Meta::new(span),
        editable_span: None,
        doc: None,
    }
}

/// Rewrite a `host fn f(..) -> R` into the forwarding wrapper
/// `pub fn f(..) -> R { <provider>.f(..) }`. The wrapper mirrors the host
/// fn's signature — value parameters keep their names (a name is
/// synthesized for any anonymous slot) and types, type-binder groups are
/// preserved — and its body re-applies every binder and parameter to the
/// provider's function, so a polymorphic host fn (`host fn loop[S][R](..)`)
/// rebinds with its quantification intact.
#[cfg(feature = "cli")]
fn host_fn_to_wrapper(h: &crate::ast::HostFn, provider_alias: &str) -> crate::ast::FnDef {
    use crate::ast::{
        CallArg, Expr, FnDef, HostFnParam, Meta, Param, Purity, Signature, SignatureParam, Type,
        Visibility,
    };

    let span = h.meta.span;
    let mut sig_params = Vec::with_capacity(h.params.len());
    let mut call_args = Vec::with_capacity(h.params.len());
    for (i, p) in h.params.iter().enumerate() {
        match p {
            HostFnParam::Type(tp) => {
                sig_params.push(SignatureParam::Type(tp.clone()));
                call_args.push(CallArg::Type(Type::synth_path(
                    vec![tp.name.clone()],
                    Vec::new(),
                    span,
                )));
            }
            HostFnParam::Value(vp) => {
                let name = vp.name.clone().unwrap_or_else(|| format!("p{i}"));
                sig_params.push(SignatureParam::Value(Param {
                    name: name.clone(),
                    ty: Some(vp.ty.clone()),
                    pattern: None,
                    meta: vp.meta.clone(),
                }));
                call_args.push(CallArg::Value(Expr::Path {
                    occurrence: Default::default(),
                    segments: vec![crate::ast::PathSegment::synth(name, span)],
                    meta: Meta::new(span),
                    ext: (),
                }));
            }
        }
    }
    let callee = Expr::Path {
        occurrence: Default::default(),
        segments: vec![
            crate::ast::PathSegment::synth(provider_alias.to_owned(), span),
            crate::ast::PathSegment::synth(h.name.clone(), span),
        ],
        meta: Meta::new(span),
        ext: (),
    };
    let body = Expr::Call {
        occurrence: Default::default(),
        callee: Box::new(callee),
        args: call_args,
        meta: Meta::new(span),
        ext: (),
    };
    FnDef {
        vis: Visibility::Public,
        purity: Purity::Impure,
        name: h.name.clone(),
        sig: Signature::from_parts(sig_params, h.param_groups.clone()),
        ret: h.ret.clone(),
        ret_elided: false,
        body,
        meta: Meta::new(span),
        doc: None,
    }
}

/// Build the visibility-preserving re-export alias
/// `pub(<scope>) type <name>[..] = <counterpart_alias>.<name>(..)` a
/// `retype` injects so a remapped `newtype` remains importable from exactly
/// the source declaration's re-rooted scope. The alias is transparent (it
/// unfolds to the counterpart's type), so it preserves the counterpart's
/// identity-exact nominal — a consumer importing `<name>` from this module
/// gets the one shared type, not a fresh nominal. A parametric newtype
/// re-exports as `pub type Box[A] = <alias>.Box(A)`, applying its own
/// binders to the counterpart (mirroring [`host_type_to_alias`]); the
/// universal-binder arity matches the counterpart's by construction
/// ([`validate_retype_congruence`] rejects a mismatch).
#[cfg(feature = "cli")]
fn reexport_type_alias(
    name: &str,
    type_params: &[crate::ast::TypeParam],
    visibility: crate::ast::Visibility,
    counterpart_alias: &str,
    span: crate::span::Span,
) -> crate::ast::TypeAlias {
    let args = type_params
        .iter()
        .map(|tp| crate::ast::Type::synth_path(vec![tp.name.clone()], Vec::new(), span))
        .collect();
    let body = crate::ast::Type::synth_path(
        vec![counterpart_alias.to_owned(), name.to_owned()],
        args,
        span,
    );
    crate::ast::TypeAlias {
        vis: visibility,
        name: name.to_owned(),
        name_span: span,
        type_params: type_params.to_vec(),
        body,
        meta: crate::ast::Meta::new(span),
        editable_span: None,
        doc: None,
    }
}

/// The `/`-joined surface spelling of a module path, for diagnostics.
#[cfg(feature = "cli")]
fn module_path_surface(path: &crate::ast::ModulePath) -> String {
    path.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// A resolved `retype` target: the re-rooted dependency module `from`, the
/// newtype names it declares that this statement remaps, and the consumer
/// module `to` that holds the same-named counterparts. Computed once per
/// dependency so the rewrite can both drop a module's matched newtype
/// declarations and redirect *other* modules' imports of those names to
/// the counterpart module.
#[cfg(feature = "cli")]
struct RetypeTarget {
    from: crate::ast::ModulePath,
    newtype_names: std::collections::BTreeSet<String>,
    to: crate::ast::ModulePath,
    span: crate::span::Span,
}

#[cfg(all(test, feature = "cli"))]
fn declared_newtypes(module: &crate::ast::Module<Surface>) -> Vec<&crate::ast::Newtype<Surface>> {
    let mut newtypes = Vec::new();
    for item in &module.items {
        crate::pass::resolve::for_each_item_declaration(item, |declaration| {
            if let Some(newtype) = declaration.newtype() {
                newtypes.push(newtype);
            }
        });
    }
    newtypes
}

#[cfg(all(test, feature = "cli"))]
fn declared_newtype<'a>(
    module: &'a crate::ast::Module<Surface>,
    name: &str,
) -> Option<&'a crate::ast::Newtype<Surface>> {
    declared_newtypes(module)
        .into_iter()
        .find(|newtype| newtype.name == name)
}

/// An ordinary nominal declaration or the explicit label entry that creates it.
#[cfg(feature = "cli")]
#[derive(Clone, Debug)]
enum DeclaredRetypeNominal<'a> {
    Newtype(&'a crate::ast::Newtype<Surface>),
    #[cfg(feature = "surface")]
    Label {
        owner: &'a crate::ast::Labels<Surface>,
        entry: &'a crate::ast::LabelEntry<Surface>,
        name: String,
    },
}

#[cfg(feature = "cli")]
impl DeclaredRetypeNominal<'_> {
    fn name(&self) -> &str {
        match self {
            Self::Newtype(newtype) => &newtype.name,
            #[cfg(feature = "surface")]
            Self::Label { name, .. } => name,
        }
    }

    fn visibility(&self) -> &crate::ast::Visibility {
        match self {
            Self::Newtype(newtype) => &newtype.vis,
            #[cfg(feature = "surface")]
            Self::Label { owner, .. } => &owner.vis,
        }
    }

    fn label(&self) -> Option<&str> {
        match self {
            Self::Newtype(_) => None,
            #[cfg(feature = "surface")]
            Self::Label { entry, .. } => Some(&entry.name),
        }
    }

    fn declaration(&self) -> std::borrow::Cow<'_, crate::ast::Newtype<Surface>> {
        match self {
            Self::Newtype(newtype) => std::borrow::Cow::Borrowed(newtype),
            #[cfg(feature = "surface")]
            Self::Label { owner, entry, name } => {
                std::borrow::Cow::Owned(crate::pass::label_elab::label_nominal_declaration(
                    owner,
                    entry,
                    name.clone(),
                    entry.payload.clone(),
                ))
            }
        }
    }

    fn snapshot(&self) -> RetypedNewtypeSnapshot {
        RetypedNewtypeSnapshot {
            declaration: self.declaration().into_owned(),
            label: self.label().map(str::to_owned),
        }
    }

    fn reexport(&self, counterpart_alias: &str) -> crate::ast::TypeAlias<Surface> {
        let (params, name_span, meta, doc) = match self {
            Self::Newtype(newtype) => (
                &newtype.type_params,
                newtype.name_span,
                &newtype.meta,
                &newtype.doc,
            ),
            #[cfg(feature = "surface")]
            Self::Label { owner, entry, .. } => {
                (&entry.type_params, entry.name_span, &entry.meta, &owner.doc)
            }
        };
        let mut alias = reexport_type_alias(
            self.name(),
            params,
            self.visibility().clone(),
            counterpart_alias,
            meta.span,
        );
        alias.name_span = name_span;
        alias.meta = meta.clone();
        alias.doc = doc.clone();
        alias
    }
}

#[cfg(feature = "cli")]
fn declared_retype_nominals(module: &Module<Surface>) -> Vec<DeclaredRetypeNominal<'_>> {
    let mut nominals = Vec::new();
    for item in &module.items {
        crate::pass::resolve::for_each_item_declaration(item, |declaration| {
            if let Some(newtype) = declaration.newtype() {
                nominals.push(DeclaredRetypeNominal::Newtype(newtype));
            }
            #[cfg(feature = "surface")]
            {
                use crate::pass::resolve::TopLevelDeclaration;
                let owner = match declaration {
                    TopLevelDeclaration::Item(Item::Labels(labels, _))
                    | TopLevelDeclaration::TypeRecMember(crate::ast::TypeRecMember::Labels(
                        labels,
                        _,
                    )) => labels,
                    _ => return,
                };
                for entry in owner.arms_in_source_order().flatten() {
                    if !entry.is_reuse_marker() {
                        nominals.push(DeclaredRetypeNominal::Label {
                            owner,
                            entry,
                            name: crate::ast::mint_label_newtype_name(&entry.name),
                        });
                    }
                }
            }
        });
    }
    nominals
}

/// One statement's written nominal occurrences, before output construction.
#[cfg(feature = "cli")]
#[derive(Debug)]
struct ResolvedRetypeSelection<'decl, 'module> {
    declaration: &'decl crate::ast::RetypeDecl,
    newtypes: Vec<DeclaredRetypeNominal<'module>>,
}

/// Resolve and validate this dependency file's `retype` selectors once.
///
/// The selector relation is a partial function from an exact re-rooted source
/// nominal: a module-form statement claims every newtype actually declared by
/// that module, while a per-type statement claims its one named newtype. Any
/// second claim is rejected at the later statement with the first statement as
/// secondary context, before the caller constructs or writes dependency output.
#[cfg(feature = "cli")]
fn resolve_retype_selections<'decl, 'module>(
    local_name: &str,
    declarations: &'decl [crate::ast::RetypeDecl],
    modules: &'module [(PathBuf, crate::ast::Module<Surface>)],
) -> Result<Vec<ResolvedRetypeSelection<'decl, 'module>>, Error> {
    let mut sources: BTreeMap<Vec<String>, BTreeMap<String, Vec<DeclaredRetypeNominal<'module>>>> =
        BTreeMap::new();
    for (_file_path, module) in modules {
        let key = std::iter::once(local_name.to_owned())
            .chain(
                module
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.name.clone()),
            )
            .collect();
        let mut newtypes: BTreeMap<String, Vec<DeclaredRetypeNominal<'module>>> = BTreeMap::new();
        for newtype in declared_retype_nominals(module) {
            newtypes
                .entry(newtype.name().to_owned())
                .or_default()
                .push(newtype);
        }
        sources.insert(key, newtypes);
    }

    let mut first_selectors: BTreeMap<(Vec<String>, String), crate::span::Span> = BTreeMap::new();
    let mut resolved = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        let from_key = declaration
            .from
            .segments
            .iter()
            .map(|segment| segment.name.clone())
            .collect::<Vec<_>>();
        let Some(source_newtypes) = sources.get(&from_key) else {
            return Err(Error::dep(
                declaration.span,
                format!(
                    "`retype` source module `{}` is not a module of dependency `{local_name}`",
                    module_path_surface(&declaration.from)
                ),
            ));
        };

        let selected = match &declaration.type_name {
            Some(name) => {
                let Some(newtypes) = source_newtypes.get(name).cloned() else {
                    return Err(Error::dep(
                        declaration.span,
                        format!(
                            "`retype` source module `{}` declares no `newtype {name}`",
                            module_path_surface(&declaration.from)
                        ),
                    ));
                };
                newtypes
            }
            None => source_newtypes.values().flatten().cloned().collect(),
        };

        // Selector overlap counts written retype statements, not repeated
        // declarations within one source. Those occurrences remain in the
        // snapshot and original-source legality check.
        for name in selected
            .iter()
            .map(DeclaredRetypeNominal::name)
            .collect::<BTreeSet<_>>()
        {
            let key = (from_key.clone(), name.to_owned());
            if let Some(first_span) = first_selectors.get(&key).copied() {
                let exact_name = format!("{}.{}", module_path_surface(&declaration.from), name);
                return Err(Error::dep(
                    declaration.span,
                    format!("`retype` source newtype `{exact_name}` is selected more than once"),
                )
                .with_secondary(
                    first_span,
                    format!("`{exact_name}` was first selected here"),
                )
                .with_help(
                    "each exact source newtype may be selected by one `retype` statement; remove \
                     one of the overlapping statements or use disjoint per-type selectors",
                ));
            }
            first_selectors.insert(key, declaration.span);
        }

        resolved.push(ResolvedRetypeSelection {
            declaration,
            newtypes: selected,
        });
    }
    Ok(resolved)
}

#[cfg(feature = "cli")]
#[derive(Clone)]
enum IndexedRetypeEdge {
    Target(crate::ast::ModulePath),
    /// More than one raw statement occupies this selector slot. The exact
    /// source-inventory preflight reports the paired-span user error; graph
    /// traversal merely refuses to make a source-order-dependent choice.
    Ambiguous,
}

#[cfg(feature = "cli")]
#[derive(Clone)]
enum CachedRetypeTerminal {
    Target(crate::ast::ModulePath),
    Invalid,
}

#[cfg(feature = "cli")]
enum IndexedRetypeNext {
    Target(crate::ast::ModulePath),
    End,
    Invalid,
}

/// Exact indexed `retype` graph shared by every dependency materialization.
/// Raw declarations are indexed once by module-default or per-type selector;
/// resolved `(module, name)` suffixes are memoized after their first walk.
#[cfg(feature = "cli")]
struct RetypeGraph {
    module_edges: BTreeMap<Vec<String>, IndexedRetypeEdge>,
    type_edges: BTreeMap<(Vec<String>, String), IndexedRetypeEdge>,
    terminals: BTreeMap<(Vec<String>, String), CachedRetypeTerminal>,
    #[cfg(test)]
    declaration_visits: usize,
    #[cfg(test)]
    edge_lookups: usize,
}

#[cfg(feature = "cli")]
impl RetypeGraph {
    fn build(declarations: &[crate::ast::RetypeDecl]) -> Self {
        let mut graph = Self {
            module_edges: BTreeMap::new(),
            type_edges: BTreeMap::new(),
            terminals: BTreeMap::new(),
            #[cfg(test)]
            declaration_visits: 0,
            #[cfg(test)]
            edge_lookups: 0,
        };
        for declaration in declarations {
            #[cfg(test)]
            {
                graph.declaration_visits += 1;
            }
            let module = declaration
                .from
                .segments
                .iter()
                .map(|segment| segment.name.clone())
                .collect::<Vec<_>>();
            match &declaration.type_name {
                Some(name) => Self::insert_edge(
                    &mut graph.type_edges,
                    (module, name.clone()),
                    declaration.to.clone(),
                ),
                None => Self::insert_edge(&mut graph.module_edges, module, declaration.to.clone()),
            }
        }
        graph
    }

    fn insert_edge<K: Ord>(
        edges: &mut BTreeMap<K, IndexedRetypeEdge>,
        key: K,
        target: crate::ast::ModulePath,
    ) {
        use std::collections::btree_map::Entry;
        match edges.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(IndexedRetypeEdge::Target(target));
            }
            Entry::Occupied(mut entry) => {
                entry.insert(IndexedRetypeEdge::Ambiguous);
            }
        }
    }

    fn next(&mut self, module: &[String], name: &str) -> IndexedRetypeNext {
        #[cfg(test)]
        {
            self.edge_lookups += 1;
        }
        let exact = self.type_edges.get(&(module.to_vec(), name.to_owned()));
        let module_default = self.module_edges.get(module);
        match (exact, module_default) {
            (None, None) => IndexedRetypeNext::End,
            (Some(IndexedRetypeEdge::Target(target)), None)
            | (None, Some(IndexedRetypeEdge::Target(target))) => {
                IndexedRetypeNext::Target(target.clone())
            }
            _ => IndexedRetypeNext::Invalid,
        }
    }

    /// Follow one exact counterpart to the module that ultimately declares
    /// the remapped newtype. Cycles and ambiguous raw edges fail closed to the
    /// written starting module; the materialization/congruence preflight owns
    /// their user-facing diagnostic.
    fn resolve_terminal(
        &mut self,
        start: &crate::ast::ModulePath,
        name: &str,
    ) -> crate::ast::ModulePath {
        let mut current = start.clone();
        let mut walked = Vec::new();
        let mut seen = BTreeSet::new();
        let terminal = loop {
            let key = (
                current
                    .segments
                    .iter()
                    .map(|segment| segment.name.clone())
                    .collect::<Vec<_>>(),
                name.to_owned(),
            );
            if let Some(cached) = self.terminals.get(&key) {
                break cached.clone();
            }
            if !seen.insert(key.clone()) {
                break CachedRetypeTerminal::Invalid;
            }
            walked.push(key.clone());
            match self.next(&key.0, name) {
                IndexedRetypeNext::Target(next) => current = next,
                IndexedRetypeNext::End => {
                    break CachedRetypeTerminal::Target(current);
                }
                IndexedRetypeNext::Invalid => break CachedRetypeTerminal::Invalid,
            }
        };
        for key in walked {
            self.terminals.insert(key, terminal.clone());
        }
        match terminal {
            CachedRetypeTerminal::Target(target) => target,
            CachedRetypeTerminal::Invalid => start.clone(),
        }
    }

    #[cfg(test)]
    fn work(&self) -> (usize, usize) {
        (self.declaration_visits, self.edge_lookups)
    }
}

/// Partition a set of remapped newtype names by the terminal module reached
/// by each exact identity. A later per-type hop may peel one name away from a
/// module-form hop, so one source statement can legitimately produce several
/// materialization targets.
#[cfg(feature = "cli")]
fn retype_chain_targets(
    to: &crate::ast::ModulePath,
    names: &std::collections::BTreeSet<String>,
    graph: &mut RetypeGraph,
) -> Vec<(crate::ast::ModulePath, std::collections::BTreeSet<String>)> {
    let mut targets: BTreeMap<
        Vec<String>,
        (crate::ast::ModulePath, std::collections::BTreeSet<String>),
    > = BTreeMap::new();
    for name in names {
        let origin = graph.resolve_terminal(to, name);
        let key = origin
            .segments
            .iter()
            .map(|segment| segment.name.clone())
            .collect();
        targets
            .entry(key)
            .or_insert_with(|| (origin, std::collections::BTreeSet::new()))
            .1
            .insert(name.clone());
    }
    targets.into_values().collect()
}

/// One retyped `newtype`'s `from`-side shape, snapshotted before
/// `apply_retype` drops the declaration. The congruence obligation
/// compares this against the `to`-side counterpart: the universal and
/// existential binder lists must match in arity and effective kind, and
/// the payloads must be structurally congruent modulo module paths (the
/// two binder lists seed the `Forall` alpha-equivalence so a polymorphic
/// dictionary payload `[A] A -> A` is not span-rejected).
#[cfg(feature = "cli")]
struct RetypedNewtypeSnapshot {
    declaration: crate::ast::Newtype<Surface>,
    /// Exact explicit label spelling in the retained written source.
    label: Option<String>,
}

/// A `retype`'s congruence obligation, captured during materialization for
/// validation **after every dependency is materialized**. The `to`
/// counterpart may be another dependency's module (the diamond `retype
/// <b>/<d'> to <d>;`) that is materialized *later* in the same run, so the
/// `from`-side payloads are recorded here (before the rewrite drops them)
/// and checked once the whole tree is on disk — order-independently.
#[cfg(feature = "cli")]
struct DeferredRetypeCheck {
    dep_file_path: PathBuf,
    span: crate::span::Span,
    from: crate::ast::ModulePath,
    to: crate::ast::ModulePath,
    /// Each retyped newtype's `from`-side shape, snapshotted before
    /// `apply_retype` drops the declaration.
    from_newtypes: Vec<RetypedNewtypeSnapshot>,
}

/// Deferred inputs accumulated while dependencies are materialized, then
/// discharged together once every possible counterpart is on disk.
#[cfg(feature = "cli")]
#[derive(Default)]
struct RetypeValidationState {
    checks: Vec<DeferredRetypeCheck>,
    source_modules: BTreeMap<String, (PathBuf, Module<Surface>)>,
    visibility_modules: Option<Vec<(PathBuf, Module<Surface>)>>,
    imports: Vec<RetypeImport>,
    pending: Vec<PendingRetypeMaterialization>,
}

#[cfg(feature = "cli")]
#[derive(Debug)]
struct RetypeImport {
    from: crate::ast::ModulePath,
    importer: crate::ast::ModulePath,
    to: crate::ast::ModulePath,
    name: String,
    span: Span,
}

#[cfg(feature = "cli")]
struct PendingRetypeMaterialization {
    dep_dir: PathBuf,
    local_name: String,
    source_span: Span,
    desired: BTreeMap<PathBuf, String>,
    #[cfg(feature = "surface")]
    label_modules: Vec<PendingLabelModule>,
}

#[cfg(all(feature = "cli", feature = "surface"))]
struct PendingLabelModule {
    out_path: PathBuf,
    module: Module<Surface>,
    forwards: Vec<LabelForwardSlot>,
}

#[cfg(all(feature = "cli", feature = "surface"))]
#[derive(Debug)]
struct LabelForwardSlot {
    before_item: usize,
    forward: retype_labels::PendingLabelForward,
}

#[cfg(feature = "cli")]
#[derive(Debug, Default)]
struct RetypeRewrite {
    imports: Vec<RetypeImport>,
    #[cfg(feature = "surface")]
    forwards: Vec<LabelForwardSlot>,
}

#[cfg(all(feature = "cli", feature = "surface"))]
fn finalize_retyped_label_forwards(
    module: &mut Module<Surface>,
    forwards: Vec<LabelForwardSlot>,
    all: &[(PathBuf, Module<Surface>)],
) -> Vec<RetypeImport> {
    let mut imports = Vec::new();
    let mut finished = Vec::with_capacity(forwards.len());
    for slot in forwards {
        let mut forward = slot.forward;
        let terminal = module_by_path(all, &forward.to)
            .and_then(|target| resolve_retyped_counterpart(target, &forward.nominal_name, all))
            .and_then(|(nominal, owner)| {
                nominal
                    .label()
                    .map(|label| (owner.path.clone(), label.to_owned()))
            });
        // Missing counterparts keep the written target edge and reach the
        // existing post-publication semantic check. No label is inferred
        // from an ordinary alias or an unresolved nominal name.
        let (provider, label) = terminal
            .clone()
            .unwrap_or_else(|| (forward.to.clone(), forward.declaration.name.clone()));
        let alias = module
            .imports
            .iter()
            .find_map(|import_clause| match &import_clause.kind {
                crate::ast::ImportKind::Qualified { path, alias }
                    if path.segments == provider.segments =>
                {
                    Some(alias.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| {
                let alias = fresh_provider_alias(module, &provider);
                module.imports.push(crate::ast::Import {
                    kind: crate::ast::ImportKind::Qualified {
                        path: provider.clone(),
                        alias: alias.clone(),
                    },
                    span: forward.declaration.name_span,
                    leading_trivia: Vec::new(),
                    trailing_trivia: Vec::new(),
                });
                alias
            });
        forward.declaration.target = format!("{alias}.{label}");
        if terminal.is_some() {
            imports.push(RetypeImport {
                from: module.path.clone(),
                importer: module.path.clone(),
                to: provider,
                name: forward.nominal_name,
                span: forward.retype_span,
            });
        }
        finished.push((slot.before_item, forward.declaration));
    }
    let original = std::mem::take(&mut module.items);
    let original_len = original.len();
    let mut finished = finished.into_iter().peekable();
    for (index, item) in original.into_iter().enumerate() {
        while finished.peek().is_some_and(|(before, _)| *before == index) {
            let (_, forward) = finished.next().expect("the pending forward exists");
            module.items.push(Item::LabelForward(forward, ()));
        }
        module.items.push(item);
    }
    for (before, forward) in finished {
        assert_eq!(
            before, original_len,
            "pending forwards retain final item boundaries"
        );
        module.items.push(Item::LabelForward(forward, ()));
    }
    imports
}

#[cfg(all(feature = "cli", feature = "surface"))]
fn finalize_retyped_label_modules(
    validation: &mut RetypeValidationState,
    modules: &mut [(PathBuf, Module<Surface>)],
    outcomes: &mut BTreeMap<String, MaterializeOutcome>,
    force: bool,
) {
    for mut pending in std::mem::take(&mut validation.pending) {
        let had_label_modules = !pending.label_modules.is_empty();
        for mut selected in std::mem::take(&mut pending.label_modules) {
            validation.imports.extend(finalize_retyped_label_forwards(
                &mut selected.module,
                selected.forwards,
                modules,
            ));
            let overlay = modules
                .iter_mut()
                .find(|(_, module)| module.path.segments == selected.module.path.segments)
                .expect("a deferred source module already has an overlaid type view");
            overlay.1 = borrowed_retype_type_projection(&selected.module);
            pending.desired.insert(
                selected.out_path,
                crate::pretty::pretty_module(&selected.module),
            );
        }
        if had_label_modules
            && !force
            && materialized_tree_matches(&pending.dep_dir, &pending.desired)
        {
            outcomes.insert(pending.local_name, MaterializeOutcome::UpToDate);
        } else {
            validation.pending.push(pending);
        }
    }
}

/// Apply a dependency's `retype` statements to one re-rooted module,
/// exactly as [`apply_rehost`] does for host items but for `newtype`s:
///
/// 1. If the module is itself a `retype` target, replace public/scoped
///    matched `newtype` declarations at their original slots with ordinary
///    identity aliases through a fresh qualified counterpart import. Private
///    heads are dropped and selectively imported, so the module's
///    own code (its constructors, projectors, and any payload reference)
///    binds to the one shared nominal type instead of its own re-rooted
///    copy. Re-partition each residual recursive group into its minimal
///    legal declarations. An alias-only residual cycle, or an atomic
///    `labels` owner that would have to be split across residual components,
///    is rejected as a dependency error.
/// 2. Redirect the module's imports of retyped newtypes to the
///    counterpart — an intra-dependency `import <retyped module>(<Name>)`
///    becomes `import <counterpart>(<Name>)`. Non-retyped names in the
///    same import keep their original provider.
///
/// `to` paths are consumer modules, written as-is — this runs *after*
/// re-rooting, so the injected / redirected imports are not themselves
/// re-rooted under the dependency.
#[cfg(feature = "cli")]
fn apply_retype(
    module: &mut crate::ast::Module,
    targets: &[RetypeTarget],
) -> Result<RetypeRewrite, Error> {
    use crate::ast::{Import, ImportItem, ImportKind, Item};

    let mut rewrite = RetypeRewrite::default();
    let source_targets = targets
        .iter()
        .filter(|target| {
            target.from.segments == module.path.segments && !target.newtype_names.is_empty()
        })
        .collect::<Vec<_>>();
    if !source_targets.is_empty() {
        let mut selected_names = std::collections::BTreeSet::new();
        let mut reexports = BTreeMap::new();
        #[cfg(feature = "surface")]
        let mut label_names = BTreeSet::new();
        #[cfg(feature = "surface")]
        let mut label_targets = BTreeMap::new();
        for target in &source_targets {
            selected_names.extend(target.newtype_names.iter().cloned());
            let public_names = declared_retype_nominals(module)
                .into_iter()
                .filter(|nominal| {
                    nominal.visibility().is_pub() && target.newtype_names.contains(nominal.name())
                })
                .map(|nominal| nominal.name().to_owned())
                .collect::<std::collections::BTreeSet<_>>();
            #[cfg(feature = "surface")]
            for nominal in declared_retype_nominals(module)
                .into_iter()
                .filter(|nominal| target.newtype_names.contains(nominal.name()))
            {
                if let Some(label) = nominal.label() {
                    label_names.insert(label.to_owned());
                    label_targets.insert(nominal.name().to_owned(), *target);
                }
            }
            if !public_names.is_empty() {
                let counterpart_alias = fresh_provider_alias(module, &target.to);
                module.imports.push(Import {
                    trailing_trivia: Vec::new(),
                    kind: ImportKind::Qualified {
                        path: target.to.clone(),
                        alias: counterpart_alias.clone(),
                    },
                    span: module.path.span,
                    leading_trivia: Vec::new(),
                });
                for nominal in declared_retype_nominals(module)
                    .into_iter()
                    .filter(|nominal| public_names.contains(nominal.name()))
                {
                    // Repeated source declarations remain user errors in the
                    // retained original-source check, not selector collisions.
                    reexports
                        .entry(nominal.name().to_owned())
                        .or_insert_with(|| nominal.reexport(&counterpart_alias));
                }
            }

            // Private heads have no local re-export. Public/scoped heads
            // instead retain one ordinary alias at their original slot.
            let private_imports = target
                .newtype_names
                .difference(&public_names)
                .map(|name| ImportItem::Name {
                    leading_trivia: Vec::new(),
                    name: name.clone(),
                    span: module.path.span,
                })
                .collect::<Vec<_>>();
            if !private_imports.is_empty() {
                module.imports.push(Import {
                    trailing_trivia: Vec::new(),
                    kind: ImportKind::Selective {
                        items: private_imports,
                        from: target.to.clone(),
                    },
                    span: module.path.span,
                    leading_trivia: Vec::new(),
                });
            }
            rewrite
                .imports
                .extend(target.newtype_names.iter().map(|name| RetypeImport {
                    from: target.from.clone(),
                    importer: module.path.clone(),
                    to: target.to.clone(),
                    name: name.clone(),
                    span: target.span,
                }));
        }

        #[cfg(feature = "surface")]
        let mut affected_labels = retype_labels::affected_owners(module, &label_names).into_iter();
        #[cfg(feature = "surface")]
        let mut forward_boundaries: BTreeMap<
            usize,
            Vec<retype_labels::PendingLabelForward>,
        > = BTreeMap::new();
        let mut retained = Vec::with_capacity(module.items.len());
        for item in std::mem::take(&mut module.items) {
            match item {
                Item::Newtype(newtype) if selected_names.contains(&newtype.name) => {
                    if let Some(alias) = reexports.remove(&newtype.name) {
                        retained.push(Item::TypeAlias(alias));
                    }
                }
                #[cfg(feature = "surface")]
                Item::Labels(labels, ()) => {
                    if affected_labels
                        .next()
                        .expect("one flag per written labels owner")
                    {
                        let meta = crate::ast::Meta::new(labels.meta.span);
                        let rec_span = labels.rec_span;
                        let decomposed =
                            retype_labels::decompose_owner(labels, &label_targets, &mut reexports);
                        if rec_span.is_some() && !decomposed.members.is_empty() {
                            retained.push(Item::TypeRecGroup(crate::ast::TypeRecGroup {
                                members: decomposed.members,
                                doc: None,
                                source_layout: None,
                                rec_span,
                                open_brace_span: None,
                                close_brace_span: None,
                                deferred_rec_labels_diagnostic: None,
                                meta,
                            }));
                        } else {
                            retained.extend(decomposed.members.into_iter().map(
                                |member| match member {
                                    crate::ast::TypeRecMember::TypeAlias(alias) => {
                                        Item::TypeAlias(alias)
                                    }
                                    crate::ast::TypeRecMember::Newtype(newtype) => {
                                        Item::Newtype(newtype)
                                    }
                                    crate::ast::TypeRecMember::Labels(labels, ()) => {
                                        Item::Labels(labels, ())
                                    }
                                },
                            ));
                        }
                        forward_boundaries
                            .entry(retained.len())
                            .or_default()
                            .extend(decomposed.forwards);
                    } else {
                        retained.push(Item::Labels(labels, ()));
                    }
                }
                Item::TypeRecGroup(mut group) => {
                    let mut members = Vec::with_capacity(group.members.len());
                    #[cfg(feature = "surface")]
                    let mut forwards = Vec::new();
                    for member in group.members {
                        match member {
                            crate::ast::TypeRecMember::Newtype(newtype)
                                if selected_names.contains(&newtype.name) =>
                            {
                                if let Some(alias) = reexports.remove(&newtype.name) {
                                    members.push(crate::ast::TypeRecMember::TypeAlias(alias));
                                }
                            }
                            #[cfg(feature = "surface")]
                            crate::ast::TypeRecMember::Labels(labels, ()) => {
                                if affected_labels
                                    .next()
                                    .expect("one flag per written labels owner")
                                {
                                    let decomposed = retype_labels::decompose_owner(
                                        labels,
                                        &label_targets,
                                        &mut reexports,
                                    );
                                    members.extend(decomposed.members);
                                    forwards.extend(decomposed.forwards);
                                    group.source_layout = None;
                                } else {
                                    members.push(crate::ast::TypeRecMember::Labels(labels, ()));
                                }
                            }
                            member => members.push(member),
                        }
                    }
                    group.members = members;
                    if !group.members.is_empty() {
                        retained.push(Item::TypeRecGroup(group));
                    }
                    #[cfg(feature = "surface")]
                    forward_boundaries
                        .entry(retained.len())
                        .or_default()
                        .extend(forwards);
                }
                item => retained.push(item),
            }
        }
        #[cfg(feature = "surface")]
        debug_assert!(affected_labels.next().is_none());
        debug_assert!(
            reexports.is_empty(),
            "every re-export replaces its written newtype"
        );
        module.items = retained;

        let primary = source_targets[0];
        let analyses =
            crate::pass::resolve::projected_surface_type_rec_analyses(module).map_err(|error| {
                Error::dep(
                    primary.span,
                    format!(
                        "`retype` of `{}` cannot re-partition its retained recursive type \
                         declarations: {}",
                        module_path_surface(&primary.from),
                        error.diag().1,
                    ),
                )
            })?;
        let mut analyses = analyses.into_iter();
        let mut partitioned = Vec::with_capacity(module.items.len());
        let selected = selected_names
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        for (item_index, item) in std::mem::take(&mut module.items).into_iter().enumerate() {
            #[cfg(not(feature = "surface"))]
            let _ = item_index;
            #[cfg(feature = "surface")]
            if let Some(forwards) = forward_boundaries.remove(&item_index) {
                rewrite
                    .forwards
                    .extend(forwards.into_iter().map(|forward| LabelForwardSlot {
                        before_item: partitioned.len(),
                        forward,
                    }));
            }
            let Item::TypeRecGroup(group) = item else {
                partitioned.push(item);
                continue;
            };
            let (analyzed_span, analysis) = analyses
                .next()
                .expect("one retype partition analysis per recursive type group");
            debug_assert_eq!(analyzed_span, group.meta.span);
            let Some(analysis) = analysis else {
                return Err(Error::dep(
                    primary.span,
                    format!(
                        "`retype` of `{}` cannot remove grouped newtype(s) `{selected}`: a \
                         retained indivisible `labels` declaration spans multiple recursive \
                         components",
                        module_path_surface(&primary.from),
                    ),
                ));
            };
            if let Some(alias_cycle) = analysis.cyclic_components.iter().find(|component| {
                component.iter().all(|&index| {
                    matches!(
                        group.members[index],
                        crate::ast::TypeRecMember::TypeAlias(_)
                    )
                })
            }) {
                let aliases = alias_cycle
                    .iter()
                    .filter_map(|&index| match &group.members[index] {
                        crate::ast::TypeRecMember::TypeAlias(alias) => Some(alias.name.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(Error::dep(
                    primary.span,
                    format!(
                        "`retype` of `{}` cannot remove grouped newtype(s) `{selected}`: \
                         retained type aliases `{aliases}` would form a recursive cycle",
                        module_path_surface(&primary.from),
                    ),
                ));
            }
            partitioned.extend(crate::pass::resolve::emit_type_rec_partition(
                group, &analysis,
            ));
        }
        debug_assert!(analyses.next().is_none());
        #[cfg(feature = "surface")]
        {
            for forwards in forward_boundaries.into_values() {
                rewrite
                    .forwards
                    .extend(forwards.into_iter().map(|forward| LabelForwardSlot {
                        before_item: partitioned.len(),
                        forward,
                    }));
            }
        }
        module.items = partitioned;
    }

    let mut rewritten = Vec::with_capacity(module.imports.len());
    for import_clause in module.imports.drain(..) {
        let ImportKind::Selective { items, from } = &import_clause.kind else {
            rewritten.push(import_clause);
            continue;
        };
        let matching_targets = targets
            .iter()
            .filter(|target| target.from.segments == from.segments)
            .collect::<Vec<_>>();
        if matching_targets.is_empty() {
            rewritten.push(import_clause);
            continue;
        }

        let mut destinations = BTreeMap::new();
        for target in matching_targets {
            for name in &target.newtype_names {
                destinations.entry(name.as_str()).or_insert(target);
            }
        }
        let mut group_indices: BTreeMap<Vec<String>, usize> = BTreeMap::new();
        let mut redirected: Vec<(crate::ast::ModulePath, Vec<ImportItem>)> = Vec::new();
        let mut kept = Vec::new();
        for item in items.iter().cloned() {
            let Some(target) = item.as_name().and_then(|name| destinations.get(name)) else {
                kept.push(item);
                continue;
            };
            let destination = &target.to;
            rewrite.imports.push(RetypeImport {
                from: target.from.clone(),
                importer: module.path.clone(),
                to: destination.clone(),
                name: item
                    .as_name()
                    .expect("a redirected import is a name")
                    .to_owned(),
                span: target.span,
            });
            let key = destination
                .segments
                .iter()
                .map(|segment| segment.name.clone())
                .collect::<Vec<_>>();
            let index = *group_indices.entry(key).or_insert_with(|| {
                let index = redirected.len();
                redirected.push((destination.clone(), Vec::new()));
                index
            });
            redirected[index].1.push(item);
        }
        if redirected.is_empty() {
            rewritten.push(import_clause);
            continue;
        }

        for (index, (destination, redirected_items)) in redirected.into_iter().enumerate() {
            rewritten.push(Import {
                trailing_trivia: Vec::new(),
                kind: ImportKind::Selective {
                    items: redirected_items,
                    from: destination,
                },
                span: import_clause.span,
                leading_trivia: if index == 0 {
                    import_clause.leading_trivia.clone()
                } else {
                    Vec::new()
                },
            });
        }
        if !kept.is_empty() {
            rewritten.push(Import {
                trailing_trivia: Vec::new(),
                kind: ImportKind::Selective {
                    items: kept,
                    from: from.clone(),
                },
                span: import_clause.span,
                leading_trivia: Vec::new(),
            });
        }
        rewritten
            .last_mut()
            .expect("a redirected import produced at least one group")
            .trailing_trivia = import_clause.trailing_trivia;
    }
    module.imports = rewritten;
    Ok(rewrite)
}

#[cfg(all(feature = "cli", feature = "surface"))]
type RetypeSemanticPhase = crate::ast::Lowered;
#[cfg(all(feature = "cli", not(feature = "surface")))]
type RetypeSemanticPhase = Prime;

/// Canonical, alias-free payload schemes for one `retype` pair. Header
/// binders are represented as outer `forall`s, so the shared structural
/// comparison keeps alpha-equivalence and binder kinds intact.
#[cfg(feature = "cli")]
struct CanonicalRetypePair {
    from: Type<RetypeSemanticPhase>,
    to: Type<RetypeSemanticPhase>,
    counterpart: crate::ast::Newtype<Surface>,
    counterpart_module: crate::ast::ModulePath,
}

/// One transient semantic view for every deferred `retype` check in a
/// materialization run. The source tree is projected to type declarations,
/// lowered once, and indexed with the ordinary package/nominal machinery.
/// Payloads are then canonicalized as one batch through the same transparent
/// alias materializer used by the typer.
#[cfg(feature = "cli")]
struct RetypeSemanticPackage {
    package: Package<RetypeSemanticPhase>,
    pairs: Vec<Vec<CanonicalRetypePair>>,
}

#[cfg(feature = "cli")]
struct RetypeProjectionPair {
    from_module: crate::ast::ModulePath,
    from_alias: String,
    from_params: Vec<TypeParam>,
    to_module: crate::ast::ModulePath,
    to_alias: String,
    to_params: Vec<TypeParam>,
    counterpart: crate::ast::Newtype<Surface>,
}

#[cfg(feature = "cli")]
impl RetypeSemanticPackage {
    fn build(
        checks: &[DeferredRetypeCheck],
        source_modules: &BTreeMap<String, (PathBuf, Module<Surface>)>,
        materialized: &[(PathBuf, Module<Surface>)],
    ) -> Result<Self, LocatedError> {
        validate_retype_source_semantics(source_modules, materialized)?;
        let roots = checks
            .iter()
            .flat_map(|check| [&check.from, &check.to])
            .map(module_path_surface)
            .collect();
        let mut projected = retype_semantic_projection(materialized, &BTreeMap::new(), roots);
        let mut descriptors = Vec::new();

        for check in checks {
            let Some(from_module) = module_by_path(materialized, &check.from) else {
                return Err(LocatedError {
                    file_path: check.dep_file_path.clone(),
                    error: Error::dep(
                        check.span,
                        format!(
                            "`retype` source module `{}` was not materialized",
                            module_path_surface(&check.from)
                        ),
                    ),
                });
            };
            let Some(to_module) = module_by_path(materialized, &check.to) else {
                return Err(LocatedError {
                    file_path: check.dep_file_path.clone(),
                    error: Error::dep(
                        check.span,
                        format!(
                            "`retype` target module `{}` was not found; declare it or materialize \
                             the dependency that provides it",
                            module_path_surface(&check.to)
                        ),
                    ),
                });
            };

            for snapshot in &check.from_newtypes {
                let name = &snapshot.declaration.name;
                let Some((counterpart, counterpart_module)) =
                    resolve_retyped_counterpart(to_module, name, materialized)
                else {
                    return Err(LocatedError {
                        file_path: check.dep_file_path.clone(),
                        error: Error::dep(
                            check.span,
                            format!(
                                "`retype` target module `{}` has no `newtype {name}` to match \
                                 `{}`'s — every retyped newtype needs a same-named counterpart",
                                module_path_surface(&check.to),
                                module_path_surface(&check.from),
                            ),
                        ),
                    });
                };

                validate_retype_label_counterpart(check, snapshot, &counterpart)?;
                let counterpart = counterpart.declaration();
                let index = descriptors.len();
                let from_alias = format!("__Retype_source{index}__");
                let to_alias = format!("__Retype_target{index}__");
                let from_params = retype_payload_params(&snapshot.declaration);
                let to_params = retype_payload_params(counterpart.as_ref());
                push_retype_snapshot_alias(
                    &mut projected,
                    &from_module.path,
                    &from_alias,
                    &from_params,
                    snapshot.declaration.payload.clone(),
                    check.span,
                );
                push_retype_snapshot_alias(
                    &mut projected,
                    &counterpart_module.path,
                    &to_alias,
                    &to_params,
                    counterpart.payload.clone(),
                    check.span,
                );
                descriptors.push(RetypeProjectionPair {
                    from_module: from_module.path.clone(),
                    from_alias,
                    from_params,
                    to_module: counterpart_module.path.clone(),
                    to_alias,
                    to_params,
                    counterpart: counterpart.into_owned(),
                });
            }
        }

        let lowered = lower_retype_type_projection(projected)?;
        let package = retype_projection_package(lowered)?;
        let mut pairs = checks
            .iter()
            .map(|check| Vec::with_capacity(check.from_newtypes.len()))
            .collect::<Vec<_>>();
        if descriptors.is_empty() {
            return Ok(Self { package, pairs });
        }

        let mut schemes = Vec::with_capacity(descriptors.len() * 2);
        for descriptor in &descriptors {
            schemes.push(retype_alias_scheme(
                &descriptor.from_module,
                &descriptor.from_alias,
                &descriptor.from_params,
            ));
            schemes.push(retype_alias_scheme(
                &descriptor.to_module,
                &descriptor.to_alias,
                &descriptor.to_params,
            ));
        }
        let batch_len = schemes.len();
        let batch = pack_retype_schemes(schemes);
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let ctx = crate::pass::typecheck_core::AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: Some(&package),
            binder_locals: None,
        };
        let (canonical, proven) =
            crate::pass::typecheck_core::canonicalize_deep_for_comparison(&batch, &ctx, true);
        if !proven {
            let check = checks
                .first()
                .expect("one retype check exists when a payload batch exists");
            return Err(LocatedError {
                file_path: check.dep_file_path.clone(),
                error: Error::dep(
                    check.span,
                    "`retype` payload types could not be resolved completely in the materialized package",
                ),
            });
        }
        let canonical = unpack_retype_schemes(canonical, batch_len)
            .expect("the canonicalizer preserves the synthetic product batch spine");
        let mut canonical = canonical.into_iter();
        let mut descriptor = descriptors.into_iter();
        for (check, check_pairs) in checks.iter().zip(&mut pairs) {
            for _ in &check.from_newtypes {
                let descriptor = descriptor
                    .next()
                    .expect("one semantic descriptor per retyped newtype");
                check_pairs.push(CanonicalRetypePair {
                    from: canonical
                        .next()
                        .expect("source scheme follows each descriptor"),
                    to: canonical
                        .next()
                        .expect("target scheme follows each source scheme"),
                    counterpart: descriptor.counterpart,
                    counterpart_module: descriptor.to_module,
                });
            }
        }
        debug_assert!(descriptor.next().is_none());
        debug_assert!(canonical.next().is_none());
        Ok(Self { package, pairs })
    }
}

/// Validate the original type declarations of every module that contributes a
/// `retype` source. The rewritten materialized view deliberately removes those
/// newtypes, so it cannot prove that their payloads were legal where they were
/// written. The type-only package retains the complete provider graph while
/// the ordinary resolver and module signature checker remain the authorities
/// for source order, recursive scope, kinds, visibility, and positivity.
#[cfg(feature = "cli")]
fn validate_retype_source_semantics(
    source_modules: &BTreeMap<String, (PathBuf, Module<Surface>)>,
    materialized: &[(PathBuf, Module<Surface>)],
) -> Result<(), LocatedError> {
    if source_modules.is_empty() {
        return Ok(());
    }

    let materialized_paths = materialized
        .iter()
        .map(|(_path, module)| module_path_surface(&module.path))
        .collect::<BTreeSet<_>>();
    if let Some((missing, (file_path, _module))) = source_modules
        .iter()
        .find(|(path, _module)| !materialized_paths.contains(*path))
    {
        return Err(LocatedError {
            file_path: file_path.clone(),
            error: Error::internal(
                Span::new(0, 0),
                format!("retype source module `{missing}` was not materialized"),
            ),
        });
    }
    let roots = source_modules.keys().cloned().collect();
    let projected = retype_semantic_projection(materialized, source_modules, roots);

    let lowered = lower_retype_type_projection(projected)
        .map_err(|error| retype_source_error(source_modules, error))?;
    let package = retype_projection_package(lowered)
        .map_err(|error| retype_source_error(source_modules, error))?;
    // Provider modules stay in the package as resolution context, but only
    // original source modules are checked. The projection contains no value
    // bodies, so this cannot claim an unrelated consumer or counterpart body
    // error for `dep fetch`.
    for (module_path, entry) in package.modules() {
        if !source_modules.contains_key(module_path) {
            continue;
        }
        crate::pass::resolve::Resolver::check_module(&entry.module)
            .map_err(|error| LocatedError {
                file_path: entry.file_path.clone(),
                error,
            })
            .map_err(|error| retype_source_error(source_modules, error))?;
        crate::pass::typecheck_core::check_module_signatures(&package, entry)
            .map_err(|error| retype_source_error(source_modules, error))?;
    }
    Ok(())
}

#[cfg(feature = "cli")]
fn retype_source_error(
    source_modules: &BTreeMap<String, (PathBuf, Module<Surface>)>,
    error: LocatedError,
) -> LocatedError {
    if !source_modules
        .values()
        .any(|(source_path, _module)| source_path == &error.file_path)
    {
        return error;
    }
    let mut diagnostic = error.error.diagnostic().clone();
    diagnostic.message = format!("in a `retype` source module: {}", diagnostic.message);
    LocatedError {
        file_path: error.file_path,
        error: Error::Dep(diagnostic),
    }
}

#[cfg(feature = "cli")]
fn module_by_path<'a, P: Phase>(
    modules: &'a [(PathBuf, Module<P>)],
    path: &crate::ast::ModulePath,
) -> Option<&'a Module<P>> {
    modules
        .iter()
        .find(|(_file, module)| module.path.segments == path.segments)
        .map(|(_file, module)| module)
}

#[cfg(feature = "cli")]
fn retype_payload_params(newtype: &crate::ast::Newtype<Surface>) -> Vec<TypeParam> {
    newtype
        .type_params
        .iter()
        .chain(&newtype.existential_params)
        .cloned()
        .collect()
}

#[cfg(feature = "cli")]
fn retype_type_projection(mut module: Module<Surface>) -> Module<Surface> {
    module.items.retain(retype_type_item);
    module
}

#[cfg(feature = "cli")]
fn retype_type_item(item: &Item<Surface>) -> bool {
    matches!(
        item,
        Item::TypeRecGroup(_)
            | Item::TypeAlias(_)
            | Item::Newtype(_)
            | Item::Labels(..)
            | Item::LabelForward(..)
            | Item::HostType(_)
    )
}

#[cfg(all(feature = "cli", feature = "surface"))]
fn borrowed_retype_type_projection(module: &Module<Surface>) -> Module<Surface> {
    Module {
        path: module.path.clone(),
        imports: module.imports.clone(),
        items: module
            .items
            .iter()
            .filter(|item| retype_type_item(item))
            .cloned()
            .collect(),
        meta: module.meta.clone(),
        doc: module.doc.clone(),
    }
}

/// Project the exact transitive import closure that can affect a `retype`
/// endpoint's type semantics. Kio module bodies cannot name a different
/// module without a selective or qualified import, so an ambient module
/// outside this closure cannot participate in resolution. Keeping it out of
/// the transient package also preserves open-world behavior: adding an
/// unrelated module cannot make an existing `retype` fail.
///
/// `overrides` supplies the retained pre-rewrite source modules to the source
/// legality pass. Every other selected module is reduced to its type surface.
#[cfg(feature = "cli")]
fn retype_semantic_projection(
    materialized: &[(PathBuf, Module<Surface>)],
    overrides: &BTreeMap<String, (PathBuf, Module<Surface>)>,
    roots: BTreeSet<String>,
) -> Vec<(PathBuf, Module<Surface>)> {
    let by_path = materialized
        .iter()
        .enumerate()
        .map(|(index, (_path, module))| (module_path_surface(&module.path), index))
        .collect::<BTreeMap<_, _>>();
    let mut reachable = BTreeSet::new();
    let mut pending = roots.into_iter().collect::<Vec<_>>();

    while let Some(path) = pending.pop() {
        if !reachable.insert(path.clone()) {
            continue;
        }
        let Some(&index) = by_path.get(&path) else {
            continue;
        };
        let module = overrides
            .get(&path)
            .map(|(_file_path, module)| module)
            .unwrap_or(&materialized[index].1);
        for import_clause in &module.imports {
            let target = match &import_clause.kind {
                crate::ast::ImportKind::Selective { from, .. } => from,
                crate::ast::ImportKind::Qualified { path, .. } => path,
                crate::ast::ImportKind::Intrinsics | crate::ast::ImportKind::Comptime => continue,
            };
            let target = module_path_surface(target);
            if by_path.contains_key(&target) && !reachable.contains(&target) {
                pending.push(target);
            }
        }
    }

    materialized
        .iter()
        .filter_map(|(path, module)| {
            let key = module_path_surface(&module.path);
            reachable.contains(&key).then(|| {
                overrides.get(&key).map_or_else(
                    || (path.clone(), retype_type_projection(module.clone())),
                    |(source_path, source_module)| (source_path.clone(), source_module.clone()),
                )
            })
        })
        .collect()
}

#[cfg(feature = "cli")]
fn push_retype_snapshot_alias(
    modules: &mut [(PathBuf, Module<Surface>)],
    module_path: &crate::ast::ModulePath,
    name: &str,
    params: &[TypeParam],
    body: Type<Surface>,
    span: Span,
) {
    let module = modules
        .iter_mut()
        .find(|(_file, module)| module.path.segments == module_path.segments)
        .map(|(_file, module)| module)
        .expect("a retype snapshot owner was copied into the type projection");
    module.items.push(Item::TypeAlias(crate::ast::TypeAlias {
        vis: crate::ast::Visibility::Private,
        name: name.to_owned(),
        name_span: span,
        type_params: params.to_vec(),
        body,
        meta: crate::ast::Meta::new(span),
        editable_span: None,
        doc: None,
    }));
}

#[cfg(all(feature = "cli", feature = "surface"))]
fn lower_retype_type_projection(
    modules: Vec<(PathBuf, Module<Surface>)>,
) -> Result<Vec<(PathBuf, Module<RetypeSemanticPhase>)>, LocatedError> {
    let desugared = crate::pass::desugar::desugar_package_with_imports(
        modules,
        &crate::pass::desugar::PackageLiteralAliases::default(),
    )?;
    crate::pass::label_elab::elaborate_package(desugared, None).map(|(modules, _)| modules)
}

#[cfg(all(feature = "cli", not(feature = "surface")))]
fn lower_retype_type_projection(
    modules: Vec<(PathBuf, Module<Surface>)>,
) -> Result<Vec<(PathBuf, Module<RetypeSemanticPhase>)>, LocatedError> {
    modules
        .into_iter()
        .map(|(file_path, module)| {
            crate::prime::lower::lower_module(module)
                .map(|module| (file_path.clone(), module))
                .map_err(|error| LocatedError { file_path, error })
        })
        .collect()
}

#[cfg(feature = "cli")]
fn retype_projection_package(
    modules: Vec<(PathBuf, Module<RetypeSemanticPhase>)>,
) -> Result<Package<RetypeSemanticPhase>, LocatedError> {
    let mut entries = BTreeMap::new();
    for (file_path, module) in modules {
        let scope =
            crate::pass::resolve::TopLevelScope::build(&module).map_err(|error| LocatedError {
                file_path: file_path.clone(),
                error,
            })?;
        entries.insert(
            module_path_surface(&module.path),
            crate::pass::resolve::ModuleEntry {
                file_path,
                module,
                scope,
            },
        );
    }
    Ok(Package::from_parts(entries, None))
}

#[cfg(feature = "cli")]
fn retype_alias_scheme(
    module: &crate::ast::ModulePath,
    alias: &str,
    params: &[TypeParam],
) -> Type<RetypeSemanticPhase> {
    let span = module.span;
    let args = params
        .iter()
        .map(|param| Type::synth_path(vec![param.name.clone()], Vec::new(), param.span))
        .collect();
    let mut segments = module
        .segments
        .iter()
        .map(|segment| segment.name.clone())
        .collect::<Vec<_>>();
    segments.push(alias.to_owned());
    let mut scheme = Type::synth_path(segments, args, span);
    for param in params.iter().rev() {
        scheme = Type::Forall {
            param: param.clone(),
            body: Box::new(scheme),
            meta: crate::ast::Meta::new(span),
        };
    }
    scheme
}

#[cfg(feature = "cli")]
fn pack_retype_schemes(mut schemes: Vec<Type<RetypeSemanticPhase>>) -> Type<RetypeSemanticPhase> {
    let mut packed = schemes.pop().expect("a non-empty retype scheme batch");
    while let Some(next) = schemes.pop() {
        let span = next.span();
        packed = Type::Product {
            left: Box::new(next),
            right: Box::new(packed),
            meta: crate::ast::Meta::new(span),
        };
    }
    packed
}

#[cfg(feature = "cli")]
fn unpack_retype_schemes(
    mut packed: Type<RetypeSemanticPhase>,
    len: usize,
) -> Option<Vec<Type<RetypeSemanticPhase>>> {
    let mut schemes = Vec::with_capacity(len);
    for _ in 1..len {
        let Type::Product { left, right, .. } = packed else {
            return None;
        };
        schemes.push(*left);
        packed = *right;
    }
    schemes.push(packed);
    Some(schemes)
}

/// Compare two alias-free, identity-qualified payload schemes. Ordinary
/// nominals compare by exact `(module, name)` path; host types are the one
/// `retype` exception and compare by their terminal host name. Header and
/// nested `forall` binders compare positionally with equal effective kinds.
#[cfg(feature = "cli")]
fn canonical_retype_payloads_congruent(
    from: &Type<RetypeSemanticPhase>,
    to: &Type<RetypeSemanticPhase>,
    package: &Package<RetypeSemanticPhase>,
) -> bool {
    fn host_name<'a>(
        segments: &'a [crate::ast::PathSegment],
        package: &Package<RetypeSemanticPhase>,
    ) -> Option<&'a str> {
        let (name, owner) = segments.split_last()?;
        if owner.is_empty() {
            return None;
        }
        let owner = owner
            .iter()
            .map(crate::ast::PathSegment::as_str)
            .collect::<Vec<_>>()
            .join("/");
        let entry = package.module(&owner)?;
        let declaration = crate::pass::resolve::declaration_by_id(
            &entry.module,
            entry.scope.lookup(name.as_str())?,
        )?;
        declaration.host_type().map(|_| name.as_str())
    }

    fn path_names_equal(
        left: &[crate::ast::PathSegment],
        right: &[crate::ast::PathSegment],
    ) -> bool {
        left.len() == right.len()
            && left
                .iter()
                .zip(right)
                .all(|(left, right)| left.name == right.name)
    }

    fn go(
        from: &Type<RetypeSemanticPhase>,
        to: &Type<RetypeSemanticPhase>,
        package: &Package<RetypeSemanticPhase>,
        binders: &mut Vec<(String, String)>,
    ) -> bool {
        match (from, to) {
            (
                Type::Path {
                    segments: from_segments,
                    args: from_args,
                    ..
                },
                Type::Path {
                    segments: to_segments,
                    args: to_args,
                    ..
                },
            ) => {
                if from_args.len() != to_args.len()
                    || !from_args
                        .iter()
                        .zip(to_args)
                        .all(|(from, to)| go(from, to, package, binders))
                {
                    return false;
                }
                let from_binder = match from_segments.as_slice() {
                    [name] => binders.iter().rposition(|(from, _)| from == name.as_str()),
                    _ => None,
                };
                let to_binder = match to_segments.as_slice() {
                    [name] => binders.iter().rposition(|(_, to)| to == name.as_str()),
                    _ => None,
                };
                if from_binder.is_some() || to_binder.is_some() {
                    return from_binder == to_binder;
                }
                match (
                    host_name(from_segments, package),
                    host_name(to_segments, package),
                ) {
                    (Some(from), Some(to)) => from == to,
                    (Some(_), None) | (None, Some(_)) => false,
                    (None, None) => path_names_equal(from_segments, to_segments),
                }
            }
            (Type::Unit { .. }, Type::Unit { .. }) | (Type::Bottom { .. }, Type::Bottom { .. }) => {
                true
            }
            (
                Type::Function {
                    param: from_param,
                    ret: from_ret,
                    ..
                },
                Type::Function {
                    param: to_param,
                    ret: to_ret,
                    ..
                },
            ) => {
                go(from_param, to_param, package, binders) && go(from_ret, to_ret, package, binders)
            }
            (
                Type::Product {
                    left: from_left,
                    right: from_right,
                    ..
                },
                Type::Product {
                    left: to_left,
                    right: to_right,
                    ..
                },
            )
            | (
                Type::Sum {
                    left: from_left,
                    right: from_right,
                    ..
                },
                Type::Sum {
                    left: to_left,
                    right: to_right,
                    ..
                },
            ) => {
                go(from_left, to_left, package, binders)
                    && go(from_right, to_right, package, binders)
            }
            (
                Type::Forall {
                    param: from_param,
                    body: from_body,
                    ..
                },
                Type::Forall {
                    param: to_param,
                    body: to_body,
                    ..
                },
            ) => {
                if from_param.effective_kind() != to_param.effective_kind() {
                    return false;
                }
                binders.push((from_param.name.clone(), to_param.name.clone()));
                let congruent = go(from_body, to_body, package, binders);
                binders.pop();
                congruent
            }
            (Type::LabelSugar { ext, .. }, _) => match *ext {},
            (_, Type::LabelSugar { ext, .. }) => match *ext {},
            _ => false,
        }
    }

    go(from, to, package, &mut Vec::new())
}

/// Resolve and re-root a single dependency, writing each re-rooted module
/// to `<consumer_root>/<local>/<mod>.kio` (canonical, committed).
///
/// When `force` is `false` and the re-rooted tree on disk already matches
/// the freshly re-rooted output byte-for-byte — every module present and
/// identical, no stale `.kio` left over — this is a no-op returning
/// [`MaterializeOutcome::UpToDate`]: the
/// dependency is already materialized at the lock's intent. `force = true`
/// skips that short-circuit and rewrites unconditionally. Either way the
/// resolve itself honors a present lock (a locked git dependency uses its
/// locked commit, no re-resolution and no network when its checkout is
/// already populated), so the up-to-date comparison is against the lock's
/// pinned commit and the skip cannot mask a stale lock or a partial
/// checkout — those surface as a resolve error or a byte mismatch and fall
/// through to a full write.
#[cfg(feature = "cli")]
fn dependency_error_with_context(
    local_name: &str,
    file_path: PathBuf,
    error: Error,
) -> LocatedError {
    let mut diagnostic = error.diagnostic().clone();
    diagnostic.message = format!("in dependency `{local_name}`: {}", diagnostic.message);
    LocatedError {
        file_path,
        error: Error::Dep(diagnostic),
    }
}

#[cfg(feature = "cli")]
fn materialize_one_dependency(
    consumer_root: &Path,
    dep_file_path: &Path,
    dependency: &crate::ast::DependencyFile<Surface>,
    local_modules: &[(String, PathBuf)],
    retype_graph: &mut RetypeGraph,
    retype_validation: &mut RetypeValidationState,
    force: bool,
) -> Result<MaterializeOutcome, LocatedError> {
    validate_rehost_selectors(&dependency.rehost).map_err(|error| LocatedError {
        file_path: dep_file_path.to_path_buf(),
        error,
    })?;
    let local_name = dependency.name.clone();
    // The span every dependency-resolution diagnostic anchors to: the
    // origin's defining token (the `path` value, or the `git` URL value).
    let source_span = match &dependency.source.origin {
        crate::ast::SourceOrigin::Path { path_span, .. } => *path_span,
        crate::ast::SourceOrigin::Git(source) => source.url_span,
    };

    // The directory this dependency's re-rooted modules are written to.
    let dep_dir = consumer_root.join(&local_name);

    // Open-world collision: the local name must not shadow a local root
    // module, so `import <local>/…` resolves unambiguously to the dependency.
    // A module whose first segment is the local name but whose file lives
    // *under* this dependency's own materialization directory is this
    // dependency's own re-rooted output (present on a re-run), not a
    // collision; a hand-written root module `<root>/<local>.kio` sits
    // beside that directory and is the genuine clash this rejects.
    let collides = local_modules.iter().any(|(first_segment, file_path)| {
        first_segment == &local_name && !file_path.starts_with(&dep_dir)
    });
    if collides {
        return Err(LocatedError {
            file_path: dep_file_path.to_path_buf(),
            error: Error::dep(
                source_span,
                format!(
                    "dependency `{local_name}` collides with the local module root \
                     `{local_name}`: a dependency's local name must not match the first \
                     path segment of any of this package's own modules, so that \
                     `import {local_name}/…(...)` resolves unambiguously to the dependency. \
                     Rename the dependency (the `.dep.kio` stem and its `dependency` \
                     header) or the local module."
                ),
            ),
        });
    }

    // Resolve the dependency's package-file location. The local `path`
    // origin names it directly; the remote `git` origin fetches a clone
    // into the per-user cache, checks out the locked / resolved commit,
    // and names the `*.pkg.kio` in the checked-out tree. Either way the
    // result is a `*.pkg.kio` on disk whose parent is the dependency
    // root the existing re-root logic consumes unchanged.
    let pkg_file = match &dependency.source.origin {
        crate::ast::SourceOrigin::Path {
            path, path_span, ..
        } => {
            // `path` is relative to the consumer package root and names
            // the dependency's `*.pkg.kio` file.
            let pkg_file = consumer_root.join(path);
            let pkg_file = canonicalize(&pkg_file).map_err(|source| LocatedError {
                file_path: dep_file_path.to_path_buf(),
                error: Error::dep(
                    *path_span,
                    format!(
                        "dependency `{local_name}` cannot be resolved: `{path}` (resolved against \
                         the package root) does not point at an existing package file: {source}"
                    ),
                ),
            })?;
            let is_pkg_file = pkg_file
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(crate::file_kind::is_package_file);
            if !is_pkg_file || !pkg_file.is_file() {
                return Err(LocatedError {
                    file_path: dep_file_path.to_path_buf(),
                    error: Error::dep(
                        *path_span,
                        format!(
                            "dependency `{local_name}`: `{path}` must name the dependency's \
                             `*.pkg.kio` package file (resolved to `{}`)",
                            DisplayPath(&pkg_file)
                        ),
                    ),
                });
            }
            pkg_file
        }
        crate::ast::SourceOrigin::Git(source) => crate::git_dep::resolve_git_dependency(
            consumer_root,
            dep_file_path,
            &local_name,
            source,
        )?,
    };
    let dep_root = pkg_file
        .parent()
        .expect("a canonical package file always has a parent directory")
        .to_path_buf();

    // The dependency is an ordinary package: parse it with the same
    // per-package walk the consumer used. A parse / collection failure
    // in the dependency keeps the failing file's own span and path (so
    // the diagnostic renders source context against the dependency
    // file), retagged into the dependency-error tier and prefixed with
    // the local name so it is clear the failure is in a dependency.
    let dep_pkg = parse_package_files(&dep_root, &SourceOverlay::empty()).map_err(|walk_err| {
        let located = walk_err.into_located();
        dependency_error_with_context(&local_name, located.file_path, located.error)
    })?;
    crate::pass::surface_registry::validate_package(&dep_pkg.root_dir, &dep_pkg.modules)
        .map_err(|error| dep_pkg.prefer_deferred_body_error(error))
        .map_err(|located| {
            dependency_error_with_context(&local_name, located.file_path, located.error)
        })?;

    // A dependency's own dependencies, if it declares any, are
    // materialized within its tree (each package materializes its own) and
    // re-root along with it as part of the self-contained tree below; an
    // un-materialized one simply fails the consumer's check.

    // Re-root every dependency module under the local name and write it,
    // fully materialized (lazy bodies forced) and pretty-printed, to
    // `<consumer_root>/<local>/<mod-path>.kio`. The written path matches
    // the re-rooted `module` declaration, so the consumer's
    // FS-path-coherence check passes it like a hand-written module. Each
    // path written is collected so stale modules from a previous
    // materialization (a module removed upstream) can be pruned after.
    // Each `rehost` statement is just its `(from, to)` paths: the rewrite
    // is local to the `from` module (`apply_rehost`), so no up-front
    // census of the rehosted host names is needed.
    let rehost_targets: Vec<RehostTarget> = dependency
        .rehost
        .iter()
        .map(|decl| RehostTarget {
            from: decl.from.clone(),
            to: decl.to.clone(),
            span: decl.span,
        })
        .collect();

    // Resolve each `retype` statement to the newtype names its `from`
    // module declares (filtered to the per-type name when given), so
    // `apply_retype` can drop those declarations and redirect their
    // imports to the counterpart module. The `from`-side payloads are
    // snapshotted into `retype_checks` for the congruence obligation,
    // discharged after every dependency is materialized: the `to`
    // counterpart may be another dependency's module materialized later in
    // this same run (the diamond), so it is not necessarily on disk yet.
    let retype_selections =
        resolve_retype_selections(&local_name, &dependency.retype, &dep_pkg.modules).map_err(
            |error| LocatedError {
                file_path: dep_file_path.to_path_buf(),
                error,
            },
        )?;
    let mut retype_targets: Vec<RetypeTarget> = Vec::new();
    for selection in retype_selections {
        let decl = selection.declaration;
        let from_newtypes = selection.newtypes;

        let mut newtype_names = std::collections::BTreeSet::new();
        let mut snapshot = Vec::new();
        for from_nt in from_newtypes {
            newtype_names.insert(from_nt.name().to_owned());
            let mut selected = from_nt.snapshot();
            reroot_newtype_visibilities(&mut selected.declaration, &local_name);
            snapshot.push(selected);
        }
        // Flatten a chained remap to its ultimate origin: when this
        // `retype`'s counterpart module is itself a `retype` source, the
        // counterpart's own `newtype` is replaced by a re-import during its
        // materialization. Identity aliases retain the terminal member
        // namespace, but writing the terminal import here keeps the one exact
        // nominal origin explicit in both materialized source and Kio'.
        // The congruence obligation keeps the written `to`
        // (it is discharged after the whole tree is on disk and follows the
        // re-import chain itself).
        let resolved_targets = retype_chain_targets(&decl.to, &newtype_names, retype_graph);

        retype_validation.checks.push(DeferredRetypeCheck {
            dep_file_path: dep_file_path.to_path_buf(),
            span: decl.span,
            from: decl.from.clone(),
            to: decl.to.clone(),
            from_newtypes: snapshot,
        });

        retype_targets.extend(
            resolved_targets
                .into_iter()
                .map(|(resolved_to, resolved_names)| RetypeTarget {
                    from: decl.from.clone(),
                    newtype_names: resolved_names,
                    to: resolved_to,
                    span: decl.span,
                }),
        );
    }

    // Build the whole desired output up front — every re-rooted module
    // keyed by the `<consumer_root>/<local>/<mod-path>.kio` path it writes
    // to — so the skip-if-already-materialized check (below) can compare it
    // against disk before any write, and so the prune knows which paths to
    // keep.
    let mut desired: BTreeMap<PathBuf, String> = BTreeMap::new();
    #[cfg(feature = "surface")]
    let mut label_modules = Vec::new();
    if let Some(modules) = &mut retype_validation.visibility_modules {
        // Replace the selected dependency's complete old tree, including
        // removed modules. Unselected on-disk providers remain available.
        modules.retain(|(path, _)| !path.starts_with(&dep_dir));
    }
    for (file_path, module) in &dep_pkg.modules {
        let mut module = match dep_pkg.lazy_modules.get(file_path) {
            // Forcing a deferred body parses it for the first time, so a
            // body syntax error in the dependency surfaces here rather
            // than in the collection walk above — retag it the same way
            // (`specs/exit-codes.md` code 30: the resolved dependency
            // does not itself collect / parse), keeping the failing
            // file's own span and path for source context.
            Some(lazy) => lazy.force_all().map_err(|error| {
                dependency_error_with_context(&local_name, file_path.clone(), error)
            })?,
            None => module.clone(),
        };
        // Re-root the module's declared path *and* every intra-dependency
        // module-path reference it makes. A reference left un-rerooted —
        // an `import helper(h);` that still says `helper` after `helper`
        // became `<local>/helper` — would resolve to a non-existent root
        // module and fail the consumer's check. The dependency is
        // self-contained — any dependencies of its own are already
        // materialized within its tree — so every non-pseudo module-path
        // reference points at one of its own (now re-rooted) modules,
        // including those under a nested materialized dependency, and gets
        // the same prefix. A regular module's AST carries `ModulePath`
        // sites in three places — the `module` declaration, the import
        // clauses, and every `pub(<module-path>)` restriction (item-level
        // *and* on a `newtype`'s constructor/projector members or a
        // `rec`-group's members) — all re-rooted below. Inline
        // alias-qualified references (`import M as m; m.item`) resolve
        // *through* the `Qualified.path` re-rooted here, since their
        // leading segment is the local alias, not a module name. The
        // `__intrinsics__` / `__comptime__` pseudo-modules are not
        // dependency modules and are left untouched.
        reroot_module_path(&mut module.path, &local_name);
        for import_clause in &mut module.imports {
            match &mut import_clause.kind {
                crate::ast::ImportKind::Selective { from, .. } => {
                    reroot_module_path(from, &local_name)
                }
                crate::ast::ImportKind::Qualified { path, .. } => {
                    reroot_module_path(path, &local_name)
                }
                crate::ast::ImportKind::Intrinsics | crate::ast::ImportKind::Comptime => {}
            }
        }
        // A `pub(P)` restriction names a module in the dependency's own
        // tree, so it re-roots under `<local>` exactly like the module
        // path and import clauses above — otherwise the restriction would
        // point at a path that no longer exists post-materialization. Every
        // visibility an item carries is re-rooted, including the member
        // visibilities a `newtype` constructor/projector or a `rec`-group
        // member may carry.
        for item in &mut module.items {
            for vis in item_visibilities_mut(item) {
                reroot_visibility(vis, &local_name);
            }
        }

        // Apply this dependency's `rehost` statements: drop the module's
        // own host declarations (rebinding them to a provider import) and
        // redirect its imports of other modules' rehosted host items to
        // their providers. Done after re-rooting so the injected /
        // redirected imports' `from` (consumer modules) stay un-rerooted.
        apply_rehost(&mut module, &rehost_targets);

        // Preserve the exact pre-`retype` declaration order and recursive
        // group ownership for the shared resolver pass run after all
        // dependencies are materialized. One module may feed several retype
        // declarations; its path-keyed snapshot is stored only once.
        if retype_targets.iter().any(|target| {
            target.from.segments == module.path.segments && !target.newtype_names.is_empty()
        }) {
            retype_validation
                .source_modules
                .entry(module_path_surface(&module.path))
                .or_insert_with(|| (file_path.clone(), retype_type_projection(module.clone())));
        }

        // Apply this dependency's `retype` statements the same way, for
        // `newtype`s: drop a retyped module's matched newtype declarations
        // (rebinding them to the counterpart's import) and redirect other
        // modules' imports of those newtypes to the counterpart. Same
        // post-re-rooting timing as `rehost`.
        let rewrite = apply_retype(&mut module, &retype_targets).map_err(|error| LocatedError {
            file_path: dep_file_path.to_path_buf(),
            error,
        })?;
        retype_validation.imports.extend(rewrite.imports);

        // The re-rooted module path is the file's location relative to the
        // consumer root: `<local>/<mod-segments>.kio`. Its leading segment
        // is `<local>` (just prepended), so the written tree is rooted at
        // `dep_dir` and coherence holds.
        let rel: PathBuf = module
            .path
            .segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<PathBuf>();
        let out_path = consumer_root.join(&rel).with_extension("kio");
        #[cfg(feature = "surface")]
        if !rewrite.forwards.is_empty() {
            if let Some(modules) = &mut retype_validation.visibility_modules {
                modules.push((out_path.clone(), borrowed_retype_type_projection(&module)));
            }
            label_modules.push(PendingLabelModule {
                out_path,
                module,
                forwards: rewrite.forwards,
            });
            continue;
        }
        let canonical_source = crate::pretty::pretty_module(&module);
        if let Some(modules) = &mut retype_validation.visibility_modules {
            modules.push((out_path.clone(), retype_type_projection(module)));
        }
        desired.insert(out_path, canonical_source);
    }

    // Skip-if-already-materialized: when not forced, a tree on disk that
    // already equals `desired` exactly — every module present and
    // byte-identical, and no extra `.kio` left under `dep_dir` — is the
    // lock's intent already materialized, so re-fetching is redundant. The
    // comparison is against the freshly re-rooted output of the lock's
    // pinned commit (resolved above with no re-resolution), so it is sound:
    // a stale lock errored before reaching here, and a partial / corrupt /
    // drifted tree mismatches and falls through to the full write below.
    #[cfg(feature = "surface")]
    let output_complete = label_modules.is_empty();
    #[cfg(not(feature = "surface"))]
    let output_complete = true;
    if output_complete && !force && materialized_tree_matches(&dep_dir, &desired) {
        return Ok(MaterializeOutcome::UpToDate);
    }

    if retype_targets
        .iter()
        .any(|target| !target.newtype_names.is_empty())
    {
        retype_validation
            .pending
            .push(PendingRetypeMaterialization {
                dep_dir,
                local_name,
                source_span,
                desired,
                #[cfg(feature = "surface")]
                label_modules,
            });
    } else {
        publish_materialized_dependency(&dep_dir, &local_name, source_span, &desired)?;
    }
    Ok(MaterializeOutcome::Fetched)
}

#[cfg(feature = "cli")]
fn publish_materialized_dependency(
    dep_dir: &Path,
    local_name: &str,
    source_span: Span,
    desired: &BTreeMap<PathBuf, String>,
) -> Result<(), LocatedError> {
    // Write each re-rooted module (content-addressed: an identical file is
    // left untouched). The paths written are collected so stale modules
    // from a previous materialization (a module removed upstream) can be
    // pruned after.
    let written: std::collections::BTreeSet<PathBuf> = desired.keys().cloned().collect();
    for (out_path, canonical_source) in desired {
        write_materialized_file(out_path, canonical_source).map_err(|source| LocatedError {
            file_path: out_path.clone(),
            error: Error::dep(
                source_span,
                format!(
                    "dependency `{local_name}`: cannot write materialized module to `{}`: \
                     {source}",
                    DisplayPath(out_path)
                ),
            ),
        })?;
    }

    // Prune any `.kio` module under `dep_dir` that this run did not write
    // — a module removed upstream since the last materialization. Files
    // re-written identically above keep their timestamps (the
    // content-addressed write), so a no-change re-run touches nothing.
    prune_stale_modules(dep_dir, &written).map_err(|source| LocatedError {
        file_path: dep_dir.to_path_buf(),
        error: Error::dep(
            source_span,
            format!(
                "dependency `{local_name}`: cannot prune the materialization directory `{}`: \
                 {source}",
                DisplayPath(dep_dir)
            ),
        ),
    })?;
    Ok(())
}

/// Remove every `.kio` module file under a materialized dependency
/// directory that is **not** in `keep` — a module that existed in a
/// previous materialization but was removed upstream — leaving the
/// directory and the still-current modules in place. Now-empty
/// subdirectories are dropped. A non-existent directory is not an error
/// (first run).
#[cfg(feature = "cli")]
fn prune_stale_modules(
    dep_dir: &Path,
    keep: &std::collections::BTreeSet<PathBuf>,
) -> io::Result<()> {
    if !dep_dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dep_dir)? {
        let path = entry?.path();
        if path.is_dir() {
            prune_stale_modules(&path, keep)?;
            // Drop the subdirectory if it is now empty; a write that still
            // needs it recreated it above, so a surviving module keeps its
            // directory. A directory still holding a kept module or a
            // stray non-`.kio` file is left alone (`remove_dir` errors on
            // non-empty and the error is ignored).
            let _ = fs::remove_dir(&path);
        } else if !keep.contains(&path)
            && path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(crate::file_kind::has_kio_extension)
        {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Whether the materialized tree under `dep_dir` already equals `desired`
/// exactly: every path in `desired` exists on disk holding identical bytes,
/// **and** no other `.kio` module lives under `dep_dir` (a stale
/// upstream-removed module would still need pruning, so its presence is a
/// mismatch). A missing directory, a missing / differing module, or an
/// extra `.kio` all return `false`, routing the caller to a full
/// (re)materialization — which writes the differences and prunes the
/// strays. The materialized tree is git-tracked (committed), so there is no
/// gitignore marker to verify; any non-`.kio` stray is ignored.
#[cfg(feature = "cli")]
fn materialized_tree_matches(dep_dir: &Path, desired: &BTreeMap<PathBuf, String>) -> bool {
    // Every desired module must be present and byte-identical.
    for (path, contents) in desired {
        match fs::read_to_string(path) {
            Ok(existing) if existing == *contents => {}
            _ => return false,
        }
    }
    // No `.kio` module under `dep_dir` may be absent from `desired` — such
    // a file is a stale module the full path would prune.
    let mut on_disk: Vec<PathBuf> = Vec::new();
    if collect_materialized_modules(dep_dir, &mut on_disk).is_err() {
        return false;
    }
    on_disk.iter().all(|path| desired.contains_key(path))
}

/// Collect every `.kio` module file under a materialized dependency
/// directory (recursively). A non-existent directory yields the empty set
/// (nothing materialized yet). Mirrors [`prune_stale_modules`]'s notion of
/// which files are managed modules — non-`.kio` strays are excluded.
#[cfg(feature = "cli")]
fn collect_materialized_modules(dep_dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if !dep_dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dep_dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_materialized_modules(&path, out)?;
        } else if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(crate::file_kind::has_kio_extension)
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Write `contents` to `path`, creating parent directories, but skip the
/// write when `path` already holds exactly `contents` — so repeated
/// materialization does not churn file timestamps when nothing changed.
#[cfg(feature = "cli")]
fn write_materialized_file(path: &Path, contents: &str) -> io::Result<()> {
    if let Ok(existing) = fs::read_to_string(path)
        && existing == contents
    {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
}

#[cfg(feature = "cli")]
fn collect_kio_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), WalkError> {
    let mut entries = fs::read_dir(dir)
        .map_err(|source| WalkError::Io {
            path: dir.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| WalkError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.path());

    let nested_package_root = dir != root
        && entries.iter().any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(crate::file_kind::is_package_file)
        });
    if nested_package_root {
        return Ok(());
    }

    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| WalkError::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_dir() {
            collect_kio_files(root, &path, out)?;
        } else if (file_type.is_file()
            || (file_type.is_symlink()
                && fs::metadata(&path)
                    .map(|metadata| metadata.is_file())
                    .unwrap_or(false)))
            && path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(crate::file_kind::has_kio_extension)
        {
            out.push(path);
        }
    }
    Ok(())
}

/// One discovered package root: the directory holding its
/// `<name>.pkg.kio` file, plus the package name (the file stem).
#[cfg(feature = "cli")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPackageRoot {
    /// The directory the package file lives in — the root each
    /// package's source walk is rooted at.
    pub dir: PathBuf,
    /// The absolute path to the `<name>.pkg.kio` file.
    pub package_file: PathBuf,
    /// The package name (the `.pkg.kio` stem).
    pub name: String,
}

/// The marker file `kio build` drops into every directory it creates for
/// generated output or on-disk caches (`prepare_target_dir`, the cache
/// `open` paths). Package discovery prunes any directory containing this
/// marker, so a package nested under a *custom* (non-`out`, non-hidden)
/// `cache "<path>";` or emit-output path is neither re-discovered (and
/// re-built into ever-deeper trees) nor shadows a real package. The
/// `out` / `target` / `.*` name heuristic stays as a fast-path that
/// avoids a `read_dir` probe for the common layout.
#[cfg(feature = "cli")]
pub const DISCOVERY_SKIP_MARKER: &str = ".kio-generated";

/// Drop the [`DISCOVERY_SKIP_MARKER`] into `dir` so package discovery
/// prunes it. Best-effort: a failure to write the marker is not fatal to
/// the build (discovery still has the name heuristic), so the error is
/// swallowed.
#[cfg(feature = "cli")]
pub fn mark_generated_dir(dir: &Path) {
    let _ = fs::write(dir.join(DISCOVERY_SKIP_MARKER), b"");
}

/// Whether `dir` carries the [`DISCOVERY_SKIP_MARKER`] (a generated /
/// cache directory discovery must not descend into).
#[cfg(feature = "cli")]
fn is_generated_dir(dir: &Path) -> bool {
    dir.join(DISCOVERY_SKIP_MARKER).exists()
}

/// Whether a positional command argument names a **package selector**
/// rather than a bare identifier (e.g. a `kio build` target id like
/// `js` / `rust` / `kio-prime`).
///
/// The rule is **deterministic** — it keys on the argument's *shape*,
/// not on the cwd filesystem state: a selector either contains a path
/// separator (`a/b`, `./pkg`) or carries the `*.pkg.kio` suffix.
/// Anything else is a bare identifier and stays a target id. Keying on
/// `Path::is_dir()` against the cwd (the old behavior) let a cwd-sibling
/// directory literally named `rust` hijack the documented
/// bare-identifier-is-a-target-id rule. Shared by `kio build` (target id
/// vs package path) and `kio sig` (selector validation).
pub fn is_package_selector_arg(arg: &str) -> bool {
    arg.contains('/')
        || arg.contains(std::path::MAIN_SEPARATOR)
        || crate::file_kind::is_package_file(arg)
}

/// Discover every package root in `cwd`'s subtree.
///
/// A directory containing one or more `<name>.pkg.kio` files is a
/// package root. Discovery continues below each root so that a nested
/// package is surfaced as its own entry; the package module walk later
/// prunes nested roots, keeping the two module trees disjoint. A
/// directory holding more than one `*.pkg.kio` is ambiguous (two
/// packages claiming one module tree) and surfaced as
/// [`WalkError::MultiplePackageFiles`], mirroring [`walk`].
///
/// Roots are returned sorted by directory then name, so the fan-out
/// order over discovered packages is deterministic. The fan-out
/// consumers (`kio build`, `kio doc build`, `kio sig`) root one
/// independent package compile at each entry.
#[cfg(feature = "cli")]
pub fn discover_package_roots(cwd: &Path) -> Result<Vec<DiscoveredPackageRoot>, WalkError> {
    let root = canonicalize(cwd).map_err(|source| WalkError::CanonicalizeRoot {
        path: cwd.to_path_buf(),
        source,
    })?;
    let mut out = Vec::new();
    collect_package_roots(&root, &mut out)?;
    out.sort_by(|a, b| a.dir.cmp(&b.dir).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

#[cfg(feature = "cli")]
fn collect_package_roots(
    dir: &Path,
    out: &mut Vec<DiscoveredPackageRoot>,
) -> Result<(), WalkError> {
    let mut entries = fs::read_dir(dir)
        .map_err(|source| WalkError::Io {
            path: dir.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| WalkError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.path());

    let mut package_files: Vec<PathBuf> = Vec::new();
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| WalkError::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_dir() {
            // Skip build-artifact (`out`, `target`) and hidden (`.*`)
            // directories — mirrors `kio fmt`'s walker. Without this the
            // discovery descends into a package's own emitted output
            // (`out/kio-prime/<pkg>.pkg.kio`, the artifact-cache `tree/`
            // copies) and re-builds them as separate packages, which on
            // a Kio'-roundtrip build recurses into ever-deeper `out/`
            // trees.
            //
            // The name list is a fast-path; the authoritative prune is
            // the generated-dir marker (`DISCOVERY_SKIP_MARKER`), which
            // `kio build` drops into every output / cache dir it creates.
            // It catches a package nested under a *custom* (non-`out`,
            // non-hidden) `cache "<path>";` or emit path, which the name
            // heuristic alone would miss — leaving that package invisible
            // or re-discovered.
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name == "out" || name == "target" || name.starts_with('.') {
                continue;
            }
            if is_generated_dir(&path) {
                continue;
            }
            subdirs.push(path);
        } else if (file_type.is_file()
            || (file_type.is_symlink()
                && fs::metadata(&path)
                    .map(|metadata| metadata.is_file())
                    .unwrap_or(false)))
            && path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(crate::file_kind::is_package_file)
        {
            package_files.push(path);
        }
    }

    match package_files.len() {
        0 => {}
        1 => {
            let package_file = package_files.into_iter().next().unwrap();
            let name = package_file
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(crate::file_kind::package_stem)
                .unwrap_or("pkg")
                .to_owned();
            out.push(DiscoveredPackageRoot {
                dir: dir.to_path_buf(),
                package_file,
                name,
            });
        }
        _ => {
            return Err(WalkError::MultiplePackageFiles {
                root: dir.to_path_buf(),
                paths: package_files,
            });
        }
    }
    // Always descend: a package nested inside another package's source
    // tree is a separate package built by its own rooted walk. The
    // enclosing package's walk (rooted at its directory) prunes the
    // nested root, so the two trees don't overlap — each module belongs
    // to exactly one package.
    for subdir in subdirs {
        collect_package_roots(&subdir, out)?;
    }
    Ok(())
}

#[cfg(feature = "cli")]
fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path)
}

#[derive(Debug)]
pub enum WalkError {
    CanonicalizeRoot {
        path: PathBuf,
        source: io::Error,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
    ReadFile {
        path: PathBuf,
        source: io::Error,
    },
    MultiplePackageFiles {
        root: PathBuf,
        paths: Vec<PathBuf>,
    },
    Parse {
        path: PathBuf,
        source_text: String,
        error: Error,
    },
}

impl WalkError {
    pub fn into_located(self) -> LocatedError {
        match self {
            WalkError::CanonicalizeRoot { path, source } => LocatedError {
                file_path: path.clone(),
                error: Error::internal(
                    Span::new(0, 0),
                    format!(
                        "cannot resolve package root `{}`: {source}",
                        DisplayPath(&path)
                    ),
                ),
            },
            WalkError::Io { path, source } | WalkError::ReadFile { path, source } => LocatedError {
                file_path: path.clone(),
                error: Error::internal(
                    Span::new(0, 0),
                    format!("cannot read `{}`: {source}", DisplayPath(&path)),
                ),
            },
            WalkError::MultiplePackageFiles { root, paths } => {
                let rendered = paths
                    .iter()
                    .map(|path| format!("`{}`", DisplayPath(path)))
                    .collect::<Vec<_>>()
                    .join(", ");
                LocatedError {
                    file_path: root.clone(),
                    error: Error::import(
                        Span::new(0, 0),
                        format!(
                            "multiple package files at package root `{}`: {rendered}",
                            DisplayPath(&root)
                        ),
                    ),
                }
            }
            WalkError::Parse { path, error, .. } => LocatedError {
                file_path: path,
                error,
            },
        }
    }
}

pub fn collect_sources(parsed: &ParsedPackageCollection) -> BTreeMap<PathBuf, String> {
    let mut sources = BTreeMap::new();
    for pkg in parsed.packages.values() {
        for (k, v) in &pkg.sources {
            sources.insert(k.clone(), v.clone());
        }
    }
    sources
}

pub type TypedPackageCollection = PackageCollection<Prime>;

pub fn find_package_entry<'a, P: Phase>(
    workspace: &'a PackageCollection<P>,
    canonical_dir: &Path,
    package_name: &str,
) -> Option<&'a PackageEntry<P>> {
    workspace.packages.get(&PackageKey {
        canonical_dir: canonical_dir.to_path_buf(),
        package_name: package_name.to_owned(),
    })
}

pub fn substitute_type<P>(ty: &Type<P>, subst: &HashMap<String, Type<P>>) -> Type<P>
where
    P: Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    crate::pass::typecheck_core::subst_type(ty, subst)
}

#[cfg(all(test, feature = "cli"))]
mod tests {

    #[cfg(feature = "cli")]
    #[test]
    fn import_grammar_provider_read_observer_calibration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("syntax.kio");
        fs::write(&path, "module syntax;").unwrap();
        let mut overlay = SourceOverlay::empty();
        overlay.insert(path.clone(), "module syntax;".to_owned());
        let (read, attempts) =
            import_grammar_with_denied_reads([path.clone()], || overlay.read(&path));
        assert_eq!(read.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(attempts, vec![path]);
    }

    #[test]
    fn import_grammar_provider_denied_entrypoints() {
        use crate::pass::parser::{self, IMPORT_GRAMMAR_CONSUMER, import_grammar_assert_consumer};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("syntax.kio");
        let consumer_path = dir.path().join("app/main.kio");
        for present in [false, true] {
            if present {
                fs::write(&path, "provider bytes must not be read").unwrap();
            }
            let (_, attempts) = import_grammar_with_denied_reads([path.clone()], || {
                import_grammar_assert_consumer(&parser::parse(IMPORT_GRAMMAR_CONSUMER).unwrap());
                import_grammar_assert_consumer(
                    &parser::parse_lazy(IMPORT_GRAMMAR_CONSUMER)
                        .unwrap()
                        .force_all()
                        .unwrap(),
                );
                import_grammar_assert_consumer(
                    &parser::parse_module_file(IMPORT_GRAMMAR_CONSUMER)
                        .unwrap()
                        .module,
                );
                import_grammar_assert_consumer(
                    &parser::parse_module_file_lazy(IMPORT_GRAMMAR_CONSUMER)
                        .unwrap()
                        .force_all()
                        .unwrap()
                        .module,
                );
                import_grammar_assert_consumer(
                    &parse_module_file_with_file_context(IMPORT_GRAMMAR_CONSUMER, &consumer_path)
                        .unwrap()
                        .module,
                );
                let (recovered, errors) =
                    parser::parse_recover_imports(IMPORT_GRAMMAR_CONSUMER).unwrap();
                assert!(errors.is_empty());
                import_grammar_assert_consumer(&recovered);
            });
            assert!(attempts.is_empty(), "{attempts:?}");
        }
        eprintln!("provider denial: source_states=2 entrypoints=6 attempts=0");
    }
    use super::*;

    fn lazy_module(source: &str) -> crate::ast::Module<crate::ast::Surface> {
        crate::pass::parser::parse_module_file_lazy(source)
            .expect("lazy module")
            .module
    }

    fn parsed_module(source: &str) -> crate::ast::Module<crate::ast::Surface> {
        crate::pass::parser::parse(source).expect("module")
    }

    fn retype_decl(from: &str, to: &str, type_name: Option<&str>) -> crate::ast::RetypeDecl {
        let suffix = type_name.map_or_else(String::new, |name| format!(".{name}"));
        let source = format!(
            "dependency fixture; source {{ path \"../fixture/fixture.pkg.kio\"; }} \
             retype {from}{suffix} to {to}{suffix};"
        );
        crate::pass::parser::parse_dependency_file(&source, None)
            .expect("dependency fixture")
            .retype
            .into_iter()
            .next()
            .expect("one retype declaration")
    }

    fn retype_dependency(statements: &str) -> crate::ast::DependencyFile<Surface> {
        crate::pass::parser::parse_dependency_file(
            &format!(
                "dependency widget; source {{ path \"../widget/widget.pkg.kio\"; }} {statements}"
            ),
            None,
        )
        .expect("dependency fixture")
    }

    fn retype_source_modules() -> Vec<(PathBuf, Module<Surface>)> {
        vec![
            (
                PathBuf::from("store.kio"),
                parsed_module(
                    "module store; \
                     pub newtype A : . { pub constructor make_a; pub projector open_a; }; \
                     pub newtype B : . { pub constructor make_b; pub projector open_b; };",
                ),
            ),
            (
                PathBuf::from("next/one.kio"),
                parsed_module(
                    "module next/one; \
                     pub newtype A : . { pub constructor make; pub projector open; };",
                ),
            ),
            (
                PathBuf::from("next/two.kio"),
                parsed_module(
                    "module next/two; \
                     pub newtype A : . { pub constructor make; pub projector open; };",
                ),
            ),
        ]
    }

    fn module_path(path: &str) -> crate::ast::ModulePath {
        parsed_module(&format!("module {path};")).path
    }

    fn target_names<'a>(
        targets: &'a [(crate::ast::ModulePath, std::collections::BTreeSet<String>)],
        path: &str,
    ) -> Vec<&'a str> {
        targets
            .iter()
            .find(|(target, _)| module_path_surface(target) == path)
            .map(|(_, names)| names.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    fn selective_import_names(module: &Module<Surface>, path: &str) -> Vec<String> {
        module
            .imports
            .iter()
            .find_map(|import_| match &import_.kind {
                crate::ast::ImportKind::Selective { items, from }
                    if module_path_surface(from) == path =>
                {
                    Some(
                        items
                            .iter()
                            .filter_map(crate::ast::ImportItem::as_name)
                            .map(str::to_owned)
                            .collect(),
                    )
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn rehost_rejects_repeated_source_paths_regardless_of_target_or_order() {
        for (first, second) in [("one", "one"), ("one", "two"), ("two", "one")] {
            let dependency = retype_dependency(&format!(
                "rehost widget/api to {first}; rehost widget/api to {second};"
            ));
            let error = validate_rehost_selectors(&dependency.rehost)
                .expect_err("each repeated source selector is invalid");
            assert!(matches!(error, Error::Dep(_)));
            let diagnostic = error.diagnostic();
            assert_eq!(diagnostic.span, dependency.rehost[1].span);
            assert_eq!(diagnostic.secondary().len(), 1);
            assert_eq!(diagnostic.secondary()[0].span, dependency.rehost[0].span);
            assert_eq!(
                diagnostic.message,
                "`rehost` source module `widget/api` is selected more than once"
            );
        }
    }

    #[test]
    fn rehost_accepts_disjoint_source_paths_with_shared_targets() {
        let dependency = retype_dependency(
            "rehost widget/one/api to provider; rehost widget/two/api to provider; \
             rehost widget/api to provider;",
        );
        validate_rehost_selectors(&dependency.rehost).expect("full paths distinguish selectors");
        validate_rehost_selectors(&[]).expect("no rehost selectors");
    }

    fn assert_duplicate_rehost_preserves_tree(force: bool, existing: bool) {
        let dir = tempfile::tempdir().expect("rehost fixture");
        let write = |path: &str, source: &str| {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        };
        write("consumer/app.pkg.kio", "package app;");
        write(
            "consumer/provider.kio",
            "module provider; pub fn ping() -> . { () }",
        );
        write("library/widget.pkg.kio", "package widget; bridge { api; }");
        write("library/api.kio", "module api; host fn ping() -> .;");
        let source = "dependency widget; source { path \"../library/widget.pkg.kio\"; } ";
        write(
            "consumer/widget.dep.kio",
            &format!("{source} rehost widget/api to provider;"),
        );
        let root = dir.path().join("consumer");
        let mut before = BTreeMap::new();
        if existing {
            write("library/old.kio", "module old;");
            materialize_dependencies_filtered(&root, None, false).expect("valid first fetch");
            for name in ["api.kio", "old.kio"] {
                before.insert(name, fs::read(root.join("widget").join(name)).unwrap());
            }
            fs::remove_file(dir.path().join("library/old.kio")).unwrap();
        }
        write(
            "library/api.kio",
            "module api; host fn ping() -> .; pub fn added() -> . { () }",
        );
        write("library/added.kio", "module added;");
        write(
            "consumer/widget.dep.kio",
            &format!("{source} rehost widget/api to provider; rehost widget/api to provider;"),
        );
        let result = materialize_dependencies_filtered(&root, None, force);
        let observed = ["api.kio", "old.kio", "added.kio"]
            .map(|name| (name, fs::read(root.join("widget").join(name)).ok()));
        let expected =
            ["api.kio", "old.kio", "added.kio"].map(|name| (name, before.get(name).cloned()));
        assert_eq!(
            observed, expected,
            "duplicate rehost must not create, replace or prune output"
        );
        if !existing {
            assert!(!root.join("widget").exists());
        }
        let error = result.expect_err("duplicate selector is rejected before publication");
        let canonical_root = root.canonicalize().expect("canonical fixture root");
        assert_eq!(error.file_path, canonical_root.join("widget.dep.kio"));
        assert!(error.error.diag().1.contains("selected more than once"));
    }

    #[test]
    fn rehost_duplicate_preserves_absent_tree() {
        assert_duplicate_rehost_preserves_tree(false, false);
    }

    #[test]
    fn rehost_duplicate_preserves_absent_tree_when_forced() {
        assert_duplicate_rehost_preserves_tree(true, false);
    }

    #[test]
    fn rehost_duplicate_preserves_existing_tree() {
        assert_duplicate_rehost_preserves_tree(false, true);
    }

    #[test]
    fn rehost_duplicate_preserves_existing_tree_when_forced() {
        assert_duplicate_rehost_preserves_tree(true, true);
    }

    #[test]
    fn retype_rejects_every_second_selector_for_one_exact_source_nominal() {
        let cases = [
            // Exact per-type repetition is invalid even when the target is
            // byte-identical.
            "retype widget/store.A to core/one.A; \
             retype widget/store.A to core/one.A;",
            // Distinct and potentially convergent target paths do not make
            // two selectors for the source nominal unambiguous.
            "retype widget/store.A to widget/next/one.A; \
             retype widget/store.A to widget/next/two.A; \
             retype widget/next/one.A to core/final.A; \
             retype widget/next/two.A to core/final.A;",
            "retype widget/store to core/one; \
             retype widget/store to core/one;",
            "retype widget/store to core/all; \
             retype widget/store.A to core/one.A;",
            "retype widget/store.A to core/one.A; \
             retype widget/store to core/all;",
        ];
        let modules = retype_source_modules();
        for statements in cases {
            let dependency = retype_dependency(statements);
            let error = resolve_retype_selections("widget", &dependency.retype, &modules)
                .expect_err("overlapping selector must be rejected");
            assert!(
                matches!(error, Error::Dep(_)),
                "selector failure must remain a dependency error: {error:?}"
            );
            assert_eq!(
                error.diagnostic().span,
                dependency.retype[1].span,
                "the later selector must be primary for {statements}"
            );
            assert_eq!(
                error.diagnostic().secondary().len(),
                1,
                "the first selector must be retained for {statements}"
            );
            assert_eq!(
                error.diagnostic().secondary()[0].span,
                dependency.retype[0].span,
                "the first selector must be secondary for {statements}"
            );
            assert!(
                error
                    .diagnostic()
                    .message
                    .contains("source newtype `widget/store.A` is selected more than once"),
                "got: {error:?}"
            );
        }
    }

    #[test]
    fn retype_accepts_disjoint_exact_source_selectors() {
        let dependency = retype_dependency(
            "retype widget/store.A to core/one.A; \
             retype widget/store.B to core/two.B;",
        );
        let modules = retype_source_modules();
        let selections = resolve_retype_selections("widget", &dependency.retype, &modules)
            .expect("disjoint selectors");
        assert_eq!(selections.len(), 2);
        assert_eq!(selections[0].newtypes[0].name(), "A");
        assert_eq!(selections[1].newtypes[0].name(), "B");
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_inventory_distinguishes_explicit_labels_from_reuse_and_forwarding() {
        let module = parsed_module(
            "module store; \
             newtype Plain : . { constructor mk; projector get; }; \
             pub labels { box[*F][A]<X>: F(A) & X }; \
             labels Later[*G][B] = { box[*G][B]: _, flag: . }; \
             type {forwarded} = {box}; \
             rec { \
               newtype Node : Edge { constructor mk; projector get; }; \
               labels { edge: Node }; \
             }",
        );
        let nominals = declared_retype_nominals(&module);
        assert_eq!(
            nominals
                .iter()
                .map(DeclaredRetypeNominal::name)
                .collect::<Vec<_>>(),
            ["Plain", "Box", "Flag", "Node", "Edge"],
        );
        let snapshots = nominals
            .iter()
            .map(DeclaredRetypeNominal::snapshot)
            .collect::<Vec<_>>();
        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.label.as_deref())
                .collect::<Vec<_>>(),
            [None, Some("box"), Some("flag"), None, Some("edge")],
        );
        assert_eq!(snapshots[1].declaration.type_params.len(), 2);
        assert_eq!(snapshots[1].declaration.existential_params.len(), 1);
        assert_eq!(snapshots[1].declaration.existential_params[0].name, "X");
        assert!(snapshots[1].declaration.vis.is_pub());
        assert!(retype_head_visibility(&module, "Box").unwrap().is_pub());
        assert!(retype_head_visibility(&module, "Forwarded").is_none());
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_selection_retains_duplicate_source_declaration_occurrences() {
        let module = parsed_module("module store; labels { foo: . }; labels { foo: . };");
        let modules = vec![(PathBuf::from("store.kio"), module)];
        let dependency = retype_dependency("retype widget/store.Foo to core/one.Foo;");
        let selections = resolve_retype_selections("widget", &dependency.retype, &modules)
            .expect("one selector preserves both occurrences for source legality checking");
        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].newtypes.len(), 2);
        let first = selections[0].newtypes[0].snapshot();
        let second = selections[0].newtypes[1].snapshot();
        assert_eq!(first.label.as_deref(), Some("foo"));
        assert_eq!(second.label.as_deref(), Some("foo"));
        assert_ne!(first.declaration.name_span, second.declaration.name_span);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn provider_alias_allocation_keeps_forwarded_labels_in_their_own_namespace() {
        let source = "module source; labels { foo: . }; type {core_store} = {foo};";
        let provider = module_path("core/store");
        assert_eq!(
            fresh_provider_alias(&parsed_module(source), &provider),
            "_rehost_gdgphcgfcphdhegphcgf"
        );
        // Labels cannot begin with `_`; ordinary values can occupy the exact alias.
        let with_value = parsed_module(&format!(
            "{source} fn _rehost_gdgphcgfcphdhegphcgf() -> . {{ () }}"
        ));
        assert_eq!(
            fresh_provider_alias(&with_value, &provider),
            "_rehost_gdgphcgfcphdhegphcgf_n2"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_duplicate_selected_labels_reach_original_source_validation() {
        let original =
            parsed_module("module widget/store; pub labels { foo: . }; pub labels { foo: . };");
        let source_path = PathBuf::from("widget/store.kio");
        let source_modules = BTreeMap::from([(
            module_path_surface(&original.path),
            (
                source_path.clone(),
                retype_type_projection(original.clone()),
            ),
        )]);
        let mut source = original;
        let target = RetypeTarget {
            from: source.path.clone(),
            newtype_names: ["Foo".to_owned()].into_iter().collect(),
            to: module_path("core/store"),
            span: Span::new(1, 2),
        };
        let rewrite = apply_retype(&mut source, &[target])
            .expect("duplicate written declarations are not selector collisions");
        assert_eq!(rewrite.forwards.len(), 2, "neither occurrence is erased");
        let mut materialized = vec![(
            PathBuf::from("core/store.kio"),
            parsed_module("module core/store; pub labels { foo: . };"),
        )];
        finalize_retyped_label_forwards(&mut source, rewrite.forwards, &materialized);
        materialized.push((source_path.clone(), source));
        let error = validate_retype_source_semantics(&source_modules, &materialized)
            .expect_err("the ordinary original-source label check rejects both declarations");
        assert_eq!(error.file_path, source_path);
        assert!(matches!(error.error, Error::Dep(_)));
        assert!(
            error
                .error
                .diag()
                .1
                .contains("label `foo` is already explicitly declared in this module")
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_label_counterpart_follows_written_aliases_without_inventing_label_heads() {
        let source = parsed_module("module source; pub labels { foo: . };");
        let origin = parsed_module("module origin; pub labels { foo: . };");
        let relay = parsed_module("module relay; import origin as o; pub type Foo = o.Foo;");
        let forward =
            parsed_module("module forward; import origin as o; pub type {foo} = {o.foo};");
        let ordinary = parsed_module(
            "module ordinary; pub newtype Foo : . { pub constructor mk; pub projector get; };",
        );
        let modules = [origin, relay.clone(), forward.clone(), ordinary.clone()]
            .into_iter()
            .map(|module| {
                (
                    PathBuf::from(format!("{}.kio", module_path_surface(&module.path))),
                    module,
                )
            })
            .collect::<Vec<_>>();
        let (counterpart, owner) = resolve_retyped_counterpart(&relay, "Foo", &modules)
            .expect("ordinary identity alias reaches the explicit origin label");
        assert_eq!(module_path_surface(&owner.path), "origin");
        assert_eq!(counterpart.label(), Some("foo"));
        assert!(matches!(
            counterpart.declaration(),
            std::borrow::Cow::Owned(_)
        ));
        assert!(resolve_retyped_counterpart(&forward, "Foo", &modules).is_none());
        let mut check = DeferredRetypeCheck {
            dep_file_path: PathBuf::from("widget.dep.kio"),
            span: source.meta.span,
            from: source.path.clone(),
            to: relay.path.clone(),
            from_newtypes: vec![declared_retype_nominals(&source)[0].snapshot()],
        };
        validate_retype_label_counterpart(&check, &check.from_newtypes[0], &counterpart)
            .expect("explicit label-to-label counterpart");
        let (counterpart, _) = resolve_retyped_counterpart(&ordinary, "Foo", &modules)
            .expect("ordinary newtype has a declaration shape");
        check.to = ordinary.path.clone();
        let error =
            validate_retype_label_counterpart(&check, &check.from_newtypes[0], &counterpart)
                .expect_err(
                    "equal member names and payload do not make an ordinary newtype a label",
                );
        assert!(matches!(error.error, Error::Dep(_)));
        assert!(error.error.diag().1.contains("label-generated counterpart"));
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_label_decomposition_preserves_later_owners_and_exact_terminal_edges() {
        let mut source = parsed_module(
            "module widget/store; \
             pub labels { foo: . }; \
             pub labels Later[A] = { foo: _, box[A]: A }; \
             pub labels Still[A] = { box[A]: _ }; \
             type {untouched} = {foo}; \
             pub fn keep(value: Foo) -> Foo { value }",
        );
        let still = source.items[2].clone();
        let untouched = source.items[3].clone();
        let body = source.items[4].clone();
        let origin = parsed_module("module core/origin; pub labels { foo: . };");
        let relay = parsed_module(
            "module core/relay; import core/origin as origin; pub type Foo = origin.Foo;",
        );
        let target = RetypeTarget {
            from: source.path.clone(),
            newtype_names: ["Foo".to_owned()].into_iter().collect(),
            to: relay.path.clone(),
            span: Span::new(1, 2),
        };
        let rewrite = apply_retype(&mut source, &[target]).expect("source-local decomposition");
        assert_eq!(rewrite.forwards.len(), 1);
        assert_eq!(rewrite.imports.len(), 1);
        assert_eq!(module_path_surface(&rewrite.imports[0].to), "core/relay");
        assert!(source.items.iter().any(|item| item == &still));
        assert!(source.items.iter().any(|item| item == &untouched));
        assert!(source.items.iter().any(|item| item == &body));
        let all = vec![
            (PathBuf::from("origin.kio"), origin),
            (PathBuf::from("relay.kio"), relay),
        ];
        let imports = finalize_retyped_label_forwards(&mut source, rewrite.forwards, &all);
        assert_eq!(imports.len(), 1);
        assert_eq!(module_path_surface(&imports[0].to), "core/origin");
        assert!(source.items.iter().any(|item| {
            matches!(item, Item::LabelForward(forward, ()) if forward.name == "foo" && forward.target == "_rehost_gdgphcgfcpgphcgjghgjgo.foo")
        }));
        assert!(source.items.iter().any(|item| item == &untouched));
        let fresh = parsed_module(&crate::pretty::pretty_module(&source));
        let mut modules = all;
        modules.push((PathBuf::from("store.kio"), fresh));
        let lowered =
            lower_retype_type_projection(modules).expect("fresh ordinary Surface label edges");
        let package = retype_projection_package(lowered).expect("ordinary nominal scopes");
        package.resolve_imports().expect("fresh exact imports");
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_label_forward_slots_survive_recursive_owner_partitioning() {
        let mut source = parsed_module(
            "module widget/store; \
             pub labels { foo: . }; \
             pub rec labels Later = { foo: _ } | { bar: Later }; \
             type {untouched} = {foo}; \
             pub fn keep(value: Later) -> Later { value }",
        );
        let original_forward = source.items[2].clone();
        let original_body = source.items[3].clone();
        let target = RetypeTarget {
            from: source.path.clone(),
            newtype_names: ["Foo".to_owned()].into_iter().collect(),
            to: module_path("core/store"),
            span: Span::new(1, 2),
        };
        let rewrite = apply_retype(&mut source, &[target]).expect("residual Later/Bar recursion");
        let all = vec![(
            PathBuf::from("core.kio"),
            parsed_module("module core/store; pub labels { foo: . };"),
        )];
        finalize_retyped_label_forwards(&mut source, rewrite.forwards, &all);
        let generated = source
            .items
            .iter()
            .position(
                |item| matches!(item, Item::LabelForward(forward, ()) if forward.name == "foo"),
            )
            .unwrap();
        let group = source
            .items
            .iter()
            .position(|item| matches!(item, Item::TypeRecGroup(_)))
            .unwrap();
        let untouched = source
            .items
            .iter()
            .position(|item| item == &original_forward)
            .unwrap();
        assert!(generated < group && group < untouched);
        assert_eq!(source.items.last(), Some(&original_body));
        let Item::TypeRecGroup(group) = &source.items[group] else {
            unreachable!()
        };
        assert_eq!(group.members.len(), 2);
        assert!(group.members.iter().any(|member| {
            matches!(member, crate::ast::TypeRecMember::Labels(labels, ()) if labels.entries[0].name == "bar")
        }));
        assert!(group.members.iter().any(|member| {
            matches!(member, crate::ast::TypeRecMember::TypeAlias(alias) if alias.name == "Later")
        }));
        let fresh = parsed_module(&crate::pretty::pretty_module(&source));
        let mut all = all;
        all.push((PathBuf::from("store.kio"), fresh));
        lower_retype_type_projection(all).expect("fresh source retains the ordinary residual SCC");
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_label_forward_slots_follow_resized_explicit_recursive_groups() {
        let mut source = parsed_module(
            "module widget/store; \
             rec { \
               pub labels { foo: Bar }; \
               pub newtype Bar : Foo | Link { pub constructor mk; pub projector get; }; \
               pub newtype Link : Bar { pub constructor mk; pub projector get; }; \
             } \
             type {untouched} = {foo}; \
             pub fn keep(value: Bar) -> Bar { value }",
        );
        let original_forward = source.items[1].clone();
        let original_body = source.items[2].clone();
        let Item::TypeRecGroup(original_group) = &source.items[0] else {
            panic!("the selected label starts inside an explicit recursive group");
        };
        assert_eq!(original_group.members.len(), 3);
        assert!(matches!(
            original_group.members[0],
            crate::ast::TypeRecMember::Labels(_, ())
        ));
        let target = RetypeTarget {
            from: source.path.clone(),
            newtype_names: ["Foo".to_owned()].into_iter().collect(),
            to: module_path("core/store"),
            span: Span::new(1, 2),
        };
        let rewrite = apply_retype(&mut source, &[target])
            .expect("the external Foo alias separates from the retained Bar/Link SCC");
        assert_eq!(rewrite.forwards.len(), 1);
        assert_eq!(rewrite.forwards[0].before_item, 2);
        assert_eq!(
            source.items.len(),
            4,
            "one group becomes an alias and a smaller group"
        );
        let all = vec![(
            PathBuf::from("core/store.kio"),
            parsed_module("module core/store; pub labels { foo: . };"),
        )];
        finalize_retyped_label_forwards(&mut source, rewrite.forwards, &all);
        assert!(matches!(&source.items[0], Item::TypeAlias(alias) if alias.name == "Foo"));
        let Item::TypeRecGroup(group) = &source.items[1] else {
            panic!("the residual recursive group follows the acyclic alias");
        };
        assert_eq!(group.members.len(), 2);
        assert!(
            matches!(&group.members[0], crate::ast::TypeRecMember::Newtype(newtype) if newtype.name == "Bar")
        );
        assert!(
            matches!(&group.members[1], crate::ast::TypeRecMember::Newtype(newtype) if newtype.name == "Link")
        );
        assert!(
            matches!(&source.items[2], Item::LabelForward(forward, ()) if forward.name == "foo")
        );
        assert_eq!(source.items[3], original_forward);
        assert_eq!(source.items[4], original_body);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_private_label_forward_keeps_the_empty_owner_slot() {
        let mut source = parsed_module(
            "module widget/store; labels { foo: . }; fn keep(value: Foo) -> Foo { value }",
        );
        let body = source.items[1].clone();
        let target = RetypeTarget {
            from: source.path.clone(),
            newtype_names: ["Foo".to_owned()].into_iter().collect(),
            to: module_path("core/store"),
            span: Span::new(1, 2),
        };
        let rewrite = apply_retype(&mut source, &[target]).unwrap();
        assert_eq!(source.items.as_slice(), std::slice::from_ref(&body));
        assert_eq!(rewrite.forwards[0].before_item, 0);
        assert_eq!(selective_import_names(&source, "core/store"), ["Foo"]);
        let all = vec![(
            PathBuf::from("core.kio"),
            parsed_module("module core/store; pub labels { foo: . };"),
        )];
        finalize_retyped_label_forwards(&mut source, rewrite.forwards, &all);
        assert!(
            matches!(&source.items[0], Item::LabelForward(forward, ()) if !forward.vis.is_pub())
        );
        assert_eq!(source.items[1], body);
        assert!(
            !source
                .items
                .iter()
                .any(|item| matches!(item, Item::TypeAlias(alias) if alias.name == "Foo"))
        );
    }

    #[test]
    fn retype_chain_preserves_each_newtypes_terminal_module() {
        let names = ["A".to_owned(), "B".to_owned()].into_iter().collect();
        let later = retype_decl("w2/store", "core/store", Some("A"));
        let mut graph = RetypeGraph::build(&[later]);
        let targets = retype_chain_targets(&module_path("w2/store"), &names, &mut graph);

        assert_eq!(target_names(&targets, "core/store"), ["A"]);
        assert_eq!(target_names(&targets, "w2/store"), ["B"]);
    }

    #[test]
    fn retype_chain_is_name_exact_across_order_module_hops_and_cycles() {
        let names = ["A".to_owned(), "B".to_owned()].into_iter().collect();
        let per_type = [
            retype_decl("w2/store", "core/b", Some("B")),
            retype_decl("w2/store", "core/a", Some("A")),
        ];
        let mut graph = RetypeGraph::build(&per_type);
        let targets = retype_chain_targets(&module_path("w2/store"), &names, &mut graph);
        assert_eq!(target_names(&targets, "core/a"), ["A"]);
        assert_eq!(target_names(&targets, "core/b"), ["B"]);
        let mut reversed = per_type.to_vec();
        reversed.reverse();
        let mut graph = RetypeGraph::build(&reversed);
        let reversed_targets = retype_chain_targets(&module_path("w2/store"), &names, &mut graph);
        assert_eq!(target_names(&reversed_targets, "core/a"), ["A"]);
        assert_eq!(target_names(&reversed_targets, "core/b"), ["B"]);

        let module_chain = [
            retype_decl("w2/store", "core/store", None),
            retype_decl("core/store", "origin/store", None),
        ];
        let mut graph = RetypeGraph::build(&module_chain);
        let targets = retype_chain_targets(&module_path("w2/store"), &names, &mut graph);
        assert_eq!(target_names(&targets, "origin/store"), ["A", "B"]);

        let cycle = [
            retype_decl("w2/store", "w3/store", None),
            retype_decl("w3/store", "w2/store", None),
        ];
        let mut graph = RetypeGraph::build(&cycle);
        let targets = retype_chain_targets(&module_path("w2/store"), &names, &mut graph);
        assert_eq!(target_names(&targets, "w2/store"), ["A", "B"]);
    }

    #[test]
    fn retype_chain_graph_visits_each_expanded_edge_once_and_memoizes_suffixes() {
        const CHAIN_LEN: usize = 128;
        const NAME_COUNT: usize = 128;
        let declarations = (0..CHAIN_LEN)
            .map(|index| {
                retype_decl(
                    &format!("dep/m{index}"),
                    &format!("dep/m{}", index + 1),
                    None,
                )
            })
            .collect::<Vec<_>>();
        let names = (0..NAME_COUNT)
            .map(|index| format!("T{index}"))
            .collect::<BTreeSet<_>>();
        let mut graph = RetypeGraph::build(&declarations);
        assert_eq!(graph.work(), (CHAIN_LEN, 0));

        let targets = retype_chain_targets(&module_path("dep/m0"), &names, &mut graph);
        assert_eq!(target_names(&targets, "dep/m128").len(), NAME_COUNT);
        let first_work = graph.work();
        assert_eq!(
            first_work,
            (CHAIN_LEN, NAME_COUNT * (CHAIN_LEN + 1)),
            "one indexed lookup per reachable exact (module, name) node"
        );
        let former_linear_candidate_visits =
            NAME_COUNT * (CHAIN_LEN * (CHAIN_LEN + 1) / 2 + CHAIN_LEN);
        assert_eq!(former_linear_candidate_visits, 1_073_152);
        assert!(
            former_linear_candidate_visits > first_work.1 * 64,
            "the indexed graph must retain the measured declaration-choice reduction"
        );

        let repeated = retype_chain_targets(&module_path("dep/m0"), &names, &mut graph);
        assert_eq!(target_names(&repeated, "dep/m128").len(), NAME_COUNT);
        assert_eq!(
            graph.work(),
            first_work,
            "completed exact identities must reuse their terminal cache"
        );
    }

    #[test]
    fn retype_split_imports_preserve_item_and_closing_comments() {
        let targets = ["A", "B"]
            .into_iter()
            .map(|name| RetypeTarget {
                from: module_path("widget/store"),
                newtype_names: [name.to_owned()].into_iter().collect(),
                to: module_path(if name == "A" { "core/a" } else { "core/b" }),
                span: Span::new(0, 0),
            })
            .collect::<Vec<_>>();
        for keep in [false, true] {
            let retained = if keep { "// selection C\n, C\n" } else { "" };
            let source = format!(
                "module widget/relay;\n// statement\nimport widget/store(\n\
                 // selection A\n, A\n// selection B\n, B\n\
                 {retained}// closing boundary\n);\n"
            );
            let mut module = parsed_module(&source);
            apply_retype(&mut module, &targets).expect("split retype import");
            assert_eq!(module.imports.len(), if keep { 3 } else { 2 });
            for (index, import) in module.imports.iter().enumerate() {
                assert_eq!(!import.leading_trivia.is_empty(), index == 0);
                assert_eq!(
                    !import.trailing_trivia.is_empty(),
                    index + 1 == module.imports.len()
                );
            }
            let formatted = crate::pretty::pretty_module(&module);
            for comment in [
                "statement",
                "selection A",
                "selection B",
                "closing boundary",
            ] {
                assert_eq!(formatted.matches(comment).count(), 1, "{formatted}");
            }
            assert_eq!(formatted.matches("selection C").count(), usize::from(keep));
            assert!(formatted.contains("// selection A\n  , A"), "{formatted}");
            assert!(formatted.contains("// selection B\n  , B"), "{formatted}");
            assert!(
                formatted.contains("\n  // closing boundary\n  );"),
                "{formatted}"
            );
            assert_eq!(
                crate::pretty::pretty_module(&parsed_module(&formatted)),
                formatted,
            );
        }
    }

    #[test]
    fn retype_redirects_each_imported_name_to_its_own_target() {
        let source = module_path("widget/store");
        for order in [["A", "B"], ["B", "A"]] {
            let targets = order
                .into_iter()
                .map(|name| RetypeTarget {
                    from: source.clone(),
                    newtype_names: [name.to_owned()].into_iter().collect(),
                    to: module_path(if name == "A" { "core/a" } else { "core/b" }),
                    span: Span::new(0, 0),
                })
                .collect::<Vec<_>>();
            let mut module = parsed_module("module widget/relay; import widget/store(A, B, C);");
            apply_retype(&mut module, &targets).expect("per-type redirects");

            assert_eq!(selective_import_names(&module, "core/a"), ["A"]);
            assert_eq!(selective_import_names(&module, "core/b"), ["B"]);
            assert_eq!(selective_import_names(&module, "widget/store"), ["C"]);
        }

        let mut module = parsed_module("module widget/relay; import widget/store(A, B, C);");
        let module_target = RetypeTarget {
            from: module_path("widget/store"),
            newtype_names: ["A".to_owned(), "B".to_owned()].into_iter().collect(),
            to: module_path("core/store"),
            span: Span::new(0, 0),
        };
        apply_retype(&mut module, &[module_target]).expect("module-form redirect");
        assert_eq!(selective_import_names(&module, "core/store"), ["A", "B"]);
        assert_eq!(selective_import_names(&module, "widget/store"), ["C"]);
    }

    #[test]
    fn module_retype_removes_and_reexports_grouped_newtypes() {
        let mut module = parsed_module(
            "module dep/store; \
             rec { \
               pub type Link = Odd; \
               pub newtype Even : . | Link { pub constructor mk_even; pub projector un_even; }; \
               pub newtype Odd : . | Even { pub constructor mk_odd; pub projector un_odd; }; \
             }",
        );
        let target_module = parsed_module("module core/store;");
        let target = RetypeTarget {
            from: module.path.clone(),
            newtype_names: ["Even".to_owned(), "Odd".to_owned()].into_iter().collect(),
            to: target_module.path.clone(),
            span: Span::new(0, 0),
        };

        apply_retype(&mut module, &[target]).expect("grouped retype");

        assert!(
            module
                .items
                .iter()
                .all(|item| !matches!(item, Item::TypeRecGroup(_))),
            "the residual acyclic alias must be emitted as an ordinary declaration"
        );
        for expected in ["Link", "Even", "Odd"] {
            assert!(
                module
                    .items
                    .iter()
                    .any(|item| matches!(item, Item::TypeAlias(alias) if alias.name == expected)),
                "missing retained or re-exported alias {expected}"
            );
        }
        assert!(selective_import_names(&module, "core/store").is_empty());
        let alias_position = |name: &str| {
            module
                .items
                .iter()
                .position(|item| matches!(item, Item::TypeAlias(alias) if alias.name == name))
                .unwrap()
        };
        assert!(alias_position("Odd") < alias_position("Link"));
    }

    #[test]
    fn retype_public_alias_keeps_its_slot_and_private_heads_remain_import_only() {
        let mut module = parsed_module(
            "module dep/store; \
             fn _rehost_gdgphcgfcphdhegphcgf() -> . { () } \
             pub newtype Box[A] <X> : A & X { pub constructor make; pub projector open; }; \
             pub fn keep[A](value: Box(A)) -> Box(A) { value } \
             newtype Hidden : . { constructor make; projector open; }; \
             fn keep_hidden(value: Hidden) -> Hidden { value }",
        );
        let original = declared_newtype(&module, "Box").unwrap();
        let span = original.name_span;
        let parameters = original.type_params.clone();
        let counterpart = parsed_module(
            "module core/store; \
             pub newtype Box[A] <Y> : A & Y { pub constructor make; pub projector open; }; \
             pub newtype Hidden : . { pub constructor make; pub projector open; };",
        );
        let target = RetypeTarget {
            from: module.path.clone(),
            newtype_names: ["Box".to_owned(), "Hidden".to_owned()]
                .into_iter()
                .collect(),
            to: counterpart.path.clone(),
            span: Span::new(0, 0),
        };
        let imports =
            apply_retype(&mut module, &[target]).expect("ordinary retype materialization");
        assert_eq!(
            imports.imports.len(),
            2,
            "both target-head visibility edges remain explicit"
        );
        assert_eq!(selective_import_names(&module, "core/store"), ["Hidden"]);
        let Item::TypeAlias(alias) = &module.items[1] else {
            panic!("public head keeps its slot");
        };
        assert_eq!(alias.name, "Box");
        assert_eq!(alias.name_span, span);
        assert_eq!(alias.type_params, parameters);
        let Type::Path { segments, args, .. } = &alias.body else {
            panic!("ordinary identity alias");
        };
        assert_eq!(segments, &["_rehost_gdgphcgfcphdhegphcgf_n2", "Box"]);
        assert_eq!(
            args.len(),
            1,
            "existentials belong to the target members, not alias binders"
        );
        assert!(matches!(&module.items[2], Item::FnDef(function) if function.name == "keep"));
        assert!(
            matches!(&module.items[3], Item::FnDef(function) if function.name == "keep_hidden")
        );
        assert_eq!(module.items.len(), 4);
        let reparsed = parsed_module(&crate::pretty::pretty_module(&module));
        #[cfg(feature = "prime")]
        {
            let modules = [("dep/store.kio", reparsed), ("core/store.kio", counterpart)]
                .into_iter()
                .map(|(file, module)| {
                    (
                        PathBuf::from(file),
                        crate::prime::lower::lower_module(module).unwrap(),
                    )
                })
                .collect();
            let package =
                Package::build(Path::new(""), modules, None).expect("fresh Prime package");
            package
                .resolve_imports()
                .expect("ordinary provider imports");
            package
                .check_in_body_resolution()
                .expect("users see the replacement at the original slot");
        }
        #[cfg(not(feature = "prime"))]
        let _ = (reparsed, counterpart);
    }

    #[test]
    fn retype_reroots_and_preserves_scoped_newtype_visibility() {
        let upstream = parsed_module(
            "module store; \
             pub(store) newtype Token : . { \
               pub(store) constructor mk; pub(store) projector get; \
             };",
        );
        let mut snapshot = declared_newtype(&upstream, "Token")
            .expect("upstream newtype")
            .clone();
        reroot_newtype_visibilities(&mut snapshot, "widget");
        for visibility in [
            &snapshot.vis,
            &snapshot.constructor.vis,
            &snapshot.projector.vis,
        ] {
            let crate::ast::Visibility::PublicIn(path) = visibility else {
                panic!("scoped visibility remains scoped");
            };
            assert_eq!(module_path_surface(path), "widget/store");
        }

        let mut materialized = parsed_module(
            "module widget/store; \
             pub(widget/store) newtype Token : . { constructor mk; projector get; };",
        );
        let target = parsed_module("module core/store;");
        let materialized_path = materialized.path.clone();
        apply_retype(
            &mut materialized,
            &[RetypeTarget {
                from: materialized_path,
                newtype_names: ["Token".to_owned()].into_iter().collect(),
                to: target.path,
                span: Span::new(0, 0),
            }],
        )
        .expect("scoped retype");
        let alias = materialized
            .items
            .iter()
            .find_map(|item| match item {
                Item::TypeAlias(alias) => Some(alias),
                _ => None,
            })
            .expect("reexport alias");
        let crate::ast::Visibility::PublicIn(path) = &alias.vis else {
            panic!("reexport retains scoped visibility");
        };
        assert_eq!(module_path_surface(path), "widget/store");
    }

    #[test]
    fn grouped_newtype_is_a_retype_counterpart() {
        let module = parsed_module(
            "module core/store; \
             rec { \
               newtype Even : . | Odd { constructor mk_even; projector un_even; }; \
               newtype Odd : . | Even { constructor mk_odd; projector un_odd; }; \
             }",
        );
        let all = vec![(PathBuf::from("store.kio"), module.clone())];
        let (counterpart, owner) = resolve_retyped_counterpart(&module, "Even", &all)
            .expect("recursive-group members are declarations of their module");
        assert_eq!(counterpart.name(), "Even");
        assert!(counterpart.label().is_none());
        let std::borrow::Cow::Borrowed(declaration) = counterpart.declaration() else {
            panic!("ordinary counterparts borrow the original declaration");
        };
        assert!(std::ptr::eq(
            declaration,
            declared_newtype(&module, "Even").unwrap()
        ));
        assert_eq!(owner.path.segments, module.path.segments);
    }

    fn canonical_retype_fixture(
        from: Module<Surface>,
        to: Module<Surface>,
        names: &[&str],
    ) -> (DeferredRetypeCheck, RetypeSemanticPackage) {
        let (check, semantics) = canonical_retype_fixture_result(from, to, names);
        (check, semantics.expect("fixture semantic projection"))
    }

    fn canonical_retype_fixture_result(
        mut from: Module<Surface>,
        to: Module<Surface>,
        names: &[&str],
    ) -> (
        DeferredRetypeCheck,
        Result<RetypeSemanticPackage, LocatedError>,
    ) {
        let source_module = from.clone();
        let snapshots = names
            .iter()
            .map(|name| RetypedNewtypeSnapshot {
                declaration: declared_newtype(&from, name)
                    .unwrap_or_else(|| panic!("missing source newtype {name}"))
                    .clone(),
                label: None,
            })
            .collect::<Vec<_>>();
        let target = RetypeTarget {
            from: from.path.clone(),
            newtype_names: names.iter().map(|name| (*name).to_owned()).collect(),
            to: to.path.clone(),
            span: Span::new(0, 0),
        };
        let check = DeferredRetypeCheck {
            dep_file_path: PathBuf::from("fixture.dep.kio"),
            span: target.span,
            from: target.from.clone(),
            to: target.to.clone(),
            from_newtypes: snapshots,
        };
        apply_retype(&mut from, &[target]).expect("fixture retype rewrite");
        let materialized = vec![
            (PathBuf::from("dep/store.kio"), from),
            (PathBuf::from("core/store.kio"), to),
        ];
        let source_modules = BTreeMap::from([(
            module_path_surface(&source_module.path),
            (
                PathBuf::from("dep/store.kio"),
                retype_type_projection(source_module),
            ),
        )]);
        let semantics = RetypeSemanticPackage::build(
            std::slice::from_ref(&check),
            &source_modules,
            &materialized,
        );
        (check, semantics)
    }

    #[test]
    fn retype_source_semantics_preserve_order_and_recursive_scope() {
        let invalid = parsed_module(
            "module dep/store; \
             pub newtype Node : Later { pub constructor mk_node; pub projector get_node; }; \
             pub newtype Later : . { pub constructor mk_later; pub projector get_later; };",
        );
        let target = parsed_module(
            "module core/store; \
             pub newtype Later : . { pub constructor mk_later; pub projector get_later; }; \
             pub newtype Node : Later { pub constructor mk_node; pub projector get_node; };",
        );
        let (_, result) =
            canonical_retype_fixture_result(invalid, target.clone(), &["Node", "Later"]);
        let error = match result {
            Ok(_) => panic!("retype must not erase an illegal source forward reference"),
            Err(error) => error,
        };
        assert_eq!(error.file_path, PathBuf::from("dep/store.kio"));
        assert!(matches!(&error.error, Error::Dep(_)));
        assert!(
            error
                .error
                .diagnostic()
                .message
                .contains("`Later` is declared later and is not visible here")
        );

        let ordered = parsed_module(
            "module dep/store; \
             pub newtype Later : . { pub constructor mk_later; pub projector get_later; }; \
             pub newtype Node : Later { pub constructor mk_node; pub projector get_node; };",
        );
        let (_, result) = canonical_retype_fixture_result(ordered, target, &["Node", "Later"]);
        result.expect("an earlier source declaration remains visible to retype validation");

        let grouped = parsed_module(
            "module dep/store; \
             rec { \
               pub type Payload = . | Node; \
               pub newtype Node : Payload { pub constructor mk_node; pub projector get_node; }; \
             }",
        );
        let grouped_target = parsed_module(
            "module core/store; \
             rec { \
               pub type Payload = . | Node; \
               pub newtype Node : Payload { pub constructor mk_node; pub projector get_node; }; \
             }",
        );
        let (_, result) = canonical_retype_fixture_result(grouped, grouped_target, &["Node"]);
        result.expect("an explicit rec group keeps selected and retained peers in scope");

        let importing_source = parsed_module(
            "module dep/consumer; \
             import dep/provider(Peer); \
             pub newtype Holder : Peer { pub constructor mk_holder; pub projector get_holder; };",
        );
        let provider_source = parsed_module(
            "module dep/provider; \
             pub newtype Peer : . { pub constructor mk_peer; pub projector get_peer; };",
        );
        let second_source = parsed_module(
            "module dep/second; \
             pub newtype Other : . { pub constructor mk_other; pub projector get_other; };",
        );
        let unrelated_invalid = parsed_module(
            "module app/broken; \
             type Duplicate = .; \
             type Duplicate = .;",
        );
        let source_modules = BTreeMap::from([
            (
                module_path_surface(&importing_source.path),
                (
                    PathBuf::from("dep/consumer.kio"),
                    retype_type_projection(importing_source.clone()),
                ),
            ),
            (
                module_path_surface(&second_source.path),
                (
                    PathBuf::from("dep/second.kio"),
                    retype_type_projection(second_source.clone()),
                ),
            ),
        ]);
        let materialized = vec![
            (PathBuf::from("dep/consumer.kio"), importing_source),
            (PathBuf::from("dep/provider.kio"), provider_source),
            (PathBuf::from("dep/second.kio"), second_source),
            (PathBuf::from("app/broken.kio"), unrelated_invalid),
        ];
        validate_retype_source_semantics(&source_modules, &materialized).expect(
            "multiple source modules retain imports from context-only providers without claiming unrelated body errors",
        );
    }

    #[test]
    fn lone_member_retype_does_not_conflate_grouped_peer_identities() {
        let from = parsed_module(
            "module dep/store; \
             rec { \
               newtype Even : . | Odd { constructor mk_even; projector un_even; }; \
               newtype Odd : . | Even { constructor mk_odd; projector un_odd; }; \
             }",
        );
        let to = parsed_module(
            "module core/store; \
             rec { \
               newtype Even : . | Odd { constructor mk_even; projector un_even; }; \
               newtype Odd : . | Even { constructor mk_odd; projector un_odd; }; \
             }",
        );
        let (_, semantics) = canonical_retype_fixture(from.clone(), to.clone(), &["Even"]);
        let pair = &semantics.pairs[0][0];
        assert!(
            !canonical_retype_payloads_congruent(&pair.from, &pair.to, &semantics.package),
            "unselected grouped peers have distinct nominal identities"
        );

        let (_, semantics) = canonical_retype_fixture(from, to, &["Even", "Odd"]);
        assert!(semantics.pairs[0].iter().all(|pair| {
            canonical_retype_payloads_congruent(&pair.from, &pair.to, &semantics.package)
        }));
    }

    #[test]
    fn retype_congruence_expands_recursive_structural_aliases() {
        let source = parsed_module(
            "module dep/store; \
             rec { \
               pub type Payload = . | Node; \
               pub newtype Node : Payload { pub constructor mk_node; pub projector un_node; }; \
             }",
        );
        let target = parsed_module(
            "module core/store; \
             rec { \
               pub type Payload = . | Node; \
               pub newtype Node : Payload { pub constructor mk_node; pub projector un_node; }; \
             }",
        );
        let (check, semantics) = canonical_retype_fixture(source, target, &["Node"]);
        validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect("transparent structural aliases are expanded before comparison");
    }

    #[test]
    fn retype_congruence_rejects_header_binder_kind_mismatch() {
        let source = parsed_module(
            "module dep/store; \
             pub newtype Token[*F] : [*G] G(.) -> G(.) { pub constructor mk; pub projector get; };",
        );

        let header_mismatch = parsed_module(
            "module core/store; \
             pub newtype Token[A] : [*H] H(.) -> H(.) { pub constructor mk; pub projector get; };",
        );
        let (check, semantics) = canonical_retype_fixture(source, header_mismatch, &["Token"]);
        let error = validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect_err("a header binder's effective kind is part of congruence");
        assert!(error.error.diag().1.contains(
            "corresponding universal type-parameter kinds differ at binder 1 (*→* vs *)"
        ));
    }

    #[test]
    fn retype_congruence_rejects_nested_forall_kind_mismatch_with_identical_bodies() {
        let source = parsed_module(
            "module dep/store; \
             pub newtype Token[*F] : [*G] . -> . { pub constructor mk; pub projector get; };",
        );

        let nested_mismatch = parsed_module(
            "module core/store; \
             pub newtype Token[*A] : [B] . -> . { pub constructor mk; pub projector get; };",
        );
        let (check, semantics) = canonical_retype_fixture(source, nested_mismatch, &["Token"]);
        validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect_err("a payload binder's effective kind is part of congruence");
    }

    #[test]
    fn retype_congruence_matches_same_kind_binders_alpha_equivalently() {
        let source = parsed_module(
            "module dep/store; \
             pub newtype Token[*F] : [*G] G(.) -> G(.) { pub constructor mk; pub projector get; };",
        );

        let alpha_renamed = parsed_module(
            "module core/store; \
             pub newtype Token[*A] : [*H] H(.) -> H(.) { pub constructor mk; pub projector get; };",
        );
        let (check, semantics) = canonical_retype_fixture(source, alpha_renamed, &["Token"]);
        validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect("equal-kind binders compare alpha-equivalently");
    }

    #[test]
    fn retype_congruence_rejects_universal_existential_classification_mismatch() {
        let source = parsed_module(
            "module dep/store; \
             pub newtype Pack <Hidden> : Hidden { pub constructor mk; pub projector get; };",
        );
        let target = parsed_module(
            "module core/store; \
             pub newtype Pack[Visible] : Visible { pub constructor mk; pub projector get; };",
        );
        let (check, semantics) = canonical_retype_fixture(source, target, &["Pack"]);
        let error = validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect_err("universal and existential binder counts have distinct roles");
        assert!(error.error.diag().1.contains(
            "type-parameter arities differ (0 universal / 1 existential vs 1 universal / 0 existential)"
        ));
    }

    #[test]
    fn retype_congruence_accepts_matching_star_and_existential_binders() {
        let star_source = parsed_module(
            "module dep/store; \
             pub newtype Box[A] : A { pub constructor mk; pub projector get; };",
        );
        let star_target = parsed_module(
            "module core/store; \
             pub newtype Box[B] : B { pub constructor mk; pub projector get; };",
        );
        let (check, semantics) = canonical_retype_fixture(star_source, star_target, &["Box"]);
        validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect("nonzero all-Star universal binders compare alpha-equivalently");

        let existential_source = parsed_module(
            "module dep/store; \
             pub newtype Pack <Hidden> : Hidden { pub constructor mk; pub projector get; };",
        );
        let existential_target = parsed_module(
            "module core/store; \
             pub newtype Pack <Secret> : Secret { pub constructor mk; pub projector get; };",
        );
        let (check, semantics) =
            canonical_retype_fixture(existential_source, existential_target, &["Pack"]);
        validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect("matching existential binders compare alpha-equivalently");
    }

    #[test]
    fn retype_counterpart_visibility_covers_the_rebound_surface() {
        let public_source = parsed_module(
            "module dep/store; \
             pub newtype Token : . { pub constructor mk; pub projector get; };",
        );
        let private_target = parsed_module(
            "module core/store; \
             newtype Token : . { pub constructor mk; pub projector get; };",
        );
        let (check, semantics) =
            canonical_retype_fixture(public_source.clone(), private_target, &["Token"]);
        validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect_err("a private counterpart cannot preserve a public source type");

        let scoped_source = parsed_module(
            "module core/client; \
             newtype Token : . { constructor mk; projector get; };",
        );
        let scoped_target = parsed_module(
            "module core/store; \
             pub(core) newtype Token : . { \
               pub(core) constructor mk; pub(core) projector get; \
             };",
        );
        let (check, semantics) =
            canonical_retype_fixture(scoped_source, scoped_target.clone(), &["Token"]);
        validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect("a private source may bind to a counterpart visible in its module");

        let public_scoped_source = parsed_module(
            "module core/client; \
             pub newtype Token : . { constructor mk; projector get; };",
        );
        let (check, semantics) =
            canonical_retype_fixture(public_scoped_source, scoped_target, &["Token"]);
        validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect_err("a scoped counterpart cannot preserve a globally public source type");

        let public_target = parsed_module(
            "module core/store; \
             pub newtype Token : . { pub constructor mk; pub projector get; };",
        );
        let (check, semantics) = canonical_retype_fixture(public_source, public_target, &["Token"]);
        validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect("a public counterpart preserves a public source type");
    }

    fn retype_interface_fixture(source: &str, target: &str) -> Result<(), LocatedError> {
        let source = parsed_module(source);
        let target = parsed_module(target);
        let check = DeferredRetypeCheck {
            dep_file_path: PathBuf::from("widget.dep.kio"),
            span: Span::new(0, 1),
            from: source.path.clone(),
            to: target.path.clone(),
            from_newtypes: vec![RetypedNewtypeSnapshot {
                declaration: declared_newtype(&source, "Token").unwrap().clone(),
                label: None,
            }],
        };
        validate_retype_interface(
            &[check],
            &[],
            &[
                (PathBuf::from("source.kio"), source),
                (PathBuf::from("target.kio"), target),
            ],
        )
    }

    #[test]
    fn retype_member_interface_checks_corresponding_roles() {
        for role in ["constructor", "projector"] {
            for visibility in ["pub", "pub(widget/feature)"] {
                let source = format!(
                    "module widget/feature/store; pub newtype Token : . {{ \
                     {visibility} {role} required; {} other; }};",
                    if role == "constructor" {
                        "projector"
                    } else {
                        "constructor"
                    },
                );
                for (target_roles, message) in [
                    (
                        "pub constructor other; pub projector required;",
                        "constructor",
                    ),
                    (
                        "pub constructor required; pub projector other;",
                        "projector",
                    ),
                ] {
                    let result = retype_interface_fixture(
                        &source,
                        &format!(
                            "module widget/feature/target; pub newtype Token : . {{ {target_roles} }};"
                        ),
                    );
                    if role == message {
                        let error = result.expect_err("the opposite role cannot supply a member");
                        assert!(matches!(error.error, Error::Dep(_)));
                        assert!(
                            error
                                .error
                                .diag()
                                .1
                                .contains(&format!("requires {role} `required`"))
                        );
                    } else {
                        result.expect("the corresponding role preserves the exposed name");
                    }
                }
                let error = retype_interface_fixture(
                    &source,
                    &format!(
                        "module widget/feature/target; pub newtype Token : . {{ \
                         pub(widget/feature/target) {role} required; {} other; }};",
                        if role == "constructor" {
                            "projector"
                        } else {
                            "constructor"
                        },
                    ),
                )
                .expect_err("a narrower target role loses source access");
                assert!(error.error.diag().1.contains("is not visible everywhere"));
                assert!(error.error.diag().1.contains(&format!("target {role}")));
            }
        }
    }

    #[test]
    fn retype_member_interface_uses_effective_visibility() {
        for (source_outer, source_member, target_outer, target_member, target_name) in [
            ("pub", "", "pub", "", "renamed"),
            ("", "pub", "pub", "", "renamed"),
            (
                "pub(widget/feature)",
                "pub",
                "pub",
                "pub(widget/feature)",
                "mk",
            ),
            (
                "pub(widget/feature)",
                "pub(widget)",
                "pub(widget)",
                "pub(widget/feature)",
                "mk",
            ),
            ("pub", "pub(widget/feature)", "pub", "pub(widget)", "mk"),
        ] {
            retype_interface_fixture(
                &format!(
                    "module widget/feature/store; {source_outer} newtype Token : . {{ \
                     {source_member} constructor mk; projector get; }};"
                ),
                &format!(
                    "module widget/feature/target; {target_outer} newtype Token : . {{ \
                     {target_member} constructor {target_name}; projector unwrap; }};"
                ),
            )
            .expect("only the effective exposed source interface is required");
        }
    }

    #[test]
    fn retype_member_interface_uses_the_exact_terminal_counterpart() {
        let source = parsed_module(
            "module widget/store; pub newtype Token : . { pub constructor mk; projector get; };",
        );
        let target = parsed_module(
            "module core/route; import core/store(Token); import core/store as origin; \
             pub type Token = origin.Token;",
        );
        let terminal = parsed_module(
            "module core/store; pub newtype Token : . { pub constructor renamed; projector get; };",
        );
        let unrelated = parsed_module(
            "module unrelated; pub newtype Token : . { pub constructor mk; pub projector get; };",
        );
        let check = DeferredRetypeCheck {
            dep_file_path: PathBuf::from("widget.dep.kio"),
            span: Span::new(0, 1),
            from: source.path.clone(),
            to: target.path.clone(),
            from_newtypes: vec![RetypedNewtypeSnapshot {
                declaration: declared_newtype(&source, "Token").unwrap().clone(),
                label: None,
            }],
        };
        let error = validate_retype_interface(
            &[check],
            &[],
            &[source, target, terminal, unrelated]
                .into_iter()
                .enumerate()
                .map(|(index, module)| (PathBuf::from(format!("{index}.kio")), module))
                .collect::<Vec<_>>(),
        )
        .expect_err("an unrelated compatible member cannot replace the exact terminal role");
        assert!(error.error.diag().1.contains("target `core/store.Token`"));
        assert!(error.error.diag().1.contains("constructor `renamed`"));
    }

    fn write_retype_fixture(root: &Path, path: &str, source: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture directory");
        fs::write(path, source).expect("fixture source");
    }

    fn retype_materialization_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("retype fixture");
        for (path, source) in [
            ("consumer/app.pkg.kio", "package app;"),
            ("library/widget.pkg.kio", "package widget;"),
            (
                "library/store.kio",
                "module store; pub newtype Token : . { pub constructor mk; pub projector get; };",
            ),
            (
                "consumer/core/store.kio",
                "module core/store; pub newtype Token : . { pub constructor mk; pub projector get; };",
            ),
            (
                "consumer/widget.dep.kio",
                "dependency widget; source { path \"../library/widget.pkg.kio\"; } \
                 retype widget/store.Token to core/store.Token;",
            ),
        ] {
            write_retype_fixture(dir.path(), path, source);
        }
        dir
    }

    #[cfg(feature = "surface")]
    fn label_retype_materialization_fixture() -> tempfile::TempDir {
        let dir = retype_materialization_fixture();
        for (path, source) in [
            (
                "library/store.kio",
                "module store; pub labels { token: . }; pub labels Later = { token: _ };",
            ),
            (
                "consumer/core/store.kio",
                "module core/store; pub labels { token: . };",
            ),
            (
                "consumer/core/relay.kio",
                "module core/relay; import core/store as origin; pub type Token = origin.Token;",
            ),
            (
                "consumer/widget.dep.kio",
                "dependency widget; source { path \"../library/widget.pkg.kio\"; } retype widget/store.Token to core/relay.Token;",
            ),
        ] {
            write_retype_fixture(dir.path(), path, source);
        }
        dir
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_labels_finalize_before_unchanged_and_forced_fetch_comparison() {
        let dir = label_retype_materialization_fixture();
        let root = dir.path().join("consumer");
        let mut first = None;
        for (force, expected) in [
            (false, MaterializeOutcome::Fetched),
            (false, MaterializeOutcome::UpToDate),
            (true, MaterializeOutcome::Fetched),
            (false, MaterializeOutcome::UpToDate),
        ] {
            let outcomes =
                materialize_dependencies_filtered(&root, None, force).expect("label fetch");
            assert_eq!(outcomes["widget"], expected);
            let source = fs::read_to_string(root.join("widget/store.kio")).unwrap();
            assert!(
                source.contains("type {token} = {_rehost_gdgphcgfcphdhegphcgf.token};"),
                "{source}"
            );
            assert!(
                !source.contains("_rehost_gdgphcgfcphcgfgmgbhj.token"),
                "relay exports only the uppercase alias"
            );
            assert!(
                source.contains("type Token = _rehost_gdgphcgfcphcgfgmgbhj.Token;"),
                "{source}"
            );
            if let Some(first) = &first {
                assert_eq!(&source, first);
            } else {
                first = Some(source);
            }
        }
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_labels_finalize_after_both_earlier_and_later_provider_overlays() {
        for (provider, store_alias) in [
            ("acore", "_rehost_gbgdgphcgfcphdhegphcgf"),
            ("zcore", "_rehost_hkgdgphcgfcphdhegphcgf"),
        ] {
            let dir = label_retype_materialization_fixture();
            let root = dir.path().join("consumer");
            write_retype_fixture(dir.path(), "target/target.pkg.kio", "package target;");
            write_retype_fixture(
                dir.path(),
                "target/store.kio",
                "module store; pub labels { token: . };",
            );
            write_retype_fixture(
                dir.path(),
                "target/relay.kio",
                "module relay; import store as origin; pub type Token = origin.Token;",
            );
            write_retype_fixture(
                dir.path(),
                &format!("consumer/{provider}.dep.kio"),
                &format!("dependency {provider}; source {{ path \"../target/target.pkg.kio\"; }}"),
            );
            write_retype_fixture(
                dir.path(),
                "consumer/widget.dep.kio",
                &format!(
                    "dependency widget; source {{ path \"../library/widget.pkg.kio\"; }} retype widget/store.Token to {provider}/relay.Token;"
                ),
            );
            let outcomes = materialize_dependencies_filtered(&root, None, false)
                .expect("complete provider overlay");
            assert_eq!(outcomes["widget"], MaterializeOutcome::Fetched);
            let source = fs::read_to_string(root.join("widget/store.kio")).unwrap();
            assert!(
                source.contains(&format!("type {{token}} = {{{store_alias}.token}};")),
                "{source}"
            );
            assert_eq!(
                materialize_dependencies_filtered(&root, None, false).unwrap()["widget"],
                MaterializeOutcome::UpToDate
            );
        }
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_labels_preserve_missing_and_cyclic_counterpart_publication_timing() {
        for target in [
            "module core/relay;",
            "module core/relay; import core/cycle as other; pub type Token = other.Token;",
        ] {
            let dir = label_retype_materialization_fixture();
            let root = dir.path().join("consumer");
            write_retype_fixture(dir.path(), "consumer/core/relay.kio", target);
            write_retype_fixture(
                dir.path(),
                "consumer/core/cycle.kio",
                "module core/cycle; import core/relay as other; pub type Token = other.Token;",
            );
            assert!(materialize_dependencies_filtered(&root, None, false).is_err());
            let source = fs::read_to_string(root.join("widget/store.kio"))
                .expect("missing/cyclic counterpart failure remains after publication");
            assert!(
                source.contains("type {token} = {_rehost_gdgphcgfcphcgfgmgbhj.token};"),
                "{source}"
            );
        }
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_labels_keep_original_alias_head_access_before_publication() {
        let dir = label_retype_materialization_fixture();
        let root = dir.path().join("consumer");
        materialize_dependencies_filtered(&root, None, false).expect("compatible first fetch");
        let before = fs::read(root.join("widget/store.kio")).unwrap();
        write_retype_fixture(
            dir.path(),
            "library/store.kio",
            "module store; pub labels { token: . }; pub fn added() -> . { () }",
        );
        write_retype_fixture(
            dir.path(),
            "consumer/core/relay.kio",
            "module core/relay; import core/store as origin; pub(core) type Token = origin.Token;",
        );
        for force in [false, true] {
            let error = materialize_dependencies_filtered(&root, None, force).unwrap_err();
            assert!(error.error.diag().1.contains("not importable"));
            assert_eq!(fs::read(root.join("widget/store.kio")).unwrap(), before);
        }
    }

    fn assert_retype_member_interface_preserves_tree(force: bool, existing: bool) {
        let dir = retype_materialization_fixture();
        let root = dir.path().join("consumer");
        let mut before = BTreeMap::new();
        if existing {
            write_retype_fixture(dir.path(), "library/old.kio", "module old;");
            materialize_dependencies_filtered(&root, None, false).expect("compatible first fetch");
            for name in ["store.kio", "old.kio"] {
                before.insert(name, fs::read(root.join("widget").join(name)).unwrap());
            }
            fs::remove_file(dir.path().join("library/old.kio")).unwrap();
        }
        write_retype_fixture(
            dir.path(),
            "library/store.kio",
            "module store; pub newtype Token : . { pub constructor mk; pub projector get; }; \
             pub fn added() -> . { () }",
        );
        write_retype_fixture(dir.path(), "library/added.kio", "module added;");
        write_retype_fixture(
            dir.path(),
            "consumer/core/store.kio",
            "module core/store; pub newtype Token : . { pub constructor create; pub projector get; };",
        );
        let result = materialize_dependencies_filtered(&root, None, force);
        let observed = ["store.kio", "old.kio", "added.kio"]
            .map(|name| (name, fs::read(root.join("widget").join(name)).ok()));
        let expected =
            ["store.kio", "old.kio", "added.kio"].map(|name| (name, before.get(name).cloned()));
        assert_eq!(
            observed, expected,
            "member incompatibility must not publish; force={force}"
        );
        let error = result.expect_err("incompatible exposed constructor");
        assert!(matches!(error.error, Error::Dep(_)));
        assert!(error.error.diag().1.contains("requires constructor `mk`"));
        assert_eq!(root.join("widget").exists(), existing);
    }

    #[test]
    fn retype_member_interface_preserves_existing_tree() {
        assert_retype_member_interface_preserves_tree(false, true);
    }

    #[test]
    fn retype_member_interface_preserves_existing_tree_when_forced() {
        assert_retype_member_interface_preserves_tree(true, true);
    }

    #[test]
    fn retype_member_interface_preserves_absent_tree() {
        assert_retype_member_interface_preserves_tree(false, false);
    }

    #[test]
    fn retype_member_interface_preserves_absent_tree_when_forced() {
        assert_retype_member_interface_preserves_tree(true, false);
    }

    #[test]
    fn retype_member_interface_checks_up_to_date_and_forced_fetches() {
        let dir = retype_materialization_fixture();
        let root = dir.path().join("consumer");
        for (force, expected) in [
            (false, MaterializeOutcome::Fetched),
            (false, MaterializeOutcome::UpToDate),
            (true, MaterializeOutcome::Fetched),
        ] {
            let outcomes = materialize_dependencies_filtered(&root, None, force).unwrap();
            assert_eq!(outcomes["widget"], expected);
        }
        let before = fs::read(root.join("widget/store.kio")).unwrap();
        write_retype_fixture(
            dir.path(),
            "consumer/core/store.kio",
            "module core/store; pub newtype Token : . { pub constructor mk; projector get; };",
        );
        for force in [false, true] {
            let error = materialize_dependencies_filtered(&root, None, force).unwrap_err();
            assert!(error.error.diag().1.contains("target projector"));
            assert_eq!(fs::read(root.join("widget/store.kio")).unwrap(), before);
        }
    }

    fn assert_retype_visibility_preserves_tree(force: bool, existing: bool) {
        let dir = retype_materialization_fixture();
        let root = dir.path().join("consumer");
        let mut before = BTreeMap::new();
        if existing {
            write_retype_fixture(dir.path(), "library/old.kio", "module old;");
            materialize_dependencies_filtered(&root, None, false).expect("valid first fetch");
            for name in ["store.kio", "old.kio"] {
                before.insert(name, fs::read(root.join("widget").join(name)).unwrap());
            }
            fs::remove_file(dir.path().join("library/old.kio")).unwrap();
        }
        write_retype_fixture(
            dir.path(),
            "library/store.kio",
            "module store; pub newtype Token : . { pub constructor mk; pub projector get; }; \
                     pub fn added() -> . { () }",
        );
        write_retype_fixture(dir.path(), "library/added.kio", "module added;");
        write_retype_fixture(
            dir.path(),
            "consumer/core/store.kio",
            "module core/store; newtype Token : . { pub constructor mk; pub projector get; };",
        );
        let error =
            materialize_dependencies_filtered(&root, None, force).expect_err("invisible target");
        assert!(matches!(error.error, Error::Dep(_)));
        assert!(error.error.diag().1.contains("is not visible everywhere"));
        let observed = ["store.kio", "old.kio", "added.kio"]
            .map(|name| (name, fs::read(root.join("widget").join(name)).ok()));
        let expected =
            ["store.kio", "old.kio", "added.kio"].map(|name| (name, before.get(name).cloned()));
        assert_eq!(
            observed, expected,
            "visibility failure must neither replace, create, nor prune modules; force={force}",
        );
        if existing {
            assert!(root.join("widget").is_dir());
        } else {
            assert!(
                !root.join("widget").exists(),
                "no first materialization on error"
            );
        }
    }

    #[test]
    fn retype_visibility_preserves_existing_tree() {
        assert_retype_visibility_preserves_tree(false, true);
    }

    #[test]
    fn retype_visibility_preserves_existing_tree_when_forced() {
        assert_retype_visibility_preserves_tree(true, true);
    }

    #[test]
    fn retype_visibility_preserves_absent_tree() {
        assert_retype_visibility_preserves_tree(false, false);
    }

    #[test]
    fn retype_visibility_preserves_absent_tree_when_forced() {
        assert_retype_visibility_preserves_tree(true, false);
    }

    #[test]
    fn retype_visibility_checks_up_to_date_and_forced_fetches() {
        let dir = retype_materialization_fixture();
        let root = dir.path().join("consumer");
        for (force, expected) in [
            (false, MaterializeOutcome::Fetched),
            (false, MaterializeOutcome::UpToDate),
            (true, MaterializeOutcome::Fetched),
        ] {
            let outcomes = materialize_dependencies_filtered(&root, None, force).unwrap();
            assert_eq!(outcomes["widget"], expected);
        }
        write_retype_fixture(
            dir.path(),
            "consumer/core/store.kio",
            "module core/store; newtype Token : . { pub constructor mk; pub projector get; };",
        );
        for force in [false, true] {
            let error = materialize_dependencies_filtered(&root, None, force).unwrap_err();
            assert!(error.error.diag().1.contains("is not visible everywhere"));
        }
    }

    #[test]
    fn retype_visibility_checks_the_actual_import_head() {
        for (head, importable) in [
            ("", false),
            ("type Token = origin.Token;", false),
            ("pub(core) type Token = origin.Token;", false),
            ("pub type Token = origin.Token;", true),
        ] {
            let dir = retype_materialization_fixture();
            let root = dir.path().join("consumer");
            write_retype_fixture(
                dir.path(),
                "consumer/core/route.kio",
                &format!(
                    "module core/route; import core/store(Token); import core/store as origin; {head}"
                ),
            );
            write_retype_fixture(
                dir.path(),
                "consumer/widget.dep.kio",
                "dependency widget; source { path \"../library/widget.pkg.kio\"; } \
                 retype widget/store.Token to core/route.Token;",
            );
            let result = materialize_dependencies_filtered(&root, None, false);
            if importable {
                result.expect("public target alias preserves importability");
                assert!(root.join("widget/store.kio").is_file());
                continue;
            }
            let error = result.unwrap_err();
            assert!(matches!(error.error, Error::Dep(_)));
            assert!(
                error.error.diag().1.contains("is not importable"),
                "{}",
                error.error.diag().1
            );
            assert!(!root.join("widget").exists());
        }
    }

    #[test]
    fn retype_visibility_uses_later_selected_and_unselected_providers() {
        let dir = retype_materialization_fixture();
        let root = dir.path().join("consumer");
        write_retype_fixture(dir.path(), "target/target.pkg.kio", "package target;");
        write_retype_fixture(
            dir.path(),
            "target/store.kio",
            "module store; pub newtype Token : . { pub constructor mk; pub projector get; };",
        );
        write_retype_fixture(
            dir.path(),
            "consumer/zcore.dep.kio",
            "dependency zcore; source { path \"../target/target.pkg.kio\"; }",
        );
        write_retype_fixture(
            dir.path(),
            "consumer/widget.dep.kio",
            "dependency widget; source { path \"../library/widget.pkg.kio\"; } \
             retype widget/store.Token to zcore/store.Token;",
        );
        materialize_dependencies_filtered(&root, None, false).expect("later selected counterpart");
        let only = BTreeSet::from(["widget".to_owned()]);
        materialize_dependencies_filtered(&root, Some(&only), true)
            .expect("unselected on-disk counterpart");

        write_retype_fixture(
            dir.path(),
            "target/store.kio",
            "module store; newtype Token : . { pub constructor mk; pub projector get; };",
        );
        let error = materialize_dependencies_filtered(&root, None, false).unwrap_err();
        assert!(error.error.diag().1.contains("is not visible everywhere"));
        // The ordinary target dependency is published immediately, even
        // though the retyped dependency is rejected later in this fetch.
        assert!(
            fs::read_to_string(root.join("zcore/store.kio"))
                .unwrap()
                .contains("\nnewtype Token")
        );
    }

    #[test]
    fn retype_non_visibility_errors_keep_their_post_publication_timing() {
        let dir = retype_materialization_fixture();
        let root = dir.path().join("consumer");
        write_retype_fixture(
            dir.path(),
            "consumer/core/store.kio",
            "module core/store; pub newtype Token : . -> . { pub constructor mk; pub projector get; };",
        );
        let error = materialize_dependencies_filtered(&root, None, false).unwrap_err();
        assert!(error.error.diag().1.contains("not structurally congruent"));
        assert!(root.join("widget/store.kio").is_file());
    }

    #[test]
    fn retype_visibility_drops_stale_selected_provider_modules() {
        let dir = retype_materialization_fixture();
        let root = dir.path().join("consumer");
        write_retype_fixture(dir.path(), "target/target.pkg.kio", "package target;");
        write_retype_fixture(
            dir.path(),
            "consumer/zcore.dep.kio",
            "dependency zcore; source { path \"../target/target.pkg.kio\"; }",
        );
        write_retype_fixture(
            dir.path(),
            "consumer/zcore/store.kio",
            "module zcore/store; newtype Token : . { pub constructor mk; pub projector get; };",
        );
        write_retype_fixture(
            dir.path(),
            "consumer/widget.dep.kio",
            "dependency widget; source { path \"../library/widget.pkg.kio\"; } \
             retype widget/store.Token to zcore/store.Token;",
        );
        let error = materialize_dependencies_filtered(&root, None, false).unwrap_err();
        assert!(matches!(error.error, Error::Dep(_)));
        assert!(!error.error.diag().1.contains("visible"));
        assert!(!error.error.diag().1.contains("importable"));
        // An absent counterpart keeps its existing post-publication error.
        // The removed private declaration cannot invent a visibility failure.
        assert!(root.join("widget/store.kio").is_file());
        assert!(!root.join("zcore/store.kio").exists());
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_congruence_resolves_generated_label_nominal_identities() {
        let source = parsed_module(
            "module dep/store; \
             rec { \
               labels { odd: Even }; \
               newtype Even : . | Odd { constructor mk_even; projector un_even; }; \
             }",
        );
        let target = parsed_module(
            "module core/store; \
             rec { \
               labels { odd: Even }; \
               newtype Even : . | Odd { constructor mk_even; projector un_even; }; \
             }",
        );
        let (_, semantics) = canonical_retype_fixture(source, target, &["Even"]);
        let pair = &semantics.pairs[0][0];
        assert!(
            !canonical_retype_payloads_congruent(&pair.from, &pair.to, &semantics.package),
            "unselected generated-label nominals have distinct declaring-module identities"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_congruence_expands_named_label_aliases() {
        let source = parsed_module(
            "module dep/store; \
             pub labels Packet = { first: ., second: . }; \
             pub newtype Holder : Packet { pub constructor mk; pub projector get; };",
        );
        let target = parsed_module(
            "module core/store; \
             import dep/store as original; \
             pub newtype Holder : original.First & original.Second { \
               pub constructor mk; pub projector get; \
             };",
        );
        let (check, semantics) = canonical_retype_fixture(source, target, &["Holder"]);
        validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect("a named label alias expands to its exact generated nominal components");
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_congruence_preserves_reused_label_identity() {
        let source = parsed_module(
            "module dep/store; \
             pub labels Reply = { item: . } | { item: _ }; \
             pub newtype Holder : Reply { pub constructor mk; pub projector get; };",
        );
        let same_identity = parsed_module(
            "module core/store; \
             import dep/store as original; \
             pub newtype Holder : original.Item | original.Item { \
               pub constructor mk; pub projector get; \
             };",
        );
        let (check, semantics) =
            canonical_retype_fixture(source.clone(), same_identity, &["Holder"]);
        validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect("a reused label denotes the first generated nominal in both sum arms");

        let distinct_identity = parsed_module(
            "module core/store; \
             pub labels Reply = { item: . } | { item: _ }; \
             pub newtype Holder : Reply { pub constructor mk; pub projector get; };",
        );
        let (check, semantics) = canonical_retype_fixture(source, distinct_identity, &["Holder"]);
        let error = validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect_err("a different provider's reused label is still a different nominal");
        assert!(
            error
                .error
                .diag()
                .1
                .contains("payloads are not structurally congruent")
        );
    }

    #[test]
    fn retype_congruence_expands_applied_aliases_without_capturing_binders() {
        let source = parsed_module(
            "module dep/store; \
             pub type Payload[A] = [B] A -> B -> A; \
             pub newtype Holder[B] : Payload(B) { pub constructor mk; pub projector get; };",
        );
        let alpha_renamed = parsed_module(
            "module core/store; \
             pub newtype Holder[C] : [D] C -> D -> C { \
               pub constructor mk; pub projector get; \
             };",
        );
        let (check, semantics) =
            canonical_retype_fixture(source.clone(), alpha_renamed, &["Holder"]);
        validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect("the alias's inner B binder cannot capture the applied outer B argument");

        let captured_shape = parsed_module(
            "module core/store; \
             pub newtype Holder[C] : [D] D -> D -> C { \
               pub constructor mk; pub projector get; \
             };",
        );
        let (check, semantics) = canonical_retype_fixture(source, captured_shape, &["Holder"]);
        let error = validate_retype_congruence(&check, &semantics.pairs[0], &semantics.package)
            .expect_err("an outer binder occurrence is not interchangeable with an inner one");
        assert!(
            error
                .error
                .diag()
                .1
                .contains("payloads are not structurally congruent")
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retype_rejects_an_indivisible_labels_owner_crossing_residual_components() {
        let mut module = parsed_module(
            "module dep/store; \
             rec { \
               labels { a: B & X, c: D & X }; \
               labels { b: A, d: C }; \
               newtype X : A & C { constructor mk_x; projector un_x; }; \
             }",
        );
        let target_module = parsed_module("module core/store;");
        let target = RetypeTarget {
            from: module.path.clone(),
            newtype_names: ["X".to_owned()].into_iter().collect(),
            to: target_module.path,
            span: Span::new(1, 2),
        };

        let error = apply_retype(&mut module, &[target])
            .expect_err("one labels declaration cannot be split across residual SCCs");
        assert!(matches!(error, Error::Dep(_)));
        assert!(error.diag().1.contains(
            "retained indivisible `labels` declaration spans multiple recursive components"
        ));
    }

    #[test]
    fn retype_rejects_a_residual_alias_only_cycle() {
        let mut module = parsed_module(
            "module dep/store; \
             rec { \
               type A = B; \
               type B = A | X; \
               newtype X : A { constructor mk_x; projector un_x; }; \
             }",
        );
        let target_module = parsed_module("module core/store;");
        let target = RetypeTarget {
            from: module.path.clone(),
            newtype_names: ["X".to_owned()].into_iter().collect(),
            to: target_module.path,
            span: Span::new(1, 2),
        };

        let error = apply_retype(&mut module, &[target])
            .expect_err("removing the nominal anchor must not emit an alias-only cycle");
        assert!(matches!(error, Error::Dep(_)));
        assert!(
            error
                .diag()
                .1
                .contains("retained type aliases `A, B` would form a recursive cycle")
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn member_retype_preserves_a_residual_labels_declaration() {
        let mut module = parsed_module(
            "module dep/store; \
             rec { \
               pub labels { a: X }; \
               pub newtype X : A { pub constructor mk_x; pub projector un_x; }; \
             }",
        );
        let target_module = parsed_module("module core/store;");
        let target = RetypeTarget {
            from: module.path.clone(),
            newtype_names: ["X".to_owned()].into_iter().collect(),
            to: target_module.path,
            span: Span::new(1, 2),
        };

        apply_retype(&mut module, &[target]).expect("labels owner remains one legal component");

        assert!(
            module
                .items
                .iter()
                .all(|item| !matches!(item, Item::TypeRecGroup(_)))
        );
        assert!(
            module.items.iter().any(|item| {
                matches!(item, Item::Labels(labels, _) if labels.rec_span.is_none())
            })
        );
        assert!(
            module
                .items
                .iter()
                .any(|item| matches!(item, Item::TypeAlias(alias) if alias.name == "X"))
        );
    }

    #[test]
    fn file_header_forcing_preserves_operator_source_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source_path = dir.path().join("consumer.kio");
        let prefix = "module consumer;\nimport syntax(op _ * _);\n\
            fn combine(a: ., b: .) -> . { a }\n";
        let operator = "op _ + _ { impl combine; };\n";
        let body = "fn subject() -> . { (() * ()) + () }\n";
        let valid = format!("{prefix}{operator}{body}");
        let invalid = format!("{prefix}{body}{operator}");
        for source in [&valid, &invalid, &valid, &invalid] {
            let header = crate::pass::parser::parse_module_file_lazy(source)
                .expect("headers do not force expression bodies");
            let result = header.force_all();
            if source == &invalid {
                let error = result.expect_err("cached header cannot expose later local operators");
                assert_eq!(error.diag().0.start as usize, source.find('+').unwrap());
            } else {
                let parsed = result.expect("preceding and imported grammars remain available");
                let formatted = crate::pretty::pretty_module(&parsed.module);
                parse_module_file_with_file_context(&formatted, &source_path)
                    .expect("formatted source reparses with the same source-local scope");
            }
        }
    }

    #[test]
    fn file_parser_context_derives_root_from_the_complete_module_suffix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source_root = dir.path().join("src");
        let module = lazy_module("module a/b/c;");
        let source_path = source_root.join("a/b/c.kio");
        let context = ModuleFileParserContext::from_module(&source_path, &module)
            .expect("matching file/module suffix");
        assert_eq!(context.source_path, source_path);
        assert_eq!(context.source_root, source_root);

        let single = lazy_module("module main;");
        let context =
            ModuleFileParserContext::from_module(&dir.path().join("src/main.kio"), &single)
                .expect("single-segment suffix");
        assert_eq!(context.source_root, dir.path().join("src"));

        let lexical = lazy_module("module a/b/c;");
        let lexical_root = dir.path().join("other/../src");
        let context =
            ModuleFileParserContext::from_module(&lexical_root.join("a/b/c.kio"), &lexical)
                .expect("lexical dot-dot prefix");
        assert_eq!(context.source_root, lexical_root);
    }

    #[test]
    fn file_parser_context_rejects_partial_or_wrong_suffixes() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("root.pkg.kio"), "package root;")
            .expect("package marker must not become a fallback root");
        let module = lazy_module("module a/b/c;");
        for path in [
            dir.path().join("src/x/a/b/copy.kio"),
            dir.path().join("src/x/b/c.kio"),
            dir.path().join("src/a/x/c.kio"),
        ] {
            let error = ModuleFileParserContext::from_module(&path, &module)
                .expect_err("the full declared suffix is mandatory");
            assert!(matches!(error, Error::Parse(_)));
            assert!(
                error
                    .diag()
                    .1
                    .contains("does not match the selected file path")
            );
        }

        let error = ModuleFileParserContext::from_module(Path::new("a/b/c.kio"), &module)
            .expect_err("callers must resolve relative selections against their cwd");
        assert!(matches!(error, Error::Parse(_)));
        assert!(error.diag().1.contains("is not absolute"));
    }

    #[cfg(windows)]
    #[test]
    fn file_parser_context_preserves_drive_and_unc_roots() {
        let module = lazy_module("module a/b/c;");
        for (source, expected_root) in [
            (r"C:\workspace\src\a\b\c.kio", r"C:\workspace\src"),
            (r"\\server\share\src\a\b\c.kio", r"\\server\share\src"),
        ] {
            let context = ModuleFileParserContext::from_module(Path::new(source), &module)
                .expect("Windows absolute source path");
            assert_eq!(context.source_root, Path::new(expected_root));
        }
    }

    #[test]
    fn selected_file_ignores_a_nearer_package_marker_for_lexical_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let consumer = dir.path().join("a/b/c.kio");
        let provider = dir.path().join("a/d/e.kio");
        std::fs::create_dir_all(consumer.parent().expect("consumer parent"))
            .expect("consumer dirs");
        std::fs::create_dir_all(provider.parent().expect("provider parent"))
            .expect("provider dirs");
        std::fs::write(dir.path().join("root.pkg.kio"), "package root;")
            .expect("matching package marker");
        std::fs::write(dir.path().join("a/near.pkg.kio"), "package near;")
            .expect("misleading package marker");
        std::fs::write(&provider, "module a/d/e; pub op _ + _ { impl combine; };")
            .expect("provider");
        let source = "module a/b/c; import a/d/e(op _ + _); fn combine(x: A, y: A) -> A { x } fn f(x: A, y: A) -> A { x + y }";
        let parsed = parse_module_file_with_file_context(source, &consumer)
            .expect("file identity follows the complete module suffix");
        assert_eq!(parsed.module.path.segments.len(), 3);
        let context =
            ModuleFileParserContext::from_module(&consumer, &parsed.module).expect("file context");
        assert_eq!(context.source_root, dir.path());
    }

    #[cfg(unix)]
    #[test]
    fn selected_symlink_identity_remains_lexical_for_syntax() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let real_root = dir.path().join("real");
        let lexical_root = dir.path().join("link");
        std::fs::create_dir_all(real_root.join("a/b")).expect("real source dirs");
        symlink(&real_root, &lexical_root).expect("root symlink");

        let target = dir.path().join("unrelated.kio");
        std::fs::write(&target, "module unrelated;").expect("different disk text");
        symlink(&target, real_root.join("a/b/c.kio")).expect("selected file symlink");
        let consumer = lexical_root.join("a/b/c.kio");
        let source = "module a/b/c; import a/d/e(op _ + _); fn f(x: A, y: A) -> A { x + y }";
        let parsed = parse_module_file_with_file_context(source, &consumer)
            .expect("selected text and lexical file identity determine syntax");
        let context = ModuleFileParserContext::from_module(&consumer, &parsed.module).unwrap();
        assert_eq!(context.source_path, consumer);
        assert_eq!(context.source_root, lexical_root);
        assert_eq!(parsed.module, crate::pass::parser::parse(source).unwrap());
    }

    #[test]
    fn walk_rejects_multiple_root_package_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("app.pkg.kio"), "package app;\n").expect("write app");
        std::fs::write(dir.path().join("other.pkg.kio"), "package other;\n").expect("write other");
        std::fs::write(dir.path().join("app.kio"), "module app;\n").expect("write module");

        let (error, _) = walk(dir.path()).expect_err("multiple package files should fail");
        match &error {
            WalkError::MultiplePackageFiles { paths, .. } => {
                assert_eq!(paths.len(), 2);
            }
            other => panic!("expected MultiplePackageFiles, got {other:?}"),
        }
        let located = error.into_located();
        match located.error {
            Error::Import(diagnostic) => {
                assert!(
                    diagnostic.message.contains("multiple package files"),
                    "got: {}",
                    diagnostic.message
                );
            }
            other => panic!("expected use error, got {other:?}"),
        }
    }

    #[test]
    fn walk_keeps_consumer_grammar_for_deferred_bodies() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("app.pkg.kio"), "package app;\n").expect("write package");
        std::fs::write(
            dir.path().join("syntax.kio"),
            "module syntax;\npub op ? _ : _ { impl choose; };\n",
        )
        .expect("write provider");
        std::fs::write(
            dir.path().join("main.kio"),
            "module main;\nimport syntax(op ? _ : _);\nfn choose(a: A, b: A) -> A { ? a : b }\n",
        )
        .expect("write consumer");

        let parsed = walk(dir.path()).expect("walk package");
        let package = parsed.packages.get(&parsed.root).expect("root package");
        let module = package
            .lazy_modules
            .get(&package.root_dir.join("main.kio"))
            .expect("consumer lazy module")
            .force_all()
            .expect("force consumer-declared body");
        let Item::FnDef(definition) = &module.items[0] else {
            panic!("expected consumer function");
        };
        let crate::ast::Expr::OpChain {
            kind: crate::ast::OpChainKind::Normal { pattern, slots },
            ..
        } = &definition.body
        else {
            panic!("expected imported operator chain");
        };
        assert_eq!(pattern.len(), 4);
        assert_eq!(slots.len(), 2);
    }

    #[test]
    fn walk_keeps_variadic_declaration_complete_in_lazy_summary() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("app.pkg.kio"), "package app;\n").expect("write package");
        let source = "module syntax;\n\
            pub varop [% %] {\n\
              foldl push empty;\n\
            };\n";
        std::fs::write(dir.path().join("syntax.kio"), source).expect("write provider");

        let parsed = walk(dir.path()).expect("walk package");
        let package = parsed.packages.get(&parsed.root).expect("root package");
        let (_, summary) = package
            .modules
            .iter()
            .find(|(_, module)| module.path.segments == vec!["syntax"])
            .expect("provider summary");
        let eager = crate::pass::parser::parse(source).expect("eager provider parse");

        assert_eq!(summary, &eager);
        let forced = package
            .lazy_modules
            .get(&package.root_dir.join("syntax.kio"))
            .expect("variadic provider lazy module")
            .force_all()
            .expect("force deferred function bodies");
        assert_eq!(forced, eager);
    }

    #[test]
    fn discover_finds_every_package_in_subtree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        // A root package, plus two sibling packages in subdirectories,
        // plus a docs-only subdir with no package file.
        std::fs::write(root.join("app.pkg.kio"), "package app;\n").expect("write app");
        std::fs::write(root.join("main.kio"), "module main;\n").expect("write main");

        std::fs::create_dir_all(root.join("libs/alpha")).expect("mk alpha");
        std::fs::write(root.join("libs/alpha/alpha.pkg.kio"), "package alpha;\n")
            .expect("write alpha");

        std::fs::create_dir_all(root.join("libs/beta")).expect("mk beta");
        std::fs::write(root.join("libs/beta/beta.pkg.kio"), "package beta;\n").expect("write beta");

        std::fs::create_dir_all(root.join("docs.md")).expect("mk docs");
        std::fs::write(root.join("docs.md/index.md"), "# hi\n").expect("write doc");

        let roots = discover_package_roots(root).expect("discover");
        let names: Vec<&str> = roots.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["app", "alpha", "beta"]);
        // Each discovered root's directory holds exactly its own file.
        assert_eq!(roots[0].dir, canonicalize(root).expect("canonicalize root"));
    }

    #[test]
    fn discover_rejects_two_package_files_in_one_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.pkg.kio"), "package a;\n").expect("write a");
        std::fs::write(dir.path().join("b.pkg.kio"), "package b;\n").expect("write b");
        let err = discover_package_roots(dir.path())
            .expect_err("two package files in one dir is ambiguous");
        match err {
            WalkError::MultiplePackageFiles { paths, .. } => assert_eq!(paths.len(), 2),
            other => panic!("expected MultiplePackageFiles, got {other:?}"),
        }
    }

    #[test]
    fn discover_empty_tree_is_no_packages() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("loose.kio"), "module loose;\n").expect("write loose");
        let roots = discover_package_roots(dir.path()).expect("discover");
        assert!(roots.is_empty());
    }

    #[test]
    fn discover_skips_build_output_and_hidden_dirs() {
        // A package whose emitted output (`out/`) and Cargo build tree
        // (`target/`) contain re-emitted `*.pkg.kio` files must not have
        // those copies discovered as separate packages — otherwise a
        // Kio'-roundtrip build recurses into ever-deeper `out/` trees.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(root.join("app.pkg.kio"), "package app;\n").expect("write app");

        // Emitted Kio' output under out/kio-prime/ — its own pkg file.
        std::fs::create_dir_all(root.join("out/kio-prime")).expect("mk out");
        std::fs::write(root.join("out/kio-prime/app.pkg.kio"), "package app;\n")
            .expect("write emitted");
        // A Cargo target tree, and a hidden cache tree.
        std::fs::create_dir_all(root.join("out/rust/target")).expect("mk target");
        std::fs::write(root.join("out/rust/target/app.pkg.kio"), "package app;\n")
            .expect("write target pkg");
        std::fs::create_dir_all(root.join(".kio-cache/x")).expect("mk hidden");
        std::fs::write(root.join(".kio-cache/x/app.pkg.kio"), "package app;\n")
            .expect("write hidden pkg");

        let roots = discover_package_roots(root).expect("discover");
        assert_eq!(
            roots.len(),
            1,
            "only the root package, not output copies: {roots:?}"
        );
        assert_eq!(roots[0].dir, canonicalize(root).expect("canon"));
    }

    #[test]
    fn package_selector_arg_keys_on_shape_not_cwd() {
        // A bare identifier is a target id, even if a cwd-sibling dir of
        // that name exists — the rule keys on the arg's shape, not the
        // filesystem, so a directory named `rust` never hijacks the
        // bare-identifier-is-a-target-id rule.
        assert!(!is_package_selector_arg("rust"));
        assert!(!is_package_selector_arg("js"));
        assert!(!is_package_selector_arg("kio-prime"));
        // A path-shaped arg or a `*.pkg.kio` suffix is a selector.
        assert!(is_package_selector_arg("./pkg"));
        assert!(is_package_selector_arg("libs/alpha"));
        assert!(is_package_selector_arg("app.pkg.kio"));
        assert!(is_package_selector_arg("dir/app.pkg.kio"));

        // Create a cwd-sibling dir literally named `rust`; the rule must
        // still classify the bare `rust` arg as a target id.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("rust")).expect("mk rust dir");
        let prev = std::env::current_dir().ok();
        if std::env::set_current_dir(dir.path()).is_ok() {
            assert!(
                !is_package_selector_arg("rust"),
                "a cwd-sibling dir named `rust` must not make `rust` a package selector"
            );
            if let Some(p) = prev {
                let _ = std::env::set_current_dir(p);
            }
        }
    }

    #[test]
    fn discover_skips_package_under_custom_cache_path() {
        // A package whose emitted output lives under a *custom*
        // (non-`out`, non-hidden) cache directory must be found exactly
        // once — the generated-dir marker prunes the copy under the
        // custom path, which the `out`/`target`/`.*` name heuristic
        // alone would miss.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(root.join("app.pkg.kio"), "package app;\n").expect("write app");

        // A custom cache dir name that the name heuristic does not skip.
        let cache_dir = root.join("build-cache");
        std::fs::create_dir_all(cache_dir.join("tree")).expect("mk cache");
        // The build would drop the marker into the cache root.
        mark_generated_dir(&cache_dir);
        // A re-emitted package copy lives under the cache tree.
        std::fs::write(cache_dir.join("tree/app.pkg.kio"), "package app;\n")
            .expect("write cache copy");

        let roots = discover_package_roots(root).expect("discover");
        assert_eq!(
            roots.len(),
            1,
            "the custom-cache copy must be pruned by the marker: {roots:?}"
        );
        assert_eq!(roots[0].dir, canonicalize(root).expect("canon"));
    }
}
