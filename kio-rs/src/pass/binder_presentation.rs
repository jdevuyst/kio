//! Source-facing names for alpha-normalized lexical binders.
//!
//! Presentation accompanies a normalized tree but never validates one.

use std::collections::HashMap;

use crate::span::Span;

/// Sparse exact-view guard, separate from the semantic key of a memo entry.
#[cfg(feature = "surface")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TypePresentationGuard(Vec<(usize, std::sync::Arc<String>)>);

#[cfg(feature = "surface")]
impl TypePresentationGuard {
    pub(crate) fn record(&mut self, ordinal: usize, param: &crate::ast::TypeParam) {
        if let Some(name) = param.shared_written_name() {
            self.0.push((ordinal, name));
        }
    }

    #[cfg(test)]
    pub(crate) fn storage_counts(&self) -> (usize, usize) {
        (
            self.0.len(),
            self.0.capacity() * std::mem::size_of::<(usize, std::sync::Arc<String>)>(),
        )
    }
}

/// Capture-safe lexical projection shared by diagnostics and composed IDE types.
pub(crate) struct TypeNameProjection {
    names: Vec<String>,
    next: usize,
    bound: HashMap<String, Vec<usize>>,
    stack: Vec<String>,
    replacements: HashMap<String, String>,
    #[cfg(all(test, feature = "surface"))]
    conflict_removals: usize,
}

impl TypeNameProjection {
    pub(crate) fn new<P>(ty: &crate::ast::Type<P>, replacements: HashMap<String, String>) -> Self
    where
        P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
    {
        use crate::ast::Type;
        enum Work<'a, P: crate::ast::Phase> {
            Type(&'a Type<P>),
            Leave(usize),
        }
        struct Preference<'a> {
            raw: &'a str,
            name: &'a str,
            conflicts: bool,
        }
        let mut pending = vec![Work::Type(ty)];
        let mut preferences: Vec<Preference<'_>> = Vec::new();
        let mut bound = HashMap::<&str, Vec<usize>>::new();
        let mut candidates = HashMap::<&str, Vec<usize>>::new();
        let mut occupied = std::collections::HashSet::new();
        #[cfg(all(test, feature = "surface"))]
        let mut conflict_removals = 0;
        while let Some(work) = pending.pop() {
            let ty = match work {
                Work::Leave(id) => {
                    let preference = &preferences[id];
                    bound
                        .get_mut(preference.raw)
                        .expect("bound display name exists")
                        .pop();
                    if !preference.conflicts {
                        let removed = candidates
                            .get_mut(preference.name)
                            .expect("preferred display name exists")
                            .pop();
                        debug_assert_eq!(removed, Some(id));
                    }
                    continue;
                }
                Work::Type(ty) => ty,
            };
            match ty {
                Type::Forall { param, body, .. } => {
                    let id = preferences.len();
                    let name = param.written_name().unwrap_or(&param.name);
                    preferences.push(Preference {
                        raw: &param.name,
                        name,
                        conflicts: false,
                    });
                    occupied.insert(name.to_owned());
                    bound.entry(&param.name).or_default().push(id);
                    candidates.entry(name).or_default().push(id);
                    pending.push(Work::Leave(id));
                    pending.push(Work::Type(body));
                }
                Type::Path { segments, args, .. } => {
                    if let [segment] = segments.as_slice() {
                        let target = bound
                            .get(segment.name.as_str())
                            .and_then(|ids| ids.last())
                            .copied();
                        let name = match target {
                            Some(id) => preferences[id].name,
                            None => {
                                let name = replacements.get(&segment.name).unwrap_or(&segment.name);
                                occupied.insert(name.clone());
                                name.as_str()
                            }
                        };
                        // A conflicting preference is removed once, even when
                        // the same outer binding is referenced many times.
                        if let Some(active) = candidates.get_mut(name) {
                            let floor = target.map_or(0, |id| id + 1);
                            while active.last().is_some_and(|id| *id >= floor) {
                                let id = active.pop().expect("active display preference exists");
                                preferences[id].conflicts = true;
                                #[cfg(all(test, feature = "surface"))]
                                {
                                    conflict_removals += 1;
                                }
                            }
                        }
                    }
                    pending.extend(args.iter().rev().map(Work::Type));
                }
                Type::Goal { args, .. } => pending.extend(args.iter().rev().map(Work::Type)),
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
                    pending.push(Work::Type(ret));
                    pending.push(Work::Type(param));
                }
                Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => {}
                Type::LabelSugar { ext, .. } => match *ext {},
            }
        }
        let mut suffixes = HashMap::<&str, usize>::new();
        let names = preferences
            .iter()
            .map(|preference| {
                if !preference.conflicts {
                    return preference.name.to_owned();
                }
                if preference.raw != preference.name && occupied.insert(preference.raw.to_owned()) {
                    return preference.raw.to_owned();
                }
                let suffix = suffixes.entry(preference.name).or_insert(2);
                loop {
                    let candidate = crate::naming::indexed_name(preference.name, *suffix);
                    *suffix += 1;
                    if occupied.insert(candidate.clone()) {
                        break candidate;
                    }
                }
            })
            .collect();
        Self {
            names,
            next: 0,
            bound: HashMap::new(),
            stack: Vec::new(),
            replacements,
            #[cfg(all(test, feature = "surface"))]
            conflict_removals,
        }
    }

    pub(crate) fn enter(&mut self, param: &crate::ast::TypeParam) -> &str {
        let id = self.next;
        self.next += 1;
        self.bound.entry(param.name.clone()).or_default().push(id);
        self.stack.push(param.name.clone());
        &self.names[id]
    }

    pub(crate) fn leave(&mut self) {
        let raw = self.stack.pop().expect("display binder scope is balanced");
        self.bound
            .get_mut(&raw)
            .expect("display binder is active")
            .pop();
    }

    pub(crate) fn name(&self, raw: &str) -> Option<&str> {
        self.bound
            .get(raw)
            .and_then(|ids| ids.last())
            .map(|id| self.names[*id].as_str())
            .or_else(|| self.replacements.get(raw).map(String::as_str))
    }
}

