//! Kio-only modules — the kio binary's front-end pipeline.
//!
//! Hosts the [`FullPipeline`] implementation that folds operators, lowers
//! `Surface → Desugared → Lowered`, checks the Lowered package, substitutes its
//! recorded completions into Prime, and runs standalone Prime validation. The
//! kio binary's `check.rs` driver dispatches through this implementation.
//!
//! This module and its dependents (`desugar`, `elaborator_registry`,
//! `label_elab`, `op_fold`, `substitute`, `typecheck_full`) are gated
//! behind `#[cfg(feature = "surface")]`, so they don't compile into the
//! kio-prime binary's slice. The Kio'-only mirror lives under
//! [`crate::prime`] (gated behind `#[cfg(feature = "prime")]`).

use std::path::PathBuf;

use crate::ast::{Desugared, Lowered, Module, PackageFile, Prime, Surface};
use crate::pass::desugar::{
    PackageLiteralAliases, collect_package_literal_aliases, desugar_module_with_imports,
    desugar_package_file, desugar_package_with_imports,
};
use crate::pass::label_elab;
use crate::pass::op_fold;
use crate::pass::resolve::{LocatedError, ModuleEntry, Package, PackageFileEntry};
use crate::pass::typecheck_full;
use crate::pipeline::Pipeline;

/// The kio binary's front-end pipeline: full Kio with the
/// elaboration-aware typer.
pub struct FullPipeline;

pub struct FullLoweringContext {
    package_ops: op_fold::PackageOpScope,
    block_scope: crate::pass::surface_registry::PackageBlockScope,
    literal_aliases: PackageLiteralAliases,
    label_tables: label_elab::PackageLabelTables,
    lowered_package_file: Option<PackageFile<Lowered>>,
}

#[cfg(feature = "lsp")]
pub(crate) struct LspSourceDeclarations {
    pub callables: Vec<op_fold::ResolvedCallableDeclaration>,
    pub blocks: Vec<crate::pass::surface_registry::ResolvedBlockDeclaration>,
}

#[cfg(feature = "lsp")]
impl FullLoweringContext {
    pub(crate) fn try_lsp_source_declarations(
        &self,
        only_source_module: Option<&str>,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Option<LspSourceDeclarations> {
        Some(LspSourceDeclarations {
            callables: self
                .package_ops
                .try_resolved_callable_declarations(only_source_module, &mut is_cancelled)?,
            blocks: self
                .block_scope
                .try_resolved_declarations(only_source_module, is_cancelled)?,
        })
    }
}

#[allow(clippy::type_complexity)]
fn lower_projected_package_named_observed<T>(
    modules: Vec<(PathBuf, Module<Surface>)>,
    package_file: Option<PackageFile<Surface>>,
    package_name: Option<&str>,
    observe: impl FnOnce(
        &op_fold::PackageOpScope,
        &crate::pass::surface_registry::PackageBlockScope,
        &[(PathBuf, Module<Surface>)],
    ) -> Option<T>,
) -> Result<
    Option<(
        Vec<(PathBuf, Module<Lowered>)>,
        Option<PackageFile<Lowered>>,
        T,
    )>,
    LocatedError,
> {
    let block_scope = crate::pass::surface_registry::PackageBlockScope::from_modules(&modules)?;
    // Operator folding and any observer projection share this one validated
    // package scope. Rebuilding it after folding would repeat declaration
    // validation and could let editor-only facts drift from lowering.
    let (mut folded, package_ops) =
        op_fold::fold_package_named_with_scope(modules, package_name)
            .map_err(|(file_path, error)| LocatedError { file_path, error })?;
    let Some(observation) = observe(&package_ops, &block_scope, &folded) else {
        return Ok(None);
    };
    let folded_package_file = match package_file {
        None => None,
        Some(e) => Some(
            op_fold::fold_package_file_in_package(e, &package_ops).map_err(|error| {
                LocatedError {
                    file_path: PathBuf::from("<package-file>"),
                    error,
                }
            })?,
        ),
    };
    let package_literal_aliases = collect_package_literal_aliases(&folded, package_name);
    for (file_path, module) in &mut folded {
        crate::pass::block_projection::project_module(module, &block_scope).map_err(|error| {
            LocatedError {
                file_path: file_path.clone(),
                error,
            }
        })?;
    }
    let desugared = desugar_package_with_imports(folded, &package_literal_aliases)?;

    let desugared_package_file = match folded_package_file {
        None => None,
        Some(e) => {
            let lowered = desugar_package_file(e).map_err(|error| LocatedError {
                file_path: PathBuf::from("<package-file>"),
                error,
            })?;
            Some(lowered)
        }
    };

    let (lowered, lowered_package_file) =
        label_elab::elaborate_package(desugared, desugared_package_file)?;
    Ok(Some((lowered, lowered_package_file, observation)))
}

#[cfg(feature = "lsp")]
impl FullPipeline {
    #[allow(clippy::type_complexity)]
    pub(crate) fn lower_projected_package_named_for_lsp(
        modules: Vec<(PathBuf, Module<Surface>)>,
        package_file: Option<PackageFile<Surface>>,
        package_name: Option<&str>,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<
        Option<(
            Vec<(PathBuf, Module<Lowered>)>,
            Option<PackageFile<Lowered>>,
            LspSourceDeclarations,
        )>,
        LocatedError,
    > {
        lower_projected_package_named_observed(
            modules,
            package_file,
            package_name,
            |package_ops, block_scope, _| {
                Some(LspSourceDeclarations {
                    callables: package_ops
                        .try_resolved_callable_declarations(None, &mut is_cancelled)?,
                    blocks: block_scope.try_resolved_declarations(None, is_cancelled)?,
                })
            },
        )
    }
}

pub(crate) struct FullModuleTypecheck {
    pub module: Module<Prime>,
    #[cfg(feature = "lsp")]
    pub elaborations: typecheck_full::Elaborations,
}

impl Pipeline for FullPipeline {
    type LoweredPhase = Lowered;
    type LoweringContext = FullLoweringContext;

