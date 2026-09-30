//! `kio doc` subcommand: validates Kiodoc directives and embedded
//! Kio snippets in one or more Markdown files.
//!
//! See [`specs/kiodoc.md`](../../../specs/kiodoc.md) for the
//! directive contract and [`specs/cli.md`](../../../specs/cli.md)
//! § `kio doc` for the CLI surface.
//!
//! ## Pipeline
//!
//! 1. [`parse`] scans a markdown file's bytes and yields a stream of
//!    [`parse::Fence`] records (visible ` ```LANG {attrs} ``` ` and
//!    the hidden `<!--LANG {attrs}\n<body>\n-->` form).
//! 2. [`attrs`] parses each fence's `{...}` attribute list into a
//!    [`attrs::FenceAttrs`] value (harness reference, bare flags,
//!    key-value attributes).
//! 3. [`document`] walks the fence stream in source order, building
//!    a [`document::Document`] — harness declarations indexed by
//!    name (with forward-reference detection), snippets, and
//!    snippet-to-output pairings.
//! 4. [`validate`] iterates the document's snippets, substitutes
//!    each into its harness (or takes it as-is for the standalone
//!    `{}` form), and invokes `kio check` in-process against a
//!    synthesized scratch package. Snippet `kio check` exits are
//!    compared against the snippet's declared `check_exit_code`
//!    (default `0`).
//!
//! ## Parallelism
//!
//! The driver fans the per-snippet validation step out across rayon
//! workers — every snippet across every markdown file is a node in
//! one flat `par_iter`. Each worker runs an independent
//! `check::compile_workspace_at` against its per-snippet scratch
//! directory; no shared state is mutated. After the parallel region
//! the driver sorts the collected errors by `(file_path, span.start)`
//! and emits them in that order, so user-facing stderr is byte
//! identical across thread counts.
//!
//! Document-build errors (parse, attribute, pairing) and snippet
//! validation errors share the same sort domain — each carries the
//! source file path and a byte-offset span, so a multi-error run
//! prints in source order.
//!
//! ## Errors
//!
//! Every Kiodoc-level violation surfaces as a [`DocError`] carrying
//! the markdown file path and the source location of the offending
//! fence. Snippet `kio check` failures are wrapped as well, with the
//! synthesized program included for the author to inspect.

pub mod attrs;
pub mod cache;
pub mod directives;
pub mod doc_comments;
pub mod document;
pub mod fmt;
pub mod parse;
pub mod refs;
pub mod render;
mod scratch;
pub mod validate;

// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(â¦)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
use crate::path_display::DisplayPath;
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cmd::package_fanout::CapturedOutput;
use crate::exit_code::ExitCode;
use cache::DocCache;
use directives as kio_directives;
use doc_comments::{DocCommentError, DocCommentSnippet};
use document::{DocError, Document, Snippet};
use validate::ValidationError;

const HELP_TEMPLATE: &str = "\
Usage: kio doc <subcommand>

Validate and render the current package's Kiodoc content.

Subcommands:
  check                Validate Kiodoc directives, embedded Kio
                       snippets, intra-doc links, and /// doc-comments.
  fmt [--check] [<path>...]
                       Format formattable Markdown Kiodoc snippets
                       using their Kiodoc harness context.
  build [--md] [--html]
                       Validate, then render a per-module documentation
                       site. --html (default) renders HTML; --md renders
                       Markdown; both may be passed.

All subcommands read the `docs` field of the `build { ... }` block in
the package's <name>.pkg.kio and operate on the package rooted
at the current directory.

Options:
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md):
  0   validation/formatting passed (and, for build, rendering succeeded)
  60  fmt --check found non-canonical Markdown snippets
  70  Kiodoc contract violation, snippet check exit mismatch,
      or unmappable snippet formatting result
  40  build error (no `docs` field, rendering failure)
  2   CLI usage error

See {base}/specs/cli.md#kio-doc-subcommand and {base}/specs/kiodoc.md.";

const HELP_CHECK_TEMPLATE: &str = "\
Usage: kio doc check [<path>...]

Validate the current package's Kiodoc content: every `kio` fence in
the markdown tree under the build block's `docs.md`, every `///`
doc-comment in the package's .kio source files, every intra-doc link,
and every @signature / @source / @type directive.

Reads the `docs` field of the `build { ... }` block in the package's
<name>.pkg.kio.

With no positional argument, every markdown file under `docs.md` and
every `.kio` module's doc-comments are validated. With one or more
<path> arguments, only the matching files are validated. Each <path>
must point at a markdown file under the docs tree or a `.kio` source
file under the package; unknown paths are CLI usage errors.

Options:
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on validation pass; 70
on Kiodoc contract violation or snippet check exit mismatch; 40 when
the package file is missing or its build block declares no `docs`
field; 2 on CLI usage error (unknown path).

See {base}/specs/cli.md#kio-doc-check-path for full command behavior.";

const HELP_FMT_TEMPLATE: &str = "\
Usage: kio doc fmt [--check] [<path>...]

Format formattable Kiodoc `kio` snippet fences in the markdown tree
under the build block's `docs.md`.

With no positional argument, every markdown file under `docs.md` is
visited. With one or more <path> arguments, each path must point at a
markdown file or directory under the docs tree; directories are walked
recursively.

Modes:
  --check       Don't write. List markdown files whose formattable
                snippets differ from canonical output, one per line,
                and exit 60 if any do (0 if all are already canonical).

Options:
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 when every formattable
snippet is canonical (rewrite mode also exits 0 after writing); 60 in
--check mode when at least one file is not canonical; 70 on a Kiodoc
contract violation or snippet whose virtual formatted source cannot be
mapped cleanly back to the original fence body; 40 when the package file
is missing or its build block declares no `docs` field; 2 on CLI usage
error.

See {base}/specs/cli.md#kio-doc-fmt---check-path for full command
behavior.";

const HELP_BUILD_TEMPLATE: &str = "\
Usage: kio doc build [--md] [--html]

Validate the current package's Kiodoc content (as `kio doc check`),
then render a per-module documentation site.

  --html   render an HTML site to the build block's `docs.html`
           directory (default `out/docs/`). This is the default
           when no format flag is given.
  --md     render a Markdown site to the build block's `docs.md_out`
           directory (default `out/docs-md/`).

Both flags may be passed; both formats are then rendered. Reads the
`docs` field of the `build { ... }` block in the package's
<name>.pkg.kio. Validation and rendering always cover the whole
package — `kio doc build` deliberately takes no <path> filter, because
the rendered site's intra-doc links require a whole-package walk.

Options:
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on validation pass and
successful rendering; 70 on Kiodoc contract violation or snippet check
exit mismatch; 40 when the package file is missing, its build block
declares no `docs` field, or rendering fails; 2 on CLI usage error.

See {base}/specs/cli.md#kio-doc-build---md---html for full command behavior.";

