//! Haskell backend — the typed FFI skin.
//!
//! Haskell's public contract is **typed** and **native**: products and sums
//! cross through package-branded closed families that reduce to right-nested
//! pairs and `Either`, a host type is the exact associated type selected by
//! the host, and a function is a native `arg -> m ret`. This module owns the
//! shared boundary renderer ([`HaskellShapes`]) used by both Haskell body
//! paths. It also provides the
//! [`crate::backends::skin::SkinProfile`] that converts between those typed
//! shapes and the universal fallback body's private carrier representation
//! ([`HaskellSkin`]).
//!
//! The conversion is driven through the shared
//! [`crate::backends::skin::convert`] driver, exactly as JS, Rust, and Swift
//! drive theirs: [`HaskellSkin`] supplies the per-leaf hooks
//! (`convert_product` / `convert_sum` / `convert_function` /
//! `convert_newtype` / `is_passthrough`) and the shared driver walks the
//! signature's type, flips direction across a function value's parameter
//! leg, and recurses into each slot.
//!
//! ## Monad-polymorphic boundary
//!
//! The host record is a **value** record of `m`-returning functions, and
//! every exported signature is `Monad m => …`. Structural family values are
//! plain pairs / `Either`; function results and first-class `forall`
//! application stages carry `m`, per
//! `specs/backends/haskell.md` § Host record contract.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::ast::{Kind, Newtype, PathSegment, Routed, Type, TypeParam};
use crate::backends::boundary_facade::{
    BoundaryHostBindingOrigin, BoundaryHostTypeBinding, PreparedBoundaryCallableSites,
};
pub(super) use crate::backends::skin::erase_scoped_type_vars;
use crate::backends::skin::{self, FfiDir, FunctionBoundaryAdapterPlan, SkinProfile};
use crate::backends::skin::{TypeReachAction, canonical_type_string, is_comptime_type_name};
use crate::pass::resolve::Package;

use super::emit::{EmitError, nest_kioprod, nest_kiosum};
use super::naming::{BoundaryId, BoundaryStep, HaskellName};

fn scoped_shape_binders(ty: &Type<Routed>, scope: &[TypeParam]) -> Vec<TypeParam> {
    let scope_names: BTreeSet<String> = scope.iter().map(|param| param.name.clone()).collect();
    let referenced = skin::type_vars_referenced_in_args(std::slice::from_ref(ty), &scope_names);
    scope
        .iter()
        .filter(|param| referenced.contains(&param.name))
        .cloned()
        .collect()
}

/// Extend a lexical type-parameter scope, replacing a shadowed outer binder.
/// The order remains the order of the binders that are actually in scope.
pub(crate) fn extend_type_scope(scope: &[TypeParam], param: &TypeParam) -> Vec<TypeParam> {
    let mut nested: Vec<TypeParam> = scope
        .iter()
        .filter(|outer| outer.name != param.name)
        .cloned()
        .collect();
    nested.push(param.clone());
    nested
}

#[cfg(test)]
mod scoped_shape_tests {
    use std::collections::{BTreeMap, BTreeSet};

    use crate::ast::Type;
    use crate::span::Span;

    use super::NewtypeResolution;

    #[test]
    fn every_qualified_newtype_path_is_direct() {
        let resolution = NewtypeResolution {
            scopes: BTreeMap::new(),
            exact_keys: BTreeSet::from(["a.Thing".to_owned(), "a/b.Thing".to_owned()]),
        };
        let canonical = Type::synth_path(
            vec!["a".to_owned(), "b".to_owned(), "Thing".to_owned()],
            Vec::new(),
            Span::new(0, 0),
        );
        let one_segment = Type::synth_path(
            vec!["a".to_owned(), "Thing".to_owned()],
            Vec::new(),
            Span::new(0, 0),
        );

        assert_eq!(
            resolution.key_of(&canonical, Some("caller")).as_deref(),
            Some("a/b.Thing"),
        );
        assert_eq!(
            resolution.key_of(&one_segment, Some("caller")).as_deref(),
            Some("a.Thing"),
        );
    }
}

pub(super) fn haskell_kind(kind: &Kind, standard_names: &super::naming::StandardNames) -> String {
    let star = format!("{}.Type", standard_names.data_kind);
    match kind {
        Kind::Star => star,
        Kind::Arrow(domain, codomain) => {
            let domain = haskell_kind(domain, standard_names);
            let domain = if domain == star {
                domain
            } else {
                format!("({domain})")
            };
            format!("{domain} -> {}", haskell_kind(codomain, standard_names))
        }
    }
}

pub(super) fn kinded_haskell_binder(
    param: &TypeParam,
    standard_names: &super::naming::StandardNames,
) -> String {
    format!(
        "({} :: {})",
        haskell_type_var(&param.name),
        haskell_kind(&param.effective_kind(), standard_names)
    )
}

/// One exact Kio `host type` binding in the generated Haskell marker
/// class. The associated family name encodes the declaring module as well
/// as the leaf, so two modules may declare the same leaf without sharing a
/// host-side equation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HaskellHostType {
    pub module_path: String,
    pub source_name: String,
    pub assoc_name: String,
    pub type_params: Vec<crate::ast::TypeParam>,
    pub role: Option<crate::ast::Role>,
    pub origin: BoundaryHostBindingOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactHostConstraint {
    Integral,
    Fractional,
    String,
    Boolean,
}

/// The exact Haskell type selected by one Kio syntax site and the ordinary
/// Haskell constraint required to spell that site at the selected type.
pub(crate) struct ExactHostSyntax {
    pub rendered: String,
    pub constraint: String,
}

/// Exact, module-aware resolution for Haskell host-type references.
///
/// Routed qualified paths already carry their canonical module identity. A
/// bare reference resolves only against the referring module's own
/// declarations and selective imports. Every successful lookup returns the
/// declaration's full `(module, leaf)` identity.
#[derive(Debug, Default)]
pub(crate) struct HaskellHostTypes {
    by_key: BTreeMap<String, HaskellHostType>,
    scopes: BTreeMap<String, BTreeMap<String, String>>,
}

impl HaskellHostTypes {
    pub(crate) fn build(
        package: &Package<Routed>,
        prepared: &PreparedBoundaryCallableSites,
        names: &super::naming::StructuralNames,
    ) -> Self {
        let mut by_key = BTreeMap::new();
        for binding in prepared.host_bindings() {
            let module_path = binding.name().module_segments().join("/");
            let source_name = binding.name().name();
            let key = host_type_key(&module_path, source_name);
            by_key.insert(
                key,
                HaskellHostType {
                    module_path: module_path.clone(),
                    source_name: source_name.to_owned(),
                    assoc_name: names.host_assoc_name(&module_path, source_name),
                    type_params: binding
                        .type_params()
                        .iter()
                        .map(|param| TypeParam {
                            name: param.name().to_owned(),
                            span: crate::span::Span::new(0, 0),
                            kind: Some(param.kind().clone()),
                        })
                        .collect(),
                    role: match binding.binding() {
                        BoundaryHostTypeBinding::Role(role) => Some(role),
                        BoundaryHostTypeBinding::Roleless => None,
                    },
                    origin: binding.origin(),
                },
            );
        }

        let mut scopes = BTreeMap::new();
        for (path, entry) in package.modules() {
            let mut scope = BTreeMap::new();
            for binding in by_key.values().filter(|b| b.module_path == path) {
                scope.insert(binding.source_name.clone(), binding.module_path.clone());
            }
            for import_decl in &entry.module.imports {
                if let crate::ast::ImportKind::Selective { items, from } = &import_decl.kind {
                    let from_path = from
                        .segments
                        .iter()
                        .map(|segment| segment.as_str())
                        .collect::<Vec<_>>()
                        .join("/");
                    for leaf in items.iter().filter_map(crate::ast::ImportItem::as_name) {
                        if by_key.contains_key(&host_type_key(&from_path, leaf))
                            && !scope.contains_key(leaf)
                        {
                            scope.insert(leaf.to_owned(), from_path.clone());
                        }
                    }
                }
            }
            scopes.insert(path.to_owned(), scope);
        }

        HaskellHostTypes { by_key, scopes }
    }

