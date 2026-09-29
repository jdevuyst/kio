//! Render a [`SignatureFile<Surface>`] changelog to canonical
//! `*.sig.kio` source text.
//!
//! The emitted text is the inverse of
//! [`crate::pass::parser::parse_signature_file`]: a `signature <pkg>
//! v(<N>);` header followed by oldest-first per-version blocks, each
//! partitioning its changes into `breaking` / `nonbreaking` sections
//! of `add` / `modify` / `remove` blocks, with braced `module <path> {
//! … }` sections inside `add` / `modify`.
//!
//! Declarations are rendered through the regular-module pretty-printer
//! (`crate::pretty`) so the recorded Kio′ signatures read exactly as a
//! user would write them, then re-indented to sit inside the sig
//! framing. Enclosing sequences place semicolons between the resulting
//! suffix-free declarations and sections after canonical ordering.

use crate::ast::{
    Import, Item, SigChangeSet, SigItem, SigItemRef, SigModuleSection, SigRemoveModule, SigVersion,
    SignatureFile, Surface, TypeRecMember,
};
use crate::pass::lexer::Trivia;

const INDENT: &str = "  ";

/// Render the whole changelog to canonical `*.sig.kio` text.
pub fn emit_signature_file(file: &SignatureFile<Surface>) -> String {
    SignatureRenderer { trivia: true }.emit_file(file)
}

/// Canonical comparison text uses the same projection and ordering as source
/// rendering, with presentation removed before width-dependent layout.
#[cfg(all(feature = "cli", feature = "surface"))]
pub(crate) fn emit_signature_comparison(file: &SignatureFile<Surface>) -> String {
    SignatureRenderer { trivia: false }.emit_file(file)
}

struct SignatureRenderer {
    trivia: bool,
}

impl SignatureRenderer {
    fn emit_file(&self, file: &SignatureFile<Surface>) -> String {
        let mut out = String::new();
        self.emit_comments(&mut out, file.meta.leading_trivia.as_slice(), 0);
        out.push_str(&format!("signature {} v({});\n", file.pkg, file.version));

        // Versions are emitted oldest-first regardless of in-memory order,
        // matching the parser's presentational ordering.
        let mut versions: Vec<&SigVersion<Surface>> = file.versions.iter().collect();
        versions.sort_by_key(|v| v.version);
        for version in versions {
            out.push('\n');
            self.emit_version(&mut out, version);
            out.push('\n');
        }
        self.emit_comments(&mut out, file.meta.trailing_trivia.as_slice(), 0);
        out
    }

    fn emit_version(&self, out: &mut String, version: &SigVersion<Surface>) {
        self.emit_comments(out, &version.leading_trivia, 0);
        // The version's changelog message renders as a `///` doc-comment run
        // immediately above the `v(<N>)` head (the inverse of the parser's
        // doc-extraction off the `v` token's leading trivia). A blank message
        // line renders as a bare `///`, matching `specs/style.md`.
        if let Some(doc) = version.doc.as_ref().filter(|_| self.trivia) {
            for line in &doc.lines {
                if line.is_empty() {
                    out.push_str("///\n");
                } else {
                    out.push_str(&format!("/// {line}\n"));
                }
            }
        }
        out.push_str(&format!("v({}) {{\n", version.version));
        let mut entries = Vec::new();
        if !version.with.is_empty() {
            entries.push(self.render_entry(|out| {
                self.emit_with_block(
                    out,
                    &version.with,
                    &version.with_leading_trivia,
                    &version.with_trailing_trivia,
                    1,
                )
            }));
        }
        if let Some(breaking) = &version.breaking {
            entries
                .push(self.render_entry(|out| self.emit_change_set(out, "breaking", breaking, 1)));
        }
        if let Some(nonbreaking) = &version.nonbreaking {
            entries.push(
                self.render_entry(|out| self.emit_change_set(out, "nonbreaking", nonbreaking, 1)),
            );
        }
        self.emit_entries(out, entries);
        self.emit_comments(out, &version.trailing_trivia, 1);
        out.push('}');
    }

