//! Haskell backend — IR → Haskell package lowering.
//!
//! Consumes a `Package<Routed>` (post-recovery + post-resolution-lowering)
//! and produces a self-contained Haskell package as a [`HaskellPackage`].
//!
//! ## Body representation
//!
//! The native body ([`super::native`]) renders Routed IR at its Haskell
//! types: an abstract application `F(A)` becomes `f a`, parametric source
//! newtypes retain declaration-stable nominal heads, and host types remain
//! exact associated-family applications. Private universal carriers and
//! their conversions support the typed FFI skin ([`super::runtime`]).
//!
//! Kio's strict evaluation is preserved in Haskell's lazy host:
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
//! exported surface. The prepared facade plans its topology,
//! [`super::skin::HaskellShapes`] renders its types, and [`super::native`]
//! renders body-to-boundary conversions. Every emitted function is
//! **monad-polymorphic** (`Monad m => …`); the host record is a value
//! record of `m`-returning functions (not a typeclass); the exported
//! surface is `Monad m => …`.

use std::collections::BTreeMap;

use crate::ast::{Kind, Routed, Type, TypeParam};
use crate::backends::boundary_facade::{BoundaryFacadeSiteOwner, PreparedBoundaryCallableSites};
use crate::backends::haskell::facade::{
    HaskellCallableHeadStage, HaskellFacadeCatalog, HaskellStructuralOccurrence,
};
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
    let mut wrapper_names = Vec::new();
    for site in facade.sites().filter(|site| site.entry().is_live()) {
        let module = site.id().module_segments().join("/");
        match site.id().owner() {
            BoundaryFacadeSiteOwner::HostFunction { .. } => {}
            BoundaryFacadeSiteOwner::ExportedFunction { name } => {
                wrapper_names.push(export_wrapper_name(&module, name));
            }
            BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member }
            | BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => {
                wrapper_names.push(export_newtype_wrapper_name(&module, newtype, member));
            }
        }
    }
    wrapper_names.sort();
    export_list.extend(wrapper_names);

    // The FFI boundary aliases: a stable-named `type` synonym per compound
    // boundary slot (and a per-arm `pattern` synonym for a sum slot), so a
    // host implementation — and the per-backend test runner, which reads no
    // emitted file — can name each family application through the stable
    // semantic-name ABI (`Host_…` / `Export_…` / `Member_…`). Flat product and
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

    let native = super::native::render_native_body(package, names, &ffi.decls, prepared)?;
    for name in &native.export_names {
        export_list.push(name.clone());
    }
    let extra_pragmas = native.extra_pragmas;
    decls.push_str(&native.host_record);
    decls.push('\n');
    decls.push_str(&shared_decls);
    decls.push_str(&native.type_decls);
    decls.push_str(&native.module_fns);
    decls.push_str(&native.export_wrappers);

    decls.push('\n');
    decls.push_str(&super::runtime_declarations(
        package,
        prepared,
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
        .filter(|(_, _, alias)| references_standard(&decls, alias))
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

fn references_standard(declarations: &str, alias: &str) -> bool {
    let reference = format!("{alias}.");
    declarations
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            !line.starts_with("--") && !line.starts_with("{ -- |") && !line.starts_with(", -- |")
        })
        .any(|line| line.contains(&reference))
}

pub(super) fn render_documented_declarations(
    mut pieces: Vec<(String, Option<&crate::ast::DocComment>)>,
) -> String {
    pieces.sort_by(|(left, _), (right, _)| left.cmp(right));
    let mut out = String::new();
    for (body, doc) in pieces {
        out.push('\n');
        push_doc(&mut out, doc, "");
        out.push_str(&body);
    }
    out
}

pub(super) fn push_doc(out: &mut String, doc: Option<&crate::ast::DocComment>, indent: &str) {
    if let Some(doc) = doc {
        let body = crate::backends::doc::render(doc, crate::backends::doc::Dialect::Haddock);
        crate::backends::doc::haddock(out, &body, indent);
    }
}

pub(super) fn documented_field(doc: Option<&crate::ast::DocComment>, signature: &str) -> String {
    let mut out = String::new();
    push_doc(&mut out, doc, "");
    out.push_str(signature);
    out.replace('\n', "\n    ")
}

#[cfg(all(test, feature = "surface"))]
mod facade_tests {
    use std::path::{Path, PathBuf};

    use crate::ast::{Expr, HostFnParam, Item, Kind, Routed, SignatureParam, Type};
    use crate::backends::haskell::naming::{
        BoundaryId, BoundaryRoot, BoundaryStep, HaskellName, ItemId, ModuleItemId, StructuralKey,
        SumPatternId,
    };
    use crate::pass::full::FullPipeline;
    use crate::pass::resolve::{Package, PackageFileEntry};
    use crate::pipeline::Pipeline;

