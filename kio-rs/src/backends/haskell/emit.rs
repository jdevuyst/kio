//! Haskell backend — IR → Haskell package lowering.
//!
//! Consumes a `Package<Routed>` (post-recovery + post-resolution-lowering)
//! and produces a self-contained Haskell package as a [`HaskellPackage`].
//!
//! ## Body representation — native types, with a universal floor
//!
//! Two body renderings share one typed FFI skin (the host record, structural
//! families and boundary patterns, exported wrappers — identical whichever
//! body a package gets):
//!
//! - The **native `f a`** body ([`super::native`]) renders the Routed IR
//!   at its native Haskell types: a kind-`*→*` carrier `F(A)` becomes a
//!   real type-constructor application `f a`, and each parametric source
//!   newtype has one declaration-stable nominal Haskell head. Concrete host
//!   types remain exact associated-family applications throughout.
//! - The **private universal-carrier** body (this module) is the floor every
//!   emitter reproduces — a **native** universal value ADT
//!   ([`super::runtime`]), not the erased `Any` / `interface{}` of Swift /
//!   Go. Products use a native slot vector, sums a native tag + payload
//!   matched by native `case`, scalars use native
//!   `Integer` / `Double` / `Text` / `Bool` inside typed constructors, a
//!   closure a native carrier-to-monadic-carrier function. No `Data.Dynamic`,
//!   no `unsafeCoerce`, no carrier-walk — HKT carriers ride Haskell's own
//!   type-constructor application. Every body expression evaluates to
//!   a monadic private carrier. A package with concrete host types must use the
//!   native body; the universal floor cannot erase a host-selected type.
//!
//! Both bridge Kio's **strict** evaluation into Haskell's **lazy** host the
//! same way:
//!
//! - **Effect order** sequences through the monad. A host call is `m a`;
//!   the body sequences sub-expressions with `>>=` (a `do` block) so host
//!   effects fire in Kio's evaluation order, and the value is produced with
//!   `pure`.
//! - **Value strictness** forces bound values. A `let` / `Seq` binding's
//!   value is forced (`seq` / the private runtime helper / strict fields) so a
//!   bound Kio value is evaluated when the strict semantics says it is, not
//!   lazily on first use.
//!
//! ## Skin
//!
//! The **skin** is the host's typed contract — the `Host h m` value record,
//! the closed product / sum families and flat patterns the FFI surfaces, the
//! exported surface — driven through the shared [`crate::backends::skin`]
//! framework via a [`super::skin::HaskellSkin`]. Every emitted function is
//! **monad-polymorphic** (`Monad m => …`); the host record is a value
//! record of `m`-returning functions (not a typeclass); the exported
//! surface is `Monad m => …`.

#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{Expr, Kind, Routed, Type, TypeParam};
use crate::backends::boundary_facade::{
    BoundaryFacadeSiteId, BoundaryFacadeSiteOwner, CallableSourceParamAdapter,
    PreparedBoundaryCallableSites,
};
use crate::backends::haskell::facade::{
    HaskellCallableHeadStage, HaskellFacadeCatalog, HaskellStructuralOccurrence,
};
use crate::backends::skin::{FfiDir, SkinProfile};
use crate::pass::resolve::Package;

use super::naming::{
    BoundaryId, BoundaryStep, HaskellName, ItemId, ModuleItemId, NameClaims, PatternId,
    StructuralNames, SumPatternId,
};

/// An unrecoverable error during Haskell emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitError {
    pub message: String,
}

impl EmitError {
    pub fn unsupported(message: impl Into<String>) -> Self {
        EmitError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Emitted Haskell package — one self-contained facade module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaskellPackage {
    /// `<Ns>.hs` — the package's invocable surface: the `<Handle>Host h m`
    /// record, the structural family API, the `<Handle> h m` handle, the
    /// `create<Handle>` factory, and every module fn / export as a
    /// monad-polymorphic top-level function.
    pub pkg_hs: String,
}

/// The branded facade names for one emitted package, all derived from the
/// effective namespace: the `module` clause (`ns`), the package handle
/// (`<Handle>`, the namespace's final segment PascalCased into a GHC
/// module segment), the `<Handle>Host` contract, the `create<Handle>`
/// factory, and the namespace-derived private runtime declarations. One
/// derivation root means a consumer can reconstruct the whole public surface
/// from the artifact's `module` clause alone.
pub(crate) struct HaskellNames {
    pub ns: String,
    pub handle: String,
    pub host_ty: String,
    pub host_types_ty: String,
    pub factory: String,
    pub product_family: String,
    pub sum_family: String,
    pub structural_names: StructuralNames,
    pub runtime_names: super::naming::RuntimeNames,
    pub standard_names: super::naming::StandardNames,
}

impl HaskellNames {
    pub(crate) fn derive(ns: &str) -> HaskellNames {
        // The handle is the namespace's final segment (already an
        // upper-initial GHC module segment); a dotted `Com.Acme.Greeter`
        // brands as `Greeter`.
        let handle = ns.rsplit('.').next().unwrap_or(ns).to_owned();
        let structural_names = StructuralNames::new(ns);
        let host_ty = format!("{handle}Host");
        let host_types_ty = format!("{handle}HostTypes");
        let factory = format!("create{handle}");
        let product_family = structural_names.product_family().to_owned();
        let sum_family = structural_names.sum_family().to_owned();
        let runtime_names = super::naming::RuntimeNames::new(ns);
        let standard_names = super::naming::StandardNames::new(ns);
        HaskellNames {
            host_ty,
            host_types_ty,
            factory,
            product_family,
            sum_family,
            structural_names,
            runtime_names,
            standard_names,
            ns: ns.to_owned(),
            handle,
        }
    }
}

/// Lower a typed (post-recovery) Kio package to a Haskell package whose
/// package module is `module <ns>`.
pub fn lower_package(package: &Package<Routed>, ns: &str) -> Result<HaskellPackage, EmitError> {
    lower_package_with_signature(package, ns, None)
}

/// Lower a typed Kio package while preserving the compatibility members
/// recovered from its signature changelog.
pub fn lower_package_with_signature(
    package: &Package<Routed>,
    ns: &str,
    sig: Option<&(u32, crate::sig::ReplayedInterface)>,
) -> Result<HaskellPackage, EmitError> {
    package.package_file().ok_or_else(|| {
        EmitError::unsupported(
            "Haskell emitter requires a package file (`<pkg>.pkg.kio`); \
             a package without one has no package boundary to expose",
        )
    })?;

    let names = HaskellNames::derive(ns);
    let prepared =
        PreparedBoundaryCallableSites::collect(package, sig.map(|(_, replayed)| replayed))
            .unwrap_or_else(|error| {
                unreachable!(
                    "post-recovery Haskell emission and replay_sig_for_build-validated retained \
                     interfaces must produce a valid PreparedBoundaryCallableSites catalog: \
                     {error}"
                )
            });
    let shapes = super::skin::HaskellShapes::new_with_prepared(package, &names, &prepared);
    let facade = HaskellFacadeCatalog::new(&prepared, &shapes);

    let pkg_hs = render_pkg_hs(package, &shapes, &facade, &names, &prepared)?;

    Ok(HaskellPackage {
        pkg_hs: finalize_hs_file(pkg_hs),
    })
}

/// Normalize an emitted Haskell file: strip trailing blank lines, end with
/// exactly one newline.
fn finalize_hs_file(mut s: String) -> String {
    while s.ends_with('\n') {
        s.pop();
    }
    s.push('\n');
    s
}

/// Render `<Ns>.hs`: the language pragmas + module header + exports, the
/// `<Handle>Host h m` record, the structural family API, the `<Handle> h m`
/// handle, the `create<Handle>` factory, every export wrapper, and every module
/// fn as a monad-polymorphic top-level function.
fn render_pkg_hs(
    package: &Package<Routed>,
    shapes: &super::skin::HaskellShapes<'_>,
    facade: &HaskellFacadeCatalog,
    names: &HaskellNames,
    prepared: &PreparedBoundaryCallableSites,
) -> Result<String, EmitError> {
    // Collect every module fn as a `(haskell-name, module-key, &FnDef)`
    // triple, in deterministic order.
    let mut fns: Vec<(String, &str, &crate::ast::FnDef<Routed>)> = Vec::new();
    for (module_key, entry) in package.modules() {
        for item in &entry.module.items {
            if let crate::ast::Item::FnDef(f) = item {
                fns.push((module_fn_name(module_key, &f.name), module_key, f));
            }
        }
    }
    fns.sort_by(|a, b| a.0.cmp(&b.0));

    let exports = collect_exports(package, facade, shapes);

    // The export list: create<Handle>, the host record + its accessor, the
    // package handle, the structural family names, and every export wrapper.
    // `mod_*` functions implement the body and are intentionally absent from
    // the facade export list.
    let mut export_list: Vec<String> = vec![
        format!("{}(..)", names.host_types_ty),
        format!("{}(..)", names.host_ty),
        format!("{}(..)", names.handle),
        names.factory.clone(),
        names.product_family.clone(),
        names.sum_family.clone(),
    ];
    for e in &exports {
        export_list.push(e.wrapper_name.clone());
    }

    // The FFI boundary aliases: a stable-named `type` synonym per compound
    // boundary slot (and a per-arm `pattern` synonym for a sum slot), so a
    // host implementation — and the per-backend test runner, which reads no
    // emitted file — can name each family application through the stable
    // semantic-name ABI (`Env_H…` / `Exp_H…`). Flat product and
    // keyed sum patterns hide the right-nested pair / `Either` representation.
    let ffi = render_ffi_aliases(facade, shapes)?;
    for name in &ffi.exported_names {
        export_list.push(name.clone());
    }

    let shape_decls = shapes.render_shape_decls();
    let mut shared_decls = shape_decls.clone();
    shared_decls.push_str(&ffi.decls);
    shared_decls.push_str(&render_package_handle(names));
    shared_decls.push('\n');
    shared_decls.push_str(&render_create_package(names));
    shared_decls.push('\n');
    let mut decls = String::new();

    // Try the native-HKT body first. The universal floor remains valid only
    // when every public host function, exported function, and exported
    // newtype member has an exactly bridgeable boundary. A private construct
    // may make native body emission fail without weakening that public
    // facade, while any erased or otherwise inexact public slot must reject
    // the fallback.
    let mut extra_pragmas: Vec<&str> = Vec::new();
    match super::native::render_native_body(package, names, &ffi.decls, prepared) {
        Ok(native) => {
            for name in &native.export_names {
                export_list.push(name.clone());
            }
            extra_pragmas = native.extra_pragmas;
            decls.push_str(&native.host_record);
            decls.push('\n');
            decls.push_str(&shared_decls);
            decls.push_str(&native.type_decls);
            decls.push_str(&native.module_fns);
            decls.push_str(&native.export_wrappers);
        }
        Err(not_native) => {
            if let Some(blocker) = fallback_facade_blocker(facade, shapes) {
                return Err(EmitError::unsupported(format!(
                    "Haskell native body is required because the universal body cannot preserve the exact public boundary at {blocker}: {}",
                    not_native.reason(),
                )));
            }
            let pkg_name = package
                .package_file()
                .map(|p| p.package_name.as_str())
                .unwrap_or("<package>");
            not_native.trace(pkg_name);
            decls.push_str(&render_host_record(shapes, facade, names)?);
            decls.push('\n');
            decls.push_str(&shared_decls);
            // Fan out the per-module-fn rendering. Each fn body is
            // self-contained over the private universal-carrier model.
            let pieces: Result<Vec<String>, EmitError> = crate::maybe_into_par_iter!(fns.clone())
                .map(|(name, module_key, f)| {
                    render_module_fn(&name, module_key, f, shapes, package, names)
                })
                .collect();
            let mut pieces = pieces?;
            pieces.sort();
            for p in pieces {
                decls.push('\n');
                decls.push_str(&p);
            }

            // Export wrappers (typed boundary entry points).
            let mut export_pieces: Vec<String> = Vec::new();
            for e in &exports {
                export_pieces.push(render_export_wrapper(e, shapes, facade, names)?);
            }
            export_pieces.sort();
            for p in export_pieces {
                decls.push('\n');
                decls.push_str(&p);
            }
        }
    }

    decls.push('\n');
    decls.push_str(&super::runtime_declarations(
        &names.host_types_ty,
        shapes.host_types(),
        &names.runtime_names,
        &names.standard_names,
    ));

    let mut out = String::new();
    out.push_str("-- Generated by kio — do not edit by hand.\n");
    out.push_str("{-# LANGUAGE RankNTypes #-}\n");
    out.push_str("{-# LANGUAGE BangPatterns #-}\n");
    out.push_str("{-# LANGUAGE ScopedTypeVariables #-}\n");
    out.push_str("{-# LANGUAGE FlexibleContexts #-}\n");
    out.push_str("{-# LANGUAGE ConstraintKinds #-}\n");
    out.push_str("{-# LANGUAGE ExplicitNamespaces #-}\n");
    out.push_str("{-# LANGUAGE PackageImports #-}\n");
    out.push_str("{-# LANGUAGE KindSignatures #-}\n");
    out.push_str("{-# LANGUAGE PolyKinds #-}\n");
    out.push_str("{-# LANGUAGE TypeFamilies #-}\n");
    out.push_str("{-# LANGUAGE DataKinds #-}\n");
    out.push_str("{-# LANGUAGE TypeOperators #-}\n");
    // Structural boundary patterns are bidirectional pattern synonyms;
    // enable the extension only when one was emitted.
    if ffi.has_pattern_synonyms {
        out.push_str("{-# LANGUAGE PatternSynonyms #-}\n");
    }
    if ffi.has_rank_n_patterns {
        out.push_str("{-# LANGUAGE ImpredicativeTypes #-}\n");
        out.push_str("{-# LANGUAGE TypeApplications #-}\n");
        out.push_str("{-# LANGUAGE ViewPatterns #-}\n");
    }
    for pragma in &extra_pragmas {
        if ffi.has_rank_n_patterns && *pragma == "ImpredicativeTypes" {
            continue;
        }
        out.push_str(&format!("{{-# LANGUAGE {pragma} #-}}\n"));
    }
    out.push_str(&format!("module {}\n", names.ns));
    out.push_str("  ( ");
    out.push_str(&export_list.join("\n  , "));
    out.push_str("\n  ) where\n");
    let imports = names
        .standard_names
        .imports()
        .into_iter()
        .filter(|(_, _, alias)| decls.contains(&format!("{alias}.")))
        .collect::<Vec<_>>();
    if !imports.is_empty() {
        out.push('\n');
        for (package, module, alias) in imports {
            out.push_str(&format!(
                "import qualified \"{package}\" {module} as {alias}\n"
            ));
        }
    }
    out.push('\n');
    out.push_str(&decls);

    Ok(out)
}

fn fallback_facade_blocker(
    facade: &HaskellFacadeCatalog,
    shapes: &super::skin::HaskellShapes<'_>,
) -> Option<String> {
    // The prepared public-newtype cut is authoritative. Every surface that
    // requires a nominal carrier, including an opaque surface, would lose its
    // host-visible abstraction in the identity fallback wrapper.
    for (name, newtype) in facade.public_newtypes() {
        if newtype.requires_nominal_carrier() {
            return Some(format!(
                "exported newtype `{}/{}`",
                name.module_segments().join("/"),
                name.name()
            ));
        }
    }

    // The realized prepared sites are the sole callable inventory.  The
    // universal body may inspect their exact Haskell slot types, but it must
    // not rediscover which raw declarations are public.
    for site in facade.sites() {
        let entry = site.entry();
        if !entry.is_live() {
            continue;
        }
        let module = site.id().module_segments().join("/");
        let label = fallback_site_label(site.id());
        let mut argument_index = 0usize;
        for stage in entry.stages() {
            match stage {
                HaskellCallableHeadStage::Type(_) => {
                    return Some(format!("{label} (generic binder)"));
                }
                HaskellCallableHeadStage::Value { slots, .. } => {
                    for slot in slots {
                        if !shapes.fallback_bridgeable_in(slot.ty(), Some(&module)) {
                            return Some(format!("{label} argument {argument_index}"));
                        }
                        argument_index += 1;
                    }
                }
            }
        }
        if !shapes.fallback_bridgeable_in(entry.returned().ty(), Some(&module)) {
            return Some(format!("{label} return"));
        }
    }
    None
}

fn fallback_site_label(site: &BoundaryFacadeSiteId) -> String {
    let module = site.module_segments().join("/");
    match site.owner() {
        BoundaryFacadeSiteOwner::HostFunction { name } => {
            format!("host fn `{module}/{name}`")
        }
        BoundaryFacadeSiteOwner::ExportedFunction { name } => {
            format!("export `{module}/{name}`")
        }
        BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, .. }
        | BoundaryFacadeSiteOwner::NewtypeProjector { newtype, .. } => {
            format!("exported newtype `{module}/{newtype}`")
        }
    }
}

#[cfg(all(test, feature = "surface"))]
mod fallback_facade_tests {
    use std::path::{Path, PathBuf};

    use crate::ast::{Expr, HostFnParam, Item, Kind, Meta, Routed, SignatureParam, Type};
    use crate::backends::haskell::naming::{
        BoundaryId, BoundaryRoot, BoundaryStep, HaskellName, ItemId, ModuleItemId, StructuralKey,
        SumPatternId,
    };
    use crate::backends::skin::{FfiDir, SkinProfile};
    use crate::pass::full::FullPipeline;
    use crate::pass::resolve::{Package, PackageFileEntry};
    use crate::pipeline::Pipeline;

    use super::{HaskellNames, fallback_facade_blocker, rank_n_pattern_shape, render_pkg_hs};

    fn module_fn(module: &str, item: &str) -> String {
        HaskellName::ModuleFn(ModuleItemId::new(module, item)).render()
    }

    fn host_field(module: &str, item: &str) -> String {
        HaskellName::HostField(ModuleItemId::new(module, item)).render()
    }

    fn export_wrapper(module: &str, item: &str) -> String {
        HaskellName::ExportWrapper(ItemId::item(module, item)).render()
    }

    fn newtype_export_wrapper(module: &str, newtype: &str, member: &str) -> String {
        HaskellName::ExportWrapper(ItemId::newtype_member(module, newtype, member)).render()
    }

    fn export_boundary(module: &str, item: &str, root: BoundaryRoot) -> BoundaryId {
        BoundaryId::exp(ItemId::item(module, item), root)
    }

    fn member_export_boundary(
        module: &str,
        newtype: &str,
        member: &str,
        root: BoundaryRoot,
    ) -> BoundaryId {
        BoundaryId::exp(ItemId::newtype_member(module, newtype, member), root)
    }

    fn boundary_alias(boundary: &BoundaryId) -> String {
        HaskellName::BoundaryAlias(boundary.clone()).render()
    }

    fn standard_hs(template: &str) -> String {
        let names = crate::backends::haskell::naming::StandardNames::new("FallbackProbe");
        template
            .replace("%KIND%", &names.data_kind)
            .replace("%VOID%", &names.data_void)
    }