    pub(crate) fn declarations(&self) -> impl Iterator<Item = &HaskellHostType> {
        self.by_key.values()
    }

    pub(crate) fn resolve(
        &self,
        ty: &Type<Routed>,
        referring_module: Option<&str>,
    ) -> Option<&HaskellHostType> {
        let Type::Path { segments, .. } = ty else {
            return None;
        };
        let (leaf, prefix) = segments.split_last()?;
        let key = if prefix.is_empty() {
            let module = referring_module?;
            let home = self
                .scopes
                .get(module)
                .and_then(|scope| scope.get(leaf.as_str()))?;
            host_type_key(home, leaf.as_str())
        } else {
            let module = prefix
                .iter()
                .map(|segment| segment.as_str().to_owned())
                .collect::<Vec<_>>()
                .join("/");
            host_type_key(&module, leaf.as_str())
        };
        self.by_key.get(&key)
    }
}

fn host_type_key(module_path: &str, leaf: &str) -> String {
    format!("{module_path}\0{leaf}")
}

#[cfg(test)]
mod host_assoc_name_tests {
    use std::collections::BTreeMap;

    use crate::ast::{Routed, Type};
    use crate::span::Span;

    use crate::backends::boundary_facade::BoundaryHostBindingOrigin;

    use super::{HaskellHostType, HaskellHostTypes, host_type_key};
    use crate::backends::haskell::naming::StructuralNames;

    #[test]
    fn source_type_variables_keep_word_boundaries_and_affixes() {
        assert_eq!(super::haskell_type_var("Item_type"), "t_itemType");
        assert_eq!(super::haskell_type_var("_Item_type__"), "t__itemType__");
        assert_ne!(
            super::haskell_type_var("Item_type"),
            super::haskell_type_var("Itemtype")
        );
    }

    #[test]
    fn exact_identity_survives_readable_suffix_collisions() {
        let names = StructuralNames::new("Pkg");
        let slash = names.host_assoc_name("a/b", "Thing");
        let underscores = names.host_assoc_name("a__b", "Thing");

        assert_ne!(slash, underscores);
        assert_eq!(slash, "HostType__a__b__Thing");
        assert!(underscores.ends_with("__aB__Thing"));
    }

    #[test]
    fn canonical_multi_segment_host_path_resolves_exactly() {
        let canonical = HaskellHostType {
            module_path: "a/b".to_owned(),
            source_name: "Thing".to_owned(),
            assoc_name: StructuralNames::new("Pkg").host_assoc_name("a/b", "Thing"),
            type_params: Vec::new(),
            role: None,
            origin: BoundaryHostBindingOrigin::Live,
        };
        let decoy = HaskellHostType {
            module_path: "x/b".to_owned(),
            source_name: "Thing".to_owned(),
            assoc_name: StructuralNames::new("Pkg").host_assoc_name("x/b", "Thing"),
            type_params: Vec::new(),
            role: None,
            origin: BoundaryHostBindingOrigin::Live,
        };
        let mut by_key = BTreeMap::new();
        by_key.insert(host_type_key("a/b", "Thing"), canonical);
        by_key.insert(host_type_key("x/b", "Thing"), decoy);
        let registry = HaskellHostTypes {
            by_key,
            scopes: BTreeMap::new(),
        };
        let ty: Type<Routed> = Type::synth_path(
            vec!["a".to_owned(), "b".to_owned(), "Thing".to_owned()],
            Vec::new(),
            Span::new(0, 0),
        );

        let resolved = registry.resolve(&ty, Some("caller")).unwrap();
        assert_eq!(resolved.module_path, "a/b");
    }

    #[test]
    fn canonical_single_segment_host_path_resolves_exactly() {
        let direct = HaskellHostType {
            module_path: "a".to_owned(),
            source_name: "Thing".to_owned(),
            assoc_name: StructuralNames::new("Pkg").host_assoc_name("a", "Thing"),
            type_params: Vec::new(),
            role: None,
            origin: BoundaryHostBindingOrigin::Live,
        };
        let aliased = HaskellHostType {
            module_path: "x".to_owned(),
            source_name: "Thing".to_owned(),
            assoc_name: StructuralNames::new("Pkg").host_assoc_name("x", "Thing"),
            type_params: Vec::new(),
            role: None,
            origin: BoundaryHostBindingOrigin::Live,
        };
        let registry = HaskellHostTypes {
            by_key: BTreeMap::from([
                (host_type_key("a", "Thing"), direct),
                (host_type_key("x", "Thing"), aliased),
            ]),
            scopes: BTreeMap::new(),
        };
        let ty: Type<Routed> = Type::synth_path(
            vec!["a".to_owned(), "Thing".to_owned()],
            Vec::new(),
            Span::new(0, 0),
        );

        let resolved = registry.resolve(&ty, Some("caller")).unwrap();
        assert_eq!(resolved.module_path, "a");
    }

    #[test]
    fn adding_same_leaf_host_type_cannot_change_moduleless_bare_lookup() {
        let names = StructuralNames::new("Pkg");
        let left = HaskellHostType {
            module_path: "left".to_owned(),
            source_name: "Shared".to_owned(),
            assoc_name: names.host_assoc_name("left", "Shared"),
            type_params: Vec::new(),
            role: None,
            origin: BoundaryHostBindingOrigin::Live,
        };
        let right = HaskellHostType {
            module_path: "right".to_owned(),
            source_name: "Shared".to_owned(),
            assoc_name: names.host_assoc_name("right", "Shared"),
            type_params: Vec::new(),
            role: None,
            origin: BoundaryHostBindingOrigin::Live,
        };
        let single = HaskellHostTypes {
            by_key: BTreeMap::from([(host_type_key("left", "Shared"), left.clone())]),
            scopes: BTreeMap::new(),
        };
        let homonymous = HaskellHostTypes {
            by_key: BTreeMap::from([
                (host_type_key("left", "Shared"), left),
                (host_type_key("right", "Shared"), right),
            ]),
            scopes: BTreeMap::new(),
        };
        let ty: Type<Routed> =
            Type::synth_path(vec!["Shared".to_owned()], Vec::new(), Span::new(0, 0));

        assert!(single.resolve(&ty, None).is_none());
        assert!(homonymous.resolve(&ty, None).is_none());
    }
}

/// The Haskell boundary-shape renderer.
pub struct HaskellShapes<'p> {
    package: &'p Package<Routed>,
    host_types: HaskellHostTypes,
    native_types: super::native::NativeTypes<'p>,
    /// The module-scoped newtype resolver. Qualified paths carry canonical
    /// owners; bare leaves resolve only in their referring module.
    resolution: NewtypeResolution,
    structural_names: super::naming::StructuralNames,
    runtime_names: super::naming::RuntimeNames,
    standard_names: super::naming::StandardNames,
}

impl<'p> HaskellShapes<'p> {
    /// Build a renderer over the package's exact host declarations.
    #[cfg(all(test, feature = "surface"))]
    pub(crate) fn new(package: &'p Package<Routed>, names: &super::emit::HaskellNames) -> Self {
        let prepared = PreparedBoundaryCallableSites::collect_live(package)
            .expect("prepare Haskell test facade");
        Self::new_with_prepared(package, names, &prepared)
    }

    /// Build a renderer from the package-complete shared binding transaction.
    pub(crate) fn new_with_prepared(
        package: &'p Package<Routed>,
        names: &super::emit::HaskellNames,
        prepared: &PreparedBoundaryCallableSites,
    ) -> Self {
        HaskellShapes {
            package,
            host_types: HaskellHostTypes::build(package, prepared, &names.structural_names),
            native_types: super::native::NativeTypes::build(
                package,
                prepared,
                &names.runtime_names,
                &names.standard_names,
            ),
            resolution: NewtypeResolution::build(package),
            structural_names: names.structural_names.clone(),
            runtime_names: names.runtime_names.clone(),
            standard_names: names.standard_names.clone(),
        }
    }

