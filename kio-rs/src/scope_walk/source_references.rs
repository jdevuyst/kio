#[cfg(feature = "lsp")]
use super::{
    CandidateKind, CandidateSink, NamedOrigin, contains, imports_scope, item_names, item_scope,
};
use super::{CandidateOrigin, Namespace, in_scope_origin, named_origin};
use crate::ast::{ImportKind, Item, Module, TypeRecMember};
use crate::span::Span;
use crate::tokens::{SourceNameKind, SourceReference};
#[cfg(feature = "lsp")]
use std::collections::HashMap;

/// The written declaration selected by a source occurrence. Forwarded labels
/// retain this binding independently of their terminal nominal identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBinding {
    pub module: String,
    pub name: String,
    /// The exact written import edge, if this occurrence selected one.
    pub import_span: Option<Span>,
}

/// Resolve a parsed source occurrence in an already validated module snapshot.
/// Local value binders are excluded: they do not denote a module declaration.
pub fn source_binding(module: &Module, reference: &SourceReference) -> Option<SourceBinding> {
    let local = |name: &str| SourceBinding {
        module: module.path.segments.join("/"),
        name: name.to_owned(),
        import_span: None,
    };
    if reference.declaration {
        return Some(local(&reference.path));
    }
    if let Some((qualifier, name)) = reference.path.split_once('.') {
        if name.contains('.') {
            return None;
        }
        return module.imports.iter().find_map(|import| match &import.kind {
            ImportKind::Qualified { path, alias } if alias == qualifier => Some(SourceBinding {
                module: path.segments.join("/"),
                name: name.to_owned(),
                import_span: Some(import.span),
            }),
            _ => None,
        });
    }
    let name = &reference.path;
    if matches!(reference.kind, SourceNameKind::Value | SourceNameKind::Type) {
        let origin = if reference.kind == SourceNameKind::Type {
            named_origin(module, reference.span.start, name, Namespace::Type)
        } else {
            in_scope_origin(module, reference.span.start, name)
        };
        return match origin? {
            CandidateOrigin::Declaration(_) => Some(local(name)),
            CandidateOrigin::Import { provider, span } => Some(SourceBinding {
                module: provider,
                name: name.clone(),
                import_span: Some(span),
            }),
            _ => None,
        };
    }
    if module.items.iter().any(|item| match (reference.kind, item) {
        (SourceNameKind::Elaborator, Item::Elaborator(def, _)) => def.name == *name,
        (SourceNameKind::Label, Item::LabelForward(def, _)) => def.name == *name,
        (SourceNameKind::Label, Item::Labels(labels, _)) => labels.entries.iter().any(|entry| {
            entry.name == *name && !entry.is_reuse_marker()
        }),
        (SourceNameKind::Label, Item::TypeRecGroup(group)) => group.members.iter().any(|member| {
            matches!(member, TypeRecMember::Labels(labels, _) if labels.entries.iter().any(|entry| entry.name == *name && !entry.is_reuse_marker()))
        }),
        _ => false,
    }) {
        return Some(local(name));
    }
    module.imports.iter().find_map(|import| {
        let ImportKind::Selective { from, items } = &import.kind else {
            return None;
        };
        items.iter().find_map(|item| {
            let selected = match reference.kind {
                SourceNameKind::Label => item.as_label(),
                SourceNameKind::Elaborator => item.as_name_with_span(),
                SourceNameKind::Value | SourceNameKind::Type | SourceNameKind::Equiv => None,
            }?;
            (selected.0 == name).then(|| SourceBinding {
                module: from.segments.join("/"),
                name: name.clone(),
                import_span: Some(selected.1),
            })
        })
    })
}

#[cfg(feature = "lsp")]
struct IndexedOrigin {
    item: usize,
    eligible_after: u32,
    origin: CandidateOrigin,
}

#[cfg(feature = "lsp")]
type OriginMap = HashMap<String, Vec<IndexedOrigin>>;

/// Module-level bindings are collected once; an occurrence still uses the
/// shared lexical walk inside its containing item.
#[cfg(feature = "lsp")]
pub(crate) struct SourceBindingIndex<'a> {
    module: &'a Module,
    declarations: [OriginMap; 4],
    imports: [OriginMap; 4],
}