    fn visible_parenthesized_type_application<'a>(line: &'a str, marker: &str) -> &'a str {
        let rest = line
            .split_once(marker)
            .unwrap_or_else(|| panic!("missing `{marker}` in `{line}`"))
            .1;
        assert!(
            rest.starts_with('('),
            "unparenthesized type application: {line}"
        );
        let mut depth = 0usize;
        for (index, ch) in rest.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth = depth
                        .checked_sub(1)
                        .unwrap_or_else(|| panic!("unbalanced type application: {line}"));
                    if depth == 0 {
                        return &rest[..=index];
                    }
                }
                _ => {}
            }
        }
        panic!("unterminated type application: {line}")
    }

    fn sum_pattern(boundary: &BoundaryId, key: StructuralKey, index: u32) -> String {
        HaskellName::SumPattern(SumPatternId {
            boundary: boundary.clone(),
            key,
            index,
        })
        .render()
    }

    fn routed_package(source: &str) -> Package<Routed> {
        routed_package_files(&[("api.kio", source)])
    }

    fn routed_package_files(sources: &[(&str, &str)]) -> Package<Routed> {
        routed_package_files_with_package(sources, None)
    }

    fn routed_package_files_with_bridge(
        sources: &[(&str, &str)],
        bridge_items: &str,
    ) -> Package<Routed> {
        let source = format!("package fallback_probe; bridge {{ {bridge_items} }}");
        let package_file = crate::pass::parser::parse_package_file(&source, None)
            .expect("parse fallback-probe package file");
        routed_package_files_with_package(sources, Some(package_file))
    }

    fn routed_package_files_with_package(
        sources: &[(&str, &str)],
        package_file: Option<crate::ast::PackageFile>,
    ) -> Package<Routed> {
        let parsed = sources
            .iter()
            .map(|(path, source)| {
                (
                    PathBuf::from(path),
                    crate::pass::parser::parse(source)
                        .unwrap_or_else(|error| panic!("parse `{path}`: {error:?}")),
                )
            })
            .collect();
        let (modules, package_file) =
            FullPipeline::lower_package(parsed, package_file).expect("lower package");
        let package_file = package_file.map(|package_file| PackageFileEntry {
            file_path: PathBuf::from("fallback_probe.pkg.kio"),
            package_name: "fallback_probe".to_owned(),
            package_file,
        });
        let package = Package::build(Path::new(""), modules, package_file).expect("build package");
        package.resolve_imports().expect("resolve imports");
        package
            .check_in_body_resolution()
            .expect("check in-body resolution");
        let prime = FullPipeline::typecheck(&package).expect("typecheck package");
        crate::pass::recover_to_low::lower(&crate::pass::structural_recovery::recover_package(
            &prime,
        ))
    }

    fn blocker(source: &str) -> Option<String> {
        let package = routed_package_files_with_bridge(&[("api.kio", source)], "**;");
        let names = HaskellNames::derive("FallbackProbe");
        let prepared =
            crate::backends::boundary_facade::PreparedBoundaryCallableSites::collect_live(&package)
                .expect("prepare Haskell blocker facade");
        let shapes = crate::backends::haskell::skin::HaskellShapes::new_with_prepared(
            &package, &names, &prepared,
        );
        let facade =
            crate::backends::haskell::facade::HaskellFacadeCatalog::new(&prepared, &shapes);
        fallback_facade_blocker(&facade, &shapes)
    }

    fn required_nominal_blocker(source: &str, name: &str) -> Option<String> {
        let package = routed_package_files_with_bridge(&[("api.kio", source)], "**;");
        let names = HaskellNames::derive("FallbackProbe");
        let prepared =
            crate::backends::boundary_facade::PreparedBoundaryCallableSites::collect_live(&package)
                .expect("prepare Haskell nominal-blocker facade");
        let shapes = crate::backends::haskell::skin::HaskellShapes::new_with_prepared(
            &package, &names, &prepared,
        );
        let facade =
            crate::backends::haskell::facade::HaskellFacadeCatalog::new(&prepared, &shapes);
        assert!(
            facade
                .public_newtype("api", name)
                .expect("the bridged public newtype is in the prepared inventory")
                .requires_nominal_carrier()
        );
        fallback_facade_blocker(&facade, &shapes)
    }

    fn render(source: &str) -> Result<String, super::EmitError> {
        render_files(&[("api.kio", source)])
    }

    fn render_files(sources: &[(&str, &str)]) -> Result<String, super::EmitError> {
        let package = routed_package_files_with_bridge(sources, "**;");
        render_package(&package)
    }

    fn render_package(package: &Package<Routed>) -> Result<String, super::EmitError> {
        render_package_as(package, "FallbackProbe")
    }

    fn render_package_as(
        package: &Package<Routed>,
        namespace: &str,
    ) -> Result<String, super::EmitError> {
        let names = HaskellNames::derive(namespace);
        let prepared =
            crate::backends::boundary_facade::PreparedBoundaryCallableSites::collect_live(package)
                .expect("prepare Haskell test facade");
        let shapes = crate::backends::haskell::skin::HaskellShapes::new_with_prepared(
            package, &names, &prepared,
        );
        let facade =
            crate::backends::haskell::facade::HaskellFacadeCatalog::new(&prepared, &shapes);
        render_pkg_hs(package, &shapes, &facade, &names, &prepared)
    }

    #[test]
    fn standard_import_alias_survives_a_data_text_facade_and_preserves_literals() {
        let package = routed_package_files_with_bridge(
            &[(
                "api.kio",
                "module api; host type String role(str); \
                 pub fn collision_text() -> String { \"Data.Text.Text\" }",
            )],
            "api;",
        );
        let rendered = render_package_as(&package, "Data.Text").expect("render Data.Text facade");
        let standard_names = crate::backends::haskell::naming::StandardNames::new("Data.Text");

        assert!(rendered.contains("module Data.Text\n"), "{rendered}");
        assert!(
            rendered.contains(&format!(
                "import qualified \"text\" Data.Text as {}",
                standard_names.data_text
            )),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!("{}.Text", standard_names.data_text)),
            "{rendered}"
        );
        assert!(rendered.contains("\"Data.Text.Text\""), "{rendered}");
        assert!(
            !rendered
                .replace("\"Data.Text.Text\"", "")
                .contains("Data.Text.Text"),
            "{rendered}"
        );
    }

    #[test]
    fn live_host_type_provenance_suppresses_a_retained_deprecation_pragma() {
        let package = routed_package_files_with_bridge(
            &[("api.kio", "module api; host type Restored;")],
            "api;",
        );
        let signature = crate::pass::parser::parse_signature_file(
            r#"signature fallback_probe v(3);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Legacy;
        host type Restored;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Legacy;
        Restored;
      }
    }
  }
}
"#,
            None,
        )
        .expect("parse Haskell retained-host-type signature");
        let replayed =
            crate::sig::replay(&signature).expect("replay Haskell retained-host-type signature");
        let emitted =
            super::lower_package_with_signature(&package, "FallbackProbe", Some(&(2, replayed)))
                .expect("emit Haskell retained host types");
        let names = HaskellNames::derive("FallbackProbe");
        let legacy = names.structural_names.host_assoc_name("api", "Legacy");
        let restored = names.structural_names.host_assoc_name("api", "Restored");
        let missing = names.runtime_names.missing_host_type(&restored);

        assert!(
            emitted
                .pkg_hs
                .contains(&format!("{{-# DEPRECATED type {legacy} \"")),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            !emitted
                .pkg_hs
                .contains(&format!("{{-# DEPRECATED type {restored} \"")),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            emitted
                .pkg_hs
                .contains(&format!("type {restored} h = {missing} h")),
            "{}",
            emitted.pkg_hs
        );
        for forbidden in [
            "module FallbackProbe.Runtime",
            "import FallbackProbe.Runtime",
            "import qualified FallbackProbe.Runtime",
        ] {
            assert!(!emitted.pkg_hs.contains(forbidden), "{}", emitted.pkg_hs);
        }
    }

    fn force_native_fallback(package: &mut Package<Routed>) {
        for entry in package.modules_mut() {
            for item in &mut entry.module.items {
                let Item::FnDef(function) = item else {
                    continue;
                };
                if function.name != "force_universal" {
                    continue;
                }
                let value = function.body.clone();
                let meta = value.meta().clone();
                function.body = crate::ast::Expr::EnrichedRecord {
                    occurrence: Default::default(),
                    fields: vec![crate::ast::RecordField {
                        name: "forced".to_owned(),
                        value,
                        meta: meta.clone(),
                    }],
                    synth_ty: function.ret.clone(),
                    meta,
                    ext: (),
                };
                return;
            }
        }
        panic!("forced-fallback fixture has a private `force_universal` function");
    }

    #[test]
    fn projected_rank_n_callees_are_annotated_before_every_visible_type_argument() {
        let package = routed_package(
            "module api; \
             import __intrinsics__; \
             host type Cell; \
             fn specialize_projected(polymorphic: [A] A) -> Cell { \
               let source = __pair__([A] A, ., polymorphic, ()); \
               __fst__([A] A, ., source)(Cell) \
             } \
             fn apply_projected(polymorphic: [A] A -> A, value: Cell) -> Cell { \
               let source = __pair__([A] A -> A, ., polymorphic, ()); \
               __fst__([A] A -> A, ., source)(Cell, value) \
             }",
        );
        let definition = |name: &str| {
            package
                .module("api")
                .expect("api module")
                .module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::FnDef(function) if function.name == name => Some(function),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing projected-application function `{name}`"))
        };
        assert!(matches!(
            &definition("specialize_projected").body,
            Expr::Let { value, body, .. }
                if matches!(value.as_ref(), Expr::EnrichedTuple { .. })
                    && matches!(
                        body.as_ref(),
                        Expr::LowTypeApplication { callee, .. }
                            if matches!(callee.as_ref(), Expr::EnrichedProject { .. })
                    )
        ));
        assert!(matches!(
            &definition("apply_projected").body,
            Expr::Let { value, body, .. }
                if matches!(value.as_ref(), Expr::EnrichedTuple { .. })
                    && matches!(
                        body.as_ref(),
                        Expr::LowIndirectCall {
                            callee,
                            type_args,
                            args,
                            ..
                        } if type_args.len() == 1
                            && args.len() == 1
                            && matches!(callee.as_ref(), Expr::EnrichedProject { .. })
                    )
        ));

        let emitted = render_package(&package).expect("render projected rank-N type application");
        for (function, expected_type) in [
            (
                "specialize_projected",
                standard_hs(":: forall (t_a :: %KIND%.Type). m t_a"),
            ),
            (
                "apply_projected",
                standard_hs(":: forall (t_a :: %KIND%.Type). m (t_a -> m t_a)"),
            ),
        ] {
            let name = module_fn("api", function);
            let equation = emitted
                .lines()
                .find(|line| line.starts_with(&format!("{name} _pkg")))
                .unwrap_or_else(|| panic!("missing `{function}` module equation"));
            let annotation = equation.find(&expected_type).unwrap_or_else(|| {
                panic!("rank-N projected callee lacks its exact type: {equation}")
            });
            let visible_argument = annotation
                + equation[annotation..].find(" @").unwrap_or_else(|| {
                    panic!("type annotation does not precede application: {equation}")
                });
            let projection = equation.find("case (k_source) of").unwrap_or_else(|| {
                panic!("fixture did not emit a raw tuple projection: {equation}")
            });

            assert!(projection < annotation, "{equation}");
            assert!(annotation < visible_argument, "{equation}");
        }
    }

    #[test]
    fn projected_rank_n_boundary_values_are_annotated_before_visible_type_arguments() {
        let emitted = render(
            "module api; \
             pub newtype Pair : . & . { \
               pub constructor mk_pair; \
               pub projector un_pair; \
             }; \
             pub newtype Tuple : . & . { \
               pub constructor mk_tuple; \
               pub projector un_tuple; \
             }; \
             pub newtype Choice : ([B] B -> B) | . { \
               pub constructor mk_choice; \
               pub projector un_choice; \
             }; \
             pub newtype Shadow[Pair] : Pair & ([A] A -> A) & Tuple & Choice { \
               pub constructor mk_shadow; \
               pub projector un_shadow; \
             }; \
             pub fn keep[Pair](value: Pair & ([A] A -> A) & Tuple & Choice) \
               -> Pair & ([A] A -> A) & Tuple & Choice { value }",
        )
        .expect("render projected rank-N boundary value");
        let name = newtype_export_wrapper("api", "Shadow", "un_shadow");
        let signature = emitted
            .lines()
            .find(|line| line.starts_with(&format!("{name} ::")))
            .expect("projector export signature");
        let returned = member_export_boundary("api", "Shadow", "un_shadow", BoundaryRoot::Ret);
        let returned_alias = boundary_alias(&returned);
        assert!(
            signature.contains(&returned_alias),
            "rank-N public signature lost its stable boundary alias: {signature}"
        );

        let equation = emitted
            .lines()
            .find(|line| line.starts_with(&format!("{name} _pkg")))
            .expect("projector export equation");
        let projection = equation
            .find("case (__hp0) of")
            .unwrap_or_else(|| panic!("fixture did not emit a product projection: {equation}"));
        let annotation = projection
            + equation[projection..]
                .find(":: forall")
                .unwrap_or_else(|| panic!("projected boundary operand is unannotated: {equation}"));
        let visible_argument = annotation
            + equation[annotation..].find(" @").unwrap_or_else(|| {
                panic!("source annotation does not precede type application: {equation}")
            });

        assert!(projection < annotation, "{equation}");
        assert!(annotation < visible_argument, "{equation}");
        let concrete = visible_parenthesized_type_application(equation, "= kioPure @");
        assert!(
            concrete.starts_with("(t_pair, (forall"),
            "a scoped type parameter was expanded as the same-named newtype: {concrete}"
        );
        assert!(
            concrete.contains("((), ())"),
            "transparent product path did not expand inside the concrete lift type: {concrete}"
        );
        assert!(
            concrete.contains("Either (forall"),
            "transparent sum path did not expand inside the concrete lift type: {concrete}"
        );
        assert!(
            !concrete.contains(&returned_alias),
            "the local lift kept the stable family alias opaque: {concrete}"
        );

        let keep = export_wrapper("api", "keep");
        let keep_equation = emitted
            .lines()
            .find(|line| line.starts_with(&format!("{keep} _pkg")))
            .expect("ordinary export equation");
        let keep_concrete = visible_parenthesized_type_application(keep_equation, "kioPure @");
        assert!(
            keep_concrete.starts_with("(t_pair, (forall"),
            "ordinary exported result did not use the shared concrete lift: {keep_equation}"
        );

        let choice = newtype_export_wrapper("api", "Choice", "un_choice");
        let choice_equation = emitted
            .lines()
            .find(|line| line.starts_with(&format!("{choice} _pkg")))
            .expect("transparent newtype projector equation");
        let choice_concrete =
            visible_parenthesized_type_application(choice_equation, "= kioPure @");
        assert!(
            choice_concrete.starts_with("(Either (forall"),
            "transparent newtype wrapper did not use the shared concrete lift: {choice_equation}"
        );

        let constructor = newtype_export_wrapper("api", "Shadow", "mk_shadow");
        let constructor_equation = emitted
            .lines()
            .find(|line| line.starts_with(&format!("{constructor} _pkg")))
            .expect("constructor export equation");
        let inward_annotation = constructor_equation.find(":: forall").unwrap_or_else(|| {
            panic!("constructor's inward rank-N operand is unannotated: {constructor_equation}")
        });
        let inward_application = inward_annotation
            + constructor_equation[inward_annotation..]
                .find(" @")
                .unwrap_or_else(|| {
                    panic!(
                        "constructor's inward annotation does not precede type application: {constructor_equation}"
                    )
                });
        assert!(
            inward_annotation < inward_application,
            "{constructor_equation}"
        );
    }

    #[test]
    fn rank_n_prepared_callback_and_host_value_returns_use_exact_lifts() {
        let emitted = render(
            "module api; \
             host fn host_return(value: .) -> ([A] A -> A) & .; \
             pub fn keep_callback(value: (. -> (([A] A -> A) & .))) \
               -> (. -> (([A] A -> A) & .)) { value } \
             fn host_value() -> (. -> (([A] A -> A) & .)) { host_return }",
        )
        .expect("render rank-N prepared callbacks and host-function value");

        let callback = export_wrapper("api", "keep_callback");
        let callback_signature = emitted
            .lines()
            .find(|line| line.starts_with(&format!("{callback} ::")))
            .expect("prepared callback export signature");
        let input_callback_ret = export_boundary("api", "keep_callback", BoundaryRoot::Arg(0))
            .nested(BoundaryStep::CallbackRet);
        let output_callback_ret = export_boundary("api", "keep_callback", BoundaryRoot::Ret)
            .nested(BoundaryStep::CallbackRet);
        for boundary in [&input_callback_ret, &output_callback_ret] {
            let alias = boundary_alias(boundary);
            assert!(
                callback_signature.contains(&alias),
                "prepared callback signature lost `{alias}`: {callback_signature}"
            );
        }
        let callback_equation = emitted
            .lines()
            .find(|line| line.starts_with(&format!("{callback} _pkg")))
            .expect("prepared callback export equation");
        let concrete = standard_hs("(forall (t_a :: %KIND%.Type). m (t_a -> m t_a), ())");
        let inward = visible_parenthesized_type_application(
            callback_equation,
            "(arg0) >>= \\__hfr0 -> kioPure @",
        );
        assert_eq!(inward, concrete, "prepared callback inward lift");
        let outward = visible_parenthesized_type_application(
            callback_equation,
            "((__er) ()) >>= \\__hfr0 -> kioPure @",
        );
        assert_eq!(outward, concrete, "prepared callback outward lift");

        let host_value = module_fn("api", "host_value");
        let host_value_equation = emitted
            .lines()
            .find(|line| line.starts_with(&format!("{host_value} _pkg")))
            .expect("host-function-value module equation");
        let host = host_field("api", "host_return");
        let host_signature = emitted
            .lines()
            .find(|line| line.contains(&format!("{host} ::")))
            .expect("host-function signature");
        let host_return = boundary_alias(&BoundaryId::env("api", "host_return", BoundaryRoot::Ret));
        assert!(
            host_signature.contains(&host_return),
            "host-function signature lost `{host_return}`: {host_signature}"
        );
        let host_marker = format!("({host} (pkgHost _pkg)) >>= \\__hfr -> kioPure @");
        let host_lift = visible_parenthesized_type_application(host_value_equation, &host_marker);
        assert_eq!(host_lift, concrete, "host-function-value return lift");
    }

    #[test]
    fn canonical_newtype_path_is_not_replayed_as_a_source_alias() {
        let package = routed_package_files(&[
            (
                "actual.kio",
                "module actual; pub newtype Box[A] : A { pub constructor mk; pub projector un; };",
            ),
            (
                "wrong.kio",
                "module wrong; pub newtype Box[A] : A & A { pub constructor mk; pub projector un; };",
            ),
            (
                "caller.kio",
                "module caller; import actual as a; import wrong as actual; fn keep[A](x: a.Box(A)) -> a.Box(A) { x }",
            ),
        ]);
        let caller = &package.module("caller").expect("caller module").module;
        let keep = caller
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(definition) if definition.name == "keep" => Some(definition),
                _ => None,
            })
            .expect("keep function");
        let parameter = keep
            .sig
            .params
            .iter()
            .find_map(|param| match param {
                SignatureParam::Value(param) => param.ty.as_ref(),
                SignatureParam::Type(_) => None,
            })
            .expect("keep value parameter");
        let assert_actual_box = |ty: &Type<Routed>| {
            let Type::Path { segments, args, .. } = ty else {
                panic!("expected canonical newtype path, got {ty:?}")
            };
            assert_eq!(
                segments
                    .iter()
                    .map(crate::ast::PathSegment::as_str)
                    .collect::<Vec<_>>(),
                ["actual", "Box"]
            );
            assert_eq!(args.len(), 1);
        };
        assert_actual_box(parameter);
        assert_actual_box(&keep.ret);

        let resolution = crate::backends::haskell::skin::NewtypeResolution::build(&package);
        assert_eq!(
            resolution.key_of(parameter, Some("caller")).as_deref(),
            Some("actual.Box")
        );
        assert_eq!(
            resolution.key_of(&keep.ret, Some("caller")).as_deref(),
            Some("actual.Box")
        );
    }

    #[test]
    fn generic_or_rank_n_public_surface_requires_native_body() {
        assert!(
            blocker("module api; pub fn same[A](x: A) -> A { x }")
                .is_some_and(|item| item.contains("generic binder"))
        );
        assert!(
            blocker("module api; pub fn keep(f: [A] A -> A) -> . { () }")
                .is_some_and(|item| item.contains("argument 0"))
        );
    }

    #[test]
    fn public_foralls_stage_only_when_the_polymorphism_is_first_class() {
        let package = routed_package_files_with_bridge(
            &[(
                "api.kio",
                "module api; \
                 host fn direct[A](value: A) -> A; \
                 host fn nested(callback: [A] A -> A) -> .;",
            )],
            "api;",
        );
        let emitted = render_package(&package).expect("render Haskell");
        let direct = emitted
            .lines()
            .find(|line| line.contains(&host_field("api", "direct")))
            .expect("direct host field");
        assert!(
            direct.contains(&standard_hs(":: forall (t_a :: %KIND%.Type). t_a -> m t_a")),
            "{direct}"
        );
        let nested = emitted
            .lines()
            .find(|line| line.contains(&host_field("api", "nested")))
            .expect("nested host field");
        assert!(
            nested.contains(&standard_hs(
                ":: (forall (t_a :: %KIND%.Type). m (t_a -> m t_a)) -> m ()"
            )),
            "{nested}"
        );
    }

    #[test]
    fn public_host_fields_use_canonical_facade_slots() {
        let package = routed_package_files_with_bridge(
            &[(
                "api.kio",
                "module api; \
                 pub type Unit_alias = .; \
                 host fn product[A][B](pair: A & B) -> .; \
                 host fn split[A][B](left: A, right: B) -> .; \
                 host fn direct_unit() -> .; \
                 host fn substituted_unit(value: Unit_alias) -> .;",
            )],
            "api;",
        );
        let emitted = render_package(&package).expect("render Haskell");
        let field = |name: &str| {
            emitted
                .lines()
                .find(|line| line.contains(&host_field("api", name)))
                .unwrap_or_else(|| panic!("missing `{name}` host field"))
        };

        let product = field("product");
        let split = field("split");
        assert!(
            product.contains(&standard_hs(
                ":: forall (t_a :: %KIND%.Type) (t_b :: %KIND%.Type). t_a -> t_b -> m ()"
            )),
            "{product}"
        );
        assert!(
            split.contains(&standard_hs(
                ":: forall (t_a :: %KIND%.Type) (t_b :: %KIND%.Type). t_a -> t_b -> m ()"
            )),
            "{split}"
        );

        let direct = field("direct_unit");
        let alias = field("substituted_unit");
        assert!(direct.contains(":: m ()"), "{direct}");
        // Transparent aliases are canonicalized before facade planning, so a
        // declaration written through a Unit alias is nullary too. The
        // distinct substituted-Unit rule applies when an existing semantic
        // slot is later instantiated with Unit; the shared planner pins that
        // occurrence and the nested callback adapter consumes its layout.
        assert!(alias.contains(":: m ()"), "{alias}");
    }

    #[test]
    fn universal_function_adapter_uses_canonical_product_and_unit_slots() {
        let package = routed_package("module api;");
        let names = HaskellNames::derive("FallbackProbe");
        let shapes = crate::backends::haskell::skin::HaskellShapes::new(&package, &names);
        let skin = crate::backends::haskell::skin::HaskellSkin {
            shapes: &shapes,
            module: Some("api"),
        };
        let span = crate::span::Span::new(0, 0);
        let unit = || Type::<Routed>::Unit {
            meta: Meta::new(span),
        };
        let function = |param, abi_arity| Type::Function {
            param: Box::new(param),
            ret: Box::new(unit()),
            meta: Meta::new(span),
            abi_arity,
            caps: Default::default(),
        };

        let product = Type::Product {
            left: Box::new(unit()),
            right: Box::new(unit()),
            meta: Meta::new(span),
        };
        let product = skin
            .convert(&function(product, 1), "internal", FfiDir::Out)
            .expect("convert packed product-domain function");
        assert!(product.contains("\\__fp0 __fp1 ->"), "{product}");
        assert!(
            product.contains(&format!(
                "{} (internal) (({} [",
                names.runtime_names.call_function, names.runtime_names.product
            )),
            "{product}"
        );

        let direct_unit = skin
            .convert(&function(unit(), 0), "direct", FfiDir::Out)
            .expect("convert direct-Unit function");
        assert!(!direct_unit.contains("\\__fp"), "{direct_unit}");
        assert!(
            direct_unit.contains(&format!(
                "{} (direct) {}",
                names.runtime_names.call_function, names.runtime_names.unit
            )),
            "{direct_unit}"
        );

        let substituted_unit = skin
            .convert(&function(unit(), 1), "substituted", FfiDir::Out)
            .expect("convert established slot instantiated with Unit");
        assert!(
            substituted_unit.contains("\\__fp0 ->"),
            "{substituted_unit}"
        );
    }

    #[test]
    fn effectful_residual_module_call_uses_rank_n_lift() {
        let emitted = render(
            "module api; \
             fn delay[X](value: X)[A] -> X { value } \
             fn caller() -> . { \
               let source = delay(.[B](left: B, _right: B) -> B { left }); \
               () \
             }",
        )
        .expect("render Haskell");
        let caller = module_fn("api", "caller");
        let body = emitted
            .lines()
            .find(|line| line.starts_with(&format!("{caller} _pkg =")))
            .expect("caller body");

        assert!(
            body.contains(&standard_hs(
                "kioPure @(forall (t_a :: %KIND%.Type). m (forall (t_b :: %KIND%.Type). m ((t_b, t_b) -> m t_b)))"
            )),
            "{body}"
        );
    }

    #[test]
    fn native_wide_closure_projects_from_one_linear_binder_pattern() {
        const WIDTH: usize = 16;
        let params = (0..WIDTH)
            .map(|index| format!("p{index}: I32"))
            .collect::<Vec<_>>()
            .join(", ");
        let args = (0..WIDTH)
            .map(|index| format!("p{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let product = std::iter::repeat_n("I32", WIDTH)
            .collect::<Vec<_>>()
            .join(" & ");
        let source = format!(
            "module api; \
             host type I32 role(i32); \
             fn consume({params}) -> I32 {{ p0 }} \
             pub fn make() -> ({product}) -> I32 {{ \
               .({params}) {{ consume({args}) }} \
             }}"
        );

        let emitted = render(&source).expect("render wide native Haskell closure");
        let make = emitted
            .lines()
            .find(|line| line.starts_with(&format!("{} _pkg =", module_fn("api", "make"))))
            .expect("native make body");

        assert_eq!(make.matches("case").count(), 0, "{make}");
        assert_eq!(make.matches("@(~(__p").count(), 1, "{make}");
        assert!(
            (0..WIDTH).all(|index| make.contains(&format!("__p{index}"))),
            "{make}"
        );
        assert!(make.len() < 4_000, "wide closure grew nonlinearly: {make}");
    }

    #[test]
    fn native_signature_observes_ordered_type_binder_scope() {
        let package = routed_package_files_with_bridge(
            &[(
                "api.kio",
                "module api; \
                 host type T role(str); \
                 pub fn ordered(value: T)[T](later: T) -> T { later }",
            )],
            "api;",
        );
        let emitted = render_package(&package).expect("render Haskell");
        let after_comment = emitted
            .split_once("-- Module fn `ordered`")
            .map(|(_, suffix)| suffix)
            .expect("ordered native function comment");
        let signature = after_comment
            .lines()
            .nth(1)
            .expect("ordered native function signature");
        let later_binder = standard_hs("forall (t_t :: %KIND%.Type).");
        let Some((before_later_binder, after_later_binder)) = signature.split_once(&later_binder)
        else {
            panic!("later T binder must remain at its declared boundary: {signature}")
        };

        assert!(
            before_later_binder.contains("HostType_") && !before_later_binder.contains(" t_t"),
            "the earlier T parameter must remain the exact host type: {signature}"
        );
        assert!(
            after_later_binder.contains("t_t -> t_t"),
            "the later parameter and return must use the later T binder: {signature}"
        );
    }

    #[test]
    fn recursive_newtype_classification_preserves_parametric_carrier_arguments() {
        let package = routed_package(
            "module api; \
             pub newtype Const[A] : . { \
               pub constructor make_const; pub projector read_const; \
             }; \
             pub rec newtype Finite : Const(Finite) { \
               pub constructor make_finite; pub projector read_finite; \
             }; \
             pub newtype Id[A] : A { \
               pub constructor make_id; pub projector read_id; \
             }; \
             pub rec newtype Recursive : Id(.) & Id(Recursive) { \
               pub constructor make_recursive; pub projector read_recursive; \
             };",
        );
        let resolution = crate::backends::haskell::skin::NewtypeResolution::build(&package);
        let declaration = |name: &str| {
            package
                .module("api")
                .and_then(|entry| {
                    entry.module.items.iter().find_map(|item| match item {
                        Item::Newtype(declaration) if declaration.name == name => Some(declaration),
                        _ => None,
                    })
                })
                .unwrap_or_else(|| panic!("missing api.{name}"))
        };
        let is_recursive = |name| {
            crate::backends::haskell::skin::newtype_is_recursive_with_atomic_newtypes(
                declaration(name),
                "api",
                &package,
                &resolution,
                |_, _| false,
            )
        };

        // `Const` is a parametric abstract Haskell carrier, so its phantom
        // argument remains in the host type even though it is absent from the
        // value payload. `Finite` therefore needs a nominal anchor too.
        assert!(is_recursive("Finite"));
        assert!(is_recursive("Recursive"));
    }

    #[test]
    fn public_opaque_newtypes_export_abstract_declaration_stable_carriers() {
        let package = routed_package_files_with_bridge(
            &[(
                "api.kio",
                "module api; \
                 newtype Hidden_payload : . { constructor mk_hidden; projector un_hidden; }; \
                 pub newtype Opaque_a : Hidden_payload { constructor mk_opaque_a; projector un_opaque_a; }; \
                 pub newtype Opaque_b : Hidden_payload { constructor mk_opaque_b; projector un_opaque_b; }; \
                 pub newtype Constructor_only[A] : A { pub constructor make_constructor; projector read_constructor; }; \
                 pub newtype Projector_only[A] : A { constructor make_projector; pub projector read_projector; }; \
                 pub newtype Existential <U> : U { constructor make_existential; pub projector read_existential; }; \
                 pub newtype Transparent_both : . { pub constructor make_transparent; pub projector read_transparent; }; \
                 rec { \
                   pub newtype Outer : Hidden { pub constructor make_outer; pub projector read_outer; }; \
                   pub newtype Hidden : Outer { constructor make_hidden_cycle; projector read_hidden_cycle; }; \
                 } \
                 rec { \
                   pub newtype Constructor_outer : Constructor_hidden { pub constructor make_constructor_outer; pub projector read_constructor_outer; }; \
                   pub newtype Constructor_hidden : Constructor_outer { pub constructor make_constructor_hidden; projector read_constructor_hidden; }; \
                 } \
                 rec { \
                   pub newtype Projector_outer : Projector_hidden { pub constructor make_projector_outer; pub projector read_projector_outer; }; \
                   pub newtype Projector_hidden : Projector_outer { constructor make_projector_hidden; pub projector read_projector_hidden; }; \
                 } \
                 pub rec newtype Direct_recursive : . | Direct_recursive { pub constructor make_direct; pub projector read_direct; }; \
                 pub fn keep_a(value: Opaque_a) -> Opaque_a { value } \
                 pub fn keep_b(value: Opaque_b) -> Opaque_b { value } \
                 host fn echo_outer(value: Outer) -> Outer; \
                 host fn echo_constructor_outer(value: Constructor_outer) -> Constructor_outer; \
                 host fn echo_projector_outer(value: Projector_outer) -> Projector_outer; \
                 host fn echo_direct(value: Direct_recursive) -> Direct_recursive;",
            )],
            "api;",
        );
        let emitted = super::lower_package(&package, "FallbackProbe").expect("emit Haskell");
        let carrier_name = |source_name: &str| {
            let marker = format!("-- Nominal representation of Kio newtype `{source_name}`.");
            emitted
                .pkg_hs
                .split_once(&marker)
                .and_then(|(_, suffix)| suffix.lines().find(|line| line.starts_with("newtype ")))
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or_else(|| panic!("missing nominal carrier for `{source_name}`"))
        };
        let a = carrier_name("Opaque_a");
        let b = carrier_name("Opaque_b");
        let constructor_only = carrier_name("Constructor_only");
        let projector_only = carrier_name("Projector_only");
        let hidden = carrier_name("Hidden");
        let constructor_hidden = carrier_name("Constructor_hidden");
        let projector_hidden = carrier_name("Projector_hidden");
        let direct = carrier_name("Direct_recursive");

        assert!(
            emitted.pkg_hs.contains(&format!("  , {a}\n")),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            emitted.pkg_hs.contains(&format!("  , {b}\n")),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            !emitted.pkg_hs.contains(&format!("{a}(..)")),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            !emitted.pkg_hs.contains(&format!("{b}(..)")),
            "{}",
            emitted.pkg_hs
        );
        for carrier in [
            constructor_only,
            projector_only,
            hidden,
            constructor_hidden,
            projector_hidden,
            direct,
        ] {
            assert!(
                emitted.pkg_hs.contains(&format!("  , {carrier}\n")),
                "{}",
                emitted.pkg_hs
            );
            assert!(
                !emitted.pkg_hs.contains(&format!("{carrier}(..)")),
                "{}",
                emitted.pkg_hs
            );
        }
        let ctor =
            super::export_newtype_wrapper_name("api", "Constructor_only", "make_constructor");
        let hidden_ctor =
            super::export_newtype_wrapper_name("api", "Constructor_only", "read_constructor");
        let projector =
            super::export_newtype_wrapper_name("api", "Projector_only", "read_projector");
        let hidden_projector =
            super::export_newtype_wrapper_name("api", "Projector_only", "make_projector");
        assert!(
            emitted.pkg_hs.contains(&format!("  , {ctor}\n")),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            !emitted.pkg_hs.contains(&format!("  , {hidden_ctor}\n")),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            emitted.pkg_hs.contains(&format!("  , {projector}\n")),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            !emitted
                .pkg_hs
                .contains(&format!("  , {hidden_projector}\n")),
            "{}",
            emitted.pkg_hs
        );
        let existential_marker = "-- Existential carrier `Existential`";
        let existential = emitted
            .pkg_hs
            .split_once(existential_marker)
            .and_then(|(_, suffix)| suffix.lines().find(|line| line.starts_with("data ")))
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("existential public carrier");
        assert!(
            emitted.pkg_hs.contains(&format!("  , {existential}\n"))
                && !emitted.pkg_hs.contains(&format!("{existential}(..)")),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            !emitted
                .pkg_hs
                .contains("-- Nominal representation of Kio newtype `Outer`.")
                && !emitted
                    .pkg_hs
                    .contains("-- Nominal representation of Kio newtype `Constructor_outer`.")
                && !emitted
                    .pkg_hs
                    .contains("-- Nominal representation of Kio newtype `Projector_outer`."),
            "{}",
            emitted.pkg_hs
        );
        for (field, carrier) in [
            (host_field("api", "echo_outer"), hidden),
            (
                host_field("api", "echo_constructor_outer"),
                constructor_hidden,
            ),
            (host_field("api", "echo_projector_outer"), projector_hidden),
            (host_field("api", "echo_direct"), direct),
        ] {
            let signature = emitted
                .pkg_hs
                .lines()
                .find(|line| line.contains(&field))
                .unwrap_or_else(|| panic!("missing host field `{field}`\n{}", emitted.pkg_hs));
            assert!(
                signature.contains(carrier),
                "{signature}\n{}",
                emitted.pkg_hs
            );
        }
    }

    #[test]
    fn same_leaf_public_newtypes_keep_exact_carrier_identities() {
        let package = routed_package_files_with_bridge(
            &[
                (
                    "left.kio",
                    "module left; \
                     pub newtype Token : . { constructor make_token; projector read_token; }; \
                     host fn round_token(value: Token) -> Token;",
                ),
                (
                    "right.kio",
                    "module right; \
                     pub newtype Token : . { constructor make_token; projector read_token; }; \
                     host fn round_token(value: Token) -> Token;",
                ),
            ],
            "left; right;",
        );
        let emitted = super::lower_package(&package, "FallbackProbe").expect("emit Haskell");
        let marker = "-- Nominal representation of Kio newtype `Token`.";
        let carriers = emitted
            .pkg_hs
            .split(marker)
            .skip(1)
            .filter_map(|suffix| suffix.lines().find(|line| line.starts_with("newtype ")))
            .filter_map(|line| line.split_whitespace().nth(1))
            .collect::<Vec<_>>();

        assert_eq!(carriers.len(), 2, "{}", emitted.pkg_hs);
        assert_ne!(carriers[0], carriers[1], "{}", emitted.pkg_hs);
        for carrier in carriers {
            assert!(
                emitted.pkg_hs.contains(&format!("  , {carrier}\n"))
                    && emitted.pkg_hs.matches(carrier).count() > 2,
                "{}",
                emitted.pkg_hs
            );
        }
    }

    #[test]
    fn unbridged_public_signatures_do_not_export_native_carrier_heads() {
        let package = routed_package_files_with_bridge(
            &[
                (
                    "api.kio",
                    "module api; pub fn keep(value: .) -> . { value }",
                ),
                (
                    "hidden.kio",
                    "module hidden; \
                     pub newtype Secret[A] : A { constructor make_secret; projector read_secret; }; \
                     pub fn keep_secret[A](value: Secret(A)) -> Secret(A) { value }",
                ),
            ],
            "api;",
        );
        let emitted = super::lower_package(&package, "FallbackProbe").expect("emit Haskell");
        let marker = "-- Nominal representation of Kio newtype `Secret`.";
        let carrier = emitted
            .pkg_hs
            .split_once(marker)
            .and_then(|(_, suffix)| suffix.lines().find(|line| line.starts_with("newtype ")))
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("unbridged carrier remains available to the package body");
        let export_header = emitted
            .pkg_hs
            .split_once(") where")
            .map(|(header, _)| header)
            .expect("generated Haskell module has an explicit export list");

        assert!(!export_header.contains(carrier), "{}", emitted.pkg_hs);
    }

    #[test]
    fn unbridged_public_values_do_not_enter_the_facade() {
        let package = routed_package_files_with_bridge(
            &[
                (
                    "api.kio",
                    "module api; pub fn keep(value: . & .) -> . & . { value }",
                ),
                (
                    "hidden.kio",
                    "module hidden; \
                     pub fn hidden_pair(value: . & .) -> . & . { value } \
                     pub newtype Hidden_pair : . & . { \
                       pub constructor make_hidden_pair; \
                       pub projector read_hidden_pair; \
                     };",
                ),
            ],
            "api;",
        );
        let emitted = super::lower_package(&package, "FallbackProbe").expect("emit Haskell");
        let export_header = emitted
            .pkg_hs
            .split_once(") where")
            .map(|(header, _)| header)
            .expect("generated Haskell module has an explicit export list");

        let visible_wrapper = export_wrapper("api", "keep");
        let hidden_wrapper = export_wrapper("hidden", "hidden_pair");
        let hidden_constructor =
            super::export_newtype_wrapper_name("hidden", "Hidden_pair", "make_hidden_pair");
        let hidden_projector =
            super::export_newtype_wrapper_name("hidden", "Hidden_pair", "read_hidden_pair");
        let hidden_fn_arg = boundary_alias(&export_boundary(
            "hidden",
            "hidden_pair",
            BoundaryRoot::Arg(0),
        ));
        let hidden_fn_ret =
            boundary_alias(&export_boundary("hidden", "hidden_pair", BoundaryRoot::Ret));
        let hidden_constructor_arg = boundary_alias(&member_export_boundary(
            "hidden",
            "Hidden_pair",
            "make_hidden_pair",
            BoundaryRoot::Arg(0),
        ));
        let hidden_projector_ret = boundary_alias(&member_export_boundary(
            "hidden",
            "Hidden_pair",
            "read_hidden_pair",
            BoundaryRoot::Ret,
        ));

        assert!(
            export_header.contains(&visible_wrapper),
            "{}",
            emitted.pkg_hs
        );
        for hidden in [
            hidden_wrapper,
            hidden_constructor,
            hidden_projector,
            hidden_fn_arg,
            hidden_fn_ret,
            hidden_constructor_arg,
            hidden_projector_ret,
        ] {
            assert!(
                !emitted.pkg_hs.contains(&hidden),
                "unbridged facade declaration `{hidden}` was emitted:\n{}",
                emitted.pkg_hs
            );
        }
        assert!(
            emitted.pkg_hs.contains(&module_fn("hidden", "hidden_pair")),
            "unbridged definitions must remain available to the package body:\n{}",
            emitted.pkg_hs
        );
    }

    #[test]
    fn unbridged_public_values_do_not_block_the_universal_fallback() {
        let package = routed_package_files_with_bridge(
            &[
                (
                    "api.kio",
                    "module api; pub fn keep(value: .) -> . { value }",
                ),
                (
                    "hidden.kio",
                    "module hidden; pub fn hidden_generic[A](value: A) -> A { value }",
                ),
            ],
            "api;",
        );
        let names = HaskellNames::derive("FallbackProbe");
        let prepared =
            crate::backends::boundary_facade::PreparedBoundaryCallableSites::collect_live(&package)
                .expect("prepare selected Haskell facade");
        let shapes = crate::backends::haskell::skin::HaskellShapes::new_with_prepared(
            &package, &names, &prepared,
        );
        let facade =
            crate::backends::haskell::facade::HaskellFacadeCatalog::new(&prepared, &shapes);

        assert_eq!(fallback_facade_blocker(&facade, &shapes), None);
    }

    #[test]
    fn private_generic_body_does_not_poison_exact_public_facade() {
        assert_eq!(
            blocker(
                "module api; \
                 fn private_same[A](x: A) -> A { x } \
                 pub fn public_pair(x: . & .) -> . & . { x }",
            ),
            None,
        );
    }

    #[test]
    fn nested_closure_type_binder_survives_newtype_result_reconstruction() {
        let output = render(
            "module api; \
             newtype T : . { constructor mk_t; projector un_t; }; \
             pub newtype Box[A] : A { pub constructor mk_box; pub projector un_box; }; \
             pub fn box_value() -> [T] T -> Box(T) { \
                 .[T](value) { Box.mk_box(T, value) } \
             }",
        )
        .expect("render nested polymorphic closure");
        let box_value = module_fn("api", "box_value");
        let body = output
            .lines()
            .find(|line| line.starts_with(&format!("{box_value} _pkg =")))
            .expect("box_value native body");

        assert!(!body.contains(":: m"), "{body}");
    }

    #[test]
    fn adding_an_unrelated_same_leaf_alias_cannot_change_a_path_kind() {
        let baseline = routed_package("module api;");
        let extended = routed_package_files(&[
            ("api.kio", "module api;"),
            ("decoy.kio", "module decoy; pub type Shared[*F][A] = F(A);"),
        ]);
        let names = HaskellNames::derive("FallbackProbe");
        let span = crate::span::Span::new(0, 0);
        let ty = Type::synth_path(
            vec!["Shared".to_owned()],
            vec![
                Type::synth_path(vec!["Left".to_owned()], Vec::new(), span),
                Type::synth_path(vec!["Right".to_owned()], Vec::new(), span),
            ],
            span,
        );
        let expected = Some(Kind::arrow_chain(2));

        for package in [&baseline, &extended] {
            let shapes = crate::backends::haskell::skin::HaskellShapes::new(package, &names);
            assert_eq!(shapes.path_kind_scoped_in(&ty, &[], Some("api")), expected);
        }
    }

    #[test]
    fn rank_n_pattern_normalization_preserves_source_type_equalities() {
        let package = routed_package(
            "module api; \
             host fn repeated[A](value: A & A & ([B] B -> B)) -> .; \
             host fn outer_result[A](value: A & ([B] B -> A)) -> .;",
        );
        let names = HaskellNames::derive("FallbackProbe");
        let shapes = crate::backends::haskell::skin::HaskellShapes::new(&package, &names);

        let shape = |name: &str| {
            let declaration = package
                .modules()
                .flat_map(|(_, entry)| &entry.module.items)
                .find_map(|item| match item {
                    Item::HostFn(declaration) if declaration.name == name => Some(declaration),
                    _ => None,
                })
                .expect("host function declaration");
            let scope = declaration
                .params
                .iter()
                .filter_map(|param| match param {
                    HostFnParam::Type(param) => Some(param.clone()),
                    HostFnParam::Value(_) => None,
                })
                .collect::<Vec<_>>();
            let value = declaration
                .params
                .iter()
                .find_map(|param| match param {
                    HostFnParam::Value(param) => Some(&param.ty),
                    HostFnParam::Type(_) => None,
                })
                .expect("host function value parameter");
            let slots = Type::right_spine_product(value);
            rank_n_pattern_shape(&slots, &scope, Some("api"), &shapes)
                .expect("normalize rank-N pattern shape")
                .expect("test shape contains a rank-N slot")
        };

        let repeated = shape("repeated");
        assert_eq!(repeated.slots[0], repeated.slots[1]);

        let outer_result = shape("outer_result");
        assert!(
            outer_result.slots[1].contains(&outer_result.slots[0]),
            "{:?}",
            outer_result.slots
        );
    }

    #[test]
    fn rank_n_pattern_normalization_expands_exact_boundary_representations() {
        let package = routed_package_files(&[
            (
                "api.kio",
                "module api; \
                 pub type Id = [A] (A) -> A; \
                 pub newtype Wrapped_id : [A] (A) -> A { constructor mk; projector un; }; \
                 host fn alias_product(value: Id & ([A] (A) -> A)) -> .; \
                 host fn alias_sum(value: Id | ([A] (A) -> A)) -> .; \
                 host fn newtype_product(value: Wrapped_id & ([A] (A) -> A)) -> .; \
                 host fn newtype_sum(value: Wrapped_id | ([A] (A) -> A)) -> .;",
            ),
            (
                "decoy.kio",
                "module decoy; type Id = .; newtype Wrapped_id : . { constructor mk; projector un; };",
            ),
        ]);
        let names = HaskellNames::derive("FallbackProbe");
        let shapes = crate::backends::haskell::skin::HaskellShapes::new(&package, &names);

        for function_name in [
            "alias_product",
            "alias_sum",
            "newtype_product",
            "newtype_sum",
        ] {
            let function = package
                .module("api")
                .expect("api module")
                .module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::HostFn(function) if function.name == function_name => Some(function),
                    _ => None,
                })
                .expect("host function");
            let value = function
                .params
                .iter()
                .find_map(|param| match param {
                    HostFnParam::Value(param) => Some(&param.ty),
                    HostFnParam::Type(_) => None,
                })
                .expect("host function value parameter");
            let slots = if function_name.ends_with("product") {
                Type::right_spine_product(value)
            } else {
                Type::right_spine_sum(value)
            };
            let rendered = slots
                .iter()
                .map(|slot| {
                    let normalized =
                        super::normalize_rank_n_boundary_type(slot, &[], Some("api"), &shapes)
                            .expect("normalize exact boundary representation");
                    super::RankNPatternRenderer::new(&[], Some("api"), &shapes)
                        .render(&normalized)
                        .expect("render normalized boundary representation")
                })
                .collect::<Vec<_>>();
            assert_eq!(rendered[0], rendered[1], "{function_name}");
            assert_eq!(
                rendered[0],
                standard_hs("forall (rq0 :: %KIND%.Type). rp0 (rq0 -> rp0 rq0)"),
                "{function_name}"
            );
            rank_n_pattern_shape(&slots, &[], Some("api"), &shapes)
                .expect("normalize rank-N boundary shape")
                .expect("expanded alias or newtype contains forall");
        }
    }

    #[test]
    fn source_type_binders_do_not_collide_with_fixed_haskell_binders() {
        fn tokens(text: &str) -> Vec<&str> {
            text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '\''))
                .filter(|token| !token.is_empty())
                .collect()
        }

        fn count_token(text: &str, expected: &str) -> usize {
            tokens(text)
                .into_iter()
                .filter(|token| *token == expected)
                .count()
        }

        let package = routed_package_files_with_bridge(
            &[(
                "api.kio",
                "module api; \
                 pub newtype Carrier[H][M] : H & M { \
                   pub constructor make_carrier; pub projector read_carrier; \
                 }; \
                 pub newtype Packed[R] <Rp0> : R & Rp0 { \
                   constructor make_packed; pub projector read_packed; \
                 }; \
                 pub fn rank[Rq0](value: Rq0 & ([X] X -> X)) \
                   -> Rq0 & ([X] X -> X) { value }",
            )],
            "**;",
        );
        let output = render_package(&package).expect("render binder-collision package");

        let carrier = crate::backends::haskell::skin::nominal_haskell_type_name(
            "KioCarrier",
            "api",
            "Carrier",
        );
        let carrier_decl = output
            .lines()
            .find(|line| line.starts_with(&format!("newtype {carrier} ")))
            .expect("generic carrier declaration");
        let (carrier_head, carrier_body) = carrier_decl
            .split_once(" = ")
            .expect("carrier declaration has a body");
        for fixed in ["h", "m"] {
            assert!(tokens(carrier_head).contains(&fixed), "{carrier_decl}");
        }
        for source in ["t_h", "t_m"] {
            assert!(tokens(carrier_head).contains(&source), "{carrier_decl}");
            assert!(tokens(carrier_body).contains(&source), "{carrier_decl}");
        }

        let existential = crate::backends::haskell::skin::nominal_haskell_type_name(
            "KioExistential",
            "api",
            "Packed",
        );
        let existential_decl = output
            .lines()
            .find(|line| line.starts_with(&format!("data {existential} ")))
            .expect("existential carrier declaration");
        let (existential_head, existential_body) = existential_decl
            .split_once(" = ")
            .expect("existential declaration has a body");
        for fixed in ["h", "m"] {
            assert!(
                tokens(existential_head).contains(&fixed),
                "{existential_decl}"
            );
        }
        assert!(
            tokens(existential_head).contains(&"t_r"),
            "{existential_decl}"
        );
        assert!(
            tokens(existential_body).contains(&"t_r"),
            "{existential_decl}"
        );
        assert_eq!(
            count_token(existential_body, "t_rp0"),
            2,
            "the existential source binder must be declared and referenced unchanged: {existential_decl}"
        );
        let projector_sig = output
            .lines()
            .find(|line| {
                line.contains(&existential)
                    && line.contains(" :: forall ")
                    && line.contains(&standard_hs("(r :: %KIND%.Type)"))
            })
            .expect("existential CPS projector signature");
        for binder in ["h", "m", "r", "t_r", "t_rp0"] {
            assert!(tokens(projector_sig).contains(&binder), "{projector_sig}");
        }
        assert!(count_token(projector_sig, "r") >= 2, "{projector_sig}");
        assert!(count_token(projector_sig, "t_r") >= 2, "{projector_sig}");
        assert!(count_token(projector_sig, "t_rp0") >= 2, "{projector_sig}");

        let names = HaskellNames::derive("FallbackProbe");
        let prepared =
            crate::backends::boundary_facade::PreparedBoundaryCallableSites::collect_live(&package)
                .expect("prepare binder-collision facade");
        let shapes = crate::backends::haskell::skin::HaskellShapes::new_with_prepared(
            &package, &names, &prepared,
        );
        let function = package
            .module("api")
            .expect("api module")
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(function) if function.name == "rank" => Some(function),
                _ => None,
            })
            .expect("rank function");
        let scope = function
            .sig
            .params
            .iter()
            .filter_map(|param| match param {
                SignatureParam::Type(param) => Some(param.clone()),
                SignatureParam::Value(_) => None,
            })
            .collect::<Vec<_>>();
        let value = function
            .sig
            .params
            .iter()
            .find_map(|param| match param {
                SignatureParam::Value(param) => param.ty.as_ref(),
                SignatureParam::Type(_) => None,
            })
            .expect("rank value parameter type");
        let slots = Type::right_spine_product(value);
        let shape = rank_n_pattern_shape(&slots, &scope, Some("api"), &shapes)
            .expect("normalize rank-N product")
            .expect("rank product has a rank-N slot");
        assert_eq!(
            shape
                .binders
                .iter()
                .map(|binder| binder.name.as_str())
                .collect::<Vec<_>>(),
            ["rp0", "rp1"]
        );
        assert_eq!(shape.slots[0], "rp0");
        assert!(
            tokens(&shape.slots[1]).contains(&"rq0"),
            "{:?}",
            shape.slots
        );
        assert!(
            count_token(&shape.slots[1], "rq0") >= 3,
            "{:?}",
            shape.slots
        );

        let wrapper = export_wrapper("api", "rank");
        let wrapper_sig = output
            .lines()
            .find(|line| line.starts_with(&format!("{wrapper} ::")))
            .unwrap_or_else(|| panic!("rank export signature `{wrapper}`:\n{output}"));
        for binder in ["h", "m", "t_rq0"] {
            assert!(tokens(wrapper_sig).contains(&binder), "{wrapper_sig}");
        }
        assert!(count_token(wrapper_sig, "t_rq0") >= 2, "{wrapper_sig}");

        let pattern = shapes
            .structural_names()
            .render(&HaskellName::ProductPattern(export_boundary(
                "api",
                "rank",
                BoundaryRoot::Ret,
            )));
        let pattern_sig = output
            .lines()
            .find(|line| line.starts_with(&format!("pattern {pattern} ::")))
            .unwrap_or_else(|| panic!("rank-N product pattern signature `{pattern}`:\n{output}"));
        for generated in ["rp0", "rp1", "rq0"] {
            assert!(tokens(pattern_sig).contains(&generated), "{pattern_sig}");
        }
    }

    #[test]
    fn scoped_higher_kinded_application_is_always_boundary_identity() {
        let package = routed_package_files(&[
            (
                "api.kio",
                "module api; \
                 newtype F[A] : A { constructor mk; projector un; }; \
                 host fn keep[*F][A][B](value: F(A & B)) -> F(A & B);",
            ),
            ("decoy.kio", "module decoy; type F[A] = A;"),
        ]);
        let names = HaskellNames::derive("FallbackProbe");
        let shapes = crate::backends::haskell::skin::HaskellShapes::new(&package, &names);
        let function = package
            .module("api")
            .expect("api module")
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::HostFn(function) if function.name == "keep" => Some(function),
                _ => None,
            })
            .expect("host function");
        let scope = function
            .params
            .iter()
            .filter_map(|param| match param {
                HostFnParam::Type(param) => Some(param.clone()),
                HostFnParam::Value(_) => None,
            })
            .collect::<Vec<_>>();
        let value = function
            .params
            .iter()
            .find_map(|param| match param {
                HostFnParam::Value(param) => Some(&param.ty),
                HostFnParam::Type(_) => None,
            })
            .expect("host function value parameter");

        assert!(shapes.is_passthrough_scoped_in(value, &scope, Some("api")));
        let rendered = shapes
            .boundary_haskell_type_scoped_in(value, &scope, Some("api"))
            .expect("render scoped HKT");
        assert_eq!(
            rendered, "t_f (FallbackProbeProduct '[t_a, t_b])",
            "scoped constructor was reinterpreted"
        );
    }

    #[test]
    fn native_and_universal_renderers_publish_the_same_structural_facade() {
        const PUBLIC_FACADE: &str = "\
            host fn exchange(value: (. & .) | .) -> (. & .) | .; \
            pub fn roundtrip(value: (. & .) | .) -> (. & .) | . { exchange(value) } \
            pub newtype Pair : . & . { \
                pub constructor mk_pair; pub projector un_pair; \
            };";

        let native_source = format!("module api; {PUBLIC_FACADE}");
        let native_package =
            routed_package_files_with_bridge(&[("api.kio", &native_source)], "api;");
        let names = HaskellNames::derive("FallbackProbe");
        let native = render_package(&native_package).expect("native structural facade");
        let native_body = module_fn("api", "roundtrip");
        let native_signature = native
            .lines()
            .find(|line| line.starts_with(&format!("{native_body} ::")))
            .expect("native module-function signature");
        assert!(
            !native_signature.contains(&names.runtime_names.opaque),
            "{native_signature}"
        );

        let prepared =
            crate::backends::boundary_facade::PreparedBoundaryCallableSites::collect_live(
                &native_package,
            )
            .expect("prepare universal host facade");
        let shapes = crate::backends::haskell::skin::HaskellShapes::new_with_prepared(
            &native_package,
            &names,
            &prepared,
        );
        let facade =
            crate::backends::haskell::facade::HaskellFacadeCatalog::new(&prepared, &shapes);
        let mut universal =
            super::render_host_record(&shapes, &facade, &names).expect("universal host facade");
        for export in super::collect_exports(&native_package, &facade, &shapes) {
            universal.push_str(
                &super::render_export_wrapper(&export, &shapes, &facade, &names)
                    .expect("universal export facade"),
            );
        }

        for name in [
            host_field("api", "exchange"),
            export_wrapper("api", "roundtrip"),
            super::export_newtype_wrapper_name("api", "Pair", "mk_pair"),
            super::export_newtype_wrapper_name("api", "Pair", "un_pair"),
        ] {
            let signature = |output: &str| {
                let needle = format!("{name} ::");
                output
                    .lines()
                    .find_map(|line| line.find(&needle).map(|start| &line[start..]))
                    .unwrap_or_else(|| panic!("missing `{name}` signature"))
                    .to_owned()
            };
            assert_eq!(signature(&native), signature(&universal), "{name}");
        }
    }

    #[test]
    fn universal_renderer_resolves_scoped_module_function_import() {
        let package = routed_package_files(&[
            (
                "api/internal.kio",
                "module api/internal; pub(api) fn message() -> . { () }",
            ),
            (
                "api/main.kio",
                "module api/main; import api/internal(message); \
                 pub fn run() -> . { message() }",
            ),
        ]);
        let names = HaskellNames::derive("FallbackProbe");
        let shapes = crate::backends::haskell::skin::HaskellShapes::new(&package, &names);
        let function = package
            .module("api/main")
            .expect("importing module")
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(function) if function.name == "run" => Some(function),
                _ => None,
            })
            .expect("importing function");

        let rendered = super::render_module_fn(
            &module_fn("api/main", "run"),
            "api/main",
            function,
            &shapes,
            &package,
            &names,
        )
        .expect("universal module function");
        let provider = module_fn("api/internal", "message");
        let importer = module_fn("api/main", "message");
        assert!(rendered.contains(&provider), "{rendered}");
        assert!(!rendered.contains(&importer), "{rendered}");
    }

    #[test]
    fn unrelated_declaration_cannot_rename_structural_facade_entries() {
        let render_ffi = |source: &str| {
            let package = routed_package_files_with_bridge(&[("api.kio", source)], "api;");
            let names = HaskellNames::derive("FallbackProbe");
            let shapes = crate::backends::haskell::skin::HaskellShapes::new(&package, &names);
            let prepared =
                crate::backends::boundary_facade::PreparedBoundaryCallableSites::collect_live(
                    &package,
                )
                .expect("prepare structural facade");
            let facade =
                crate::backends::haskell::facade::HaskellFacadeCatalog::new(&prepared, &shapes);
            super::render_ffi_aliases(&facade, &shapes)
                .expect("structural facade")
                .decls
        };
        let boundary = "host fn exchange(value: . & .) -> . & .;";
        let with_unrelated = "host fn exchange(value: . & .) -> . & .; \
            newtype Other[A] : A { constructor mk; projector un; };";
        assert_eq!(
            render_ffi(&format!("module api; {boundary}")),
            render_ffi(&format!("module api; {with_unrelated}")),
        );
    }

    #[test]
    fn bottom_boundary_is_void_in_native_and_universal_facades() {
        let native_source = "module api; \
            import __intrinsics__; \
            host fn stop(value: .) -> !; \
            host fn discard(value: !) -> .; \
            host fn inspect(value: . & !) -> .; \
            pub newtype Never : ! { pub constructor mk; pub projector un; }; \
            pub fn stop_public(value: .) -> ! { stop(value) } \
            pub fn discard_public(value: !) -> . { discard(value) } \
            pub fn absurd_public(value: !) -> . { __absurd__(., value) }";
        let native_package =
            routed_package_files_with_bridge(&[("api.kio", native_source)], "api;");
        let native = render_package(&native_package).expect("native Bottom boundary");

        assert!(
            native.contains(&standard_hs(
                "import qualified \"base\" Data.Void as %VOID%\n"
            )),
            "{native}"
        );
        assert!(
            native.contains(&standard_hs(&format!(
                "{} :: m %VOID%.Void",
                host_field("api", "stop")
            ))),
            "{native}"
        );
        assert!(
            native.contains(&standard_hs(&format!(
                "{} :: %VOID%.Void -> m ()",
                host_field("api", "discard")
            ))),
            "{native}"
        );
        assert!(native.contains(&standard_hs("%VOID%.absurd")), "{native}");
        assert!(!native.contains("kio: bottom boundary reached"), "{native}");

        let without_bottom_package = routed_package_files_with_bridge(
            &[(
                "api.kio",
                "module api; pub fn public_unit(value: .) -> . { value }",
            )],
            "api;",
        );
        let without_bottom = render_package(&without_bottom_package).expect("unit-only facade");
        assert!(without_bottom.contains(&standard_hs(
            "import qualified \"base\" Data.Void as %VOID%"
        )));

        let names = HaskellNames::derive("FallbackProbe");
        let prepared =
            crate::backends::boundary_facade::PreparedBoundaryCallableSites::collect_live(
                &native_package,
            )
            .expect("prepare universal Bottom facade");
        let shapes = crate::backends::haskell::skin::HaskellShapes::new_with_prepared(
            &native_package,
            &names,
            &prepared,
        );
        let facade =
            crate::backends::haskell::facade::HaskellFacadeCatalog::new(&prepared, &shapes);
        let mut universal = super::render_host_record(&shapes, &facade, &names)
            .expect("universal Bottom host facade");
        for export in super::collect_exports(&native_package, &facade, &shapes) {
            universal.push_str(
                &super::render_export_wrapper(&export, &shapes, &facade, &names)
                    .expect("universal Bottom export facade"),
            );
        }
        assert!(
            universal.contains(&standard_hs(&format!(
                "{} :: m %VOID%.Void",
                host_field("api", "stop")
            ))),
            "{universal}"
        );
        assert!(
            universal.contains(&standard_hs("%VOID%.absurd")),
            "{universal}"
        );
        let stop_public = export_wrapper("api", "stop_public");
        let wrapper = universal
            .split(&format!("{stop_public} ::"))
            .nth(1)
            .expect("stop export wrapper");
        let effect = wrapper.find("__f0 <-").expect("effectful package call");
        let unreachable = wrapper
            .find("kio: bottom boundary reached")
            .expect("explicit impossible universal conversion");
        assert!(effect < unreachable, "{wrapper}");
    }

    #[test]
    fn user_comptime_named_types_keep_lexical_identity() {
        let package = routed_package_files_with_bridge(
            &[(
                "api.kio",
                "module api; \
                 pub newtype Comptime_bool[A] : A { constructor make_bool; projector read_bool; }; \
                 pub newtype Comptime_str : . { pub constructor make_str; pub projector read_str; }; \
                 pub fn keep_bool[A](value: Comptime_bool(A)) -> Comptime_bool(A) { value } \
                 pub fn keep_str(value: Comptime_str) -> Comptime_str { value } \
                 pub fn keep_binder[Comptime_bool](value: Comptime_bool) -> Comptime_bool { value }",
            )],
            "api;",
        );
        let emitted = super::lower_package(&package, "FallbackProbe").expect("emit Haskell");
        let carrier = crate::backends::haskell::skin::nominal_haskell_type_name(
            "KioCarrier",
            "api",
            "Comptime_bool",
        );
        let bool_export = export_wrapper("api", "keep_bool");
        let str_export = export_wrapper("api", "keep_str");
        let binder_export = export_wrapper("api", "keep_binder");

        assert!(
            emitted.pkg_hs.contains(&format!("newtype {carrier} ")),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            emitted.pkg_hs.contains(&standard_hs(&format!(
                "{bool_export} :: forall (h :: %KIND%.Type) \
                 (m :: %KIND%.Type -> %KIND%.Type) (t_a :: %KIND%.Type)."
            ))),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            emitted.pkg_hs.contains(&format!("({carrier} h m t_a)")),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            emitted.pkg_hs.contains(&standard_hs(&format!(
                "{str_export} :: forall (h :: %KIND%.Type)"
            ))),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            emitted.pkg_hs.contains(&format!("{str_export} _pkg arg0"))
                && emitted.pkg_hs.contains("-> () -> m ()"),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            emitted.pkg_hs.contains(&standard_hs(&format!(
                "{binder_export} :: forall (h :: %KIND%.Type) \
                 (m :: %KIND%.Type -> %KIND%.Type) \
                 (t_comptimeBool :: %KIND%.Type)."
            ))),
            "{}",
            emitted.pkg_hs
        );
        assert!(
            emitted
                .pkg_hs
                .contains("FallbackProbe h m -> t_comptimeBool -> m t_comptimeBool"),
            "{}",
            emitted.pkg_hs
        );
    }

    #[test]
    fn generic_public_facade_blocks_the_universal_route() {
        assert_eq!(
            blocker("module api; pub fn public_same[A](value: A) -> A { value }"),
            Some("export `api/public_same` (generic binder)".to_owned())
        );
    }

    #[test]
    fn every_required_public_nominal_blocks_the_fallback() {
        for (source, name) in [
            (
                "module api; pub newtype Secret : . { constructor mk; projector un; };",
                "Secret",
            ),
            (
                "module api; pub newtype Box[A] : A { constructor mk; projector un; };",
                "Box",
            ),
            (
                "module api; pub newtype Pack <A> : A { constructor mk; projector un; };",
                "Pack",
            ),
            (
                "module api; pub rec newtype Tree : (. | (Tree)) { constructor mk; projector un; };",
                "Tree",
            ),
        ] {
            assert_eq!(
                required_nominal_blocker(source, name),
                Some(format!("exported newtype `api/{name}`"))
            );
        }
    }

    #[test]
    fn public_nominal_members_block_an_inexact_fallback() {
        for (source, name) in [
            (
                "module api; pub newtype Input : . { pub constructor mk; projector un; };",
                "Input",
            ),
            (
                "module api; pub newtype Output : . { constructor mk; pub projector un; };",
                "Output",
            ),
            (
                "module api; pub newtype Box[A] : A { pub constructor mk; projector un; };",
                "Box",
            ),
            (
                "module api; pub newtype Pack <A> : A { constructor mk; pub projector un; };",
                "Pack",
            ),
            (
                "module api; pub rec newtype Tree : (. | (Tree)) { pub constructor mk; projector un; };",
                "Tree",
            ),
        ] {
            assert_eq!(
                blocker(source),
                Some(format!("exported newtype `api/{name}`"))
            );
        }
        assert_eq!(
            blocker("module api; pub newtype Token : . { pub constructor mk; pub projector un; };"),
            None,
        );
    }

    #[test]
    fn forced_fallback_preserves_every_required_public_newtype_abstraction() {
        for (surface, declaration) in [
            (
                "opaque",
                "pub newtype Secret : . { constructor mk_secret; projector un_secret; };",
            ),
            (
                "constructor-only",
                "pub newtype Input : . { pub constructor mk_input; projector un_input; };",
            ),
            (
                "projector-only",
                "pub newtype Output : . { constructor mk_output; pub projector un_output; };",
            ),
        ] {
            // Inject an EnrichedRecord only after the typed Routed package is
            // complete.  The node is a supported universal-floor value but
            // deliberately lies outside the native renderer, giving this
            // guard test a deterministic forced-fallback cause independent of
            // ordinary source lowering.  The unrelated public nominal newtype
            // must reject fallback rather than acquire an identity wrapper
            // over its Unit payload.
            let source = format!(
                "module api; {declaration} \
                 fn force_universal() -> . {{ () }}"
            );
            let mut package = routed_package_files_with_bridge(&[("api.kio", &source)], "**;");
            force_native_fallback(&mut package);
            let error =
                render_package(&package).expect_err("nominal public surface rejects fallback");
            assert!(
                error
                    .message
                    .contains("universal body cannot preserve the exact public boundary"),
                "{surface}: {}",
                error.message
            );
            assert!(
                error.message.contains("exported newtype `api/"),
                "{surface}: {}",
                error.message
            );
            assert!(
                error.message.contains("EnrichedRecord"),
                "{surface}: {}",
                error.message
            );
        }
    }

    #[test]
    fn recursive_comptime_payload_keeps_its_structural_boundary() {
        let output = render(
            "module api; \
             import __comptime__; \
             pub rec newtype Type_list : . | (__Type__ & Type_list) { \
               pub constructor mk; pub projector un; \
             };",
        )
        .expect("recursive comptime payload has a structural Haskell boundary");

        let constructor = member_export_boundary("api", "Type_list", "mk", BoundaryRoot::Arg(0));
        let projector = member_export_boundary("api", "Type_list", "un", BoundaryRoot::Ret);
        let constructor_alias = boundary_alias(&constructor);
        let projector_alias = boundary_alias(&projector);
        let constructor_product = constructor.nested(BoundaryStep::Slot(1));
        let projector_product = projector.nested(BoundaryStep::Slot(1));
        let constructor_product_alias = boundary_alias(&constructor_product);
        let projector_product_alias = boundary_alias(&projector_product);

        assert!(
            output.contains(&standard_hs(&format!(
                "type {constructor_alias} (h :: %KIND%.Type) (m :: %KIND%.Type -> %KIND%.Type) = FallbackProbeSum '[(), ({constructor_product_alias} h m)]"
            ))),
            "{output}"
        );
        assert!(
            output.contains(&standard_hs(&format!(
                "type {projector_alias} (h :: %KIND%.Type) (m :: %KIND%.Type -> %KIND%.Type) = FallbackProbeSum '[(), ({projector_product_alias} h m)]"
            ))),
            "{output}"
        );
        for product_alias in [&constructor_product_alias, &projector_product_alias] {
            assert!(
                output.contains(&standard_hs(&format!(
                    "type {product_alias} (h :: %KIND%.Type) (m :: %KIND%.Type -> %KIND%.Type) = FallbackProbeProduct '[%VOID%.Void,"
                ))),
                "{output}"
            );
        }
        let unit_pattern = sum_pattern(&constructor, StructuralKey::Positional(0), 0);
        let product_pattern = sum_pattern(&constructor, StructuralKey::Positional(1), 1);
        assert!(
            output.contains(&format!("pattern {unit_pattern} __x = Left __x")),
            "{output}"
        );
        assert!(
            output.contains(&format!("pattern {product_pattern} __x = Right (__x)")),
            "{output}"
        );
        let nested_pattern = HaskellName::ProductPattern(constructor_product).render();
        assert!(
            output.contains(&format!("pattern {nested_pattern} ")),
            "{output}"
        );
    }

    #[test]
    fn transparent_newtype_payload_keeps_its_prepared_structural_boundaries() {
        let output = render(
            "module api; \
             pub newtype Wrapped : . & (. | .) { \
               pub constructor mk_wrapped; pub projector un_wrapped; \
             }; \
             host fn nested(value: . & Wrapped) -> .;",
        )
        .expect("transparent nested payload has an exact Haskell facade");

        let root = BoundaryId::env("api", "nested", BoundaryRoot::Arg(1));
        let nested_sum = root.nested(BoundaryStep::Slot(1));
        let root_alias = boundary_alias(&root);
        let nested_sum_alias = boundary_alias(&nested_sum);

        assert!(
            output.contains(&standard_hs(&format!(
                "type {root_alias} (h :: %KIND%.Type) (m :: %KIND%.Type -> %KIND%.Type) = FallbackProbeProduct '[(), ({nested_sum_alias} h m)]"
            ))),
            "{output}"
        );
        assert!(
            output.contains(&standard_hs(&format!(
                "type {nested_sum_alias} (h :: %KIND%.Type) (m :: %KIND%.Type -> %KIND%.Type) = FallbackProbeSum '[(), ()]"
            ))),
            "{output}"
        );
        assert!(
            !output.contains(&standard_hs(&format!(
                "type {root_alias} (h :: %KIND%.Type) (m :: %KIND%.Type -> %KIND%.Type) = FallbackProbeProduct '[(), (FallbackProbeSum '[(), ()])]"
            ))),
            "the transparent payload Sum was inlined across its prepared boundary:\n{output}"
        );
    }

    #[test]
    fn public_nominal_carrier_blocks_before_its_callable_use() {
        assert_eq!(
            blocker(
                "module api; \
                 pub rec newtype Tree : (. | (Tree)) { constructor mk; projector un; }; \
                 pub fn keep(value: Tree) -> Tree { value }"
            ),
            Some("exported newtype `api/Tree`".to_owned())
        );
    }

    #[test]
    fn universal_host_record_resolves_bare_newtypes_in_the_declaring_module() {
        let sources = [
            (
                "left.kio",
                "module left; \
                 pub newtype Same : . { constructor mk; projector un; }; \
                 pub host fn round(value: Same) -> Same; \
                 pub fn round_public(value: Same) -> Same { round(value) }",
            ),
            (
                "right.kio",
                "module right; \
                 pub newtype Same : (. & .) { constructor mk; projector un; };",
            ),
            (
                "caller.kio",
                "module caller; \
                 import left as l; \
                 import left(round); \
                 pub newtype Same : (. & .) { constructor mk; projector un; }; \
                 pub fn call(value: l.Same) -> l.Same { round(value) }",
            ),
        ];
        let package = routed_package_files_with_bridge(&sources, "caller; left; right;");
        let names = HaskellNames::derive("FallbackProbe");
        let prepared =
            crate::backends::boundary_facade::PreparedBoundaryCallableSites::collect_live(&package)
                .expect("prepare universal host record");
        let shapes = crate::backends::haskell::skin::HaskellShapes::new_with_prepared(
            &package, &names, &prepared,
        );
        let facade =
            crate::backends::haskell::facade::HaskellFacadeCatalog::new(&prepared, &shapes);
        let output =
            super::render_host_record(&shapes, &facade, &names).expect("universal host record");
        let carrier =
            crate::backends::haskell::skin::nominal_haskell_type_name("KioCarrier", "left", "Same");
        assert!(
            output.contains(&format!(
                "{} :: ({carrier} h m) -> m ({carrier} h m)",
                host_field("left", "round")
            )),
            "{output}"
        );
    }

    #[test]
    fn rank_n_structural_export_keeps_parameterized_boundary_aliases() {
        let output = render(
            "module api; \
             pub fn keep(value: [A] (A & .) -> (A & .)) \
               -> [A] (A & .) -> (A & .) { value }",
        )
        .expect("render rank-N structural export");
        let wrapper = export_wrapper("api", "keep");
        let signature = output
            .lines()
            .find(|line| line.starts_with(&format!("{wrapper} ::")))
            .expect("export signature");

        let callback_arg = export_boundary("api", "keep", BoundaryRoot::Arg(0))
            .nested(BoundaryStep::CallbackArg(0));
        let input_callback_ret =
            export_boundary("api", "keep", BoundaryRoot::Arg(0)).nested(BoundaryStep::CallbackRet);
        let output_callback_ret =
            export_boundary("api", "keep", BoundaryRoot::Ret).nested(BoundaryStep::CallbackRet);

        assert!(!signature.contains(&boundary_alias(&callback_arg)));
        assert!(signature.contains(&boundary_alias(&input_callback_ret)));
        assert!(signature.contains(&boundary_alias(&output_callback_ret)));
    }

    #[test]
    fn sum_patterns_use_the_assigned_three_step_keys() {
        let output = render(
            "module api; \
             pub newtype Same : . { constructor mk; projector un; }; \
             pub fn keep(value: Same | Same | .) -> Same | Same | . { value }",
        )
        .expect("render repeated sum slots");

        let boundary = export_boundary("api", "keep", BoundaryRoot::Arg(0));
        let bare = sum_pattern(&boundary, StructuralKey::BareNewtype("Same".to_owned()), 0);
        let qualified = sum_pattern(
            &boundary,
            StructuralKey::QualifiedNewtype {
                module: crate::backends::haskell::naming::ModuleId::from_path("api"),
                newtype: "Same".to_owned(),
            },
            1,
        );
        let positional = sum_pattern(&boundary, StructuralKey::Positional(2), 2);

        assert!(output.contains(&format!("pattern {bare} ")));
        assert!(output.contains(&format!("pattern {qualified} ")));
        assert!(output.contains(&format!("pattern {positional} ")));
        assert!(output.contains(&format!(
            "{{-# COMPLETE {bare}, {qualified}, {positional} #-}}"
        )));
    }

    #[test]
    fn polymorphic_function_newtype_uses_the_shared_nominal_head() {
        let output = render_files(&[
            (
                "types.kio",
                "module types; \
                 pub newtype Box[A] : A { constructor mk_box; projector un_box; }; \
                 pub newtype Functor[*F] : [A][B] ((A -> B) & F(A)) -> F(B) { \
                   constructor mk_functor; projector fmap; \
                 };",
            ),
            (
                "api.kio",
                "module api; \
                 import types(Box, Functor); \
                 pub fn keep(value: Functor(Box)) -> Functor(Box) { value }",
            ),
        ])
        .expect("render polymorphic-newtype export");
        let wrapper = export_wrapper("api", "keep");
        let signature = output
            .lines()
            .find(|line| line.starts_with(&format!("{wrapper} ::")))
            .expect("export signature");
        let nominal = crate::backends::haskell::skin::nominal_haskell_type_name(
            "KioCarrier",
            "types",
            "Functor",
        );
        let header = output
            .split(") where")
            .next()
            .expect("generated module header");

        assert!(signature.contains(&nominal), "{signature}");
        assert!(header.contains(&nominal), "{header}");
        assert!(!header.contains(&format!("{nominal}(..)")), "{header}");
        let callback_arg = export_boundary("api", "keep", BoundaryRoot::Arg(0))
            .nested(BoundaryStep::CallbackArg(0));
        assert!(!signature.contains(&boundary_alias(&callback_arg)));
    }

    #[test]
    fn polymorphic_function_newtype_members_use_canonical_callable_slots() {
        let output = render(
            "module api; \
             pub newtype Pick_first : [A] (A & A) -> A { \
               pub constructor mk_pick_first; pub projector pick_first; \
             };",
        )
        .expect("render polymorphic function newtype members");
        let canonical = standard_hs("forall (t_a :: %KIND%.Type). m (t_a -> t_a -> m t_a)");

        for wrapper in [
            newtype_export_wrapper("api", "Pick_first", "mk_pick_first"),
            newtype_export_wrapper("api", "Pick_first", "pick_first"),
        ] {
            let signature = output
                .lines()
                .find(|line| line.starts_with(&format!("{wrapper} ::")))
                .unwrap_or_else(|| panic!("missing `{wrapper}` signature"));
            assert_eq!(signature.matches(&canonical).count(), 2, "{signature}");
            assert!(
                !signature.contains("m ((t_a, t_a) -> m t_a)"),
                "{signature}"
            );
        }
    }
}

