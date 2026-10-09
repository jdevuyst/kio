//! Documentation joins use the selected live site's exact Routed declaration.

use crate::ast::{DocComment, Item, Routed};
use crate::pass::resolve::{Package, TopLevelDeclaration, declaration_by_id};

use super::boundary_facade::{
    BoundaryFacadeSiteOwner, BoundaryHostBinding, BoundaryHostBindingOrigin,
    BoundaryPublicNewtypeInventoryEntry, PreparedBoundaryCallableSite, QualifiedTypeName,
};

fn declaration<'a>(
    package: &'a Package<Routed>,
    name: &QualifiedTypeName,
) -> TopLevelDeclaration<'a, Routed> {
    let module_path = name.module_segments().join("/");
    let entry = package
        .module(&module_path)
        .expect("a live boundary type names its Routed module");
    let id = entry
        .scope
        .lookup(name.name())
        .expect("a live boundary type names its Routed declaration");
    declaration_by_id(&entry.module, id).expect("the Routed scope names an existing declaration")
}

pub(crate) fn host_type<'a>(
    package: &'a Package<Routed>,
    binding: &BoundaryHostBinding,
) -> Option<&'a DocComment> {
    if !matches!(binding.origin(), BoundaryHostBindingOrigin::Live) {
        return None;
    }
    match declaration(package, binding.name()) {
        TopLevelDeclaration::Item(Item::HostType(declaration)) => declaration.doc.as_ref(),
        _ => unreachable!("a live host-type binding retains its exact declaration role"),
    }
}

pub(crate) fn newtype<'a>(
    package: &'a Package<Routed>,
    entry: &BoundaryPublicNewtypeInventoryEntry,
) -> Option<&'a DocComment> {
    declaration(package, entry.name())
        .newtype()
        .expect("a live public newtype retains its exact declaration role")
        .doc
        .as_ref()
}

