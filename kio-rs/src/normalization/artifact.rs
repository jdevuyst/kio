//! Validated evaluator ownership and same-artifact context preparation.

use std::sync::Arc;

use super::{
    Env, EvalAstExpr, EvalCtx, EvalMetrics, EvalPackage, EvalPhase, EvalPrimitiveEnv,
    EvalSourceGeneration, FnMap, QualifiedFnMap, SharedEvalFunctionCache, SharedExactCallMemoCache,
    SharedFnClosureCache, SharedTypeViewCache, Value, eval_fn_tables, eval_with_compile_scope,
};
use crate::ast::{Expr, Module, UncheckedPrime};
#[cfg(test)]
use crate::ast::{Meta, PathSegment};
use crate::pass::resolve::Package;
#[cfg(test)]
use crate::span::Span;

/// Resolved evaluator assembly, before its consumer-specific validation.
#[derive(Debug)]
pub(crate) struct EvalArtifactInput {
    package: EvalPackage,
    primitives: EvalPrimitiveEnv,
}

impl EvalArtifactInput {
    pub(crate) fn new(package: EvalPackage, primitives: EvalPrimitiveEnv) -> Self {
        Self {
            package,
            primitives,
        }
    }

    pub(crate) fn package(&self) -> &EvalPackage {
        &self.package
    }

    pub(crate) fn primitives(&self) -> &EvalPrimitiveEnv {
        &self.primitives
    }

    pub(crate) fn into_parts(self) -> (EvalPackage, EvalPrimitiveEnv) {
        (self.package, self.primitives)
    }
}

/// Independently validated Kio' evaluator input paired with the explicit
/// host-provided primitive environment under which it may reduce.
/// Cloning retains one private evaluator generation; constructing another
/// artifact, including from identical parts, creates a distinct generation.
#[derive(Debug, Clone)]
pub struct EvalArtifact {
    package: Arc<EvalPackage>,
    primitives: EvalPrimitiveEnv,
    source_generation: EvalSourceGeneration,
}

impl EvalArtifact {
    fn from_validated_parts(package: EvalPackage, primitives: EvalPrimitiveEnv) -> Self {
        Self {
            package: Arc::new(package),
            primitives,
            source_generation: EvalSourceGeneration::fresh(),
        }
    }

    pub(crate) fn from_completed_source(
        source: &crate::pass::typecheck_full::CompletedLoweredSource,
    ) -> Result<Self, crate::pass::resolve::LocatedError> {
        let checked = source.checked();
        let input = crate::pass::substitute::substitute_package_for_eval(
            checked.normalized(),
            checked.elaborations(),
        )?;
        let (package, primitives) = input.into_parts();
        Ok(Self::from_validated_parts(package, primitives))
    }

    pub fn package(&self) -> &Package<UncheckedPrime> {
        &self.package
    }

    pub fn primitives(&self) -> &EvalPrimitiveEnv {
        &self.primitives
    }

    fn source_generation(&self) -> EvalSourceGeneration {
        self.source_generation.clone()
    }

    /// Derive the module and its resolution tables together, once per batch.
    pub(crate) fn prepare_module<'m>(
        &'m self,
        module_path: &'m str,
    ) -> Result<PreparedEvalModule<'m>, crate::error::Error> {
        let module = &self
            .package
            .module(module_path)
            .unwrap_or_else(|| {
                panic!("validated evaluator artifact is missing module `{module_path}`")
            })
            .module;
        let (fn_defs, qualified_fn_defs) = eval_fn_tables(self.package(), module)?;
        Ok(PreparedEvalModule {
            artifact: self,
            module,
            module_path,
            fn_defs,
            qualified_fn_defs,
        })
    }
}

/// Executable staged code sealed by its existing implementation validator.
#[derive(Debug)]
pub(crate) struct UserElaboratorEvalArtifactPair {
    artifact: Arc<EvalArtifact>,
    implementation: EvalAstExpr,
    implementation_module_path: String,
}

impl UserElaboratorEvalArtifactPair {
    pub(crate) fn validate(
        input: EvalArtifactInput,
        implementation: EvalAstExpr,
        implementation_module_path: String,
    ) -> Result<Self, crate::error::Error> {
        crate::pass::typecheck_full::validate_user_elaborator_eval_artifact(
            &input,
            &implementation,
            &implementation_module_path,
        )?;
        let (package, primitives) = input.into_parts();
        Ok(Self {
            artifact: Arc::new(EvalArtifact::from_validated_parts(package, primitives)),
            implementation,
            implementation_module_path,
        })
    }

    pub(crate) fn artifact(&self) -> &Arc<EvalArtifact> {
        &self.artifact
    }