/// Render the `<Handle>Host h m` value record: one field per `host fn`,
/// typed `<args> -> m <ret>`. A `()` return is `m ()`. The record is a
/// value of monad-returning functions (the host supplies the bodies), not
/// a typeclass.
///
/// Generic and rank-N fields retain their ordinary Haskell polymorphism;
/// the record never chooses a representation from a Kio role.
fn render_host_record(
    shapes: &super::skin::HaskellShapes<'_>,
    facade: &HaskellFacadeCatalog,
    names: &HaskellNames,
) -> Result<String, EmitError> {
    // Per-backend limitation (`specs/backends/haskell.md` § Deprecated host
    // items > Host-fn removal is not source-stable on Haskell): the `Host h m`
    // record is built from the *live* host-fn set only. A Haskell record field
    // carries no per-field default, so a removed host fn cannot be kept
    // settable-but-optional as a deprecated Rust trait method can. Dropping
    // the field breaks an existing host's record literal; keeping it forces
    // every new host to initialise it. Host-fn removal is source-breaking.
    let mut fields: Vec<String> = Vec::new();
    for site in facade.sites() {
        let BoundaryFacadeSiteOwner::HostFunction { name } = site.id().owner() else {
            continue;
        };
        let entry = site.entry();
        if !entry.is_live() {
            continue;
        }
        let module = site.id().module_segments().join("/");
        let field = host_field_name(&module, name);
        let mut scope = Vec::new();
        let mut arg_tys = Vec::new();
        for stage in entry.stages() {
            match stage {
                HaskellCallableHeadStage::Type(param) => {
                    scope = super::skin::extend_type_scope(&scope, param);
                }
                HaskellCallableHeadStage::Value { slots, .. } => {
                    for slot in slots {
                        arg_tys.push(shapes.boundary_alias_haskell_type_in(
                            slot.boundary(),
                            slot.ty(),
                            &scope,
                            Some(&module),
                        )?);
                    }
                }
            }
        }
        let ret_ty = shapes.boundary_alias_haskell_type_in(
            entry.returned().boundary(),
            entry.returned().ty(),
            &scope,
            Some(&module),
        )?;
        let mut ty = String::new();
        if !scope.is_empty() {
            ty.push_str("forall ");
            ty.push_str(
                &scope
                    .iter()
                    .map(|param| super::skin::kinded_haskell_binder(param, shapes.standard_names()))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            ty.push_str(". ");
        }
        for a in &arg_tys {
            ty.push_str(&fn_arg_paren(a));
            ty.push_str(" -> ");
        }
        ty.push_str(&format!("m {}", ty_arg_paren(&ret_ty)));
        fields.push(format!("{field} :: {ty}"));
    }

    let mut out = String::new();
    out.push_str(
        "-- The host record: a value record of m-returning functions, one\n\
         -- field per `host fn`. The host supplies the field values; the\n\
         -- package calls them through the record. Monad-polymorphic in m.\n",
    );
    let kind = &names.standard_names.data_kind;
    out.push_str(&format!(
        "data {ty} (h :: {kind}.Type) (m :: {kind}.Type -> {kind}.Type) = {ty}\n",
        ty = names.host_ty
    ));
    if fields.is_empty() {
        // An empty record is still well-kinded: `<Handle>Host h m` mentions
        // `m`, so the package handle and the monad-polymorphic exports
        // type-check even with no host fields.
        out.push_str("  {\n");
        out.push_str("  }\n");
    } else {
        out.push_str("  { ");
        out.push_str(&fields.join("\n  , "));
        out.push_str("\n  }\n");
    }
    Ok(out)
}

/// Render the `<Handle> h m` handle: owns the host record for the package's
/// lifetime. Module fns and exports are reached as top-level functions
/// taking the package.
fn render_package_handle(names: &HaskellNames) -> String {
    let (handle, host_ty) = (&names.handle, &names.host_ty);
    let kind = &names.standard_names.data_kind;
    format!(
        "-- The package handle: owns the host conformance for the lifetime of\n\
         -- the package. Exported items are invoked as monad-polymorphic\n\
         -- functions taking the package.\n\
         newtype {handle} (h :: {kind}.Type) (m :: {kind}.Type -> {kind}.Type) = {handle}\n\
         \x20 {{ pkgHost :: {host_ty} h m\n\
         \x20 }}\n"
    )
}

/// Render the `create<Handle>` factory: instantiate the package against a
/// host record. `Monad m =>` so the exported surface stays
/// monad-polymorphic.
fn render_create_package(names: &HaskellNames) -> String {
    let (handle, host_ty, factory, host_types) = (
        &names.handle,
        &names.host_ty,
        &names.factory,
        &names.host_types_ty,
    );
    format!(
        "-- {factory} instantiates the package against a host record.\n\
         {factory} :: forall h m. ({host_types} h, Monad m) => {host_ty} h m -> {handle} h m\n\
         {factory} h = {handle} {{ pkgHost = h }}\n"
    )
}

/// Render one module fn as a monad-polymorphic top-level function:
/// `name :: Monad m => <Handle> h m -> <private> -> … -> m <private>`.
/// The body is produced through the body visitor and threaded through `m`.
fn render_module_fn(
    name: &str,
    module_key: &str,
    f: &crate::ast::FnDef<Routed>,
    shapes: &super::skin::HaskellShapes<'_>,
    package: &Package<Routed>,
    names: &HaskellNames,
) -> Result<String, EmitError> {
    let entry = package
        .module(module_key)
        .expect("module function must belong to its package module");
    let selective_imports = build_selective_imports(&entry.module, package);
    let qualified_imports = build_qualified_imports(&entry.module.imports);
    // Each value group becomes a sequence of private-carrier
    // parameters; the body is the innermost expression, with later groups
    // wrapped in the runtime's private function constructor.
    let value_groups = value_param_name_groups(&f.sig);
    let mut emitter = BodyEmitter::new(module_key, &selective_imports, &qualified_imports, shapes);
    for g in &value_groups {
        for n in g {
            emitter.locals.push((*n).to_owned());
        }
    }
    let body = emitter.emit_expr(&f.body)?;

    // Wrap later groups (curry) into nested closures. The first group's
    // binders are the top-level params; each later group becomes
    // a private function constructor returned from the previous.
    let first = value_groups.first().cloned().unwrap_or_default();
    let mut acc = body;
    for g in value_groups.iter().skip(1).rev() {
        acc = format!("pure {}", emitter.wrap_group_lambda(g, acc));
    }

    let mut params = String::new();
    for n in &first {
        params.push(' ');
        params.push_str(&haskell_local_ident(n));
    }

    let mut out = String::new();
    out.push_str(&format!(
        "-- Module fn `{}` in module `{module_key}`. Monad-polymorphic; the\n\
         -- body sequences host effects through m and produces its value\n\
         -- with `pure`.\n",
        f.name
    ));
    let arity = first.len();
    let opaque = &names.runtime_names.opaque;
    let arg_tys = format!("{opaque} h m -> ").repeat(arity);
    let handle = &names.handle;
    let host_types = &names.host_types_ty;
    out.push_str(&format!(
        "{name} :: forall h m. ({host_types} h, Monad m) => {handle} h m -> {arg_tys}m ({opaque} h m)\n"
    ));
    out.push_str(&format!("{name} _pkg{params} = {acc}\n"));
    Ok(out)
}

/// The value-parameter name groups of a signature (each curried group).
fn value_param_name_groups(sig: &crate::ast::Signature<Routed>) -> Vec<Vec<&str>> {
    let mut groups: Vec<Vec<&str>> = sig
        .canonical_groups()
        .into_iter()
        .filter_map(|group| match group {
            crate::ast::SignatureGroupRef::Value(params) => Some(
                params
                    .iter()
                    .filter_map(|p| match p {
                        crate::ast::SignatureParam::Value(vp) => Some(vp.name.as_str()),
                        crate::ast::SignatureParam::Type(_) => None,
                    })
                    .collect(),
            ),
            crate::ast::SignatureGroupRef::Type(_) => None,
        })
        .collect();
    if groups.is_empty() {
        groups.push(Vec::new());
    }
    groups
}

/// The injective Haskell value name for module fn `leaf` in `module_key`.
pub(super) fn module_fn_name(module_key: &str, leaf: &str) -> String {
    HaskellName::ModuleFn(ModuleItemId::new(module_key, leaf)).render()
}

/// The injective Haskell `Host` record field for a source host function.
pub(super) fn host_field_name(module_path: &str, leaf: &str) -> String {
    HaskellName::HostField(ModuleItemId::new(module_path, leaf)).render()
}

pub(super) fn export_wrapper_name(module_key: &str, leaf: &str) -> String {
    HaskellName::ExportWrapper(ItemId::item(module_key, leaf)).render()
}

pub(super) fn export_newtype_wrapper_name(module_key: &str, newtype: &str, member: &str) -> String {
    HaskellName::ExportWrapper(ItemId::newtype_member(module_key, newtype, member)).render()
}

/// Parenthesize a Haskell type when it appears as a function-argument
/// position (`a -> b` needs parens there, an atom does not).
fn fn_arg_paren(ty: &str) -> String {
    ty_arg_paren(ty)
}

/// Parenthesize a Haskell type for an application/argument position: wrap
/// if it contains a top-level space or arrow and is not already
/// parenthesized.
fn ty_arg_paren(ty: &str) -> String {
    let t = ty.trim();
    if needs_type_parens(t) {
        format!("({t})")
    } else {
        t.to_owned()
    }
}

/// True when a rendered Haskell type needs parentheses in an argument
/// position — it has a top-level space (application or arrow) and is not
/// already a single parenthesized / bracketed group.
fn needs_type_parens(t: &str) -> bool {
    if !t.contains(' ') {
        return false;
    }
    // Already a single wrapped group?
    if (t.starts_with('(') && balanced_wrap(t, '(', ')'))
        || (t.starts_with('[') && balanced_wrap(t, '[', ']'))
    {
        return false;
    }
    true
}

/// True when `t` is a single balanced wrap by `open`/`close` (the outer
/// pair encloses everything).
fn balanced_wrap(t: &str, open: char, close: char) -> bool {
    if !t.starts_with(open) || !t.ends_with(close) {
        return false;
    }
    let mut depth = 0i32;
    for (i, c) in t.char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 && i + close.len_utf8() != t.len() {
                return false;
            }
        }
    }
    depth == 0
}