/// Top-level entry point for `kio doc`. Dispatches the `check`,
/// `fmt`, and `build` subcommands; returns the process exit code per
/// `specs/exit-codes.md`.
pub fn run(args: &[String]) -> ExitCode {
    if args.first().map(|a| a == "-h" || a == "--help") == Some(true) {
        println!(
            "{}",
            HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }
    match args.first().map(String::as_str) {
        Some("check") => run_check(&args[1..]),
        Some("fmt") => run_fmt(&args[1..]),
        Some("build") => run_build(&args[1..]),
        Some(other) => {
            eprintln!("error: kio doc: unknown subcommand: {other}");
            eprintln!();
            eprintln!(
                "{}",
                HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
            );
            ExitCode::Usage
        }
        None => {
            eprintln!("error: kio doc: missing subcommand (expected `check`, `fmt`, or `build`)");
            eprintln!();
            eprintln!(
                "{}",
                HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
            );
            ExitCode::Usage
        }
    }
}

/// The package's resolved `docs` configuration — the markdown source
/// directory plus the rendered-output directories.
struct DocsConfig {
    /// Absolute path to the package root (the cwd).
    package_root: PathBuf,
    /// Absolute path to the markdown source tree (`docs.md`).
    md_dir: PathBuf,
    /// Absolute paths to additional root module support-file directories.
    support_dirs: Vec<PathBuf>,
    /// Absolute path the HTML site renders to (`docs.html`).
    html_out: PathBuf,
    /// Absolute path the Markdown site renders to (`docs.md_out`).
    md_out: PathBuf,
}

/// Resolve the package's `docs` configuration from the `build { ... }`
/// block in its package file. Returns `Err(exit_code)` with a
/// diagnostic already printed when the package file is missing, its
/// build block declares no `docs` field, or the `docs.md` directory
/// doesn't exist.
fn resolve_docs_config() -> Result<DocsConfig, ExitCode> {
    let package_root = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return Err(ExitCode::Internal);
        }
    };
    match resolve_docs_config_at(&package_root, true) {
        Ok(Some(config)) => Ok(config),
        Ok(None) => {
            // `require_docs = true` turns the no-docs-field case into a
            // diagnostic + `Err`, so `Ok(None)` is unreachable here.
            unreachable!(
                "resolve_docs_config_at(require_docs = true) returns Err on no docs field"
            );
        }
        Err(code) => Err(code),
    }
}

/// Resolve the `docs` configuration for the package rooted at
/// `package_root`. With `require_docs = true`, a package whose build
/// block declares no `docs` field is an error (the single-package
/// contract). With `require_docs = false` it returns `Ok(None)` so the
/// multi-package fan-out can skip a package that simply has no docs.
fn resolve_docs_config_at(
    package_root: &Path,
    require_docs: bool,
) -> Result<Option<DocsConfig>, ExitCode> {
    let package_root = package_root.to_path_buf();

    // Find the package's `<name>.pkg.kio`.
    let package_path = match find_package_file(&package_root) {
        Some(p) => p,
        None => {
            eprintln!(
                "error: kio doc: no `<name>.pkg.kio` at {} — `kio doc` requires a \
                 package file with a `build {{ … }}` block declaring a `docs` field; \
                 run `kio init` to scaffold a package \
                 (see specs/package.md § Build target files)",
                DisplayPath(&package_root)
            );
            return Err(ExitCode::Build);
        }
    };
    let source = match fs::read_to_string(&package_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "error: kio doc: cannot read {}: {e}",
                DisplayPath(&package_path)
            );
            return Err(ExitCode::Internal);
        }
    };
    let stem = package_path
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(crate::file_kind::package_stem);
    let package = match crate::pass::parser::parse_package_file(&source, stem) {
        Ok(d) => d,
        Err(err) => {
            crate::cmd::check::eprint_error(&package_path, &source, &err);
            return Err(ExitCode::Build);
        }
    };
    let docs = match package.build.and_then(|b| b.docs) {
        Some(docs) => docs,
        None => {
            if require_docs {
                eprintln!(
                    "error: kio doc: package's package file ({}) declares no `docs` field in its \
                     `build {{ … }}` block — add `build {{ cache (); docs {{ md \"<path>\"; }}; }}` \
                     (see specs/package.md § Build target files)",
                    DisplayPath(&package_path)
                );
                return Err(ExitCode::Build);
            }
            // Multi-package fan-out: a package with no docs field is
            // simply skipped, not an error.
            return Ok(None);
        }
    };

    // Resolve the three paths relative to the package root.
    let resolve = |p: &str| -> PathBuf {
        let pb = PathBuf::from(p);
        if pb.is_absolute() {
            pb
        } else {
            package_root.join(pb)
        }
    };
    let md_dir = resolve(&docs.md);
    let support_dirs: Vec<PathBuf> = docs.support.iter().map(|p| resolve(p)).collect();
    let html_out = resolve(docs.html.as_deref().unwrap_or("out/docs"));
    let md_out = resolve(docs.md_out.as_deref().unwrap_or("out/docs-md"));

    if !md_dir.is_dir() {
        eprintln!(
            "error: kio doc: the `docs.md` directory {} does not exist",
            DisplayPath(&md_dir)
        );
        return Err(ExitCode::Build);
    }
    for support_dir in &support_dirs {
        if !support_dir.is_dir() {
            eprintln!(
                "error: kio doc: the `docs.support` directory {} does not exist",
                DisplayPath(support_dir)
            );
            return Err(ExitCode::Build);
        }
    }

    Ok(Some(DocsConfig {
        package_root,
        md_dir,
        support_dirs,
        html_out,
        md_out,
    }))
}

/// Apply selectors to the markdown and .kio file lists. Each
/// selector is a `Path` form (per `run_check`'s promote step); the
/// matching is purely lexical on the canonicalised filename — a
/// path that matches a markdown file restricts the markdown side,
/// one that matches a `.kio` file restricts the doc-comment side,
/// and an unrecognised path is a CLI usage error.
fn apply_selectors(
    md_files: &[PathBuf],
    kio_files: &[PathBuf],
    selectors: &[crate::cmd::module_selector::Selector],
    _package_root: &Path,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), ExitCode> {
    use crate::cmd::module_selector::Selector;

    let mut selected_md: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    let mut selected_kio: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();

    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());

    for sel in selectors {
        let Selector::Path(p) = sel else {
            // Unreachable: `run_check` promotes every selector to a
            // `Path` variant before calling here. Panic with a
            // pointed message so a future caller that forgets the
            // promote sees it fast.
            unreachable!("kio doc check selectors must be Path variants; got {sel:?}");
        };
        let canonical = canon(p);
        if let Some(md) = md_files.iter().find(|f| canon(f) == canonical) {
            selected_md.insert(md.clone());
        } else if let Some(kio) = kio_files.iter().find(|f| canon(f) == canonical) {
            selected_kio.insert(kio.clone());
        } else {
            eprintln!(
                "error: kio doc: no markdown or .kio source file at `{}` in this package",
                p.display()
            );
            eprintln_doc_available(md_files, kio_files);
            return Err(ExitCode::Usage);
        }
    }

    Ok((
        selected_md.into_iter().collect(),
        selected_kio.into_iter().collect(),
    ))
}

fn eprintln_doc_available(md_files: &[PathBuf], kio_files: &[PathBuf]) {
    if !md_files.is_empty() {
        eprintln!("available markdown files:");
        for p in md_files {
            eprintln!("  {}", DisplayPath(p));
        }
    }
    if !kio_files.is_empty() {
        eprintln!("available .kio source files:");
        for p in kio_files {
            eprintln!("  {}", DisplayPath(p));
        }
    }
}

/// Find the package's `<name>.pkg.kio` at `dir`. Returns `None`
/// for "missing", "multiple", or I/O failure.
fn find_package_file(dir: &Path) -> Option<PathBuf> {
    let mut hits = Vec::new();
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if crate::file_kind::is_package_file(name) {
            hits.push(path);
        }
    }
    if hits.len() == 1 {
        hits.into_iter().next()
    } else {
        None
    }
}

