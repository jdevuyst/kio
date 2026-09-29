use std::collections::{HashMap, HashSet};

use crate::ast::{Lowered, TypeParam};
use crate::pass::visit_mut::{TypecheckVisitMut, walk_type};
use crate::span::Span;

use super::{PositionIndex, Type, TypeBinderScope};

impl PositionIndex {
    pub(crate) fn display_let_type(&self, module: &str, span: Span, ty: &Type) -> Type {
        let empty = TypeBinderScope::default();
        let scope = self
            .inlay_let_types
            .get(&(module.to_owned(), span))
            .map(|(_, scope)| scope)
            .unwrap_or(&empty);
        self.present_type(module, ty, scope).0
    }

    pub(crate) fn display_call_type_arguments(&self, module: &str) -> Vec<(Span, Vec<Type>)> {
        let mut result = Vec::new();
        for ((owner, end), groups) in &self.inlay_type_args {
            if owner != module {
                continue;
            }
            let mut calls: Vec<(crate::ast::ExpressionSite, Vec<&super::InlayTypeArguments>)> =
                Vec::new();
            for group in groups {
                if let Some((_, stages)) = calls
                    .iter_mut()
                    .find(|(site, _)| site.id == group.call_site.id)
                {
                    stages.push(group);
                } else {
                    calls.push((group.call_site, vec![group]));
                }
            }
            let mut observations = Vec::new();
            let mut rendered = Vec::new();
            for (site, mut stages) in calls {
                stages.sort_by_key(|stage| stage.slot_offset);
                let mut display = Vec::new();
                let mut identity = Vec::new();
                for stage in stages {
                    for ty in &stage.types {
                        let (shown, key) = self.present_type(module, ty, &stage.binders);
                        display.push(shown);
                        identity.push(key);
                    }
                }
                // Coalesce complete repeated observations of one written call,
                // never individual equal type-argument slots.
                if observations
                    .iter()
                    .any(|(span, key)| span == &site.span && key == &identity)
                {
                    continue;
                }
                observations.push((site.span, identity));
                rendered.extend(display);
            }
            if !rendered.is_empty() {
                result.push((*end, rendered));
            }
        }
        result
    }

    pub(crate) fn display_position_type(&self, module: &str, span: Span, ty: &Type) -> Type {
        let empty = TypeBinderScope::default();
        let scope = self
            .type_binders
            .get(&(module.to_owned(), span))
            .unwrap_or(&empty);
        self.present_type(module, ty, scope).0
    }

    /// Presentation works on copies. The second copy is only an observation
    /// comparison key: free binders use their source identity and quantified
    /// binders use lexical depth. It never enters a checker or phase artifact.
    pub(super) fn present_type(
        &self,
        module: &str,
        ty: &Type,
        scope: &TypeBinderScope,
    ) -> (Type, Type) {
        let mut free = HashSet::new();
        crate::pass::typecheck_core::collect_free_type_vars(ty, &mut free);
        let mut sources = HashMap::new();
        let mut identities = HashMap::new();
        for name in &free {
            let Some(Some(declaration)) = scope.0.get(name) else {
                continue;
            };
            let Some(source) = self
                .binder_presentations
                .iter()
                .find_map(|presentation| presentation.source_name(module, *declaration, name))
            else {
                continue;
            };
            sources.insert(name.clone(), source.to_owned());
            identities.insert(
                name.clone(),
                format!(
                    "\0source:{module}:{}:{}",
                    declaration.start, declaration.end
                ),
            );
        }
        let mut display_names = HashMap::<String, HashSet<String>>::new();
        for name in &free {
            display_names
                .entry(sources.get(name).unwrap_or(name).clone())
                .or_default()
                .insert(identities.get(name).unwrap_or(name).clone());
        }
        // Two distinct free binders may share one written spelling. Keep their
        // checked names where reverting both would conflate their identities.
        sources.retain(|_, source| display_names[source].len() == 1);
        (
            project(ty, &free, sources, false),
            project(ty, &free, identities, true),
        )
    }
}

fn project(
    ty: &Type,
    free: &HashSet<String>,
    replacements: HashMap<String, String>,
    identity: bool,
) -> Type {
    let occupied = free
        .iter()
        .map(|name| replacements.get(name).unwrap_or(name).clone())
        .collect();
    let mut visitor = Projection {
        replacements,
        occupied,
        bound: Vec::new(),
        identity,
    };
    let mut result = ty.clone();
    visitor.visit_type(&mut result);
    result
}

