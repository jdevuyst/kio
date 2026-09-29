//! `Pipeline` impl for the kio-prime binary.
//!
//! Wires [`prime::lower`](crate::prime::lower) (the rejecting Surface → Prime
//! walk) and [`prime::typer::check_package`](crate::prime::typer) (standalone
//! Kio' validation, completion baking, and canonicalization) into the
//! [`crate::pipeline::Pipeline`] abstraction.
//! [`crate::cmd::check::compile_package`] dispatches through this impl
//! when the kio-prime binary runs.

use std::path::PathBuf;

use crate::ast::{Module, PackageFile, Prime, Surface};
use crate::pass::resolve::{LocatedError, ModuleEntry, Package, PackageFileEntry};
use crate::pipeline::Pipeline;
use crate::prime::{lower, typer};

/// Pipeline for the kio-prime binary. `LoweredPhase = Prime`:
/// `prime::lower` walks Surface directly to Prime, rejecting every
/// surface-only variant; `prime::typer::check_package` then
/// validates the Prime AST and consumes a narrow [`typer::PrimeElaborations`]
/// table that materializes Kio'-admitted call and lambda completions.
pub struct PrimePipeline;

pub struct PrimeLoweringContext {
    lowered_package_file: Option<PackageFile<Prime>>,
}

impl Pipeline for PrimePipeline {
    type LoweredPhase = Prime;
    type LoweringContext = PrimeLoweringContext;

    const CACHE_TAG: &'static str = "prime-alpha.";

    /// Lower a parsed Surface package directly to Prime via
    /// [`prime::lower::lower_module`] / [`prime::lower::lower_package_file`].
    /// Each rejection of a surface-only form is wrapped as a
    /// `LocatedError` pointing at the offending file.
    fn lower_package_named(
        modules: Vec<(PathBuf, Module<Surface>)>,
        package_file: Option<PackageFile<Surface>>,
        _package_name: Option<&str>,
    ) -> Result<(Vec<(PathBuf, Module<Prime>)>, Option<PackageFile<Prime>>), LocatedError> {
        let mut lowered: Vec<(PathBuf, Module<Prime>)> = Vec::with_capacity(modules.len());
        for (path, module) in modules {
            let m = lower::lower_module(module).map_err(|error| LocatedError {
                file_path: path.clone(),
                error,
            })?;
            lowered.push((path, m));
        }

        let lowered_package_file = match package_file {
            None => None,
            Some(e) => {
                let lowered_e = lower::lower_package_file(e).map_err(|error| LocatedError {
                    file_path: PathBuf::from("<package-file>"),
                    error,
                })?;
                Some(lowered_e)
            }
        };

        Ok((lowered, lowered_package_file))
    }

    fn lowering_context_named(
        _modules: &[(PathBuf, Module<Surface>)],
        package_file: Option<&PackageFile<Surface>>,
        _package_name: Option<&str>,
    ) -> Result<Self::LoweringContext, LocatedError> {
        let lowered_package_file = match package_file {
            None => None,
            Some(e) => {
                Some(
                    lower::lower_package_file(e.clone()).map_err(|error| LocatedError {
                        file_path: PathBuf::from("<package-file>"),
                        error,
                    })?,
                )
            }
        };
        Ok(PrimeLoweringContext {
            lowered_package_file,
        })
    }

    fn lower_module_with_context(
        _context: &Self::LoweringContext,
        file_path: PathBuf,
        module: Module<Surface>,
    ) -> Result<Module<Self::LoweredPhase>, LocatedError> {
        lower::lower_module(module).map_err(|error| LocatedError { file_path, error })
    }

    fn lowered_package_file_from_context(
        context: &Self::LoweringContext,
    ) -> Option<PackageFile<Self::LoweredPhase>> {
        context.lowered_package_file.clone()
    }

    /// Run the standalone Kio'-only typer on a `Package<Prime>`
    /// via [`prime::typer::check_package`]. The typer drives the
    /// shared scoped per-module checker from `typecheck_core`, with
    /// the recursive `synth_expr` / `check_value_against` calls
    /// landing in `PrimeTyper`'s `Typer<Prime>` impl.
    fn typecheck(package: &Package<Prime>) -> Result<Package<Prime>, LocatedError> {
        typer::check_package(package)
    }

