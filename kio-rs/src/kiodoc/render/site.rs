//! The documentation-site model.
//!
//! [`Site`] is the format-agnostic intermediate that `kio doc build`
//! produces from a package: an index page, one page per `.kio`
//! module, a package-boundary page (the `*.pkg.kio` surface), and the
//! mirrored tutorial tree under the build block's `docs.md`. The HTML
//! and Markdown emitters ([`super::html`], [`super::md`]) consume a
//! `Site`; the model itself does no rendering.
//!
//! Construction is purely structural — every page's URL and every
//! item's anchor is keyed by source identity (module path, item kind and
//! complete declaration name), never by what else exists. See
//! [`specs/kiodoc.md`](../../../../specs/kiodoc.md) § "Rendered
//! output".

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::ast::{DocComment, Module};
use crate::doc_entry::DocEntry;

/// The complete documentation-site model for one package.
pub struct Site {
    /// The package name (package-file stem, or the first module-path
    /// segment when there is no package file).
    pub package_name: String,
    /// One page per `.kio` module, sorted by module path.
    pub modules: Vec<ModulePage>,
    /// The package-boundary page — `None` when the package has no
    /// `*.pkg.kio`.
    pub package_boundary: Option<PackageBoundaryPage>,
    /// Mirrored tutorial / guide pages from the markdown source
    /// tree, sorted by relative path.
    pub tutorials: Vec<TutorialPage>,
    /// The symbol index — every documented name mapped to its page
    /// URL + anchor. Drives intra-doc link rewriting.
    pub index: SymbolIndex,
}

/// One module's documentation page.
pub struct ModulePage {
    /// Declared module path (`pkg/util/thing`).
    pub module_path: String,
    /// Page URL stem, relative to the site root, with no extension —
    /// e.g. `pkg/util/thing`.
    pub url_stem: String,
    /// The module-level `///` doc-comment body, if any.
    pub module_doc: Option<String>,
    /// Per-item sections in source declaration order.
    pub items: Vec<ItemSection>,
    /// Unnamed group prose, positioned before its first member section.
    pub group_docs: Vec<(usize, String)>,
    /// The module's raw source — passed to the directive rewriter so
    /// `@source` can embed item bodies.
    pub source: String,
    /// The provider-aware Surface AST for this module. Rendering keeps this
    /// artifact instead of reparsing `source` without its package context.
    pub module: Module,
}

/// The package-boundary page — the package's host and bridge surface.
pub struct PackageBoundaryPage {
    /// Per-item sections, one per host or export entry.
    pub items: Vec<ItemSection>,
}

/// One documented item: a `fn` / `type` / `literal` / `labels` / `op` / `newtype`
/// in a module, or a host or export entry on the package-boundary page.
pub struct ItemSection {
    /// The item's source identifier (the operator token-run for
    /// `op`, the type-alias name for `labels`).
    pub name: String,
    /// Declaring module, also the context of the item's own doc comment.
    pub module_path: String,
    /// Stable identity fragment shared by module and boundary pages.
    pub anchor: String,
    /// The item kind, for the section label (`fn`, `type`, …).
    pub kind: &'static str,
    /// The pretty-printed signature (header line, no body).
    pub signature: String,
    /// The item's `///` doc-comment body, if any.
    pub doc: Option<String>,
}

/// One tutorial / guide markdown page, mirrored from `docs.md`.
pub struct TutorialPage {
    /// Page URL stem, relative to the site root, no extension.
    pub url_stem: String,
    /// The raw markdown body of the source `.md` file.
    pub source: String,
    /// The page title — the first ATX heading, or the file stem.
    pub title: String,
}

/// Maps every documented name to the page + anchor that documents
/// it. Built once during [`Site`] construction; the directive /
/// intra-doc-link rewriter consults it to turn `` [`name`] `` into a
/// working link.
#[derive(Default)]
pub struct SymbolIndex {
    /// Simple name → `(url_stem, anchor)`. The `url_stem` has no
    /// extension; the emitter appends `.html` / `.md`.
    entries: BTreeMap<String, (String, String)>,
    identities: BTreeMap<String, String>,
}