    const CACHE_TAG: &'static str = "prime-validated-alpha.";

    fn validate_source_package(
        package_root: &std::path::Path,
        modules: &[(PathBuf, Module<Surface>)],
    ) -> Result<(), LocatedError> {
        crate::pass::surface_registry::validate_package(package_root, modules)
    }

    /// Lower a parsed Surface package to the Lowered phase that
    /// `typecheck_full::check_package` expects:
    ///
    /// 1. `op_fold` rewrites registered operator chains into ordinary calls.
    /// 2. `desugar_module` per regular module and `desugar_package_file` for
    ///    the optional package file perform the Surface → Desugared rewrite.
    /// 3. `label_elab::elaborate_package` over the desugared batch performs
    ///    Desugared → Lowered. Builds per-module label tables, then
    ///    walks each module rewriting `Item::Labels` to minted
    ///    `Item::Newtype` declarations (or one `Item::TypeRecGroup` for a
    ///    recursive labels scope) and label sugar in types/expressions to the
    ///    corresponding `F` references.
    ///
    /// Each desugar-pass error is wrapped as a `LocatedError`
    /// pointing at the offending file; label-elab already returns
    /// `LocatedError`. The ordered pass execution stops on its first returned
    /// error.
    fn lower_package_named(
        modules: Vec<(PathBuf, Module<Surface>)>,
        package_file: Option<PackageFile<Surface>>,
        package_name: Option<&str>,
    ) -> Result<
        (
            Vec<(PathBuf, Module<Lowered>)>,
            Option<PackageFile<Lowered>>,
        ),
        LocatedError,
    > {
        let package_root = crate::pass::surface_registry::inferred_package_root(&modules);
        crate::pass::surface_registry::validate_package(&package_root, &modules)?;
        Self::lower_projected_package_named(modules, package_file, package_name)
    }

    fn lower_projected_package_named(
        modules: Vec<(PathBuf, Module<Surface>)>,
        package_file: Option<PackageFile<Surface>>,
        package_name: Option<&str>,
    ) -> Result<
        (
            Vec<(PathBuf, Module<Lowered>)>,
            Option<PackageFile<Lowered>>,
        ),
        LocatedError,
    > {
        let (lowered, lowered_package_file, ()) = lower_projected_package_named_observed(
            modules,
            package_file,
            package_name,
            |_, _, _| Some(()),
        )?
        .expect("the ordinary lowering observer cannot cancel");
        Ok((lowered, lowered_package_file))
    }

