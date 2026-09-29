use super::RetypeTarget;
use crate::ast::{
    Item, LabelEntry, LabelForward, Labels, Meta, Module, ModulePath, Surface, TypeAlias,
    TypeRecMember, mint_label_newtype_name,
};
use crate::span::Span;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub(super) struct PendingLabelForward {
    pub declaration: LabelForward<Surface>,
    pub nominal_name: String,
    pub to: ModulePath,
    pub retype_span: Span,
}

pub(super) struct DecomposedLabels {
    pub members: Vec<TypeRecMember<Surface>>,
    pub forwards: Vec<PendingLabelForward>,
}

pub(super) fn affected_owners(
    module: &Module<Surface>,
    selected_labels: &BTreeSet<String>,
) -> Vec<bool> {
    let mut earlier: BTreeMap<&str, &LabelEntry<Surface>> = BTreeMap::new();
    let mut affected = Vec::new();
    for item in &module.items {
        crate::pass::resolve::for_each_item_declaration(item, |declaration| {
            use crate::pass::resolve::TopLevelDeclaration;
            let labels = match declaration {
                TopLevelDeclaration::Item(Item::Labels(labels, _))
                | TopLevelDeclaration::TypeRecMember(TypeRecMember::Labels(labels, _)) => labels,
                _ => return,
            };
            let mut selected = false;
            for entry in labels.arms_in_source_order().flatten() {
                if entry.is_reuse_marker() {
                    if labels.type_alias_name.is_some()
                        && entry.existential_params.is_empty()
                        && labels.unbound_entry_universal(entry).is_none()
                        && earlier.get(entry.name.as_str()).is_some_and(|origin| {
                            origin.universal_header_matches(entry)
                                && selected_labels.contains(&origin.name)
                        })
                    {
                        selected = true;
                    }
                } else {
                    selected |= selected_labels.contains(&entry.name);
                    // Retain the first explicit occurrence. Original-source
                    // validation still rejects every later declaration.
                    earlier.entry(&entry.name).or_insert(entry);
                }
            }
            affected.push(selected);
        });
    }
    affected
}

