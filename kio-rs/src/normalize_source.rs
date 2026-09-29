//! In-memory source normalization for embedding hosts.
//!
//! [`normalize_source`] takes a package's source text — a package file plus one
//! regular module — runs the parse → lower → resolve → typecheck → elaborate →
//! normalize pipeline, and renders the residual normal form of the package's
//! `main` binding. Every step is the same primitive the `kio` driver uses,
//! assembled without source-file traversal or external commands: the two files
//! and intermediate artifacts are supplied in memory rather than walked off
//! disk. The shared timing support still reads `KIO_DEBUG_TIMING` /
//! `KIO_DEBUG_EVAL_TIMING`; when enabled, evaluation samples the monotonic clock
//! and writes metrics to standard error.
//!
//! The pieces it chains — the same ones `kio test`'s `discharge_equivs`
//! and the REPL's `reduce_lowered` use, just sourced in memory:
//!
//! - [`crate::pass::parser::parse`] for the regular module and
//!   [`crate::pass::parser::parse_package_file`] for the package file;
//! - [`crate::pass::full::FullPipeline::lower_package`] to run the
//!   front-end lowering (op-fold → desugar → label-elab) on both;
//! - [`crate::pass::resolve::Package::build`] with an empty root path to
//!   resolve the lowered modules into a package (that overload is fed
//!   already-lowered modules and walks no directory);
//! - [`crate::pass::typecheck_full::check_package_preserving_normalization`]
//!   for the checked, normalization-bearing Lowered package;
//! - [`crate::pass::typecheck_full::validate_substituted_package`] for
//!   standalone validation of the elaborated Prime package;
//! - the checked package's evaluator-artifact exit to consume its
//!   boundary-local elaborations into an independently resolvable Kio'-shaped
//!   package;
//! - [`crate::pass::typecheck_core::ModuleEnv::build`] for the
//!   artifact's per-module fn lookup tables; and
//! - [`crate::normalization`]'s artifact-backed evaluation / `value_to_string`
//!   path.
//!
//! The entry point lives outside the `cli` and `repl` features on
//! purpose. A host embedding the normalization core can call it directly
//! without linking the `cmd` / `cache` driver layer. It requires only the
//! `surface` feature for the
//! full-Kio front end and `normalization` interpreter.
//!
//! ## Panic behavior at the host boundary
//!
//! Lex / parse / resolve / typecheck failures are user input errors and
//! come back as the `Err` arm (a [`crate::error::Error`] carrying span +
//! message). The interpreter's internal-contract violations stay panics
//! (`unreachable!` / `expect` inside `normalization`) — they are bugs, not
//! input errors. On `wasm32-unknown-unknown` a Rust panic becomes an
//! opaque `unreachable` trap unless a panic hook is installed. This
//! target-neutral library does not select a host hook, so a wasm host that
//! exposes this entry point installs one before calling it if internal bugs
//! must surface as readable host exceptions.

use crate::ast::Item;
use crate::error::Error;
use crate::normalization::{
    AuthenticatedEvalCtx, Env, EvalMetrics, EvalMetricsSnapshot, eval_authenticated,
    structural_recur_stuck_message, value_to_string,
};
use crate::pass::full::FullPipeline;
use crate::pass::resolve::{Package, PackageFileEntry};
use crate::pass::typecheck_full::check_package_preserving_normalization;
use crate::pipeline::Pipeline;
use std::path::{Path, PathBuf};