    pub(crate) fn implementation(&self) -> &EvalAstExpr {
        &self.implementation
    }

    pub(crate) fn implementation_module_path(&self) -> &str {
        &self.implementation_module_path
    }

    #[cfg(test)]
    pub(crate) fn unit_for_test(implementation_name: &str) -> Self {
        use crate::pipeline::Pipeline;
        let source = format!("module memo; pure fn {implementation_name}() -> . {{ () }}");
        let parsed = crate::pass::parser::parse(&source).expect("parse memo fixture");
        let (modules, _) = crate::pass::full::FullPipeline::lower_package(
            vec![(std::path::PathBuf::from("memo.kio"), parsed)],
            None,
        )
        .expect("lower memo fixture");
        let package =
            Package::build(std::path::Path::new(""), modules, None).expect("resolve memo fixture");
        let normalized = crate::pass::alpha_normalize::normalize_package(&package);
        let input = crate::pass::substitute::substitute_package_for_eval(
            &normalized,
            &crate::pass::typecheck_full::Elaborations::new(),
        )
        .expect("assemble memo fixture");
        Self::validate(
            input,
            Expr::Path {
                occurrence: Default::default(),
                segments: vec![PathSegment::new(
                    implementation_name.to_owned(),
                    Span::new(0, 0),
                )],
                meta: Meta::new(Span::new(0, 0)),
                ext: (),
            },
            "memo".to_owned(),
        )
        .expect("validate memo fixture")
    }
}

/// Artifact-issued module/path/tables; fields cannot be independently replaced.
#[derive(Clone)]
pub(crate) struct PreparedEvalModule<'m> {
    artifact: &'m EvalArtifact,
    module: &'m Module<EvalPhase>,
    module_path: &'m str,
    fn_defs: FnMap<'m>,
    qualified_fn_defs: QualifiedFnMap<'m>,
}

impl<'m> PreparedEvalModule<'m> {
    #[cfg(feature = "cli")]
    pub(crate) fn module(&self) -> &'m Module<EvalPhase> {
        self.module
    }
}

impl std::ops::Deref for EvalArtifact {
    type Target = Package<UncheckedPrime>;

    fn deref(&self) -> &Self::Target {
        &self.package
    }
}

/// Immutable evaluator context bound to one validated artifact generation.
///
/// This wrapper is crate-private so the shared compile/closure caches can be
/// used only while the package, primitive grants, and root resolution tables
/// remain exactly those validated for the retained artifact. Public mutable
/// [`EvalCtx`] values always compile under a fresh root generation instead.
pub(crate) struct AuthenticatedEvalCtx<'m> {
    inner: EvalCtx<'m>,
    source_generation: EvalSourceGeneration,
}

impl<'m> AuthenticatedEvalCtx<'m> {
    pub(crate) fn for_module(prepared: super::PreparedEvalModule<'m>) -> Self {
        let PreparedEvalModule {
            artifact,
            module,
            module_path,
            fn_defs,
            qualified_fn_defs,
        } = prepared;
        Self {
            inner: EvalCtx::new()
                .with_fn_defs(fn_defs)
                .with_qualified_fn_defs(qualified_fn_defs)
                .with_module_path(module_path)
                .with_root_module(module)
                .with_package(Some(artifact.package()))
                .with_primitive_env(artifact.primitives().clone()),
            source_generation: artifact.source_generation(),
        }
    }

    pub(crate) fn with_metrics(mut self, metrics: Arc<EvalMetrics>) -> Self {
        self.inner = self.inner.with_metrics(metrics);
        self
    }

    pub(crate) fn with_fn_closure_cache(mut self, cache: SharedFnClosureCache) -> Self {
        self.inner = self.inner.with_fn_closure_cache(cache);
        self
    }

    pub(crate) fn with_eval_function_cache(mut self, cache: SharedEvalFunctionCache) -> Self {
        self.inner = self.inner.with_eval_function_cache(cache);
        self
    }

    pub(crate) fn with_type_view_cache(mut self, cache: SharedTypeViewCache) -> Self {
        self.inner = self.inner.with_type_view_cache(cache);
        self
    }

    pub(crate) fn with_exact_call_memo_cache(mut self, cache: SharedExactCallMemoCache) -> Self {
        self.inner = self.inner.with_exact_call_memo_cache(cache);
        self
    }
}

