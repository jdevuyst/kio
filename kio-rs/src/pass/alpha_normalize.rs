//! Capture-avoiding lexical binder names at the shared checker boundary.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::ast::{Module, Type, TypeParam};
use crate::pass::binder_presentation::BinderPresentation;
use crate::pass::resolve::Package;
use crate::pass::visit_mut::{TypecheckVisitMut, TypecheckVisitPhase, walk_type};
use crate::span::Span;

/// Proof that lexical type binders in `package` have been alpha-normalized.
#[derive(Debug, Clone)]
pub(crate) struct AlphaNormalizedPackage<P: TypecheckVisitPhase> {
    package: Box<Package<P>>,
    presentation: Arc<BinderPresentation>,
}

/// Proof that lexical type binders in one auxiliary module were normalized.
pub(crate) struct AlphaNormalizedModule<P: TypecheckVisitPhase> {
    module: Module<P>,
    presentation: BinderPresentation,
}

impl<P: TypecheckVisitPhase> AlphaNormalizedModule<P> {
    pub(crate) fn module(&self) -> &Module<P> {
        &self.module
    }

    pub(crate) fn presentation(&self) -> &BinderPresentation {
        &self.presentation
    }
}

impl<P: TypecheckVisitPhase> AlphaNormalizedPackage<P> {
    pub(crate) fn package(&self) -> &Package<P> {
        &self.package
    }

    pub(crate) fn presentation(&self) -> &BinderPresentation {
        &self.presentation
    }

    pub(crate) fn presentation_arc(&self) -> Arc<BinderPresentation> {
        self.presentation.clone()
    }

    #[cfg(any(feature = "surface", feature = "cli", test))]
    pub(crate) fn renormalize_after<Q, F>(self, transform: F) -> AlphaNormalizedPackage<Q>
    where
        Q: TypecheckVisitPhase,
        F: FnOnce(Package<P>) -> Package<Q>,
    {
        renormalize_package(transform(*self.package), self.presentation)
    }

    pub(crate) fn into_parts(self) -> (Package<P>, BinderPresentation) {
        let presentation = Arc::unwrap_or_clone(self.presentation);
        (*self.package, presentation)
    }
}

impl<P: TypecheckVisitPhase> std::ops::Deref for AlphaNormalizedPackage<P> {
    type Target = Package<P>;

    fn deref(&self) -> &Self::Target {
        &self.package
    }
}

pub(crate) fn normalize_package<P>(package: &Package<P>) -> AlphaNormalizedPackage<P>
where
    P: TypecheckVisitPhase,
{
    let (package, presentation) =
        normalize_owned_package(package.clone(), true, OccurrencePolicy::Refresh);
    AlphaNormalizedPackage {
        package,
        presentation: Arc::new(presentation),
    }
}

#[cfg(feature = "surface")]
pub(crate) fn normalize_transformed_package<P>(package: &Package<P>) -> AlphaNormalizedPackage<P>
where
    P: TypecheckVisitPhase,
{
    let (package, presentation) =
        normalize_owned_package(package.clone(), false, OccurrencePolicy::Refresh);
    AlphaNormalizedPackage {
        package,
        presentation: Arc::new(presentation),
    }
}

#[cfg(any(feature = "surface", feature = "cli", test))]
pub(crate) fn renormalize_package<P>(
    package: Package<P>,
    presentation: Arc<BinderPresentation>,
) -> AlphaNormalizedPackage<P>
where
    P: TypecheckVisitPhase,
{
    let (package, _) = normalize_owned_package(package, false, OccurrencePolicy::Refresh);
    AlphaNormalizedPackage {
        package,
        presentation,
    }
}

/// Reestablish binder normalization after filtering one checked derivation.
/// Retained bodies remain paired with that derivation's elaboration tables.
#[cfg(feature = "surface")]
pub(crate) fn renormalize_checked_package<P>(
    package: Package<P>,
    presentation: Arc<BinderPresentation>,
) -> AlphaNormalizedPackage<P>
where
    P: TypecheckVisitPhase,
{
    let (package, _) = normalize_owned_package(package, false, OccurrencePolicy::Preserve);
    AlphaNormalizedPackage {
        package,
        presentation,
    }
}