    pub(crate) fn host_types(&self) -> &HaskellHostTypes {
        &self.host_types
    }

    /// The shared module-scoped newtype resolver (also used by the native
    /// body so both paths agree on which module a bare leaf names).
    pub(crate) fn resolution(&self) -> &NewtypeResolution {
        &self.resolution
    }

    pub(crate) fn has_nominal_boundary_carrier(
        &self,
        ty: &Type<Routed>,
        referring_module: Option<&str>,
    ) -> bool {
        self.native_types
            .has_nominal_boundary_carrier(ty, referring_module)
    }

    pub(crate) fn newtype_has_nominal_boundary_carrier(
        &self,
        module_path: &str,
        name: &str,
    ) -> bool {
        self.native_types
            .has_nominal_boundary_carrier_key(&newtype_qual_key(module_path, name))
    }

    pub(crate) fn structural_names(&self) -> &super::naming::StructuralNames {
        &self.structural_names
    }

    pub(crate) fn runtime_names(&self) -> &super::naming::RuntimeNames {
        &self.runtime_names
    }

    pub(crate) fn standard_names(&self) -> &super::naming::StandardNames {
        &self.standard_names
    }

    /// The boundary Haskell type, resolving a bare newtype leaf in the
    /// referring module's scope (so a same-leaf newtype in another module
    /// never supplies the wrong payload). The module-aware entry the native
    /// export-wrapper / host-record emitters use.
    pub fn boundary_haskell_type_in(
        &self,
        ty: &Type<Routed>,
        module: Option<&str>,
    ) -> Result<String, EmitError> {
        self.haskell_type_of(ty, &mut BTreeSet::new(), module)
    }

    pub(crate) fn boundary_haskell_type_scoped_in(
        &self,
        ty: &Type<Routed>,
        scope: &[TypeParam],
        module: Option<&str>,
    ) -> Result<String, EmitError> {
        self.haskell_type_of_scoped(ty, &mut BTreeSet::new(), module, scope)
    }

    pub(crate) fn boundary_alias_haskell_type_in(
        &self,
        boundary: &BoundaryId,
        ty: &Type<Routed>,
        scope: &[TypeParam],
        module: Option<&str>,
    ) -> Result<String, EmitError> {
        match ty {
            Type::Product { .. } | Type::Sum { .. } => Ok(self.applied_alias(
                &self
                    .structural_names
                    .render(&HaskellName::BoundaryAlias(boundary.clone())),
                ty,
                scope,
            )),
            Type::Function {
                param,
                ret,
                abi_arity,
                ..
            } => {
                let mut rendered = String::new();
                for (index, param) in Type::right_spine_take(param, *abi_arity)
                    .into_iter()
                    .enumerate()
                {
                    let param = self.boundary_alias_haskell_type_in(
                        &boundary.nested(BoundaryStep::CallbackArg(
                            index.try_into().expect("callback argument index fits u32"),
                        )),
                        param,
                        scope,
                        module,
                    )?;
                    rendered.push_str(&paren_arg(&param));
                    rendered.push_str(" -> ");
                }
                let ret = self.boundary_alias_haskell_type_in(
                    &boundary.nested(BoundaryStep::CallbackRet),
                    ret,
                    scope,
                    module,
                )?;
                rendered.push_str(&format!("m {}", paren_arg(&ret)));
                Ok(format!("({rendered})"))
            }
            Type::Forall { param, body, .. } => {
                let nested = extend_type_scope(scope, param);
                let body = self.boundary_alias_haskell_type_in(boundary, body, &nested, module)?;
                Ok(format!(
                    "forall {}. m {}",
                    kinded_haskell_binder(param, &self.standard_names),
                    paren_arg(&body)
                ))
            }
            Type::Path { .. } => {
                if let Type::Path { segments, args, .. } = ty
                    && let [name] = segments.as_slice()
                    && scope.iter().any(|param| param.name == name.as_str())
                {
                    let mut rendered = haskell_type_var(name.as_str());
                    for (index, arg) in args.iter().enumerate() {
                        let arg = self.boundary_alias_haskell_type_in(
                            &boundary.nested(BoundaryStep::App(
                                index.try_into().expect("type argument index fits u32"),
                            )),
                            arg,
                            scope,
                            module,
                        )?;
                        rendered.push(' ');
                        rendered.push_str(&paren_arg(&arg));
                    }
                    return Ok(rendered);
                }
                if let Some((body, owner)) = self.boundary_path_expansion_in(ty, scope, module) {
                    return self.boundary_alias_haskell_type_in(
                        boundary,
                        &body,
                        scope,
                        Some(&owner),
                    );
                }
                self.boundary_haskell_type_scoped_in(ty, scope, module)
            }
            Type::Unit { .. } | Type::Bottom { .. } => {
                self.boundary_haskell_type_scoped_in(ty, scope, module)
            }
            Type::LabelSugar { ext, .. } => match *ext {},
            Type::Infer { ext, .. } => match *ext {},
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    /// Expand a path whose public boundary representation is its body while
    /// preserving the declaration module used to resolve nested bare names.
    pub(crate) fn boundary_path_expansion_in(
        &self,
        ty: &Type<Routed>,
        scope: &[TypeParam],
        module: Option<&str>,
    ) -> Option<(Type<Routed>, String)> {
        if let Some((declaration, owner)) =
            resolve_newtype_in(ty, self.package, &self.resolution, module)
        {
            if self.has_nominal_boundary_carrier(ty, module) {
                return None;
            }
            let bound = scope.iter().map(|param| param.name.clone()).collect();
            let payload = instantiate_newtype_payload_in(
                declaration,
                ty,
                self.package,
                &owner,
                module,
                &bound,
            )?;
            return Some((payload, owner));
        }
        None
    }

    fn applied_alias(&self, alias: &str, ty: &Type<Routed>, scope: &[TypeParam]) -> String {
        let mut rendered = format!("{alias} h m");
        for binder in scoped_shape_binders(ty, scope) {
            rendered.push(' ');
            rendered.push_str(&haskell_type_var(&binder.name));
        }
        rendered
    }

    pub(crate) fn scoped_binders(&self, ty: &Type<Routed>, scope: &[TypeParam]) -> Vec<TypeParam> {
        scoped_shape_binders(ty, scope)
    }

    /// Whether `ty` names one exact host declaration in `module`'s scope.
    pub(crate) fn is_host_type_in(&self, ty: &Type<Routed>, module: Option<&str>) -> bool {
        self.host_types.resolve(ty, module).is_some()
    }

    /// The kind of a path head as it appears in one lexical scope.
    /// Boundary pattern helpers use this to replace a potentially
    /// non-injective host-associated family with a representation variable
    /// of the same kind.
    pub(crate) fn path_kind_scoped_in(
        &self,
        ty: &Type<Routed>,
        scope: &[TypeParam],
        module: Option<&str>,
    ) -> Option<Kind> {
        fn constructor_kind(params: &[TypeParam]) -> Kind {
            params.iter().rev().fold(Kind::Star, |result, param| {
                Kind::Arrow(Box::new(param.effective_kind()), Box::new(result))
            })
        }

        let Type::Path { segments, args, .. } = ty else {
            return None;
        };
        if let [name] = segments.as_slice()
            && let Some(param) = scope.iter().rev().find(|param| param.name == name.as_str())
        {
            return Some(param.effective_kind());
        }
        if let Some(binding) = self.host_types.resolve(ty, module) {
            return Some(constructor_kind(&binding.type_params));
        }
        if let Some((declaration, _)) =
            resolve_newtype_in(ty, self.package, &self.resolution, module)
        {
            return Some(constructor_kind(&declaration.type_params));
        }

        let (leaf, prefix) = segments.split_last()?;
        let exact_module = if prefix.is_empty() {
            module.map(str::to_owned)
        } else {
            Some(
                prefix
                    .iter()
                    .map(PathSegment::as_str)
                    .collect::<Vec<_>>()
                    .join("/"),
            )
        };
        if let Some(exact_module) = exact_module
            && let Some(entry) = self.package.module(&exact_module)
        {
            for item in &entry.module.items {
                let mut found = None;
                crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                    if let Some(alias) = declaration.type_alias()
                        && alias.name == leaf.as_str()
                    {
                        found = Some(alias);
                    }
                });
                if let Some(alias) = found {
                    return Some(constructor_kind(&alias.type_params));
                }
            }
        }
        Some(Kind::arrow_chain(args.len()))
    }

