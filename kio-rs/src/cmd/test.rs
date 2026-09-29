//! Implementation of `kio test`.
//!
//! Walks the cwd's Kio source tree, parses + typechecks the package,
//! collects every `Item::Equiv` from the typed `Lowered` AST, and
//! discharges each one by partial-evaluating its `term` bodies via
//! [`crate::normalization`] and grouping the residual NFs by
//! alpha+eta-equivalence. A pass means exactly one NF group per
//! `equiv`; a fail means at least one block split into ≥2 groups.
//!
//! The runner retains the typed `Lowered` package because substitution
//! filters `Equiv` items out of the codegen-input AST, while `kio test`
//! must read those items. Before discharging them, it substitutes and
//! validates a separate Prime copy of the package so the same final
//! artifact contract holds as in the build pipeline.
//!
//! A package that declares no in-scope `equiv` block takes a fast path:
//! the walk censuses `equiv` items straight from the lazily-parsed
//! modules (bodies stay deferred), and finding none, `kio test` just
//! runs the `kio check` pipeline — reusing its package-check cache — and
//! prints `no equiv blocks found`, rather than lowering, collecting
//! elaborations, and building discharge tables it would throw away. This
//! matches `specs/cli.md` § `kio test`: step 1 is the `kio check`
//! pipeline; step 5 is the no-equiv report.
//!
//! `kio-prime` does not implement `kio test` — it parses Kio' only,
//! and `equiv` is a surface-only declaration form. Invoking
//! `kio-prime test` is a usage error.
//!
//! Equiv blocks in modules materialized from a dependency are skipped
//! by default: a consumer's `kio test` tests the consumer, not its
//! dependencies' internals. The command reads the `<local>.dep.kio`
//! local names (the leading path segments the dependency's modules are
//! re-rooted under) to recognize those modules; the evaluator stays
//! dependency-agnostic. `--include-deps` opts them back in. See
//! `specs/cli.md` § `kio test`.

// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(â¦)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
use crate::ast::{Expr, Item, Lowered, Module, UncheckedPrime};
use crate::cache::keys::{DeclaredModulePath, EquivRenderMode};
use crate::cmd::check::{
    analyze_parsed_workspace_with, prepare_resolved_package_from_walked, report_analysis_failure,
    report_located, walk_or_report,
};
use crate::exit_code::ExitCode;
use crate::normalization::{
    AuthenticatedEvalCtx, Env, EvalCtx, EvalMetrics, EvalMetricsSnapshot, Value,
    eval_authenticated, nf_eq_checked, structural_recur_stuck_message, value_to_string,
};
use crate::pass::full::FullPipeline;
use crate::pass::resolve::Package;
use crate::pass::typecheck_full::check_normalized_package_collect_errors;
#[cfg(feature = "parallel")]
use rayon::prelude::*;

const HELP_TEMPLATE: &str = "\
Usage: kio test [<module>...]

Discharge every `equiv` declaration in the current package by
partial-evaluating each `term` body and reporting whether they all
reduce to the same residual normal form.

With no positional argument, every module in the package is
discharged. With one or more <module> selectors, only the matching
modules' equivs run. A selector is either a module path
(`pkg/utils/string`) or a filename path (`src/utils.kio`).

`equiv` blocks in modules materialized from a dependency (declared in
a `<local>.dep.kio` file) are skipped by default — `kio test` tests
the current package, not its dependencies' internals. Pass
--include-deps to discharge them too.

Options:
  --include-deps    Also discharge equiv blocks in dependency modules.
  -h, --help        Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 if every equiv passes
(or the source tree declares none); 50 if any equiv splits into ≥2
normal-form groups; 16 if evaluated structural recursion fails its Totality
check; another matching `1x` category code for compile-time failures
(parse / use / name-res / type errors); 2 on CLI usage error
(unknown selector, or no .kio files in the current directory).

See {base}/specs/cli.md#kio-test-module for full command behavior.";