pub(super) fn decompose_owner(
    mut owner: Labels<Surface>,
    targets: &BTreeMap<String, &RetypeTarget>,
    reexports: &mut BTreeMap<String, TypeAlias<Surface>>,
) -> DecomposedLabels {
    let named_alias = owner.type_alias_name.as_ref().map(|name| TypeAlias {
        vis: owner.vis.clone(),
        name: name.clone(),
        name_span: owner
            .type_alias_span
            .expect("named labels carry a name span"),
        type_params: owner.type_alias_params.clone(),
        body: crate::pass::label_elab::named_label_alias_body(
            owner
                .type_alias_arms
                .as_deref()
                .expect("named labels carry arms"),
            owner.meta.span,
            |entry| vec![mint_label_newtype_name(&entry.name)],
        ),
        meta: Meta::new(owner.meta.span),
        editable_span: None,
        doc: owner.doc.clone(),
    });
    let entries = match owner.type_alias_arms.take() {
        Some(arms) => arms.into_iter().flat_map(|arm| arm.entries).collect(),
        None => std::mem::take(&mut owner.entries),
    };
    let mut members = Vec::new();
    let mut forwards = Vec::new();
    for entry in entries {
        if entry.is_reuse_marker() {
            continue;
        }
        let nominal_name = mint_label_newtype_name(&entry.name);
        if let Some(target) = targets.get(&nominal_name) {
            if let Some(mut alias) = reexports.remove(&nominal_name) {
                alias.meta.leading_trivia.clear();
                members.push(TypeRecMember::TypeAlias(alias));
            }
            forwards.push(PendingLabelForward {
                declaration: LabelForward {
                    vis: owner.vis.clone(),
                    name: entry.name.clone(),
                    name_span: entry.name_span,
                    target: entry.name,
                    target_span: entry.name_span,
                    body_trivia: entry.meta.leading_trivia,
                    meta: Meta::new(entry.meta.span),
                    editable_span: None,
                    doc: owner.doc.clone(),
                },
                nominal_name,
                to: target.to.clone(),
                retype_span: target.span,
            });
        } else {
            members.push(TypeRecMember::Labels(
                Labels {
                    vis: owner.vis.clone(),
                    rec_span: None,
                    type_alias_name: None,
                    type_alias_span: None,
                    type_alias_params: Vec::new(),
                    type_alias_arms: None,
                    entries: vec![entry],
                    meta: Meta::new(owner.meta.span),
                    editable_span: None,
                    doc: owner.doc.clone(),
                },
                (),
            ));
        }
    }
    if let Some(alias) = named_alias {
        members.push(TypeRecMember::TypeAlias(alias));
    }
    if let Some(first) = members.first_mut() {
        first.meta_mut().leading_trivia = owner.meta.leading_trivia;
    } else if let Some(first) = forwards.first_mut() {
        first.declaration.meta.leading_trivia = owner.meta.leading_trivia;
    }
    DecomposedLabels { members, forwards }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Module<Surface> {
        crate::pass::parser::parse_module_file(source)
            .expect("surface module")
            .module
    }

    #[test]
    fn affected_owners_follow_only_earlier_explicit_local_origins() {
        let module = parse(
            "module source; \
             labels Before = { foo: _ }; \
             type {forwarded} = {foo}; \
             labels Via_forward = { forwarded: _ }; \
             pub labels { foo: . }; \
             pub labels Later[A] = { foo: _, box[A]: A }; \
             labels Still[A] = { box[A]: _ };",
        );
        assert_eq!(
            affected_owners(&module, &["foo".to_owned()].into_iter().collect()),
            [false, false, true, true, false],
        );
    }

    #[test]
    fn affected_owners_preserve_header_and_recursive_member_boundaries() {
        let module = parse(
            "module source; \
             labels { box[*F][A]<X>: F(A) & X }; \
             labels Wrong[F][A] = { box[F][A]: _ }; \
             rec { \
               labels Later[*G][B] = { box[*G][B]: _ } | { link[*G][B]: Next(G, B) }; \
               labels Next[*H][C] = { box[*H][C]: _ } | { back[*H][C]: Later(H, C) }; \
             }",
        );
        assert_eq!(
            affected_owners(&module, &["box".to_owned()].into_iter().collect()),
            [true, false, true, true],
        );
    }

    #[test]
    fn decomposition_keeps_unselected_origins_and_named_arm_arguments() {
        let module = parse(
            "module source; \
             /// A family.\n\
             pub labels Choice[A] = { value[A]<X>: A & X } | { value[A]: _, empty: . };",
        );
        let Item::Labels(owner, ()) = &module.items[0] else {
            panic!("labels owner");
        };
        let target = RetypeTarget {
            from: module.path.clone(),
            newtype_names: ["Value".to_owned()].into_iter().collect(),
            to: parse("module target;").path,
            span: module.meta.span,
        };
        let entry = &owner.type_alias_arms.as_ref().unwrap()[0].entries[0];
        let alias = super::super::reexport_type_alias(
            "Value",
            &entry.type_params,
            owner.vis.clone(),
            "target",
            entry.meta.span,
        );
        let mut reexports = [("Value".to_owned(), alias)].into_iter().collect();
        let targets = [("Value".to_owned(), &target)].into_iter().collect();
        let result = decompose_owner(owner.clone(), &targets, &mut reexports);
        assert!(reexports.is_empty());
        assert_eq!(result.forwards.len(), 1);
        assert_eq!(result.forwards[0].declaration.name, "value");
        assert_eq!(result.forwards[0].declaration.doc, owner.doc);
        assert_eq!(result.members.len(), 3);
        let TypeRecMember::TypeAlias(selected) = &result.members[0] else {
            panic!("selected nominal alias");
        };
        assert_eq!(
            selected.type_params.len(),
            1,
            "existentials stay on the target nominal"
        );
        let TypeRecMember::Labels(retained, ()) = &result.members[1] else {
            panic!("unselected explicit origin");
        };
        assert_eq!(retained.entries[0].name, "empty");
        assert!(!retained.entries[0].is_reuse_marker());
        let TypeRecMember::TypeAlias(named) = &result.members[2] else {
            panic!("named sum alias");
        };
        assert_eq!(named.name, "Choice");
        assert_eq!(named.type_params, owner.type_alias_params);
        assert_eq!(named.doc, owner.doc);
        let crate::ast::Type::Sum { left, right, .. } = &named.body else {
            panic!("two source arms remain a sum");
        };
        let crate::ast::Type::Product {
            left: reused,
            right: empty,
            ..
        } = right.as_ref()
        else {
            panic!("the second arm retains its product");
        };
        for value in [left.as_ref(), reused.as_ref()] {
            let crate::ast::Type::Path { segments, args, .. } = value else {
                panic!("ordinary nominal occurrence");
            };
            assert_eq!(segments, &["Value"]);
            let [crate::ast::Type::Path { segments, args, .. }] = args.as_slice() else {
                panic!("one written universal argument");
            };
            assert_eq!(segments, &["A"]);
            assert!(args.is_empty());
        }
        let crate::ast::Type::Path { segments, args, .. } = empty.as_ref() else {
            panic!("unselected nominal occurrence");
        };
        assert_eq!(segments, &["Empty"]);
        assert!(args.is_empty());
    }
}