#[cfg(feature = "lsp")]
struct IndexSink<'a> {
    origins: &'a mut OriginMap,
    item: usize,
    eligible_after: u32,
}

#[cfg(feature = "lsp")]
impl CandidateSink for IndexSink<'_> {
    fn push(&mut self, name: &str, _kind: CandidateKind, origin: CandidateOrigin) {
        self.origins
            .entry(name.to_owned())
            .or_default()
            .push(IndexedOrigin {
                item: self.item,
                eligible_after: self.eligible_after,
                origin,
            });
    }

    fn has_placeholder(&self) -> bool {
        false
    }
}

#[cfg(feature = "lsp")]
impl<'a> SourceBindingIndex<'a> {
    pub(crate) fn new(module: &'a Module) -> Self {
        let mut index = Self {
            module,
            declarations: std::array::from_fn(|_| HashMap::new()),
            imports: std::array::from_fn(|_| HashMap::new()),
        };
        for (item_index, item) in module.items.iter().enumerate() {
            for (slot, ns) in [Namespace::Value, Namespace::Type].into_iter().enumerate() {
                item_names(
                    item,
                    ns,
                    &mut IndexSink {
                        origins: &mut index.declarations[slot],
                        item: item_index,
                        eligible_after: item.span().end,
                    },
                );
            }
            let mut label = |name: &str, span| {
                IndexSink {
                    origins: &mut index.declarations[2],
                    item: item_index,
                    eligible_after: item.span().end,
                }
                .push(
                    name,
                    CandidateKind::Type,
                    CandidateOrigin::Declaration(span),
                );
            };
            match item {
                Item::LabelForward(def, _) => label(&def.name, def.name_span),
                Item::Labels(labels, _) => {
                    for entry in &labels.entries {
                        if !entry.is_reuse_marker() {
                            label(&entry.name, entry.name_span);
                        }
                    }
                }
                Item::TypeRecGroup(group) => {
                    for member in &group.members {
                        if let TypeRecMember::Labels(labels, _) = member {
                            for entry in &labels.entries {
                                if !entry.is_reuse_marker() {
                                    label(&entry.name, entry.name_span);
                                }
                            }
                        }
                    }
                }
                Item::Elaborator(def, _) => {
                    IndexSink {
                        origins: &mut index.declarations[3],
                        item: item_index,
                        eligible_after: item.span().end,
                    }
                    .push(
                        &def.name,
                        CandidateKind::Function,
                        CandidateOrigin::Declaration(def.name_span),
                    );
                }
                _ => {}
            }
        }
        for import in &module.imports {
            for (slot, ns) in [Namespace::Value, Namespace::Type].into_iter().enumerate() {
                imports_scope(
                    std::slice::from_ref(import),
                    u32::MAX,
                    ns,
                    &mut IndexSink {
                        origins: &mut index.imports[slot],
                        item: 0,
                        eligible_after: import.span.end,
                    },
                );
            }
            if let ImportKind::Selective { from, items } = &import.kind {
                for selected in items {
                    for (slot, name) in
                        [(2, selected.as_label()), (3, selected.as_name_with_span())]
                    {
                        if let Some((name, span)) = name {
                            IndexSink {
                                origins: &mut index.imports[slot],
                                item: 0,
                                eligible_after: import.span.end,
                            }
                            .push(
                                name,
                                CandidateKind::Function,
                                CandidateOrigin::Import {
                                    provider: from.segments.join("/"),
                                    span,
                                },
                            );
                        }
                    }
                }
            }
        }
        index
    }

    fn active_item(&self, offset: u32) -> Option<usize> {
        let index = self
            .module
            .items
            .partition_point(|item| item.span().end < offset);
        self.module
            .items
            .get(index)
            .filter(|item| contains(item.span(), offset))
            .map(|_| index)
    }

