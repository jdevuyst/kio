//! `Pipeline` trait — the front-end abstraction shared between the
//! `kio` and `kio-prime` binaries.
//!
//! Each binary supplies its own [`Pipeline`] impl describing how a
//! parsed Surface package is lowered to the typer's input phase,
//! plus how the typer is invoked. The driver code in `check.rs`
//! (and `build.rs` indirectly, via `check::compile_package`) is
//! generic over the trait, so the same file-walking glue, error
//! reporting, and backend selection serves both binaries.
//!
//! The two impls:
//!
//! - **[`crate::pass::full::FullPipeline`]** (kio binary, `--features surface`).
//!   `LoweredPhase = Lowered`. `lower_package` folds operators, runs
//!   `desugar_module` + `desugar_package_file` per file, then
//!   `label_elab::elaborate_package` over the desugared batch (which
//!   needs every module's local label table available before
//!   rewriting). `typecheck` calls
//!   [`crate::pass::typecheck_full::check_package`], which records
//!   boundary-local completions while checking Lowered, substitutes them into
//!   a fresh `Package<Prime>`, and validates that package with the standalone
//!   Prime checker.
//!
//! - **[`crate::prime::pipeline::PrimePipeline`]** (kio-prime binary,
//!   `--features prime`). `LoweredPhase = Prime`. `lower_package`
//!   calls `prime::lower::lower_module` (and `lower_package_file`)
//!   on each entry — the walk rejects every surface-only variant
//!   with a parse error and emits Prime directly. `typecheck`
//!   calls `prime::typer::check_package`, the standalone
//!   Kio'-only walker that routes the recursive `synth_expr` /
//!   `check_value_against` through the `Typer<Prime>` impl
//!   (`PrimeTyper`). Surface-bearing variants discharge via `match *ext {}`;
//!   its narrower `PrimeElaborations` table records only Kio'-admitted call and
//!   lambda completions and is baked into the returned AST.

use std::path::{Path, PathBuf};

use crate::ast::{Module, PackageFile, Phase, Prime, Surface};
use crate::pass::resolve::{LocatedError, ModuleEntry, Package, PackageFileEntry};

/// Enforce the auxiliary-carrier boundary before either pipeline normalizes or
/// typechecks the carrier. Its declared slash path must be absent from the
/// package module set; otherwise two different module bodies would claim one
/// nominal identity.
pub(crate) fn assert_aux_module_outside_package<P: Phase>(
    module: &Module<P>,
    package: &Package<P>,
) {
    let module_path = module
        .path
        .segments
        .iter()
        .map(crate::ast::PathSegment::as_str)
        .collect::<Vec<_>>()
        .join("/");
    assert!(
        package.module(&module_path).is_none(),
        "auxiliary module `{module_path}` must not be part of the package module set"
    );
}

/// Front-end pipeline: lower a parsed Surface package to the
/// typer's input phase, then type-check it into `Package<Prime>`.
///
/// The typer's input phase varies by binary — `Lowered` for the
/// full Kio compiler (after desugar + label-elab strip surface
/// sugar), `Prime` for the kio-prime binary (which lowers Surface
/// directly to Prime, rejecting surface-only forms with a parse
/// error). Either way the typer's *output* phase is `Prime`, so every later
/// path starts from a uniform validated `Package<Prime>` regardless of which
/// front-end produced it. The Kio' emitter consumes that package directly;
/// host backends consume its recovered and routed descendants.
///
/// ## Why package-level lowering
///
/// Both implementations lower at the package level rather than
/// per-module. For the full pipeline, `label_elab` builds a
/// cross-module label table during Pass 1, then walks each module
/// against the effective table during Pass 2 — per-module
/// lowering would have to re-build the table for every call.
/// For the prime pipeline, per-module lowering would be
/// sufficient (Kio' has no `labels` and no cross-module label
/// imports), but a package-level method composes uniformly with
/// the full pipeline and matches the shape `check.rs` already
/// hands the typer (`Vec<(PathBuf, Module<…>)>` + optional
/// package file).
pub trait Pipeline {
    /// The phase the typer consumes. `Lowered` for the full
    /// pipeline, `Prime` for the prime pipeline.
    type LoweredPhase: Phase + crate::pass::visit_mut::TypecheckVisitPhase;

    /// Package-level lowering summary. Built once from lazy module
    /// headers/meta, then reused to lower individual ready modules.
    type LoweringContext: Sync;

    /// Pipeline tag folded into package-check cache entries so the
    /// two binaries never share a cache file. Their typers differ,
    /// so a cache validated by one must not let the other skip its
    /// own typecheck.
    const CACHE_TAG: &'static str;