struct Projection {
    replacements: HashMap<String, String>,
    occupied: HashSet<String>,
    bound: Vec<(String, String)>,
    identity: bool,
}

impl TypecheckVisitMut<Lowered> for Projection {
    fn enter_type_binder(&mut self, param: &mut TypeParam, _scope: Span) {
        let raw = param.name.clone();
        let preferred = if self.identity {
            format!("\0bound:{}", self.bound.len())
        } else {
            raw.clone()
        };
        let name = if self.occupied.contains(&preferred) {
            crate::pass::typecheck_core::fresh_type_var(&preferred, &self.occupied)
        } else {
            preferred
        };
        self.occupied.insert(name.clone());
        self.bound.push((raw, name.clone()));
        param.name = name;
    }

    fn exit_type_binder(&mut self, _param: &mut TypeParam, _scope: Span) {
        let (_, chosen) = self.bound.pop().expect("display binder scope is balanced");
        self.occupied.remove(&chosen);
    }

    fn visit_type(&mut self, ty: &mut Type) {
        if let Type::Path { segments, .. } = ty
            && let [segment] = segments.as_mut_slice()
        {
            if let Some((_, bound)) = self
                .bound
                .iter()
                .rev()
                .find(|(raw, _)| raw == &segment.name)
            {
                segment.name = bound.clone();
            } else if let Some(replacement) = self.replacements.get(&segment.name) {
                segment.name = replacement.clone();
            }
        }
        walk_type(self, ty);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{ExpressionOccurrence, ExpressionSite, Meta};
    use crate::pass::binder_presentation::BinderPresentation;

    fn path(name: &str) -> Type {
        Type::synth_path(vec![name.to_owned()], Vec::new(), Span::new(30, 31))
    }
    fn scope(names: &[(&str, Span)]) -> TypeBinderScope {
        TypeBinderScope(
            names
                .iter()
                .map(|(name, span)| (name.to_string(), Some(*span)))
                .collect(),
        )
    }
    fn index() -> PositionIndex {
        let mut presentation = BinderPresentation::default();
        for (raw, span) in [
            ("A", Span::new(1, 2)),
            ("A_n2", Span::new(10, 11)),
            ("A_n3", Span::new(10, 11)),
        ] {
            presentation.record_name("main", span, raw, "A");
        }
        let mut index = PositionIndex::new();
        index.present_binder_names(std::sync::Arc::new(presentation));
        index
    }

    #[cfg(feature = "lsp")]
    #[test]
    #[ignore = "explicit bounded package-storage measurement"]
    fn measure_position_presentation_storage() {
        let path =
            std::env::var_os("KIO_DEBUG_PRESENTATION_PROFILE").expect("set the package root");
        crate::cache::policy::disable_for_process();
        let analysis = crate::cmd::check::analyze_workspace_at_with_overlay_lsp(
            std::path::Path::new(&path),
            &crate::package_collection::SourceOverlay::empty(),
        )
        .expect("profile package checks");
        let index = &analysis.position_index;
        let scopes = index
            .type_binders
            .values()
            .chain(index.inlay_let_types.values().map(|(_, scope)| scope))
            .chain(
                index
                    .inlay_type_args
                    .values()
                    .flatten()
                    .map(|group| &group.binders),
            )
            .collect::<Vec<_>>();
        println!(
            "positions={} scopes={} bindings={} scope_capacity={} old_name_entry={} origin_entry={} hint_stages={} hint_stage_size={} presentations={:?}",
            index.types.len(),
            scopes.len(),
            scopes.iter().map(|scope| scope.0.len()).sum::<usize>(),
            scopes.iter().map(|scope| scope.0.capacity()).sum::<usize>(),
            std::mem::size_of::<String>(),
            std::mem::size_of::<(String, Option<Span>)>(),
            index.inlay_type_args.values().map(Vec::len).sum::<usize>(),
            std::mem::size_of::<super::super::InlayTypeArguments>(),
            index
                .binder_presentations
                .iter()
                .map(|presentation| presentation.storage_counts())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn display_preserves_distinct_free_and_quantified_binders() {
        let index = index();
        let binders = scope(&[("A", Span::new(1, 2)), ("A_n2", Span::new(10, 11))]);
        let function = Type::Function {
            param: Box::new(path("A_n2")),
            ret: Box::new(path("A")),
            abi_arity: 1,
            caps: (),
            meta: Meta::new(Span::new(0, 40)),
        };
        let quantified = Type::Forall {
            param: TypeParam {
                name: "A_n2".into(),
                span: Span::new(10, 11),
                kind: None,
            },
            body: Box::new(function),
            meta: Meta::new(Span::new(0, 40)),
        };
        let (shown, _) = index.present_type("main", &quantified, &binders);
        assert_eq!(
            crate::pass::typecheck_core::display_type(&shown),
            "[A_n2] A_n2 -> A"
        );
        let pair = Type::Product {
            left: Box::new(path("A")),
            right: Box::new(path("A_n2")),
            meta: Meta::new(Span::new(0, 40)),
        };
        let (shown, key) = index.present_type("main", &pair, &binders);
        assert_eq!(shown, pair);
        let Type::Product { left, right, .. } = key else {
            panic!("product key");
        };
        assert_ne!(left, right);
    }

    #[test]
    fn display_excludes_foreign_quantifiers_and_qualified_nominals() {
        let index = index();
        let binders = scope(&[("A_n2", Span::new(10, 11))]);
        let foreign = Type::Forall {
            param: TypeParam {
                name: "A_n2".into(),
                span: Span::new(10, 11),
                kind: None,
            },
            body: Box::new(path("A_n2")),
            meta: Meta::new(Span::new(0, 40)),
        };
        assert_eq!(index.present_type("main", &foreign, &binders).0, foreign);
        let nominal = Type::synth_path(
            vec!["provider".into(), "A_n2".into()],
            Vec::new(),
            Span::new(10, 11),
        );
        assert_eq!(index.present_type("main", &nominal, &binders).0, nominal);
        assert_eq!(
            index.present_type("provider", &path("A_n2"), &binders).0,
            path("A_n2")
        );
    }

    #[test]
    fn display_renames_a_quantifier_that_would_capture_a_projected_free_name() {
        let index = index();
        let binders = scope(&[("A_n2", Span::new(10, 11))]);
        let ty = Type::Forall {
            param: TypeParam {
                name: "A".into(),
                span: Span::new(50, 51),
                kind: None,
            },
            body: Box::new(Type::Product {
                left: Box::new(path("A")),
                right: Box::new(path("A_n2")),
                meta: Meta::new(Span::new(0, 40)),
            }),
            meta: Meta::new(Span::new(0, 40)),
        };
        let shown = index.present_type("main", &ty, &binders).0;
        let Type::Forall { param, body, .. } = shown else {
            panic!("forall");
        };
        let Type::Product { left, right, .. } = *body else {
            panic!("product");
        };
        assert_ne!(param.name, "A");
        assert_eq!(left, Box::new(path(&param.name)));
        assert_eq!(right, Box::new(path("A")));
    }

    #[test]
    fn call_observations_refine_by_owner_and_stage_then_coalesce_whole_groups() {
        let mut index = index();
        let site = ExpressionSite {
            id: ExpressionOccurrence::fresh().key(),
            span: Span::new(100, 120),
        };
        let copy = ExpressionSite {
            id: ExpressionOccurrence::fresh().key(),
            ..site
        };
        let end = Span::new(110, 110);
        let first = scope(&[("A_n2", Span::new(10, 11))]);
        let second = scope(&[("A_n3", Span::new(10, 11))]);
        index.record_inlay_type_args(
            "main",
            site,
            end,
            0,
            vec![Type::Bottom {
                meta: Meta::new(end),
            }],
            first.clone(),
        );
        let mut shard = PositionIndex::new();
        shard.record_inlay_type_args("main", site, end, 2, vec![path("A_n2")], first.clone());
        shard.record_inlay_type_args(
            "main",
            site,
            end,
            0,
            vec![path("A_n2"), path("A_n2")],
            first,
        );
        index.merge(shard);
        index.record_inlay_type_args(
            "main",
            copy,
            end,
            0,
            vec![path("A_n3"), path("A_n3")],
            second.clone(),
        );
        index.record_inlay_type_args("main", copy, end, 2, vec![path("A_n3")], second);
        assert_eq!(
            index.display_call_type_arguments("main"),
            vec![(end, vec![path("A"), path("A"), path("A")])]
        );
    }
}