impl<P: crate::pass::visit_mut::TypecheckVisitPhase> crate::pass::visit_mut::TypecheckVisitMut<P>
    for TypeNameProjection
{
    fn enter_type_binder(&mut self, param: &mut crate::ast::TypeParam, _scope: Span) {
        param.name = self.enter(param).to_owned();
        param.presentation = Default::default();
    }

    fn exit_type_binder(&mut self, _param: &mut crate::ast::TypeParam, _scope: Span) {
        self.leave();
    }

    fn visit_type(&mut self, ty: &mut crate::ast::Type<P>) {
        if let crate::ast::Type::Path { segments, .. } = ty
            && let [segment] = segments.as_mut_slice()
            && let Some(name) = self.name(&segment.name)
        {
            segment.name = name.to_owned();
        }
        crate::pass::visit_mut::walk_type(self, ty);
    }
}

/// Lexical names and their declaration origins at an observation's check site.
/// Manually constructed name-only facts retain membership without claiming an origin.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg(feature = "surface")]
pub struct TypeBinderScope(pub(crate) HashMap<String, Option<Span>>);

#[cfg(feature = "surface")]
impl From<std::collections::HashSet<String>> for TypeBinderScope {
    fn from(names: std::collections::HashSet<String>) -> Self {
        Self(names.into_iter().map(|name| (name, None)).collect())
    }
}