    fn lowering_context_named(
        modules: &[(PathBuf, Module<Surface>)],
        package_file: Option<&PackageFile<Surface>>,
        package_name: Option<&str>,
    ) -> Result<Self::LoweringContext, LocatedError> {
        let package_ops = op_fold::PackageOpScope::from_modules_named(modules, package_name)
            .map_err(|(file_path, error)| LocatedError { file_path, error })?;
        let literal_aliases = collect_package_literal_aliases(modules, package_name);
        let block_scope = crate::pass::surface_registry::PackageBlockScope::from_modules(modules)?;
        let mut desugared_summaries = Vec::with_capacity(modules.len());
        for (file_path, module) in modules {
            let desugared = lower_surface_module_summary(
                file_path.clone(),
                module.clone(),
                &package_ops,
                &block_scope,
                &literal_aliases,
            )?;
            desugared_summaries.push((file_path.clone(), desugared));
        }
        let label_tables = label_elab::PackageLabelTables::build(&desugared_summaries)?;
        let lowered_package_file = match package_file {
            None => None,
            Some(package_file) => {
                let folded =
                    op_fold::fold_package_file_in_package(package_file.clone(), &package_ops)
                        .map_err(|error| LocatedError {
                            file_path: PathBuf::from("<package-file>"),
                            error,
                        })?;
                let desugared = desugar_package_file(folded).map_err(|error| LocatedError {
                    file_path: PathBuf::from("<package-file>"),
                    error,
                })?;
                Some(
                    label_elab::elaborate_package_file(desugared).map_err(|error| {
                        LocatedError {
                            file_path: PathBuf::from("<package-file>"),
                            error,
                        }
                    })?,
                )
            }
        };
        Ok(FullLoweringContext {
            package_ops,
            block_scope,
            literal_aliases,
            label_tables,
            lowered_package_file,
        })
    }

    fn lower_module_with_context(
        context: &Self::LoweringContext,
        file_path: PathBuf,
        module: Module<Surface>,
    ) -> Result<Module<Self::LoweredPhase>, LocatedError> {
        let desugared = lower_surface_module_summary(
            file_path.clone(),
            module,
            &context.package_ops,
            &context.block_scope,
            &context.literal_aliases,
        )?;
        context
            .label_tables
            .elaborate_module(desugared)
            .map_err(|error| LocatedError { file_path, error })
    }

    fn lowered_package_file_from_context(
        context: &Self::LoweringContext,
    ) -> Option<PackageFile<Self::LoweredPhase>> {
        context.lowered_package_file.clone()
    }

    /// Run the elaboration-aware typer on a `Package<Lowered>`.
    /// Returns the `Package<Prime>` produced by substituting recorded
    /// elaborations and validating the result with the standalone
    /// Kio' typer before any backend consumes it.
    fn typecheck(package: &Package<Lowered>) -> Result<Package<Prime>, LocatedError> {
        typecheck_full::check_package(package)
    }

    fn typecheck_module(
        module_path: &str,
        entry: &ModuleEntry<Lowered>,
        package: &Package<Lowered>,
    ) -> Result<Module<Prime>, Vec<LocatedError>> {
        typecheck_module_collect_elaborations(module_path, entry, package).map(|typed| typed.module)
    }

    fn typecheck_module_with_interner(
        module_path: &str,
        entry: &ModuleEntry<Lowered>,
        package: &Package<Lowered>,
        type_interner: std::sync::Arc<crate::pass::typecheck_core::TypeInterner<Lowered>>,
    ) -> Result<Module<Prime>, Vec<LocatedError>> {
        typecheck_module_collect_elaborations_with_interner(
            module_path,
            entry,
            package,
            type_interner,
        )
        .map(|typed| typed.module)
    }

    fn typecheck_module_with_typecheck_scope(
        module_path: &str,
        entry: &ModuleEntry<Lowered>,
        package: &Package<Lowered>,
        typecheck_scope: std::sync::Arc<
            crate::pass::typecheck_core::PackageTypecheckScope<Lowered>,
        >,
    ) -> Result<Module<Prime>, Vec<LocatedError>> {
        typecheck_module_collect_elaborations_with_typecheck_scope(
            module_path,
            entry,
            package,
            typecheck_scope,
        )
        .map(|typed| typed.module)
    }

    fn typecheck_package_file(
        package: &Package<Lowered>,
    ) -> Result<Option<PackageFileEntry<Prime>>, LocatedError> {
        let Some(entry) = package.package_file() else {
            return Ok(None);
        };
        // The package file carries only the phase-independent `bridge`
        // glob list; its host-contract surface is validated in the
        // resolver (`Package::validate_bridge_contract`), so there is
        // nothing to typecheck here. Substitution still runs to lift the
        // package file into `Prime`, with an empty elaboration table.
        let elaborations = typecheck_full::Elaborations::new();
        Ok(Some(PackageFileEntry::<Prime> {
            file_path: entry.file_path.clone(),
            package_name: entry.package_name.clone(),
            package_file: crate::pass::substitute::substitute_package_file_with_pkg(
                &entry.package_file,
                &elaborations,
                &entry.package_name,
            ),
        }))
    }