    /// Render an exact host-type application, if `ty` is one, using the
    /// associated family selected by the host marker. Non-host paths return
    /// `None`; all recursive argument rendering follows the ordinary boundary
    /// type path.
    pub(crate) fn exact_host_haskell_type_in(
        &self,
        ty: &Type<Routed>,
        module: Option<&str>,
    ) -> Result<Option<String>, EmitError> {
        if !self.is_host_type_in(ty, module) {
            return Ok(None);
        }
        self.haskell_type_of(ty, &mut BTreeSet::new(), module)
            .map(Some)
    }

    /// Render the exact host type selected by an annotated literal or a
    /// conditional condition, following transparent aliases but never
    /// consulting role metadata. Returns both the rendered type and the
    /// ordinary Haskell constraint induced by that syntax site; the native
    /// call-graph analysis attaches it only to functions that can reach it.
    pub(crate) fn exact_host_syntax(
        &self,
        ty: &Type<Routed>,
        module: Option<&str>,
        constraint: ExactHostConstraint,
    ) -> Result<ExactHostSyntax, EmitError> {
        if self.host_types.resolve(ty, module).is_none() {
            return Err(EmitError::unsupported(format!(
                "Haskell emitter: exact host type unavailable for annotated syntax `{}`",
                canonical_type_string(ty)
            )));
        }
        let rendered = self
            .exact_host_haskell_type_in(ty, module)?
            .ok_or_else(|| {
                EmitError::unsupported(format!(
                    "Haskell emitter: annotated syntax type `{}` does not resolve to an exact host declaration",
                    canonical_type_string(ty)
                ))
            })?;
        let constraint = match constraint {
            ExactHostConstraint::Integral => format!("Num ({rendered})"),
            ExactHostConstraint::Fractional => format!("Fractional ({rendered})"),
            ExactHostConstraint::String => {
                format!("{}.IsString ({rendered})", self.standard_names.data_string)
            }
            ExactHostConstraint::Boolean => format!("({rendered} ~ Bool)"),
        };
        Ok(ExactHostSyntax {
            rendered,
            constraint,
        })
    }

    fn haskell_type_of(
        &self,
        ty: &Type<Routed>,
        seen_newtypes: &mut BTreeSet<String>,
        module: Option<&str>,
    ) -> Result<String, EmitError> {
        self.haskell_type_of_scoped(ty, seen_newtypes, module, &[])
    }

