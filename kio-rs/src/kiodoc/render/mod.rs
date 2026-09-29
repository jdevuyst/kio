//! `kio doc build` — the documentation-site renderer.
//!
//! Where [`super::validate`] and [`super::doc_comments`] *check*
//! Kiodoc content, this module *renders* it. `kio doc build` walks
//! the package the build block's `docs` field points at, builds a
//! [`site::Site`] model, and emits a navigable per-module
//! documentation site — HTML by default, Markdown when `--md` is
//! passed.
//!
//! ## Pipeline
//!
//! 1. [`site::build_site`] walks the package's `.kio` modules, the
//!    `*.pkg.kio` file, and the markdown tree under `docs.md`,
//!    producing the [`site::Site`] model and a symbol index.
//! 2. For each page, [`rewrite::rewrite`] turns the Kiodoc
//!    directives and intra-doc links in every doc-comment / tutorial
//!    body into plain Markdown.
//! 3. The HTML emitter runs the rewritten Markdown through
//!    [`markdown::render_html`] and wraps it in the page skeleton +
//!    sidebar; the Markdown emitter writes the rewritten Markdown
//!    directly.
//!
//! See [`specs/kiodoc.md`](../../../../specs/kiodoc.md) § "Rendered
//! output" for the site structure, URL scheme, and anchor format.

pub mod assets;
pub mod markdown;
pub mod rewrite;
pub mod scope;
pub mod site;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::kiodoc::refs::collect_ref_overrides;
use rewrite::DirectiveScope;
use site::{ItemSection, Site};

/// A failure during `kio doc build` rendering.
#[derive(Debug)]
pub enum RenderError {
    /// Building the site model failed.
    Site(site::SiteError),
    /// Writing an output file failed.
    Io(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::Site(e) => write!(f, "{e}"),
            RenderError::Io(msg) => write!(f, "{msg}"),
        }
    }
}

/// Render the package rooted at `package_root` to a documentation
/// site. `md_dir` is the markdown source tree (the build block's
/// `docs.md`). When `html_out` is `Some`, an HTML site is written
/// there; when `md_out` is `Some`, a Markdown site is written there.
/// At least one is always `Some` (the caller defaults to HTML).
pub fn build(
    package_root: &Path,
    md_dir: &Path,
    html_out: Option<&Path>,
    md_out: Option<&Path>,
) -> Result<(), RenderError> {
    let site = site::build_site(package_root, md_dir).map_err(RenderError::Site)?;
    let package_file_source = read_package_file_source(package_root);

    if let Some(out) = html_out {
        write_site(
            &site,
            md_dir,
            package_file_source.as_deref(),
            out,
            Format::Html,
        )?;
    }
    if let Some(out) = md_out {
        write_site(
            &site,
            md_dir,
            package_file_source.as_deref(),
            out,
            Format::Markdown,
        )?;
    }
    Ok(())
}

/// Output format selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Html,
    Markdown,
}

impl Format {
    /// The file extension produced pages carry.
    fn ext(self) -> &'static str {
        match self {
            Format::Html => ".html",
            Format::Markdown => ".md",
        }
    }
}

/// One entry in the navigation sidebar.
struct NavEntry {
    /// Display label.
    label: String,
    /// Page URL stem (no extension), relative to the site root.
    url_stem: String,
}