    fn typecheck_module(
        module_path: &str,
        entry: &ModuleEntry<Prime>,
        package: &Package<Prime>,
    ) -> Result<Module<Prime>, Vec<LocatedError>> {
        Self::typecheck_module_with_interner(
            module_path,
            entry,
            package,
            std::sync::Arc::new(crate::pass::typecheck_core::TypeInterner::default()),
        )
    }

    fn typecheck_module_with_interner(
        module_path: &str,
        _entry: &ModuleEntry<Prime>,
        package: &Package<Prime>,
        type_interner: std::sync::Arc<crate::pass::typecheck_core::TypeInterner<Prime>>,
    ) -> Result<Module<Prime>, Vec<LocatedError>> {
        let normalized = crate::pass::alpha_normalize::normalize_package(package);
        let entry = normalized
            .package()
            .module(module_path)
            .expect("normalized package retains the selected module");
        let typecheck_scope =
            crate::pass::typecheck_core::PackageTypecheckScope::fresh_for_normalized_package(
                &normalized,
                type_interner,
            );
        Self::typecheck_module_with_typecheck_scope(
            module_path,
            entry,
            normalized.package(),
            typecheck_scope,
        )
    }

    fn typecheck_module_with_typecheck_scope(
        module_path: &str,
        entry: &ModuleEntry<Prime>,
        package: &Package<Prime>,
        typecheck_scope: std::sync::Arc<crate::pass::typecheck_core::PackageTypecheckScope<Prime>>,
    ) -> Result<Module<Prime>, Vec<LocatedError>> {
        let mut elaborations = typer::PrimeElaborations::default();
        if let Err(errors) =
            crate::pass::typecheck_core::check_module_in_package_with_typecheck_scope_collect_errors(
                &entry.module,
                package.package_file().map(|e| &e.package_file),
                package.package_file().map(|e| e.package_name.as_str()),
                Some(package),
                &mut elaborations,
                typecheck_scope.clone(),
            )
        {
            let errors = errors
                .into_iter()
                .map(|error| LocatedError::new(entry.file_path.clone(), error))
                .collect::<Vec<_>>();
            return Err(errors);
        }
        let mut typed = entry.module.clone();
        typer::bake_prime_elaborations_in_module(&mut typed, &elaborations, module_path);
        crate::prime::canonical::canonicalize_module(&mut typed);
        crate::pass::alpha_normalize::erase_module_occurrences(&mut typed);
        Ok(typed)
    }

    fn typecheck_package_file(
        package: &Package<Prime>,
    ) -> Result<Option<PackageFileEntry<Prime>>, LocatedError> {
        let Some(entry) = package.package_file() else {
            return Ok(None);
        };
        // The package file carries only the phase-independent `bridge`
        // glob list; its host-contract surface is validated in the
        // resolver (`Package::validate_bridge_contract`), and it carries
        // no item bodies to typecheck or bake elaborations into.
        Ok(Some(PackageFileEntry::<Prime> {
            file_path: entry.file_path.clone(),
            package_name: entry.package_name.clone(),
            package_file: entry.package_file.clone(),
        }))
    }