fn normalize_owned_package<P>(
    mut package: Package<P>,
    protect_synthesized_call_heads: bool,
    occurrences: OccurrencePolicy,
) -> (Box<Package<P>>, BinderPresentation)
where
    P: TypecheckVisitPhase,
{
    let mut normalizer = PackageNormalizer {
        presentation: BinderPresentation::default(),
        protect_synthesized_call_heads,
        occurrences,
    };
    normalizer.visit_package(&mut package);
    (Box::new(package), normalizer.presentation)
}

pub(crate) fn normalize_module<P>(module: &Module<P>) -> AlphaNormalizedModule<P>
where
    P: TypecheckVisitPhase,
{
    let mut module = module.clone();
    let mut presentation = BinderPresentation::default();
    normalize_module_in_place(
        &mut module,
        &mut presentation,
        true,
        OccurrencePolicy::Refresh,
    );
    AlphaNormalizedModule {
        module,
        presentation,
    }
}

struct PackageNormalizer {
    presentation: BinderPresentation,
    protect_synthesized_call_heads: bool,
    occurrences: OccurrencePolicy,
}

#[derive(Clone, Copy)]
enum OccurrencePolicy {
    Refresh,
    #[cfg(feature = "surface")]
    Preserve,
}

pub(crate) fn erase_package_occurrences<P: TypecheckVisitPhase>(package: &mut Package<P>) {
    OccurrenceEraser.visit_package(package);
}

#[cfg(feature = "prime")]
pub(crate) fn erase_module_occurrences<P: TypecheckVisitPhase>(module: &mut Module<P>) {
    OccurrenceEraser.visit_module(module);
}

struct OccurrenceEraser;

impl<P: TypecheckVisitPhase> TypecheckVisitMut<P> for OccurrenceEraser {
    fn visit_expr(&mut self, expr: &mut crate::ast::Expr<P>) {
        *expr.occurrence_mut() = Default::default();
        crate::pass::visit_mut::walk_expr(self, expr);
    }
}

impl<P: TypecheckVisitPhase> TypecheckVisitMut<P> for PackageNormalizer {
    fn visit_module(&mut self, module: &mut Module<P>) {
        normalize_module_in_place(
            module,
            &mut self.presentation,
            self.protect_synthesized_call_heads,
            self.occurrences,
        );
    }
}

fn normalize_module_in_place<P: TypecheckVisitPhase>(
    module: &mut Module<P>,
    presentation: &mut BinderPresentation,
    protect_synthesized_call_heads: bool,
    occurrences: OccurrencePolicy,
) {
    let module_path = module
        .path
        .segments
        .iter()
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join("/");
    for item in &mut module.items {
        let mut names = CollectedNames::default();
        NameCollector(&mut names).visit_item(item);
        if !protect_synthesized_call_heads {
            names.protected_value_names.clear();
            names.protected_value_paths.clear();
        }
        BinderNormalizer::new(&module_path, presentation, names, occurrences).visit_item(item);
    }
}

#[derive(Default)]
struct CollectedNames {
    occupied: HashSet<String>,
    protected_value_names: HashSet<String>,
    protected_value_paths: HashSet<(Span, String)>,
}

struct NameCollector<'a>(&'a mut CollectedNames);

impl<P: TypecheckVisitPhase> TypecheckVisitMut<P> for NameCollector<'_> {
    fn enter_type_binder(&mut self, param: &mut TypeParam, _scope: Span) {
        self.0.occupied.insert(param.name.clone());
    }

    fn enter_value_binder(&mut self, name: &mut String, _span: Span, _scope: Span) {
        self.0.occupied.insert(name.clone());
    }

    fn visit_type(&mut self, ty: &mut Type<P>) {
        if let Type::Path { segments, .. } = ty
            && let [segment] = segments.as_slice()
        {
            self.0.occupied.insert(segment.name.clone());
        }
        walk_type(self, ty);
    }

    fn visit_expr(&mut self, expr: &mut crate::ast::Expr<P>) {
        if let crate::ast::Expr::Path { segments, .. } = expr
            && let [segment] = segments.as_slice()
        {
            self.0.occupied.insert(segment.name.clone());
        }
        if let crate::ast::Expr::Call { callee, meta, .. } = expr
            && let crate::ast::Expr::Path {
                segments,
                meta: callee_meta,
                ..
            } = callee.as_ref()
            && callee_meta.span == meta.span
            && let [segment] = segments.as_slice()
        {
            self.0.protected_value_names.insert(segment.name.clone());
            self.0
                .protected_value_paths
                .insert((segment.span, segment.name.clone()));
        }
        crate::pass::visit_mut::walk_expr(self, expr);
    }
}