impl SymbolIndex {
    /// Record a documented name. The first registration of a name
    /// wins — module-page items are registered before the package boundary, so a
    /// name declared in a module links to its module page.
    pub fn insert(&mut self, name: &str, url_stem: &str, anchor: &str) {
        self.identities
            .entry(anchor.to_owned())
            .or_insert_with(|| url_stem.to_owned());
        self.entries
            .entry(name.to_owned())
            .or_insert_with(|| (url_stem.to_owned(), anchor.to_owned()));
    }

    /// Look up a name. Returns `(url_stem, anchor)` when the name is
    /// documented in this package, `None` otherwise.
    pub fn lookup(&self, name: &str) -> Option<(&str, &str)> {
        self.entries
            .get(name)
            .map(|(u, a)| (u.as_str(), a.as_str()))
    }

    pub fn lookup_anchor(&self, anchor: &str) -> Option<(&str, &str)> {
        self.identities
            .get_key_value(anchor)
            .map(|(anchor, url)| (url.as_str(), anchor.as_str()))
    }
}

/// Build the per-item documentation sections for a module's items.
/// Source-order preserved; anonymous items (anonymous `labels`,
/// `equiv`, anonymous `op` with no resolvable function name) are
/// skipped — they have no name to anchor or link.
fn module_item_sections(module: &Module) -> Vec<ItemSection> {
    let module_path = module.path.segments.join("/");
    crate::doc_entry::documented_items_module(module)
        .into_iter()
        .map(|entry| item_section_from_entry(&module_path, entry))
        .collect()
}

/// Build an [`ItemSection`] from a [`DocEntry`] — the single
/// chokepoint over documentable declarations. Used for both module
/// items and package-file items, so a new documentable kind is rendered
/// uniformly without touching this function.
fn item_section_from_entry(module_path: &str, entry: DocEntry) -> ItemSection {
    ItemSection {
        name: entry.name().to_owned(),
        module_path: module_path.to_owned(),
        anchor: entry.anchor(module_path),
        kind: entry.kind(),
        signature: entry.signature(),
        doc: entry.doc().cloned().map(doc_comment_body),
    }
}

/// Join a doc-comment's lines into a markdown body string.
fn doc_comment_body(doc: DocComment) -> String {
    doc.lines.join("\n")
}

/// Build the URL stem for a module path — `pkg/util/thing` →
/// `pkg/util/thing`. The surface module path already uses `/`, which
/// mirrors the source layout, so the stem is the path verbatim.
fn module_url_stem(module_path: &str) -> String {
    module_path.to_owned()
}

/// Compute the title of a tutorial page: the first ATX heading text
/// if one exists, else the file stem with `-`/`_` turned to spaces.
fn tutorial_title(source: &str, stem: &str) -> String {
    for line in source.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix('#') {
            let heading = rest.trim_start_matches('#').trim();
            if !heading.is_empty() {
                return heading.to_owned();
            }
        }
    }
    stem.replace(['-', '_'], " ")
}

/// Recursively collect `.md` files under `dir`, returning
/// `(relative_path, absolute_path)` pairs. Skips build-artifact
/// (`out`, `target`), hidden (`.*`), and the `_assets` override
/// directory.
fn collect_md_tree(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(PathBuf, PathBuf)>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if path.is_dir() {
            if name == "target" || name == "out" || name == "_assets" || name.starts_with('.') {
                continue;
            }
            collect_md_tree(root, &path, out)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            out.push((rel, path));
        }
    }
    Ok(())
}

/// A failure while building the [`Site`] model.
#[derive(Debug)]
pub enum SiteError {
    /// An I/O failure with a human-readable context message.
    Io(String),
}

impl std::fmt::Display for SiteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SiteError::Io(msg) => write!(f, "{msg}"),
        }
    }
}