/// `kio doc build` — validate, then render the documentation site.
fn run_build(args: &[String]) -> ExitCode {
    let mut want_html = false;
    let mut want_md = false;
    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "{}",
                    HELP_BUILD_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
                );
                return ExitCode::Success;
            }
            "--html" => want_html = true,
            "--md" => want_md = true,
            other => {
                eprintln!("error: kio doc build: unexpected argument: {other}");
                eprintln!();
                eprintln!(
                    "{}",
                    HELP_BUILD_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
                );
                return ExitCode::Usage;
            }
        }
    }
    // HTML is the default when no format flag is given.
    if !want_html && !want_md {
        want_html = true;
    }

    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return ExitCode::Internal;
        }
    };

    // Discover every package in the cwd subtree and build docs for
    // each one whose build block declares a `docs` field. A package
    // with no docs field is skipped (not an error) so a workspace of
    // mixed library/doc packages builds the doc-bearing ones. Packages
    // are independent — a later package still builds after an earlier
    // one fails, and the overall exit code is the first failure.
    let roots = match crate::package_collection::discover_package_roots(&cwd) {
        Ok(roots) => roots,
        Err(crate::package_collection::WalkError::MultiplePackageFiles { root, paths }) => {
            eprintln!(
                "error: kio doc build: multiple `*.pkg.kio` files at the package root {}; expected exactly one per package directory",
                DisplayPath(&root)
            );
            for p in paths {
                eprintln!("  {}", DisplayPath(&p));
            }
            return ExitCode::Build;
        }
        Err(e) => {
            eprintln!(
                "error: kio doc build: walking source tree: {}",
                e.into_located().error.diag().1
            );
            return ExitCode::Internal;
        }
    };
    if roots.is_empty() {
        eprintln!(
            "error: kio doc: no `<name>.pkg.kio` at {} or its subdirectories — `kio doc` requires a \
             package file with a `build {{ … }}` block declaring a `docs` field; \
             run `kio init` to scaffold a package (see specs/package.md § Build target files)",
            DisplayPath(&cwd)
        );
        return ExitCode::Build;
    }

    // Each package is independent — own docs tree, own snippet
    // validation, disjoint render output — so the fan-out runs them in
    // parallel (`cmd::package_fanout`), buffering each package's
    // diagnostics and replaying them in input order. First-failure is
    // order-independent, so the overall code does not depend on which
    // worker finished first.
    let dirs: Vec<PathBuf> = roots.iter().map(|r| r.dir.clone()).collect();
    crate::cmd::package_fanout::run(
        &dirs,
        "kio doc build",
        |dir, cap| doc_build_one_package(dir, want_html, want_md, cap),
        |acc, next| {
            if acc != ExitCode::Success { acc } else { next }
        },
    )
}

/// Build the doc site for one package rooted at `package_root`. A
/// package with no `docs` field is skipped (returns `Success`); the
/// multi-package fan-out treats a docs-less package as a no-op.
fn doc_build_one_package(
    package_root: &Path,
    want_html: bool,
    want_md: bool,
    cap: &mut CapturedOutput,
) -> ExitCode {
    let config = match resolve_docs_config_at(package_root, false) {
        Ok(Some(c)) => c,
        Ok(None) => return ExitCode::Success,
        Err(code) => return code,
    };

    // Rendering only happens against a valid input — run `kio doc
    // check` first and abort with its exit code on failure.
    // Build's render walks the whole package; a partial check would
    // give a misleading "validation passed" signal for a site that
    // will then expose cross-references the partial check didn't
    // exercise. So `kio doc build` is whole-package by design.
    let check_code = check_docs(
        &config.package_root,
        &config.md_dir,
        &config.support_dirs,
        &[],
        cap,
    );
    if check_code != ExitCode::Success {
        return check_code;
    }

    let html_out = want_html.then_some(config.html_out.as_path());
    let md_out = want_md.then_some(config.md_out.as_path());
    match render::build(&config.package_root, &config.md_dir, html_out, md_out) {
        Ok(()) => {
            // Report the output directories package-relative — a
            // machine-independent message that golden tests can pin.
            let rel = |p: &Path| -> PathBuf {
                p.strip_prefix(&config.package_root)
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|_| p.to_path_buf())
            };
            if let Some(p) = html_out {
                cap_outln!(cap, "rendered HTML site to {}", DisplayPath(rel(p)));
            }
            if let Some(p) = md_out {
                cap_outln!(cap, "rendered Markdown site to {}", DisplayPath(rel(p)));
            }
            ExitCode::Success
        }
        Err(e) => {
            cap_errln!(cap, "error: kio doc build: {e}");
            ExitCode::Build
        }
    }
}

/// `kio doc check` — validate the package's Kiodoc content.
///
/// Positional `<path>` arguments restrict validation to the named
/// files (markdown under the docs tree or `.kio` sources under the
/// package). `kio doc check` accepts paths only (not module names)
/// — the two surfaces it validates are file-keyed, not module-keyed,
/// and forcing a single dispatch surface keeps the contract obvious.
fn run_check(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "{}",
            HELP_CHECK_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }
    for a in args {
        if a.starts_with("--") {
            eprintln!("error: kio doc check: unknown flag: {a}");
            eprintln!();
            eprintln!(
                "{}",
                HELP_CHECK_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
            );
            return ExitCode::Usage;
        }
    }
    let config = match resolve_docs_config() {
        Ok(c) => c,
        Err(code) => return code,
    };
    // `kio doc check` accepts paths only — module-name selectors are
    // a stretch here (the surface mixes markdown + .kio sources,
    // which aren't module-keyed). Parse each arg as a `Path`
    // selector unconditionally; an argument that doesn't lex as a
    // path-shaped selector still resolves through path semantics
    // and falls into the "no markdown or module file" branch.
    let selectors: Vec<crate::cmd::module_selector::Selector> = args
        .iter()
        .map(|a| {
            use crate::cmd::module_selector::Selector;
            // Promote any module-shaped arg to a Path so the
            // path-only filter never sees the Module variant.
            match crate::cmd::module_selector::parse(a, &config.package_root) {
                Selector::Path(p) => Selector::Path(p),
                Selector::Module(_) => {
                    let raw = std::path::PathBuf::from(a);
                    let joined = if raw.is_absolute() {
                        raw
                    } else {
                        config.package_root.join(raw)
                    };
                    let canonical = std::fs::canonicalize(&joined).unwrap_or(joined);
                    Selector::Path(canonical)
                }
            }
        })
        .collect();
    // Single-package path: `check_docs` buffers its diagnostics into a
    // local capture (so it can share the buffered fan-out code path);
    // replay it immediately since there is no parallel sibling to
    // interleave with.
    let mut cap = CapturedOutput::new();
    let code = check_docs(
        &config.package_root,
        &config.md_dir,
        &config.support_dirs,
        &selectors,
        &mut cap,
    );
    cap.replay_now();
    code
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocFmtMode {
    Rewrite,
    Check,
}