struct BinderNormalizer<'a> {
    module_path: &'a str,
    occurrences: OccurrencePolicy,
    presentation: &'a mut BinderPresentation,
    occupied: HashSet<String>,
    type_bindings: HashMap<String, Vec<String>>,
    type_stack: Vec<String>,
    protected_value_names: HashSet<String>,
    protected_value_paths: HashSet<(Span, String)>,
    value_bindings: HashMap<String, Vec<String>>,
    value_stack: Vec<String>,
}

impl<'a> BinderNormalizer<'a> {
    fn new(
        module_path: &'a str,
        presentation: &'a mut BinderPresentation,
        names: CollectedNames,
        occurrences: OccurrencePolicy,
    ) -> Self {
        Self {
            module_path,
            occurrences,
            presentation,
            occupied: names.occupied,
            type_bindings: HashMap::new(),
            type_stack: Vec::new(),
            protected_value_names: names.protected_value_names,
            protected_value_paths: names.protected_value_paths,
            value_bindings: HashMap::new(),
            value_stack: Vec::new(),
        }
    }

    fn fresh_shadow(&mut self, source: &str) -> String {
        for suffix in 2usize.. {
            let candidate = crate::naming::indexed_name(source, suffix);
            if self.occupied.insert(candidate.clone()) {
                return candidate;
            }
        }
        unreachable!("unbounded lexical-binder suffix search must find a fresh name")
    }
}

impl<P: TypecheckVisitPhase> TypecheckVisitMut<P> for BinderNormalizer<'_> {
    fn enter_type_binder(&mut self, param: &mut TypeParam, _scope: Span) {
        let source = param.name.clone();
        let normalized = if self.type_bindings.contains_key(&source) {
            self.fresh_shadow(&source)
        } else {
            source.clone()
        };
        self.presentation
            .record_name(self.module_path, param.span, &normalized, &source);
        param.name = normalized.clone();
        self.type_bindings
            .entry(source.clone())
            .or_default()
            .push(normalized);
        self.type_stack.push(source);
    }

    fn exit_type_binder(&mut self, _param: &mut TypeParam, _scope: Span) {
        let source = self
            .type_stack
            .pop()
            .expect("alpha-normalizer exits a live type binder");
        let normalized = self
            .type_bindings
            .get_mut(&source)
            .expect("alpha-normalizer retains every entered type binder");
        normalized.pop();
        if normalized.is_empty() {
            self.type_bindings.remove(&source);
        }
    }

    fn visit_type(&mut self, ty: &mut Type<P>) {
        if let Type::Path { segments, .. } = ty
            && let [segment] = segments.as_mut_slice()
            && let Some(normalized) = self
                .type_bindings
                .get(segment.name.as_str())
                .and_then(|entries| entries.last())
                .cloned()
        {
            let source = segment.name.clone();
            self.presentation
                .record_name(self.module_path, segment.span, &normalized, &source);
            segment.name = normalized;
        }
        walk_type(self, ty);
    }

    fn enter_value_binder(&mut self, name: &mut String, span: Span, _scope: Span) {
        let source = name.clone();
        if source == "_" {
            self.value_stack.push(source);
            return;
        }
        let normalized = if self.protected_value_names.contains(&source)
            || self.value_bindings.contains_key(&source)
        {
            self.fresh_shadow(&source)
        } else {
            source.clone()
        };
        self.presentation
            .record_name(self.module_path, span, &normalized, &source);
        *name = normalized.clone();
        self.value_bindings
            .entry(source.clone())
            .or_default()
            .push(normalized);
        self.value_stack.push(source);
    }

    fn exit_value_binder(&mut self, _name: &mut String, _span: Span, _scope: Span) {
        let source = self
            .value_stack
            .pop()
            .expect("alpha-normalizer exits a live value binder");
        if source == "_" {
            return;
        }
        let normalized = self
            .value_bindings
            .get_mut(&source)
            .expect("alpha-normalizer retains every entered value binder");
        normalized.pop();
        if normalized.is_empty() {
            self.value_bindings.remove(&source);
        }
    }

    fn visit_expr(&mut self, expr: &mut crate::ast::Expr<P>) {
        if matches!(self.occurrences, OccurrencePolicy::Refresh) {
            *expr.occurrence_mut() = crate::ast::ExpressionOccurrenceCarrier::fresh();
        }
        if let crate::ast::Expr::Path { segments, .. } = expr
            && let [segment] = segments.as_mut_slice()
            && let Some(normalized) = self
                .type_bindings
                .get(segment.name.as_str())
                .and_then(|entries| entries.last())
                .or_else(|| {
                    if self
                        .protected_value_paths
                        .contains(&(segment.span, segment.name.clone()))
                    {
                        None
                    } else {
                        self.value_bindings
                            .get(segment.name.as_str())
                            .and_then(|entries| entries.last())
                    }
                })
                .cloned()
        {
            let source = segment.name.clone();
            self.presentation
                .record_name(self.module_path, segment.span, &normalized, &source);
            segment.name = normalized;
        }
        crate::pass::visit_mut::walk_expr(self, expr);
    }
}