/// The step-5 report printed when the source tree declares no in-scope
/// `equiv` block (see `specs/cli.md` § `kio test`). Shared by the
/// no-equiv fast path and `discharge_equivs`'s zero-job arm so the two
/// spell it identically.
const NO_EQUIV_BLOCKS_REPORT: &str = "no equiv blocks found";

use crate::normalization::{FnMap, QualifiedFnMap};

struct EquivJob<'s, 'e> {
    path_str: &'s str,
    file_path: std::path::PathBuf,
    module: &'e Module<UncheckedPrime>,
    name: String,
    equiv_span: crate::span::Span,
    sig: crate::ast::Signature<UncheckedPrime>,
    terms: Vec<Expr<UncheckedPrime>>,
    term_spans: Vec<crate::span::Span>,
    fn_defs: FnMap<'e>,
    qualified_fn_defs: QualifiedFnMap<'e>,
}

enum EquivDischargeResult {
    Completed {
        passed: bool,
        output: String,
        metrics: EvalMetricsSnapshot,
    },
    Totality {
        span: crate::span::Span,
        message: String,
        metrics: EvalMetricsSnapshot,
    },
}

impl EquivDischargeResult {
    fn metrics(&self) -> &EvalMetricsSnapshot {
        match self {
            Self::Completed { metrics, .. } | Self::Totality { metrics, .. } => metrics,
        }
    }
}

pub fn run(args: &[String], prime_only: bool) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "{}",
            HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }
    if prime_only {
        eprintln!(
            "error: `kio test` is not available under `kio-prime` — `equiv` is a surface-only \
             declaration form, not part of the Kio' grammar"
        );
        return ExitCode::Usage;
    }
    // `--include-deps` opts the dependency modules' equiv blocks back
    // into the run (default skips them). Any other flag-looking arg is
    // rejected up front; positional selectors are accepted.
    let mut include_deps = false;
    let mut selector_args: Vec<&String> = Vec::new();
    for a in args {
        if a == "--include-deps" {
            include_deps = true;
        } else if a.starts_with('-') {
            eprintln!("error: unknown flag for `kio test`: {a}");
            return ExitCode::Usage;
        } else {
            selector_args.push(a);
        }
    }
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return ExitCode::Internal;
        }
    };
    let selectors: Vec<crate::cmd::module_selector::Selector> = selector_args
        .iter()
        .map(|a| crate::cmd::module_selector::parse(a, &cwd))
        .collect();
    // With no positional selector, a directory containing zero `.kio`
    // files is a usage error. This is distinct from a directory that
    // has `.kio` source but declares no `equiv` blocks — that case
    // still prints "no equiv blocks found" and exits 0 (see
    // `discharge_equivs`). With explicit selectors a missing file is
    // the separate unknown-selector path.
    if selectors.is_empty()
        && let Err(code) = crate::cmd::check::error_if_no_kio_files(&cwd)
    {
        return code;
    }
    match run_inner(&selectors, include_deps, &cwd) {
        Ok(()) => ExitCode::Success,
        Err(code) => code,
    }
}