    fn haskell_type_of_scoped(
        &self,
        ty: &Type<Routed>,
        seen_newtypes: &mut BTreeSet<String>,
        module: Option<&str>,
        scope: &[TypeParam],
    ) -> Result<String, EmitError> {
        if let Type::Path { segments, args, .. } = ty
            && let [name] = segments.as_slice()
            && scope.iter().any(|param| param.name == name.as_str())
        {
            let mut rendered = haskell_type_var(name.as_str());
            for arg in args {
                rendered.push(' ');
                rendered.push_str(&paren_arg(&self.haskell_type_of_scoped(
                    arg,
                    seen_newtypes,
                    module,
                    scope,
                )?));
            }
            return Ok(rendered);
        }
        if let Type::Path { args, .. } = ty
            && let Some((declaration, owner)) =
                resolve_newtype_in(ty, self.package, &self.resolution, module)
            && self.has_nominal_boundary_carrier(ty, module)
        {
            if args.len() > declaration.type_params.len() {
                return Err(EmitError::unsupported(format!(
                    "Haskell emitter: nominal newtype `{owner}/{}` reached code generation with {} argument(s), expected at most {}",
                    declaration.name,
                    args.len(),
                    declaration.type_params.len(),
                )));
            }
            let native_scope = scope
                .iter()
                .map(|param| param.name.as_str())
                .collect::<std::collections::HashSet<_>>();
            return Ok(self.native_types.render_type_scoped(
                ty,
                &native_scope,
                module,
                &self.host_types,
            ));
        }
        match ty {
            Type::Unit { .. } => Ok("()".to_owned()),
            Type::Bottom { .. } => Ok(format!("{}.Void", self.standard_names.data_void)),
            Type::Product { .. } => self.render_structural_family(
                self.structural_names.product_family(),
                Type::right_spine_product(ty),
                seen_newtypes,
                module,
                scope,
            ),
            Type::Sum { .. } => self.render_structural_family(
                self.structural_names.sum_family(),
                Type::right_spine_sum(ty),
                seen_newtypes,
                module,
                scope,
            ),
            Type::Function {
                param,
                ret,
                abi_arity,
                ..
            } => {
                let param_slots = Type::right_spine_take(param, *abi_arity);
                let mut parts = Vec::with_capacity(param_slots.len());
                for p in &param_slots {
                    parts.push(self.haskell_type_of_scoped(p, seen_newtypes, module, scope)?);
                }
                let ret_ty = self.haskell_type_of_scoped(ret, seen_newtypes, module, scope)?;
                // A Haskell function is curried: `a -> b -> m ret`.
                let mut out = String::new();
                for p in &parts {
                    out.push_str(&paren_arg(p));
                    out.push_str(" -> ");
                }
                out.push_str(&format!("m {}", paren_arg(&ret_ty)));
                Ok(format!("({out})"))
            }
            Type::Forall { param, body, .. } => {
                let nested = extend_type_scope(scope, param);
                let body = self.haskell_type_of_scoped(body, seen_newtypes, module, &nested)?;
                Ok(format!(
                    "forall {}. m {}",
                    kinded_haskell_binder(param, &self.standard_names),
                    paren_arg(&body)
                ))
            }
            Type::Path { args, .. } => {
                if let Some(binding) = self.host_types.resolve(ty, module) {
                    if args.len() != binding.type_params.len() {
                        return Err(EmitError::unsupported(format!(
                            "Haskell emitter: host type `{}/{}` reached code generation with {} argument(s), expected {}",
                            binding.module_path,
                            binding.source_name,
                            args.len(),
                            binding.type_params.len()
                        )));
                    }
                    let mut rendered = format!("{} h", binding.assoc_name);
                    for arg in args {
                        rendered.push(' ');
                        rendered.push_str(&paren_arg(&self.haskell_type_of_scoped(
                            arg,
                            seen_newtypes,
                            module,
                            scope,
                        )?));
                    }
                    return Ok(rendered);
                }
                match resolve_newtype_in(ty, self.package, &self.resolution, module) {
                    Some((d, module_path)) => {
                        if self.has_nominal_boundary_carrier(ty, module) {
                            return Ok(format!("({} h m)", self.runtime_names.opaque));
                        }
                        let key = format!("{module_path}.{}", d.name);
                        if !seen_newtypes.insert(key.clone()) {
                            return Ok(format!("({} h m)", self.runtime_names.opaque));
                        }
                        let bound = scope.iter().map(|param| param.name.clone()).collect();
                        let Some(payload) = instantiate_newtype_payload_in(
                            d,
                            ty,
                            self.package,
                            &module_path,
                            module,
                            &bound,
                        ) else {
                            seen_newtypes.remove(&key);
                            let native_scope = scope
                                .iter()
                                .map(|param| param.name.as_str())
                                .collect::<std::collections::HashSet<_>>();
                            return Ok(self.native_types.render_type_scoped(
                                ty,
                                &native_scope,
                                module,
                                &self.host_types,
                            ));
                        };
                        // The payload's bare leaves are written in the
                        // newtype's declaring module, so resolution continues
                        // against `module_path`, not the use site.
                        let t = self.haskell_type_of_scoped(
                            &payload,
                            seen_newtypes,
                            Some(&module_path),
                            scope,
                        )?;
                        seen_newtypes.remove(&key);
                        Ok(t)
                    }
                    None => Ok(format!("({} h m)", self.runtime_names.opaque)),
                }
            }
            Type::LabelSugar { ext, .. } => match *ext {},
            Type::Infer { ext, .. } => match *ext {},
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    fn render_structural_family(
        &self,
        family: &str,
        slots: Vec<&Type<Routed>>,
        seen_newtypes: &mut BTreeSet<String>,
        module: Option<&str>,
        scope: &[TypeParam],
    ) -> Result<String, EmitError> {
        let rendered = slots
            .into_iter()
            .map(|slot| {
                self.haskell_type_of_scoped(slot, seen_newtypes, module, scope)
                    .map(|slot| paren_arg(&slot))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(format!("{family} '[{}]", rendered.join(", ")))
    }

    /// `true` when a value of type `ty` crosses the boundary unchanged.
    /// Type variables retain the universal representation; recursive and
    /// existential nominal leaves retain their native carrier representation.
    /// Structural parents still recurse. Unit and bottom are not passthrough:
    /// the host boundary uses `()` / `Data.Void.Void`, while the universal body
    /// uses private unit / universal carriers, so those slots convert.
    pub fn is_passthrough(&self, ty: &Type<Routed>) -> bool {
        self.is_passthrough_inner(ty, &mut BTreeSet::new(), None, &[])
    }

    /// Module-aware passthrough test (a bare newtype leaf resolves in the
    /// referring module's scope), for the native body's boundary converter.
    pub fn is_passthrough_in(&self, ty: &Type<Routed>, module: Option<&str>) -> bool {
        self.is_passthrough_inner(ty, &mut BTreeSet::new(), module, &[])
    }

    pub(crate) fn is_passthrough_scoped_in(
        &self,
        ty: &Type<Routed>,
        scope: &[TypeParam],
        module: Option<&str>,
    ) -> bool {
        self.is_passthrough_inner(ty, &mut BTreeSet::new(), module, scope)
    }

    pub(crate) fn fallback_bridgeable_in(&self, ty: &Type<Routed>, module: Option<&str>) -> bool {
        self.fallback_bridgeable_inner(ty, &mut BTreeSet::new(), module)
    }

    fn fallback_bridgeable_inner(
        &self,
        ty: &Type<Routed>,
        seen: &mut BTreeSet<String>,
        module: Option<&str>,
    ) -> bool {
        if self.type_involves_comptime(ty, module, &mut BTreeSet::new(), &[]) {
            return false;
        }
        match ty {
            Type::Unit { .. } | Type::Bottom { .. } => true,
            Type::Forall { .. } => false,
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                self.fallback_bridgeable_inner(left, seen, module)
                    && self.fallback_bridgeable_inner(right, seen, module)
            }
            Type::Function {
                param,
                ret,
                abi_arity,
                ..
            } => {
                Type::right_spine_take(param, *abi_arity)
                    .into_iter()
                    .all(|param| self.fallback_bridgeable_inner(param, seen, module))
                    && self.fallback_bridgeable_inner(ret, seen, module)
            }
            Type::Path { .. } => {
                if self.host_types.resolve(ty, module).is_some() {
                    return false;
                }
                match resolve_newtype_in(ty, self.package, &self.resolution, module) {
                    Some((declaration, owner)) => {
                        if self.has_nominal_boundary_carrier(ty, module) {
                            return false;
                        }
                        let key = format!("{owner}.{}", declaration.name);
                        if !seen.insert(key.clone()) {
                            return false;
                        }
                        let Some(payload) = instantiate_newtype_payload_in(
                            declaration,
                            ty,
                            self.package,
                            &owner,
                            module,
                            &BTreeSet::new(),
                        ) else {
                            seen.remove(&key);
                            return false;
                        };
                        let bridgeable =
                            self.fallback_bridgeable_inner(&payload, seen, Some(&owner));
                        seen.remove(&key);
                        bridgeable
                    }
                    None => false,
                }
            }
            Type::LabelSugar { ext, .. } => match *ext {},
            Type::Infer { ext, .. } => match *ext {},
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    fn type_involves_comptime(
        &self,
        ty: &Type<Routed>,
        module: Option<&str>,
        visited: &mut BTreeSet<String>,
        scope: &[TypeParam],
    ) -> bool {
        let recurse = |ty: &Type<Routed>,
                       module: Option<&str>,
                       visited: &mut BTreeSet<String>,
                       scope: &[TypeParam]| {
            self.type_involves_comptime(ty, module, visited, scope)
        };
        match ty {
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                recurse(left, module, visited, scope) || recurse(right, module, visited, scope)
            }
            Type::Function { param, ret, .. } => {
                recurse(param, module, visited, scope) || recurse(ret, module, visited, scope)
            }
            Type::Forall { param, body, .. } => {
                let nested = extend_type_scope(scope, param);
                recurse(body, module, visited, &nested)
            }
            Type::Bottom { .. } => false,
            Type::Path { segments, args, .. } => {
                if let [name] = segments.as_slice()
                    && scope.iter().any(|param| param.name == name.as_str())
                {
                    return args.iter().any(|arg| recurse(arg, module, visited, scope));
                }
                if self.host_types.resolve(ty, module).is_some() {
                    return false;
                }
                if let Some((declaration, owner)) =
                    resolve_newtype_in(ty, self.package, &self.resolution, module)
                {
                    if self.has_nominal_boundary_carrier(ty, module) {
                        return false;
                    }
                    let key = format!("{owner}.{}", declaration.name);
                    if !visited.insert(key.clone()) {
                        return false;
                    }
                    let bound = scope.iter().map(|param| param.name.clone()).collect();
                    let result = instantiate_newtype_payload_in(
                        declaration,
                        ty,
                        self.package,
                        &owner,
                        module,
                        &bound,
                    )
                    .is_some_and(|payload| recurse(&payload, Some(&owner), visited, scope));
                    visited.remove(&key);
                    return result;
                }
                args.iter().any(|arg| recurse(arg, module, visited, scope))
                    || segments
                        .last()
                        .is_some_and(|last| is_comptime_type_name(last.as_str()))
            }
            Type::Unit { .. } => false,
            Type::LabelSugar { ext, .. } => match *ext {},
            Type::Infer { ext, .. } => match *ext {},
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    fn is_passthrough_inner(
        &self,
        ty: &Type<Routed>,
        seen: &mut BTreeSet<String>,
        module: Option<&str>,
        scope: &[TypeParam],
    ) -> bool {
        if let Type::Path { segments, .. } = ty
            && let [name] = segments.as_slice()
            && scope.iter().any(|param| param.name == name.as_str())
        {
            return true;
        }
        if self.has_nominal_boundary_carrier(ty, module) {
            return true;
        }
        match ty {
            // Unit and bottom have distinct public and universal-body
            // representations, so neither is passthrough.
            Type::Unit { .. } | Type::Bottom { .. } => false,
            Type::Product { .. } | Type::Sum { .. } => false,
            Type::Function { .. } => false,
            Type::Forall { param, body, .. } => {
                let nested = extend_type_scope(scope, param);
                self.is_passthrough_inner(body, seen, module, &nested)
            }
            Type::Path { .. } => {
                if self.host_types.resolve(ty, module).is_some() {
                    // A concrete host type has an exact associated-family
                    // representation and therefore is never an erased slot.
                    return false;
                }
                match resolve_newtype_in(ty, self.package, &self.resolution, module) {
                    None => true,
                    Some((d, module_path)) => {
                        let key = format!("{module_path}.{}", d.name);
                        if !seen.insert(key.clone()) {
                            return true;
                        }
                        let Some(payload) = instantiate_newtype_payload_in(
                            d,
                            ty,
                            self.package,
                            &module_path,
                            module,
                            &scope.iter().map(|param| param.name.clone()).collect(),
                        ) else {
                            seen.remove(&key);
                            return true;
                        };
                        let r =
                            self.is_passthrough_inner(&payload, seen, Some(&module_path), scope);
                        seen.remove(&key);
                        r
                    }
                }
            }
            Type::LabelSugar { ext, .. } => match *ext {},
            Type::Infer { ext, .. } => match *ext {},
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    /// Render the substitution-stable structural carriers shared by every
    /// boundary alias in this package.
    pub fn render_shape_decls(&self) -> String {
        let kind = &self.standard_names.data_kind;
        let void = &self.standard_names.data_void;
        format!(
            "\ntype family {} (xs :: [{kind}.Type]) :: {kind}.Type where\n  {} '[] = ()\n  {} '[x] = x\n  {} (x ': xs) = (x, {} xs)\n\ntype family {} (xs :: [{kind}.Type]) :: {kind}.Type where\n  {} '[] = {void}.Void\n  {} '[x] = x\n  {} (x ': xs) = Either x ({} xs)\n",
            self.structural_names.product_family(),
            self.structural_names.product_family(),
            self.structural_names.product_family(),
            self.structural_names.product_family(),
            self.structural_names.product_family(),
            self.structural_names.sum_family(),
            self.structural_names.sum_family(),
            self.structural_names.sum_family(),
            self.structural_names.sum_family(),
            self.structural_names.sum_family(),
        )
    }
}

/// The Haskell skin profile: the per-backend leaves of the boundary-wrapper
/// walk for Haskell's value representation (`Product` / `Sum` family
/// applications reduce to right-folded pairs / `Either` at the host boundary,
/// with patterns and selectors providing the presentation; the universal
/// carrier remains internal).
pub struct HaskellSkin<'a> {
    pub shapes: &'a HaskellShapes<'a>,
    /// The module the converted item is declared in, so a bare newtype leaf
    /// resolves to the right same-leaf newtype. At a package-surface position,
    /// `None` permits only already-qualified identities to resolve.
    pub module: Option<&'a str>,
}

impl SkinProfile for HaskellSkin<'_> {
    type Err = EmitError;

    /// Override the shared driver's atomic leaves whose public and universal
    /// representations differ. Exact host types are handled natively and never
    /// enter the universal floor.
    fn convert(&self, ty: &Type<Routed>, expr: &str, dir: FfiDir) -> Result<String, EmitError> {
        let runtime = self.shapes.runtime_names();
        if matches!(ty, Type::Unit { .. }) {
            return Ok(match dir {
                // Native `()` -> internal private unit (the `()` value is run
                // for effects upstream; here we just produce the rep).
                FfiDir::In => format!("(({expr}) `seq` {})", runtime.unit),
                // Internal private unit -> native `()`.
                FfiDir::Out => format!("(({expr}) `seq` ())"),
            });
        }
        if matches!(ty, Type::Bottom { .. }) {
            return Ok(match dir {
                FfiDir::In => format!(
                    "({}.absurd ({expr}))",
                    self.shapes.standard_names().data_void
                ),
                FfiDir::Out => {
                    format!("(({expr}) `seq` error \"kio: bottom boundary reached\")")
                }
            });
        }
        skin::convert(self, ty, expr, dir)
    }

    fn is_passthrough(&self, ty: &Type<Routed>) -> bool {
        self.shapes.is_passthrough_in(ty, self.module)
    }

    fn convert_newtype(
        &self,
        ty: &Type<Routed>,
        expr: &str,
        dir: FfiDir,
    ) -> Result<Option<String>, EmitError> {
        if let Some(binding) = self.shapes.host_types.resolve(ty, self.module) {
            return Err(EmitError::unsupported(format!(
                "Haskell universal body cannot erase exact host type `{}/{}`; the native body must render this package",
                binding.module_path, binding.source_name
            )));
        }
        // Haskell carries a newtype transparently (the newtype value shares
        // its payload's private-carrier rep); the payload IS the boundary shape.
        // Resolve in the converted item's module so a same-leaf newtype in
        // another module never supplies the wrong payload.
        let Some((d, owner)) = resolve_newtype_in(
            ty,
            self.shapes.package,
            self.shapes.resolution(),
            self.module,
        ) else {
            return Ok(None);
        };
        let Some(payload) = instantiate_newtype_payload_in(
            d,
            ty,
            self.shapes.package,
            &owner,
            self.module,
            &BTreeSet::new(),
        ) else {
            return Ok(None);
        };
        Ok(Some(self.convert(&payload, expr, dir)?))
    }

    fn convert_product(
        &self,
        _ty: &Type<Routed>,
        slots: &[&Type<Routed>],
        expr: &str,
        dir: FfiDir,
    ) -> Result<String, EmitError> {
        match dir {
            // Native right-nested pair -> internal product.
            FfiDir::In => {
                let mut parts = Vec::with_capacity(slots.len());
                for (index, slot) in slots.iter().enumerate() {
                    let access = super::native::tuple_proj(expr, index, slots.len());
                    parts.push(self.convert(slot, &access, FfiDir::In)?);
                }
                Ok(nest_kioprod(self.shapes.runtime_names(), &parts))
            }
            // Internal product -> native right-nested pair.
            FfiDir::Out => {
                let n = slots.len();
                let mut parts = Vec::with_capacity(n);
                for (i, slot) in slots.iter().enumerate() {
                    let access =
                        format!("({} {i} {n} ({expr}))", self.shapes.runtime_names().project);
                    parts.push(self.convert(slot, &access, FfiDir::Out)?);
                }
                Ok(super::native::nest_tuple_value(&parts))
            }
        }
    }

    fn convert_sum(
        &self,
        _ty: &Type<Routed>,
        slots: &[&Type<Routed>],
        expr: &str,
        dir: FfiDir,
    ) -> Result<String, EmitError> {
        let n = slots.len();
        match dir {
            // Native `Either` -> internal nested-binary sum.
            FfiDir::In => {
                let mut arms = String::new();
                for (k, slot) in slots.iter().enumerate() {
                    let payload = self.convert(slot, "__c", FfiDir::In)?;
                    let injected =
                        nest_kiosum(self.shapes.runtime_names(), k, n, &format!("({payload})"));
                    arms.push_str(&format!(
                        "{} -> {injected}; ",
                        super::native::either_pattern(k, n, "__c")
                    ));
                }
                Ok(format!("(case ({expr}) of {{ {arms} }})"))
            }
            // Internal nested-binary sum -> native `Either`. Peel the tag-1
            // remainder per arm: a non-final arm tests tag 0 and reads its
            // payload, the final arm is the fully-peeled remainder.
            FfiDir::Out => self.convert_sum_out(slots, expr, 0),
        }
    }

    fn convert_function(
        &self,
        param: &Type<Routed>,
        ret: &Type<Routed>,
        abi_arity: usize,
        expr: &str,
        dir: FfiDir,
    ) -> Result<String, EmitError> {
        let plan = FunctionBoundaryAdapterPlan::new(param, abi_arity);
        let boundary_tys = plan.boundary_slots();
        let boundary_arity = plan.boundary_arity();
        let internal_arity = plan.internal_arity();
        // The native boundary function is curried `a -> b -> m ret`; the
        // internal closure uses the private function constructor and takes the stored ABI partition as
        // one product param (or a single value when unary), returning
        // a monadic private carrier. The shared plan is the sole authority for
        // repartitioning those stored arguments into public facade slots.
        let params: Vec<String> = (0..boundary_arity).map(|i| format!("__fp{i}")).collect();
        match dir {
            // Internal function -> native boundary function. Produce a native
            // curried `\a b -> m ret_native`: convert each native arg `In` to
            // the internal rep, repartition the canonical slots into the
            // stored callable ABI, build its one Kio value-group product,
            // call the internal function, and convert the internal result
            // `Out` to the native boundary return.
            FfiDir::Out => {
                let boundary_values = boundary_tys
                    .iter()
                    .zip(&params)
                    .map(|(ty, param)| self.convert(ty, param, FfiDir::In))
                    .collect::<Result<Vec<_>, _>>()?;
                let internal_values =
                    plan.boundary_to_internal_args(&boundary_values, |left, right| {
                        format!(
                            "({} [{left}, {right}])",
                            self.shapes.runtime_names().product
                        )
                    });
                let arg_internal = nest_kioprod(self.shapes.runtime_names(), &internal_values);
                let ret_conv = self.convert(ret, "__fr", FfiDir::Out)?;
                if boundary_arity == 0 {
                    return Ok(format!(
                        "({} ({expr}) {} >>= \\__fr -> pure ({ret_conv}))",
                        self.shapes.runtime_names().call_function,
                        self.shapes.runtime_names().unit,
                    ));
                }
                let lam_params = if params.is_empty() {
                    "()".to_owned()
                } else {
                    params.join(" ")
                };
                Ok(format!(
                    "(\\{lam_params} -> {} ({expr}) ({arg_internal}) >>= \\__fr -> pure ({ret_conv}))",
                    self.shapes.runtime_names().call_function,
                ))
            }
            // Native boundary function -> internal function. It takes
            // the stored product arg, reconstructs the canonical facade
            // slots, converts each `Out` to its native boundary type, calls
            // the native boundary fn, and converts its result `In` back to
            // the internal rep.
            FfiDir::In => {
                let arg = "__fa";
                let internal_values = match internal_arity {
                    0 => Vec::new(),
                    1 => vec![arg.to_owned()],
                    arity => (0..arity)
                        .map(|index| {
                            format!(
                                "({} {index} {arity} {arg})",
                                self.shapes.runtime_names().project
                            )
                        })
                        .collect(),
                };
                let boundary_values = plan.internal_to_boundary_args(
                    &internal_values,
                    |left, right| {
                        format!(
                            "({} [{left}, {right}])",
                            self.shapes.runtime_names().product
                        )
                    },
                    |value, index, arity| {
                        format!(
                            "({} {index} {arity} ({value}))",
                            self.shapes.runtime_names().project
                        )
                    },
                );
                let mut natives = Vec::new();
                for (ty, value) in boundary_tys.iter().zip(&boundary_values) {
                    // Convert internal slot -> native param (Out direction).
                    let native = self.convert(ty, value, FfiDir::Out)?;
                    natives.push(paren_arg_expr(&native));
                }
                let call = if natives.is_empty() {
                    format!("({expr})")
                } else {
                    format!("({expr}) {}", natives.join(" "))
                };
                // The native result is converted In to the internal rep.
                let ret_conv = self.convert(ret, "__fr", FfiDir::In)?;
                Ok(format!(
                    "({} (\\{arg} -> ({call}) >>= \\__fr -> pure ({ret_conv})))",
                    self.shapes.runtime_names().function,
                ))
            }
        }
    }
}

impl HaskellSkin<'_> {
    /// Recursive peel for `convert_sum`'s `Out` direction: select the native
    /// constructor for arm `k` (and deeper) from a nested-binary internal
    /// sum `access`. A non-final arm tests its tag-0 head; the final arm is
    /// the fully-peeled remainder.
    fn convert_sum_out(
        &self,
        slots: &[&Type<Routed>],
        access: &str,
        k: usize,
    ) -> Result<String, EmitError> {
        let n = slots.len();
        if k + 1 == n {
            // Final arm: the fully-peeled value is the payload directly.
            let payload = self.convert(slots[k], access, FfiDir::Out)?;
            return Ok(super::native::either_inject(k, n, &payload));
        }
        let payload = self.convert(slots[k], "__cp", FfiDir::Out)?;
        let arm = super::native::either_inject(k, n, &payload);
        let rest = self.convert_sum_out(slots, "__cr", k + 1)?;
        Ok(format!(
            "(case {} ({access}) of {{ (__ct, __cp) -> case __ct of {{ 0 -> {arm}; _ -> let {{ __cr = __cp }} in {rest} }} }})",
            self.shapes.runtime_names().match_tag,
        ))
    }
}

// The shared `convert` driver short-circuits `is_passthrough` types and
// dispatches the rest. Role atoms and unit are NOT passthrough (they
// convert), but they are `Type::Path` / `Type::Unit` — neither a product,
// sum, nor function — so the driver would route them to `convert_newtype`
// (Path) or identity (Unit). We intercept both in a wrapper around the
// shared driver via `HaskellSkin::convert`'s default; instead, override
// the Path/Unit handling by implementing the leaves to cover them. Because
// the shared driver calls `convert_newtype` for every non-passthrough
// `Type::Path`, the scalar handling lives there.

pub(super) fn haskell_type_var(name: &str) -> String {
    let mut out = String::from("t_");
    let mut initial = true;
    for ch in crate::backends::public_names::host_name_core(name).chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '\'' {
            out.push(if initial && ch.is_ascii_alphabetic() {
                initial = false;
                ch.to_ascii_lowercase()
            } else {
                ch
            });
        } else {
            out.push('_');
        }
    }
    out
}

pub(super) fn nominal_haskell_type_name(prefix: &str, module_path: &str, name: &str) -> String {
    let exact = format!("{module_path}\0{name}");
    let encoded = exact
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let readable_module = module_path
        .split('/')
        .map(crate::backends::public_names::host_name_core)
        .collect::<Vec<_>>()
        .join("/");
    let readable_name = crate::backends::public_names::host_name_core(name);
    let readable = format!("{readable_module}_{readable_name}")
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("{prefix}_H{encoded}__{readable}")
}

/// Parenthesize a rendered Haskell type in an argument position.
fn paren_arg(ty: &str) -> String {
    let t = ty.trim();
    if !t.contains(' ') {
        return t.to_owned();
    }
    if (t.starts_with('(') && t.ends_with(')')) || (t.starts_with('[') && t.ends_with(']')) {
        return t.to_owned();
    }
    format!("({t})")
}

/// Parenthesize a rendered Haskell expression in an argument position
/// (wrap if it contains a space and is not already wrapped).
fn paren_arg_expr(expr: &str) -> String {
    let t = expr.trim();
    if !t.contains(' ') {
        return t.to_owned();
    }
    if t.starts_with('(') && t.ends_with(')') {
        return t.to_owned();
    }
    format!("({t})")
}

/// A newtype's package-global key — `<module>.<name>`. Two same-leaf
/// newtypes in different modules get distinct keys.
pub(crate) fn newtype_qual_key(module_path: &str, name: &str) -> String {
    format!("{module_path}.{name}")
}

/// Per-module bare-leaf → declaring-module resolution scope (own
/// declarations + `import N(…);` imports), built once over the package.
/// Qualified Routed paths already carry canonical module identities. Bare
/// leaves resolve only through the referring-module scope.
#[derive(Debug, Default)]
pub(crate) struct NewtypeResolution {
    /// referring-module → (bare leaf → declaring-module slash-path).
    scopes: BTreeMap<String, BTreeMap<String, String>>,
    /// Every exact `<module>.<name>` declaration key.
    exact_keys: BTreeSet<String>,
}

impl NewtypeResolution {
    pub(crate) fn build(package: &Package<Routed>) -> Self {
        let mut exact_keys = BTreeSet::new();
        for (path, entry) in package.modules() {
            for item in &entry.module.items {
                crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                    if let Some(newtype) = declaration.newtype() {
                        exact_keys.insert(newtype_qual_key(path, &newtype.name));
                    }
                });
            }
        }
        let mut scopes: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for (path, entry) in package.modules() {
            let mut scope: BTreeMap<String, String> = BTreeMap::new();
            for item in &entry.module.items {
                crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                    if let Some(newtype) = declaration.newtype() {
                        scope.insert(newtype.name.clone(), path.to_owned());
                    }
                });
            }
            for u in &entry.module.imports {
                if let crate::ast::ImportKind::Selective { items, from } = &u.kind {
                    let from_path = from
                        .segments
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join("/");
                    for name in items.iter().filter_map(crate::ast::ImportItem::as_name) {
                        if exact_keys.contains(&newtype_qual_key(&from_path, name))
                            && !scope.contains_key(name)
                        {
                            scope.insert(name.to_owned(), from_path.clone());
                        }
                    }
                }
            }
            scopes.insert(path.to_owned(), scope);
        }
        NewtypeResolution { scopes, exact_keys }
    }