type SelectiveImports = BTreeMap<String, String>;
type QualifiedImports = BTreeMap<String, String>;

fn build_qualified_imports(imports: &[crate::ast::Import]) -> QualifiedImports {
    let mut out = BTreeMap::new();
    for u in imports {
        if let crate::ast::ImportKind::Qualified { path, alias } = &u.kind {
            let path_str = path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("/");
            out.insert(alias.clone(), path_str);
        }
    }
    out
}

fn build_selective_imports(
    importer: &crate::ast::Module<Routed>,
    package: &Package<Routed>,
) -> SelectiveImports {
    crate::backends::selective_module_fn_import_owners(importer, package)
        .into_iter()
        .map(|(name, owner)| {
            let target = module_fn_name(&owner, &name);
            (name, target)
        })
        .collect()
}

// =========================================================================
// Exports — typed boundary entry points.
// =========================================================================

/// One exported item — a `pub fn` or a `pub newtype` member — rendered as
/// a typed boundary wrapper that converts native boundary args to
/// the private carrier, calls the internal module fn, and converts the result
/// back to the native boundary type.
struct ExportEntry {
    /// The wrapper's injective Haskell ABI name.
    wrapper_name: String,
    /// Exact shared facade site; source spellings never have to be recovered
    /// from the rendered wrapper name.
    facade_site: BoundaryFacadeSiteId,
    /// The declaring module's slash-path — the scope a bare newtype leaf in
    /// the param / return types resolves against (so a same-leaf newtype in
    /// another module never supplies the wrong boundary type).
    module: String,
    /// The internal target the wrapper calls.
    target: ExportTarget,
    /// Every value param's declared type across all value groups, in
    /// declaration order — the flat wrapper parameter list.
    param_types: Vec<Option<Type<Routed>>>,
    /// Value-group sizes, in order. The opaque body renders group 0's
    /// params as the module fn's top-level binders and each later group
    /// as one private function layer over one argument (a product when the
    /// group has two or more params); the wrapper applies accordingly.
    group_sizes: Vec<usize>,
    /// The declared return type.
    ret_type: Type<Routed>,
}