fn run_inner(
    selectors: &[crate::cmd::module_selector::Selector],
    include_deps: bool,
    cwd: &std::path::Path,
) -> Result<(), ExitCode> {
    // Dependencies are materialized at `kio dep fetch`/`update` and committed;
    // the prep walk consumes their re-rooted modules as ordinary source.
    // `kio test` does not re-materialize.

    // The dependency local names are the leading path segments under
    // which `materialize_dependencies` re-rooted each dependency's
    // modules. A module whose first path segment is one of these belongs
    // to a dependency, not to the consumer's own source. `kio test`
    // discharges only the consumer's equiv blocks by default — testing
    // the dependencies' internals is the dependency authors' job, not the
    // consumer's — so this is the one extra command-level dependency read
    // the runner needs. The compiler proper stays dependency-agnostic.
    let dep_prefixes = if include_deps {
        std::collections::BTreeSet::new()
    } else {
        crate::cmd::check::dependency_local_names_or_report(cwd)?
    };

    // Walk and parse the source tree once. The equiv census below reads
    // item kinds straight from the lazily-parsed modules (`equiv` bodies
    // stay deferred — see `LazyModule::module`), so a package declaring
    // no in-scope equiv blocks skips the lower / typecheck / discharge-
    // table build it would otherwise do and throw away.
    let walk_start = crate::timing::frontend_enabled().then(std::time::Instant::now);
    let parsed_ws = walk_or_report(cwd)?;
    let walk_elapsed = walk_start
        .map(|start| start.elapsed())
        .unwrap_or(std::time::Duration::ZERO);

    // Fast path: no `<module>` selector and no in-scope equiv block.
    // `kio test` then reduces to the `kio check` pipeline (spec
    // `cli.md` § `kio test` step 1) followed by the step-5 "no equiv
    // blocks found" line. Running the check pipeline surfaces every
    // compile-time error a discharge run would — and, like `kio check`,
    // validates the substituted Prime — while a warm run reuses the
    // package-check cache, so it costs about what `kio check` costs.
    if selectors.is_empty()
        && let Some(root_pkg) = parsed_ws.packages.get(&parsed_ws.root)
    {
        let (in_scope, skipped_dep) = census_equiv_blocks(&root_pkg.lazy_modules, &dep_prefixes);
        if in_scope == 0 {
            // `kio check` additionally forces `skip_ok = false` under a
            // `*.sig.kio` so it can materialize the typed root for its
            // contract-staleness advisory; `kio test` emits no such
            // advisory and discards the typed result, so it honors the
            // package-check cache whenever caches are enabled.
            let skip_ok = crate::cache::policy::caches_enabled();
            if let Err(failure) =
                analyze_parsed_workspace_with::<FullPipeline>(&parsed_ws, skip_ok, walk_elapsed)
            {
                return Err(report_analysis_failure(&failure));
            }
            eprint_dependency_skip_note(skipped_dep);
            println!("{NO_EQUIV_BLOCKS_REPORT}");
            return Ok(());
        }
    }

    // Discharge path: some in-scope equiv block, or an explicit
    // selector. Resolve the root package from the same parsed workspace
    // (no second walk), then diverge from `kio check` at the typer call
    // by collecting elaborations *without* running substitute, so
    // `Item::Equiv` survives for the runner to read.
    let prepared = prepare_resolved_package_from_walked::<FullPipeline>(&parsed_ws)?;
    let package = prepared.package;
    let sources = prepared.sources;
    let checked = match check_normalized_package_collect_errors(package) {
        Ok(checked) => checked,
        Err(errors) => {
            let diag = errors
                .into_iter()
                .next()
                .expect("collected package errors are non-empty");
            return Err(report_located(&sources, diag));
        }
    };
    // Equiv discharge still needs the Lowered package and its elaborations,
    // so validate a separately substituted Prime copy before evaluating them.
    if let Err(diag) =
        crate::pass::typecheck_full::validate_substituted_package(checked.substituted())
    {
        return Err(report_located(&sources, diag));
    }
    let equiv_cache = match checked
        .package()
        .package_file()
        .and_then(|entry| entry.package_file.build.as_ref())
    {
        Some(build) => crate::cache::equiv::resolve_from_cache_field(cwd, &build.cache),
        None => crate::cache::equiv::EquivCache::disabled(),
    };
    // Validate selectors against the resolved package's module set
    // (requires the package to typecheck, so the surface produces
    // friendlier errors when a selector is misspelled — the user
    // sees the available list).
    let filter = if selectors.is_empty() {
        None
    } else {
        let available: Vec<(String, std::path::PathBuf)> = checked
            .package()
            .modules()
            .map(|(path_str, entry)| (path_str.to_owned(), entry.file_path.clone()))
            .collect();
        match crate::cmd::module_selector::resolve_against_modules(&available, selectors) {
            Ok(s) => Some(s),
            Err(crate::cmd::module_selector::UnknownSelector) => return Err(ExitCode::Usage),
        }
    };
    let eval_package = match checked.eval_artifact() {
        Ok(package) => package,
        Err(diag) => return Err(report_located(&sources, diag)),
    };
    discharge_equivs(
        checked.package(),
        &eval_package,
        checked.elaborations(),
        filter.as_ref(),
        &dep_prefixes,
        &equiv_cache,
        &sources,
    )
}