    fn render_entry(&self, emit: impl FnOnce(&mut String)) -> String {
        let mut entry = String::new();
        emit(&mut entry);
        entry
    }

    fn emit_entries(&self, out: &mut String, entries: Vec<String>) {
        let entries: Vec<_> = entries
            .into_iter()
            .filter(|entry| !entry.is_empty())
            .collect();
        if !entries.is_empty() {
            out.push_str(&entries.join(";\n"));
            out.push('\n');
        }
    }

    fn emit_comments(&self, out: &mut String, trivia: &[Trivia], depth: usize) {
        if !self.trivia {
            return;
        }
        let comments = crate::pretty::pretty_leading_trivia(trivia);
        if !comments.is_empty() {
            out.push_str(&self.indent_entry(&comments, &INDENT.repeat(depth)));
            out.push('\n');
        }
    }

    fn emit_change_set(
        &self,
        out: &mut String,
        name: &str,
        set: &SigChangeSet<Surface>,
        depth: usize,
    ) {
        // An empty partition is never emitted (the parser omits an empty
        // `breaking` / `nonbreaking`); the caller only calls this for a
        // present section, but guard anyway so a stray empty set produces
        // no noise.
        if set.add.is_empty()
            && set.add_refs.is_empty()
            && set.modify.is_empty()
            && set.modify_refs.is_empty()
            && set.remove.is_empty()
            && set.remove_refs.is_empty()
        {
            return;
        }
        let pad = INDENT.repeat(depth);
        self.emit_comments(out, &set.leading_trivia, depth);
        out.push_str(&format!("{pad}{name} {{\n"));
        let mut entries = Vec::new();
        if !set.add.is_empty() || !set.add_refs.is_empty() {
            entries.push(self.render_entry(|out| {
                self.emit_operation_block(
                    out,
                    "add",
                    &set.add,
                    &set.add_refs,
                    (&set.add_leading_trivia, &set.add_trailing_trivia),
                    depth + 1,
                )
            }));
        }
        if !set.modify.is_empty() || !set.modify_refs.is_empty() {
            entries.push(self.render_entry(|out| {
                self.emit_operation_block(
                    out,
                    "modify",
                    &set.modify,
                    &set.modify_refs,
                    (&set.modify_leading_trivia, &set.modify_trailing_trivia),
                    depth + 1,
                )
            }));
        }
        if !set.remove.is_empty() || !set.remove_refs.is_empty() {
            entries.push(self.render_entry(|out| {
                self.emit_remove_block(
                    out,
                    &set.remove,
                    &set.remove_refs,
                    &set.remove_leading_trivia,
                    &set.remove_trailing_trivia,
                    depth + 1,
                )
            }));
        }
        self.emit_entries(out, entries);
        self.emit_comments(out, &set.trailing_trivia, depth + 1);
        out.push_str(&format!("{pad}}}"));
    }

    fn emit_with_block(
        &self,
        out: &mut String,
        sections: &[SigModuleSection<Surface>],
        leading: &[Trivia],
        trailing: &[Trivia],
        depth: usize,
    ) {
        let pad = INDENT.repeat(depth);
        self.emit_comments(out, leading, depth);
        out.push_str(&format!("{pad}with {{\n"));
        let mut sections: Vec<&SigModuleSection<Surface>> = sections.iter().collect();
        sections.sort_by_key(|section| module_path_str(&section.path));
        self.emit_entries(
            out,
            sections
                .into_iter()
                .map(|section| {
                    self.render_entry(|out| self.emit_module_section(out, section, depth + 1))
                })
                .collect(),
        );
        self.emit_comments(out, trailing, depth + 1);
        out.push_str(&format!("{pad}}}"));
    }

