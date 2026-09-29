//! Per-page directive resolution scopes.
//!
//! A `` [`@signature term`] `` / `` [`@source term`] `` /
//! `` [`@type term`] `` directive resolves `term` against the same
//! scope `kio doc check` validates it in:
//!
//! - inside a `///` doc-comment, the surrounding module's items;
//! - inside a `.md` tutorial page, the package's package-file items.
//!
//! [`ModuleScope`] and [`PackageBoundaryScope`] implement
//! [`super::rewrite::DirectiveScope`] for those two cases — they map
//! a term to the resolved item's pretty-printed declaration header,
//! source, or bound-value type.
//!
//! A qualified-path term (`pkg.mod.name`) cannot be resolved to a
//! concrete item without the full package graph, so both scopes
//! return `None` for it — the rewriter then leaves the directive
//! verbatim, matching `kio doc check`'s optimistic treatment of
//! references outside the documented package.

use crate::ast::Module;
use crate::doc_entry::{DocEntry, documented_entries_for_item};

use super::rewrite::{DirectiveScope, LinkTarget};

/// Directive scope for a `.kio` module page — resolves terms against
/// the module's top-level items.
pub struct ModuleScope {
    module: Module,
}

impl ModuleScope {
    pub fn from_module(module: Module) -> Self {
        ModuleScope { module }
    }

    /// Build a scope by parsing a module's source. Returns `None`
    /// when the source does not parse (`kio doc check` is the gate;
    /// an unparseable module is simply skipped).
    pub fn from_source(source: &str) -> Option<Self> {
        crate::pass::parser::parse(source)
            .ok()
            .map(|module| ModuleScope { module })
    }

    /// Find the exact top-level declaration whose resolvable name is `term`.
    fn find(&self, term: &str) -> Option<DocEntry<'_>> {
        self.module
            .items
            .iter()
            .flat_map(documented_entries_for_item)
            .find(|entry| crate::kiodoc::refs::documented_entry_matches_reference(entry, term))
    }
}

impl DirectiveScope for ModuleScope {
    fn link_target(&self, term: &str) -> LinkTarget {
        match self.find(term) {
            Some(entry) => LinkTarget::Exact(entry.anchor(&self.module.path.segments.join("/"))),
            None => LinkTarget::LegacyName,
        }
    }

    fn signature(&self, term: &str) -> Option<String> {
        self.find(term).map(|entry| entry.signature())
    }

    fn source(&self, term: &str) -> Option<String> {
        self.find(term).map(|entry| entry.source())
    }

    fn ty(&self, term: &str) -> Option<String> {
        self.find(term).and_then(|entry| entry.ty())
    }
}

/// Directive scope for a `.md` tutorial page. The package file carries
/// only the `bridge { … }` glob list — it names no declarations — so a
/// package-boundary term (`` [`@signature emit`] `` for a host fn)
/// resolves against the **bridged modules'** items, which form the
/// package boundary, per `specs/kiodoc.md` § Reference resolution.
pub struct PackageBoundaryScope {
    modules: Vec<Module>,
}

impl PackageBoundaryScope {
    /// Build a package-boundary scope from the package file's bridge
    /// globs and the package's parsed module ASTs. `module_sources`
    /// pairs each module's declared path with its source text; modules
    /// selected by a bridge glob contribute their items.
    pub fn from_bridged_modules<'a>(
        package_file_source: Option<&str>,
        module_sources: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> Self {
        let package_file =
            package_file_source.and_then(|s| crate::pass::parser::parse_package_file(s, None).ok());
        let Some(package_file) = package_file else {
            return PackageBoundaryScope {
                modules: Vec::new(),
            };
        };
        let modules = module_sources
            .filter_map(|(_, source)| crate::pass::parser::parse(source).ok())
            .filter(|module| crate::kiodoc::module_is_bridged(&package_file, module))
            .collect();
        PackageBoundaryScope { modules }
    }