#[cfg(feature = "surface")]
impl TypeBinderScope {
    pub(crate) fn names(&self) -> std::collections::HashSet<String> {
        self.0.keys().cloned().collect()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct BinderPresentation {
    locations: HashMap<NameLocation, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct NameLocation {
    module_path: String,
    span: Span,
    normalized: String,
}

impl BinderPresentation {
    pub(super) fn record_name(
        &mut self,
        module_path: &str,
        span: Span,
        normalized: &str,
        source: &str,
    ) {
        let location = NameLocation {
            module_path: module_path.to_owned(),
            span,
            normalized: normalized.to_owned(),
        };
        self.locations.insert(location, source.to_owned());
    }

    #[cfg(any(feature = "surface", feature = "prime"))]
    pub(crate) fn extend(&mut self, other: &Self) {
        for (location, source) in &other.locations {
            self.record_name(
                &location.module_path,
                location.span,
                &location.normalized,
                source,
            );
        }
    }

    #[cfg(all(test, feature = "surface", feature = "lsp"))]
    pub(crate) fn storage_counts(&self) -> (usize, usize, usize) {
        (
            self.locations.len(),
            self.locations.capacity(),
            self.locations
                .iter()
                .map(|(key, value)| {
                    key.module_path.capacity() + key.normalized.capacity() + value.capacity()
                })
                .sum(),
        )
    }

    #[cfg(feature = "surface")]
    pub(crate) fn present_name(&self, module_path: &str, position: Span, name: &mut String) {
        if let Some(source) = self.source_name(module_path, position, name) {
            *name = source.to_owned();
        }
    }

    #[cfg(feature = "surface")]
    pub(crate) fn source_name(
        &self,
        module_path: &str,
        span: Span,
        normalized: &str,
    ) -> Option<&str> {
        self.locations
            .get(&NameLocation {
                module_path: module_path.to_owned(),
                span,
                normalized: normalized.to_owned(),
            })
            .map(String::as_str)
    }
}

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;

    use crate::ast::{Lowered, Meta, Type, TypeParam, TypeParamPresentation};

    fn path(name: &str) -> Type<Lowered> {
        Type::synth_path(vec![name.into()], Vec::new(), Span::new(0, 1))
    }

    fn forall(raw: &str, written: &str, body: Type<Lowered>) -> Type<Lowered> {
        let mut param = TypeParam {
            name: written.into(),
            span: Span::new(0, 1),
            kind: None,
            presentation: TypeParamPresentation::written(),
        };
        param.rename(raw.into());
        Type::Forall {
            param,
            body: Box::new(body),
            meta: Meta::new(Span::new(0, 1)),
        }
    }

    fn pair(left: Type<Lowered>, right: Type<Lowered>) -> Type<Lowered> {
        Type::Product {
            left: Box::new(left),
            right: Box::new(right),
            meta: Meta::new(Span::new(0, 1)),
        }
    }

    fn check_projection(ty: &Type<Lowered>, expected: &str) {
        use crate::pass::visit_mut::TypecheckVisitMut;
        assert_eq!(crate::pass::typecheck_core::display_type(ty), expected);
        let mut copy = ty.clone();
        TypeNameProjection::new(ty, HashMap::new()).visit_type(&mut copy);
        assert_eq!(crate::pass::typecheck_core::display_type(&copy), expected);
    }

    #[test]
    fn composed_names_allow_safe_shadowing_and_preserve_authored_suffixes() {
        let ty = forall(
            "Outer",
            "A",
            Type::synth_function(
                vec![path("Outer")],
                forall(
                    "Inner",
                    "A",
                    Type::synth_function(vec![path("Inner")], path("Inner"), Span::new(0, 1)),
                ),
                Span::new(0, 1),
            ),
        );
        check_projection(&ty, "[A] A -> [A] A -> A");
        check_projection(&forall("Renamed", "A_n2", path("Renamed")), "[A_n2] A_n2");
        let applied = Type::synth_path(vec!["Renamed".into()], vec![path("Free")], Span::new(0, 1));
        check_projection(&forall("Renamed", "F", applied), "[F] F(Free)");
    }

    #[test]
    fn composed_names_keep_outer_and_free_references_unambiguous() {
        let ty = forall("A", "A", forall("A_n2", "A", pair(path("A"), path("A_n2"))));
        check_projection(&ty, "[A][A_n2] A & A_n2");
        let ty = forall("A_n2", "A", pair(path("A"), path("A_n2")));
        check_projection(&ty, "[A_n2] A & A_n2");
        let ty = forall(
            "A",
            "A",
            pair(
                forall("A_n2", "A", pair(path("A"), path("A_n2"))),
                forall("Other", "A_n2", path("Other")),
            ),
        );
        check_projection(&ty, "[A] ([A_n3] A & A_n3) & ([A_n2] A_n2)");
        let nominal = Type::synth_path(
            vec!["provider".into(), "A".into()],
            vec![path("Raw")],
            Span::new(0, 1),
        );
        check_projection(&forall("Raw", "A", nominal), "[A] provider.A(A)");
    }

    #[test]
    fn conflicting_preference_sets_use_conservative_hygienic_fallbacks() {
        let ty = forall(
            "Outer",
            "A",
            forall("Inner", "A", pair(path("Outer"), path("A"))),
        );
        check_projection(&ty, "[Outer][Inner] Outer & A");
    }

    #[test]
    fn written_metadata_does_not_change_equality_debug_or_serialization() {
        let Type::Forall { mut param, .. } = forall("A_n2", "A", path("A_n2")) else {
            unreachable!()
        };
        let mut erased = param.clone();
        erased.presentation = Default::default();
        assert_eq!(param, erased);
        assert_eq!(format!("{param:?}"), format!("{erased:?}"));
        assert_eq!(
            serde_json::to_vec(&param).unwrap(),
            serde_json::to_vec(&erased).unwrap()
        );
        let decoded: TypeParam =
            serde_json::from_slice(&serde_json::to_vec(&param).unwrap()).unwrap();
        assert_eq!(decoded.written_name(), None);
        param.rename("Again".into());
        assert_eq!(param.written_name(), Some("A"));
        erased.rename("Again".into());
        assert_eq!(erased.written_name(), None);
    }

    #[test]
    fn presentation_guards_clone_slots_but_share_original_names() {
        let Type::Forall { param, .. } = forall("A_n2", "A", path("A_n2")) else {
            unreachable!()
        };
        let mut guard = TypePresentationGuard::default();
        let mut generated = param.clone();
        generated.presentation = Default::default();
        guard.record(0, &generated);
        assert_eq!(guard.storage_counts(), (0, 0));
        guard.record(1, &param);
        let cloned = guard.clone();
        assert!(std::sync::Arc::ptr_eq(&guard.0[0].1, &cloned.0[0].1));
        assert!(!std::ptr::eq(guard.0.as_ptr(), cloned.0.as_ptr()));
        assert_eq!(guard.storage_counts().0, 1);
        assert_eq!(
            cloned.storage_counts(),
            (1, std::mem::size_of::<(usize, std::sync::Arc<String>)>())
        );
        eprintln!(
            "presentation guard: inline={} original={:?} clone={:?} shared_name_capacity={}",
            std::mem::size_of::<TypePresentationGuard>(),
            guard.storage_counts(),
            cloned.storage_counts(),
            guard.0[0].1.capacity()
        );
    }

    #[test]
    fn deep_conflicting_preferences_are_removed_once() {
        let mut ty = pair(path("Free"), path("R2047"));
        for index in (0..2048).rev() {
            ty = forall(&format!("R{index}"), "Free", ty);
        }
        let projection = TypeNameProjection::new(&ty, HashMap::new());
        assert_eq!(projection.names.len(), 2048);
        assert_eq!(projection.conflict_removals, 2048);
        assert!(
            projection
                .names
                .iter()
                .enumerate()
                .all(|(i, name)| name == &format!("R{i}"))
        );
        // The test's synthetic depth does not rely on recursive AST destruction.
        while let Type::Forall { body, .. } = ty {
            ty = *body;
        }
    }

    #[test]
    fn unrelated_module_at_same_span_cannot_perturb_exact_name_presentation() {
        let span = Span::new(17, 20);
        let mut presentation = BinderPresentation::default();
        presentation.record_name("provider_a", span, "A_n2", "A");
        presentation.record_name("provider_b", span, "A_n2", "Z");

        let mut provider_a_name = "A_n2".to_owned();
        presentation.present_name("provider_a", span, &mut provider_a_name);
        assert_eq!(provider_a_name, "A");

        let mut provider_b_name = "A_n2".to_owned();
        presentation.present_name("provider_b", span, &mut provider_b_name);
        assert_eq!(provider_b_name, "Z");
    }
}