    fn module_origin(
        &self,
        slot: usize,
        name: &str,
        offset: u32,
        active: Option<usize>,
    ) -> Option<CandidateOrigin> {
        self.declarations[slot]
            .get(name)
            .and_then(|entries| {
                entries.iter().find(|entry| {
                    slot >= 2
                        || active
                            .map_or(entry.eligible_after <= offset, |active| entry.item < active)
                })
            })
            .or_else(|| {
                self.imports[slot]
                    .get(name)
                    .and_then(|entries| entries.iter().find(|entry| entry.eligible_after <= offset))
            })
            .map(|entry| entry.origin.clone())
    }

    pub(crate) fn origin(&self, reference: &SourceReference) -> Option<CandidateOrigin> {
        if reference.declaration {
            return Some(CandidateOrigin::Declaration(reference.span));
        }
        let offset = reference.span.start;
        if let Some((qualifier, _)) = reference.path.split_once('.') {
            return self.imports[0]
                .get(qualifier)
                .and_then(|entries| {
                    entries.iter().find(|entry| {
                        entry.eligible_after <= offset
                            && matches!(entry.origin, CandidateOrigin::QualifiedImport(_))
                    })
                })
                .map(|entry| entry.origin.clone());
        }
        let slot = match reference.kind {
            SourceNameKind::Value => 0,
            SourceNameKind::Type => 1,
            SourceNameKind::Label => 2,
            SourceNameKind::Elaborator => 3,
            SourceNameKind::Equiv => return None,
        };
        let active = self.active_item(offset);
        if slot < 2
            && let Some(item) = active
        {
            let mut out = NamedOrigin {
                name: &reference.path,
                origin: None,
                has_placeholder: false,
            };
            item_scope(
                &self.module.items[item],
                offset,
                if slot == 0 {
                    Namespace::Value
                } else {
                    Namespace::Type
                },
                &mut out,
            );
            if out.origin.is_some() {
                return out.origin;
            }
        }
        self.module_origin(slot, &reference.path, offset, active)
    }