    /// Resolve a newtype `Type::Path` to its `<module>.<name>` key, given
    /// the referring module. Every nonempty qualifier carries its canonical
    /// module path directly. A bare leaf resolves in `referring_module`'s
    /// scope. `None` for a non-newtype path or a module-less bare leaf.
    pub(crate) fn key_of(
        &self,
        ty: &Type<Routed>,
        referring_module: Option<&str>,
    ) -> Option<String> {
        let Type::Path { segments, .. } = ty else {
            return None;
        };
        if let Some((name, module_segments)) = segments.split_last()
            && !module_segments.is_empty()
        {
            let parts: Vec<String> = module_segments
                .iter()
                .map(|s| s.as_str().to_owned())
                .collect();
            let direct = newtype_qual_key(&parts.join("/"), name.as_str());
            return self.exact_keys.contains(&direct).then_some(direct);
        }
        let [seg] = segments.as_slice() else {
            return None;
        };
        self.leaf_key(seg.as_str(), referring_module)
    }

    /// Resolve a **bare newtype leaf** to its `<module>.<name>` key in the
    /// referring module's scope (own declaration → selective import). The
    /// entry the `Low*` IR's newtype-construct / -project variants use (they
    /// carry the leaf name directly, not a `Type::Path`).
    pub(crate) fn leaf_key(&self, leaf: &str, referring_module: Option<&str>) -> Option<String> {
        let module = referring_module?;
        let home = self.scopes.get(module).and_then(|s| s.get(leaf))?;
        Some(newtype_qual_key(home, leaf))
    }
}

