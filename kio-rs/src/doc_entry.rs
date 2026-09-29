//! The single chokepoint for **documentable declarations**.
//!
//! Every consumer that walks a module looking for declarations to
//! document — `kio doc` (the Kiodoc site renderer), the REPL's `:doc`
//! command, and any future doc tooling — goes through
//! [`documented_items_module`] and consumes the resulting [`DocEntry`]
//! values rather than matching the AST [`Item`] enum directly.
//!
//! The point is **correct-by-construction ripple coverage**: the
//! `documented_items_module` builder below matches exhaustively with no
//! wildcard arm, so adding a new declaration kind to [`Item`] forces
//! the author to decide — right here — whether it is documentable, and
//! every downstream consumer picks the new kind up for free through the
//! shared enum. A new *consumer* likewise handles every kind by
//! construction (it matches [`DocEntry`], which is closed). This removes
//! the silent-drop failure mode where a new attachment point or a new
//! consumer quietly skips a kind.
//!
//! The field-presence-as-permission half of the design lives in the
//! AST: a documentable declaration struct carries a
//! `doc: Option<DocComment>` field; a non-documentable kind (today
//! only [`crate::ast::Equiv`]) simply lacks the field, so the parser's
//! doc-attachment helpers cannot attach a doc to it — attaching is a
//! compile error, not a silent drop.

use crate::ast::{
    DocComment, FnDef, HostFn, HostType, Item, LabelForward, Labels, LiteralAlias, Module, Newtype,
    Op, OpBody, OperatorGrammar, PackageFile, Surface, TypeAlias, TypeRecGroup, UserElaboratorDef,
    VariadicOperator,
};