    fn typecheck_aux_module(
        module: &Module<Lowered>,
        package: &Package<Lowered>,
        package_name: Option<&str>,
    ) -> Result<Module<Prime>, LocatedError> {
        crate::pipeline::assert_aux_module_outside_package(module, package);
        let package = crate::pass::alpha_normalize::normalize_package(package);
        let module = crate::pass::alpha_normalize::normalize_module(module);
        let typecheck_scope =
            crate::pass::typecheck_core::PackageTypecheckScope::fresh_for_normalized_module(
                &module,
                &package,
                std::sync::Arc::new(crate::pass::typecheck_core::TypeInterner::default()),
            );
        // Collect elaborations from typing the carrier module against
        // the consumer package (host + cross-module items in
        // scope), then substitute them into the carrier just as
        // `check_package` does for the package's own
        // modules. The carrier's module path (set by the workspace
        // driver to `<pkg>.<…>`) is the elaboration-keying namespace
        // shared by the typecheck and substitute walks.
        let mut elaborations = typecheck_full::Elaborations::new();
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
                PathBuf::new(),
                errors
                    .into_iter()
                    .next()
                    .expect("collected auxiliary-module errors are non-empty"),
            );
            return Err(error);
        }
        let mut typed = crate::pass::substitute::substitute_module_with_path_in_package(
            module.module(),
            &elaborations,
            &module_path,
            Some(package.package()),
        );
        crate::prime::canonical::canonicalize_module(&mut typed);
        Ok(typed)
    }
}

pub(crate) fn typecheck_module_collect_elaborations(
    module_path: &str,
    entry: &ModuleEntry<Lowered>,
    package: &Package<Lowered>,
) -> Result<FullModuleTypecheck, Vec<LocatedError>> {
    typecheck_module_collect_elaborations_with_interner(
        module_path,
        entry,
        package,
        std::sync::Arc::new(crate::pass::typecheck_core::TypeInterner::default()),
    )
}

pub(crate) fn typecheck_module_collect_elaborations_with_interner(
    module_path: &str,
    _entry: &ModuleEntry<Lowered>,
    package: &Package<Lowered>,
    type_interner: std::sync::Arc<crate::pass::typecheck_core::TypeInterner<Lowered>>,
) -> Result<FullModuleTypecheck, Vec<LocatedError>> {
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
    typecheck_module_collect_elaborations_with_typecheck_scope(
        module_path,
        entry,
        normalized.package(),
        typecheck_scope,
    )
}

pub(crate) fn typecheck_module_collect_elaborations_with_typecheck_scope(
    module_path: &str,
    entry: &ModuleEntry<Lowered>,
    package: &Package<Lowered>,
    typecheck_scope: std::sync::Arc<crate::pass::typecheck_core::PackageTypecheckScope<Lowered>>,
) -> Result<FullModuleTypecheck, Vec<LocatedError>> {
    let mut elaborations = typecheck_full::Elaborations::new();
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
    elaborations.present_binder_names(typecheck_scope.alpha_presentation().clone());
    let mut module = crate::pass::substitute::substitute_module_with_path_in_package(
        &entry.module,
        &elaborations,
        module_path,
        Some(package),
    );
    crate::prime::canonical::canonicalize_module(&mut module);
    Ok(FullModuleTypecheck {
        module,
        #[cfg(feature = "lsp")]
        elaborations,
    })
}