pub(crate) fn callable<'a>(
    package: &'a Package<Routed>,
    site: PreparedBoundaryCallableSite<'_>,
) -> Option<&'a DocComment> {
    site.execution()?;
    let name = match site.site().owner() {
        BoundaryFacadeSiteOwner::HostFunction { name }
        | BoundaryFacadeSiteOwner::ExportedFunction { name } => name,
        // A newtype's prose describes its nominal presentation, not its members.
        BoundaryFacadeSiteOwner::NewtypeConstructor { .. }
        | BoundaryFacadeSiteOwner::NewtypeProjector { .. } => return None,
    };
    let module_path = site.site().module_segments().join("/");
    let entry = package
        .module(&module_path)
        .expect("a live boundary site names its Routed module");
    let id = entry
        .scope
        .lookup(name)
        .expect("a live boundary site names its Routed declaration");
    let declaration = declaration_by_id(&entry.module, id)
        .expect("the Routed scope names an existing declaration");
    match (site.site().owner(), declaration) {
        (
            BoundaryFacadeSiteOwner::HostFunction { .. },
            TopLevelDeclaration::Item(Item::HostFn(declaration)),
        ) => declaration.doc.as_ref(),
        (
            BoundaryFacadeSiteOwner::ExportedFunction { .. },
            TopLevelDeclaration::Item(Item::FnDef(declaration)),
        ) if declaration.vis.is_exported() => declaration.doc.as_ref(),
        _ => unreachable!("a live callable boundary retains its exact declaration role"),
    }
}

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::backends::boundary_facade::PreparedBoundaryCallableSites;
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::{parse, parse_package_file};
    use crate::pass::resolve::PackageFileEntry;
    use crate::pipeline::Pipeline;
    use std::path::{Path, PathBuf};

    fn package() -> Package<Routed> {
        package_from_sources(&[
            (
                "a.kio",
                "module a;\n/// First export.\npub fn same() { () }\n/// First host.\nhost fn supply() -> .;\n/// Private prose.\nfn hidden() { () }\n/// Replacement private prose.\nfn old() { () }\n",
            ),
            (
                "b.kio",
                "module b;\n/// Second export.\npub fn same() { () }\npub fn undocumented() { () }\n",
            ),
            (
                "c.kio",
                "module c;\n/// Unbridged prose.\npub fn same() { () }\n",
            ),
        ])
    }

    fn package_from_sources(sources: &[(&str, &str)]) -> Package<Routed> {
        let modules = sources
            .iter()
            .map(|(path, source)| (PathBuf::from(path), parse(source).unwrap()))
            .collect();
        let package_file = parse_package_file("package fixture; bridge { a; b; }", None).unwrap();
        let (modules, package_file) =
            FullPipeline::lower_package(modules, Some(package_file)).unwrap();
        let package = Package::build(
            Path::new(""),
            modules,
            package_file.map(|package_file| PackageFileEntry {
                file_path: PathBuf::from("fixture.pkg.kio"),
                package_name: "fixture".into(),
                package_file,
            }),
        )
        .unwrap();
        package.resolve_imports().unwrap();
        package.check_in_body_resolution().unwrap();
        let prime = FullPipeline::typecheck(&package).unwrap();
        crate::pass::route::route_package(&crate::pass::structural_recovery::recover_package(
            &prime,
        ))
    }

    #[test]
    fn exact_live_selection_preserves_docs_without_private_or_unbridged_entries() {
        let package = package();
        let prepared = PreparedBoundaryCallableSites::collect(&package, None).unwrap();
        let observed = prepared
            .sites()
            .map(|site| {
                (
                    site.site().module_segments().join("/"),
                    format!("{:?}", site.site().owner()),
                    callable(&package, site).map(|doc| doc.lines.join("\n")),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(observed.len(), 4);
        assert_eq!(
            observed
                .iter()
                .filter_map(|(_, _, doc)| doc.as_deref())
                .collect::<Vec<_>>(),
            ["First host.", "First export.", "Second export."]
        );
        assert!(observed.iter().any(|(module, owner, doc)| module == "b"
            && owner.contains("undocumented")
            && doc.is_none()));
        assert!(
            !observed
                .iter()
                .any(|(module, owner, _)| module == "c" || owner.contains("hidden"))
        );
    }

    #[test]
    fn retained_site_cannot_read_same_name_current_private_docs() {
        let package = package();
        let signature = crate::pass::parser::parse_signature_file(
            "signature fixture v(3); v(1) { nonbreaking { add { module a { host fn old() -> .; } } } } v(2) { breaking { remove { module a { old; } } } }",
            None,
        ).unwrap();
        let replayed = crate::sig::replay(&signature).unwrap();
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed)).unwrap();
        let old = prepared.sites().find(|site| matches!(site.site().owner(), BoundaryFacadeSiteOwner::HostFunction { name } if name == "old")).expect("retained host site");
        assert!(old.execution().is_none());
        assert!(callable(&package, old).is_none());
    }

    #[test]
    fn python_serializes_only_documented_export_presentations() {
        let package = package();
        let ir = super::super::serialized_runtime_ir::python_package_ir(&package, None).unwrap();
        let json = serde_json::to_value(ir).unwrap();
        let sites = json["boundary"]["sites"].as_array().unwrap();
        assert_eq!(sites.len(), 4);
        let documented = sites
            .iter()
            .filter_map(|site| {
                site.get("doc")
                    .map(|doc| (site["module"][0].as_str().unwrap(), doc.as_str().unwrap()))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            documented,
            [("a", "First export."), ("b", "Second export.")]
        );
        let host = sites
            .iter()
            .find(|site| site["owner"]["kind"] == "host_function")
            .unwrap();
        assert!(host.get("doc").is_none());
    }

    #[test]
    fn native_facade_docs_cover_methods_and_nominal_declarations() {
        let package = package_from_sources(&[
            (
                "a.kio",
                "module a;\n/// Host constructor prose.\nhost type Box[A];\n/// Host method prose.\nhost fn supply() -> .;\n/// Public function prose.\npub fn same() -> . { supply() }\n/// Nominal prose.\npub newtype Token : . { pub constructor make; pub projector read; };\n",
            ),
            ("b.kio", "module b;"),
        ]);
        let prepared = PreparedBoundaryCallableSites::collect(&package, None).unwrap();
        let binding = prepared
            .host_bindings()
            .find(|binding| binding.name().name() == "Box")
            .unwrap();
        assert_eq!(
            host_type(&package, binding).unwrap().lines,
            ["Host constructor prose."]
        );
        let nominal = prepared.public_newtypes().next().unwrap();
        assert_eq!(
            newtype(&package, nominal).unwrap().lines,
            ["Nominal prose."]
        );
        assert!(
            prepared
                .sites()
                .filter(|site| matches!(
                    site.site().owner(),
                    BoundaryFacadeSiteOwner::NewtypeConstructor { .. }
                        | BoundaryFacadeSiteOwner::NewtypeProjector { .. }
                ))
                .all(|site| callable(&package, site).is_none())
        );
        let js =
            super::super::js::emit::lower_package_to_factory_module(&package, "fixture").unwrap();
        let ts = super::super::ts::emit::lower_package_to_dts(&package, "fixture").unwrap();
        let py = super::super::python::stub::lower_package_to_stub(&package, "fixture").unwrap();
        for text in [&js, &ts, &py] {
            for prose in [
                "Host method prose.",
                "Public function prose.",
                "Nominal prose.",
            ] {
                assert!(text.contains(prose), "missing {prose}: {text}");
            }
        }
        assert!(ts.contains("Host constructor prose."), "{ts}");
        assert!(py.contains("Host constructor prose."), "{py}");
        assert!(
            py.contains("-> None:\n        \"Host method prose.\"\n        ..."),
            "{py}"
        );
    }

    #[test]
    fn documentation_cannot_select_javascript_runtime_helpers() {
        let markers = "__kioOpaqueOut( __kioOpaqueIn( __kioProductSlots( __kioProductSlot( __kioSumPayload( __kioSumInject(";
        let benign = "Documentation about this declaration.";
        for source in [
            "module a;\npub fn unit() -> . { () }\npub newtype Token : . { pub constructor make; pub projector read; };\n",
            "module a;\npub newtype Opaque : . { pub constructor make; projector read; };\npub fn pair(value: . & .) -> . & . { value }\n",
        ] {
            let emit = |prose| {
                let source = source.replace("\npub ", &format!("\n/// {prose}\npub "));
                let package = package_from_sources(&[("a.kio", &source), ("b.kio", "module b;")]);
                super::super::js::emit::lower_package_to_factory_module(&package, "fixture")
                    .unwrap()
            };
            let js = emit(markers);
            // Both declaration comments also appear in the factory's documentation.
            assert_eq!(js.matches(markers).count(), 4, "{js}");
            assert_eq!(js.replace(markers, benign), emit(benign));
        }
    }

    #[test]
    fn typescript_nominal_witnesses_keep_the_selected_declarations_docs() {
        let package = package_from_sources(&[
            (
                "a.kio",
                "module a;\n/// Opaque nominal prose.\npub newtype Opaque : . { pub constructor make; projector read; };\n/// Recursive nominal prose.\npub rec newtype Recursive : . | Recursive { pub constructor make_rec; pub projector read_rec; };\n",
            ),
            ("b.kio", "module b;"),
        ]);
        let ts = super::super::ts::emit::lower_package_to_dts(&package, "fixture").unwrap();
        for (name, prose) in [
            ("Opaque", "Opaque nominal prose."),
            ("Recursive", "Recursive nominal prose."),
        ] {
            let identity = super::super::js::emit::encoded_newtype_identity("a", name);
            assert!(
                ts.contains(&format!(
                    " * {prose}\n */\ndeclare class __KioOpaque_H{identity}"
                )),
                "{ts}"
            );
        }
    }

    #[test]
    fn python_keyword_members_document_the_existing_dynamic_accessor() {
        let package = package_from_sources(&[
            (
                "a.kio",
                "module a;\n/// Keyword host prose.\nhost fn is() -> .;\n/// Keyword export prose.\npub fn return() -> . { () }\n",
            ),
            ("b.kio", "module b;"),
        ]);
        let py = super::super::python::stub::lower_package_to_stub(&package, "fixture").unwrap();
        assert!(
            py.contains(
                "def __getattr__(self, name: str) -> object:\n        \"is:\\nKeyword host prose.\""
            ),
            "{py}"
        );
        assert!(py.contains("def __getattr__(self, name: str) -> object:\n        \"return:\\nKeyword export prose.\""), "{py}");
        assert!(!py.contains("def is("), "{py}");
        assert!(!py.contains("def return("), "{py}");
    }
}