/// Resolve a `Type::Path` reference to the newtype it names, plus the
/// declaring module's slash-path, **scoped to the referring module** so a
/// same-leaf newtype in another module never wins. `None` for anything that
/// is not a newtype reference. This is the resolution every boundary-type /
/// body-type query must use; the bare-leaf, module-blind walk is gone.
pub(crate) fn resolve_newtype_in<'p>(
    t: &Type<Routed>,
    package: &'p Package<Routed>,
    resolution: &NewtypeResolution,
    referring_module: Option<&str>,
) -> Option<(&'p Newtype<Routed>, String)> {
    let key = resolution.key_of(t, referring_module)?;
    let (module_path, name) = key.rsplit_once('.')?;
    let entry = package.module(module_path)?;
    for item in &entry.module.items {
        let mut found = None;
        crate::pass::resolve::for_each_item_declaration(item, |declaration| {
            if let Some(newtype) = declaration.newtype()
                && newtype.name == name
            {
                found = Some(newtype);
            }
        });
        if let Some(newtype) = found {
            return Some((newtype, module_path.to_owned()));
        }
    }
    None
}

/// Instantiate a Haskell newtype payload without moving an unqualified
/// nominal actual into the declaration owner's scope. The declaration body
/// and each actual argument are first qualified in their own lexical modules;
/// only then does capture-avoiding substitution combine them. A partially
/// applied newtype remains nominal, since expanding it would turn an HKT into
/// a value-kinded payload.
pub(super) fn instantiate_newtype_payload_in(
    declaration: &Newtype<Routed>,
    reference: &Type<Routed>,
    package: &Package<Routed>,
    owner: &str,
    referring_module: Option<&str>,
    bound: &BTreeSet<String>,
) -> Option<Type<Routed>> {
    let Type::Path { args, .. } = reference else {
        return None;
    };
    if args.len() != declaration.type_params.len() {
        return None;
    }

    let owner_module = &package.module(owner)?.module;
    let owner_scope: HashMap<String, Kind> = declaration
        .type_params
        .iter()
        .chain(&declaration.existential_params)
        .map(|param| (param.name.clone(), param.effective_kind()))
        .collect();
    let payload = crate::pass::resolve::qualify_routed_contract_type_in_module(
        &declaration.payload,
        owner_module,
        &owner_scope,
    );

    let actuals = if args.is_empty() {
        Vec::new()
    } else {
        let caller = &package.module(referring_module?)?.module;
        let caller_scope: HashMap<String, Kind> = bound
            .iter()
            .map(|name| (name.clone(), Kind::Star))
            .collect();
        args.iter()
            .map(|arg| {
                crate::pass::resolve::qualify_routed_contract_type_in_module(
                    arg,
                    caller,
                    &caller_scope,
                )
            })
            .collect()
    };
    let substitutions = declaration
        .type_params
        .iter()
        .zip(actuals)
        .map(|(param, arg)| (param.name.clone(), arg))
        .collect();
    Some(crate::pass::typecheck_core::subst_type(
        &payload,
        &substitutions,
    ))
}