    fn emit_operation_block(
        &self,
        out: &mut String,
        op: &str,
        sections: &[SigModuleSection<Surface>],
        refs: &[SigItemRef],
        trivia: (&[Trivia], &[Trivia]),
        depth: usize,
    ) {
        let (leading, trailing) = trivia;
        let pad = INDENT.repeat(depth);
        self.emit_comments(out, leading, depth);
        out.push_str(&format!("{pad}{op} {{\n"));
        // Sections render in module-path order regardless of in-memory
        // order, so a hand-edited or merge-reordered draft emits to the same
        // canonical text the recompute side produces (the equality check in
        // `cmd::sig::drafts_equal` compares emitted text). This mirrors the
        // recompute side's BTreeMap-by-module ordering in
        // `RecordedSurface::sections_for`.
        let inner = INDENT.repeat(depth + 1);
        let mut refs: Vec<&SigItemRef> = refs.iter().collect();
        refs.sort_by_key(|reference| item_ref_str(reference));
        let mut entries = Vec::new();
        for reference in refs {
            entries.push(self.render_entry(|out| {
                self.emit_comments(out, &reference.leading_trivia, depth + 1);
                out.push_str(&format!("{inner}{}", item_ref_str(reference)));
            }));
        }
        let mut sections: Vec<&SigModuleSection<Surface>> = sections.iter().collect();
        sections.sort_by_key(|s| module_path_str(&s.path));
        for section in sections {
            entries
                .push(self.render_entry(|out| self.emit_module_section(out, section, depth + 1)));
        }
        self.emit_entries(out, entries);
        self.emit_comments(out, trailing, depth + 1);
        out.push_str(&format!("{pad}}}"));
    }

    fn emit_module_section(
        &self,
        out: &mut String,
        section: &SigModuleSection<Surface>,
        depth: usize,
    ) {
        let pad = INDENT.repeat(depth);
        let path = module_path_str(&section.path);
        self.emit_comments(out, &section.leading_trivia, depth);
        out.push_str(&format!("{pad}module {path} {{\n"));
        let inner = INDENT.repeat(depth + 1);
        // `import` clauses render in canonical order (the imported head/alias),
        // so a reordered set of clauses emits identically — the recompute
        // side appends synthesized self-imports last, but the emitted order
        // is normalized here so the two sides still compare equal.
        let mut items: Vec<SigItem<Surface>> = section
            .items
            .iter()
            .map(super::project_signature_item)
            .collect();
        let projected_imports = super::record::imports_needed_by(&items, section.imports.clone());
        let mut imports: Vec<&Import> = projected_imports.iter().collect();
        imports.sort_by_key(|u| import_sort_key(u));
        let mut entries = Vec::new();
        for import_clause in &imports {
            entries.push(self.render_entry(|out| {
                self.emit_comments(out, &import_clause.leading_trivia, depth + 1);
                out.push_str(&self.indent_entry(&self.emit_import(import_clause), &inner));
            }));
        }
        // Items render in declared-name order, mirroring the recompute side's
        // `names.sort()` in `RecordedSurface::sections_for`, so a reordered
        // section is still canonical text.
        items.sort_by(|a, b| sig_item_name(a).cmp(sig_item_name(b)));
        for item in &items {
            let rendered = self.emit_sig_item(item);
            entries.push(self.indent_entry(&rendered, &inner));
        }
        self.emit_entries(out, entries);
        self.emit_comments(out, &section.trailing_trivia, depth + 1);
        out.push_str(&format!("{pad}}}"));
    }

