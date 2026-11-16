use crate::ast::{
    Item, Labels, Meta, Module, Surface, TypeAlias, TypeRecMember, mint_label_newtype_name,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct DecomposedLabels {
    pub members: Vec<TypeRecMember<Surface>>,
}

pub(super) fn affected_owners(
    module: &Module<Surface>,
    selected_labels: &BTreeSet<String>,
) -> Vec<bool> {
    let mut affected = Vec::new();
    for item in &module.items {
        crate::pass::resolve::for_each_item_declaration(item, |declaration| {
            use crate::pass::resolve::TopLevelDeclaration;
            let labels = match declaration {
                TopLevelDeclaration::Item(Item::Labels(labels, _))
                | TopLevelDeclaration::TypeRecMember(TypeRecMember::Labels(labels, _)) => labels,
                _ => return,
            };
            affected.push(
                labels
                    .arms_in_source_order()
                    .flatten()
                    .any(|entry| selected_labels.contains(&entry.name)),
            );
        });
    }
    affected
}

pub(super) fn decompose_owner(
    mut owner: Labels<Surface>,
    selected_names: &BTreeSet<String>,
    reexports: &mut BTreeMap<String, TypeAlias<Surface>>,
) -> DecomposedLabels {
    let named_alias = owner.type_alias_name.as_ref().map(|name| TypeAlias {
        vis: owner.vis.clone(),
        name: name.clone(),
        name_span: owner
            .type_alias_span
            .expect("named labels carry a name span"),
        type_params: owner.type_alias_params.clone(),
        body: crate::pass::nominal_declarations::named_nominal_alias_body(&owner, |entry| {
            vec![mint_label_newtype_name(&entry.name)]
        }),
        meta: Meta::new(owner.meta.span),
        editable_span: None,
        doc: owner.doc.clone(),
    });
    let entries = match owner.type_alias_arms.take() {
        Some(arms) => arms.into_iter().flat_map(|arm| arm.entries).collect(),
        None => std::mem::take(&mut owner.entries),
    };
    let mut members = Vec::new();
    for entry in entries {
        let nominal_name = mint_label_newtype_name(&entry.name);
        if selected_names.contains(&nominal_name) {
            if let Some(mut alias) = reexports.remove(&nominal_name) {
                alias.meta.leading_trivia.clear();
                members.push(TypeRecMember::TypeAlias(alias));
            }
        } else {
            members.push(TypeRecMember::Labels(
                Labels {
                    vis: owner.vis.clone(),
                    rec_span: None,
                    type_alias_name: None,
                    type_alias_span: None,
                    type_alias_params: Vec::new(),
                    type_alias_arms: None,
                    type_alias_rhs: None,
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
    }
    DecomposedLabels { members }
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
    fn affected_owners_ignore_ordinary_rhs_references() {
        let module = parse(
            "mod source; type Before = Foo & { First: .; }; type Forwarded = Foo; type Via_forward = Forwarded & { Second: .; }; pub type { Foo: .; } pub type Later[A] = Foo & { Box[A]: A; }; type Still[A] = Box[A] & { Last: .; };",
        );
        assert_eq!(
            affected_owners(&module, &["Foo".to_owned()].into_iter().collect()),
            [false, false, true, false, false],
        );
    }

    #[test]
    fn affected_owners_select_only_written_recursive_member_declarations() {
        let module = parse(
            "mod source; type { Box[*F][A]<X>: F[A] & X; } type Plain[*F][A] = Box[F][A] & { Marker: .; }; rec { type Later[*G][B] = Box[G][B] | { Link[*G][B]: Next[G][ B]; }; type Next[*H][C] = Box[H][C] | { Back[*H][C]: Later[H][ C]; }; }",
        );
        assert_eq!(
            affected_owners(&module, &["Box".to_owned()].into_iter().collect()),
            [true, false, false, false],
        );
    }

    #[test]
    fn decomposition_keeps_unselected_origins_and_named_arm_arguments() {
        let module = parse(
            "mod source; /// A family.
pub type Choice[A] = { Value[A]<X>: A & X; } | (Value[A] & { Empty: .; });",
        );
        let Item::Labels(owner, ()) = &module.items[0] else {
            panic!("labels owner");
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
        let targets = ["Value".to_owned()].into_iter().collect();
        let result = decompose_owner(owner.clone(), &targets, &mut reexports);
        assert!(reexports.is_empty());
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
        assert_eq!(retained.entries[0].name, "Empty");
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