/// Build the [`Site`] model for the package rooted at
/// `package_root`, with markdown sources under `md_dir`.
///
/// Construction assumes the package has already passed
/// `kio doc check`: every `.kio` file parses, every directive
/// resolves. A `.kio` file that fails to parse here is skipped
/// rather than erroring — `kio doc check` is the gate.
pub fn build_site(package_root: &Path, md_dir: &Path) -> Result<Site, SiteError> {
    // ---- Walk the package's .kio modules ----
    let mut kio_files: Vec<PathBuf> = Vec::new();
    crate::kiodoc::doc_comments::collect_kio_files(package_root, &mut kio_files)
        .map_err(|e| SiteError::Io(format!("walking {}: {e}", package_root.display())))?;
    kio_files.sort();

    let mut modules: Vec<ModulePage> = Vec::new();
    let mut module_asts: BTreeMap<String, Module> = BTreeMap::new();
    for path in &kio_files {
        let source = fs::read_to_string(path)
            .map_err(|e| SiteError::Io(format!("reading {}: {e}", path.display())))?;
        let Ok(module_file) = crate::pass::parser::parse_module_file(&source) else {
            // Parse failure — `kio doc check` already reported it.
            continue;
        };
        let module = module_file.module;
        // Surface module path: segments joined with the `/`
        // separator, the same spelling a user reads and writes.
        let module_path = module.path.segments.join("/");
        let url_stem = module_url_stem(&module_path);
        let items = module_item_sections(&module);
        let mut item_offset = 0;
        let mut group_docs = Vec::new();
        for item in &module.items {
            if let crate::ast::Item::TypeRecGroup(group) = item
                && let Some(doc) = &group.doc
            {
                group_docs.push((item_offset, doc.lines.join("\n")));
            }
            item_offset += crate::doc_entry::documented_entries_for_item(item).len();
        }
        module_asts.insert(module_path.clone(), module.clone());
        modules.push(ModulePage {
            module_path,
            url_stem,
            module_doc: module.doc.clone().map(doc_comment_body),
            items,
            group_docs,
            source,
            module,
        });
    }
    modules.sort_by(|a, b| a.module_path.cmp(&b.module_path));

    // ---- Find and parse the package file ----
    let mut package_name = String::new();
    let mut package_boundary: Option<PackageBoundaryPage> = None;
    if let Some((package_file_path, package_file_source)) = find_package_file(package_root) {
        package_name = package_file_path
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(crate::file_kind::package_stem)
            .unwrap_or("")
            .to_owned();
        if let Ok(package_file) =
            crate::pass::parser::parse_package_file(&package_file_source, Some(&package_name))
        {
            package_boundary = Some(PackageBoundaryPage {
                items: package_file_item_sections(&package_file, &module_asts),
            });
        }
    }
    if package_name.is_empty() {
        // No package file — fall back to the first module-path segment.
        package_name = modules
            .first()
            .and_then(|m| m.module_path.split('/').next())
            .unwrap_or("package")
            .to_owned();
    }

    // ---- Walk the markdown tutorial tree ----
    let mut tutorials: Vec<TutorialPage> = Vec::new();
    if md_dir.is_dir() {
        let mut md_files: Vec<(PathBuf, PathBuf)> = Vec::new();
        collect_md_tree(md_dir, md_dir, &mut md_files)
            .map_err(|e| SiteError::Io(format!("walking {}: {e}", md_dir.display())))?;
        md_files.sort();
        for (rel, abs) in &md_files {
            let source = fs::read_to_string(abs)
                .map_err(|e| SiteError::Io(format!("reading {}: {e}", abs.display())))?;
            let stem = rel.with_extension("");
            let url_stem = stem.to_string_lossy().replace('\\', "/");
            let file_stem = stem.file_name().and_then(|s| s.to_str()).unwrap_or("page");
            let title = tutorial_title(&source, file_stem);
            tutorials.push(TutorialPage {
                url_stem,
                source,
                title,
            });
        }
    }
    tutorials.sort_by(|a, b| a.url_stem.cmp(&b.url_stem));

    // ---- Build the symbol index ----
    let mut index = SymbolIndex::default();
    for m in &modules {
        for it in &m.items {
            index.insert(&it.name, &m.url_stem, &it.anchor);
        }
    }
    if let Some(ex) = &package_boundary {
        for it in &ex.items {
            index.insert(&it.name, "package-boundary", &it.anchor);
        }
    }

    Ok(Site {
        package_name,
        modules,
        package_boundary,
        tutorials,
        index,
    })
}

/// Find the package's `*.pkg.kio` at `package_root` and read it.
fn find_package_file(package_root: &Path) -> Option<(PathBuf, String)> {
    let entries = fs::read_dir(package_root).ok()?;
    let mut hits: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if crate::file_kind::is_package_file(name) {
            hits.push(path);
        }
    }
    hits.sort();
    let path = hits.into_iter().next()?;
    let src = fs::read_to_string(&path).ok()?;
    Some((path, src))
}