/// Write the whole site in one format under `out_dir`.
fn write_site(
    site: &Site,
    md_dir: &Path,
    package_file_source: Option<&str>,
    out_dir: &Path,
    format: Format,
) -> Result<(), RenderError> {
    fs::create_dir_all(out_dir)
        .map_err(|e| RenderError::Io(format!("creating {}: {e}", out_dir.display())))?;

    // Build the navigation model once — shared across every page.
    let nav = build_nav(site);
    let module_scopes = site
        .modules
        .iter()
        .map(|module| {
            (
                module.module_path.as_str(),
                scope::ModuleScope::from_module(module.module.clone()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let boundary_scope = scope::PackageBoundaryScope::from_bridged_module_asts(
        package_file_source,
        site.modules
            .iter()
            .map(|module| (module.module_path.as_str(), &module.module)),
    );

    // Index page.
    let index_body = render_index(site, format);
    write_page(
        out_dir,
        "index",
        &nav_wrap(&index_body, &nav, "index", format),
        format,
    )?;

    // Package-boundary page.
    if let Some(package_boundary) = &site.package_boundary {
        let body = render_item_page(
            "Package Boundary",
            &package_boundary.items,
            &site.index,
            &module_scopes,
            "package-boundary",
            format,
        );
        write_page(
            out_dir,
            "package-boundary",
            &nav_wrap(&body, &nav, "package-boundary", format),
            format,
        )?;
    }

    // Per-module pages.
    for module in &site.modules {
        let scope = &module_scopes[module.module_path.as_str()];
        let body = render_module_page(module, &site.index, Some(scope), format);
        write_page(
            out_dir,
            &module.url_stem,
            &nav_wrap(&body, &nav, &module.url_stem, format),
            format,
        )?;
    }

    // Tutorial / guide mirror tree.
    for tutorial in &site.tutorials {
        let body = render_tutorial_page(
            &tutorial.source,
            &tutorial.url_stem,
            &site.index,
            &boundary_scope,
            format,
        );
        write_page(
            out_dir,
            &tutorial.url_stem,
            &nav_wrap(&body, &nav, &tutorial.url_stem, format),
            format,
        )?;
    }

    // HTML: ship the static assets (or the author's override).
    if format == Format::Html {
        write_assets(md_dir, out_dir)?;
    }
    Ok(())
}

/// Build the navigation sidebar model from the site.
fn build_nav(site: &Site) -> Vec<NavSection> {
    let mut sections = Vec::new();

    let mut top = vec![NavEntry {
        label: "Index".to_owned(),
        url_stem: "index".to_owned(),
    }];
    if site.package_boundary.is_some() {
        top.push(NavEntry {
            label: "Package Boundary".to_owned(),
            url_stem: "package-boundary".to_owned(),
        });
    }
    sections.push(NavSection {
        title: None,
        entries: top,
    });

    if !site.modules.is_empty() {
        sections.push(NavSection {
            title: Some("Modules".to_owned()),
            entries: site
                .modules
                .iter()
                .map(|m| NavEntry {
                    label: m.module_path.clone(),
                    url_stem: m.url_stem.clone(),
                })
                .collect(),
        });
    }

    if !site.tutorials.is_empty() {
        sections.push(NavSection {
            title: Some("Guides".to_owned()),
            entries: site
                .tutorials
                .iter()
                .map(|t| NavEntry {
                    label: t.title.clone(),
                    url_stem: t.url_stem.clone(),
                })
                .collect(),
        });
    }

    sections
}

/// A titled group of navigation entries.
struct NavSection {
    title: Option<String>,
    entries: Vec<NavEntry>,
}

/// Compute the `../`-chain from a page at `url_stem` back to the
/// site root. A page at `a/b/c` is two directories deep, so the
/// prefix is `../../`.
fn url_prefix(url_stem: &str) -> String {
    let depth = url_stem.matches('/').count();
    "../".repeat(depth)
}

/// Write one page file under `out_dir`, creating parent directories.
fn write_page(
    out_dir: &Path,
    url_stem: &str,
    content: &str,
    format: Format,
) -> Result<(), RenderError> {
    let rel = format!("{url_stem}{}", format.ext());
    let path = out_dir.join(&rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| RenderError::Io(format!("creating {}: {e}", parent.display())))?;
    }
    fs::write(&path, content)
        .map_err(|e| RenderError::Io(format!("writing {}: {e}", path.display())))?;
    Ok(())
}

/// Write the HTML site's static assets to `<out_dir>/_assets/`. If
/// the markdown source tree carries a `_assets/` directory, its
/// files override the built-in defaults.
fn write_assets(md_dir: &Path, out_dir: &Path) -> Result<(), RenderError> {
    let assets_dir = out_dir.join("_assets");
    fs::create_dir_all(&assets_dir)
        .map_err(|e| RenderError::Io(format!("creating {}: {e}", assets_dir.display())))?;
    fs::write(assets_dir.join("style.css"), assets::STYLE_CSS)
        .map_err(|e| RenderError::Io(format!("writing style.css: {e}")))?;
    fs::write(assets_dir.join("nav.js"), assets::NAV_JS)
        .map_err(|e| RenderError::Io(format!("writing nav.js: {e}")))?;
    // Author override: copy any files from a source `_assets/` dir
    // on top of the defaults.
    let src_assets = md_dir.join("_assets");
    if src_assets.is_dir() {
        copy_dir_flat(&src_assets, &assets_dir)?;
    }
    Ok(())
}

/// Copy every regular file from `src` into `dst` (one level; nested
/// directories under `_assets/` are copied recursively).
fn copy_dir_flat(src: &Path, dst: &Path) -> Result<(), RenderError> {
    for entry in
        fs::read_dir(src).map_err(|e| RenderError::Io(format!("reading {}: {e}", src.display())))?
    {
        let entry =
            entry.map_err(|e| RenderError::Io(format!("reading {}: {e}", src.display())))?;
        let path = entry.path();
        let name = path.file_name().unwrap_or_default();
        let target = dst.join(name);
        if path.is_dir() {
            fs::create_dir_all(&target)
                .map_err(|e| RenderError::Io(format!("creating {}: {e}", target.display())))?;
            copy_dir_flat(&path, &target)?;
        } else {
            fs::copy(&path, &target)
                .map_err(|e| RenderError::Io(format!("copying {}: {e}", path.display())))?;
        }
    }
    Ok(())
}

/// Read the package's `*.pkg.kio` source, if present.
fn read_package_file_source(package_root: &Path) -> Option<String> {
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
    fs::read_to_string(hits.into_iter().next()?).ok()
}

// =========================================================================
// Page bodies — produced as a Markdown or HTML *body* fragment; the
// nav wrapper adds the skeleton.
// =========================================================================

/// Render the index page body.
fn render_index(site: &Site, format: Format) -> String {
    let mut md = String::new();
    md.push_str(&format!("# {} documentation\n\n", site.package_name));

    if !site.modules.is_empty() {
        md.push_str("## Modules\n\n");
        for module in &site.modules {
            let summary = module
                .module_doc
                .as_deref()
                .map(first_paragraph)
                .unwrap_or_default();
            let ext = format.ext();
            if summary.is_empty() {
                md.push_str(&format!(
                    "- [`{}`]({}{})\n",
                    module.module_path, module.url_stem, ext
                ));
            } else {
                md.push_str(&format!(
                    "- [`{}`]({}{}) — {}\n",
                    module.module_path, module.url_stem, ext, summary
                ));
            }
        }
        md.push('\n');
    }

    if site.package_boundary.is_some() {
        md.push_str("## Package surface\n\n");
        md.push_str(&format!(
            "- [Package Boundary](package-boundary{}) — the package's host and bridge items.\n\n",
            format.ext()
        ));
    }

    if !site.tutorials.is_empty() {
        md.push_str("## Guides\n\n");
        for tutorial in &site.tutorials {
            md.push_str(&format!(
                "- [{}]({}{})\n",
                tutorial.title,
                tutorial.url_stem,
                format.ext()
            ));
        }
        md.push('\n');
    }

    finish_body(&md, format)
}

/// Render a module's documentation page body.
fn render_module_page(
    module: &site::ModulePage,
    index: &site::SymbolIndex,
    scope: Option<&scope::ModuleScope>,
    format: Format,
) -> String {
    let prefix = url_prefix(&module.url_stem);
    let scope_dyn: Option<&dyn DirectiveScope> = scope.map(|s| s as &dyn DirectiveScope);
    let mut md = String::new();
    md.push_str(&format!("# Module `{}`\n\n", module.module_path));

    // The module-level doc-comment.
    if let Some(doc) = &module.module_doc {
        let overrides = collect_ref_overrides(doc);
        md.push_str(&rewrite_or_plain(
            doc, &prefix, format, index, scope_dyn, &overrides,
        ));
        md.push_str("\n\n");
    }

    let mut next_item = 0;
    for (before_item, doc) in &module.group_docs {
        render_item_sections(
            &mut md,
            &module.items[next_item..*before_item],
            &prefix,
            index,
            scope_dyn,
            format,
        );
        md.push_str("## Recursive group\n\n");
        let overrides = collect_ref_overrides(doc);
        md.push_str(&rewrite_or_plain(
            doc, &prefix, format, index, scope_dyn, &overrides,
        ));
        md.push_str("\n\n");
        next_item = *before_item;
    }
    render_item_sections(
        &mut md,
        &module.items[next_item..],
        &prefix,
        index,
        scope_dyn,
        format,
    );

    finish_body(&md, format)
}

/// Render an item-list page body.
fn render_item_page(
    title: &str,
    items: &[ItemSection],
    index: &site::SymbolIndex,
    scopes: &BTreeMap<&str, scope::ModuleScope>,
    url_stem: &str,
    format: Format,
) -> String {
    let prefix = url_prefix(url_stem);
    let mut md = format!("# {title}\n\n");
    for item in items {
        let scope = scopes
            .get(item.module_path.as_str())
            .expect("a boundary section retains its parsed module owner");
        render_item_sections(
            &mut md,
            std::slice::from_ref(item),
            &prefix,
            index,
            Some(scope),
            format,
        );
    }
    finish_body(&md, format)
}

/// Append each item section to `md`.
fn render_item_sections(
    md: &mut String,
    items: &[ItemSection],
    prefix: &str,
    index: &site::SymbolIndex,
    scope: Option<&dyn DirectiveScope>,
    format: Format,
) {
    for item in items {
        // The anchor is emitted as a raw HTML span for HTML output
        // (markdown headings get auto-ids, but a stable explicit id
        // is what the URL scheme promises); Markdown output uses an
        // HTML anchor too — GitHub honors it.
        md.push_str(&format!(
            "<h3 id=\"{}\"><code>{}</code> <span class=\"item-kind\">{}</span></h3>\n\n",
            item.anchor, item.name, item.kind
        ));
        md.push_str(&format!(
            "<pre class=\"sig\"><code>{}</code></pre>\n\n",
            markdown::highlight_kio_item_signature(&item.signature)
        ));
        if let Some(doc) = &item.doc {
            let overrides = collect_ref_overrides(doc);
            md.push_str(&rewrite_or_plain(
                doc, prefix, format, index, scope, &overrides,
            ));
            md.push_str("\n\n");
        }
    }
}

/// Render a tutorial / guide page body — the source markdown with
/// Kiodoc directives and intra-doc links rewritten.
fn render_tutorial_page(
    source: &str,
    url_stem: &str,
    index: &site::SymbolIndex,
    scope: &dyn DirectiveScope,
    format: Format,
) -> String {
    let prefix = url_prefix(url_stem);
    let overrides = collect_ref_overrides(source);
    let rewritten = rewrite::rewrite(source, &prefix, format.ext(), index, scope, &overrides);
    finish_body(&rewritten, format)
}

/// Rewrite a doc-comment / prose fragment, tolerating a missing
/// scope (an unparseable module yields no scope — the directives
/// were already validated, so they pass through verbatim).
fn rewrite_or_plain(
    prose: &str,
    prefix: &str,
    format: Format,
    index: &site::SymbolIndex,
    scope: Option<&dyn DirectiveScope>,
    overrides: &HashSet<String>,
) -> String {
    match scope {
        Some(s) => rewrite::rewrite(prose, prefix, format.ext(), index, s, overrides),
        None => {
            // No scope: still rewrite intra-doc links via a
            // resolve-nothing scope so `` [`name`] `` becomes a link.
            let empty = scope::PackageBoundaryScope::from_bridged_modules(None, std::iter::empty());
            rewrite::rewrite(prose, prefix, format.ext(), index, &empty, overrides)
        }
    }
}

/// Finish a page body: for HTML, run the rewritten Markdown through
/// the HTML renderer; for Markdown, return it as-is.
fn finish_body(md: &str, format: Format) -> String {
    match format {
        Format::Html => markdown::render_html(md),
        Format::Markdown => md.to_owned(),
    }
}

/// The first paragraph of a markdown body — used as a module
/// summary on the index page. Stops at the first blank line.
fn first_paragraph(md: &str) -> String {
    let mut para = String::new();
    for line in md.lines() {
        if line.trim().is_empty() {
            if !para.is_empty() {
                break;
            }
            continue;
        }
        // Skip a leading heading line.
        if line.trim_start().starts_with('#') && para.is_empty() {
            continue;
        }
        if !para.is_empty() {
            para.push(' ');
        }
        para.push_str(line.trim());
    }
    para
}

// =========================================================================
// Navigation wrapper — wraps a page body in the format skeleton.
// =========================================================================

/// Wrap a rendered page body in the per-format skeleton + sidebar.
fn nav_wrap(body: &str, nav: &[NavSection], url_stem: &str, format: Format) -> String {
    match format {
        Format::Html => html_skeleton(body, nav, url_stem),
        Format::Markdown => md_skeleton(body, nav, url_stem),
    }
}

/// Assemble a complete HTML page.
fn html_skeleton(body: &str, nav: &[NavSection], url_stem: &str) -> String {
    let prefix = url_prefix(url_stem);
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\" />\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n");
    out.push_str(&format!(
        "<link rel=\"stylesheet\" href=\"{prefix}_assets/style.css\" />\n"
    ));
    out.push_str(&format!(
        "<script src=\"{prefix}_assets/nav.js\" defer></script>\n"
    ));
    out.push_str("<title>kio docs</title>\n</head>\n<body>\n");
    out.push_str("<div class=\"layout\">\n");
    out.push_str("<button class=\"toggle\">Menu</button>\n");
    out.push_str(&html_sidebar(nav, &prefix));
    out.push_str("<main>\n");
    out.push_str(body);
    out.push_str("</main>\n</div>\n</body>\n</html>\n");
    out
}

/// Render the HTML sidebar.
fn html_sidebar(nav: &[NavSection], prefix: &str) -> String {
    let mut out = String::from("<nav class=\"sidebar\">\n");
    for section in nav {
        if let Some(title) = &section.title {
            out.push_str(&format!("<h2>{}</h2>\n", markdown::escape_html(title)));
        }
        out.push_str("<ul>\n");
        for entry in &section.entries {
            out.push_str(&format!(
                "<li><a href=\"{}{}.html\">{}</a></li>\n",
                prefix,
                entry.url_stem,
                markdown::escape_html(&entry.label)
            ));
        }
        out.push_str("</ul>\n");
    }
    out.push_str("</nav>\n");
    out
}

/// Assemble a complete Markdown page — the sidebar is rendered as a
/// leading navigation list, since Markdown has no layout chrome.
fn md_skeleton(body: &str, nav: &[NavSection], url_stem: &str) -> String {
    let prefix = url_prefix(url_stem);
    let mut out = String::new();
    out.push_str("<!-- Navigation -->\n\n");
    for section in nav {
        if let Some(title) = &section.title {
            out.push_str(&format!("**{title}**\n\n"));
        }
        for entry in &section.entries {
            out.push_str(&format!(
                "- [{}]({}{}.md)\n",
                entry.label, prefix, entry.url_stem
            ));
        }
        out.push('\n');
    }
    out.push_str("---\n\n");
    out.push_str(body);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_item_comments_keep_their_distinct_module_scopes() {
        let mut scopes = BTreeMap::new();
        let mut index = site::SymbolIndex::default();
        let mut items = Vec::new();
        for owner in ["api/left", "api/right"] {
            let source =
                format!("module {owner}; fn helper() -> . {{ () }} pub fn value() -> . {{ () }}");
            let scope = scope::ModuleScope::from_source(&source).unwrap();
            let rewrite::LinkTarget::Exact(helper_anchor) = scope.link_target("helper") else {
                panic!("the actual module scope selects its private helper");
            };
            index.insert("helper", owner, &helper_anchor);
            let rewrite::LinkTarget::Exact(value_anchor) = scope.link_target("value") else {
                panic!("the actual module scope selects its public function");
            };
            items.push(ItemSection {
                name: "value".to_owned(),
                module_path: owner.to_owned(),
                anchor: value_anchor,
                kind: "fn",
                signature: "pub fn value() -> .".to_owned(),
                doc: Some("Uses [`helper`].".to_owned()),
            });
            scopes.insert(owner, scope);
        }
        for format in [Format::Html, Format::Markdown] {
            let body = render_item_page(
                "Boundary",
                &items,
                &index,
                &scopes,
                "package-boundary",
                format,
            );
            for owner in ["left", "right"] {
                let target = format!("api/{owner}{}#item-api_2f{owner}-helper", format.ext());
                assert_eq!(body.matches(&target).count(), 1, "{body}");
            }
        }
    }

    #[test]
    fn url_prefix_by_depth() {
        assert_eq!(url_prefix("index"), "");
        assert_eq!(url_prefix("pkg/main"), "../");
        assert_eq!(url_prefix("pkg/util/thing"), "../../");
    }

    #[test]
    fn first_paragraph_stops_at_blank() {
        assert_eq!(
            first_paragraph("# Title\n\nFirst para.\nStill first.\n\nSecond."),
            "First para. Still first."
        );
    }

    #[test]
    fn first_paragraph_skips_heading() {
        assert_eq!(first_paragraph("# Heading\nbody text"), "body text");
    }
}