    /// Validate source-phase package invariants that would otherwise be
    /// consumed by lowering. The full Surface pipeline validates namespace,
    /// use, and binding-origin invariants here, including registries consumed
    /// before the ordinary resolver runs; Kio' has no consumed source
    /// registries. Callers that hold lazy bodies may probe them before
    /// returning an error from this validation; the successful path stays lazy.
    fn validate_source_package(
        _package_root: &Path,
        _modules: &[(PathBuf, Module<Surface>)],
    ) -> Result<(), LocatedError> {
        Ok(())
    }

    /// Lower a parsed Surface package to the typer's input
    /// phase. Takes every parsed module (paired with its file
    /// path for diagnostics) and the optional package file, then
    /// returns the lowered counterparts in the same shape.
    ///
    /// `FullPipeline`'s impl runs `op_fold`, threads each module
    /// through `desugar_module`, the package file through
    /// `desugar_package_file`, then hands the desugared batch to
    /// `label_elab::elaborate_package`.
    /// `PrimePipeline`'s impl calls `prime::lower::lower_module`
    /// (and `lower_package_file`) on each entry; rejection of
    /// surface-only variants happens inside that walk.
    #[allow(clippy::type_complexity)] // documented Vec-of-pairs / option pattern
    fn lower_package(
        modules: Vec<(PathBuf, Module<Surface>)>,
        package_file: Option<PackageFile<Surface>>,
    ) -> Result<
        (
            Vec<(PathBuf, Module<Self::LoweredPhase>)>,
            Option<PackageFile<Self::LoweredPhase>>,
        ),
        LocatedError,
    > {
        Self::lower_package_named(modules, package_file, None)
    }

    /// As [`Self::lower_package`], but additionally carries the package name
    /// (the `<name>.pkg.kio` filename stem) for package-boundary bookkeeping.
    /// The name must not change how regular modules are lowered: their
    /// semantic identities come from their declared module paths, and the
    /// typed-module cache reuses byte-identical module inputs across package
    /// names after validating each package boundary independently.
    #[allow(clippy::type_complexity)]
    fn lower_package_named(
        modules: Vec<(PathBuf, Module<Surface>)>,
        package_file: Option<PackageFile<Surface>>,
        package_name: Option<&str>,
    ) -> Result<
        (
            Vec<(PathBuf, Module<Self::LoweredPhase>)>,
            Option<PackageFile<Self::LoweredPhase>>,
        ),
        LocatedError,
    >;

    /// Lower a signature-only projection derived from a source package that
    /// was validated before its bodies and consumed declarations were
    /// stripped. The default has no distinct path; the full Surface pipeline
    /// overrides it so projection cannot revalidate information that is no
    /// longer present.
    #[allow(clippy::type_complexity)]
    fn lower_projected_package_named(
        modules: Vec<(PathBuf, Module<Surface>)>,
        package_file: Option<PackageFile<Surface>>,
        package_name: Option<&str>,
    ) -> Result<
        (
            Vec<(PathBuf, Module<Self::LoweredPhase>)>,
            Option<PackageFile<Self::LoweredPhase>>,
        ),
        LocatedError,
    > {
        Self::lower_package_named(modules, package_file, package_name)
    }

    /// Build the package-level lowering summary used by the scheduled
    /// frontend. The context fields consumed by [`Self::lower_module_with_context`]
    /// must be derived only from stable declared surfaces: module paths, `import`
    /// clauses, `pub op`, `pub type`, `pub literal`, and labels. They must not
    /// depend on the package name, package-only configuration, export entries,
    /// or unrelated private bodies. Package-file lowering may remain in the
    /// same context, but the driver validates and consumes that boundary
    /// independently from regular typed-module cache entries.
    fn lowering_context_named(
        modules: &[(PathBuf, Module<Surface>)],
        package_file: Option<&PackageFile<Surface>>,
        package_name: Option<&str>,
    ) -> Result<Self::LoweringContext, LocatedError>;

    /// Lower one module against a pre-built package lowering context.
    fn lower_module_with_context(
        context: &Self::LoweringContext,
        file_path: PathBuf,
        module: Module<Surface>,
    ) -> Result<Module<Self::LoweredPhase>, LocatedError>;

    /// Return the package file lowered while building the context.
    fn lowered_package_file_from_context(
        context: &Self::LoweringContext,
    ) -> Option<PackageFile<Self::LoweredPhase>>;