/// Whether `module_path` belongs to a materialized dependency: its
/// leading path segment is one of the dependency local names. An empty
/// `dep_prefixes` (no dependencies, or `--include-deps`) classifies
/// nothing as a dependency module.
fn is_dependency_module(
    module_path: &str,
    dep_prefixes: &std::collections::BTreeSet<String>,
) -> bool {
    let first_segment = module_path.split('/').next().unwrap_or(module_path);
    dep_prefixes.contains(first_segment)
}

/// Census the root package's `equiv` blocks straight from the walk,
/// without lowering or typechecking. Lazy parsing keeps every top-level
/// item's kind (`equiv` bodies stay deferred — see
/// [`crate::pass::parser::LazyModule::module`]), so `Item::Equiv` is
/// visible here for the cost of a scan. Returns the
/// `(in_scope, skipped_dependency)` block counts under the same scoping
/// [`discharge_equivs`] applies: a dependency module (leading path
/// segment in `dep_prefixes`) counts toward the skipped total, every
/// other module toward the in-scope total. `--include-deps` leaves
/// `dep_prefixes` empty, so nothing is a dependency and everything is in
/// scope.
fn census_equiv_blocks(
    lazy_modules: &std::collections::BTreeMap<std::path::PathBuf, crate::pass::parser::LazyModule>,
    dep_prefixes: &std::collections::BTreeSet<String>,
) -> (usize, usize) {
    let mut in_scope = 0;
    let mut skipped_dep = 0;
    for lazy in lazy_modules.values() {
        let module_path = lazy
            .module()
            .path
            .segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join("/");
        let count = lazy
            .module()
            .items
            .iter()
            .filter(|item| matches!(item, Item::Equiv(_, _)))
            .count();
        if is_dependency_module(&module_path, dep_prefixes) {
            skipped_dep += count;
        } else {
            in_scope += count;
        }
    }
    (in_scope, skipped_dep)
}

/// The one-line stderr note reporting how many dependency-module equiv
/// blocks the default scoping held back. Suppressed when nothing was
/// skipped (no dependency, or `--include-deps`, both of which leave the
/// count at zero). Shared by the no-equiv fast path and
/// [`discharge_equivs`] so the two spell the note identically.
fn eprint_dependency_skip_note(skipped_dep_equiv_blocks: usize) {
    if skipped_dep_equiv_blocks == 0 {
        return;
    }
    eprintln!(
        "note: {skipped_dep_equiv_blocks} equiv block{plural} in dependency modules \
         skipped; pass --include-deps to include",
        plural = if skipped_dep_equiv_blocks == 1 {
            ""
        } else {
            "s"
        },
    );
}