    use super::{HaskellNames, rank_n_pattern_shape, render_pkg_hs};

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
        crate::pass::route::route_package(&crate::pass::structural_recovery::recover_package(
            &prime,
        ))
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
    fn native_docs_cover_declaration_renderers() {
        let package = routed_package_files_with_bridge(
            &[(
                "api.kio",
                "module api;\n\
            /// HOSTTYPEDOC\n\
            host type Value;\n\
            /// HOSTFNDOC\n\
            host fn effect() -> .;\n\
            /// OPAQUEDOC\n\
            pub newtype Token : . { pub constructor make; projector read; };\n\
            /// EXPORTDOC\n\
            pub fn call() -> . { effect() }\n\
            /// PRIVATEDOC\n\
            fn hidden() -> . { () }",
            )],
            "api;",
        );
        let native = render_package(&package).expect("native documented facade");
        for marker in ["HOSTTYPEDOC", "HOSTFNDOC", "OPAQUEDOC", "EXPORTDOC"] {
            assert!(native.contains(&format!("-- | {marker}")), "{native}");
        }
        assert!(!native.contains("PRIVATEDOC"));
        assert!(!super::references_standard(
            "-- | Alias.value\nf = ()",
            "Alias"
        ));
        assert!(!super::references_standard(
            "  { -- | Alias.value\n    field :: ()\n  , -- | Alias.other\n    other :: ()\n  }",
            "Alias"
        ));
        assert!(super::references_standard(
            "-- | prose\nf = Alias.value",
            "Alias"
        ));
    }

    #[test]
    fn native_docs_do_not_choose_declaration_order() {
        let source =
            "module api;\n/// ZZZ\npub fn alpha() -> . { () }\n/// AAA\npub fn zeta() -> . { () }";
        let package = routed_package_files_with_bridge(&[("api.kio", source)], "api;");
        let native = render_package(&package).expect("native docs ordering");
        assert!(native.find("-- | ZZZ").unwrap() < native.find("-- | AAA").unwrap());
        let plain = super::render_documented_declarations(vec![
            ("zeta :: ()\n".to_owned(), None),
            ("alpha :: ()\n".to_owned(), None),
        ]);
        assert_eq!(plain, "\nalpha :: ()\n\nzeta :: ()\n");
    }