    /// Run the typer on a fully-resolved package. Returns `Package<Prime>` on
    /// success. Both pipelines produce the same validated, canonical output
    /// phase even though their input phases differ. The full pipeline checks
    /// Lowered, substitutes its recorded completions, and revalidates the
    /// resulting artifact. The prime pipeline checks its Prime input and bakes
    /// the checker's narrower completion records before returning it.
    fn typecheck(package: &Package<Self::LoweredPhase>) -> Result<Package<Prime>, LocatedError>;

    /// Type-check one already-resolved regular module inside
    /// `package`, returning only that module's typed `Prime` form.
    /// The workspace driver uses this for the typed-module cache:
    /// cache hits bypass this method, misses call it and then store
    /// the result. Implementations must perform the same per-module
    /// work their package-level [`typecheck`] path would
    /// perform, including any phase-specific elaboration baking. The returned
    /// module may depend on its exact lowered input and the ordinary modules
    /// reachable through its `import` clauses, but not on package-file metadata
    /// or unrelated declarations; the driver's semantic key records that
    /// dependency closure and deliberately omits the independently validated
    /// package boundary.
    fn typecheck_module(
        module_path: &str,
        entry: &ModuleEntry<Self::LoweredPhase>,
        package: &Package<Self::LoweredPhase>,
    ) -> Result<Module<Prime>, Vec<LocatedError>>;

    /// As [`Self::typecheck_module`], but with a caller-owned
    /// type interner shared across a package-level scheduling run.
    /// Implementations that do not use interning can fall back to the
    /// plain entry point.
    fn typecheck_module_with_interner(
        module_path: &str,
        entry: &ModuleEntry<Self::LoweredPhase>,
        package: &Package<Self::LoweredPhase>,
        _type_interner: std::sync::Arc<
            crate::pass::typecheck_core::TypeInterner<Self::LoweredPhase>,
        >,
    ) -> Result<Module<Prime>, Vec<LocatedError>> {
        Self::typecheck_module(module_path, entry, package)
    }

    /// As [`Self::typecheck_module_with_interner`], but shares the
    /// package-version memo cache and the current analysis's ephemeral
    /// state across the modules in one scheduling run.
    fn typecheck_module_with_typecheck_scope(
        module_path: &str,
        entry: &ModuleEntry<Self::LoweredPhase>,
        package: &Package<Self::LoweredPhase>,
        typecheck_scope: std::sync::Arc<
            crate::pass::typecheck_core::PackageTypecheckScope<Self::LoweredPhase>,
        >,
    ) -> Result<Module<Prime>, Vec<LocatedError>> {
        Self::typecheck_module_with_interner(
            module_path,
            entry,
            package,
            typecheck_scope.type_interner().clone(),
        )
    }

    /// Type-check and phase-convert the optional package file for
    /// `package`. Regular module bodies are deliberately out of
    /// scope here; this mirrors the package-level typer's final
    /// package-file pass after module checking has succeeded.
    fn typecheck_package_file(
        package: &Package<Self::LoweredPhase>,
    ) -> Result<Option<PackageFileEntry<Prime>>, LocatedError>;

    /// Type-check one **auxiliary** module against an
    /// already-resolved package and return its `Prime` form. The
    /// module is not part of the package's own module set — it is a
    /// synthetic carrier the workspace driver builds for the
    /// package bridge adaptation bodies. Typing it against the
    /// consumer package puts the package's host and
    /// cross-module items in scope, so the body resolves exactly as a
    /// bridge RHS would; the returned `Prime` body is what codegen runs
    /// at the adapter boundary.
    ///
    /// `FullPipeline` collects completions and substitutes them into the
    /// carrier. `PrimePipeline` checks its already-Prime carrier, bakes any
    /// Kio'-admitted call/lambda completion, and canonicalizes it.
    fn typecheck_aux_module(
        module: &Module<Self::LoweredPhase>,
        package: &Package<Self::LoweredPhase>,
        package_name: Option<&str>,
    ) -> Result<Module<Prime>, LocatedError>;
}

#[cfg(all(test, feature = "surface", feature = "prime"))]
mod tests {
    use super::*;
    use crate::pass::full::FullPipeline;
    use crate::prime::pipeline::PrimePipeline;

    #[test]
    fn pipeline_can_be_used_generically() {
        // A driver function generic over `<P: Pipeline>` is the
        // shape `check::compile_package_with` uses — exercise the
        // bound against both real impls.
        fn driver_shape<P: Pipeline>() {
            // No body — we're just exercising the generic bound.
        }
        driver_shape::<FullPipeline>();
        driver_shape::<PrimePipeline>();
    }
}