/// Walk every module in the package and discharge each `Item::Equiv`.
///
/// `equiv` discharge is pure functional reduction with no mutable
/// state, so per-`equiv` execution is trivially parallelizable. Each
/// equiv's report is buffered into a `String` and emitted in source
/// order at the end of the run, so output is reproducible regardless
/// of how the work-stealing scheduler interleaves the workers.
///
/// The shared module env (typed AST + elaborations) is read-only; per
/// rayon's `Sync` requirement, both sides get borrowed by reference.
/// Returns `Err(ExitCode::TestFailure)` if any equiv split into ≥2 NF
/// groups, `Ok(())` otherwise.
fn discharge_equivs(
    package: &Package<Lowered>,
    eval_package: &crate::normalization::EvalArtifact,
    elaborations: &crate::pass::typecheck_full::Elaborations,
    module_filter: Option<&std::collections::BTreeSet<String>>,
    dep_prefixes: &std::collections::BTreeSet<String>,
    equiv_cache: &crate::cache::equiv::EquivCache,
    sources: &std::collections::HashMap<std::path::PathBuf, String>,
) -> Result<(), ExitCode> {
    // Cache-key summary of exact newtype declaration identities. Reduction
    // resolves members independently in each expression owner's lexical scope.
    let newtype_registry = crate::normalization::newtype_registry_for_package(eval_package);

    // A module is in scope for this run when it passes the explicit
    // `<module>` selector filter (if any). On top of that, equiv blocks in
    // dependency modules are skipped unless `--include-deps` was given —
    // `dep_prefixes` is empty in that case, so nothing is classified as a
    // dependency module and the dep-scoping is a no-op.
    let passes_selector = |path_str: &str| module_filter.is_none_or(|set| set.contains(path_str));

    // Count the equiv blocks that the dep-scoping skips, so the note on
    // stderr can report how many were held back (and only print when the
    // count is positive). A skipped block is one in a dependency module
    // that would otherwise have run (i.e. passes the selector filter).
    let skipped_dep_equiv_blocks: usize = package
        .modules()
        .filter(|(path_str, _)| {
            passes_selector(path_str) && is_dependency_module(path_str, dep_prefixes)
        })
        .map(|(_, entry)| {
            entry
                .module
                .items
                .iter()
                .filter(|item| matches!(item, Item::Equiv(_, _)))
                .count()
        })
        .sum();

    let mut jobs: Vec<EquivJob<'_, '_>> = Vec::new();
    for (path_str, entry) in package.modules().filter(|(path_str, _)| {
        passes_selector(path_str) && !is_dependency_module(path_str, dep_prefixes)
    }) {
        let eval_module = &eval_package
            .module(path_str)
            .expect("evaluator artifact preserves every source module")
            .module;
        // Build the artifact-backed lookup tables once per module. Every
        // job gets a shallow map clone, while definitions remain borrowed
        // from the one validated artifact package.
        let (fn_defs, qualified_fn_defs) =
            crate::normalization::eval_fn_tables(eval_package, eval_module)
                .expect("ModuleEnv::build succeeds for the resolved evaluator artifact");
        for item in &entry.module.items {
            if let Item::Equiv(equiv, _) = item {
                let term_spans = equiv.terms.iter().map(|term| term.meta.span).collect();
                let terms = equiv
                    .terms
                    .iter()
                    .map(|term| {
                        crate::pass::substitute::substitute_expr_for_eval(
                            &term.body,
                            elaborations,
                            path_str,
                            Some(package),
                            Some(&entry.module),
                        )
                    })
                    .collect();
                jobs.push(EquivJob {
                    path_str,
                    file_path: entry.file_path.clone(),
                    module: eval_module,
                    name: equiv.name.clone(),
                    equiv_span: equiv.meta.span,
                    sig: crate::pass::substitute::substitute_signature_for_eval(
                        &equiv.sig,
                        elaborations,
                        path_str,
                        Some(package),
                        Some(&entry.module),
                    ),
                    terms,
                    term_spans,
                    fn_defs: fn_defs.clone(),
                    qualified_fn_defs: qualified_fn_defs.clone(),
                });
            }
        }
    }

    // One note on stderr when dep equiv blocks were held back, so the
    // machine-checked stdout result stays the consumer's own blocks only.
    eprint_dependency_skip_note(skipped_dep_equiv_blocks);

    let total = jobs.len();
    if total == 0 {
        println!("{NO_EQUIV_BLOCKS_REPORT}");
        return Ok(());
    }

    // Happy-path result text goes to stdout; colour it only for an
    // interactive stdout (captured / piped output — every stdout golden
    // — stays plain). Resolved once and shared across jobs.
    let color = crate::diagnostic::ColorMode::for_stdout();
    let render_mode = EquivRenderMode::new(format!("{color:?}"));

    // The package-global cache-key inputs (body-free package structure,
    // primitive environment, newtype registry, render mode, compiler identity)
    // are identical for every `equiv`, so hash them into one digest *before*
    // the parallel fan-out. Per-job substituted terms and referenced function
    // bodies stay outside this shared prelude so unrelated body edits retain
    // their warm entries.
    // Skipped entirely when the cache is off, keeping `--no-cache` runs
    // allocation-light.
    let key_prelude = equiv_cache.is_active().then(|| {
        crate::cache::equiv::EquivCacheKeyPrelude::new(
            &render_mode,
            eval_package.package(),
            eval_package.primitives(),
            &newtype_registry,
        )
    });

    // Each value parameter binds once per equiv block to a fresh
    // `Value::Atom` shared across every `term` body, so equal
    // references on different sides compare equal. Type parameters
    // are erased and need no binding.
    let mut results: Vec<(usize, std::path::PathBuf, EquivDischargeResult)> =
        crate::maybe_par_iter!(jobs)
            .enumerate()
            .map(|(source_index, job)| {
                let module_path = DeclaredModulePath::new(job.path_str);
                if let Some(prelude) = key_prelude.as_ref()
                    && let Some(key) =
                        equiv_cache.key_for(crate::cache::equiv::EquivCacheKeyInput {
                            prelude,
                            module_path: &module_path,
                            name: &job.name,
                            sig: &job.sig,
                            terms: &job.terms,
                            package: eval_package.package(),
                            module: job.module,
                        })
                {
                    if let Some(entry) = equiv_cache.lookup(&key, module_path.as_str(), &job.name) {
                        return (
                            source_index,
                            job.file_path.clone(),
                            EquivDischargeResult::Completed {
                                passed: entry.passed,
                                output: entry.output,
                                metrics: EvalMetricsSnapshot::default(),
                            },
                        );
                    }
                    let result = discharge_one_equiv(job, eval_package, color);
                    if let EquivDischargeResult::Completed { passed, output, .. } = &result {
                        equiv_cache.store(
                            &key,
                            module_path.as_str(),
                            &job.name,
                            &crate::cache::equiv::EquivCacheEntry {
                                passed: *passed,
                                output: output.clone(),
                            },
                        );
                    }
                    return (source_index, job.file_path.clone(), result);
                }
                (
                    source_index,
                    job.file_path.clone(),
                    discharge_one_equiv(job, eval_package, color),
                )
            })
            .collect();

    results.sort_by_key(|(source_index, _, _)| *source_index);

    if crate::timing::eval_enabled() {
        let mut eval_metrics = EvalMetricsSnapshot::default();
        for (_, _, result) in &results {
            eval_metrics.add_assign(result.metrics().clone());
        }
        log_eval_timing_line(total, eval_metrics);
    }

    if let Some((_, file_path, EquivDischargeResult::Totality { span, message, .. })) = results
        .iter()
        .find(|(_, _, result)| matches!(result, EquivDischargeResult::Totality { .. }))
    {
        return Err(report_located(
            sources,
            crate::pass::resolve::LocatedError {
                file_path: file_path.clone(),
                error: crate::error::Error::totality(*span, message.clone()),
            },
        ));
    }

    for (_, _, result) in &results {
        let EquivDischargeResult::Completed { output, .. } = result else {
            unreachable!("totality outcomes return before normal equiv reporting")
        };
        print!("{output}");
    }

    let failed = results
        .iter()
        .filter(|(_, _, result)| {
            matches!(
                result,
                EquivDischargeResult::Completed { passed: false, .. }
            )
        })
        .count();
    if failed > 0 {
        println!(
            "\nresult: {failed}/{total} equiv block{plural} {}",
            crate::diagnostic::style_fail("failed", color),
            plural = if failed == 1 { "" } else { "s" }
        );
        return Err(ExitCode::TestFailure);
    }
    println!(
        "\nresult: {total}/{total} equiv block{plural} {}",
        crate::diagnostic::style_pass("passed", color),
        plural = if total == 1 { "" } else { "s" }
    );
    Ok(())
}