/// `kio doc fmt` — format Markdown Kiodoc snippets in place.
fn run_fmt(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "{}",
            HELP_FMT_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }

    let mut mode = DocFmtMode::Rewrite;
    let mut paths: Vec<String> = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--check" => mode = DocFmtMode::Check,
            other if other.starts_with("--") => {
                eprintln!("error: kio doc fmt: unknown flag: {other}");
                eprintln!();
                eprintln!(
                    "{}",
                    HELP_FMT_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
                );
                return ExitCode::Usage;
            }
            other => paths.push(other.to_owned()),
        }
    }

    let config = match resolve_docs_config() {
        Ok(c) => c,
        Err(code) => return code,
    };

    let mut md_files = Vec::new();
    if let Err(e) = collect_md_files(&config.md_dir, &mut md_files) {
        eprintln!(
            "error: kio doc fmt: walking {}: {}",
            DisplayPath(&config.md_dir),
            e
        );
        return ExitCode::Internal;
    }
    md_files.sort();
    md_files.dedup();

    if !paths.is_empty() {
        match apply_doc_fmt_selectors(&config.md_dir, &md_files, &paths) {
            Ok(selected) => md_files = selected,
            Err(code) => return code,
        }
    }

    let relativize = |p: &PathBuf| -> PathBuf {
        p.strip_prefix(&config.package_root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| p.clone())
    };
    let md_files: Vec<PathBuf> = md_files.iter().map(&relativize).collect();

    let results: Vec<DocFmtFileResult> = crate::maybe_par_iter!(md_files)
        .map(|path| format_markdown_file(path))
        .collect();

    let mut diagnostics = Vec::new();
    let mut dirty = Vec::new();
    for result in results {
        match result {
            DocFmtFileResult::Clean => {}
            DocFmtFileResult::Dirty { path, formatted } => dirty.push((path, formatted)),
            DocFmtFileResult::IoErr(path, e) => {
                eprintln!(
                    "error: kio doc fmt: cannot read {}: {}",
                    DisplayPath(&path),
                    e
                );
                return ExitCode::Internal;
            }
            DocFmtFileResult::DocumentErr { path, source, err } => {
                diagnostics.push(KiodocError::Document { path, source, err });
            }
            DocFmtFileResult::FormatErr {
                path,
                source,
                errors,
            } => {
                for err in errors {
                    diagnostics.push(KiodocError::Document {
                        path: path.clone(),
                        source: source.clone(),
                        err,
                    });
                }
            }
        }
    }

    if !diagnostics.is_empty() {
        diagnostics.sort_by_key(|e| e.sort_key());
        for err in &diagnostics {
            err.eprint();
        }
        return ExitCode::DocError;
    }

    if mode == DocFmtMode::Check {
        for (path, _) in &dirty {
            println!("{}", DisplayPath(path));
        }
        return if dirty.is_empty() {
            ExitCode::Success
        } else {
            ExitCode::FmtDiff
        };
    }

    for (path, formatted) in &dirty {
        if let Err(e) = crate::cmd::atomic_write::write_atomic(path, formatted.as_bytes()) {
            eprintln!(
                "error: kio doc fmt: cannot write {}: {}",
                DisplayPath(path),
                e
            );
            return ExitCode::Internal;
        }
        println!("{}", DisplayPath(path));
    }

    ExitCode::Success
}

fn apply_doc_fmt_selectors(
    md_dir: &Path,
    all_md_files: &[PathBuf],
    selectors: &[String],
) -> Result<Vec<PathBuf>, ExitCode> {
    let docs_root = std::fs::canonicalize(md_dir).unwrap_or_else(|_| md_dir.to_path_buf());
    let mut selected = std::collections::BTreeSet::new();

    for selector in selectors {
        let raw = PathBuf::from(selector);
        let path = if raw.is_absolute() {
            raw
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(raw)
        };
        let metadata = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "error: kio doc fmt: cannot stat {}: {}",
                    DisplayPath(&path),
                    e
                );
                return Err(ExitCode::Usage);
            }
        };
        let canonical = std::fs::canonicalize(&path).unwrap_or(path.clone());
        if !canonical.starts_with(&docs_root) {
            eprintln!(
                "error: kio doc fmt: {} is not under the configured docs.md tree {}",
                DisplayPath(&path),
                DisplayPath(&docs_root)
            );
            return Err(ExitCode::Usage);
        }
        if metadata.is_dir() {
            let mut nested = Vec::new();
            if let Err(e) = collect_md_files(&path, &mut nested) {
                eprintln!("error: kio doc fmt: walking {}: {}", DisplayPath(&path), e);
                return Err(ExitCode::Internal);
            }
            for md in nested {
                let canonical_md = std::fs::canonicalize(&md).unwrap_or(md.clone());
                if all_md_files.iter().any(|known| {
                    std::fs::canonicalize(known).unwrap_or_else(|_| known.clone()) == canonical_md
                }) {
                    selected.insert(md);
                }
            }
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            let Some(md) = all_md_files.iter().find(|known| {
                std::fs::canonicalize(known).unwrap_or_else(|_| (*known).clone()) == canonical
            }) else {
                eprintln!(
                    "error: kio doc fmt: {} is not a markdown file in the configured docs tree",
                    DisplayPath(&path)
                );
                return Err(ExitCode::Usage);
            };
            selected.insert(md.clone());
        } else {
            eprintln!(
                "error: kio doc fmt: {} is not a markdown file or directory",
                DisplayPath(&path)
            );
            return Err(ExitCode::Usage);
        }
    }

    Ok(selected.into_iter().collect())
}

fn format_markdown_file(path: &Path) -> DocFmtFileResult {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return DocFmtFileResult::IoErr(path.to_path_buf(), e),
    };
    let document = match Document::build(path, &source, parse::scan(&source)) {
        Ok(d) => d,
        Err(err) => {
            return DocFmtFileResult::DocumentErr {
                path: path.to_path_buf(),
                source,
                err,
            };
        }
    };
    let edits = match fmt::format_document(&source, &document) {
        Ok(edits) => edits,
        Err(errors) => {
            return DocFmtFileResult::FormatErr {
                path: path.to_path_buf(),
                source,
                errors,
            };
        }
    };
    if edits.is_empty() {
        DocFmtFileResult::Clean
    } else {
        DocFmtFileResult::Dirty {
            path: path.to_path_buf(),
            formatted: fmt::apply_edits(&source, &edits),
        }
    }
}

enum DocFmtFileResult {
    Clean,
    Dirty {
        path: PathBuf,
        formatted: String,
    },
    IoErr(PathBuf, std::io::Error),
    DocumentErr {
        path: PathBuf,
        source: String,
        err: DocError,
    },
    FormatErr {
        path: PathBuf,
        source: String,
        errors: Vec<DocError>,
    },
}