fn lower_surface_module_summary(
    file_path: PathBuf,
    mut module: Module<Surface>,
    package_ops: &op_fold::PackageOpScope,
    block_scope: &crate::pass::surface_registry::PackageBlockScope,
    literal_aliases: &PackageLiteralAliases,
) -> Result<Module<Desugared>, LocatedError> {
    op_fold::fold_module_in_package(&mut module, package_ops).map_err(|error| LocatedError {
        file_path: file_path.clone(),
        error,
    })?;
    crate::pass::block_projection::project_module(&mut module, block_scope).map_err(|error| {
        LocatedError {
            file_path: file_path.clone(),
            error,
        }
    })?;
    desugar_module_with_imports(module, literal_aliases)
        .map_err(|error| LocatedError { file_path, error })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{CallArg, Expr, Item};
    use crate::pass::parser::parse;
    use std::path::{Path, PathBuf};

    #[test]
    fn forwarding_context_and_whole_package_lowering_emit_the_same_prime() {
        let modules = [
            ("origin", "module origin; pub labels { foo: . };"),
            (
                "provider",
                "module provider; import origin as o; pub type {bar} = {o.foo};",
            ),
            (
                "consumer",
                "module consumer; import provider as p; fn value() -> . { {p.bar = ()}.?{p.bar} }",
            ),
        ]
        .into_iter()
        .map(|(name, source)| (PathBuf::from(format!("{name}.kio")), parse(source).unwrap()))
        .collect::<Vec<_>>();
        let context = FullPipeline::lowering_context_named(&modules, None, None).unwrap();
        let contextual = modules
            .iter()
            .map(|(path, module)| {
                let lowered =
                    FullPipeline::lower_module_with_context(&context, path.clone(), module.clone())
                        .unwrap();
                (path.clone(), lowered)
            })
            .collect();
        let (whole, _) = FullPipeline::lower_package(modules, None).unwrap();
        let emitted = |lowered| {
            let package = Package::build(Path::new(""), lowered, None).unwrap();
            let prime = FullPipeline::typecheck(&package).unwrap();
            ["origin", "provider", "consumer"].map(|name| {
                crate::backends::kio_prime::emit_module(&prime.module(name).unwrap().module)
            })
        };
        assert_eq!(emitted(contextual), emitted(whole));
    }

    /// End-to-end test: parse a small Kio module, run it through
    /// `FullPipeline::lower_package` + `FullPipeline::typecheck`,
    /// confirm we get a `Package<Prime>` back. Exercises every
    /// step of the wrapped flow (desugar + label_elab + typer +
    /// substitute) through the trait-based entry points.
    #[test]
    fn full_pipeline_lowers_and_typechecks_a_small_module() {
        // Per `specs/package.md` § Module-name rules, the declared
        // segments equal the file's path relative to the package root.
        let source = r#"module main;

fn id[A](x: A) -> A { x }
"#;
        let module = parse(source).expect("parse");
        let modules = vec![(PathBuf::from("main.kio"), module)];

        let (lowered_modules, lowered_package_file) =
            FullPipeline::lower_package(modules, None).expect("lower_package");
        assert_eq!(lowered_modules.len(), 1);
        assert!(lowered_package_file.is_none());

        let package = Package::build(Path::new(""), lowered_modules, None).expect("Package::build");
        let prime = FullPipeline::typecheck(&package).expect("typecheck");
        // `Package<Prime>` confirmed by the type returned;
        // the value's structure is exercised by the rest of the
        // typer's tests. Just confirm the module is present.
        assert!(prime.module("main").is_some());
    }

    fn has_expression_position_chain(e: &Expr<Prime>) -> bool {
        match e {
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                matches!(value.as_ref(), Expr::Let { .. } | Expr::Seq { .. })
                    || has_expression_position_chain(value)
                    || has_expression_position_chain(body)
            }
            Expr::Call { callee, args, .. } => {
                matches!(callee.as_ref(), Expr::Let { .. } | Expr::Seq { .. })
                    || has_expression_position_chain(callee)
                    || args.iter().any(|arg| match arg {
                        CallArg::Value(value) => {
                            matches!(value, Expr::Let { .. } | Expr::Seq { .. })
                                || has_expression_position_chain(value)
                        }
                        CallArg::Type(_) => false,
                    })
            }
            Expr::FnExpr { body, .. } => has_expression_position_chain(body),
            _ => false,
        }
    }

    #[test]
    fn typed_module_cache_output_has_a_canonical_statement_spine() {
        let source = r#"module main;

host type I32 role(i32);
host fn effect(value: I32) -> I32;

labels Pair_fields = { first: I32, second: I32 };
type Pair = First & Second;

fn build(x: I32) -> Pair {
  let row = { first = effect(x), second = effect(x) };
  row
}
"#;
        let module = parse(source).expect("parse");
        let (lowered_modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower_package");
        let package = Package::build(Path::new(""), lowered_modules, None).expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        let entry = package.module("main").expect("main module");
        let typed =
            FullPipeline::typecheck_module("main", entry, &package).expect("typecheck_module");
        let body = typed
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == "build" => Some(&def.body),
                _ => None,
            })
            .expect("build function");

        assert!(!has_expression_position_chain(body));
    }

    #[test]
    fn single_module_typecheck_accepts_a_logical_package_entry_clone() {
        let source = r#"module main;

fn id[A](value: A) -> A { value }
"#;
        let module = parse(source).expect("parse");
        let (lowered_modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower_package");
        let package = Package::build(Path::new(""), lowered_modules, None).expect("Package::build");
        let entry = package.module("main").expect("main module").clone();

        FullPipeline::typecheck_module("main", &entry, &package)
            .expect("a logical entry clone remains a valid single-module input");
    }

    #[test]
    #[should_panic(expected = "must not be part of the package module set")]
    fn full_auxiliary_module_rejects_a_package_path_collision() {
        let module = parse("module collision; fn id[A](value: A) -> A { value }").expect("parse");
        let (lowered_modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("collision.kio"), module)], None)
                .expect("lower package");
        let package =
            Package::build(Path::new(""), lowered_modules, None).expect("build package shell");
        let carrier = package
            .module("collision")
            .expect("collision module")
            .module
            .clone();

        let _ = FullPipeline::typecheck_aux_module(&carrier, &package, None);
    }

    #[test]
    fn auxiliary_module_stages_imported_elaborator_from_bound_package() {
        let sources = [
            (
                "helpers.kio",
                r#"module helpers;

import __comptime__;

pub pure fn keep(_ct: __Comptime__, value: __Checked_term__) -> __Checked_term__ { value }
"#,
            ),
            (
                "provider.kio",
                r#"module provider;

import __comptime__;
import helpers(keep);

pure fn identity_impl(
    _ct: __Comptime__,
    _source: __Type__,
    value: __Checked_term__,
    _target: __Type__ | .,
) -> __Checked_term__ {
    keep(_ct, value)
}

pub elab identity : [Source] Source -> [Target] Target { impl identity_impl; };
"#,
            ),
            (
                "adapter.kio",
                r#"module adapter;

import provider(identity);

type Expected = .;

fn adapt() -> . {
    let inferred = identity!(());
    inferred
}

fn adapt_expected() -> Expected {
    identity!(())
}
"#,
            ),
        ];
        let modules = sources
            .into_iter()
            .map(|(path, source)| {
                (
                    PathBuf::from(path),
                    parse(source).unwrap_or_else(|error| panic!("parse {path}: {error:?}")),
                )
            })
            .collect();
        let (mut lowered_modules, _) =
            FullPipeline::lower_package(modules, None).expect("lower_package");
        let adapter_index = lowered_modules
            .iter()
            .position(|(path, _)| path == Path::new("adapter.kio"))
            .expect("adapter module");
        let (_, adapter) = lowered_modules.remove(adapter_index);
        let package = Package::build(Path::new(""), lowered_modules, None).expect("Package::build");
        package.resolve_imports().expect("resolve package uses");
        package
            .check_in_body_resolution()
            .expect("resolve package bodies");

        FullPipeline::typecheck_aux_module(&adapter, &package, None)
            .expect("the auxiliary module may stage its imported package elaborator");
    }

    #[test]
    fn auxiliary_module_keeps_alias_owner_identity_outside_package_index() {
        let sources = [
            ("base.kio", "module base; pub host type Value;"),
            ("wrong.kio", "module wrong; pub host type Value;"),
            (
                "owner.kio",
                "module owner; import base as base; pub type Exact = base.Value;",
            ),
            (
                "adapter.kio",
                "module adapter;
                 import base(Value);
                 import owner(Exact);
                 import wrong as base;
                 fn witness(value: Value) -> Value {
                     .(item: Exact) -> Exact { item }(value)
                 }",
            ),
        ];
        let modules = sources
            .into_iter()
            .map(|(path, source)| {
                (
                    PathBuf::from(path),
                    parse(source).unwrap_or_else(|error| panic!("parse {path}: {error:?}")),
                )
            })
            .collect();
        let (mut lowered_modules, _) =
            FullPipeline::lower_package(modules, None).expect("lower package and adapter");
        let adapter_index = lowered_modules
            .iter()
            .position(|(path, _)| path == Path::new("adapter.kio"))
            .expect("adapter module");
        let (_, adapter) = lowered_modules.remove(adapter_index);
        let package = Package::build(Path::new(""), lowered_modules, None).expect("build package");
        package.resolve_imports().expect("resolve package uses");
        package
            .check_in_body_resolution()
            .expect("resolve package bodies");

        FullPipeline::typecheck_aux_module(&adapter, &package, None).expect(
            "the owner-qualified `base.Value` must not be re-read through adapter alias `base`",
        );
    }
}