fn discharge_one_equiv(
    job: &EquivJob<'_, '_>,
    artifact: &crate::normalization::EvalArtifact,
    color: crate::diagnostic::ColorMode,
) -> EquivDischargeResult {
    let mut out = String::new();
    let metrics = EvalMetrics::enabled();
    let mut ctx = AuthenticatedEvalCtx::for_module_with_fn_tables(
        artifact,
        job.module,
        job.path_str,
        job.fn_defs.clone(),
        job.qualified_fn_defs.clone(),
    );
    if let Some(metrics) = &metrics {
        ctx = ctx.with_metrics(metrics.clone());
    }
    let mut env = Env::new();
    for p in &job.sig.params {
        if let crate::ast::SignatureParam::Value(vp) = p {
            env.insert(
                vp.name.clone(),
                Value::Atom(format!("__param_{}__", vp.name)),
            );
        }
    }
    let metrics_snapshot = || {
        metrics
            .as_ref()
            .map(|metrics| metrics.snapshot())
            .unwrap_or_default()
    };
    let mut nfs = Vec::with_capacity(job.terms.len());
    for (index, term) in job.terms.iter().enumerate() {
        let nf = eval_authenticated(term, &env, &ctx);
        if let Some(message) = structural_recur_stuck_message(&nf, &ctx) {
            return EquivDischargeResult::Totality {
                span: job.term_spans[index],
                message,
                metrics: metrics_snapshot(),
            };
        }
        nfs.push(nf);
    }
    let groups = match group_by_nf_eq_checked(&nfs, &ctx) {
        Ok(groups) => groups,
        Err(fault) => {
            let message = structural_recur_stuck_message(&fault, &ctx)
                .expect("checked NF equality returns only structural-recursion faults");
            return EquivDischargeResult::Totality {
                // Alpha/eta comparison may execute either operand's
                // closure. A fault has no source-arm provenance, so the
                // enclosing claim is the narrowest truthful location.
                span: job.equiv_span,
                message,
                metrics: metrics_snapshot(),
            };
        }
    };
    if groups.len() == 1 {
        out.push_str(&format!(
            "  {} equiv {} in {}\n",
            crate::diagnostic::style_pass("pass", color),
            crate::diagnostic::style_inline_code(&format!("`{}`", job.name), color),
            job.path_str
        ));
        EquivDischargeResult::Completed {
            passed: true,
            output: out,
            metrics: metrics_snapshot(),
        }
    } else {
        out.push_str(&format!(
            "  {} equiv {} in {}: arms split into {} normal-form groups\n",
            crate::diagnostic::style_fail("fail", color),
            crate::diagnostic::style_inline_code(&format!("`{}`", job.name), color),
            job.path_str,
            groups.len()
        ));
        for (gi, indices) in groups.iter().enumerate() {
            let group_label = char::from(b'A' + gi as u8);
            let arm_list: Vec<String> = indices.iter().map(|i| (i + 1).to_string()).collect();
            out.push_str(&format!(
                "    group {label} (arms {arms}): {nf}\n",
                label = group_label,
                arms = arm_list.join(", "),
                nf = value_to_string(&nfs[indices[0]]),
            ));
        }
        EquivDischargeResult::Completed {
            passed: false,
            output: out,
            metrics: metrics_snapshot(),
        }
    }
}