    fn typecheck_aux_module(
        module: &crate::ast::Module<Prime>,
        package: &Package<Prime>,
        package_name: Option<&str>,
    ) -> Result<crate::ast::Module<Prime>, LocatedError> {
        crate::pipeline::assert_aux_module_outside_package(module, package);
        let package = crate::pass::alpha_normalize::normalize_package(package);
        let module = crate::pass::alpha_normalize::normalize_module(module);
        let typecheck_scope =
            crate::pass::typecheck_core::PackageTypecheckScope::fresh_for_normalized_module(
                &module,
                &package,
                std::sync::Arc::new(crate::pass::typecheck_core::TypeInterner::default()),
            );
        // The carrier is already Prime (Kio' in, Kio' out). Typing it against
        // the consumer package records call/lambda completions, which the
        // per-module baker stamps in — the prime pipeline's narrow counterpart
        // of `substitute`. See `FullPipeline`'s implementation.
        let mut elaborations = typer::PrimeElaborations::default();
        let module_path =
            crate::pass::typecheck_core::module_path_key(module.module(), package_name);
        if let Err(errors) =
            crate::pass::typecheck_core::check_module_in_package_with_typecheck_scope_collect_errors(
                module.module(),
                package.package_file().map(|e| &e.package_file),
                package_name,
                Some(package.package()),
                &mut elaborations,
                typecheck_scope.clone(),
            )
        {
            let error = LocatedError::new(
                std::path::PathBuf::new(),
                errors
                    .into_iter()
                    .next()
                    .expect("collected auxiliary-module errors are non-empty"),
            );
            return Err(error);
        }
        let mut typed = module.module().clone();
        typer::bake_prime_elaborations_in_module(&mut typed, &elaborations, &module_path);
        crate::prime::canonical::canonicalize_module(&mut typed);
        crate::pass::alpha_normalize::erase_module_occurrences(&mut typed);
        Ok(typed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::parser::parse;
    use std::path::PathBuf;

    /// Confirms `PrimePipeline::lower_package` accepts a Kio' input
    /// and emits Prime. The typer half is exercised by
    /// `prime_pipeline_typecheck_succeeds_for_kio_prime` below.
    #[test]
    fn prime_pipeline_lowers_a_kio_prime_module() {
        // Per `specs/package.md` § Module-name rules, the declared
        // segments equal the file's path relative to the package root.
        let source = r#"module x/main;

fn id[A](x: A) -> A { x }
"#;
        let module = parse(source).expect("parse");
        let modules = vec![(PathBuf::from("x/main.kio"), module)];

        let (lowered_modules, lowered_package_file) =
            PrimePipeline::lower_package(modules, None).expect("lower_package");
        assert_eq!(lowered_modules.len(), 1);
        assert!(lowered_package_file.is_none());
        assert_eq!(lowered_modules[0].1.path.segments, vec!["x", "main"]);
    }

    /// Confirms `PrimePipeline::lower_package` rejects surface-only
    /// forms — exercised here via a tuple literal that's accepted
    /// by the parser but not by Kio'. The error wraps the underlying
    /// `prime::lower` rejection.
    #[test]
    fn prime_pipeline_rejects_surface_only_form() {
        let source = r#"module x/main;

fn f() -> . { (.t, .f) }
"#;
        let module = parse(source).expect("parse");
        let modules = vec![(PathBuf::from("x/main.kio"), module)];

        let err = PrimePipeline::lower_package(modules, None).expect_err("rejects tuple");
        assert_eq!(err.file_path, PathBuf::from("x/main.kio"));
        let crate::error::Error::Parse(crate::error::Diagnostic { message, .. }) = err.error else {
            panic!("expected Parse error");
        };
        assert!(message.contains("tuple literal"), "names form: {message}");
    }

    /// Confirms `PrimePipeline::typecheck` calls through to
    /// `prime::typer::check_package` and returns a typed
    /// `Package<Prime>` for a well-typed Kio' program. Exercises
    /// the full kio-prime front-end through the trait.
    #[test]
    fn prime_pipeline_typecheck_succeeds_for_kio_prime() {
        let source = r#"module x/main;

fn id[A](x: A) -> A { x }
"#;
        let module = parse(source).expect("parse");
        let modules = vec![(PathBuf::from("x/main.kio"), module)];

        let (lowered_modules, _) =
            PrimePipeline::lower_package(modules, None).expect("lower_package");
        let package = Package::<Prime>::build(std::path::Path::new(""), lowered_modules, None)
            .expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package.check_no_value_cycles().expect("cycle check");
        package
            .check_in_body_resolution()
            .expect("in-body resolution");

        let prime = PrimePipeline::typecheck(&package).expect("typecheck");
        assert!(prime.module("x/main").is_some());
    }

    #[test]
    #[should_panic(expected = "must not be part of the package module set")]
    fn prime_auxiliary_module_rejects_a_package_path_collision() {
        let source = "module collision; fn id[A](value: A) -> A { value }";
        let module = parse(source).expect("parse");
        let (lowered_modules, _) =
            PrimePipeline::lower_package(vec![(PathBuf::from("collision.kio"), module)], None)
                .expect("lower package");
        let package = Package::<Prime>::build(std::path::Path::new(""), lowered_modules, None)
            .expect("build package shell");
        let carrier = package
            .module("collision")
            .expect("collision module")
            .module
            .clone();

        let _ = PrimePipeline::typecheck_aux_module(&carrier, &package, None);
    }
}