enum ExportTarget {
    /// Calls module-fn `mangled`.
    ModuleFn { mangled: String },
    /// A non-generic, non-existential transparent newtype member on the
    /// universal-body fallback. Generic and existential members require the
    /// native exact wrapper and are rejected before this path renders.
    TransparentNewtype,
    /// A public newtype whose prepared surface requires a nominal carrier.
    /// The universal body must never render this target as identity.
    NominalNewtype,
}

fn collect_exports(
    package: &Package<Routed>,
    facade: &HaskellFacadeCatalog,
    shapes: &super::skin::HaskellShapes<'_>,
) -> Vec<ExportEntry> {
    let mut out = Vec::new();
    for site in facade.sites() {
        if !site.entry().is_live() {
            continue;
        }
        let module = site.id().module_segments().join("/");
        match site.id().owner() {
            BoundaryFacadeSiteOwner::HostFunction { .. } => {}
            BoundaryFacadeSiteOwner::ExportedFunction { name } => {
                let f = exact_exported_function(package, &module, name);
                let groups = value_group_param_types(&f.sig);
                out.push(ExportEntry {
                    wrapper_name: export_wrapper_name(&module, name),
                    facade_site: site.id().clone(),
                    module: module.clone(),
                    target: ExportTarget::ModuleFn {
                        mangled: module_fn_name(&module, name),
                    },
                    param_types: groups.iter().flatten().cloned().collect(),
                    group_sizes: groups.iter().map(Vec::len).collect(),
                    ret_type: signature_ret_type(&f.sig, &f.ret),
                });
            }
            BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member }
            | BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => {
                let declaration = exact_newtype_declaration(package, &module, newtype);
                let public = facade
                    .public_newtype(&module, newtype)
                    .expect("a prepared newtype-member site has public inventory metadata");
                let target = if public.requires_nominal_carrier()
                    || !shapes.fallback_bridgeable_in(&declaration.payload, Some(&module))
                {
                    ExportTarget::NominalNewtype
                } else {
                    ExportTarget::TransparentNewtype
                };
                out.push(ExportEntry {
                    wrapper_name: export_newtype_wrapper_name(&module, newtype, member),
                    facade_site: site.id().clone(),
                    module: module.clone(),
                    target,
                    param_types: vec![Some(declaration.payload.clone())],
                    group_sizes: vec![1],
                    ret_type: declaration.payload.clone(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.wrapper_name.cmp(&b.wrapper_name));
    out
}

fn exact_exported_function<'p>(
    package: &'p Package<Routed>,
    module: &str,
    name: &str,
) -> &'p crate::ast::FnDef<Routed> {
    package
        .module(module)
        .and_then(|entry| {
            entry.module.items.iter().find_map(|item| match item {
                crate::ast::Item::FnDef(function) if function.name == name => Some(function),
                _ => None,
            })
        })
        .unwrap_or_else(|| {
            unreachable!("prepared Haskell export `{module}/{name}` has no exact declaration")
        })
}

fn exact_newtype_declaration<'p>(
    package: &'p Package<Routed>,
    module: &str,
    name: &str,
) -> &'p crate::ast::Newtype<Routed> {
    let entry = package.module(module).unwrap_or_else(|| {
        unreachable!("prepared Haskell public newtype `{module}/{name}` has no source module")
    });
    for item in &entry.module.items {
        let mut found = None;
        crate::pass::resolve::for_each_item_declaration(item, |declaration| {
            if let Some(candidate) = declaration.newtype()
                && candidate.name == name
            {
                found = Some(candidate);
            }
        });
        if let Some(declaration) = found {
            return declaration;
        }
    }
    unreachable!("prepared Haskell public newtype `{module}/{name}` has no exact declaration")
}

/// Every value group's param types, in declaration order — one inner
/// vec per group, flattened by the exported wrapper
/// (`specs/backends/README.md` § Function-type FFI canonicalization).
/// The native-typed internal module fn is curried, so the wrapper
/// passes all args in one saturated application.
fn value_group_param_types(sig: &crate::ast::Signature<Routed>) -> Vec<Vec<Option<Type<Routed>>>> {
    let mut scope = BTreeSet::new();
    let mut groups = Vec::new();
    for group in sig.canonical_groups() {
        match group {
            crate::ast::SignatureGroupRef::Type(params) => {
                for param in params {
                    if let crate::ast::SignatureParam::Type(param) = param {
                        scope.insert(param.name.clone());
                    }
                }
            }
            crate::ast::SignatureGroupRef::Value(params) => {
                groups.push(
                    params
                        .iter()
                        .filter_map(|param| match param {
                            crate::ast::SignatureParam::Value(param) => Some(
                                param
                                    .ty
                                    .as_ref()
                                    .map(|ty| super::skin::erase_scoped_type_vars(ty, &scope)),
                            ),
                            crate::ast::SignatureParam::Type(_) => None,
                        })
                        .collect(),
                );
            }
        }
    }
    groups
}

fn signature_ret_type(sig: &crate::ast::Signature<Routed>, ret: &Type<Routed>) -> Type<Routed> {
    let scope: BTreeSet<String> = sig
        .params
        .iter()
        .filter_map(|param| match param {
            crate::ast::SignatureParam::Type(param) => Some(param.name.clone()),
            crate::ast::SignatureParam::Value(_) => None,
        })
        .collect();
    super::skin::erase_scoped_type_vars(ret, &scope)
}

// =========================================================================
// FFI boundary aliases and flat structural patterns.
// =========================================================================

/// The rendered FFI boundary aliases plus the names to add to the module
/// export list.
struct FfiAliases {
    /// The `type` / `pattern` synonym declarations.
    decls: String,
    /// The export-list entries (a bare type-synonym name, or a
    /// `pattern <Name>` entry for a pattern synonym).
    exported_names: Vec<String>,
    /// True when at least one pattern synonym was emitted (gates the
    /// `PatternSynonyms` pragma).
    has_pattern_synonyms: bool,
    /// True when an impredicative slot requires a hidden view helper.
    has_rank_n_patterns: bool,
}

/// Emit the stable-named boundary aliases for every host-fn and export
/// boundary slot. Mirrors Go's `render_ffi_go` / Swift's `render_ffi_swift`:
///
/// - host fns get typed `Env_H…` argument / return aliases;
/// - exports get typed `Exp_H…` argument / return aliases;
/// - a function-typed slot recurses into its compound legs as
///   `<base>_cbarg<j>` / `<base>_cbret`;
/// - a product / sum slot emits `type <alias> ... = <Family> '[...]`; a
///   product also gets one flat record pattern and a sum gets one keyed
///   bidirectional pattern per arm, hiding the right-nested representation;
/// - a passthrough slot emits nothing: the runner can name its direct
///   host-selected, unit, universal, or abstract nominal carrier type.
fn render_ffi_aliases(
    facade: &HaskellFacadeCatalog,
    shapes: &super::skin::HaskellShapes<'_>,
) -> Result<FfiAliases, EmitError> {
    let mut acc = AliasAcc::default();

    // The package-complete shared catalog is the sole public-topology walk.
    // Haskell deliberately does not preserve removed host-function fields,
    // so retained callable roots contribute no aliases or record slots.
    for site in facade.sites() {
        let entry = site.entry();
        if !entry.is_live() {
            continue;
        }
        let module_path = site.id().module_segments().join("/");
        let module = Some(module_path.as_str());
        let mut scope = Vec::new();
        for stage in entry.stages() {
            match stage {
                HaskellCallableHeadStage::Type(param) => {
                    scope = super::skin::extend_type_scope(&scope, param);
                }
                HaskellCallableHeadStage::Value { slots, .. } => {
                    for slot in slots {
                        emit_ffi_slot_aliases(
                            slot.boundary(),
                            slot.ty(),
                            &scope,
                            module,
                            shapes,
                            facade,
                            &mut acc,
                        )?;
                    }
                }
            }
        }
        emit_ffi_slot_aliases(
            entry.returned().boundary(),
            entry.returned().ty(),
            &scope,
            module,
            shapes,
            facade,
            &mut acc,
        )?;
    }

    Ok(acc.finish())
}

/// Accumulator for the alias walk. Claims are keyed by typed semantic
/// identity; rendered strings never decide whether two declarations dedupe.
#[derive(Default)]
struct AliasAcc {
    decls: String,
    exported_names: Vec<String>,
    claims: NameClaims,
    has_pattern_synonyms: bool,
    has_rank_n_patterns: bool,
}

impl AliasAcc {
    fn finish(self) -> FfiAliases {
        FfiAliases {
            decls: self.decls,
            exported_names: self.exported_names,
            has_pattern_synonyms: self.has_pattern_synonyms,
            has_rank_n_patterns: self.has_rank_n_patterns,
        }
    }
}