pub(super) fn newtype_is_recursive_with_atomic_newtypes(
    d: &Newtype<Routed>,
    module_path: &str,
    package: &Package<Routed>,
    resolution: &NewtypeResolution,
    is_atomic: impl Fn(&str, &Newtype<Routed>) -> bool,
) -> bool {
    let root = format!("{module_path}.{}", d.name);
    type_reaches_newtype(
        &d.payload,
        package,
        resolution,
        RecursionReachContext {
            module: Some(module_path.to_owned()),
            type_vars: d
                .type_params
                .iter()
                .chain(&d.existential_params)
                .map(|p| p.name.clone())
                .collect(),
            newtype_path: BTreeSet::from([root.clone()]),
        },
        &root,
        &is_atomic,
    )
}

#[derive(Clone)]
struct RecursionReachContext {
    module: Option<String>,
    type_vars: BTreeSet<String>,
    newtype_path: BTreeSet<String>,
}

fn type_reaches_newtype(
    t: &Type<Routed>,
    package: &Package<Routed>,
    resolution: &NewtypeResolution,
    context: RecursionReachContext,
    root: &str,
    is_atomic: &impl Fn(&str, &Newtype<Routed>) -> bool,
) -> bool {
    crate::backends::skin::type_reaches_with(
        t,
        context,
        &|ty, _context: &RecursionReachContext| ty.clone(),
        &mut |_, _| false,
        &mut |ty, context: &RecursionReachContext| {
            let Type::Path { segments, args, .. } = ty else {
                return TypeReachAction::IgnoreSubtree;
            };
            if segments.len() == 1
                && let Some(name) = segments.first().map(|s| s.as_str())
                && context.type_vars.contains(name)
            {
                return TypeReachAction::TraverseArguments;
            }
            if let Some((d, module_path)) =
                resolve_newtype_in(ty, package, resolution, context.module.as_deref())
            {
                let key = format!("{module_path}.{}", d.name);
                if key == root {
                    return TypeReachAction::Found;
                }
                if is_atomic(&module_path, d) {
                    return TypeReachAction::IgnoreSubtree;
                }
                if context.newtype_path.contains(&key) {
                    return TypeReachAction::TraverseArguments;
                }
                let filled = args.len().min(d.type_params.len());
                let mut type_vars: BTreeSet<String> = d
                    .type_params
                    .iter()
                    .skip(filled)
                    .chain(&d.existential_params)
                    .map(|p| p.name.clone())
                    .collect();
                type_vars.extend(skin::type_vars_referenced_in_args(args, &context.type_vars));
                let Some(payload) = instantiate_newtype_payload_in(
                    d,
                    ty,
                    package,
                    &module_path,
                    context.module.as_deref(),
                    &context.type_vars,
                ) else {
                    return TypeReachAction::IgnoreSubtree;
                };
                let mut newtype_path = context.newtype_path.clone();
                newtype_path.insert(key);
                return TypeReachAction::Descend {
                    ty: payload,
                    context: RecursionReachContext {
                        module: Some(module_path),
                        type_vars,
                        newtype_path,
                    },
                    // Haskell retains every parameter of a parametric
                    // newtype's abstract boundary carrier, including a
                    // phantom parameter absent from its value payload.
                    // Its arguments therefore remain relevant to whether the
                    // rendered host type needs a finite nominal anchor.
                    traverse_arguments: true,
                };
            }
            TypeReachAction::TraverseArguments
        },
        &|context, param| {
            let mut next = context.clone();
            next.type_vars.insert(param.name.clone());
            next
        },
    )
}