    /// Build a package-boundary scope from provider-aware module ASTs.
    /// Production Kiodoc paths use this form so imported custom operators do
    /// not require a context-free reparse of each module source.
    pub fn from_bridged_module_asts<'a>(
        package_file_source: Option<&str>,
        modules: impl Iterator<Item = (&'a str, &'a Module)>,
    ) -> Self {
        let package_file =
            package_file_source.and_then(|s| crate::pass::parser::parse_package_file(s, None).ok());
        let Some(package_file) = package_file else {
            return PackageBoundaryScope {
                modules: Vec::new(),
            };
        };
        let modules = modules
            .filter(|(_, module)| crate::kiodoc::module_is_bridged(&package_file, module))
            .map(|(_, module)| module.clone())
            .collect();
        PackageBoundaryScope { modules }
    }

    /// Find the exact boundary declaration whose resolvable name is `term`.
    fn find(&self, term: &str) -> Option<(&Module, DocEntry<'_>)> {
        for module in &self.modules {
            let names = crate::kiodoc::refs::module_scope_from_surface_for_boundary(module);
            if names.top_level.iter().any(|name| name == term) {
                // A selected role without a declaration view cannot borrow a
                // different module's same-spelling declaration.
                return crate::doc_entry::exported_documented_items_module(module)
                    .into_iter()
                    .find(|entry| {
                        crate::kiodoc::refs::documented_entry_matches_reference(entry, term)
                    })
                    .map(|entry| (module, entry));
            }
        }
        None
    }
}

impl DirectiveScope for PackageBoundaryScope {
    fn link_target(&self, term: &str) -> LinkTarget {
        match self.find(term) {
            Some((module, entry)) => {
                LinkTarget::Exact(entry.anchor(&module.path.segments.join("/")))
            }
            None => LinkTarget::Plain,
        }
    }

    fn signature(&self, term: &str) -> Option<String> {
        self.find(term).map(|(_, entry)| entry.signature())
    }

    fn source(&self, term: &str) -> Option<String> {
        self.find(term).map(|(_, entry)| entry.source())
    }