    #[test]
    fn public_callable_facades_retain_monad_specialization_unfoldings() {
        let native = render(
            "module api; \
             host fn tick() -> .; \
             pub fn direct() -> . { () } \
             pub fn effectful() -> . { tick() } \
             pub newtype Box[A] : A { pub constructor mk; pub projector un; };",
        )
        .expect("native facade unfoldings");
        for wrapper in [
            super::export_wrapper_name("api", "direct"),
            super::export_wrapper_name("api", "effectful"),
            super::export_newtype_wrapper_name("api", "Box", "mk"),
            super::export_newtype_wrapper_name("api", "Box", "un"),
        ] {
            assert!(native.contains(&format!("{{-# INLINABLE {wrapper} #-}}\n{wrapper} ::")));
        }
        assert_eq!(native.matches("{-# INLINABLE ").count(), 4);
        assert!(!native.contains("{-# INLINE "));
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

    fn inject_unknown_newtype_constructor(package: &mut Package<Routed>) {
        for entry in package.modules_mut() {
            for item in &mut entry.module.items {
                let Item::FnDef(function) = item else {
                    continue;
                };
                if function.name != "native_error" {
                    continue;
                }
                let value = function.body.clone();
                let meta = value.meta().clone();
                function.body = crate::ast::Expr::LowNewtypeCtor {
                    occurrence: Default::default(),
                    newtype: "MissingNativeErrorType".to_owned(),
                    member: "make".to_owned(),
                    type_args: Vec::new(),
                    payload: Box::new(value),
                    meta,
                    ext: (),
                };
                return;
            }
        }
        panic!("native-error fixture has a private `native_error` function");
    }

    #[test]
    fn native_emitter_errors_propagate() {
        let mut package = routed_package("module api; fn native_error() -> . { () }");
        inject_unknown_newtype_constructor(&mut package);
        let error = render_package(&package).expect_err("native emitter errors propagate");
        assert_eq!(
            error.message,
            "Haskell native body: newtype ctor `MissingNativeErrorType` is neither carrier nor existential"
        );
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
            let projection = equation.find("case (k_source) of").unwrap_or_else(|| {
                panic!("fixture did not emit a raw tuple projection: {equation}")
            });
            let annotation = projection
                + equation[projection..]
                    .find(&expected_type)
                    .unwrap_or_else(|| {
                        panic!("rank-N projected callee lacks its exact type: {equation}")
                    });
            let visible_argument = annotation
                + equation[annotation..].find(" @").unwrap_or_else(|| {
                    panic!("type annotation does not precede application: {equation}")
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
    fn generic_sum_boundaries_demand_payloads_and_convert_callable_slots() {
        let emitted = render(
            "module api; \
             host fn choose[A][B](step: . -> (A | B)) -> A | B; \
             fn roundtrip[A][B](step: . -> (A | B)) -> A | B { choose(A, B, step) } \
             host fn thunks(value: (. -> .) | .) -> (. -> .) | .; \
             fn converted(value: (. -> .) | .) -> (. -> .) | . { thunks(value) }",
        )
        .expect("render generic and callable sum boundaries");
        let equation = |name: &str| {
            let prefix = format!("{} _pkg", module_fn("api", name));
            emitted
                .lines()
                .find(|line| line.starts_with(&prefix))
                .expect("module equation")
        };
        let unchanged = equation("roundtrip");
        assert!(unchanged.contains("host__api__choose"), "{unchanged}");
        assert!(unchanged.contains("case"), "{unchanged}");
        assert!(unchanged.contains("`seq`"), "{unchanged}");
        assert_eq!(
            unchanged.matches("host__api__choose").count(),
            1,
            "{unchanged}"
        );
        let converted = equation("converted");
        assert!(converted.contains("case"), "{converted}");
        assert!(converted.contains("__hfd"), "{converted}");
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
        let marker = ">>= \\__hfr0 -> kioPure @";
        let lifts = callback_equation
            .match_indices(marker)
            .map(|(index, _)| {
                visible_parenthesized_type_application(&callback_equation[index..], marker)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            lifts,
            [concrete.as_str(), concrete.as_str()],
            "prepared callback inward and outward exact lifts: {callback_equation}"
        );

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
    fn generic_host_calls_check_demand_under_the_declared_schema() {
        let emitted = render(
            "module api; \
             host fn invoke[A](step: A -> A, value: A) -> A; \
             host fn keep[*F][A](value: F(A)) -> F(A); \
             host fn obtain[A](_value: A) -> [B] B -> B; \
             host fn observe() -> .; \
             pub fn anonymous() { invoke(., .(value) { value }, observe()) } \
             pub fn opaque[*G][T](value: G(T)) -> G(T) { keep(G, T, value) } \
             pub fn returned() -> [B] B -> B { obtain(., ()) }",
        )
        .expect("generic host-call schemas");
        let equation = |name: &str| {
            let prefix = format!("{} _pkg", module_fn("api", name));
            emitted
                .lines()
                .find(|line| line.starts_with(&prefix))
                .expect("module equation")
        };
        let anonymous = equation("anonymous");
        let signature = anonymous.find(":: forall").expect("local schema signature");
        let schema = anonymous[..signature]
            .rfind("let {")
            .expect("local whole-call schema");
        assert!(anonymous[..schema].contains("kioEval"), "{anonymous}");
        assert!(
            anonymous[..schema].contains("host__api__observe"),
            "{anonymous}"
        );
        assert_eq!(
            anonymous.matches("host__api__observe").count(),
            1,
            "{anonymous}"
        );
        assert_eq!(
            anonymous.matches("host__api__invoke").count(),
            1,
            "{anonymous}"
        );
        assert!(
            anonymous[schema..].contains(&standard_hs(
                ":: forall (t_a :: %KIND%.Type). (t_a -> m t_a) -> t_a -> m t_a"
            )),
            "{anonymous}"
        );
        assert!(
            anonymous[schema..].contains("(pkgHost _pkg) @t_a"),
            "{anonymous}"
        );
        let opaque = equation("opaque");
        assert!(opaque.contains(&standard_hs(
            ":: forall (t_f :: %KIND%.Type -> %KIND%.Type) (t_a :: %KIND%.Type). (t_f t_a) -> m (t_f t_a)"
        )), "{opaque}");
        assert!(opaque.contains("(pkgHost _pkg) @t_f @t_a"), "{opaque}");
        assert!(opaque.contains("@t_g @t_t"), "{opaque}");
        let returned = equation("returned");
        assert!(returned.contains(&standard_hs(
            ":: forall (t_a :: %KIND%.Type). t_a -> m (forall (t_b :: %KIND%.Type). m (t_b -> m t_b))"
        )), "{returned}");
    }

    #[test]
    fn public_host_fields_preserve_source_parameters() {
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
                ":: forall (t_a :: %KIND%.Type) (t_b :: %KIND%.Type). (t_a, t_b) -> m ()"
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
    fn discarded_values_keep_exact_rank_n_types_and_strictness() {
        let mut package = routed_package_files_with_bridge(
            &[(
                "api.kio",
                "module api; \
             host fn observe() -> .; \
             newtype Receiver : [A][B] A -> A { constructor mk; projector get }; \
             fn identity[A][B](value: A) -> A { value } \
             fn factory() -> Receiver { observe(); Receiver.mk(identity) } \
             fn pure_discard(value: Receiver) -> . { let unused = Receiver.get(value); () } \
             fn effectful_discard() -> . { let unused = Receiver.get(factory()); observe() } \
             fn ordinary_discard() -> . { observe(); () }",
            )],
            "**;",
        );
        let mut replaced = 0;
        for entry in package.modules_mut() {
            for item in &mut entry.module.items {
                let Item::FnDef(function) = item else {
                    continue;
                };
                if !matches!(function.name.as_str(), "pure_discard" | "effectful_discard") {
                    continue;
                }
                let Expr::Let {
                    value, body, meta, ..
                } = &function.body
                else {
                    panic!("discard fixture begins with its unused binding")
                };
                function.body = Expr::Seq {
                    occurrence: Default::default(),
                    value: value.clone(),
                    body: body.clone(),
                    meta: meta.clone(),
                };
                replaced += 1;
            }
        }
        assert_eq!(replaced, 2);
        let emitted = render_package(&package).expect("render discarded values");
        let equation = |name: &str| {
            let prefix = format!("{} _pkg", module_fn("api", name));
            emitted
                .lines()
                .find(|line| line.starts_with(&prefix))
                .expect("module equation")
        };
        let pure = equation("pure_discard");
        assert!(pure.contains("seq @(forall"), "{pure}");
        assert!(!pure.contains("kioBind"), "{pure}");
        let effectful = equation("effectful_discard");
        assert!(effectful.contains("kioBind @(forall"), "{effectful}");
        assert!(effectful.contains("seq @(forall"), "{effectful}");
        assert!(!effectful.contains(" >> "), "{effectful}");
        let ordinary = equation("ordinary_discard");
        assert!(ordinary.contains(" >>= "), "{ordinary}");
        assert!(ordinary.contains(" `seq` "), "{ordinary}");
        assert!(!ordinary.contains("kioBind"), "{ordinary}");
    }

    #[test]
    fn native_rank_n_sum_injections_keep_typed_constructors_and_demand() {
        let emitted = render(
            "module api; import __intrinsics__; \
             host type I32 role(i32); \
             host fn produce() -> [A] I32; \
             type Stage = [A] I32; \
             fn left(value: [A] I32) -> ([A] I32) | . { \
               __left__([A] I32, ., value) \
             } \
             fn right(value: [A] I32) -> . | ([A] I32) { \
               __right__(., [A] I32, value) \
             } \
             fn effectful() -> . | ([A] I32) { \
               __right__(., [A] I32, produce()) \
             } \
             fn alias(value: Stage) -> . | Stage { \
               __right__(., Stage, value) \
             } \
             fn nested(value: . & Stage) -> . | (. & Stage) { \
               __right__(., . & Stage, value) \
             }",
        )
        .expect("render native rank-N sum injections");
        for (name, constructor, effectful) in [
            ("left", "Left @(forall", false),
            ("right", "Right @() @(forall", false),
            ("effectful", "Right @() @(forall", true),
            ("alias", "Right @() @(forall", false),
            ("nested", "Right @() @(", false),
        ] {
            let prefix = format!("{} _pkg", module_fn("api", name));
            let body = emitted
                .lines()
                .find(|line| line.starts_with(&prefix))
                .expect("module equation");
            assert!(body.contains(constructor), "{body}");
            assert!(body.contains("seq @("), "{body}");
            assert!(body.contains("forall"), "{body}");
            assert_eq!(body.contains("kioBind @(forall"), effectful, "{body}");
        }
    }

    #[test]
    fn effectful_residual_module_call_uses_rank_n_lift() {
        let result_type = standard_hs(
            "forall (t_a :: %KIND%.Type). m (forall (t_b :: %KIND%.Type). m ((t_b, t_b) -> m t_b))",
        );
        for (argument, effectful) in [
            (".[B](left: B, _right: B) -> B { left }", false),
            ("produce()", true),
        ] {
            let emitted = render(&format!(
                "module api; \
                 host fn produce() -> [B] (B & B) -> B; \
                 fn delay[X](value: X)[A] -> X {{ value }} \
                 fn caller() -> . {{ let source = delay({argument}); () }}",
            ))
            .expect("render Haskell");
            let caller = module_fn("api", "caller");
            let body = emitted
                .lines()
                .find(|line| line.starts_with(&format!("{caller} _pkg =")))
                .expect("caller body");

            assert_eq!(
                body.contains(&format!("kioPure @({result_type})")),
                effectful,
                "{body}"
            );
            assert!(body.contains(&format!("seq @({result_type})")), "{body}");
            if effectful {
                assert!(body.contains(&host_field("api", "produce")), "{body}");
                assert!(body.contains("kioBind @(forall"), "{body}");
            } else {
                assert!(
                    body.contains(&format!("k_source :: {result_type}")),
                    "{body}"
                );
                assert!(!body.contains("kioBind"), "{body}");
            }
        }
    }

    #[test]
    fn strict_pure_residual_argument_keeps_the_inner_forall_annotation() {
        let emitted = render(
            "module api; \
             fn staged[A](_left: A)[B][C](_middle: B, right: C) -> C { right } \
             pub fn run() { staged((), (), ()) }",
        )
        .expect("render strict residual argument");
        let caller = module_fn("api", "run");
        let body = emitted
            .lines()
            .find(|line| line.starts_with(&format!("{caller} _pkg =")))
            .expect("caller body");
        let residual = body
            .split_once(" `seq` ")
            .expect("supplied argument is demanded before returning the residual")
            .1;
        assert!(residual.starts_with("(((\\ @t_b ->"), "{body}");
        let expected = standard_hs(
            ":: forall (t_b :: %KIND%.Type). m (forall (t_c :: %KIND%.Type). m ((t_b, t_c) -> m t_c))",
        );
        assert!(residual.contains(&expected), "{body}");
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
        let locals = (0..WIDTH)
            .map(|index| format!("k_p{index}"))
            .collect::<Vec<_>>();
        let pattern = locals
            .iter()
            .rev()
            .cloned()
            .reduce(|tail, head| format!("({head}, {tail})"))
            .expect("nonempty closure domain");
        assert_eq!(
            make.matches(&format!("\\{pattern} ->")).count(),
            1,
            "{make}"
        );
        assert_eq!(make.matches('\\').count(), 1, "{make}");
        let demanded = (0..WIDTH)
            .map(|index| format!("__m{index}"))
            .collect::<Vec<_>>();
        let demand = locals
            .iter()
            .zip(&demanded)
            .map(|(source, value)| format!("let {{ {value} = {source} }} in {value} `seq` "))
            .collect::<String>();
        assert!(
            make.contains(&format!(
                "{demand}{} _pkg {}",
                module_fn("api", "consume"),
                demanded.join(" ")
            )),
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
    fn opaque_application_arguments_keep_canonical_callable_domains() {
        let output = render(
            "module api; host type N; host type Box[A]; \
             pub fn boxed(value: Box(. -> N)) -> Box(. -> N) { value } \
             pub fn abstract_box[*F](value: F(. -> N)) -> F(. -> N) { value } \
             pub fn direct(value: . -> N) -> . -> N { value } \
             pub fn compound(value: Box((. -> N) & ([A] A -> A))) \
                 -> Box((. -> N) & ([A] A -> A)) { value } \
             pub fn visible(value: (. -> N) & ([A] A -> A)) \
                 -> (. -> N) & ([A] A -> A) { value }",
        )
        .expect("render opaque native callable arguments");
        let line = |prefix: &str| {
            output
                .lines()
                .find(|line| line.starts_with(prefix))
                .unwrap_or_else(|| panic!("missing {prefix}: {output}"))
        };
        for name in ["boxed", "abstractBox"] {
            let signature = line(&format!("export__api__{name} ::"));
            let head = if name == "boxed" {
                "HostType__api__Box h"
            } else {
                "t_f"
            };
            assert!(
                signature.contains(&format!("{head} (() -> m (HostType__api__N h))")),
                "{signature}"
            );
        }
        let direct = line("export__api__direct ::");
        assert!(!direct.contains("() -> m"), "{direct}");
        let nested = line("type Export_api__compound_Arg0_App0 ");
        assert!(nested.contains("() -> m"), "{nested}");
        let visible = line("type Export_api__visible_Result ");
        assert!(!visible.contains("() -> m"), "{visible}");
    }

    #[test]
    fn sum_boundary_conversion_retains_exact_polymorphic_arms() {
        let output = render(
            "module api; host type N; \
             pub fn direct_choice(value: (. -> N) | ([A] A -> A)) \
                 -> (. -> N) | ([A] A -> A) { value } \
             pub fn ordinary(value: N | .) -> N | . { value }",
        )
        .expect("render polymorphic sum conversion");
        let direct = output
            .lines()
            .find(|line| line.starts_with("export__api__directChoice _pkg"))
            .expect("direct sum wrapper");
        assert_eq!(direct.matches("either @").count(), 2, "{direct}");
        assert!(
            direct.contains("either @(m (HostType__api__N h)) @(Either (() -> m"),
            "{direct}"
        );
        assert!(
            direct.contains("either @(() -> m (HostType__api__N h)) @(Either (m"),
            "{direct}"
        );
        assert_eq!(direct.matches("Left @").count(), 2, "{direct}");
        assert_eq!(direct.matches("Right @").count(), 2, "{direct}");
        assert_eq!(direct.matches("(arg0)").count(), 1, "{direct}");
        assert!(!direct.contains("case "), "{direct}");

        let ordinary = output
            .lines()
            .find(|line| line.starts_with("export__api__ordinary _pkg"))
            .expect("ordinary sum wrapper");
        assert_eq!(ordinary.matches("(arg0)").count(), 1, "{ordinary}");
        assert_eq!(ordinary.matches("case ").count(), 2, "{ordinary}");
        assert_eq!(ordinary.matches("`seq`").count(), 4, "{ordinary}");
        assert_eq!(
            ordinary.matches(&module_fn("api", "ordinary")).count(),
            1,
            "{ordinary}"
        );
    }

    #[test]
    fn sum_boundary_conversion_covers_nested_arms_and_callback_legs() {
        let output = render(
            "module api; host type N; \
             host fn echo(value: (. -> N) | ([A] A -> A)) \
                 -> (. -> N) | ([A] A -> A); \
             pub fn via_host(value: (. -> N) | ([A] A -> A)) \
                 -> (. -> N) | ([A] A -> A) { echo(value) } \
             pub fn three(value: (. -> N) | . | ([A] A -> A)) \
                 -> (. -> N) | . | ([A] A -> A) { value } \
             pub fn callback(value: (. -> N) | ([A] A -> A), \
                 apply: ((. -> N) | ([A] A -> A)) -> ((. -> N) | ([A] A -> A))) \
                 -> (. -> N) | ([A] A -> A) { apply(value) } \
             pub fn nested[A](value: ((. -> A) | ([B] B -> B)) & N) \
                 -> ((. -> A) | ([B] B -> B)) & N { value }",
        )
        .expect("render nested and callback sum conversion");
        let wrapper = |name: &str| {
            output
                .lines()
                .find(|line| line.starts_with(&format!("export__api__{name} _pkg")))
                .unwrap_or_else(|| panic!("missing wrapper {name}: {output}"))
        };
        assert_eq!(wrapper("three").matches("either @").count(), 4);
        assert_eq!(wrapper("three").matches("(arg0)").count(), 1);
        assert_eq!(wrapper("callback").matches("either @").count(), 4);
        assert_eq!(wrapper("nested").matches("either @").count(), 2);
        assert!(wrapper("nested").contains("either @(m t_a)"));
        let host_body = output
            .lines()
            .find(|line| line.contains("__api__viaHost _pkg k_value ="))
            .expect("native host-calling body");
        assert_eq!(host_body.matches("either @").count(), 2, "{host_body}");
    }

    #[test]
    fn wide_polymorphic_sum_conversion_does_not_duplicate_arm_adapters() {
        let mut small_size = None;
        for arity in [4, 16, 32] {
            let mut arms = vec![". -> N"; arity - 1];
            arms.push("[A] A -> A");
            let sum = arms
                .iter()
                .map(|arm| format!("({arm})"))
                .collect::<Vec<_>>()
                .join(" | ");
            let output = render(&format!(
                "module api; host type N; \
                 pub fn wide(value: {sum}) -> {sum} {{ value }}"
            ))
            .expect("render wide polymorphic boundary sum");
            let wrapper = output
                .lines()
                .find(|line| line.starts_with("export__api__wide _pkg"))
                .expect("wide wrapper");
            assert_eq!(wrapper.matches("either @").count(), 2 * (arity - 1));
            assert_eq!(wrapper.matches("Left @").count(), 2 * (arity - 1));
            assert_eq!(wrapper.matches("Right @").count(), 2 * (arity - 1));
            assert_eq!(wrapper.matches("\\ @t_a ->").count(), 2);
            assert_eq!(wrapper.matches("(arg0)").count(), 1);
            if let Some(bytes) = small_size {
                // Explicit tail type arguments repeat type text quadratically;
                // arm converters and constructor/eliminator nodes stay linear.
                assert!(wrapper.len() <= bytes * (arity / 4).pow(2));
            } else {
                small_size = Some(wrapper.len());
            }
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
            rank_n_pattern_shape(&slots, &scope, Some("api"), &shapes, false)
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
    fn rank_n_patterns_keep_canonical_domains_only_inside_opaque_arguments() {
        let package = routed_package(
            "module api; host type N; host type Box[A]; \
             host fn nested(value: Box((. -> N) & ([A] A -> A))) -> .;",
        );
        let names = HaskellNames::derive("FallbackProbe");
        let shapes = crate::backends::haskell::skin::HaskellShapes::new(&package, &names);
        let declaration = package
            .modules()
            .flat_map(|(_, entry)| &entry.module.items)
            .find_map(|item| match item {
                Item::HostFn(declaration) => Some(declaration),
                _ => None,
            })
            .expect("nested host function");
        let ty = declaration
            .params
            .iter()
            .find_map(|param| match param {
                HostFnParam::Value(param) => Some(&param.ty),
                _ => None,
            })
            .expect("host value parameter");
        let Type::Path { args, .. } = ty else {
            panic!("host Box application")
        };
        let slots = Type::right_spine_product(&args[0]);
        let native = rank_n_pattern_shape(&slots, &[], Some("api"), &shapes, true)
            .expect("opaque argument pattern")
            .expect("rank-N slot");
        let direct = rank_n_pattern_shape(&slots, &[], Some("api"), &shapes, false)
            .expect("direct boundary pattern")
            .expect("rank-N slot");
        assert!(native.slots[0].contains("() ->"), "{:?}", native.slots);
        assert!(!direct.slots[0].contains("() ->"), "{:?}", direct.slots);
        let nested = rank_n_pattern_shape(&[ty], &[], Some("api"), &shapes, false)
            .expect("nested opaque application")
            .expect("rank-N argument");
        assert!(nested.slots[0].contains("() ->"), "{:?}", nested.slots);
        let Type::Product { left, right, meta } = &args[0] else {
            panic!("compound argument")
        };
        let sum = Type::Sum {
            left: left.clone(),
            right: right.clone(),
            meta: meta.clone(),
        };
        for canonical in [false, true] {
            let shape = rank_n_pattern_shape(&[&sum], &[], Some("api"), &shapes, canonical)
                .expect("sum context")
                .expect("rank-N arm");
            assert_eq!(
                shape.slots[0].contains("() ->"),
                canonical,
                "{:?}",
                shape.slots
            );
        }
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
                        .render(&normalized, false)
                        .expect("render normalized boundary representation")
                })
                .collect::<Vec<_>>();
            assert_eq!(rendered[0], rendered[1], "{function_name}");
            assert_eq!(
                rendered[0],
                standard_hs("forall (rq0 :: %KIND%.Type). rp0 (rq0 -> rp0 rq0)"),
                "{function_name}"
            );
            rank_n_pattern_shape(&slots, &[], Some("api"), &shapes, false)
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
        let shape = rank_n_pattern_shape(&slots, &scope, Some("api"), &shapes, false)
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
            rendered, "t_f (t_a, t_b)",
            "scoped constructor was reinterpreted"
        );
    }

    #[test]
    fn native_renderer_publishes_the_structural_facade() {
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
            assert!(!signature(&native).contains(&names.runtime_names.opaque));
        }
    }

    #[test]
    fn native_renderer_resolves_scoped_module_function_import() {
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
        let rendered = render_package(&package).expect("native scoped import");
        let caller = module_fn("api/main", "run");
        let equation = rendered
            .lines()
            .find(|line| line.starts_with(&format!("{caller} _pkg")))
            .expect("the importing function's equation");
        let provider = module_fn("api/internal", "message");
        let importer = module_fn("api/main", "message");
        assert!(equation.contains(&provider), "{equation}");
        assert!(!equation.contains(&importer), "{equation}");
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
    fn bottom_boundary_is_void_in_native_facade() {
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
    fn native_emitter_errors_propagate_for_public_nominals() {
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
            // An invalid routed owner pins native emitter-error ownership.
            let source = format!(
                "module api; {declaration} \
                 fn native_error() -> . {{ () }}"
            );
            let mut package = routed_package_files_with_bridge(&[("api.kio", &source)], "**;");
            inject_unknown_newtype_constructor(&mut package);
            let error = render_package(&package).expect_err("native emitter errors propagate");
            assert_eq!(
                error.message,
                "Haskell native body: newtype ctor `MissingNativeErrorType` is neither carrier nor existential",
                "{surface}"
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
    fn native_host_record_resolves_bare_newtypes_in_the_declaring_module() {
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
        let output = render_package(&package).expect("native host record");
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
    fn polymorphic_function_newtype_members_preserve_product_parameters() {
        let output = render(
            "module api; \
             pub newtype Pick_first : [A] (A & A) -> A { \
               pub constructor mk_pick_first; pub projector pick_first; \
             };",
        )
        .expect("render polymorphic function newtype members");
        let grouped = standard_hs("forall (t_a :: %KIND%.Type). m ((t_a, t_a) -> m t_a)");

        for wrapper in [
            newtype_export_wrapper("api", "Pick_first", "mk_pick_first"),
            newtype_export_wrapper("api", "Pick_first", "pick_first"),
        ] {
            let signature = output
                .lines()
                .find(|line| line.starts_with(&format!("{wrapper} ::")))
                .unwrap_or_else(|| panic!("missing `{wrapper}` signature"));
            assert_eq!(signature.matches(&grouped).count(), 2, "{signature}");
        }
    }
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
/// - host fns get typed `Host_…` argument / return aliases;
/// - exports and newtype members get typed `Export_…` / `Member_…` aliases;
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
            emit_compound_type_alias(boundary, ty, occurrence, scope, module, shapes, facade, acc)?;
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
            let (decl, helpers) = if let Some(shape) = rank_n_pattern_shape(
                &product_slots,
                scope,
                module,
                shapes,
                boundary
                    .tail
                    .iter()
                    .any(|step| matches!(step, BoundaryStep::App(_))),
            )? {
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
            emit_compound_type_alias(boundary, ty, occurrence, scope, module, shapes, facade, acc)?;
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
            let rank_n_shape = rank_n_pattern_shape(
                &sum_slots,
                scope,
                module,
                shapes,
                boundary
                    .tail
                    .iter()
                    .any(|step| matches!(step, BoundaryStep::App(_))),
            )?;
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
#[allow(clippy::too_many_arguments)]
fn emit_compound_type_alias(
    boundary: &BoundaryId,
    ty: &Type<Routed>,
    occurrence: &HaskellStructuralOccurrence,
    scope: &[crate::ast::TypeParam],
    module: Option<&str>,
    shapes: &super::skin::HaskellShapes<'_>,
    facade: &HaskellFacadeCatalog,
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
                .boundary_alias_haskell_type_in(facade, slot.boundary(), slot.ty(), scope, module)
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

    fn render(&mut self, ty: &Type<Routed>, canonical: bool) -> Result<String, EmitError> {
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
                    let arg = self.render(arg, true)?;
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
                let params = Type::right_spine_take(param, *abi_arity)
                    .into_iter()
                    .map(|param| self.render(param, canonical))
                    .collect::<Result<Vec<_>, _>>()?;
                let params = if canonical {
                    vec![raw_product_type(&params)]
                } else {
                    params
                };
                for param in params {
                    rendered.push_str(&fn_arg_paren(&param));
                    rendered.push_str(" -> ");
                }
                let ret = self.render(ret, canonical)?;
                rendered.push_str(&effect);
                rendered.push(' ');
                rendered.push_str(&ty_arg_paren(&ret));
                Ok(format!("({rendered})"))
            }
            Type::Product { .. } => {
                let slots = Type::right_spine_product(ty)
                    .into_iter()
                    .map(|slot| self.render(slot, canonical))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(raw_product_type(&slots))
            }
            Type::Sum { .. } => {
                let slots = Type::right_spine_sum(ty)
                    .into_iter()
                    .map(|slot| self.render(slot, canonical))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(raw_sum_type(&slots, self.shapes.standard_names()))
            }
            Type::Forall { param, body, .. } => {
                let name = format!("rq{}", self.forall_depth);
                self.forall_depth += 1;
                self.local_binders.push((param.name.clone(), name.clone()));
                let body = self.render(body, canonical)?;
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
    canonical: bool,
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
        rendered.push(renderer.render(slot, canonical)?);
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

pub(super) fn raw_sum_expr(
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

pub(super) fn typed_either(
    source: (&str, &str),
    result: &str,
    handlers: (&str, &str),
    value: &str,
) -> String {
    format!(
        "either {} {} {} ({}) ({}) ({value})",
        explicit_type_arg(source.0),
        explicit_type_arg(result),
        explicit_type_arg(source.1),
        handlers.0,
        handlers.1,
    )
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
        [head, rest @ ..] if index == 0 => typed_either(
            (head, &raw_sum_type(rest, standard_names)),
            result_type,
            (
                &format!("\\__kioPayload -> {matched} __kioPayload"),
                &format!("\\_ -> {missed}"),
            ),
            value,
        ),
        [head, rest @ ..] => {
            let tail = raw_sum_view_expr(
                rest,
                index - 1,
                "__kioTail",
                result_type,
                matched,
                missed,
                standard_names,
            );
            typed_either(
                (head, &raw_sum_type(rest, standard_names)),
                result_type,
                (
                    &format!("\\_ -> {missed}"),
                    &format!("\\__kioTail -> {tail}"),
                ),
                value,
            )
        }
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