impl<'m> std::ops::Deref for AuthenticatedEvalCtx<'m> {
    type Target = EvalCtx<'m>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub(crate) fn eval_authenticated(
    expr: &Expr<EvalPhase>,
    env: &Env,
    ctx: &AuthenticatedEvalCtx<'_>,
) -> Value {
    eval_with_compile_scope(expr, env, ctx, ctx.source_generation.clone(), true)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::ast::{ImportKind, Item, Lowered as SourcePhase};
    use crate::normalization::module_path_string;
    use crate::pass::typecheck_full::Elaborations;

    pub(in crate::normalization) fn lowered_package(mods: &[(&str, &str)]) -> EvalArtifact {
        use std::path::PathBuf;
        let parsed: Vec<(PathBuf, _)> = mods
            .iter()
            .map(|(path, src)| {
                let surface = crate::pass::parser::parse(src).expect("parse");
                let desugared = crate::pass::desugar::desugar_module(surface).expect("desugar");
                (PathBuf::from(path), desugared)
            })
            .collect();
        let (lowered, _) =
            crate::pass::label_elab::elaborate_package(parsed, None).expect("label_elab");
        let package: crate::pass::resolve::Package<SourcePhase> =
            crate::pass::resolve::Package::build(std::path::Path::new(""), lowered, None)
                .expect("package build");
        package.resolve_imports().expect("resolve imports");
        let package = crate::pass::alpha_normalize::normalize_package(&package);
        let input =
            crate::pass::substitute::substitute_package_for_eval(&package, &Elaborations::new())
                .expect("evaluator artifact");
        let (package, primitives) = input.into_parts();
        EvalArtifact::from_validated_parts(package, primitives)
    }

    pub(in crate::normalization) fn reserved_shadow_artifact() -> EvalArtifact {
        use std::path::PathBuf;

        let sources = [
            (
                "provider.kio",
                "module provider; import __comptime__; \
                 pub fn shadow(proof: .)(left: ., middle: ., right: .) -> . { left }",
            ),
            (
                "selective.kio",
                "module selective; import __comptime__; import provider(shadow); fn keep() -> . { () }",
            ),
            (
                "qualified.kio",
                "module qualified; import __comptime__; import provider as p; fn keep() -> . { () }",
            ),
            (
                "builtin.kio",
                "module builtin; import __comptime__; fn keep() -> . { () }",
            ),
        ];
        let parsed = sources
            .iter()
            .map(|(path, source)| {
                let surface = crate::pass::parser::parse(source).expect("parse shadow package");
                let desugared =
                    crate::pass::desugar::desugar_module(surface).expect("desugar shadow package");
                (PathBuf::from(path), desugared)
            })
            .collect();
        let (mut lowered, _) = crate::pass::label_elab::elaborate_package(parsed, None)
            .expect("label-elaborate shadow package");
        for (_, module) in &mut lowered {
            match module_path_string(module).as_str() {
                "provider" => {
                    let def = module
                        .items
                        .iter_mut()
                        .find_map(|item| match item {
                            Item::FnDef(def) if def.name == "shadow" => Some(def),
                            _ => None,
                        })
                        .expect("provider shadow definition");
                    def.name = "__structural_recur__".to_owned();
                }
                "selective" => {
                    let import_item = module
                        .imports
                        .iter_mut()
                        .find_map(|import_| match &mut import_.kind {
                            ImportKind::Selective { items, .. } => items.first_mut(),
                            _ => None,
                        })
                        .expect("selective shadow import");
                    let crate::ast::ImportItem::Name { name, .. } = import_item else {
                        panic!("ordinary selective shadow import")
                    };
                    *name = "__structural_recur__".to_owned();
                }
                _ => {}
            }
        }
        let package: crate::pass::resolve::Package<SourcePhase> =
            crate::pass::resolve::Package::build(std::path::Path::new(""), lowered, None)
                .expect("shadow package build");
        package.resolve_imports().expect("resolve shadow uses");
        let package = crate::pass::alpha_normalize::normalize_package(&package);
        let input =
            crate::pass::substitute::substitute_package_for_eval(&package, &Elaborations::new())
                .expect("shadow evaluator artifact");
        let (package, primitives) = input.into_parts();
        EvalArtifact::from_validated_parts(package, primitives)
    }

    #[test]
    fn artifact_clones_share_package_and_generation() {
        let source =
            "module x; import __comptime__; fn held(value: .) -> . { __structural_recur__ }";
        let artifact = lowered_package(&[("x.kio", source)]);
        let cloned = artifact.clone();
        let rebuilt = lowered_package(&[("x.kio", source)]);
        assert!(Arc::ptr_eq(&artifact.package, &cloned.package));
        assert!(!Arc::ptr_eq(&artifact.package, &rebuilt.package));
        assert_eq!(artifact.source_generation(), cloned.source_generation());
        assert_ne!(artifact.source_generation(), rebuilt.source_generation());
    }
}