/// Emit the alias(es) for one boundary slot of stable identity `boundary`
/// carrying type `ty`. See [`render_ffi_aliases`] for the per-shape rules.
fn emit_ffi_slot_aliases(
    boundary: &BoundaryId,
    ty: &Type<Routed>,
    scope: &[crate::ast::TypeParam],
    module: Option<&str>,
    shapes: &super::skin::HaskellShapes<'_>,
    facade: &HaskellFacadeCatalog,
    acc: &mut AliasAcc,
) -> Result<(), EmitError> {
    if let Type::Path { args, .. } = ty {
        for (index, arg) in args.iter().enumerate() {
            emit_ffi_slot_aliases(
                &boundary.nested(BoundaryStep::App(
                    index.try_into().expect("type argument index fits u32"),
                )),
                arg,
                scope,
                module,
                shapes,
                facade,
                acc,
            )?;
        }
    }
    if let Type::Path { segments, .. } = ty
        && let [name] = segments.as_slice()
        && scope.iter().any(|param| param.name == name.as_str())
    {
        return Ok(());
    }
    if let Type::Forall { param, body, .. } = ty {
        let nested = super::skin::extend_type_scope(scope, param);
        return emit_ffi_slot_aliases(boundary, body, &nested, module, shapes, facade, acc);
    }
    if shapes.is_passthrough_scoped_in(ty, scope, module) {
        return Ok(());
    }
    if matches!(ty, Type::Path { .. })
        && let Some((body, owner)) = shapes.boundary_path_expansion_in(ty, scope, module)
    {
        return emit_ffi_slot_aliases(boundary, &body, scope, Some(&owner), shapes, facade, acc);
    }
    match ty {
        Type::Product { .. } => {
            let occurrence = facade
                .structural_occurrence(
                    boundary,
                    crate::backends::boundary_facade::FacadeKind::Product,
                )
                .expect("a prepared Haskell product carries its exact occurrence");
            emit_compound_type_alias(boundary, ty, occurrence, scope, module, shapes, acc)?;
            for slot in occurrence.slots() {
                emit_ffi_slot_aliases(
                    slot.boundary(),
                    slot.ty(),
                    scope,
                    module,
                    shapes,
                    facade,
                    acc,
                )?;
            }
            let keys = occurrence.keys();
            let pattern_id = PatternId::Product(boundary.clone());
            let pattern_name = HaskellName::ProductPattern(boundary.clone());
            let pat = shapes.structural_names().render(&pattern_name);
            let selectors = keys
                .iter()
                .enumerate()
                .map(|(index, key)| HaskellName::ProductSelector {
                    boundary: boundary.clone(),
                    key: key.clone(),
                    index: index.try_into().expect("product selector index fits u32"),
                })
                .collect::<Vec<_>>();
            let rendered_selectors = selectors
                .iter()
                .map(|name| shapes.structural_names().render(name))
                .collect::<Vec<_>>();
            let product_slots = occurrence
                .slots()
                .iter()
                .map(|slot| slot.ty())
                .collect::<Vec<_>>();
            let (decl, helpers) =
                if let Some(shape) = rank_n_pattern_shape(&product_slots, scope, module, shapes)? {
                    acc.has_rank_n_patterns = true;
                    render_rank_n_product_pattern(
                        &pattern_id,
                        &pat,
                        &rendered_selectors,
                        &shape,
                        shapes.structural_names(),
                        shapes.standard_names(),
                    )
                } else {
                    (
                        format!(
                            "pattern {pat} {{ {} }} = {}\n",
                            rendered_selectors.join(", "),
                            nested_product_pattern(&rendered_selectors),
                        ),
                        Vec::new(),
                    )
                };
            let complete = format!("{{-# COMPLETE {pat} #-}}\n");
            let payload = format!("{decl}{complete}");
            if acc.claims.claim(pattern_name, &pat, &payload) {
                for (selector, rendered) in selectors.into_iter().zip(&rendered_selectors) {
                    assert!(acc.claims.claim(selector, rendered, &payload));
                }
                for helper in helpers {
                    let rendered = shapes.structural_names().render(&helper);
                    assert!(acc.claims.claim(helper, &rendered, &payload));
                }
                acc.decls.push_str(&payload);
                acc.exported_names.push(format!("pattern {pat}"));
                acc.exported_names.extend(rendered_selectors);
                acc.has_pattern_synonyms = true;
            }
        }
        Type::Sum { .. } => {
            let occurrence = facade
                .structural_occurrence(boundary, crate::backends::boundary_facade::FacadeKind::Sum)
                .expect("a prepared Haskell sum carries its exact occurrence");
            emit_compound_type_alias(boundary, ty, occurrence, scope, module, shapes, acc)?;
            for slot in occurrence.slots() {
                emit_ffi_slot_aliases(
                    slot.boundary(),
                    slot.ty(),
                    scope,
                    module,
                    shapes,
                    facade,
                    acc,
                )?;
            }
            let keys = occurrence.keys();
            let sum_slots = occurrence
                .slots()
                .iter()
                .map(|slot| slot.ty())
                .collect::<Vec<_>>();
            let rank_n_shape = rank_n_pattern_shape(&sum_slots, scope, module, shapes)?;
            let mut complete = Vec::with_capacity(keys.len());
            for (k, key) in keys.iter().enumerate() {
                let pattern = SumPatternId {
                    boundary: boundary.clone(),
                    key: key.clone(),
                    index: k.try_into().expect("sum arm index fits u32"),
                };
                let pattern_name = HaskellName::SumPattern(pattern.clone());
                let pat = shapes.structural_names().render(&pattern_name);
                let (decl, helpers) = if let Some(shape) = &rank_n_shape {
                    acc.has_rank_n_patterns = true;
                    render_rank_n_sum_pattern(
                        &pattern,
                        &pat,
                        k,
                        shape,
                        shapes.structural_names(),
                        shapes.standard_names(),
                    )
                } else {
                    (
                        format!(
                            "pattern {pat} __x = {}\n",
                            nested_sum_pattern(k, keys.len(), "__x"),
                        ),
                        Vec::new(),
                    )
                };
                if acc.claims.claim(pattern_name, &pat, &decl) {
                    for helper in helpers {
                        let rendered = shapes.structural_names().render(&helper);
                        assert!(acc.claims.claim(helper, &rendered, &decl));
                    }
                    acc.decls.push_str(&decl);
                    acc.exported_names.push(format!("pattern {pat}"));
                    acc.has_pattern_synonyms = true;
                }
                complete.push(pat);
            }
            if !complete.is_empty() {
                acc.decls
                    .push_str(&format!("{{-# COMPLETE {} #-}}\n", complete.join(", ")));
            }
        }
        Type::Function {
            param,
            ret,
            abi_arity,
            ..
        } => {
            let layout = facade.function_layout(boundary).unwrap_or_else(|| {
                unreachable!(
                    "an exact Haskell function alias has no prepared execution layout: {boundary:?}"
                )
            });
            assert_eq!(
                layout.facade_slot_count(),
                *abi_arity,
                "the realized Haskell function type preserves every prepared facade slot"
            );
            for (j, p) in Type::right_spine_take(param, *abi_arity).iter().enumerate() {
                emit_ffi_slot_aliases(
                    &boundary.nested(BoundaryStep::CallbackArg(
                        j.try_into().expect("callback argument index fits u32"),
                    )),
                    p,
                    scope,
                    module,
                    shapes,
                    facade,
                    acc,
                )?;
            }
            emit_ffi_slot_aliases(
                &boundary.nested(BoundaryStep::CallbackRet),
                ret,
                scope,
                module,
                shapes,
                facade,
                acc,
            )?;
        }
        Type::Forall { .. } => unreachable!("forall handled before the passthrough check"),
        Type::Path { .. } => {}
        _ => {}
    }
    Ok(())
}