    fn indent_entry(&self, entry: &str, indent: &str) -> String {
        entry
            .lines()
            .map(|line| {
                if line.is_empty() {
                    String::new()
                } else {
                    format!("{indent}{line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn emit_remove_block(
        &self,
        out: &mut String,
        removes: &[SigRemoveModule],
        refs: &[SigItemRef],
        leading: &[Trivia],
        trailing: &[Trivia],
        depth: usize,
    ) {
        let pad = INDENT.repeat(depth);
        self.emit_comments(out, leading, depth);
        out.push_str(&format!("{pad}remove {{\n"));
        // Canonical output uses exact FQNs even when the parsed input used the
        // legacy nested-module removal spelling.
        let mut names = refs
            .iter()
            .map(|reference| (item_ref_str(reference), reference.leading_trivia.clone()))
            .collect::<Vec<_>>();
        for remove in removes {
            let path = module_path_str(&remove.path);
            names.extend(remove.names.iter().enumerate().map(|(index, name)| {
                let mut trivia = Vec::new();
                if index == 0 {
                    trivia.extend_from_slice(&remove.leading_trivia);
                }
                trivia.extend_from_slice(&name.leading_trivia);
                (format!("{path}.{}", name.name), trivia)
            }));
        }
        names.sort_by(|a, b| a.0.cmp(&b.0));
        let inner = INDENT.repeat(depth + 1);
        self.emit_entries(
            out,
            names
                .into_iter()
                .map(|(name, trivia)| {
                    self.render_entry(|out| {
                        self.emit_comments(out, &trivia, depth + 1);
                        out.push_str(&format!("{inner}{name}"));
                    })
                })
                .collect(),
        );
        for remove in removes {
            self.emit_comments(out, &remove.trailing_trivia, depth + 1);
        }
        self.emit_comments(out, trailing, depth + 1);
        out.push_str(&format!("{pad}}}"));
    }

    /// Render one signature item to its canonical declaration text (no
    /// indentation or terminating semicolon — the caller owns both).
    /// Shared declarations and body-less exports use the regular pretty-printer.
    fn emit_sig_item(&self, item: &SigItem<Surface>) -> String {
        let mut comparison_item;
        let item = if self.trivia {
            item
        } else {
            comparison_item = item.clone();
            omit_item_trivia(&mut comparison_item);
            &comparison_item
        };
        match item {
            SigItem::HostType(h) => {
                crate::pretty::pretty_item_block_entry(&Item::HostType(h.clone()))
            }
            SigItem::HostFn(h) => crate::pretty::pretty_item_block_entry(&Item::HostFn(h.clone())),
            SigItem::TypeAlias(a) => {
                crate::pretty::pretty_item_block_entry(&Item::TypeAlias(a.clone()))
            }
            SigItem::Newtype(n) => crate::pretty::pretty_item_block_entry(&Item::Newtype(
                super::project_newtype_declaration(n.clone()),
            )),
            SigItem::TypeRecGroup(group) => {
                crate::pretty::pretty_item_block_entry(&Item::TypeRecGroup(group.clone()))
            }
            SigItem::ExportFn(export) => crate::pretty::pretty_export_fn_block_entry(export),
        }
    }

    fn emit_import(&self, import_clause: &Import) -> String {
        let mut comparison_import;
        let import_clause = if self.trivia {
            import_clause
        } else {
            comparison_import = import_clause.clone();
            comparison_import.leading_trivia.clear();
            comparison_import.trailing_trivia.clear();
            if let crate::ast::ImportKind::Selective { items, .. } = &mut comparison_import.kind {
                for item in items {
                    item.leading_trivia_mut().clear();
                }
            }
            &comparison_import
        };
        crate::pretty::pretty_import_block_entry(import_clause)
    }
}

fn omit_meta_trivia(meta: &mut crate::ast::Meta<Surface>) {
    meta.leading_trivia.clear();
    meta.trailing_trivia.clear();
}

fn omit_type_trivia(ty: &mut crate::ast::Type<Surface>) {
    use crate::ast::Type;
    omit_meta_trivia(ty.meta_mut());
    match ty {
        Type::Path { args, .. } => args.iter_mut().for_each(omit_type_trivia),
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
        } => {
            omit_type_trivia(param);
            omit_type_trivia(ret);
        }
        Type::Forall { body, .. } => omit_type_trivia(body),
        Type::LabelSugar { labels, .. } => {
            for label in labels {
                omit_meta_trivia(&mut label.meta);
                if let Some(payload) = &mut label.payload {
                    omit_type_trivia(payload);
                }
            }
        }
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => {}
        Type::Goal { ext, .. } => match *ext {},
    }
}

fn omit_host_fn_trivia(function: &mut crate::ast::HostFn<Surface>) {
    function.doc = None;
    omit_meta_trivia(&mut function.meta);
    for param in &mut function.params {
        if let crate::ast::HostFnParam::Value(param) = param {
            omit_meta_trivia(&mut param.meta);
            omit_type_trivia(&mut param.ty);
        }
    }
    omit_type_trivia(&mut function.ret);
}

fn omit_alias_trivia(alias: &mut crate::ast::TypeAlias<Surface>) {
    alias.doc = None;
    omit_meta_trivia(&mut alias.meta);
    omit_type_trivia(&mut alias.body);
}

fn omit_newtype_trivia(newtype: &mut crate::ast::Newtype<Surface>) {
    newtype.doc = None;
    omit_meta_trivia(&mut newtype.meta);
    newtype.constructor.leading_trivia.clear();
    newtype.projector.leading_trivia.clear();
    omit_type_trivia(&mut newtype.payload);
}

fn omit_item_trivia(item: &mut SigItem<Surface>) {
    match item {
        SigItem::HostType(host) => {
            host.doc = None;
            omit_meta_trivia(&mut host.meta);
        }
        SigItem::HostFn(function) => omit_host_fn_trivia(function),
        SigItem::ExportFn(export) => omit_host_fn_trivia(&mut export.function),
        SigItem::TypeAlias(alias) => omit_alias_trivia(alias),
        SigItem::Newtype(newtype) => omit_newtype_trivia(newtype),
        SigItem::TypeRecGroup(group) => {
            group.doc = None;
            omit_meta_trivia(&mut group.meta);
            for member in &mut group.members {
                match member {
                    TypeRecMember::TypeAlias(alias) => omit_alias_trivia(alias),
                    TypeRecMember::Newtype(newtype) => omit_newtype_trivia(newtype),
                    TypeRecMember::Labels(labels, _) => {
                        labels.doc = None;
                        omit_meta_trivia(&mut labels.meta);
                        for arm in labels.type_alias_arms.iter_mut().flatten() {
                            omit_meta_trivia(&mut arm.meta);
                            for entry in &mut arm.entries {
                                omit_meta_trivia(&mut entry.meta);
                                omit_type_trivia(&mut entry.payload);
                            }
                        }
                        for entry in &mut labels.entries {
                            omit_meta_trivia(&mut entry.meta);
                            omit_type_trivia(&mut entry.payload);
                        }
                    }
                }
            }
        }
    }
}

/// The declared name of a [`SigItem`] — the key the canonical emit order
/// sorts items by (mirroring the recompute side's `names.sort()`).
fn sig_item_name(item: &SigItem<Surface>) -> &str {
    match item {
        SigItem::HostType(h) => &h.name,
        SigItem::HostFn(h) => &h.name,
        SigItem::ExportFn(export) => &export.function.name,
        SigItem::TypeAlias(a) => &a.name,
        SigItem::Newtype(n) => &n.name,
        SigItem::TypeRecGroup(group) => group
            .members
            .first()
            .map(type_rec_member_name)
            .unwrap_or(""),
    }
}

fn type_rec_member_name(member: &TypeRecMember<Surface>) -> &str {
    match member {
        TypeRecMember::TypeAlias(alias) => &alias.name,
        TypeRecMember::Newtype(newtype) => &newtype.name,
        TypeRecMember::Labels(labels, _) => labels
            .type_alias_name
            .as_deref()
            .unwrap_or("<anonymous labels>"),
    }
}

fn item_ref_str(reference: &SigItemRef) -> String {
    format!("{}.{}", module_path_str(&reference.path), reference.name)
}

/// A stable sort key for an `import` clause, so a reordered set of clauses
/// emits in one canonical order. A selective import keys on its first
/// imported name; a qualified import on its alias.
fn import_sort_key(u: &Import) -> String {
    use crate::ast::{ImportItem, ImportKind};
    match &u.kind {
        ImportKind::Selective { items, .. } => items
            .iter()
            .filter_map(ImportItem::as_name)
            .next()
            .unwrap_or("")
            .to_owned(),
        ImportKind::Qualified { alias, .. } => alias.clone(),
        // Compile-time type references retain their explicit
        // `import __comptime__;` authority. Intrinsics contribute no type heads.
        // Sort either block import before named imports.
        ImportKind::Intrinsics | ImportKind::Comptime => String::new(),
    }
}

fn module_path_str(p: &crate::ast::ModulePath) -> String {
    p.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_semicolons_join_sorted_sections_imports_and_declarations() {
        let source = "signature demo v(1);
            v(1) { ;
                with { ; module z { type Z = .; }; module a { type A = .; }; };
                breaking { ;
                    add { ; z.Z; module y { type Y = .; }; };
                    modify { ; module x { type X = .; }; };
                    remove { ; z.old; a.old; };
                };
                nonbreaking { ; add { ; module m {
                    import z as z;
                    import a as a;
                    host type Z;
                    type A = a.A;
                    type B = z.Z;
                    pub pure fn run(value: A) -> A;
                }; }; };
            };";
        let parsed = crate::pass::parser::parse_signature_file(source, None).unwrap();
        let rendered = emit_signature_file(&parsed);
        let reparsed = crate::pass::parser::parse_signature_file(&rendered, None).unwrap();
        assert_eq!(rendered, emit_signature_file(&reparsed));
        for expected in [
            "type A = .\n",
            "    };\n    module z",
            "  };\n  breaking",
            "      z.Z;\n      module y",
            "    };\n    modify",
            "    };\n    remove",
            "      a.old;\n      z.old\n",
            "  };\n  nonbreaking",
            "        import a as a;\n        import z as z;\n        type A = a.A;\n        type B = z.Z;\n        host type Z;\n        pub pure fn run(value: A) -> A\n",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected:?}:\n{rendered}"
            );
        }
    }

    #[test]
    fn signature_semicolon_comments_survive_sorted_children_and_each_closer() {
        let source = "// header note
            signature demo v(1);
            // version note
            v(1) { ; // with note
                with { ; // context note
                    module context { ; type Context = .; // context closer
                    }; // with closer
                }; // partition note
                nonbreaking { ; // add note
                    add { ; // z note
                        z.Z;
                        // a note
                        a.A;
                        // module note
                        module m { ; // declaration note
                            type Item = .;
                            // export note
                            pub fn run(value: Item) -> Item;
                            // module closer
                        }; // add closer
                    }; // remove note
                    remove { ; // old note
                        m.old;
                        // remove closer
                    }; // partition closer
                }; // version closer
            }; // file closer
        ";
        let parsed = crate::pass::parser::parse_signature_file(source, None).unwrap();
        let rendered = emit_signature_file(&parsed);
        for comment in [
            "header note",
            "version note",
            "with note",
            "context note",
            "context closer",
            "with closer",
            "partition note",
            "add note",
            "z note",
            "a note",
            "module note",
            "declaration note",
            "export note",
            "module closer",
            "add closer",
            "remove note",
            "old note",
            "remove closer",
            "partition closer",
            "version closer",
            "file closer",
        ] {
            assert_eq!(rendered.matches(comment).count(), 1, "{rendered}");
        }
        assert!(rendered.find("// a note").unwrap() < rendered.find("// z note").unwrap());
        let reparsed = crate::pass::parser::parse_signature_file(&rendered, None).unwrap();
        assert_eq!(rendered, emit_signature_file(&reparsed));
    }
}