/// Validate every `.md` file under `md_dir` and every `.kio`
/// doc-comment under `package_root`. Returns the validation exit
/// code per `specs/exit-codes.md`.
///
/// `selectors` filters which files are validated: empty = every
/// markdown file plus every module's doc-comments; non-empty =
/// only the matching files. See the `kio doc` § per-spec entry in
/// `specs/cli.md` for the selector dispatch.
fn check_docs(
    package_root: &Path,
    md_dir: &Path,
    support_dirs: &[PathBuf],
    selectors: &[crate::cmd::module_selector::Selector],
    cap: &mut CapturedOutput,
) -> ExitCode {
    // Resolve the per-snippet doc cache from the package's
    // `build { ... }` block, rooted at the package root. A package
    // whose build block declares `cache "<path>";` enables the
    // cache; `cache ()` leaves the cache in the disabled-backend
    // form. Both paths go through [`validate::validate_snippet`]; the
    // disabled form just lookup-misses and store-no-ops.
    //
    // Gated on [`crate::cache::policy::caches_enabled`]: when the
    // operator hasn't opted in to the Kio-semantic caches, fall
    // back to the disabled variant regardless of what the build
    // file declares.
    let cache = if crate::cache::policy::caches_enabled() {
        DocCache::resolve_from_workspace(package_root)
    } else {
        DocCache::disabled()
    };

    // Collect .md files from the docs tree and regular .kio module
    // files from the whole package.
    let mut md_files: Vec<PathBuf> = Vec::new();
    let mut kio_files: Vec<PathBuf> = Vec::new();
    if let Err(e) = collect_md_files(md_dir, &mut md_files) {
        cap_errln!(
            cap,
            "error: kio doc: walking {}: {}",
            DisplayPath(&md_dir),
            e
        );
        return ExitCode::Internal;
    }
    if let Err(e) = doc_comments::collect_kio_files(package_root, &mut kio_files) {
        cap_errln!(
            cap,
            "error: kio doc: walking {}: {}",
            DisplayPath(&package_root),
            e
        );
        return ExitCode::Internal;
    }
    let support_files = match collect_package_module_support_files(package_root, support_dirs) {
        Ok(files) => files,
        Err(e) => {
            cap_errln!(
                cap,
                "error: kio doc: reading module support files under {}: {}",
                DisplayPath(&package_root),
                e
            );
            return ExitCode::Internal;
        }
    };
    md_files.sort();
    md_files.dedup();
    kio_files.sort();
    kio_files.dedup();

    // Apply selector filter. With an empty selector list, both
    // collections survive unchanged; with selectors present, only
    // the matching files survive. Unknown selectors are CLI usage
    // errors per the spec.
    if !selectors.is_empty() {
        match apply_selectors(&md_files, &kio_files, selectors, package_root) {
            Ok((mds, kios)) => {
                md_files = mds;
                kio_files = kios;
            }
            Err(code) => return code,
        }
    }

    // Relativize collected paths to the package root. The package
    // root is the process's current directory, so a path relative to
    // it still resolves for I/O; rendering it relative keeps
    // diagnostics package-rooted (`input/a.md:…`, not an absolute
    // path) — stable across machines and matching the golden corpus.
    let relativize = |p: &PathBuf| -> PathBuf {
        p.strip_prefix(package_root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| p.clone())
    };
    let md_files: Vec<PathBuf> = md_files.iter().map(&relativize).collect();
    let kio_files: Vec<PathBuf> = kio_files.iter().map(&relativize).collect();

    // Build the per-file document model sequentially. This phase is
    // I/O + markdown parsing; it produces the snippet inventory that
    // the parallel validation step iterates over. Document-build
    // errors (parse, attribute, pairing) are collected alongside
    // snippet-validation errors so the post-parallel sort prints
    // every diagnostic in source order regardless of which phase
    // produced it.
    let mut files: Vec<LoadedFile> = Vec::with_capacity(md_files.len());
    let mut errors: Vec<KiodocError> = Vec::new();
    for md in &md_files {
        match load_file(md) {
            Ok(loaded) => files.push(loaded),
            Err(LoadError::Io(code)) => return code,
            Err(LoadError::Document(path, source, err)) => {
                errors.push(KiodocError::Document { path, source, err })
            }
        }
    }

    // Check intra-doc references in each .md file's prose. This runs
    // after document-build so we have access to the fence spans (and
    // thus can exclude fence bodies from the prose scan).
    for file in &files {
        let ref_errors = check_md_inline_refs(&file.path, &file.source);
        for (span, message) in ref_errors {
            errors.push(KiodocError::Document {
                path: (*file.path).clone(),
                source: (*file.source).clone(),
                err: document::DocError { span, message },
            });
        }
    }

    // Extract doc-comment snippets from every .kio file.
    let mut dc_work: Vec<DocCommentSnippet> = Vec::new();
    for kio in &kio_files {
        let kio_source = match fs::read_to_string(kio) {
            Ok(s) => s,
            Err(e) => {
                cap_errln!(
                    cap,
                    "error: kio doc: cannot read {}: {}",
                    DisplayPath(&kio),
                    e
                );
                return ExitCode::Internal;
            }
        };
        match doc_comments::extract_doc_snippets(kio, &kio_source) {
            Ok(snippets) => dc_work.extend(snippets),
            Err(errs) => {
                for err in errs {
                    errors.push(KiodocError::DocComment {
                        path: kio.clone(),
                        source: kio_source.clone(),
                        err,
                    });
                }
            }
        }
    }

    // Build the flat snippet list across every loaded .md file. Each
    // entry borrows back into the per-file `Document` via `Arc` so
    // workers share a single immutable handle. Members of an
    // accumulating harness skip the per-snippet path — they validate
    // as one aggregate per harness, collected below.
    let mut work: Vec<SnippetWorkItem> = Vec::new();
    let mut aggregate_work: Vec<AggregateWorkItem> = Vec::new();
    for file in &files {
        // Group accumulating-harness members in document order.
        let mut accum_members: std::collections::HashMap<String, Vec<document::Snippet>> =
            std::collections::HashMap::new();
        for snippet in file.document.snippets() {
            if snippet.ignored {
                continue;
            }
            if let Some(name) = &snippet.harness_ref
                && let Some(h) = file.document.harnesses.get(name)
                && h.accumulate
            {
                accum_members
                    .entry(name.clone())
                    .or_default()
                    .push(snippet.clone());
                continue;
            }
            work.push(SnippetWorkItem {
                path: file.path.clone(),
                source: file.source.clone(),
                document: file.document.clone(),
                snippet: snippet.clone(),
            });
        }
        // Each accumulating harness produces one aggregate work item.
        // Every accumulating harness is included — even with zero
        // members — so the validator's no-member short-circuit
        // (Ok(())) gets exercised consistently.
        for (name, h) in &file.document.harnesses {
            if !h.accumulate {
                continue;
            }
            let members = accum_members.remove(name).unwrap_or_default();
            aggregate_work.push(AggregateWorkItem {
                path: file.path.clone(),
                source: file.source.clone(),
                harness: h.clone(),
                members,
            });
        }
    }

    // Fan the per-snippet validation out across rayon workers.
    // Both .md snippets and .kio doc-comment snippets share one
    // rayon pass so their errors can be merged and sorted together.
    let md_validation_errors: Vec<KiodocError> = crate::maybe_par_iter!(work)
        .filter_map(|item| {
            match validate::validate_snippet(
                &item.path,
                &item.source,
                &item.document,
                &item.snippet,
                &support_files,
                &cache,
            ) {
                Ok(()) => None,
                Err(err) => Some(KiodocError::Validation {
                    path: item.path.clone(),
                    source: item.source.clone(),
                    err,
                }),
            }
        })
        .collect();
    errors.extend(md_validation_errors);

    // Each accumulating harness validates once over all its members.
    let aggregate_validation_errors: Vec<KiodocError> = crate::maybe_par_iter!(aggregate_work)
        .filter_map(|item| {
            match validate::validate_aggregate(
                &item.path,
                &item.harness,
                &item.members,
                &support_files,
                &cache,
            ) {
                Ok(()) => None,
                Err(err) => Some(KiodocError::Validation {
                    path: item.path.clone(),
                    source: item.source.clone(),
                    err,
                }),
            }
        })
        .collect();
    errors.extend(aggregate_validation_errors);

    let dc_validation_errors: Vec<KiodocError> = crate::maybe_par_iter!(dc_work)
        .filter_map(
            |snippet| match doc_comments::validate_doc_comment_snippet(snippet, &cache) {
                Ok(()) => None,
                Err(err) => Some(KiodocError::DocComment {
                    path: snippet.kio_path.clone(),
                    source: snippet.kio_source.clone(),
                    err,
                }),
            },
        )
        .collect();
    errors.extend(dc_validation_errors);

    // Sort errors deterministically by (file_path, span.start) so
    // user-facing stderr is byte identical across thread counts.
    // This is what makes the multi-error goldens stable under rayon's
    // work-stealing scheduler.
    errors.sort_by_key(|e| e.sort_key());

    let any_failure = !errors.is_empty();
    for err in &errors {
        err.render(&mut cap.stderr);
    }

    if any_failure {
        ExitCode::DocError
    } else {
        ExitCode::Success
    }
}