fn log_eval_timing_line(blocks: usize, eval: EvalMetricsSnapshot) {
    eprintln!(
        "eval-timing: equiv blocks={} root_eval_ms={:.3} root_eval_calls={} \
         expr_visits={} apply_ms={:.3} apply_calls={} closure_apply_ms={:.3} \
         closure_apply_calls={} comptime_ms={:.3} comptime_calls={} \
         type_unfold_ms={:.3} type_unfold_calls={} type_equiv_ms={:.3} \
         type_equiv_calls={} nf_eq_ms={:.3} nf_eq_calls={} eta_contract_ms={:.3} \
         eta_contract_calls={} env_clones={} closure_builds={}",
        blocks,
        duration_ms(eval.root_eval),
        eval.root_eval_calls,
        eval.expr_visits,
        duration_ms(eval.apply),
        eval.apply_calls,
        duration_ms(eval.closure_apply),
        eval.closure_apply_calls,
        duration_ms(eval.reflection),
        eval.reflection_calls,
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
    );
}

fn duration_ms(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Group `nfs` indices by checked normal-form equality. Returns one
/// `Vec<usize>` per equivalence class, in source-position order of the first
/// representative.
fn group_by_nf_eq_checked(nfs: &[Value], ctx: &EvalCtx<'_>) -> Result<Vec<Vec<usize>>, Value> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    'outer: for (i, v) in nfs.iter().enumerate() {
        for g in groups.iter_mut() {
            let rep = &nfs[g[0]];
            let equal = nf_eq_checked(rep, v, ctx)?;
            if equal {
                g.push(i);
                continue 'outer;
            }
        }
        groups.push(vec![i]);
    }
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `run` rejects unknown flags before doing any disk work.
    /// Positional selectors are accepted (per `specs/cli.md`
    /// § `kio test`); the unknown-selector case routes through
    /// `resolve_selectors` after the package typechecks.
    #[test]
    fn run_with_unknown_flag_returns_usage_exit() {
        let ec = run(&["--bogus".to_owned()], false);
        assert_eq!(ec, ExitCode::Usage);
    }

    /// In Kio'-only mode, `kio-prime` doesn't expose `test` — the
    /// `prime_only` branch should return `Usage` even with no args.
    #[test]
    fn run_in_prime_only_mode_returns_usage_exit() {
        assert_eq!(run(&[], true), ExitCode::Usage);
    }

    /// Help flags route through `Success` without touching disk.
    #[test]
    fn run_with_help_returns_success() {
        assert_eq!(run(&["-h".to_owned()], false), ExitCode::Success);
        assert_eq!(run(&["--help".to_owned()], false), ExitCode::Success);
    }

    /// A module whose leading path segment is a dependency local name is
    /// a dependency module; the consumer's own modules (and modules whose
    /// non-leading segment happens to match) are not. An empty prefix set
    /// (the `--include-deps` / no-dependency case) classifies nothing.
    #[test]
    fn dependency_module_classified_by_leading_segment() {
        let mut deps = std::collections::BTreeSet::new();
        deps.insert("elab".to_owned());
        assert!(is_dependency_module("elab", &deps));
        assert!(is_dependency_module("elab/match", &deps));
        assert!(is_dependency_module("elab/elab/main", &deps));
        assert!(!is_dependency_module("host/m", &deps));
        // Only the *leading* segment counts: a consumer module whose
        // non-leading segment is named like a dependency is not a dep
        // module.
        assert!(!is_dependency_module("host/elab", &deps));

        let empty = std::collections::BTreeSet::new();
        assert!(!is_dependency_module("elab/match", &empty));
    }

    /// The no-equiv fast path hinges on `census_equiv_blocks`: it counts
    /// `Item::Equiv` straight from the lazily-parsed modules (bodies stay
    /// deferred) and scopes dependency modules into the skipped tally. A
    /// miscount would either skip a real discharge run or force a
    /// needless fall-back to the slow path, so pin both directions.
    #[test]
    fn census_counts_in_scope_and_skips_dependency_equivs() {
        let lazy = |src: &str| {
            crate::pass::parser::parse_module_file_lazy(src)
                .expect("module parses")
                .lazy
                .expect("lazy handle present for a regular module")
        };
        let mut modules = std::collections::BTreeMap::new();
        modules.insert(
            std::path::PathBuf::from("/pkg/main.kio"),
            lazy("module main;\n\nequiv a() { (); () }\n"),
        );
        modules.insert(
            std::path::PathBuf::from("/pkg/util.kio"),
            lazy("module util;\n\npub fn f() -> . { () }\n"),
        );
        modules.insert(
            std::path::PathBuf::from("/pkg/elab/law.kio"),
            lazy("module elab/law;\n\nequiv b() { (); () }\n"),
        );

        // Default scoping: `main` is the one in-scope block, `util` has
        // none, and the `elab/law` dependency block is skipped.
        let mut deps = std::collections::BTreeSet::new();
        deps.insert("elab".to_owned());
        assert_eq!(census_equiv_blocks(&modules, &deps), (1, 1));

        // `--include-deps` (empty prefix set): the dependency block is
        // now in scope and nothing is skipped.
        let empty = std::collections::BTreeSet::new();
        assert_eq!(census_equiv_blocks(&modules, &empty), (2, 0));
    }
}