/// Parse, typecheck, and normalize a Kio package in memory, returning the
/// rendered residual normal form of its `main` binding.
///
/// `package_src` is the package's `<pkg>.pkg.kio` contract, headed by
/// `package <name>;`. `module_src` is one regular module (a
/// `module <path>;` header plus `fn` definitions) that defines
/// `fn main`. Both are fed in memory; nothing is read from or written
/// to disk.
///
/// Returns `Err` for any user-facing failure — a lex / parse error in
/// either file, a resolution or type error, a compile-time Totality failure,
/// or a missing `main` binding with a body — each carrying a span and message.
/// Internal-contract violations inside the interpreter panic rather than
/// returning `Err`; see the module-level note on the wasm panic hook.
pub fn normalize_source(package_src: &str, module_src: &str) -> Result<String, Error> {
    // Parse the two files (Surface phase). `parse_package_file`'s
    // `stem = None` takes the package name from the file's
    // `package <name>;` header rather than checking it against
    // a filename — there is no filename here.
    let surface_module = crate::pass::parser::parse(module_src)?;
    let surface_package_file = crate::pass::parser::parse_package_file(package_src, None)?;
    let package_name = surface_package_file.name.clone();

    // Front-end lowering: op-fold → desugar → label-elab, the same
    // `FullPipeline` the `kio` binary runs. Lowering maps a
    // front-end error to a `LocatedError`; surface its inner `Error`.
    // The synthetic file path must match the module's declared path:
    // `Package::build` checks that a module declared `module a/b;` lives
    // at file `a/b.kio` (the declared name is the file path relative to
    // the package root, `.kio` stripped, `/`-separated). Derive the path
    // from the parsed module so the two always agree — no fs access; the
    // path is only an identity key.
    let module_file = PathBuf::from(format!("{}.kio", surface_module.path.segments.join("/")));
    let (lowered_modules, lowered_package_file) = FullPipeline::lower_package(
        vec![(module_file, surface_module)],
        Some(surface_package_file),
    )
    .map_err(|located| located.error)?;

    // Wrap the lowered package file in the package-level entry
    // `Package::build` expects, then resolve. The empty root path is
    // fine: this overload is fed already-lowered modules and walks no
    // directory.
    let package_entry = lowered_package_file.map(|package_file| PackageFileEntry {
        file_path: PathBuf::from("<normalize_source>.pkg.kio"),
        package_name: package_name.clone(),
        package_file,
    });
    let package = Package::build(Path::new(""), lowered_modules, package_entry)
        .map_err(|located| located.error)?;

    // Typecheck and collect the boundary-local elaboration table. Validate
    // the resulting runtime Prime package before constructing the separate
    // Kio'-shaped evaluator artifact from the same elaborations.
    let checked =
        check_package_preserving_normalization(&package).map_err(|located| located.error)?;
    crate::pass::typecheck_full::validate_substituted_package(checked.substituted())
        .map_err(|located| located.error)?;
    let eval_package = checked.eval_artifact().map_err(|located| located.error)?;

    // The single regular artifact module — its body holds `main`.
    let (module_path, module) = eval_package
        .modules()
        .next()
        .map(|(path, entry)| (path.to_owned(), &entry.module))
        .expect("the Kio' module is in the resolved evaluator artifact");

    // Find `main`'s `fn` definition and reduce its body. `normalize_source`
    // evaluates a module entry point, so an absent `main` function is
    // the user error.
    let main_def = module
        .items
        .iter()
        .find_map(|item| match item {
            Item::FnDef(d) if d.name.as_str() == "main" => Some(d),
            _ => None,
        })
        .ok_or_else(|| {
            Error::name_res(
                module.path.span,
                "the module defines no `main` binding".to_owned(),
            )
        })?;

    let metrics = EvalMetrics::enabled();
    let mut ctx = AuthenticatedEvalCtx::for_module(&eval_package, module, &module_path)
        .expect("ModuleEnv::build succeeds post-typecheck");
    if let Some(metrics) = &metrics {
        ctx = ctx.with_metrics(metrics.clone());
    }
    let value = eval_authenticated(&main_def.body, &Env::new(), &ctx);
    if let Some(metrics) = metrics {
        log_eval_timing_line("eval-source", metrics.snapshot());
    }
    if let Some(message) = structural_recur_stuck_message(&value, &ctx) {
        return Err(Error::totality(main_def.body.span(), message));
    }
    Ok(value_to_string(&value))
}