/// Validate one markdown file end to end — a single-file convenience
/// wrapper. `kio doc check` itself goes through [`check_docs`], which
/// walks the package's `docs.md` tree and `.kio` sources; this entry
/// validates a single `.md` file in isolation.
///
/// The cache is resolved from the process's current working
/// directory; callers that want a specific cache configuration
/// (or none) should go through [`check_docs`] directly.
pub fn check_file(path: &Path) -> Result<(), ExitCode> {
    let package_root = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(_) => PathBuf::from("."),
    };
    let support_files = collect_package_module_support_files(&package_root, &[]).map_err(|e| {
        eprintln!(
            "error: kio doc: reading module support files under {}: {}",
            DisplayPath(&package_root),
            e
        );
        ExitCode::Internal
    })?;
    let cache = if crate::cache::policy::caches_enabled() {
        match std::env::current_dir() {
            Ok(cwd) => DocCache::resolve_from_workspace(&cwd),
            Err(_) => DocCache::disabled(),
        }
    } else {
        DocCache::disabled()
    };
    match load_file(path) {
        Ok(loaded) => {
            // Split snippets between individual validations and
            // accumulating-harness members. The latter aggregate per
            // harness; the former go straight through `validate_snippet`.
            let mut individual: Vec<Snippet> = Vec::new();
            let mut accum_members: std::collections::HashMap<String, Vec<Snippet>> =
                std::collections::HashMap::new();
            for snippet in loaded.document.snippets() {
                if snippet.ignored {
                    continue;
                }
                if let Some(name) = &snippet.harness_ref
                    && let Some(h) = loaded.document.harnesses.get(name)
                    && h.accumulate
                {
                    accum_members
                        .entry(name.clone())
                        .or_default()
                        .push(snippet.clone());
                    continue;
                }
                individual.push(snippet.clone());
            }

            let mut errors: Vec<KiodocError> = crate::maybe_par_iter!(individual)
                .filter_map(|snippet| {
                    match validate::validate_snippet(
                        &loaded.path,
                        &loaded.source,
                        &loaded.document,
                        snippet,
                        &support_files,
                        &cache,
                    ) {
                        Ok(()) => None,
                        Err(err) => Some(KiodocError::Validation {
                            path: loaded.path.clone(),
                            source: loaded.source.clone(),
                            err,
                        }),
                    }
                })
                .collect();

            // One aggregate per accumulating harness, including any
            // empty ones (validate_aggregate's no-member short-circuit
            // covers that case).
            let aggregates: Vec<(document::Harness, Vec<Snippet>)> = loaded
                .document
                .harnesses
                .values()
                .filter(|h| h.accumulate)
                .map(|h| (h.clone(), accum_members.remove(&h.name).unwrap_or_default()))
                .collect();
            let aggregate_errors: Vec<KiodocError> = crate::maybe_par_iter!(aggregates)
                .filter_map(|(h, members)| {
                    match validate::validate_aggregate(
                        &loaded.path,
                        h,
                        members,
                        &support_files,
                        &cache,
                    ) {
                        Ok(()) => None,
                        Err(err) => Some(KiodocError::Validation {
                            path: loaded.path.clone(),
                            source: loaded.source.clone(),
                            err,
                        }),
                    }
                })
                .collect();
            errors.extend(aggregate_errors);

            errors.sort_by_key(|e| e.sort_key());
            for err in &errors {
                err.eprint();
            }
            if errors.is_empty() {
                Ok(())
            } else {
                Err(ExitCode::DocError)
            }
        }
        Err(LoadError::Io(code)) => Err(code),
        Err(LoadError::Document(path, source, err)) => {
            let e = KiodocError::Document { path, source, err };
            e.eprint();
            Err(ExitCode::DocError)
        }
    }
}

fn collect_package_module_support_files(
    package_root: &Path,
    support_dirs: &[PathBuf],
) -> std::io::Result<Vec<validate::AssembledFile>> {
    let mut files = Vec::new();
    let mut seen_roots = std::collections::BTreeSet::new();
    collect_module_support_files_from_dir(package_root, &mut files, &mut seen_roots)?;
    for dir in support_dirs {
        collect_module_support_files_from_dir(dir, &mut files, &mut seen_roots)?;
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn collect_module_support_files_from_dir(
    dir: &Path,
    files: &mut Vec<validate::AssembledFile>,
    seen_roots: &mut std::collections::BTreeSet<String>,
) -> std::io::Result<()> {
    let mut paths = Vec::new();
    collect_support_module_files(dir, dir, &mut paths)?;
    paths.sort();
    let mut added_roots = std::collections::BTreeSet::new();
    for path in paths {
        let relative = path
            .strip_prefix(dir)
            .expect("support walk stays under its root");
        let Some(segments) = relative
            .iter()
            .map(|part| part.to_str())
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let name = segments.join("/");
        let module_path = name
            .strip_suffix(crate::file_kind::KIO_SUFFIX)
            .expect("support walk selects module files");
        let root = module_path.split('/').next().expect("module path");
        if seen_roots.contains(root) {
            continue;
        }
        added_roots.insert(root.to_owned());
        files.push(validate::AssembledFile {
            path: name,
            body: fs::read_to_string(&path)?,
        });
    }
    seen_roots.extend(added_roots);
    Ok(())
}

fn collect_support_module_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    let entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    if dir != root
        && entries.iter().any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(crate::file_kind::is_package_file)
        })
    {
        return Ok(());
    }
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let name = name.to_str().unwrap_or("");
        if file_type.is_dir() {
            if name != "out" && name != "target" && !name.starts_with('.') {
                collect_support_module_files(root, &path, out)?;
            }
        } else if crate::file_kind::is_module_file(name)
            && (file_type.is_file() || (file_type.is_symlink() && path.is_file()))
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Read one markdown file from disk, run the fence scanner over its
/// bytes, and build the document model. Returns the fully-loaded
/// file ready for snippet fan-out, or a structured load error
/// classified by category (I/O vs. document-build).
fn load_file(path: &Path) -> Result<LoadedFile, LoadError> {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: kio doc: cannot read {}: {}", DisplayPath(&path), e);
            return Err(LoadError::Io(ExitCode::Internal));
        }
    };
    let fences = parse::scan(&source);
    let document = match Document::build(path, &source, fences) {
        Ok(d) => d,
        Err(err) => return Err(LoadError::Document(path.to_path_buf(), source, err)),
    };
    Ok(LoadedFile {
        path: Arc::new(path.to_path_buf()),
        source: Arc::new(source),
        document: Arc::new(document),
    })
}

/// A markdown file's loaded state — source + parsed document — held
/// behind `Arc` so per-snippet workers can share immutable views
/// without copying.
struct LoadedFile {
    path: Arc<PathBuf>,
    source: Arc<String>,
    document: Arc<Document>,
}

/// One unit of per-snippet work passed to the rayon `par_iter`.
/// Holds the snippet by clone so a worker doesn't have to re-walk
/// the document's fence vector at validation time; the shared
/// `Document` is still kept (the validator passes it to
/// `validate_snippet` so harness substitution can look up the
/// snippet's harness body) behind an `Arc`.
struct SnippetWorkItem {
    path: Arc<PathBuf>,
    source: Arc<String>,
    document: Arc<Document>,
    snippet: Snippet,
}

/// One unit of aggregate-harness validation work — one entry per
/// accumulating harness in one markdown file. The validator
/// concatenates the member bodies (already pre-collected in
/// document order) and substitutes the result into the harness
/// once.
struct AggregateWorkItem {
    path: Arc<PathBuf>,
    source: Arc<String>,
    harness: document::Harness,
    members: Vec<Snippet>,
}

/// A document-build failure caught during the load phase. Carries
/// the source and path so the post-parallel error-print step can
/// render it alongside any validation errors from other files.
enum LoadError {
    Io(ExitCode),
    Document(PathBuf, String, DocError),
}

/// One emitted kiodoc-level error, kept structured until the
/// post-parallel sort runs. All variants carry the source path
/// (sort domain key) and a byte offset (sort domain tiebreak), so
/// errors from any file or phase share one total order.
enum KiodocError {
    Document {
        path: PathBuf,
        source: String,
        err: DocError,
    },
    Validation {
        path: Arc<PathBuf>,
        source: Arc<String>,
        err: ValidationError,
    },
    /// A doc-comment-level error from a `.kio` source file.
    DocComment {
        path: PathBuf,
        source: String,
        err: DocCommentError,
    },
}

impl KiodocError {
    /// `(file_path, start_byte)` — the spec's sort key. Document and
    /// validation errors are interleaved on this key so the final
    /// printed output stays in source order across every file.
    fn sort_key(&self) -> (PathBuf, u32) {
        match self {
            KiodocError::Document { path, err, .. } => (path.clone(), err.span.start),
            KiodocError::Validation { path, err, .. } => ((**path).clone(), err.span.start),
            KiodocError::DocComment { path, err, .. } => (path.clone(), err.span.start),
        }
    }