#[cfg(all(test, feature = "prime"))]
mod tests {
    use super::*;
    use crate::ast::{Expr, Item, SignatureParam};
    #[cfg(feature = "surface")]
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::parse;
    use crate::pipeline::Pipeline;
    use crate::prime::pipeline::PrimePipeline;
    use std::path::{Path, PathBuf};

    fn retained_type_argument_package() -> Package<crate::ast::Prime> {
        let source = r#"module main;

fn outer[*F][A](value: F(A)) -> F(A) {
    let occupied = identity(A_n2, ());
    let inner = .[*F][A](other: F(A)) -> F(A) {
        let qualified = identity(types.A, ());
        identity(F(A), other)
    };
    inner(F, A, value)
}
"#;
        let module = parse(source).expect("parse");
        let (modules, _) =
            PrimePipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower Kio'");
        Package::build(Path::new(""), modules, None).expect("build package shell")
    }

    #[derive(Default)]
    struct LexicalNames {
        binders: Vec<String>,
        paths: Vec<(Span, String)>,
    }

    impl TypecheckVisitMut<crate::ast::Prime> for LexicalNames {
        fn enter_type_binder(&mut self, param: &mut TypeParam, _scope: Span) {
            self.binders.push(param.name.clone());
        }

        fn visit_expr(&mut self, expr: &mut Expr<crate::ast::Prime>) {
            if let Expr::Path { segments, .. } = expr {
                self.paths.push((
                    segments[0].span,
                    segments
                        .iter()
                        .map(|segment| segment.name.as_str())
                        .collect::<Vec<_>>()
                        .join("."),
                ));
            }
            crate::pass::visit_mut::walk_expr(self, expr);
        }
    }

    fn assert_retained_type_argument_names(module: &Module<crate::ast::Prime>) -> LexicalNames {
        let mut names = LexicalNames::default();
        names.visit_module(&mut module.clone());
        assert_eq!(names.binders, ["F", "A", "F_n2", "A_n3"]);
        assert_eq!(
            names
                .paths
                .iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>(),
            [
                "identity", "A_n2", "identity", "types.A", "identity", "F_n2", "A_n3", "other",
                "inner", "F", "A", "value",
            ]
        );
        names
    }

    #[test]
    fn retained_type_arguments_keep_lexical_identity_and_free_names() {
        let normalized = normalize_package(&retained_type_argument_package());
        let module = &normalized.package().module("main").expect("main").module;
        assert_retained_type_argument_names(module);
    }