fn log_eval_timing_line(label: &str, eval: EvalMetricsSnapshot) {
    eprintln!(
        "eval-timing: {} root_eval_ms={:.3} root_eval_calls={} expr_visits={} \
         apply_ms={:.3} apply_calls={} closure_apply_ms={:.3} closure_apply_calls={} \
         comptime_ms={:.3} comptime_calls={} type_unfold_ms={:.3} \
         type_unfold_calls={} type_equiv_ms={:.3} \
         type_equiv_calls={} nf_eq_ms={:.3} nf_eq_calls={} eta_contract_ms={:.3} \
         eta_contract_calls={} env_clones={} closure_builds={}",
        label,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_main_to_residual_nf() {
        // A literal-alias-shaped two-file package, fed in memory: a
        // package file admitting the root module, then a regular module
        // whose `main` returns an aliased value.
        // `greeting` is a literal alias expanded inline at the
        // Surface → Desugared boundary, so `main` reduces to the string
        // literal "hi" — a residual normal form the renderer prints as a
        // quoted string. The whole pipeline runs without the filesystem.
        let package = "\
package evalsrc;

bridge {
  main;
}
";
        let module = "\
module main;

host type I32 role(i32);
host type Str role(str);

literal greeting = \"hi\";

fn main() -> Str { greeting }
";
        let rendered = normalize_source(package, module).expect("normalize_source should succeed");
        assert_eq!(rendered, "\"hi\"");
    }

    #[test]
    fn validates_elaborated_pure_main_before_evaluation() {
        let package = "\
package evalsrc;

bridge {
  main;
}
";
        let module = "\
module main;

import __comptime__;

host fn effect() -> .;

pure fn effect_call_impl(ct: __Comptime__, effect_term: __Checked_term__) -> __Checked_term__ {
  let unit = __type_unit__(ct);
  let one = __type_arity_succ__(ct, __type_arity_zero__(ct));
  let effect_type = __type_arrow__(ct, unit, one, unit);
  __term_call__(ct, effect_type, effect_term, __term_unit__(ct))
}

elab effect_call : . -> . { captures effect; impl effect_call_impl; };

pure fn main() -> . { effect_call!() }
";

        let error = normalize_source(package, module).expect_err("expected purity error");
        match error {
            Error::Type(diagnostic) => assert!(
                diagnostic
                    .message
                    .contains("pure function cannot reference host fn `effect`"),
                "got: {}",
                diagnostic.message
            ),
            other => panic!("expected type error, got {other:?}"),
        }
    }

    fn same_module_elaborator_sources() -> (&'static str, &'static str) {
        let package = "\
package evalsrc;

bridge {
  main;
}
";
        let module = "\
module main;

import __comptime__;

pure fn make_rep_impl(ct: __Comptime__, _rep_type: __Type__, ctor: __Checked_term__, _source: __Type__) -> __Checked_term__ {
  __term_call__(ct, __term_type__(ct, ctor), ctor, __term_unit__(ct))
}

newtype Rep : . { constructor mk_rep; projector un_rep; };

elab make_rep : [T] . -> Rep { captures (Rep, Rep.mk_rep); impl make_rep_impl; };

pure fn main() -> . {
  let rep = make_rep!(., ());
  rep.>Rep.un_rep
}
";
        (package, module)
    }

    #[test]
    fn same_module_capture_replays_without_a_self_import() {
        let (package, module) = same_module_elaborator_sources();

        let rendered =
            normalize_source(package, module).expect("same-module captured members should replay");
        assert_eq!(rendered, "()");
    }

    #[cfg(feature = "repl-core")]
    #[test]
    fn same_module_elaborator_ground_result_matches_in_memory_repl() {
        let (package, module) = same_module_elaborator_sources();
        let embedded =
            normalize_source(package, module).expect("same-module captured members should replay");

        let root = PathBuf::from("eval-source-repl-parity");
        let mut files = std::collections::BTreeMap::new();
        files.insert(root.join("evalsrc.pkg.kio"), package.to_owned());
        files.insert(root.join("main.kio"), module.to_owned());
        let mut session = crate::repl_core::session::Session::new_in_memory(root, files);

        let loaded = crate::repl_core::commands::Command::Load("main".to_owned())
            .run(&mut session, crate::repl_core::highlight::Palette::plain());
        assert!(loaded.output.contains("loaded main"), "{}", loaded.output);

        let queried = crate::repl_core::expr_query::query_expr(&mut session, "main()")
            .unwrap_or_else(|_| panic!("in-memory REPL should evaluate main()"));
        assert_eq!(queried.rendered_value, embedded);
    }

    #[test]
    fn structural_recursion_fault_is_a_totality_error_not_a_residual() {
        let package = "\
package evalsrc;

bridge {
  main;
}
";
        let module = "\
module main;

import __comptime__;

pure fn run(ct: __Comptime__, k: . -> .) -> . {
  __structural_recur__(
    , ct
    , __Type__
    , .
    , .
    , __type_unit__(ct)
    , ()
    , .(
        , _recur: (__Type__ & .) -> .
        , _fuel: __Type__
        , _input: .
        ) -> . { k(()) }
    )
}

pure fn main(ct: __Comptime__) -> . {
  let ignored = run(ct, .(_outer: .) -> . {
    run(ct, .(_inner: .) -> . { () })
  });
  ()
}
";

        let error = normalize_source(package, module)
            .expect_err("executed structural recursion fault must fail normalize_source");
        let Error::Totality(diagnostic) = error else {
            panic!("expected Totality error, got {error:?}");
        };
        assert!(
            diagnostic.message.contains("same helper origin")
                && diagnostic.message.contains("root=1, current=1, next=1"),
            "{}",
            diagnostic.message
        );
    }
}