    fn eprint(&self) {
        match self {
            KiodocError::Document { path, source, err } => err.eprint(path, source),
            KiodocError::Validation { path, source, err } => err.eprint(path, source),
            KiodocError::DocComment { path, source, err } => err.eprint(path, source),
        }
    }

    /// As [`Self::eprint`], but renders into `buf` so the multi-package
    /// fan-out can buffer each package's diagnostics and replay them in
    /// input order (`cmd::package_fanout`).
    fn render(&self, buf: &mut String) {
        let rendered = match self {
            KiodocError::Document { path, source, err } => err.render(path, source),
            KiodocError::Validation { path, source, err } => err.render(path, source),
            KiodocError::DocComment { path, source, err } => err.render(path, source),
        };
        buf.push_str(&rendered);
    }
}

/// Check all `` [`name`] `` intra-doc references and
/// `` [`@KEYWORD term`] `` directives in the prose of a `.md` file.
/// Returns a list of `(span, message)` pairs, one per error.
///
/// The package scope is derived by searching for a `*.pkg.kio`
/// file near the `.md` file — typically two levels up from the
/// docs directory. If no package file is found, only fully-qualified
/// paths and empty-scope names are accepted.
///
/// References and directive terms overridden by a Markdown reference-
/// link definition (`[name]: url`) are silently accepted.
fn check_md_inline_refs(md_path: &Path, md_source: &str) -> Vec<(crate::span::Span, String)> {
    // Collection excludes fenced examples internally.
    let overrides = refs::collect_ref_overrides(md_source);

    let prose = parse::blank_fences(md_source);

    // Build a package scope by searching for a package file near
    // the `.md` file.
    let scope = find_package_scope_for_md(md_path);

    let all_names: Vec<String> = scope.exported_names.clone();

    let mut results: Vec<(crate::span::Span, String)> = Vec::new();

    // Validate plain `` [`name`] `` intra-doc refs.
    let ref_errors = refs::check_refs_in_package(&prose, &scope, &overrides);
    for re in ref_errors {
        let span = crate::span::Span::new(re.offset, re.offset);
        let suggestion = refs::closest_name(&re.name, &all_names);
        let message = refs::format_unresolved_ref_message(&re.name, suggestion);
        results.push((span, message));
    }

    // Validate `` [`@KEYWORD term`] `` directives.
    let (unknown_kw_errors, unresolved_term_errors, type_level_errors) =
        kio_directives::check_directives_in_package(&prose, &scope, &overrides);

    for uke in unknown_kw_errors {
        let span = crate::span::Span::new(uke.offset, uke.offset);
        let message = kio_directives::format_unknown_directive_message(&uke.keyword);
        results.push((span, message));
    }

    for ute in unresolved_term_errors {
        let span = crate::span::Span::new(ute.offset, ute.offset);
        let suggestion = refs::closest_name(&ute.term, &all_names);
        let message =
            kio_directives::format_unresolved_directive_message(ute.keyword, &ute.term, suggestion);
        results.push((span, message));
    }

    for tle in type_level_errors {
        let span = crate::span::Span::new(tle.offset, tle.offset);
        let message = kio_directives::format_type_level_directive_message(&tle.term, tle.kind);
        results.push((span, message));
    }

    // Sort by byte offset for deterministic output.
    results.sort_by_key(|(span, _)| span.start);
    results
}

/// Try to find a package file near `md_path` and build a
/// [`refs::PackageScope`] from it. If no package file is found,
/// returns an empty scope (which only accepts fully-qualified paths).
fn find_package_scope_for_md(md_path: &Path) -> refs::PackageScope {
    // Search parent and grandparent directories for `*.pkg.kio`.
    // When `md_path` is relative (e.g. `input.md`), its parent is
    // `""` which `read_dir` rejects — canonicalize to `.` in that case.
    let to_search_dir = |p: &Path| -> PathBuf {
        if p.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            p.to_path_buf()
        }
    };
    let search_dirs: Vec<_> = {
        let mut dirs = Vec::new();
        if let Some(p) = md_path.parent() {
            dirs.push(to_search_dir(p));
            if let Some(gp) = p.parent() {
                dirs.push(to_search_dir(gp));
                if let Some(ggp) = gp.parent() {
                    dirs.push(to_search_dir(ggp));
                }
            }
        } else {
            dirs.push(PathBuf::from("."));
        }
        dirs
    };

    for dir in &search_dirs {
        // Collect all `*.pkg.kio` files in this directory.
        let read_dir = match fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let Some(stem) = crate::file_kind::package_stem(name) else {
                continue;
            };
            let stem = stem.to_owned();
            let src = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let package_file = match crate::pass::parser::parse_package_file(&src, Some(&stem)) {
                Ok(e) => e,
                Err(_) => continue,
            };
            // The package file names no declarations of its own — its
            // `bridge { … }` glob list selects the modules whose
            // `host` items and `pub` declarations form the package
            // boundary. Gather those names from the bridged module
            // files so `.md` prose references (`` [`print`] ``) and
            // module-segment qualified paths resolve, per
            // `specs/kiodoc.md` § Reference resolution.
            let bridged = gather_bridged_boundary(dir, &package_file);
            let module_segments: Vec<&str> =
                bridged.module_segments.iter().map(String::as_str).collect();
            let mut scope = refs::package_scope_from_package_file(&package_file, &module_segments);
            scope.exported_names.extend(bridged.names);
            scope.type_level.extend(bridged.type_level);
            return scope;
        }
    }

    refs::PackageScope::default()
}

/// The package-boundary names and module segments gathered from a
/// package's bridged module files.
#[derive(Default)]
struct BridgedBoundary {
    names: Vec<String>,
    module_segments: Vec<String>,
    type_level: Vec<(String, refs::TypeLevelKind)>,
}

pub(crate) fn module_is_bridged(
    package_file: &crate::ast::PackageFile,
    module: &crate::ast::Module,
) -> bool {
    let module_path = module.path.segments.join("/");
    package_file.bridge.as_ref().is_some_and(|bridge| {
        bridge
            .globs
            .iter()
            .any(|glob| crate::pass::resolve::glob_matches(&glob.segments, &module_path))
    })
}

/// Walk `package_dir` for `.kio` module files selected by the package
/// file's `bridge { … }` globs and collect each matched module's
/// public declaration and role names plus
/// the module-path segments. Files that fail to parse are skipped —
/// the standalone `kio doc` walker is best-effort and never aborts the
/// whole run on one malformed module.
fn gather_bridged_boundary(
    package_dir: &Path,
    package_file: &crate::ast::PackageFile<crate::ast::Surface>,
) -> BridgedBoundary {
    let mut boundary = BridgedBoundary::default();
    if package_file.bridge.is_none() {
        return boundary;
    }

    let mut kio_files: Vec<PathBuf> = Vec::new();
    if collect_kio_module_files(package_dir, &mut kio_files).is_err() {
        return boundary;
    }

    let mut seen_segments: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for path in kio_files {
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(file) = crate::pass::parser::parse_module_file(&src) else {
            continue;
        };
        let module = &file.module;
        if !module_is_bridged(package_file, module) {
            continue;
        }
        for seg in &module.path.segments {
            if seen_segments.insert(seg.name.clone()) {
                boundary.module_segments.push(seg.name.clone());
            }
        }
        let module_scope = refs::module_scope_from_surface_for_boundary(module);
        boundary.names.extend(module_scope.top_level);
        boundary.type_level.extend(module_scope.type_level);
    }
    boundary
}