    /// Project a type name already chosen by lexical role selection, whose
    /// module scope is independent of a same-spelled local type parameter.
    pub(crate) fn selected_module_type_origin(
        &self,
        name: &str,
        offset: u32,
    ) -> Option<CandidateOrigin> {
        self.module_origin(1, name, offset, self.active_item(offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "lsp")]
    #[test]
    fn source_dependency_index_keeps_type_import_edges_and_binder_boundaries() {
        let source = "module main; import provider(Foo); import provider as q;
            type Plain = Foo;
            type Qualified = q.Foo;
            fn generic[Foo](value: Foo) -> Foo { value }
            type Structural = ([Foo] Foo -> Foo) & Foo;
            type Hidden = .;
            pure fn helper(value: .) -> . { value }
            elab build : [Hidden] Hidden -> Hidden { captures Hidden; impl helper; };";
        let module = crate::pass::parser::parse(source).unwrap();
        let references = crate::tokens::source_dependencies(source, &module).unwrap();
        let index = SourceBindingIndex::new(&module);
        let types = references
            .iter()
            .filter(|reference| reference.kind == SourceNameKind::Type)
            .collect::<Vec<_>>();
        let origins = types
            .iter()
            .map(|reference| index.origin(reference).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(origins.len(), 10);
        let ImportKind::Selective { items, .. } = &module.imports[0].kind else {
            panic!("selective import")
        };
        let import = CandidateOrigin::Import {
            provider: "provider".into(),
            span: items[0].as_name_with_span().unwrap().1,
        };
        assert_eq!(origins[0], import);
        assert_eq!(
            origins[1],
            CandidateOrigin::QualifiedImport(module.imports[1].span)
        );
        for position in [2, 3, 4, 5, 7, 8] {
            assert!(
                matches!(origins[position], CandidateOrigin::Local(_)),
                "{position}: {:?}",
                origins[position]
            );
        }
        assert_eq!(origins[6], import);
        let Item::TypeAlias(hidden) = &module.items[4] else {
            panic!("Hidden declaration")
        };
        assert_eq!(origins[9], CandidateOrigin::Declaration(hidden.name_span));
        assert_eq!(
            index.selected_module_type_origin("Foo", types[2].span.start),
            Some(import)
        );
        assert_eq!(
            index.selected_module_type_origin("Hidden", types[7].span.start),
            Some(CandidateOrigin::Declaration(hidden.name_span))
        );
        for reference in types
            .iter()
            .filter(|reference| !reference.path.contains('.'))
        {
            let legacy = super::super::type_candidates(&module, reference.span.start)
                .into_iter()
                .find(|candidate| candidate.label == reference.path)
                .map(|candidate| candidate.origin);
            assert_eq!(index.origin(reference), legacy, "{reference:?}");
        }
    }

    #[cfg(feature = "lsp")]
    #[test]
    fn source_dependency_index_preserves_recursive_type_and_value_namespaces() {
        let source = "module main; import provider(marker, Foo);
            rec newtype Tree[A] : A & Tree(A) { constructor mk; projector un; };
            rec { type Left = Right; newtype Right[Left] : Left { constructor right; projector unright; }; }
            fn bare() -> . { marker }
            fn parameter(marker: .) -> . { marker }
            fn nested() -> . { let marker = marker; .(marker: .) { marker }(marker) }
            fn placeholders() -> . { .x. { x1 } }";
        let module = crate::pass::parser::parse(source).unwrap();
        let references = crate::tokens::source_dependencies(source, &module).unwrap();
        let index = SourceBindingIndex::new(&module);
        let mut declarations = 0;
        let mut locals = 0;
        let mut placeholders = 0;
        let mut imports = 0;
        for reference in &references {
            let candidates = match reference.kind {
                SourceNameKind::Type => {
                    super::super::type_candidates(&module, reference.span.start)
                }
                SourceNameKind::Value => {
                    super::super::in_scope_candidates(&module, reference.span.start)
                }
                _ => continue,
            };
            let expected = candidates
                .into_iter()
                .find(|candidate| candidate.label == reference.path)
                .map(|candidate| candidate.origin);
            let actual = index.origin(reference);
            assert_eq!(actual, expected, "{reference:?}");
            match actual.unwrap() {
                CandidateOrigin::Declaration(_) => declarations += 1,
                CandidateOrigin::Local(_) => locals += 1,
                CandidateOrigin::Placeholder(_) | CandidateOrigin::NumberedBinding(_) => {
                    placeholders += 1
                }
                CandidateOrigin::Import { .. } => imports += 1,
                other => panic!("unexpected origin {other:?}"),
            }
        }
        assert_eq!((declarations, locals, placeholders, imports), (2, 6, 1, 2));
    }

    #[cfg(feature = "lsp")]
    #[test]
    fn source_dependency_index_preserves_label_elaborator_and_builtin_origins() {
        let source = "module main;
            import provider({tag}, expand); import provider as q;
            import __intrinsics__; import __intrinsics__;
            import __comptime__; import __comptime__;
            labels { local: . }; type {forwarded} = {q.tag};
            pure fn helper(value: .) -> . { value }
            elab build : . -> . { impl helper; };
            fn run() -> . { let _ = {tag = ()}; let _ = {local = ()}; let _ = {forwarded = ()};
                let _ = expand!(()); let _ = build!(()); let _ = __pair__((), ()); __term_unit__(()) }";
        let module = crate::pass::parser::parse(source).unwrap();
        let references = crate::tokens::source_dependencies(source, &module).unwrap();
        let index = SourceBindingIndex::new(&module);
        let mut special = 0;
        for reference in references.iter().filter(|reference| {
            matches!(
                reference.kind,
                SourceNameKind::Label | SourceNameKind::Elaborator
            )
        }) {
            let old = source_binding(&module, reference).unwrap();
            let new = index.origin(reference).unwrap();
            match new {
                CandidateOrigin::Declaration(_) => {
                    assert_eq!((old.module.as_str(), old.import_span), ("main", None))
                }
                CandidateOrigin::Import { provider, span } => {
                    assert_eq!((old.module, old.import_span), (provider, Some(span)))
                }
                CandidateOrigin::QualifiedImport(span) => assert_eq!(old.import_span, Some(span)),
                other => panic!("unexpected special origin {other:?}"),
            }
            special += 1;
        }
        assert_eq!(special, 9);
        for (name, class) in [
            ("__pair__", "__intrinsics__"),
            ("__term_unit__", "__comptime__"),
        ] {
            let reference = references
                .iter()
                .find(|reference| reference.path == name)
                .unwrap();
            assert_eq!(
                index.origin(reference),
                Some(CandidateOrigin::Builtin(class))
            );
        }
    }

    #[cfg(feature = "lsp")]
    #[test]
    fn source_dependency_index_keeps_rec_drivers_outside_member_binders() {
        let source = "module main; import provider(driver); import provider as q;
            rec(driver) fn direct(driver: .) -> . { driver }
            rec(q.driver) fn qualified(value: .) -> . { value }";
        let module = crate::pass::parser::parse(source).unwrap();
        let legacy = crate::tokens::source_references(source, &module).unwrap();
        assert_eq!(
            legacy
                .iter()
                .map(|reference| reference.path.as_str())
                .collect::<Vec<_>>(),
            ["driver", "value"]
        );
        let references = crate::tokens::source_dependencies(source, &module).unwrap();
        assert_eq!(
            references
                .iter()
                .map(|reference| reference.path.as_str())
                .collect::<Vec<_>>(),
            ["driver", "driver", "q.driver", "value"]
        );
        let index = SourceBindingIndex::new(&module);
        let ImportKind::Selective { items, .. } = &module.imports[0].kind else {
            panic!("driver import")
        };
        assert_eq!(
            index.origin(&references[0]),
            Some(CandidateOrigin::Import {
                provider: "provider".into(),
                span: items[0].as_name_with_span().unwrap().1,
            })
        );
        assert!(matches!(
            index.origin(&references[1]),
            Some(CandidateOrigin::Local(_))
        ));
        assert_eq!(
            index.origin(&references[2]),
            Some(CandidateOrigin::QualifiedImport(module.imports[1].span))
        );
        assert!(matches!(
            index.origin(&references[3]),
            Some(CandidateOrigin::Local(_))
        ));
    }

    #[cfg(feature = "lsp")]
    #[test]
    fn source_dependency_index_accounts_for_remaining_per_item_walk_cost() {
        use crate::ast::{Expr, Meta, PathSegment};

        const PREFIX: &str = "module main; import provider(marker); fn run() -> . { ";
        const STATEMENT: &str = "let _ = marker; ";

        for count in [32, 128, 512] {
            let source = format!("{PREFIX}{} () }}", STATEMENT.repeat(count));
            // Keep this a scope-walk probe: parsing the 512-statement chain
            // exhausts the default test stack before either lookup runs.
            // The shallow shell preserves every surrounding source span.
            let shell = format!("{PREFIX}{} () }}", " ".repeat(STATEMENT.len() * count));
            eprintln!("source dependency scope: uses={count} begin shallow shell parse");
            let mut module = crate::pass::parser::parse(&shell).unwrap();
            let Item::FnDef(function) = &mut module.items[0] else {
                panic!("fixture has one function");
            };
            let mut body = function.body.clone();
            let end = body.span().end;
            for statement in (0..count).rev() {
                let start = (PREFIX.len() + statement * STATEMENT.len()) as u32;
                let name_span = Span::new(start + 4, start + 5);
                let value_span = Span::new(start + 8, start + 14);
                assert_eq!(
                    &source[name_span.start as usize..name_span.end as usize],
                    "_"
                );
                assert_eq!(
                    &source[value_span.start as usize..value_span.end as usize],
                    "marker"
                );
                body = Expr::Let {
                    occurrence: Default::default(),
                    name: "_".to_owned(),
                    name_span,
                    ty: None,
                    pattern: None,
                    value: Box::new(Expr::Path {
                        occurrence: Default::default(),
                        segments: vec![PathSegment::new("marker", value_span)],
                        meta: Meta::new(value_span),
                        ext: (),
                    }),
                    body: Box::new(body),
                    meta: Meta::new(Span::new(start, end)),
                };
            }
            function.body = body;
            if count <= 128 {
                eprintln!(
                    "source dependency scope: uses={count} begin full parser equality control"
                );
                let parsed = crate::pass::parser::parse(&source).unwrap();
                assert_eq!(module, parsed);
                // PathSegment equality intentionally omits its span; the
                // complete debug representation also compares those spans.
                assert_eq!(format!("{module:?}"), format!("{parsed:?}"));
            }
            eprintln!(
                "source dependency scope: uses={count} fixture ready; begin legacy token walk"
            );
            let legacy_references = crate::tokens::source_references(&source, &module).unwrap();
            assert_eq!(legacy_references.len(), count);
            eprintln!(
                "source dependency scope: uses={count} legacy tokens done; begin dependency token walk"
            );
            let references = crate::tokens::source_dependencies(&source, &module).unwrap();
            assert_eq!(references.len(), count);
            assert_eq!(references, legacy_references);
            eprintln!(
                "source dependency scope: uses={count} dependency tokens done; begin index construction"
            );
            let index = SourceBindingIndex::new(&module);
            let ImportKind::Selective { items, .. } = &module.imports[0].kind else {
                panic!("fixture has one selective import");
            };
            let expected_origin = CandidateOrigin::Import {
                provider: "provider".to_owned(),
                span: items[0].as_name_with_span().unwrap().1,
            };
            eprintln!(
                "source dependency scope: uses={count} index built; begin indexed query batch"
            );
            super::super::EXPR_SCOPE_VISITS.with(|visits| visits.set(0));
            let started = std::time::Instant::now();
            for reference in &references {
                assert_eq!(index.origin(reference), Some(expected_origin.clone()));
            }
            let elapsed = started.elapsed();
            let indexed = super::super::EXPR_SCOPE_VISITS.with(|visits| visits.replace(0));
            eprintln!(
                "source dependency scope: uses={count} indexed queries done; begin legacy query batch"
            );
            let started = std::time::Instant::now();
            for reference in &references {
                assert_eq!(
                    super::super::in_scope_origin(&module, reference.span.start, &reference.path),
                    Some(expected_origin.clone())
                );
            }
            let legacy_elapsed = started.elapsed();
            let legacy = super::super::EXPR_SCOPE_VISITS.with(|visits| visits.replace(0));
            assert_eq!(indexed, legacy);
            assert!(indexed <= count * (count + 3));
            eprintln!(
                "source dependency scope: uses={count} expression visits={indexed} indexed elapsed={elapsed:?} legacy elapsed={legacy_elapsed:?}; containing-item traversal retained, no completion vectors"
            );
            eprintln!("source dependency scope: uses={count} queries done; begin teardown");
            drop(index);
            drop(references);
            drop(legacy_references);
            drop(module);
            eprintln!("source dependency scope: uses={count} teardown done");
        }
    }

    #[test]
    fn targeted_value_lookup_preserves_completion_shadowing() {
        let source = concat!(
            "module main; import provider(marker);\n",
            "fn bare() -> . { marker }\n",
            "fn parameter(marker: .) -> . { marker }\n",
            "fn nested() -> . { let marker = (); .(marker: .) { marker }(marker) }\n",
            "fn initializer() -> . { let marker = marker; marker }\n",
        );
        let module = crate::pass::parser::parse(source).unwrap();
        let references = crate::tokens::source_references(source, &module).unwrap();
        let mut imported = 0;
        let mut local = 0;
        for reference in references
            .iter()
            .filter(|reference| reference.path == "marker")
        {
            let complete = super::super::in_scope_candidates(&module, reference.span.start)
                .into_iter()
                .find(|candidate| candidate.label == "marker")
                .unwrap()
                .origin;
            assert_eq!(
                in_scope_origin(&module, reference.span.start, "marker"),
                Some(complete.clone())
            );
            if matches!(complete, CandidateOrigin::Import { .. }) {
                imported += 1;
                assert_eq!(
                    source_binding(&module, reference).unwrap().module,
                    "provider"
                );
            } else {
                local += 1;
                assert!(source_binding(&module, reference).is_none());
            }
        }
        assert_eq!((imported, local), (2, 4));
    }
}