/// One documentable declaration, borrowed from a Surface-phase
/// module. Recursive type entries retain their written group owner alongside
/// the selected declaration, and labels expose their generated nominal names.
///
/// Consumers read a `DocEntry` through [`DocEntry::name`],
/// [`DocEntry::doc`], [`DocEntry::kind`], [`DocEntry::anchor`], and
/// [`DocEntry::signature`] — never by re-matching the AST. Not
/// `Copy`: operator spellings and generated nominal names are owned strings.
#[derive(Debug, Clone)]
pub enum DocEntry<'a> {
    Fn(&'a FnDef<Surface>),
    TypeAlias(&'a TypeAlias<Surface>, Option<&'a TypeRecGroup>),
    LiteralAlias(&'a LiteralAlias<Surface>),
    Newtype(&'a Newtype<Surface>, Option<&'a TypeRecGroup>),
    /// The named form `labels T = { … };` only — the anonymous form has
    /// no documentable name (see [`documented_items_module`]).
    Labels(&'a Labels<Surface>, &'a str, Option<&'a TypeRecGroup>),
    /// A nonminting label declaration and its braced selection spelling.
    LabelForward(&'a LabelForward<Surface>, String),
    /// A nominal introduced by labels, rendered from its source owner.
    LabelNominal {
        labels: &'a Labels<Surface>,
        entry: &'a crate::ast::LabelEntry,
        name: String,
        group: Option<&'a TypeRecGroup>,
    },
    /// A fixed operator and its complete tagged grammar.
    Op(&'a Op<Surface>, String),
    /// A variadic operator and its complete tagged import projection.
    VariadicOperator(&'a VariadicOperator<Surface>, String),
    Elaborator(&'a UserElaboratorDef<Surface>),
    HostType(&'a HostType<Surface>),
    HostFn(&'a HostFn<Surface>),
}

impl<'a> DocEntry<'a> {
    /// The resolvable name of the documented declaration.
    pub fn name(&self) -> &str {
        match self {
            DocEntry::Fn(d) => &d.name,
            DocEntry::TypeAlias(a, _) => &a.name,
            DocEntry::LiteralAlias(l) => &l.name,
            DocEntry::Newtype(n, _) => &n.name,
            DocEntry::Labels(_, name, _) => name,
            DocEntry::LabelForward(_, name) => name,
            DocEntry::LabelNominal { name, .. } => name,
            DocEntry::Op(_, name) | DocEntry::VariadicOperator(_, name) => name,
            DocEntry::Elaborator(e) => &e.name,
            DocEntry::HostType(h) => &h.name,
            DocEntry::HostFn(h) => &h.name,
        }
    }

    /// The `///` doc-comment attached to the declaration, if any.
    /// Borrowed from the underlying AST node (lifetime `'a`), so the
    /// reference outlives this `DocEntry` value.
    pub fn doc(&self) -> Option<&'a DocComment> {
        match self {
            DocEntry::Fn(d) => d.doc.as_ref(),
            DocEntry::TypeAlias(a, _) => a.doc.as_ref(),
            DocEntry::LiteralAlias(l) => l.doc.as_ref(),
            DocEntry::Newtype(n, _) => n.doc.as_ref(),
            DocEntry::Labels(t, _, _) | DocEntry::LabelNominal { labels: t, .. } => t.doc.as_ref(),
            DocEntry::LabelForward(forward, _) => forward.doc.as_ref(),
            DocEntry::Op(o, _) => o.doc.as_ref(),
            DocEntry::VariadicOperator(o, _) => o.doc.as_ref(),
            DocEntry::Elaborator(e) => e.doc.as_ref(),
            DocEntry::HostType(h) => h.doc.as_ref(),
            DocEntry::HostFn(h) => h.doc.as_ref(),
        }
    }

    /// The declaration-kind label used in rendered output
    /// (`"fn"`, `"newtype"`, `"host type"`, …).
    pub fn kind(&self) -> &'static str {
        match self {
            DocEntry::Fn(_) => "fn",
            DocEntry::TypeAlias(_, _) => "type",
            DocEntry::LiteralAlias(_) => "literal",
            DocEntry::Newtype(_, _) | DocEntry::LabelNominal { .. } => "newtype",
            DocEntry::Labels(_, _, _) => "labels",
            DocEntry::LabelForward(_, _) => "label",
            DocEntry::Op(_, _) => "op",
            DocEntry::VariadicOperator(_, _) => "varop",
            DocEntry::Elaborator(_) => "elab",
            DocEntry::HostType(_) => "host type",
            DocEntry::HostFn(_) => "host fn",
        }
    }

    /// Stable identity shared by the declaration's module and boundary sections.
    pub fn anchor(&self, module_path: &str) -> String {
        let (kind, identity) = match self {
            Self::Labels(_, name, _) => ("labels", (*name).to_owned()),
            Self::LabelForward(_, name) => ("label", name.clone()),
            Self::Op(_, grammar) | Self::VariadicOperator(_, grammar) => ("op", grammar.clone()),
            other => ("item", other.name().to_owned()),
        };
        format!(
            "{kind}-{}-{}",
            encode_anchor_identity(module_path),
            encode_anchor_identity(&identity)
        )
    }

    /// The rendered signature line for this declaration.
    pub fn signature(&self) -> String {
        crate::pretty::pretty_item_signature(&self.as_item())
    }

    /// The canonical source for this declaration, excluding its doc comment.
    pub fn source(&self) -> String {
        crate::pretty::pretty_item_source(&self.as_item())
    }

    /// The bound-value type for this declaration, when it binds a value.
    pub fn ty(&self) -> Option<String> {
        crate::pretty::pretty_item_type(&self.as_item())
    }

    /// Exact written head of a type-oriented entry, including a label's
    /// lowercase source token when the selected name is its generated nominal.
    pub fn type_name_span(&self) -> Option<crate::span::Span> {
        match self {
            Self::TypeAlias(alias, _) => Some(alias.name_span),
            Self::Newtype(newtype, _) => Some(newtype.name_span),
            Self::Labels(labels, _, _) => labels.type_alias_span,
            Self::LabelNominal { entry, .. } => Some(entry.name_span),
            _ => None,
        }
    }

    /// Selection visibility, independent of peers retained as source context.
    pub fn visibility(&self) -> &crate::ast::Visibility {
        match self {
            Self::Fn(function) => &function.vis,
            Self::TypeAlias(alias, _) => &alias.vis,
            Self::LiteralAlias(literal) => &literal.vis,
            Self::Newtype(newtype, _) => &newtype.vis,
            Self::Labels(labels, _, _) | Self::LabelNominal { labels, .. } => &labels.vis,
            Self::LabelForward(forward, _) => &forward.vis,
            Self::Op(op, _) => &op.vis,
            Self::VariadicOperator(op, _) => &op.vis,
            Self::Elaborator(elaborator) => &elaborator.vis,
            Self::HostType(_) | Self::HostFn(_) => &crate::ast::Visibility::Public,
        }
    }

    pub fn is_exported(&self) -> bool {
        self.visibility().is_exported()
    }

    /// Re-wrap an entry as an owned [`Item`] for the existing
    /// pretty-printer entry point. Cheap clones of borrowed nodes.
    fn as_item(&self) -> Item<Surface> {
        let group = match self {
            Self::TypeAlias(_, group)
            | Self::Newtype(_, group)
            | Self::Labels(_, _, group)
            | Self::LabelNominal { group, .. } => *group,
            _ => None,
        };
        if let Some(group) = group {
            return Item::TypeRecGroup(group.clone());
        }
        match self {
            DocEntry::Fn(d) => Item::FnDef((*d).clone()),
            DocEntry::TypeAlias(a, _) => Item::TypeAlias((*a).clone()),
            DocEntry::LiteralAlias(l) => Item::LiteralAlias((*l).clone(), ()),
            DocEntry::Newtype(n, _) => Item::Newtype((*n).clone()),
            DocEntry::Labels(t, _, _) | DocEntry::LabelNominal { labels: t, .. } => {
                Item::Labels((*t).clone(), ())
            }
            DocEntry::LabelForward(forward, _) => Item::LabelForward((*forward).clone(), ()),
            DocEntry::Op(o, _) => Item::Op(Box::new((*o).clone()), ()),
            DocEntry::VariadicOperator(o, _) => Item::VariadicOperator(Box::new((*o).clone()), ()),
            DocEntry::Elaborator(e) => Item::Elaborator((*e).clone(), ()),
            DocEntry::HostType(h) => Item::HostType((*h).clone()),
            DocEntry::HostFn(h) => Item::HostFn((*h).clone()),
        }
    }
}

fn encode_anchor_identity(identity: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::new();
    for byte in identity.bytes() {
        if byte.is_ascii_alphanumeric() {
            encoded.push(char::from(byte));
        } else {
            encoded.push('_');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
    encoded
}

/// Build every [`DocEntry`] represented by one module-body [`Item`].
/// Most items contribute zero or one entry; a recursion group contributes one
/// entry per member. The match is exhaustive so new declaration kinds cannot
/// silently disappear from documentation consumers.
pub fn documented_entries_for_item(item: &Item<Surface>) -> Vec<DocEntry<'_>> {
    match item {
        Item::FnDef(d) => vec![DocEntry::Fn(d)],
        Item::TypeAlias(a) => vec![DocEntry::TypeAlias(a, None)],
        Item::LiteralAlias(l, _) => vec![DocEntry::LiteralAlias(l)],
        Item::Newtype(n) => vec![DocEntry::Newtype(n, None)],
        Item::Labels(t, _) => labels_entries(t, None),
        Item::LabelForward(forward, _) => {
            vec![DocEntry::LabelForward(
                forward,
                format!("{{{}}}", forward.name),
            )]
        }
        Item::Op(o, _) => {
            let OpBody::Normal { pattern, .. } = &o.body;
            vec![DocEntry::Op(o, OperatorGrammar::fixed(pattern).render())]
        }
        Item::VariadicOperator(o, _) => vec![DocEntry::VariadicOperator(
            o,
            OperatorGrammar::variadic(&o.open, &o.spec).render(),
        )],
        Item::Elaborator(e, _) => vec![DocEntry::Elaborator(e)],
        Item::HostType(h) => vec![DocEntry::HostType(h)],
        Item::HostFn(h) => vec![DocEntry::HostFn(h)],
        Item::RecGroup(g, _) => g.members.iter().map(DocEntry::Fn).collect(),
        Item::TypeRecGroup(g) => g
            .members
            .iter()
            .flat_map(|member| match member {
                crate::ast::TypeRecMember::TypeAlias(alias) => {
                    vec![DocEntry::TypeAlias(alias, Some(g))]
                }
                crate::ast::TypeRecMember::Newtype(newtype) => {
                    vec![DocEntry::Newtype(newtype, Some(g))]
                }
                crate::ast::TypeRecMember::Labels(labels, _) => labels_entries(labels, Some(g)),
            })
            .collect(),
        Item::Equiv(_, _) => Vec::new(),
    }
}

fn labels_entries<'a>(labels: &'a Labels, group: Option<&'a TypeRecGroup>) -> Vec<DocEntry<'a>> {
    let mut entries = Vec::new();
    if let Some(name) = labels.type_alias_name.as_deref() {
        entries.push(DocEntry::Labels(labels, name, group));
    }
    entries.extend(labels.entries.iter().filter_map(|entry| {
        let name = crate::ast::mint_label_newtype_name(&entry.name);
        (!entry.is_reuse_marker() && labels.type_alias_name.as_ref() != Some(&name)).then_some(
            DocEntry::LabelNominal {
                labels,
                entry,
                name,
                group,
            },
        )
    }));
    entries
}

/// Resolve one exact documentable declaration represented by `item`.
pub fn doc_entry_for_name<'a>(item: &'a Item<Surface>, name: &str) -> Option<DocEntry<'a>> {
    documented_entries_for_item(item)
        .into_iter()
        .find(|entry| entry.name() == name)
}

/// Build the [`DocEntry`] list for a module body, in source order.
///
/// The match is **exhaustive with no wildcard**: a new [`Item`]
/// variant forces a deliberate documentable / non-documentable
/// decision here. `equiv` is the sole non-documentable module item
/// (a test-claim with no public-API surface). Anonymous `labels` have no
/// declaration name of their own, but expose their generated nominal entries.
pub fn documented_items_module(module: &Module<Surface>) -> Vec<DocEntry<'_>> {
    module
        .items
        .iter()
        .flat_map(documented_entries_for_item)
        .collect()
}

/// Exact public declarations, retaining their original source owners and order.
pub fn exported_documented_items_module(module: &Module<Surface>) -> Vec<DocEntry<'_>> {
    documented_items_module(module)
        .into_iter()
        .filter(DocEntry::is_exported)
        .collect()
}

/// The package file carries no documentable declarations — its only
/// content is the `bridge { … }` glob list, which selects modules
/// rather than naming declarations. The host contract surface is
/// derived by scanning the bridged modules, whose `host` and `pub`
/// items are documented through [`documented_items_module`].
pub fn documented_items_package_file(_package_file: &PackageFile<Surface>) -> Vec<DocEntry<'_>> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_projection_selects_each_declaration_kind_by_exact_visibility() {
        let declarations = [
            "fn value(x: .) -> . { x }",
            "type Alias = .;",
            "literal text = \"x\";",
            "newtype Box : . { constructor make; projector read; };",
            "labels Row = { field: . };",
            "labels { field: . };",
            "type {forward} = {field};",
            "op _ + _ { impl add; };",
            "varop [* *] { foldl push empty; };",
            "elab choose : . -> . { impl choose_impl; };",
        ];
        for declaration in declarations {
            for visibility in ["", "pub(api) ", "pub "] {
                let module =
                    crate::pass::parser::parse(&format!("module api; {visibility}{declaration}"))
                        .unwrap();
                let local = documented_items_module(&module);
                assert!(!local.is_empty(), "{declaration}");
                if declaration.starts_with("varop ") {
                    assert_eq!(local[0].kind(), "varop");
                    assert_eq!(local[0].name(), "varop [* *]");
                }
                let public = exported_documented_items_module(&module);
                assert_eq!(
                    public.len(),
                    if visibility == "pub " { local.len() } else { 0 },
                    "{visibility}{declaration}"
                );
            }
        }
        let hosts = crate::pass::parser::parse(
            "module api; host type Text role(str); host fn emit(value: Text) -> .;",
        )
        .unwrap();
        assert_eq!(exported_documented_items_module(&hosts).len(), 2);
    }

    #[test]
    fn anchors_encode_complete_module_and_operator_identities() {
        let identities = ["a/b", "a.b", "a_b", "a-b", "a_2fb", "a b", "é", "_c3_a9"];
        let encoded = identities.map(encode_anchor_identity);
        assert_eq!(
            encoded
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            identities.len()
        );
        assert_eq!(encode_anchor_identity("pkg/left"), "pkg_2fleft");
        assert_eq!(encode_anchor_identity("é"), "_c3_a9");
        let module = crate::pass::parser::parse(
            "module pkg/math; op - __ { impl negate; }; op _ - __ { impl subtract; };",
        )
        .unwrap();
        let entries = documented_items_module(&module);
        assert_eq!(
            entries.iter().map(DocEntry::name).collect::<Vec<_>>(),
            ["op - __", "op _ - __"]
        );
        let anchors = entries
            .iter()
            .map(|entry| entry.anchor("pkg/math"))
            .collect::<Vec<_>>();
        assert_eq!(
            anchors,
            [
                "op-pkg_2fmath-op_20_2d_20_5f_5f",
                "op-pkg_2fmath-op_20_5f_20_2d_20_5f_5f"
            ]
        );
        assert_ne!(entries[0].anchor("pkg/math"), entries[0].anchor("pkg.math"));
    }

    #[test]
    fn ordinary_label_nominals_keep_the_written_owner_and_skip_reuse() {
        let module = crate::pass::parser::parse(
            "module pkg; labels { field: . }; labels Row = { field: _, other: . };",
        )
        .unwrap();
        let entries = documented_items_module(&module);
        let names = entries.iter().map(DocEntry::name).collect::<Vec<_>>();
        assert_eq!(names, ["Field", "Row", "Other"]);
        for (name, owner) in [
            ("Field", "labels { field: . };"),
            ("Other", "labels Row = { field: _, other: . };"),
        ] {
            let entry = entries.iter().find(|entry| entry.name() == name).unwrap();
            assert_eq!(entry.kind(), "newtype");
            assert_eq!(entry.signature(), owner);
            assert_eq!(entry.source(), owner);
            assert_eq!(entry.ty(), None);
        }
    }

    #[test]
    fn forwarded_label_docs_keep_a_distinct_nonminting_identity() {
        let module = crate::pass::parser::parse(concat!(
            "module pkg; fn field() -> . { () }\n",
            "/// The forwarded label.\n",
            "pub type {field} = {original};\n",
        ))
        .unwrap();
        let entries = documented_items_module(&module);
        assert_eq!(
            entries.iter().map(DocEntry::name).collect::<Vec<_>>(),
            ["field", "{field}"]
        );
        let forward = &entries[1];
        assert_eq!(forward.kind(), "label");
        assert_eq!(forward.signature(), "pub type {field} = {original};");
        assert_eq!(forward.source(), "pub type {field} = {original};");
        assert!(forward.doc().is_some());
        assert!(forward.is_exported());
        assert_eq!(forward.type_name_span(), None);
        assert_eq!(forward.ty(), None);
        assert_eq!(forward.anchor("pkg"), "label-pkg-_7bfield_7d");
        assert_ne!(forward.anchor("pkg"), entries[0].anchor("pkg"));
        assert!(doc_entry_for_name(&module.items[1], "field").is_none());
        assert!(doc_entry_for_name(&module.items[1], "Field").is_none());
        assert!(doc_entry_for_name(&module.items[1], "{field}").is_some());
    }

    #[test]
    fn rec_group_documents_every_module_member() {
        let module = crate::pass::parser::parse(
            "module pkg/main; \
             rec(loop) { \
               fn local(value: .) -> . { rec exported(value) }; \
               pub fn exported(value: .) -> . { rec local(value) } \
             }",
        )
        .expect("fixture parses");
        let names = documented_items_module(&module)
            .into_iter()
            .map(|entry| entry.name().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["local", "exported"]);
    }
}