    fn ty(&self, term: &str) -> Option<String> {
        self.find(term).and_then(|(_, entry)| entry.ty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_queries_keep_short_spelling_and_complete_identity() {
        let module = crate::pass::parser::parse(concat!(
            "module api; ",
            "pub op _ + _ { impl plus; }; ",
            "pub op - __ { impl negate; }; ",
            "pub op _ - __ { impl subtract; };"
        ))
        .unwrap();
        let local_refs = crate::kiodoc::refs::module_scope_from_surface(&module);
        let boundary_refs = crate::kiodoc::refs::module_scope_from_surface_for_boundary(&module);
        let local = ModuleScope::from_module(module.clone());
        let boundary = PackageBoundaryScope::from_bridged_module_asts(
            Some("package pkg; bridge { api; }"),
            std::iter::once(("api", &module)),
        );
        for (query, signature, anchor) in [
            (
                "+",
                "pub op _ + _ { impl plus }",
                "op-api-op_20_5f_20_2b_20_5f",
            ),
            (
                "op _ + _",
                "pub op _ + _ { impl plus }",
                "op-api-op_20_5f_20_2b_20_5f",
            ),
            (
                "-",
                "pub op - __ { impl negate }",
                "op-api-op_20_2d_20_5f_5f",
            ),
            (
                "op - __",
                "pub op - __ { impl negate }",
                "op-api-op_20_2d_20_5f_5f",
            ),
            (
                "op _ - __",
                "pub op _ - __ { impl subtract }",
                "op-api-op_20_5f_20_2d_20_5f_5f",
            ),
        ] {
            for refs in [&local_refs, &boundary_refs] {
                assert_eq!(
                    crate::kiodoc::refs::resolve_in_module(query, refs),
                    crate::kiodoc::refs::RefOutcome::Resolved,
                    "{query}"
                );
            }
            for scope in [&local as &dyn DirectiveScope, &boundary] {
                assert_eq!(
                    scope.signature(query).as_deref(),
                    Some(signature),
                    "{query}"
                );
                assert_eq!(scope.source(query).as_deref(), Some(signature), "{query}");
                assert!(
                    matches!(scope.link_target(query), LinkTarget::Exact(actual) if actual == anchor)
                );
            }
        }
    }

    #[test]
    fn operator_query_aliases_preserve_visibility_and_first_boundary_owner() {
        let first =
            "module first; op _ + _ { impl hidden; }; pub op _ * _ { impl first_product; };";
        let second = "module second; pub op _ + _ { impl second_sum; }; pub op _ * _ { impl second_product; };";
        let third = "module third; pub op _ + _ { impl third_sum; };";
        let boundary = PackageBoundaryScope::from_bridged_modules(
            Some("package pkg; bridge { first; second; third; }"),
            [("first", first), ("second", second), ("third", third)].into_iter(),
        );
        for query in ["+", "op _ + _"] {
            assert_eq!(
                boundary.signature(query).as_deref(),
                Some("pub op _ + _ { impl second_sum }")
            );
            assert!(
                matches!(boundary.link_target(query), LinkTarget::Exact(anchor) if anchor == "op-second-op_20_5f_20_2b_20_5f")
            );
        }
        for query in ["*", "op _ * _"] {
            assert_eq!(
                boundary.signature(query).as_deref(),
                Some("pub op _ * _ { impl first_product }")
            );
            assert!(
                matches!(boundary.link_target(query), LinkTarget::Exact(anchor) if anchor == "op-first-op_20_5f_20_2a_20_5f")
            );
        }
        let local = ModuleScope::from_source(first).unwrap();
        assert_eq!(
            local.signature("+").as_deref(),
            Some("op _ + _ { impl hidden }")
        );
        assert!(boundary.signature("hidden").is_none());
    }

    #[test]
    fn exact_operator_names_precede_qualified_path_fallback() {
        let module = crate::pass::parser::parse(concat!(
            "module api; ",
            "pub op _ +. _ { impl dotted; }; ",
            "pub op _ / _ { impl slash; }; ",
            "pub varop [* *] { foldl push empty; };"
        ))
        .unwrap();
        let local_refs = crate::kiodoc::refs::module_scope_from_surface(&module);
        let public_refs = crate::kiodoc::refs::module_scope_from_surface_for_boundary(&module);
        let package_refs = crate::kiodoc::refs::PackageScope {
            exported_names: public_refs.top_level,
            ..Default::default()
        };
        let local = ModuleScope::from_module(module.clone());
        let boundary = PackageBoundaryScope::from_bridged_module_asts(
            Some("package pkg; bridge { api; }"),
            std::iter::once(("api", &module)),
        );
        for (query, anchor) in [
            ("+.", "op-api-op_20_5f_20_2b_2e_20_5f"),
            ("op _ +. _", "op-api-op_20_5f_20_2b_2e_20_5f"),
            ("/", "op-api-op_20_5f_20_2f_20_5f"),
            ("op _ / _", "op-api-op_20_5f_20_2f_20_5f"),
            ("varop [* *]", "op-api-varop_20_5b_2a_20_2a_5d"),
        ] {
            assert_eq!(
                crate::kiodoc::refs::resolve_in_module(query, &local_refs),
                crate::kiodoc::refs::RefOutcome::Resolved,
                "{query}"
            );
            assert_eq!(
                crate::kiodoc::refs::resolve_in_package(query, &package_refs),
                crate::kiodoc::refs::RefOutcome::Resolved,
                "{query}"
            );
            for scope in [&local as &dyn DirectiveScope, &boundary] {
                assert!(scope.signature(query).is_some(), "{query}");
                assert!(
                    matches!(scope.link_target(query), LinkTarget::Exact(actual) if actual == anchor)
                );
            }
        }
        for scope in [&local as &dyn DirectiveScope, &boundary] {
            assert!(scope.signature("elsewhere.item").is_none());
        }
    }

    #[test]
    fn boundary_checker_and_views_agree_on_all_supported_declaration_kinds() {
        let module = crate::pass::parser::parse(
            "module api; pub fn value(x: .) -> . { x } \
             pub type Alias = .; pub literal text = \"x\"; \
             pub newtype Box : . { pub constructor make; projector read; }; \
             pub labels Row = { field: . }; pub labels { other: . }; \
             pub type {forward} = {field}; \
             pub op _ + _ { impl add; }; pub elab choose : . -> . { impl choose_impl; }; \
             host type Text role(str); host fn emit(value: Text) -> .; \
             rec(loop) { pub fn first(x: .) -> . { rec second(x) }; fn second(x: .) -> . { rec first(x) } } \
             rec { pub type Chain = Node; pub newtype Node : . | Chain { constructor wrap; projector unwrap; }; pub labels Markers = { mark: . }; }"
        ).unwrap();
        let refs = crate::kiodoc::refs::module_scope_from_surface_for_boundary(&module);
        let boundary = PackageBoundaryScope::from_bridged_module_asts(
            Some("package pkg; bridge { api; }"),
            std::iter::once(("api", &module)),
        );
        let entries = crate::doc_entry::exported_documented_items_module(&module);
        assert_eq!(entries.len(), 17);
        for entry in entries {
            let name = entry.name();
            assert_eq!(
                crate::kiodoc::refs::resolve_in_module(name, &refs),
                crate::kiodoc::refs::RefOutcome::Resolved,
                "{name}"
            );
            assert_eq!(boundary.signature(name), Some(entry.signature()), "{name}");
            assert_eq!(boundary.source(name), Some(entry.source()), "{name}");
            assert_eq!(boundary.ty(name), entry.ty(), "{name}");
            assert_eq!(
                boundary.ty(name).is_some(),
                matches!(entry.kind(), "fn" | "host fn"),
                "{name}"
            );
        }
        for name in ["second", "read", "wrap", "unwrap", "add", "choose_impl"] {
            assert_eq!(
                crate::kiodoc::refs::resolve_in_module(name, &refs),
                crate::kiodoc::refs::RefOutcome::Unresolved,
                "{name}"
            );
            assert_eq!(boundary.signature(name), None, "{name}");
        }
        assert_eq!(
            crate::kiodoc::refs::resolve_in_module("make", &refs),
            crate::kiodoc::refs::RefOutcome::Resolved
        );
        assert_eq!(boundary.signature("make"), None);
    }

    #[test]
    fn boundary_links_preserve_selection_and_never_borrow_unsupported_names() {
        use crate::kiodoc::render::{rewrite::rewrite, site::SymbolIndex};
        let public = "module api; pub labels { foo: . }; fn foo() -> . { () } \
                      pub newtype Box : . { pub constructor make; projector read; };";
        let other = "module other; pub fn make() -> . { () } pub newtype Box : . { constructor box; projector unbox; };";
        let boundary = PackageBoundaryScope::from_bridged_modules(
            Some("package pkg; bridge { api; other; }"),
            [("api", public), ("other", other)].into_iter(),
        );
        let mut index = SymbolIndex::default();
        index.insert("Box", "other", "item-other-Box");
        index.insert("foo", "api", "item-api-foo");
        index.insert("make", "other", "item-other-make");
        assert_eq!(
            rewrite(
                "[`Box`], [`foo`], [`make`].",
                "",
                ".md",
                &index,
                &boundary,
                &std::collections::HashSet::new()
            ),
            "`Box`, `foo`, `make`."
        );
        index.insert("Box", "api", "item-api-Box");
        assert_eq!(
            rewrite(
                "[`Box`], [`foo`], [`make`].",
                "",
                ".md",
                &index,
                &boundary,
                &std::collections::HashSet::new()
            ),
            "[`Box`](api.md#item-api-Box), `foo`, `make`."
        );
        let local = ModuleScope::from_source(public).unwrap();
        assert_eq!(
            rewrite(
                "[`Box`]",
                "",
                ".md",
                &SymbolIndex::default(),
                &local,
                &std::collections::HashSet::new()
            ),
            "`Box`"
        );
    }

    #[test]
    fn module_scope_resolves_signature() {
        let scope = ModuleScope::from_source("module pkg/main;\npub fn add(x: Int) -> Int { x }\n")
            .unwrap();
        let sig = scope.signature("add").unwrap();
        assert!(sig.contains("fn add"));
        assert!(!sig.contains('{'));
    }

    #[test]
    fn module_scope_resolves_source() {
        let scope = ModuleScope::from_source("module pkg/main;\npub fn add(x: Int) -> Int { x }\n")
            .unwrap();
        let src = scope.source("add").unwrap();
        assert!(src.contains("fn add"));
        assert!(src.contains('{'));
    }

    #[test]
    fn module_scope_strips_doc_comment_from_source() {
        let scope = ModuleScope::from_source(
            "module pkg/main;\n/// docs here\npub fn add(x: Int) -> Int { x }\n",
        )
        .unwrap();
        let src = scope.source("add").unwrap();
        assert!(!src.contains("docs here"));
    }

    #[test]
    fn module_scope_resolves_type() {
        let scope = ModuleScope::from_source("module pkg/main;\npub fn add(x: Int) -> Int { x }\n")
            .unwrap();
        let ty = scope.ty("add").unwrap();
        // The bound-value type — no `fn` keyword, no item name, no
        // value-binder names.
        assert!(!ty.contains("fn"));
        assert!(!ty.contains("add"));
        assert!(ty.contains("Int"));
    }

    #[test]
    fn ordinary_label_nominals_render_the_owner_with_exact_visibility() {
        let source = "module api; labels { hidden: . }; pub(api) labels { scoped: . }; pub labels Row = { field: . };";
        let module = ModuleScope::from_source(source).unwrap();
        for name in ["Hidden", "Scoped", "Field"] {
            assert!(module.signature(name).is_some(), "{name}");
            assert_eq!(module.ty(name), None);
        }
        let boundary = PackageBoundaryScope::from_bridged_modules(
            Some("package pkg; bridge { api; }"),
            std::iter::once(("api", source)),
        );
        assert_eq!(
            boundary.signature("Field"),
            Some("pub labels Row = { field: . };".to_owned())
        );
        assert_eq!(boundary.source("Field"), boundary.signature("Field"));
        for name in ["Hidden", "Scoped"] {
            assert_eq!(boundary.signature(name), None);
            assert_eq!(boundary.source(name), None);
        }
    }

    #[test]
    fn recursive_group_boundary_lookup_keeps_context_without_exporting_peers() {
        let module = crate::pass::parser::parse(
            "module api;\nrec { pub newtype Visible : . | Hidden { constructor make; projector read; }; newtype Hidden : . | Visible { constructor wrap; projector unwrap; }; }\n",
        ).unwrap();
        for scope in [
            PackageBoundaryScope::from_bridged_module_asts(
                Some("package pkg; bridge { api; }"),
                std::iter::once(("api", &module)),
            ),
            PackageBoundaryScope::from_bridged_modules(
                Some("package pkg; bridge { api; }"),
                std::iter::once((
                    "api",
                    "module api;\nrec { pub newtype Visible : . | Hidden { constructor make; projector read; }; newtype Hidden : . | Visible { constructor wrap; projector unwrap; }; }\n",
                )),
            ),
        ] {
            let source = scope.source("Visible").expect("exported member");
            assert!(source.starts_with("rec {"), "{source}");
            assert!(source.contains("newtype Hidden"), "{source}");
            assert_eq!(scope.signature("Visible"), Some(source));
            assert_eq!(scope.source("Hidden"), None);
            assert_eq!(scope.signature("Hidden"), None);
            assert_eq!(scope.ty("Hidden"), None);
        }
    }

    #[test]
    fn module_scope_resolves_exact_rec_group_member() {
        let scope = ModuleScope::from_source(
            "module pkg/main;\n\
             rec(loop) {\n\
               fn first(value: First) -> First { rec second(value) };\n\
               fn second(value: Second) -> Second { rec first(value) }\n\
             }\n",
        )
        .unwrap();

        let signature = scope.signature("second").unwrap();
        assert!(signature.contains("fn second"), "got: {signature}");
        assert!(!signature.contains("fn first"), "got: {signature}");

        let source = scope.source("second").unwrap();
        assert!(source.contains("fn second"), "got: {source}");
        assert!(!source.contains("fn first"), "got: {source}");

        let ty = scope.ty("second").unwrap();
        assert!(ty.contains("Second"), "got: {ty}");
        assert!(!ty.contains("First"), "got: {ty}");
    }

    #[test]
    fn module_scope_resolves_host_fn_signature() {
        let scope = ModuleScope::from_source(
            "module pkg/main;\nhost type S role(str);\nhost fn print(p0: S) -> .;\n",
        )
        .unwrap();
        let sig = scope.signature("print").unwrap();
        assert!(sig.contains("host fn print"), "got: {sig}");
    }

    #[test]
    fn module_scope_host_fn_type() {
        let scope = ModuleScope::from_source(
            "module pkg/main;\nhost type S role(str);\nhost fn print(p0: S) -> .;\n",
        )
        .unwrap();
        let ty = scope.ty("print").unwrap();
        assert!(!ty.contains("fn"));
        assert!(!ty.contains("print"));
        assert!(ty.contains("S"));
    }

    #[test]
    fn module_scope_type_on_host_type_none() {
        // `@type` against a `host type` resolves to `None` — a host
        // type binds no value.
        let scope = ModuleScope::from_source("module pkg/main;\nhost type S role(str);\n").unwrap();
        assert!(scope.ty("S").is_none());
    }

    #[test]
    fn module_scope_type_on_type_level_name_none() {
        // `@type` against a `type` resolves to `None` — the alias
        // binds no value; `kio doc check` reports the kind error.
        let scope = ModuleScope::from_source("module pkg/main;\ntype Same[A] = A;\n").unwrap();
        assert!(scope.ty("Same").is_none());
    }

    #[test]
    fn forwarding_views_select_the_braced_declaration_in_both_scopes() {
        let source = concat!(
            "module pkg/api; pub fn field() -> . { () } ",
            "pub type {field} = {original}; type {hidden} = {original};",
        );
        let local = ModuleScope::from_source(source).unwrap();
        let boundary = PackageBoundaryScope::from_bridged_modules(
            Some("package pkg; bridge { pkg/api; }"),
            std::iter::once(("pkg/api", source)),
        );
        for scope in [&local as &dyn DirectiveScope, &boundary] {
            assert_eq!(
                scope.signature("{field}").as_deref(),
                Some("pub type {field} = {original};")
            );
            assert_eq!(scope.source("{field}"), scope.signature("{field}"));
            assert_eq!(scope.ty("{field}"), None);
            assert!(
                scope
                    .signature("field")
                    .unwrap()
                    .starts_with("pub fn field")
            );
            assert_eq!(scope.signature("Field"), None);
            let LinkTarget::Exact(anchor) = scope.link_target("{field}") else {
                panic!("forwarded label has an exact declaration anchor");
            };
            assert_eq!(anchor, "label-pkg_2fapi-_7bfield_7d");
        }
        assert!(local.signature("{hidden}").is_some());
        assert_eq!(boundary.signature("{hidden}"), None);
    }

    #[test]
    fn module_scope_unknown_term_none() {
        let scope =
            ModuleScope::from_source("module pkg/main;\npub fn add() -> . { () }\n").unwrap();
        assert!(scope.signature("mystery").is_none());
    }

    #[test]
    fn module_scope_qualified_path_none() {
        let scope =
            ModuleScope::from_source("module pkg/main;\npub fn add() -> . { () }\n").unwrap();
        assert!(scope.signature("pkg.other.add").is_none());
    }

    #[test]
    fn package_boundary_scope_resolves_bridged_host_fn() {
        let scope = PackageBoundaryScope::from_bridged_modules(
            Some("package pkg;\nbridge { pkg; }\n"),
            std::iter::once((
                "pkg",
                "module pkg;\nhost type S role(str);\nhost fn emit(value: S) -> .;\n",
            )),
        );
        let sig = scope.signature("emit").expect("emit resolves");
        assert!(sig.contains("host fn emit"), "got: {sig}");
        assert!(scope.signature("missing").is_none());
    }

    #[test]
    fn package_boundary_scope_skips_unbridged_module() {
        // A module not selected by any glob contributes no items.
        let scope = PackageBoundaryScope::from_bridged_modules(
            Some("package pkg;\nbridge { pkg; }\n"),
            std::iter::once(("other", "module other;\nhost fn emit() -> .;\n")),
        );
        assert!(scope.signature("emit").is_none());
    }

    #[test]
    fn package_boundary_scope_resolves_only_exported_recursive_members() {
        let scope = PackageBoundaryScope::from_bridged_modules(
            Some("package pkg;\nbridge { pkg/main; }\n"),
            std::iter::once((
                "pkg/main",
                "module pkg/main;\n\
                 rec(loop) {\n\
                   fn local(value: .) -> . { rec exported(value) };\n\
                   pub(pkg) fn scoped(value: .) -> . { rec local(value) };\n\
                   pub fn exported(value: .) -> . { rec scoped(value) }\n\
                 }\n",
            )),
        );

        assert!(scope.signature("exported").is_some());
        assert!(scope.signature("local").is_none());
        assert!(scope.signature("scoped").is_none());
    }

    #[test]
    fn package_boundary_scope_no_package_file_resolves_nothing() {
        let scope = PackageBoundaryScope::from_bridged_modules(None, std::iter::empty());
        assert!(scope.signature("anything").is_none());
    }
}