/// Walk `dir` recursively, appending every `.kio` module file (not a
/// `*.pkg.kio` package file) to `out`. Skips build-artifact (`out`,
/// `target`) and hidden (`.*`) directories — mirrors
/// [`collect_md_files`].
fn collect_kio_module_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        // Branch on the *un-followed* file type so a directory symlink
        // is treated as a leaf, never descended — a `docs/self -> docs`
        // loop would otherwise recurse unbounded. Matches
        // `package_collection`'s symlink discipline.
        if entry.file_type()?.is_dir() {
            if name == "target" || name == "out" || name.starts_with('.') {
                continue;
            }
            collect_kio_module_files(&path, out)?;
        } else if crate::file_kind::is_module_file(name) {
            out.push(path);
        }
    }
    Ok(())
}

/// Walk `dir` recursively, appending every `.md` file to `out`.
/// Skips build-artifact (`out`, `target`) and hidden (`.*`)
/// directories — mirrors `kio fmt`'s walker.
fn collect_md_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        // Un-followed file type so a directory symlink is a leaf, never
        // descended (a symlink loop would otherwise recurse unbounded).
        if entry.file_type()?.is_dir() {
            if name == "target" || name == "out" || name.starts_with('.') {
                continue;
            }
            collect_md_files(&path, out)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn write_tempfile(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        p
    }

    #[test]
    fn empty_md_file_passes() {
        let dir = tempdir();
        let md = write_tempfile(dir.path(), "x.md", "# Just prose\n\nNothing to validate.\n");
        assert_eq!(check_file(&md), Ok(()));
    }

    #[test]
    fn ignore_fence_passes_without_validation() {
        let dir = tempdir();
        let md = write_tempfile(
            dir.path(),
            "x.md",
            "```kio {ignore}\nthis is intentionally invalid Kio source\n```\n",
        );
        assert_eq!(check_file(&md), Ok(()));
    }

    #[test]
    fn bare_kio_fence_without_attrs_is_error() {
        let dir = tempdir();
        let md = write_tempfile(dir.path(), "x.md", "```kio\nlet x = 1\n```\n");
        assert!(check_file(&md).is_err());
    }

    #[test]
    fn markdown_refs_ignore_every_fence_language() {
        let dir = tempdir();
        let md = write_tempfile(
            dir.path(),
            "x.md",
            "```text\n[`missing`] [`@signature missing`]\n```\n<!--markdown\n[`also_missing`]\n-->\n",
        );
        assert_eq!(check_file(&md), Ok(()));
    }

    #[test]
    fn fenced_reference_definition_does_not_override_markdown_prose() {
        let dir = tempdir();
        let source = "See [`missing`].\n\n```markdown\n[missing]: https://example.com\n```\n";
        let md = write_tempfile(dir.path(), "x.md", source);
        let errors = check_md_inline_refs(&md, source);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].1.contains("`missing` is not in scope"));
    }

    #[test]
    fn markdown_package_scope_excludes_non_exported_recursive_members() {
        let dir = tempdir();
        write_tempfile(
            dir.path(),
            "pkg.pkg.kio",
            "package pkg;\nbridge { pkg/main; }\n",
        );
        write_tempfile(
            dir.path(),
            "main.kio",
            "module pkg/main;\n\
             rec(loop) {\n\
               fn local(value: .) -> . { rec exported(value) };\n\
               pub(pkg) fn scoped(value: .) -> . { rec local(value) };\n\
               pub fn exported(value: .) -> . { rec scoped(value) }\n\
             }\n",
        );
        let md = write_tempfile(dir.path(), "guide.md", "See [`exported`].\n");
        let scope = find_package_scope_for_md(&md);

        assert!(scope.exported_names.iter().any(|name| name == "exported"));
        assert!(!scope.exported_names.iter().any(|name| name == "local"));
        assert!(!scope.exported_names.iter().any(|name| name == "scoped"));
    }

    #[test]
    fn configured_support_dirs_extend_package_module_support_files() {
        let dir = tempdir();
        let package = dir.path().join("package");
        let support = dir.path().join("support");
        fs::create_dir(&package).unwrap();
        fs::create_dir(&support).unwrap();
        write_tempfile(&package, "local.kio", "module local;\n");
        write_tempfile(
            &package,
            "shared.kio",
            "module shared; fn root() -> . { () }\n",
        );
        write_tempfile(&support, "external.kio", "module external;\n");
        write_tempfile(
            &support,
            "shared.kio",
            "module shared; fn stale() -> . { () }\n",
        );

        let files = collect_package_module_support_files(&package, &[support]).unwrap();
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, vec!["external.kio", "local.kio", "shared.kio"]);
        let shared = files.iter().find(|file| file.path == "shared.kio").unwrap();
        assert!(shared.body.contains("root()"));
    }

    #[test]
    fn support_discovery_keeps_full_paths_and_nearest_package_boundary() {
        let dir = tempdir();
        for child in ["helpers", "nested", "out", "target", ".hidden"] {
            fs::create_dir(dir.path().join(child)).unwrap();
        }
        write_tempfile(&dir.path().join("helpers"), "a.kio", "module helpers/a;");
        write_tempfile(&dir.path().join("helpers"), "b.kio", "module helpers/b;");
        write_tempfile(&dir.path().join("nested"), "own.pkg.kio", "package own;");
        for child in ["nested", "out", "target", ".hidden"] {
            write_tempfile(&dir.path().join(child), "ignored.kio", "not a module");
        }
        let files = collect_package_module_support_files(dir.path(), &[]).unwrap();
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["helpers/a.kio", "helpers/b.kio"]);
    }

    #[test]
    fn earlier_support_namespace_shadows_later_descendants_as_a_unit() {
        let dir = tempdir();
        let package = dir.path().join("package");
        let first = dir.path().join("first");
        let later = dir.path().join("later");
        for root in [&package, &first, &later] {
            fs::create_dir(root).unwrap();
            fs::create_dir(root.join("helpers")).unwrap();
        }
        write_tempfile(&first.join("helpers"), "a.kio", "module helpers/a;");
        write_tempfile(&first.join("helpers"), "b.kio", "module helpers/b;");
        write_tempfile(&later.join("helpers"), "c.kio", "module helpers/c;");
        let files = collect_package_module_support_files(&package, &[first, later]).unwrap();
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["helpers/a.kio", "helpers/b.kio"]);
    }

    #[cfg(unix)]
    #[test]
    fn support_discovery_does_not_follow_directory_symlinks() {
        let dir = tempdir();
        write_tempfile(dir.path(), "actual.kio", "module actual;");
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop.kio")).unwrap();
        let files = collect_package_module_support_files(dir.path(), &[]).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "actual.kio");
    }

    fn tempdir() -> tempfile::TempDir {
        // `tempfile::TempDir` cleans up on drop — the previous
        // `PathBuf` flavour leaked, leaving hundreds of
        // `kio-doc-test-*` dirs in `/tmp` after a few `cargo test`
        // cycles.
        tempfile::Builder::new()
            .prefix("kio-doc-test-")
            .tempdir()
            .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn kio_module_walk_terminates_on_dir_symlink_loop() {
        // A directory symlink `self -> .` would recurse unbounded if the
        // walk followed it. Branching on the un-followed file type treats
        // the symlink as a leaf, so the walk terminates and never
        // descends through the loop.
        let dir = tempdir();
        let root = dir.path();
        write_tempfile(root, "m.kio", "module m;\n");
        std::os::unix::fs::symlink(root, root.join("self")).expect("mk symlink loop");

        let mut out = Vec::new();
        // The load-bearing assertion is that this returns at all (no
        // unbounded recursion / stack overflow).
        collect_kio_module_files(root, &mut out).expect("walk terminates");
        assert!(
            out.iter().any(|p| p.ends_with("m.kio")),
            "the real module file is still collected"
        );
    }
}