/// Public declarations keep the same identities and source owners on both pages.
fn package_file_item_sections(
    package_file: &crate::ast::PackageFile,
    modules: &BTreeMap<String, Module>,
) -> Vec<ItemSection> {
    modules
        .iter()
        .filter(|(_, module)| crate::kiodoc::module_is_bridged(package_file, module))
        .flat_map(|(path, module)| {
            crate::doc_entry::exported_documented_items_module(module)
                .into_iter()
                .map(|entry| item_section_from_entry(path, entry))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_index_first_wins() {
        let mut idx = SymbolIndex::default();
        idx.insert("foo", "mod/a", "item-foo");
        idx.insert("foo", "package-boundary", "item-foo");
        assert_eq!(idx.lookup("foo"), Some(("mod/a", "item-foo")));
    }

    #[test]
    fn module_url_stem_mirrors_path() {
        // The caller already renders the module path with the `/`
        // separator, which is also the URL path separator, so the stem
        // mirrors it directly.
        assert_eq!(module_url_stem("pkg/util/thing"), "pkg/util/thing");
        assert_eq!(module_url_stem("pkg"), "pkg");
    }

    #[test]
    fn tutorial_title_from_heading() {
        assert_eq!(tutorial_title("# My Title\n\nbody", "stem"), "My Title");
        assert_eq!(tutorial_title("no heading here", "my-page"), "my page");
    }

    #[test]
    fn item_section_for_fn() {
        let module = crate::pass::parser::parse(
            "module pkg/main;\n/// Adds two numbers.\npub fn add(x: Int) -> Int { x }\n",
        )
        .unwrap();
        let sections = module_item_sections(&module);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].name, "add");
        assert_eq!(sections[0].anchor, "item-pkg_2fmain-add");
        assert_eq!(sections[0].kind, "fn");
        assert!(sections[0].signature.contains("fn add"));
        assert!(!sections[0].signature.contains('{'));
        assert_eq!(sections[0].doc.as_deref(), Some("Adds two numbers."));
    }

    #[test]
    fn item_section_for_newtype_carries_doc() {
        // A `///` on a `newtype` now flows into the doc-site model
        // (previously hardcoded `doc: None`).
        let module = crate::pass::parser::parse(
            "module pkg/main;\n/// Wraps a unit.\n\
             pub newtype Wrapped : . { constructor mk_w; projector un_w; };\n",
        )
        .unwrap();
        let sections = module_item_sections(&module);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].name, "Wrapped");
        assert_eq!(sections[0].kind, "newtype");
        assert_eq!(sections[0].doc.as_deref(), Some("Wraps a unit."));
    }

    #[test]
    fn package_file_without_matching_modules_has_no_item_sections() {
        let package_file =
            crate::pass::parser::parse_package_file("package pkg;\nbridge { main; }\n", None)
                .unwrap();
        let sections = package_file_item_sections(&package_file, &BTreeMap::new());
        assert!(sections.is_empty());
    }

    #[test]
    fn boundary_sections_preserve_owners_and_complete_operator_identities() {
        let package =
            crate::pass::parser::parse_package_file("package pkg; bridge { api/**; }", None)
                .unwrap();
        let mut modules = BTreeMap::new();
        for (path, body) in [
            (
                "api/left",
                "pub newtype Box : . { constructor wrap; projector unwrap; }; pub fn make() -> . { () } fn hidden() -> . { () } pub op - __ { impl negate; }; pub op _ - __ { impl subtract; };",
            ),
            (
                "api/right",
                "pub newtype Box : . { constructor wrap; projector unwrap; }; pub fn make() -> . { () }",
            ),
            ("outside", "pub fn skipped() -> . { () }"),
        ] {
            modules.insert(
                path.to_owned(),
                crate::pass::parser::parse(&format!("module {path}; {body}")).unwrap(),
            );
        }
        let sections = package_file_item_sections(&package, &modules);
        assert_eq!(sections.len(), 6);
        let anchors = sections
            .iter()
            .map(|entry| entry.anchor.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            anchors
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            6
        );
        assert!(anchors.contains(&"op-api_2fleft-op_20_2d_20_5f_5f".to_owned()));
        assert!(anchors.contains(&"op-api_2fleft-op_20_5f_20_2d_20_5f_5f".to_owned()));
        let mut index = SymbolIndex::default();
        for section in &sections {
            index.insert(&section.name, &section.module_path, &section.anchor);
        }
        for section in &sections {
            assert_eq!(
                index.lookup_anchor(&section.anchor),
                Some((section.module_path.as_str(), section.anchor.as_str()))
            );
            let local = module_item_sections(&modules[&section.module_path]);
            assert!(local.iter().any(
                |entry| entry.anchor == section.anchor && entry.signature == section.signature
            ));
        }
        modules.insert(
            "api/unrelated".to_owned(),
            crate::pass::parser::parse("module api/unrelated; pub type Other = .;").unwrap(),
        );
        let unchanged = package_file_item_sections(&package, &modules)
            .into_iter()
            .filter(|entry| entry.module_path != "api/unrelated")
            .map(|entry| entry.anchor)
            .collect::<Vec<_>>();
        assert_eq!(unchanged, anchors);
    }

    #[test]
    fn module_section_for_host_type_carries_doc() {
        // A `///` on a `host type` flows into the module page model.
        let module = crate::pass::parser::parse(
            "module pkg/main;\n/// A string host type.\nhost type Text role(str);\n",
        )
        .unwrap();
        let sections = module_item_sections(&module);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].name, "Text");
        assert_eq!(sections[0].kind, "host type");
        assert_eq!(sections[0].doc.as_deref(), Some("A string host type."));
    }

    #[test]
    fn anonymous_op_skipped() {
        // An anonymous `equiv` produces no section.
        let module =
            crate::pass::parser::parse("module pkg/main;\nequiv e() { (); () }\n").unwrap();
        assert!(module_item_sections(&module).is_empty());
    }

    #[test]
    fn imported_operator_module_keeps_site_docs_source_and_snippets() {
        use crate::kiodoc::render::rewrite::DirectiveScope;

        let temp = tempfile::tempdir().expect("temporary package");
        let package_root = temp.path();
        let module_dir = package_root.join("pkg");
        fs::create_dir_all(&module_dir).expect("create module directory");
        fs::write(package_root.join("pkg.pkg.kio"), "package pkg;\n").expect("write package file");
        let provider_source = "module pkg/syntax; \
             pub fn combine(left: ., right: .) -> . { () } \
             pub op _ + _ { impl combine; };\n";
        let provider =
            crate::pass::parser::parse(provider_source).expect("parse operator provider");
        assert!(
            matches!(
                provider.items.first(),
                Some(crate::ast::Item::FnDef(def)) if def.vis.is_pub()
            ),
            "public operator implementation must be public"
        );
        fs::write(module_dir.join("syntax.kio"), provider_source).expect("write operator provider");
        let main_source = concat!(
            "module pkg/main;\n",
            "import pkg/syntax(op _ + _);\n",
            "/// [`@source documented`]\n",
            "/// ```kio {@}\n",
            "/// let value = () + ()\n",
            "/// ```\n",
            "pub fn documented() -> . { () + () }\n",
        );
        let main_path = module_dir.join("main.kio");
        fs::write(&main_path, main_source).expect("write operator consumer");

        let site = build_site(package_root, &package_root.join("docs")).expect("build site");
        let page = site
            .modules
            .iter()
            .find(|page| page.module_path == "pkg/main")
            .expect("consumer module remains documented");
        let item = page
            .items
            .iter()
            .find(|item| item.name == "documented")
            .expect("documented declaration remains present");
        assert!(
            item.doc
                .as_deref()
                .is_some_and(|doc| doc.contains("@source documented"))
        );
        let scope = crate::kiodoc::render::scope::ModuleScope::from_module(page.module.clone());
        assert!(
            scope
                .source("documented")
                .is_some_and(|source| source.contains("() + ()"))
        );

        let snippets = crate::kiodoc::doc_comments::extract_doc_snippets(&main_path, main_source)
            .expect("extract provider-aware doc snippets");
        assert_eq!(snippets.len(), 1);
        assert!(snippets[0].body.contains("() + ()"));
    }
}