    #[test]
    fn normalization_assigns_each_occurrence_and_erasure_clears_every_node() {
        #[derive(Default)]
        struct Occurrences(Vec<Option<crate::ast::ExpressionOccurrenceId>>);
        impl TypecheckVisitMut<crate::ast::Prime> for Occurrences {
            fn visit_expr(&mut self, expr: &mut Expr<crate::ast::Prime>) {
                self.0.push(expr.occurrence().assigned_key());
                crate::pass::visit_mut::walk_expr(self, expr);
            }
        }
        let raw = retained_type_argument_package();
        let first = normalize_package(&raw);
        let independent = normalize_package(first.package());
        let mut first_ids = Occurrences::default();
        first_ids.visit_package(&mut first.package().clone());
        assert!(!first_ids.0.is_empty());
        assert!(first_ids.0.iter().all(Option::is_some));
        let first_set = first_ids.0.iter().copied().collect::<HashSet<_>>();
        assert_eq!(first_set.len(), first_ids.0.len());
        let mut independent_ids = Occurrences::default();
        independent_ids.visit_package(&mut independent.package().clone());
        assert_eq!(first_ids.0.len(), independent_ids.0.len());
        assert!(independent_ids.0.iter().all(|id| !first_set.contains(id)));
        let mut erased = first.package().clone();
        erase_package_occurrences(&mut erased);
        let mut erased_ids = Occurrences::default();
        erased_ids.visit_package(&mut erased);
        assert_eq!(first_ids.0.len(), erased_ids.0.len());
        assert!(erased_ids.0.iter().all(Option::is_none));
        assert_eq!(
            erased.module("main").expect("main").module,
            first.package().module("main").expect("main").module,
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn retained_type_argument_source_presentation_is_preserved() {
        let normalized = normalize_package(&retained_type_argument_package());
        let module = &normalized.package().module("main").expect("main").module;
        let names = assert_retained_type_argument_names(module);
        for (span, mut name) in names.paths {
            let expected = match name.as_str() {
                "F_n2" => "F".to_owned(),
                "A_n3" => "A".to_owned(),
                _ => name.clone(),
            };
            normalized
                .presentation()
                .present_name("main", span, &mut name);
            assert_eq!(name, expected);
        }
    }

    #[test]
    fn same_span_type_application_heads_are_lexical_and_renormalization_is_idempotent() {
        struct SameSpanCalls;

        impl TypecheckVisitMut<crate::ast::Prime> for SameSpanCalls {
            fn visit_expr(&mut self, expr: &mut Expr<crate::ast::Prime>) {
                if let Expr::Call { callee, meta, .. } = expr {
                    callee.meta_mut().span = meta.span;
                }
                crate::pass::visit_mut::walk_expr(self, expr);
            }
        }

        let mut package = retained_type_argument_package();
        SameSpanCalls.visit_package(&mut package);
        let normalized = normalize_package(&package);
        assert_retained_type_argument_names(
            &normalized.package().module("main").expect("main").module,
        );
        let repaired = normalized.renormalize_after(|_| package);
        let module = repaired
            .package()
            .module("main")
            .expect("main")
            .module
            .clone();
        assert_retained_type_argument_names(&module);
        let repeated = repaired.renormalize_after(|package| package);
        assert_eq!(
            repeated.package().module("main").expect("main").module,
            module
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn freshening_and_exact_name_presentation_are_declaration_local() {
        let source = r#"module main;

fn first[A](seed: A) -> . {
    let f = .[A](value: A) { value };
    ()
}

fn second[A_n2](value: A_n2) -> A_n2 { value }
"#;
        let module = parse(source).expect("parse");
        let (modules, _) =
            PrimePipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower Kio'");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        let normalized = normalize_package(&package);
        let entry = normalized.package().module("main").expect("main module");
        let [Item::FnDef(first), Item::FnDef(second)] = entry.module.items.as_slice() else {
            panic!("expected two functions");
        };
        let Expr::Let { value, .. } = &first.body else {
            panic!("expected let body");
        };
        let Expr::FnExpr { sig, .. } = value.as_ref() else {
            panic!("expected anonymous function");
        };
        let [SignatureParam::Type(inner), ..] = sig.params.as_slice() else {
            panic!("expected lambda type binder");
        };
        assert_eq!(inner.name, "A_n2");
        let SignatureParam::Type(second_param) = &second.sig.params[0] else {
            panic!("expected function type binder");
        };
        assert_eq!(second_param.name, "A_n2");

        let mut inner_display = inner.name.clone();
        normalized
            .presentation()
            .present_name("main", inner.span, &mut inner_display);
        assert_eq!(inner_display, "A");
        let mut second_display = second_param.name.clone();
        normalized
            .presentation()
            .present_name("main", second_param.span, &mut second_display);
        assert_eq!(second_display, "A_n2");
    }

    #[cfg(feature = "surface")]
    #[test]
    fn generated_operator_call_keeps_its_declaration_target_under_a_local_shadow() {
        let source = r#"module main;

fn apply(_left: ., _right: .) -> . { () }

op _ + _ { impl apply; };

fn shadow(apply: .) -> . {
    let observed = apply;
    () + ()
}
"#;
        let module = parse(source).expect("parse");
        let (modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower operators");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        let normalized = normalize_package(&package);
        let entry = normalized.package().module("main").expect("main module");
        let shadow = entry
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == "shadow" => Some(def),
                _ => None,
            })
            .expect("shadow function");
        let [SignatureParam::Value(param)] = shadow.sig.params.as_slice() else {
            panic!("expected one value parameter");
        };
        assert_eq!(param.name, "apply_n2");
        let Expr::Let { value, body, .. } = &shadow.body else {
            panic!("expected lowered let");
        };
        let Expr::Path { segments, .. } = value.as_ref() else {
            panic!("expected ordinary local reference");
        };
        assert_eq!(segments[0].name, "apply_n2");
        let Expr::Call { callee, .. } = body.as_ref() else {
            panic!("expected folded operator call");
        };
        let Expr::Path { segments, .. } = callee.as_ref() else {
            panic!("expected callable path");
        };
        assert_eq!(segments[0].name, "apply");

        let mut displayed = param.name.clone();
        normalized
            .presentation()
            .present_name("main", param.meta.span, &mut displayed);
        assert_eq!(displayed, "apply");
    }

    #[test]
    fn renormalization_treats_transformed_same_span_calls_as_ordinary_lexical_calls() {
        let source = r#"module main;

fn run() -> . {
    let _kg0 = .(value: .) { value };
    _kg0(())
}
"#;
        let module = parse(source).expect("parse");
        let (modules, _) =
            PrimePipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower Kio'");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        let normalized = normalize_package(&package);
        let repeated = normalized.renormalize_after(|mut package| {
            let entry = package.modules_mut().next().expect("one test module");
            let [Item::FnDef(run)] = entry.module.items.as_mut_slice() else {
                panic!("expected one function")
            };
            let Expr::Let { body, .. } = &mut run.body else {
                panic!("expected let body")
            };
            let Expr::Call { callee, meta, .. } = body.as_mut() else {
                panic!("expected call body")
            };
            callee.meta_mut().span = meta.span;
            package
        });
        let entry = repeated.package().module("main").expect("main module");
        let [Item::FnDef(run)] = entry.module.items.as_slice() else {
            panic!("expected one function")
        };
        let Expr::Let { name, body, .. } = &run.body else {
            panic!("expected let body")
        };
        let Expr::Call { callee, .. } = body.as_ref() else {
            panic!("expected call body")
        };
        let Expr::Path { segments, .. } = callee.as_ref() else {
            panic!("expected path callee")
        };
        assert_eq!(name, "_kg0");
        assert_eq!(segments[0].name, "_kg0");
    }

    #[test]
    fn renormalization_repairs_arbitrary_transform_output_and_is_idempotent() {
        let source = r#"module main;

fn outer[A](seed: A) -> . {
    let inner = .[A](value: A) { value };
    ()
}
"#;
        let module = parse(source).expect("parse");
        let (modules, _) =
            PrimePipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower Kio'");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        let normalized = normalize_package(&package);
        let presentation = normalized.presentation_arc();

        let repaired = normalized.renormalize_after(|_| package);
        assert!(Arc::ptr_eq(&presentation, &repaired.presentation_arc()));
        let repaired_module = repaired
            .package()
            .module("main")
            .expect("main module")
            .module
            .clone();
        let [Item::FnDef(outer)] = repaired_module.items.as_slice() else {
            panic!("expected outer function");
        };
        let Expr::Let { value, .. } = &outer.body else {
            panic!("expected let body");
        };
        let Expr::FnExpr { sig, .. } = value.as_ref() else {
            panic!("expected inner function");
        };
        let [SignatureParam::Type(inner), ..] = sig.params.as_slice() else {
            panic!("expected inner type binder");
        };
        assert_eq!(inner.name, "A_n2");

        let repeated = repaired.renormalize_after(|package| package);
        assert_eq!(
            repeated
                .package()
                .module("main")
                .expect("main module")
                .module,
            repaired_module
        );
    }
}