/// Emit the stable alias for a compound slot, deduped by alias name, and add
/// the bare alias name to the export list.
fn emit_compound_type_alias(
    boundary: &BoundaryId,
    ty: &Type<Routed>,
    occurrence: &HaskellStructuralOccurrence,
    scope: &[crate::ast::TypeParam],
    module: Option<&str>,
    shapes: &super::skin::HaskellShapes<'_>,
    acc: &mut AliasAcc,
) -> Result<(), EmitError> {
    let name = HaskellName::BoundaryAlias(boundary.clone());
    let alias = shapes.structural_names().render(&name);
    let binders = shapes.scoped_binders(ty, scope);
    let binder_decls = binders
        .iter()
        .map(|param| {
            format!(
                " {}",
                super::skin::kinded_haskell_binder(param, shapes.standard_names())
            )
        })
        .collect::<String>();
    let family = match occurrence.kind() {
        crate::backends::boundary_facade::FacadeKind::Product => {
            shapes.structural_names().product_family()
        }
        crate::backends::boundary_facade::FacadeKind::Sum => shapes.structural_names().sum_family(),
    };
    let slots = occurrence
        .slots()
        .iter()
        .map(|slot| {
            shapes
                .boundary_alias_haskell_type_in(slot.boundary(), slot.ty(), scope, module)
                .map(|ty| ty_arg_paren(&ty))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let structural = format!("{family} '[{}]", slots.join(", "));
    let kind = &shapes.standard_names().data_kind;
    let declaration = format!(
        "type {alias} (h :: {kind}.Type) (m :: {kind}.Type -> {kind}.Type){binder_decls} = {structural}\n"
    );
    if acc.claims.claim(name, &alias, &declaration) {
        acc.decls.push_str(&declaration);
        acc.exported_names.push(alias);
    }
    Ok(())
}

fn nested_product_pattern(fields: &[String]) -> String {
    match fields {
        [] => "()".to_owned(),
        [only] => only.clone(),
        [head, rest @ ..] => format!("({head}, {})", nested_product_pattern(rest)),
    }
}

fn nested_sum_pattern(index: usize, arity: usize, payload: &str) -> String {
    let mut pattern = if index + 1 == arity {
        payload.to_owned()
    } else {
        format!("Left {payload}")
    };
    for _ in 0..index {
        pattern = format!("Right ({pattern})");
    }
    pattern
}

pub(super) fn type_contains_forall(ty: &Type<Routed>) -> bool {
    match ty {
        Type::Forall { .. } => true,
        Type::Path { args, .. } => args.iter().any(type_contains_forall),
        Type::Function { param, ret, .. }
        | Type::Product {
            left: param,
            right: ret,
            ..
        }
        | Type::Sum {
            left: param,
            right: ret,
            ..
        } => type_contains_forall(param) || type_contains_forall(ret),
        Type::Unit { .. } | Type::Bottom { .. } => false,
        Type::LabelSugar { ext, .. } => match *ext {},
        Type::Infer { ext, .. } => match *ext {},
        Type::Goal { ext, .. } => match *ext {},
    }
}

fn normalize_rank_n_boundary_type(
    ty: &Type<Routed>,
    scope: &[TypeParam],
    module: Option<&str>,
    shapes: &super::skin::HaskellShapes<'_>,
) -> Result<Type<Routed>, EmitError> {
    match ty {
        Type::Path {
            segments,
            args,
            meta,
        } => {
            let normalized_args = args
                .iter()
                .map(|arg| normalize_rank_n_boundary_type(arg, scope, module, shapes))
                .collect::<Result<Vec<_>, _>>()?;
            let normalized = Type::Path {
                segments: segments.clone(),
                args: normalized_args,
                meta: meta.clone(),
            };
            let scoped = matches!(segments.as_slice(), [name]
                if scope.iter().rev().any(|param| param.name == name.as_str()));
            if !scoped
                && let Some((body, owner)) =
                    shapes.boundary_path_expansion_in(&normalized, scope, module)
            {
                return normalize_rank_n_boundary_type(&body, scope, Some(&owner), shapes);
            }
            Ok(normalized)
        }
        Type::Unit { meta } => Ok(Type::Unit { meta: meta.clone() }),
        Type::Bottom { meta } => Ok(Type::Bottom { meta: meta.clone() }),
        Type::Function {
            param,
            ret,
            meta,
            abi_arity,
            caps,
        } => Ok(Type::Function {
            param: Box::new(normalize_rank_n_boundary_type(
                param, scope, module, shapes,
            )?),
            ret: Box::new(normalize_rank_n_boundary_type(ret, scope, module, shapes)?),
            meta: meta.clone(),
            abi_arity: *abi_arity,
            caps: caps.clone(),
        }),
        Type::Product { left, right, meta } => Ok(Type::Product {
            left: Box::new(normalize_rank_n_boundary_type(left, scope, module, shapes)?),
            right: Box::new(normalize_rank_n_boundary_type(
                right, scope, module, shapes,
            )?),
            meta: meta.clone(),
        }),
        Type::Sum { left, right, meta } => Ok(Type::Sum {
            left: Box::new(normalize_rank_n_boundary_type(left, scope, module, shapes)?),
            right: Box::new(normalize_rank_n_boundary_type(
                right, scope, module, shapes,
            )?),
            meta: meta.clone(),
        }),
        Type::Forall { param, body, meta } => {
            let nested = super::skin::extend_type_scope(scope, param);
            Ok(Type::Forall {
                param: param.clone(),
                body: Box::new(normalize_rank_n_boundary_type(
                    body, &nested, module, shapes,
                )?),
                meta: meta.clone(),
            })
        }
        Type::LabelSugar { ext, .. } => match *ext {},
        Type::Infer { ext, .. } => match *ext {},
        Type::Goal { ext, .. } => match *ext {},
    }
}

struct RankNPatternBinder {
    name: String,
    kind: Kind,
}

struct RankNPatternShape {
    slots: Vec<String>,
    binders: Vec<RankNPatternBinder>,
}

struct RankNPatternRenderer<'a, 'p> {
    scope: &'a [TypeParam],
    module: Option<&'a str>,
    shapes: &'a super::skin::HaskellShapes<'p>,
    local_binders: Vec<(String, String)>,
    representation_heads: BTreeMap<String, usize>,
    binders: Vec<RankNPatternBinder>,
    forall_depth: usize,
}

impl<'a, 'p> RankNPatternRenderer<'a, 'p> {
    fn new(
        scope: &'a [TypeParam],
        module: Option<&'a str>,
        shapes: &'a super::skin::HaskellShapes<'p>,
    ) -> Self {
        Self {
            scope,
            module,
            shapes,
            local_binders: Vec::new(),
            representation_heads: BTreeMap::new(),
            binders: Vec::new(),
            forall_depth: 0,
        }
    }

    fn representation_head(&mut self, key: String, kind: Kind) -> Result<String, EmitError> {
        if let Some(index) = self.representation_heads.get(&key).copied() {
            let binder = &self.binders[index];
            if binder.kind != kind {
                return Err(EmitError::unsupported(format!(
                    "Haskell boundary pattern head `{key}` was observed at incompatible kinds {} and {kind}",
                    binder.kind
                )));
            }
            return Ok(binder.name.clone());
        }
        let name = format!("rp{}", self.binders.len());
        let index = self.binders.len();
        self.binders.push(RankNPatternBinder {
            name: name.clone(),
            kind,
        });
        self.representation_heads.insert(key, index);
        Ok(name)
    }

    fn render(&mut self, ty: &Type<Routed>) -> Result<String, EmitError> {
        match ty {
            Type::Path { segments, args, .. } => {
                let local = if let [name] = segments.as_slice() {
                    self.local_binders
                        .iter()
                        .rev()
                        .find(|(source, _)| source == name.as_str())
                        .map(|(_, rendered)| rendered.clone())
                } else {
                    None
                };
                let mut rendered = if let Some(local) = local {
                    local
                } else {
                    let (key, kind) = if let [name] = segments.as_slice()
                        && self
                            .scope
                            .iter()
                            .rev()
                            .any(|param| param.name == name.as_str())
                    {
                        (
                            format!("scope:{}", name.as_str()),
                            self.shapes
                                .path_kind_scoped_in(ty, self.scope, self.module)
                                .unwrap_or_else(|| Kind::arrow_chain(args.len())),
                        )
                    } else if let Some(binding) = self.shapes.host_types().resolve(ty, self.module)
                    {
                        (
                            format!("host:{}\0{}", binding.module_path, binding.source_name),
                            self.shapes
                                .path_kind_scoped_in(ty, self.scope, self.module)
                                .unwrap_or_else(|| Kind::arrow_chain(args.len())),
                        )
                    } else if let Some(key) = self.shapes.resolution().key_of(ty, self.module) {
                        (
                            format!("newtype:{key}"),
                            self.shapes
                                .path_kind_scoped_in(ty, self.scope, self.module)
                                .unwrap_or_else(|| Kind::arrow_chain(args.len())),
                        )
                    } else {
                        let path = segments
                            .iter()
                            .map(|segment| segment.as_str())
                            .collect::<Vec<_>>()
                            .join("/");
                        let scoped_path = if segments.len() == 1 {
                            format!("{}:{path}", self.module.unwrap_or("<moduleless>"))
                        } else {
                            path
                        };
                        (
                            format!("path:{scoped_path}"),
                            self.shapes
                                .path_kind_scoped_in(ty, self.scope, self.module)
                                .unwrap_or_else(|| Kind::arrow_chain(args.len())),
                        )
                    };
                    self.representation_head(key, kind)?
                };
                for arg in args {
                    let arg = self.render(arg)?;
                    rendered.push(' ');
                    rendered.push_str(&ty_arg_paren(&arg));
                }
                Ok(rendered)
            }
            Type::Unit { .. } => Ok("()".to_owned()),
            Type::Bottom { .. } => Ok(format!("{}.Void", self.shapes.standard_names().data_void)),
            Type::Function {
                param,
                ret,
                abi_arity,
                ..
            } => {
                let effect = self.representation_head(
                    "effect".to_owned(),
                    Kind::Arrow(Box::new(Kind::Star), Box::new(Kind::Star)),
                )?;
                let mut rendered = String::new();
                for param in Type::right_spine_take(param, *abi_arity) {
                    let param = self.render(param)?;
                    rendered.push_str(&fn_arg_paren(&param));
                    rendered.push_str(" -> ");
                }
                let ret = self.render(ret)?;
                rendered.push_str(&effect);
                rendered.push(' ');
                rendered.push_str(&ty_arg_paren(&ret));
                Ok(format!("({rendered})"))
            }
            Type::Product { .. } => {
                let slots = Type::right_spine_product(ty)
                    .into_iter()
                    .map(|slot| self.render(slot))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(raw_product_type(&slots))
            }
            Type::Sum { .. } => {
                let slots = Type::right_spine_sum(ty)
                    .into_iter()
                    .map(|slot| self.render(slot))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(raw_sum_type(&slots, self.shapes.standard_names()))
            }
            Type::Forall { param, body, .. } => {
                let name = format!("rq{}", self.forall_depth);
                self.forall_depth += 1;
                self.local_binders.push((param.name.clone(), name.clone()));
                let body = self.render(body)?;
                self.local_binders.pop();
                let effect = self.representation_head(
                    "effect".to_owned(),
                    Kind::Arrow(Box::new(Kind::Star), Box::new(Kind::Star)),
                )?;
                Ok(format!(
                    "forall ({name} :: {}). {effect} {}",
                    super::skin::haskell_kind(
                        &param.effective_kind(),
                        self.shapes.standard_names(),
                    ),
                    ty_arg_paren(&body),
                ))
            }
            Type::LabelSugar { ext, .. } => match *ext {},
            Type::Infer { ext, .. } => match *ext {},
            Type::Goal { ext, .. } => match *ext {},
        }
    }
}

fn rank_n_pattern_shape(
    slots: &[&Type<Routed>],
    scope: &[TypeParam],
    module: Option<&str>,
    shapes: &super::skin::HaskellShapes<'_>,
) -> Result<Option<RankNPatternShape>, EmitError> {
    let normalized = slots
        .iter()
        .map(|slot| normalize_rank_n_boundary_type(slot, scope, module, shapes))
        .collect::<Result<Vec<_>, _>>()?;
    if !normalized.iter().any(type_contains_forall) {
        return Ok(None);
    }
    let mut renderer = RankNPatternRenderer::new(scope, module, shapes);
    let mut rendered = Vec::with_capacity(slots.len());
    for slot in &normalized {
        rendered.push(renderer.render(slot)?);
    }
    Ok(Some(RankNPatternShape {
        slots: rendered,
        binders: renderer.binders,
    }))
}

fn rank_n_binder_decls(
    shape: &RankNPatternShape,
    standard_names: &super::naming::StandardNames,
) -> String {
    shape
        .binders
        .iter()
        .map(|binder| {
            format!(
                "({} :: {})",
                binder.name,
                super::skin::haskell_kind(&binder.kind, standard_names)
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn rank_n_binder_args(shape: &RankNPatternShape) -> String {
    shape
        .binders
        .iter()
        .map(|binder| binder.name.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn explicit_type_arg(ty: &str) -> String {
    format!("@{}", ty_arg_paren(ty))
}

fn raw_product_type(slots: &[String]) -> String {
    match slots {
        [] => "()".to_owned(),
        [only] => only.clone(),
        [head, rest @ ..] => format!("({head}, {})", raw_product_type(rest)),
    }
}

fn raw_sum_type(slots: &[String], standard_names: &super::naming::StandardNames) -> String {
    match slots {
        [] => format!("{}.Void", standard_names.data_void),
        [only] => only.clone(),
        [head, rest @ ..] => format!(
            "Either {head_arg} {tail_arg}",
            head_arg = ty_arg_paren(head),
            tail_arg = ty_arg_paren(&raw_sum_type(rest, standard_names)),
        ),
    }
}

fn raw_product_expr(slots: &[String], fields: &[String]) -> String {
    match (slots, fields) {
        ([], []) => "()".to_owned(),
        ([_], [only]) => only.clone(),
        ([head, rest @ ..], [field, other_fields @ ..]) => format!(
            "(,) {} {} {field} ({})",
            explicit_type_arg(head),
            explicit_type_arg(&raw_product_type(rest)),
            raw_product_expr(rest, other_fields)
        ),
        _ => unreachable!("product pattern slots and fields have equal arity"),
    }
}

fn raw_product_projection(slots: &[String], index: usize, value: &str) -> String {
    let mut remaining = slots;
    let mut expression = value.to_owned();
    for _ in 0..index {
        let [head, rest @ ..] = remaining else {
            unreachable!("product projection index is in range");
        };
        expression = format!(
            "snd {} {} ({expression})",
            explicit_type_arg(head),
            explicit_type_arg(&raw_product_type(rest))
        );
        remaining = rest;
    }
    match remaining {
        [_] => expression,
        [head, rest @ ..] => format!(
            "fst {} {} ({expression})",
            explicit_type_arg(head),
            explicit_type_arg(&raw_product_type(rest))
        ),
        [] => unreachable!("product projection requires a slot"),
    }
}

fn render_rank_n_product_pattern(
    pattern_id: &PatternId,
    pattern: &str,
    selectors: &[String],
    shape: &RankNPatternShape,
    names: &StructuralNames,
    standard_names: &super::naming::StandardNames,
) -> (String, Vec<HaskellName>) {
    let view_type_id = HaskellName::RankNViewType(pattern_id.clone());
    let view_constructor_id = HaskellName::RankNViewConstructor(pattern_id.clone());
    let view_fn_id = HaskellName::RankNViewFunction(pattern_id.clone());
    let view_type = names.render(&view_type_id);
    let view_constructor = names.render(&view_constructor_id);
    let view_fn = names.render(&view_fn_id);
    let binder_decls = rank_n_binder_decls(shape, standard_names);
    let binder_args = rank_n_binder_args(shape);
    let data_binders = if binder_decls.is_empty() {
        String::new()
    } else {
        format!(" {binder_decls}")
    };
    let view_result = if binder_args.is_empty() {
        view_type.clone()
    } else {
        format!("{view_type} {binder_args}")
    };
    let forall = if binder_decls.is_empty() {
        String::new()
    } else {
        format!("forall {binder_decls}. ")
    };
    let fields = shape
        .slots
        .iter()
        .map(|slot| ty_arg_paren(slot))
        .collect::<Vec<_>>()
        .join(" ");
    let projections = (0..shape.slots.len())
        .map(|index| {
            format!(
                "({})",
                raw_product_projection(&shape.slots, index, "__kioValue")
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let signature_args = shape
        .slots
        .iter()
        .map(|slot| format!("{} -> ", fn_arg_paren(slot)))
        .collect::<String>();
    let built = raw_product_expr(&shape.slots, selectors);
    let declaration = format!(
        "data {view_type}{data_binders} = {view_constructor} {fields}\n\
         {view_fn} :: {forall}{raw_type} -> {view_result}\n\
         {view_fn} __kioValue = {view_constructor} {projections}\n\
         {{-# INLINE {view_fn} #-}}\n\
         pattern {pattern} :: {forall}{signature_args}{raw_type}\n\
         pattern {pattern} {{ {selectors} }} <- ({view_fn} -> {view_constructor} {selector_values})\n\
         \x20where\n\
         \x20  {pattern} {selector_values} = {built}\n",
        raw_type = raw_product_type(&shape.slots),
        selectors = selectors.join(", "),
        selector_values = selectors.join(" "),
    );
    (
        declaration,
        vec![view_type_id, view_constructor_id, view_fn_id],
    )
}

fn raw_sum_expr(
    slots: &[String],
    index: usize,
    payload: &str,
    standard_names: &super::naming::StandardNames,
) -> String {
    match slots {
        [_] if index == 0 => payload.to_owned(),
        [head, rest @ ..] if index == 0 => format!(
            "Left {} {} {payload}",
            explicit_type_arg(head),
            explicit_type_arg(&raw_sum_type(rest, standard_names))
        ),
        [head, rest @ ..] => format!(
            "Right {} {} ({})",
            explicit_type_arg(head),
            explicit_type_arg(&raw_sum_type(rest, standard_names)),
            raw_sum_expr(rest, index - 1, payload, standard_names)
        ),
        _ => unreachable!("sum pattern arm is in range"),
    }
}

fn raw_sum_view_expr(
    slots: &[String],
    index: usize,
    value: &str,
    result_type: &str,
    matched: &str,
    missed: &str,
    standard_names: &super::naming::StandardNames,
) -> String {
    match slots {
        [_] if index == 0 => format!("{matched} {value}"),
        [head, rest @ ..] if index == 0 => format!(
            "either {} {} {} (\\__kioPayload -> {matched} __kioPayload) (\\_ -> {missed}) ({value})",
            explicit_type_arg(head),
            explicit_type_arg(result_type),
            explicit_type_arg(&raw_sum_type(rest, standard_names))
        ),
        [head, rest @ ..] => format!(
            "either {} {} {} (\\_ -> {missed}) (\\__kioTail -> {}) ({value})",
            explicit_type_arg(head),
            explicit_type_arg(result_type),
            explicit_type_arg(&raw_sum_type(rest, standard_names)),
            raw_sum_view_expr(
                rest,
                index - 1,
                "__kioTail",
                result_type,
                matched,
                missed,
                standard_names,
            )
        ),
        _ => unreachable!("sum pattern arm is in range"),
    }
}

fn render_rank_n_sum_pattern(
    pattern_id: &SumPatternId,
    pattern: &str,
    index: usize,
    shape: &RankNPatternShape,
    names: &StructuralNames,
    standard_names: &super::naming::StandardNames,
) -> (String, Vec<HaskellName>) {
    let nested = PatternId::Sum(pattern_id.clone());
    let view_type_id = HaskellName::RankNViewType(nested.clone());
    let matched_id = HaskellName::RankNViewConstructor(nested.clone());
    let no_match_id = HaskellName::RankNNoMatch(pattern_id.clone());
    let view_fn_id = HaskellName::RankNViewFunction(nested);
    let view_type = names.render(&view_type_id);
    let no_match = names.render(&no_match_id);
    let matched = names.render(&matched_id);
    let view_fn = names.render(&view_fn_id);
    let binder_decls = rank_n_binder_decls(shape, standard_names);
    let binder_args = rank_n_binder_args(shape);
    let data_binders = if binder_decls.is_empty() {
        String::new()
    } else {
        format!(" {binder_decls}")
    };
    let view_result = if binder_args.is_empty() {
        view_type.clone()
    } else {
        format!("{view_type} {binder_args}")
    };
    let forall = if binder_decls.is_empty() {
        String::new()
    } else {
        format!("forall {binder_decls}. ")
    };
    let payload_type = &shape.slots[index];
    let raw_type = raw_sum_type(&shape.slots, standard_names);
    let view = raw_sum_view_expr(
        &shape.slots,
        index,
        "__kioValue",
        &view_result,
        &matched,
        &no_match,
        standard_names,
    );
    let built = raw_sum_expr(&shape.slots, index, "__x", standard_names);
    let declaration = format!(
        "data {view_type}{data_binders} = {no_match} | {matched} {payload}\n\
         {view_fn} :: {forall}{raw_type} -> {view_result}\n\
         {view_fn} __kioValue = {view}\n\
         {{-# INLINE {view_fn} #-}}\n\
         pattern {pattern} :: {forall}{payload} -> {raw_type}\n\
         pattern {pattern} __x <- ({view_fn} -> {matched} __x)\n\
         \x20where\n\
         \x20  {pattern} __x = {built}\n",
        payload = fn_arg_paren(payload_type),
    );
    (
        declaration,
        vec![view_type_id, matched_id, no_match_id, view_fn_id],
    )
}

/// Render one exported item as a typed boundary wrapper. Params are native
/// boundary types converted `In` to the private carrier; the result is the
/// internal module-fn call converted `Out` to the native boundary type. A
/// `()`-returning export returns `m ()`.
fn render_export_wrapper(
    e: &ExportEntry,
    shapes: &super::skin::HaskellShapes<'_>,
    facade: &HaskellFacadeCatalog,
    names: &HaskellNames,
) -> Result<String, EmitError> {
    let module = Some(e.module.as_str());
    let skin = super::skin::HaskellSkin { shapes, module };
    let name = &e.wrapper_name;
    let handle = &names.handle;
    let entry = facade.exact_site(&e.facade_site).entry();
    let scope = entry
        .stages()
        .iter()
        .filter_map(|stage| match stage {
            HaskellCallableHeadStage::Type(param) => Some(param.clone()),
            HaskellCallableHeadStage::Value { .. } => None,
        })
        .collect::<Vec<_>>();

    // Newtype identity: payload in, payload out (runtime-identity). The
    // wrapper threads the value through the package monad.
    if let ExportTarget::TransparentNewtype = e.target {
        let (slots, layout) = entry
            .stages()
            .iter()
            .find_map(|stage| match stage {
                HaskellCallableHeadStage::Value {
                    slots,
                    execution: Some(layout),
                } => Some((slots.as_slice(), layout)),
                _ => None,
            })
            .expect("a live newtype member has a prepared value stage");
        let [source] = layout.source_params() else {
            unreachable!("a newtype member has one source parameter")
        };
        let mut param_tys = Vec::with_capacity(slots.len());
        for slot in slots {
            param_tys.push(shapes.boundary_alias_haskell_type_in(
                slot.boundary(),
                slot.ty(),
                &scope,
                module,
            )?);
        }
        let arg_names = (0..slots.len())
            .map(|index| format!("arg{index}"))
            .collect::<Vec<_>>();
        let range = source.facade_slots();
        let value = match source.adapter() {
            CallableSourceParamAdapter::UnitValue => {
                assert!(range.is_empty());
                "()".to_owned()
            }
            CallableSourceParamAdapter::Identity => {
                assert_eq!(range.len(), 1);
                arg_names[range.start].clone()
            }
            CallableSourceParamAdapter::RightNest => {
                super::native::nest_tuple_value(&arg_names[range])
            }
        };
        let ret_ty = shapes.boundary_alias_haskell_type_in(
            entry.returned().boundary(),
            entry.returned().ty(),
            &scope,
            module,
        )?;
        let kind = &shapes.standard_names().data_kind;
        let mut sig = format!(
            "{name} :: forall (h :: {kind}.Type) (m :: {kind}.Type -> {kind}.Type). ({} h, Monad m) => {handle} h m",
            names.host_types_ty,
        );
        for param_ty in &param_tys {
            sig.push_str(" -> ");
            sig.push_str(&fn_arg_paren(param_ty));
        }
        sig.push_str(&format!(" -> m {}", ty_arg_paren(&ret_ty)));
        let params = arg_names
            .iter()
            .map(|arg| format!(" {arg}"))
            .collect::<String>();
        return Ok(format!("{sig}\n{name} _pkg{params} = pure ({value})\n"));
    }

    if let ExportTarget::NominalNewtype = e.target {
        unreachable!(
            "fallback_facade_blocker must reject prepared nominal newtype export `{}` before \
             universal Haskell wrapper rendering",
            e.wrapper_name
        );
    }

    let ExportTarget::ModuleFn { mangled } = &e.target else {
        unreachable!("newtype export targets handled above")
    };

    let mut param_decls: Vec<String> = Vec::new();
    let mut param_sig_tys: Vec<String> = Vec::new();
    let mut call_args: Vec<String> = Vec::new();
    let mut prelude: Vec<String> = Vec::new();
    let mut source_offset = 0usize;
    let mut public_offset = 0usize;
    let mut value_stage_index = 0usize;
    for stage in entry.stages() {
        let HaskellCallableHeadStage::Value {
            slots,
            execution: Some(layout),
        } = stage
        else {
            continue;
        };
        let group_size = e.group_sizes[value_stage_index];
        value_stage_index += 1;
        let source_group = &e.param_types[source_offset..source_offset + group_size];
        source_offset += group_size;
        assert_eq!(source_group.len(), layout.source_params().len());

        for slot in slots {
            let pname = format!("arg{public_offset}");
            public_offset += 1;
            param_sig_tys.push(shapes.boundary_alias_haskell_type_in(
                slot.boundary(),
                slot.ty(),
                &scope,
                module,
            )?);
            param_decls.push(pname);
        }
        let stage_public_start = public_offset - slots.len();
        for (source, pty) in layout.source_params().iter().zip(source_group) {
            let range = source.facade_slots();
            let internal = match (source.adapter(), pty) {
                (CallableSourceParamAdapter::UnitValue, _) => {
                    assert!(range.is_empty());
                    names.runtime_names.unit.clone()
                }
                (CallableSourceParamAdapter::Identity, Some(ty)) => {
                    assert_eq!(range.len(), 1);
                    skin.convert(
                        ty,
                        &param_decls[stage_public_start + range.start],
                        FfiDir::In,
                    )?
                }
                (CallableSourceParamAdapter::Identity, None) => {
                    assert_eq!(range.len(), 1);
                    param_decls[stage_public_start + range.start].clone()
                }
                (CallableSourceParamAdapter::RightNest, Some(ty)) => {
                    let parts = Type::right_spine_product(ty);
                    assert_eq!(parts.len(), range.len());
                    let converted = parts
                        .into_iter()
                        .zip(range)
                        .map(|(part, index)| {
                            skin.convert(part, &param_decls[stage_public_start + index], FfiDir::In)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    nest_kioprod(&names.runtime_names, &converted)
                }
                (CallableSourceParamAdapter::RightNest, None) => {
                    unreachable!("an untyped source parameter cannot own a product adapter")
                }
            };
            let kv = format!("__kv{}", call_args.len());
            prelude.push(format!("      let {{ {kv} = {internal} }}"));
            call_args.push(kv);
        }
    }
    assert_eq!(source_offset, e.param_types.len());
    assert_eq!(public_offset, param_decls.len());
    assert_eq!(value_stage_index, e.group_sizes.len());

    // Group 0's kvs saturate the module fn's top-level binders; each
    // later group is one private function layer applied through the runtime
    // helper — one argument per group (a private product
    // when the group has two or more params), mirroring the body
    // emitter's `emit_apply`.
    let g0 = e.group_sizes.first().copied().unwrap_or(call_args.len());
    let call = if call_args.is_empty() || g0 == 0 {
        format!("{mangled} _pkg")
    } else {
        format!("{mangled} _pkg {}", call_args[..g0].join(" "))
    };
    let mut chain: Vec<String> = Vec::new();
    let mut chain_offset = g0;
    for (gi, size) in e.group_sizes.iter().enumerate().skip(1) {
        let prev = if gi == 1 {
            "__f0".to_owned()
        } else {
            format!("__f{}", gi - 1)
        };
        let argv = if *size == 1 {
            call_args[chain_offset].clone()
        } else {
            let prod = format!("__gp{gi}");
            chain.push(format!(
                "      let {{ {prod} = {} }}",
                nest_kioprod(
                    &names.runtime_names,
                    &call_args[chain_offset..chain_offset + size]
                )
            ));
            prod
        };
        chain.push(format!(
            "      __f{gi} <- {} {prev} {argv}",
            names.runtime_names.call_function
        ));
        chain_offset += size;
    }
    let final_var = if e.group_sizes.len() > 1 {
        format!("__f{}", e.group_sizes.len() - 1)
    } else {
        "__f0".to_owned()
    };

    // Build the signature.
    let kind = &shapes.standard_names().data_kind;
    let mut forall = vec![
        format!("(h :: {kind}.Type)"),
        format!("(m :: {kind}.Type -> {kind}.Type)"),
    ];
    forall.extend(
        scope
            .iter()
            .map(|param| super::skin::kinded_haskell_binder(param, shapes.standard_names())),
    );
    let mut sig = format!(
        "{name} :: forall {}. ({} h, Monad m) => {handle} h m",
        forall.join(" "),
        names.host_types_ty
    );
    for pty in &param_sig_tys {
        sig.push_str(" -> ");
        sig.push_str(&fn_arg_paren(pty));
    }
    let ret_bty = shapes.boundary_alias_haskell_type_in(
        entry.returned().boundary(),
        entry.returned().ty(),
        &scope,
        module,
    )?;
    sig.push_str(&format!(" -> m {}", ty_arg_paren(&ret_bty)));

    let params_str = if param_decls.is_empty() {
        String::new()
    } else {
        format!(" {}", param_decls.join(" "))
    };

    let mut body = String::new();
    let emit_head = |body: &mut String| {
        body.push_str(&format!("{name} _pkg{params_str} = do\n"));
        for p in &prelude {
            body.push_str(p);
            body.push('\n');
        }
        body.push_str(&format!("      __f0 <- {call}\n"));
        for c in &chain {
            body.push_str(c);
            body.push('\n');
        }
    };
    if matches!(entry.returned().ty(), Type::Unit { .. }) {
        // Run the internal fn for its effects, discard the unit value.
        emit_head(&mut body);
        body.push_str(&format!("      _ <- pure {final_var}\n"));
        body.push_str("      pure ()\n");
    } else if shapes.is_passthrough_in(&e.ret_type, module) {
        emit_head(&mut body);
        body.push_str(&format!("      pure {final_var}\n"));
    } else {
        let converted = skin.convert(&e.ret_type, "__r", FfiDir::Out)?;
        emit_head(&mut body);
        body.push_str(&format!("      let {{ __r = {final_var} }}\n"));
        body.push_str(&format!("      pure ({converted})\n"));
    }

    Ok(format!("{sig}\n{body}"))
}

// =========================================================================
// Body visitor.
// =========================================================================

/// Walks a module fn body and renders a Haskell expression of type
/// a monadic private carrier — the value lifted into the package's monad. Host /
/// module calls reach `pkgHost _pkg` / the top-level module-fn functions.
struct BodyEmitter<'a, 'p> {
    /// In-scope Kio binder names, pushed/popped as the walk descends into
    /// `let` / `fn` / match-arm scopes. Carried so a future scope-sensitive
    /// rendering (shadowing-aware identifiers) has the binder stack.
    locals: Vec<String>,
    module_key: &'a str,
    selective_imports: &'a SelectiveImports,
    qualified_imports: &'a QualifiedImports,
    shapes: &'a super::skin::HaskellShapes<'p>,
    /// A monotonic counter for fresh `do`-bind / lambda variable names.
    fresh: std::cell::Cell<usize>,
}

impl<'a, 'p> BodyEmitter<'a, 'p> {
    fn new(
        module_key: &'a str,
        selective_imports: &'a SelectiveImports,
        qualified_imports: &'a QualifiedImports,
        shapes: &'a super::skin::HaskellShapes<'p>,
    ) -> Self {
        BodyEmitter {
            locals: Vec::new(),
            module_key,
            selective_imports,
            qualified_imports,
            shapes,
            fresh: std::cell::Cell::new(0),
        }
    }

    fn fresh_var(&self, prefix: &str) -> String {
        let n = self.fresh.get();
        self.fresh.set(n + 1);
        format!("__{prefix}{n}")
    }

    /// Render one Routed-phase expression to a monadic private carrier
    /// expression.
    fn emit_expr(&mut self, e: &Expr<Routed>) -> Result<String, EmitError> {
        match e {
            crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
            Expr::Unit { .. } => Ok(format!("pure {}", self.shapes.runtime_names().unit)),
            // Pre-Routed forms are gone by Routed (extension is `Never`).
            Expr::Path { ext, .. }
            | Expr::Call { ext, .. }
            | Expr::Tuple { ext, .. }
            | Expr::FnPlaceholder { ext, .. }
            | Expr::LabelValue { ext, .. }
            | Expr::RowLet { ext, .. }
            | Expr::Elaborator { ext, .. }
            | Expr::RecOrder { ext, .. }
            | Expr::RecQuote { ext, .. }
            | Expr::UserElaborator { ext, .. }
            | Expr::Ufcs { ext, .. }
            | Expr::OpChain { ext, .. }
            | Expr::RecCall { ext, .. } => match *ext {},

            Expr::StrLit { value, .. } => Ok(format!(
                "pure ({} {})",
                self.shapes.runtime_names().text,
                haskell_text_lit(value, self.shapes.standard_names())
            )),
            Expr::IntLit {
                digits, annotation, ..
            } => {
                let _ = annotation;
                Ok(format!(
                    "pure ({} ({}))",
                    self.shapes.runtime_names().int,
                    int_lit(digits)
                ))
            }
            Expr::FloatLit {
                digits, annotation, ..
            } => {
                let _ = annotation;
                Ok(format!(
                    "pure ({} ({}))",
                    self.shapes.runtime_names().double,
                    float_lit(digits)
                ))
            }
            Expr::BoolLit { value, .. } => {
                let b = if *value { "True" } else { "False" };
                Ok(format!("pure ({} {b})", self.shapes.runtime_names().bool_))
            }
            Expr::LowBoundRef { name, .. } => Ok(format!("pure {}", haskell_local_ident(name))),

            Expr::Let {
                name, value, body, ..
            } => self.emit_let(name, value, body),
            Expr::Seq { value, body, .. } => self.emit_seq(value, body),

            Expr::LowHostCall {
                name,
                module_path,
                args,
                sig,
                ret_ty,
                ..
            } => self.emit_low_host_call(name, module_path, args, sig, ret_ty),

            Expr::EnrichedTuple { items, .. } => self.emit_tuple(items),
            Expr::EnrichedRecord { fields, .. } => {
                let items: Vec<&Expr<Routed>> = fields.iter().map(|f| &f.value).collect();
                self.emit_tuple_refs(&items)
            }
            Expr::EnrichedProject {
                target,
                index,
                arity,
                ..
            } => self.emit_project(target, *index, *arity),
            Expr::EnrichedFieldGet {
                target,
                index,
                arity,
                ..
            } => self.emit_project(target, *index, *arity),
            Expr::EnrichedInject {
                payload,
                variant,
                variants,
                ..
            } => self.emit_inject(payload, *variant, *variants),
            Expr::EnrichedMatch {
                scrutinee, arms, ..
            } => self.emit_match(scrutinee, arms),
            Expr::EnrichedConditional {
                cond,
                then_branch,
                else_branch,
                ..
            } => self.emit_conditional(cond, then_branch, else_branch),

            Expr::LowModuleCall {
                mangled, args, sig, ..
            } => self.emit_module_call(mangled, args, sig),
            Expr::LowQualifiedModuleCall {
                mangled, args, sig, ..
            } => self.emit_qualified_module_call(mangled, args, sig),
            Expr::LowNewtypeCtor { payload, .. } => self.emit_expr(payload),
            Expr::LowNewtypeProj { target, .. } => self.emit_expr(target),
            Expr::LowQualifiedNewtypeMember { payload, .. } => self.emit_expr(payload),
            Expr::LowAbsurdCall { value_arg, .. } => {
                let v = self.fresh_var("absurd");
                let arg = self.emit_expr(value_arg)?;
                Ok(format!(
                    "({arg} >>= \\{v} -> {v} `seq` error \"kio: __absurd__ reached\")"
                ))
            }
            Expr::FnExpr { sig, body, .. } => self.emit_fn_expr(sig, body),
            Expr::LowClosureCall { name, args, .. } => {
                let callee = format!("pure {}", haskell_local_ident(name));
                self.emit_apply(&callee, args)
            }
            Expr::LowIndirectCall { callee, args, .. } => {
                let c = self.emit_expr(callee)?;
                self.emit_apply(&c, args)
            }
            Expr::LowTypeApplication { callee, .. } => {
                let c = self.emit_expr(callee)?;
                self.emit_apply(&c, &[])
            }
            Expr::LowHostFnValueRef {
                name,
                module_path,
                sig,
                ret_ty,
                ..
            } => self.emit_host_fn_value_ref(name, module_path, sig, ret_ty),
            Expr::LowModuleFnValueRef { mangled, sig, .. } => {
                self.emit_module_fn_value_ref(mangled, sig)
            }
            Expr::LowCpsProjectorApply {
                receiver,
                continuation,
                continuation_ty,
                ..
            } => self.emit_cps_projector_apply(receiver, continuation, continuation_ty),
        }
    }

    /// `let name = value in body` — force the bound value (Kio strictness)
    /// and thread effects through `m`. The value's monadic result is bound
    /// with `>>=`, forced with the private strictness helper, and the body continues.
    fn emit_let(
        &mut self,
        name: &str,
        value: &Expr<Routed>,
        body: &Expr<Routed>,
    ) -> Result<String, EmitError> {
        let v = self.emit_expr(value)?;
        let ident = haskell_local_ident(name);
        self.locals.push(name.to_owned());
        let b = self.emit_expr(body)?;
        self.locals.pop();
        // Forcing the bound value to WHNF makes a `let` evaluate when Kio's strict semantics
        // says, not lazily on first use.
        Ok(format!(
            "({v} >>= \\{ident}_raw -> let {{ {ident} = {} {ident}_raw }} in {ident} `seq` ({b}))",
            self.shapes.runtime_names().force
        ))
    }

    /// `value; body` — run `value` for its effects (forcing its result),
    /// then evaluate `body`.
    fn emit_seq(&mut self, value: &Expr<Routed>, body: &Expr<Routed>) -> Result<String, EmitError> {
        let v = self.emit_expr(value)?;
        let b = self.emit_expr(body)?;
        let tmp = self.fresh_var("seq");
        Ok(format!("({v} >>= \\{tmp} -> {tmp} `seq` ({b}))"))
    }

    /// `LowHostCall` → sequence each value-arg (monadic), convert it `Out`
    /// to the native boundary type, call the typed host field, and
    /// convert the result `In` back to the private carrier. A `()`-returning
    /// host fn yields private unit.
    fn emit_low_host_call(
        &mut self,
        name: &str,
        module_path: &str,
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
        ret_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let member = host_field_name(module_path, name);
        let param_tys = host_sig_value_param_types(sig);
        let ret_ty = signature_ret_type(sig, ret_ty);
        if param_tys.len() != args.len() {
            return Err(EmitError::unsupported(format!(
                "Haskell emitter: host fn `{name}` arity mismatch ({} params, {} args)",
                param_tys.len(),
                args.len()
            )));
        }
        // Evaluate each arg to its native boundary value, in order, binding
        // through `>>=` so effects fire left-to-right.
        let mut binds = String::new();
        let mut call_args: Vec<String> = Vec::new();
        let skin = super::skin::HaskellSkin {
            shapes: self.shapes,
            module: Some(module_path),
        };
        for (a, pty) in args.iter().zip(param_tys.iter()) {
            let va = self.emit_expr(a)?;
            let tmp = self.fresh_var("a");
            binds.push_str(&format!("{va} >>= \\{tmp} -> "));
            // Convert the private carrier to the host's native arg type.
            let native = match pty {
                Some(t) => skin.convert(t, &tmp, FfiDir::Out)?,
                None => tmp.clone(),
            };
            call_args.push(ty_arg_paren(&native));
        }
        let call_args_str = if call_args.is_empty() {
            String::new()
        } else {
            format!(" {}", call_args.join(" "))
        };
        let call = format!("{member} (pkgHost _pkg){call_args_str}");
        let ret = self.fresh_var("r");
        let result = if matches!(&ret_ty, Type::Unit { .. }) {
            format!(
                "{call} >>= \\{ret} -> {ret} `seq` pure {}",
                self.shapes.runtime_names().unit
            )
        } else if self.shapes.is_passthrough_in(&ret_ty, Some(module_path)) {
            // The host returns a native value that IS the internal rep
            // (unit / type-var). Pass it through.
            format!("{call} >>= \\{ret} -> pure {ret}")
        } else {
            // Convert the host's native return `In` to the internal rep.
            let converted = skin.convert(&ret_ty, &ret, FfiDir::In)?;
            format!("{call} >>= \\{ret} -> pure ({converted})")
        };
        Ok(format!("({binds}{result})"))
    }

    /// A product `(A & B & …)` literal → the nested-binary internal value
    /// after sequencing each element's effects in order. An n-ary product
    /// is right-folded into private `[head, tail]` cons cells (the same
    /// representation the projection helper and JS / Go bodies use), the last
    /// slot held bare; a 1-slot product is the bare value (no wrap).
    fn emit_tuple(&mut self, items: &[Expr<Routed>]) -> Result<String, EmitError> {
        let refs: Vec<&Expr<Routed>> = items.iter().collect();
        self.emit_tuple_refs(&refs)
    }

    fn emit_tuple_refs(&mut self, items: &[&Expr<Routed>]) -> Result<String, EmitError> {
        if items.is_empty() {
            return Ok(format!("pure {}", self.shapes.runtime_names().unit));
        }
        let mut binds = String::new();
        let mut slots: Vec<String> = Vec::new();
        for item in items {
            let v = self.emit_expr(item)?;
            let tmp = self.fresh_var("t");
            binds.push_str(&format!("{v} >>= \\{tmp} -> "));
            slots.push(tmp);
        }
        Ok(format!(
            "({binds}pure {})",
            nest_kioprod(self.shapes.runtime_names(), &slots)
        ))
    }

    /// Project slot `index` (of `arity`) from a product.
    fn emit_project(
        &mut self,
        target: &Expr<Routed>,
        index: usize,
        arity: usize,
    ) -> Result<String, EmitError> {
        let t = self.emit_expr(target)?;
        let tmp = self.fresh_var("p");
        Ok(format!(
            "({t} >>= \\{tmp} -> pure ({} {index} {arity} {tmp}))",
            self.shapes.runtime_names().project
        ))
    }

    /// Inject `payload` as variant `variant` of `variants` → the
    /// nested-binary tagged value (the same representation the JS / Go
    /// backends use, and the shape the recovery's `__left__` / `__right__`
    /// chains and the `loop` `s | r` ABI assume): a right-tagged sum wraps
    /// `variant` times around the left-tagged payload — or the bare payload
    /// after the wraps for the final variant, so the right-most arm is the
    /// nested remainder. A 1-variant sum is the bare payload.
    fn emit_inject(
        &mut self,
        payload: &Expr<Routed>,
        variant: usize,
        variants: usize,
    ) -> Result<String, EmitError> {
        let p = self.emit_expr(payload)?;
        let tmp = self.fresh_var("i");
        Ok(format!(
            "({p} >>= \\{tmp} -> pure {})",
            nest_kiosum(self.shapes.runtime_names(), variant, variants, &tmp)
        ))
    }

    /// Match on a nested-binary sum scrutinee. Tag 0 selects this arm and tag 1
    /// selects the remainder, so arm `k` is reached
    /// by peeling the tag-1 remainder `k` times: a non-final arm tests its
    /// peeled head's tag 0 and binds the head's payload; the final arm binds
    /// the fully-peeled value directly (it is the bare remainder). This is
    /// the shape the recovery's `__left__` / `__right__` chains and the host
    /// `loop`'s `s | r` ABI assume — a flat tag would collapse a sub-sum (a
    /// `A | (B | C)` widening) and lose its nesting.
    fn emit_match(
        &mut self,
        scrutinee: &Expr<Routed>,
        arms: &[crate::ast::EnrichedArm<Routed>],
    ) -> Result<String, EmitError> {
        let n = arms.len();
        if n == 0 {
            unreachable!(
                "Haskell emitter: EnrichedMatch with no arms; structural_recovery builds every \
                 EnrichedMatch from a sum's variants (recover_either / recover_dynamic_right \
                 yield >= 2 arms) and routes an uninhabited scrutinee through \
                 __absurd__/LowAbsurdCall, so a zero-arm match is unreachable"
            );
        }
        let s = self.emit_expr(scrutinee)?;
        let sv = self.fresh_var("m");
        let body = self.emit_match_arms(&sv, arms, 0)?;
        Ok(format!("({s} >>= \\{sv} -> {body})"))
    }

    /// Render the match-arm chain for arms `[k..]`, where `access` names the
    /// (already tag-1-peeled `k` times) sub-scrutinee expression. The
    /// non-final arm tests its tag-0 head and binds the head payload; the
    /// final arm binds the remaining value directly.
    fn emit_match_arms(
        &mut self,
        access: &str,
        arms: &[crate::ast::EnrichedArm<Routed>],
        k: usize,
    ) -> Result<String, EmitError> {
        let n = arms.len();
        let arm = &arms[k];
        let ident = haskell_local_ident(&arm.param);
        if k + 1 == n {
            // Final arm: the fully-peeled value is the payload directly.
            self.locals.push(arm.param.clone());
            let b = self.emit_expr(&arm.body)?;
            self.locals.pop();
            return Ok(format!(
                "(let {{ {ident} = {access} }} in {ident} `seq` ({b}))"
            ));
        }
        // Non-final arm: `access` is a tagged payload; tag 0 selects
        // this arm (binding `payload`), tag 1 recurses into the payload.
        let tagv = self.fresh_var("tag");
        let payv = self.fresh_var("pay");
        self.locals.push(arm.param.clone());
        let b = self.emit_expr(&arm.body)?;
        self.locals.pop();
        let rest = self.emit_match_arms(payv.as_str(), arms, k + 1)?;
        Ok(format!(
            "(let {{ ({tagv}, {payv}) = {} ({access}) }} in case {tagv} of {{ 0 -> let {{ {ident} = {payv} }} in {ident} `seq` ({b}); _ -> {rest} }})",
            self.shapes.runtime_names().match_tag
        ))
    }

    /// `if cond then t else e` → narrow the scrutinee to `Bool`, then a
    /// native `if`/`then`/`else`.
    fn emit_conditional(
        &mut self,
        cond: &Expr<Routed>,
        then_branch: &Expr<Routed>,
        else_branch: &Expr<Routed>,
    ) -> Result<String, EmitError> {
        let c = self.emit_expr(cond)?;
        let t = self.emit_expr(then_branch)?;
        let e = self.emit_expr(else_branch)?;
        let cv = self.fresh_var("c");
        Ok(format!(
            "({c} >>= \\{cv} -> if {} {cv} then ({t}) else ({e}))",
            self.shapes.runtime_names().as_bool
        ))
    }

    /// Lower a Kio closure `.(x, y) { body }` to a private function value.
    /// Each value group becomes one private function constructor; an `n>=2` group
    /// destructures the single product param into its binders. The closure
    /// returns the monadic private carrier.
    fn emit_fn_expr(
        &mut self,
        sig: &crate::ast::Signature<Routed>,
        body: &Expr<Routed>,
    ) -> Result<String, EmitError> {
        let groups = value_param_name_groups(sig);
        let mut pushed = 0usize;
        for g in &groups {
            for n in g {
                self.locals.push((*n).to_owned());
                pushed += 1;
            }
        }
        let body_src = self.emit_expr(body);
        for _ in 0..pushed {
            self.locals.pop();
        }
        let mut acc = body_src?;
        // Wrap each value group as a private function, innermost group first.
        // The lambda body must produce the monadic private carrier. The
        // innermost wrap already has that shape; every outer group's `inner`
        // is the previous bare function value, so it must lift with `pure` to meet
        // that kind (the currying `emit_module_fn` applies to its later
        // groups).
        for (i, g) in groups.iter().rev().enumerate() {
            let inner = if i == 0 { acc } else { format!("pure {acc}") };
            acc = self.wrap_group_lambda(g, inner);
        }
        // A closure value is itself a pure private-carrier function.
        Ok(format!("pure {acc}"))
    }

    /// Wrap a monadic private-carrier `inner` in the private function constructor for
    /// one value group. A 1-binder group binds the param directly; an
    /// `n>=2` group binds a fresh product and projects each binder.
    fn wrap_group_lambda(&self, g: &[&str], inner: String) -> String {
        if g.len() == 1 {
            let id = haskell_local_ident(g[0]);
            return format!(
                "({} (\\{id} -> {inner}))",
                self.shapes.runtime_names().function
            );
        }
        if g.is_empty() {
            // A nullary group: a thunk that ignores its (unit) arg.
            let v = self.fresh_var("z");
            return format!(
                "({} (\\{v} -> {v} `seq` {inner}))",
                self.shapes.runtime_names().function
            );
        }
        let n = g.len();
        let prod = self.fresh_var("g");
        let mut lets = String::new();
        for (i, b) in g.iter().enumerate() {
            let id = haskell_local_ident(b);
            lets.push_str(&format!(
                "let {{ {id} = {} {i} {n} {prod} }} in ",
                self.shapes.runtime_names().project
            ));
        }
        format!(
            "({} (\\{prod} -> {lets}{inner}))",
            self.shapes.runtime_names().function
        )
    }

    /// Apply a monadic private-carrier callee to a sequence of args. Kio currying
    /// is one product-arg per group at the ABI; a multi-arg group is built
    /// into a single product. We apply one arg at a time: each
    /// application evaluates the callee to a private function, the arg to a value,
    /// then calls.
    fn emit_apply(&mut self, callee: &str, args: &[Expr<Routed>]) -> Result<String, EmitError> {
        // Evaluate the callee once.
        let cv = self.fresh_var("f");
        let mut binds = format!("{callee} >>= \\{cv} -> ");
        if args.is_empty() {
            // A nullary Kio closure call `f()` — a `() -> R` closure applied
            // to the unit argument the empty value group passes at the ABI.
            // The function was emitted taking a unit param, so apply it to
            // private unit.
            return Ok(format!(
                "({binds}{} {cv} {})",
                self.shapes.runtime_names().call_function,
                self.shapes.runtime_names().unit
            ));
        }
        // Build the single argument (Kio ABI: one product per call group;
        // here `args` is the flat value-arg list of one group).
        let argv = if args.len() == 1 {
            let v = self.emit_expr(&args[0])?;
            let tmp = self.fresh_var("x");
            binds.push_str(&format!("{v} >>= \\{tmp} -> "));
            tmp
        } else {
            // Multiple args = a product param.
            let mut slots: Vec<String> = Vec::new();
            for a in args {
                let v = self.emit_expr(a)?;
                let tmp = self.fresh_var("x");
                binds.push_str(&format!("{v} >>= \\{tmp} -> "));
                slots.push(tmp);
            }
            let prod = self.fresh_var("xp");
            binds.push_str(&format!(
                "let {{ {prod} = {} }} in ",
                nest_kioprod(self.shapes.runtime_names(), &slots)
            ));
            prod
        };
        Ok(format!(
            "({binds}{} {cv} {argv})",
            self.shapes.runtime_names().call_function
        ))
    }

    fn emit_qualified_module_call(
        &mut self,
        mangled: &str,
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let (alias, leaf) = mangled.split_once('.').ok_or_else(|| {
            EmitError::unsupported(format!(
                "Haskell emitter: qualified module call `{mangled}` is not `<alias>.<leaf>`"
            ))
        })?;
        let module_key = self.qualified_imports.get(alias).ok_or_else(|| {
            EmitError::unsupported(format!(
                "Haskell emitter: qualified-call alias `{alias}` has no `import … as {alias};` mapping"
            ))
        })?;
        let fn_name = module_fn_name(module_key, leaf);
        self.emit_grouped_module_call(&fn_name, args, sig)
    }

    fn emit_module_call(
        &mut self,
        mangled: &str,
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let fn_name = match self.selective_imports.get(mangled) {
            Some(m) => m.clone(),
            None => module_fn_name(self.module_key, mangled),
        };
        self.emit_grouped_module_call(&fn_name, args, sig)
    }

    /// Call a top-level module fn. The first group's args become positional
    /// private-carrier parameters; later groups apply through the returned
    /// function value. Under-application yields a partial function value.
    fn emit_grouped_module_call(
        &mut self,
        fn_name: &str,
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let group_arities = value_group_arities(sig);
        let first = group_arities.first().copied().unwrap_or(0);

        // Evaluate all args (monadic), in order.
        let mut binds = String::new();
        let mut argvs: Vec<String> = Vec::new();
        for a in args {
            let v = self.emit_expr(a)?;
            let tmp = self.fresh_var("x");
            binds.push_str(&format!("{v} >>= \\{tmp} -> "));
            argvs.push(tmp);
        }

        if args.len() < first {
            // Under-applied: build a private function capturing the args so far and
            // taking the rest of the first group as its single param. The
            // function receives one private carrier: when one arg is missing, the
            // param IS that arg; when two+ are missing, the param is a
            // product the closure projects each remaining slot from.
            let missing = first - args.len();
            let prod = self.fresh_var("pp");
            let mut proj_args = argvs.clone();
            if missing == 1 {
                proj_args.push(prod.clone());
            } else {
                for i in 0..missing {
                    proj_args.push(format!(
                        "({} {i} {missing} {prod})",
                        self.shapes.runtime_names().project
                    ));
                }
            }
            let inner = format!("{fn_name} _pkg {}", proj_args.join(" "));
            return Ok(format!(
                "({binds}pure ({} (\\{prod} -> {inner})))",
                self.shapes.runtime_names().function
            ));
        }

        // Fully (or over-) applied. The first `first` args are positional;
        // remaining groups apply through the returned private function.
        let first_args = &argvs[..first];
        let mut call = if first_args.is_empty() {
            format!("{fn_name} _pkg")
        } else {
            format!("{fn_name} _pkg {}", first_args.join(" "))
        };
        let mut consumed = first;
        for arity in group_arities.iter().skip(1) {
            if consumed >= argvs.len() {
                break;
            }
            let slice = &argvs[consumed..(consumed + arity).min(argvs.len())];
            let arg = nest_kioprod(self.shapes.runtime_names(), slice);
            // `call` returns a private function in the package monad; apply it.
            let fv = self.fresh_var("g");
            call = format!(
                "({call} >>= \\{fv} -> {} {fv} {arg})",
                self.shapes.runtime_names().call_function
            );
            consumed += arity;
        }
        Ok(format!("({binds}{call})"))
    }

    /// `LowHostFnValueRef` → a private function forwarding through the host record
    /// with the same FFI conversion as a direct host call.
    fn emit_host_fn_value_ref(
        &mut self,
        name: &str,
        module_path: &str,
        sig: &crate::ast::Signature<Routed>,
        ret_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let member = host_field_name(module_path, name);
        let param_tys = host_sig_value_param_types(sig);
        let ret_ty = signature_ret_type(sig, ret_ty);
        let skin = super::skin::HaskellSkin {
            shapes: self.shapes,
            module: Some(module_path),
        };
        // The host fn's value form takes its value params as a single
        // group (curried into one product param when arity >= 2).
        let arity = param_tys.len();
        let arg = self.fresh_var("p");
        // Extract each native arg from the product param (or use it
        // directly when unary).
        let mut natives: Vec<String> = Vec::new();
        for (i, pty) in param_tys.iter().enumerate() {
            let slot = if arity == 1 {
                arg.clone()
            } else {
                format!(
                    "({} {i} {arity} {arg})",
                    self.shapes.runtime_names().project
                )
            };
            let native = match pty {
                Some(t) => skin.convert(t, &slot, FfiDir::Out)?,
                None => slot,
            };
            natives.push(ty_arg_paren(&native));
        }
        let natives_str = if natives.is_empty() {
            String::new()
        } else {
            format!(" {}", natives.join(" "))
        };
        let call = format!("{member} (pkgHost _pkg){natives_str}");
        let rv = self.fresh_var("hr");
        let body = if matches!(&ret_ty, Type::Unit { .. }) {
            format!(
                "{call} >>= \\{rv} -> {rv} `seq` pure {}",
                self.shapes.runtime_names().unit
            )
        } else if self.shapes.is_passthrough_in(&ret_ty, Some(module_path)) {
            format!("{call} >>= \\{rv} -> pure {rv}")
        } else {
            let converted = skin.convert(&ret_ty, &rv, FfiDir::In)?;
            format!("{call} >>= \\{rv} -> pure ({converted})")
        };
        Ok(format!(
            "pure ({} (\\{arg} -> {body}))",
            self.shapes.runtime_names().function
        ))
    }

    /// `LowModuleFnValueRef` → a private function forwarding to the resolved
    /// module-fn function. The module fn's first group is the product
    /// param of the closure value.
    fn emit_module_fn_value_ref(
        &mut self,
        mangled: &str,
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let fn_name = match self.selective_imports.get(mangled) {
            Some(m) => m.clone(),
            None => module_fn_name(self.module_key, mangled),
        };
        let group_arities = value_group_arities(sig);
        let first = group_arities.first().copied().unwrap_or(0);
        let arg = self.fresh_var("p");
        let mut call_args: Vec<String> = Vec::new();
        for i in 0..first {
            if first == 1 {
                call_args.push(arg.clone());
            } else {
                call_args.push(format!(
                    "({} {i} {first} {arg})",
                    self.shapes.runtime_names().project
                ));
            }
        }
        let call = if call_args.is_empty() {
            format!("{fn_name} _pkg")
        } else {
            format!("{fn_name} _pkg {}", call_args.join(" "))
        };
        // Later groups apply on the returned private function; the value-form
        // exposes the first group as the closure param and returns whatever
        // the call returns (a further function for multi-group fns).
        Ok(format!(
            "pure ({} (\\{arg} -> {call}))",
            self.shapes.runtime_names().function
        ))
    }

    /// CPS-projector-apply: an existential newtype projection in CPS form.
    /// The receiver's payload is handed to the continuation. For the
    /// non-HKT corpus the newtype is runtime-identity, so this is `cont
    /// receiver` — evaluate the receiver, apply the continuation to the
    /// unwrapped payload.
    fn emit_cps_projector_apply(
        &mut self,
        receiver: &Expr<Routed>,
        continuation: &Expr<Routed>,
        continuation_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let recv = self.emit_expr(receiver)?;
        let cont = self.emit_expr(continuation)?;
        let rv = self.fresh_var("cr");
        let mut cv = self.fresh_var("ck");
        let (type_stages, callable) = continuation_ty.peel_leading_foralls();
        let Type::Function { abi_arity, .. } = callable else {
            unreachable!("a routed CPS projector continuation has a function type")
        };
        assert!(
            *abi_arity <= 1,
            "a routed CPS projector continuation has zero or one ABI slot"
        );
        let mut body = format!("{cont} >>= \\{cv} -> ");
        for _ in 0..type_stages {
            let next = self.fresh_var("ct");
            body.push_str(&format!(
                "{} {cv} {} >>= \\{next} -> ",
                self.shapes.runtime_names().call_function,
                self.shapes.runtime_names().unit
            ));
            cv = next;
        }
        let payload = if *abi_arity == 0 {
            self.shapes.runtime_names().unit.as_str()
        } else {
            &rv
        };
        body.push_str(&format!(
            "{} {cv} {payload}",
            self.shapes.runtime_names().call_function
        ));
        Ok(format!("({recv} >>= \\{rv} -> {body})"))
    }
}

fn value_group_arities(sig: &crate::ast::Signature<Routed>) -> Vec<usize> {
    let arities: Vec<usize> = sig
        .canonical_groups()
        .into_iter()
        .filter_map(|group| match group {
            crate::ast::SignatureGroupRef::Value(params) => Some(
                params
                    .iter()
                    .filter(|p| matches!(p, crate::ast::SignatureParam::Value(_)))
                    .count(),
            ),
            crate::ast::SignatureGroupRef::Type(_) => None,
        })
        .collect();
    if arities.is_empty() { vec![0] } else { arities }
}

pub(super) fn host_sig_value_param_types(
    sig: &crate::ast::Signature<Routed>,
) -> Vec<Option<Type<Routed>>> {
    value_group_param_types(sig).into_iter().flatten().collect()
}

// =========================================================================
// Literal + identifier helpers.
// =========================================================================

/// Render a Kio integer literal as a Haskell `Integer` expression
/// (parenthesized negatives handled by the caller). The private integer
/// constructor carries `Integer`, so the literal is exact at every width.
fn int_lit(digits: &str) -> String {
    digits.to_owned()
}

/// Build the nested-binary internal product value from already-rendered
/// slot expressions. An n-ary product is right-folded into private
/// `[head, tail]` cons cells (the representation the projection helper
/// reads, matching the JS / Go bodies), the last slot held bare; a 1-slot
/// product is the bare slot (no wrap), and an empty product is private unit.
pub(super) fn nest_kioprod(
    runtime_names: &super::naming::RuntimeNames,
    slots: &[String],
) -> String {
    match slots {
        [] => runtime_names.unit.clone(),
        [only] => only.clone(),
        [head, rest @ ..] => format!(
            "({} [{head}, {}])",
            runtime_names.product,
            nest_kioprod(runtime_names, rest)
        ),
    }
}

/// Build the nested-binary internal sum value injecting an already-rendered
/// `payload` as `variant` of `variants`: a right-tagged value wraps `variant`
/// times around a left-tagged payload, or the bare payload after the
/// wraps for the final variant (the right-most arm is the nested
/// remainder). A 1-variant sum is the bare payload. Matches the JS / Go
/// encoding the private tag helper reads.
pub(super) fn nest_kiosum(
    runtime_names: &super::naming::RuntimeNames,
    variant: usize,
    variants: usize,
    payload: &str,
) -> String {
    let mut acc = if variant + 1 < variants {
        format!("({} 0 ({payload}))", runtime_names.sum)
    } else {
        payload.to_owned()
    };
    for _ in 0..variant {
        acc = format!("({} 1 ({acc}))", runtime_names.sum);
    }
    acc
}

/// Render a Kio float literal as a Haskell `Double` expression.
fn float_lit(digits: &str) -> String {
    // Haskell requires a digit before `.` and after; Kio digits already
    // carry a well-formed decimal. Ensure a leading-`.`/trailing-`.` is not
    // produced — Kio's lexer disallows those, so pass through.
    digits.to_owned()
}

/// Render a Kio string value as a Haskell `Text` value without relying on
/// `OverloadedStrings`. The standard-module qualifier is allocated for this
/// package, so a facade named `Data.Text` cannot capture the reference.
fn haskell_text_lit(value: &str, standard_names: &super::naming::StandardNames) -> String {
    format!(
        "({}.pack {})",
        standard_names.data_text,
        haskell_string_lit(value)
    )
}

/// Render a Kio string value as a Haskell double-quoted `String` literal.
fn haskell_string_lit(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\{}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The Haskell local identifier for a Kio binder `name`. Prefixed with `k_`
/// so it never collides with a Haskell keyword or the emitter's own
/// identifiers, and sanitized to the Haskell identifier grammar.
fn haskell_local_ident(name: &str) -> String {
    let mut s = String::with_capacity(name.len() + 2);
    s.push_str("k_");
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '\'' {
            s.push(c);
        } else {
            s.push('_');
        }
    }
    s
}
