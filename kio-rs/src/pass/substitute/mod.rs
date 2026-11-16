//! `Lowered → Prime` substitution: walks a typechecked package,
//! replaces each elaboration-bearing node (`if`/`else`, internal
//! elaboration forms, and user-defined macro calls) with the
//! typer's recorded elaboration, then materializes compiler-only binders
//! before constructing the [`Prime`] artifact. After this pass,
//! every elaboration-bearing
//! variant is statically uninhabited (`Prime::ExprElab = Never`), so the Kio'
//! emitter can match on `Expr<Prime>` and discharge those arms via
//! `match *ext {}` rather than consulting a side table.
//!
//! The walk is by reference rather than by-value so substitution can
//! inspect typed source terms while deep-cloning the produced
//! [`Package<Prime>`]. Elaboration entries are keyed by the stable
//! `NodeId` carried on each elaboration-bearing expression, while
//! type-argument and literal resolutions use the checked expression's
//! occurrence identity within its module.
//!
//! The per-variant clone-and-recurse boilerplate lives on the
//! private `LoweredToPrePrime` visitor trait; this
//! module's [`SubstituteVisitor`] supplies the rewrite rules:
//!
//! - `rewrite_expr_elaborator`: consult `Elaborations`. If present,
//!   substitute the elaboration; if absent, fall through to the inner
//!   expression for structural passthrough cases.
//! - `rewrite_expr_user_macro`: consult `Elaborations`; absent ⇒
//!   typer-pipeline invariant violation.
//! - `rewrite_expr_if_else`: consult `Elaborations`; absent ⇒ typer
//!   pipeline invariant violation.
//! - `visit_expr_call`: canonical-slot infer-arg insertion using
//!   `Elaborations::type_resolution_for`.
//! - `rewrite_type_infer`: an unresolved `Type::Infer` is a
//!   typer-pipeline invariant violation in every substitution mode.
//! - `walk_literal_annotation`: compile-time evaluation may preserve an
//!   unresolved literal annotation as the explicit `UncheckedPrime` hole.
//! - `rewrite_item_equiv`: drop the item (equiv declarations have
//!   no execution semantics).

#[cfg(test)]
mod checked_call_tests;
mod pre_prime;
mod walk;

use crate::ast::{
    CallArg, Equiv, Expr, FnDef, Import, ImportItem, ImportKind, Item, Lowered, Meta, Module,
    ModulePath, Never, PackageFile, Param, PathSegment, Phase, Prime, Signature,
    SignatureGroupKind, SignatureParam, Type, TypeParam, UncheckedPrime, convert_expr,
    convert_meta, convert_signature,
};
use crate::error::Error;
use crate::pass::alpha_normalize::AlphaNormalizedPackage;
use crate::pass::resolve::{LocatedError, ModuleEntry, Package, PackageFileEntry, TopLevelScope};
use crate::pass::substitute::walk::LoweredToPrePrime;
use crate::pass::typecheck_core::UserMacroTemplateGeneratedImport;
use crate::pass::typecheck_full::{Elaborations, RecordedElaboration};
use crate::span::Span;
#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// Kio'-shaped substitution output before compiler-only value binders are
/// materialized into the ordinary Kio value namespace. During checked-term
/// replay it may also carry unspellable generated type-binder sentinels until
/// the complete replay tree is assembled. The type is confined to
/// Lowered-boundary substitution and checked-term replay; production exits
/// consume those private names before returning `Prime` or `UncheckedPrime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PrePrime;

impl Phase for PrePrime {
    type FnBody = Expr<Self>;
    type ExprBlockSyntax = Never;
    type ExpressionOccurrence = ();
    type CallDiagnostic = ();
    type LitAnnotation = Type<PrePrime>;
    type ExprTuple = Never;
    type ExprFnPlaceholder = Never;
    type ExprOpChain = Never;
    type ExprElab = Never;
    type ExprRecCall = Never;
    type ExprRecOrder = Never;
    type ExprUfcs = Never;
    type ExprEnriched = Never;
    type ExprEnrichedSynthTy = Never;
    type ExprLow = Never;
    type ExprResolved = ();
    type TypeInfer = Never;
    type TypeGoal = Never;
    type ItemLabels = Never;
    type ItemEquiv = Never;
    type ItemMacro = Never;
    type ItemOp = Never;
    type ItemRecGroup = Never;
    type ItemLiteralAlias = Never;
    type ParamPatternExt = ();
    type FnDefRetElided = ();
    type FnPurity = crate::ast::Purity;
    type LeadingTrivia = ();
    type SourceDelimitedTrivia = ();
    type SourceSignatureTrivia = ();
    type FnExprCapabilities = ();
    type FnTypeCapabilities = ();
}

impl crate::pass::visit_mut::TypecheckVisitPhase for PrePrime {}

/// Exact residual callable layers retained at the Lowered → PrePrime seam.
/// Evaluator output and ordinary calls both feed this representation; only
/// the boundary materializer turns it into ordinary self-validating Kio'.
#[derive(Debug, Clone, Default)]
pub(crate) struct EtaPlan<P: Phase> {
    pub(crate) layers: Vec<EtaLayer<P>>,
}

#[derive(Debug, Clone)]
pub(crate) struct EtaLayer<P: Phase> {
    pub(crate) type_params: Vec<TypeParam>,
    pub(crate) value_params: Vec<EtaValueParam<P>>,
    /// The public plan distinguishes an entered source-empty value layer from
    /// an untouched pure type layer; the latter always has no value params.
    pub(crate) has_value_layer: bool,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<EtaLayer<UncheckedPrime>>() == 56);

#[derive(Debug, Clone)]
pub(crate) struct EtaValueParam<P: Phase> {
    pub(crate) name: String,
    pub(crate) ty: Type<P>,
}

pub(crate) fn map_eta_plan_to_pre_prime(
    plan: &EtaPlan<UncheckedPrime>,
    mut map_type: impl FnMut(&Type<UncheckedPrime>) -> Type<PrePrime>,
) -> EtaPlan<PrePrime> {
    EtaPlan {
        layers: plan
            .layers
            .iter()
            .map(|layer| EtaLayer {
                type_params: layer.type_params.clone(),
                value_params: layer
                    .value_params
                    .iter()
                    .map(|value| EtaValueParam {
                        name: value.name.clone(),
                        ty: map_type(&value.ty),
                    })
                    .collect(),
                has_value_layer: layer.has_value_layer,
            })
            .collect(),
    }
}

fn first_eta_layer_from_residual_type(residual: &Type<PrePrime>) -> Option<EtaPlan<PrePrime>> {
    let mut current = residual.clone();
    let mut type_params = Vec::new();
    while let Type::Forall { param, body, .. } = current {
        type_params.push(param);
        current = *body;
    }
    let Type::Function {
        param, abi_arity, ..
    } = current
    else {
        // Type application to a polymorphic non-function has no runtime
        // value layer to eta-expand. Its existing erased representation is
        // already the residual value; never invent a Unit application.
        return None;
    };
    // A zero-slot runtime domain still binds a semantic Unit argument. Its
    // opaque residual must be forwarded rather than replaced with literal ().
    let value_params = Type::right_spine_take(param.as_ref(), abi_arity.max(1))
        .into_iter()
        .enumerate()
        .map(|(slot, ty)| EtaValueParam {
            name: format!("__eta_value0_s{slot}__"),
            ty: ty.clone(),
        })
        .collect();
    Some(EtaPlan {
        layers: vec![EtaLayer {
            type_params,
            value_params,
            has_value_layer: true,
        }],
    })
}

pub(crate) enum EtaTerminal {
    Body(Expr<PrePrime>),
    Call {
        callee: Expr<PrePrime>,
        args: Vec<CallArg<PrePrime>>,
    },
}

pub(crate) fn eta_materialize_pre_prime(
    plan: &EtaPlan<PrePrime>,
    terminal: EtaTerminal,
    span: Span,
) -> Expr<PrePrime> {
    fn wrap_layer(layer: &EtaLayer<PrePrime>, body: Expr<PrePrime>, span: Span) -> Expr<PrePrime> {
        let mut params = layer
            .type_params
            .iter()
            .cloned()
            .map(SignatureParam::Type)
            .collect::<Vec<_>>();
        params.extend(layer.value_params.iter().map(|value| {
            SignatureParam::Value(Param {
                name: value.name.clone(),
                ty: Some(value.ty.clone()),
                pattern: (),
                meta: Meta::new(span),
            })
        }));
        let mut groups = layer
            .type_params
            .iter()
            .map(|_| SignatureGroupKind::Type { len: 1 })
            .collect::<Vec<_>>();
        if layer.has_value_layer {
            groups.push(SignatureGroupKind::Value {
                len: layer.value_params.len(),
            });
        }
        Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(params, groups),
            ret_ty: None,
            body: Box::new(body),
            meta: Meta::new(span),
            caps: (),
        }
    }

    enum CallStage<'a> {
        Type(&'a TypeParam),
        Value(&'a EtaLayer<PrePrime>),
    }

    fn wrap_call_stage(stage: &CallStage<'_>, body: Expr<PrePrime>, span: Span) -> Expr<PrePrime> {
        let (params, groups) = match stage {
            CallStage::Type(param) => (
                vec![SignatureParam::Type((*param).clone())],
                vec![SignatureGroupKind::Type { len: 1 }],
            ),
            CallStage::Value(layer) => (
                layer
                    .value_params
                    .iter()
                    .map(|value| {
                        SignatureParam::Value(Param {
                            name: value.name.clone(),
                            ty: Some(value.ty.clone()),
                            pattern: (),
                            meta: Meta::new(span),
                        })
                    })
                    .collect(),
                vec![SignatureGroupKind::Value {
                    len: layer.value_params.len(),
                }],
            ),
        };
        Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(params, groups),
            ret_ty: None,
            body: Box::new(body),
            meta: Meta::new(span),
            caps: (),
        }
    }

    fn apply_call_stage(
        stage: &CallStage<'_>,
        callee: Expr<PrePrime>,
        span: Span,
    ) -> Expr<PrePrime> {
        let args = match stage {
            CallStage::Type(param) => vec![CallArg::Type(Type::Path {
                segments: vec![PathSegment::new(param.name.clone(), span)],
                args: Vec::new(),
                meta: Meta::new(span),
            })],
            CallStage::Value(layer) if layer.value_params.is_empty() => {
                vec![CallArg::Value(Expr::Unit {
                    source_trivia: Default::default(),
                    occurrence: Default::default(),
                    meta: Meta::new(span),
                })]
            }
            CallStage::Value(layer) => layer
                .value_params
                .iter()
                .map(|value| {
                    CallArg::Value(Expr::Path {
                        occurrence: Default::default(),
                        segments: vec![PathSegment::new(value.name.clone(), span)],
                        meta: Meta::new(span),
                        ext: (),
                    })
                })
                .collect(),
        };
        Expr::synth_call(callee, args, span)
    }

    fn materialize_call_stages(
        stages: &[CallStage<'_>],
        index: usize,
        callee: Expr<PrePrime>,
        span: Span,
    ) -> Expr<PrePrime> {
        let stage = &stages[index];
        let applied = apply_call_stage(stage, callee, span);
        let body = if index + 1 == stages.len() {
            applied
        } else {
            // Applying each type binder and value group is observable under
            // CBV even when the returned function is never invoked. Bind the
            // result before constructing the next residual abstraction.
            let next_name = format!("__eta_next{index}__");
            let next_path = Expr::Path {
                occurrence: Default::default(),
                segments: vec![PathSegment::new(next_name.clone(), span)],
                meta: Meta::new(span),
                ext: (),
            };
            Expr::Let {
                occurrence: Default::default(),
                name: next_name,
                name_span: span,
                ty: None,
                pattern: (),
                value: Box::new(applied),
                body: Box::new(materialize_call_stages(stages, index + 1, next_path, span)),
                meta: Meta::new(span),
            }
        };
        wrap_call_stage(stage, body, span)
    }

    match terminal {
        EtaTerminal::Body(body) => plan.layers.iter().rev().fold(body, |body, layer| {
            if layer.has_value_layer {
                wrap_layer(layer, body, span)
            } else {
                assert!(
                    layer.value_params.is_empty(),
                    "a type-only eta layer retained a value ABI"
                );
                layer.type_params.iter().rev().fold(body, |body, param| {
                    wrap_call_stage(&CallStage::Type(param), body, span)
                })
            }
        }),
        EtaTerminal::Call { callee, args } if plan.layers.is_empty() => {
            if args.is_empty() {
                callee
            } else {
                Expr::synth_call(callee, args, span)
            }
        }
        EtaTerminal::Call { callee, args } => {
            let (callee, outer_binding) = if args.is_empty() {
                (callee, None)
            } else {
                let name = "__eta_prefix__".to_owned();
                let path = Expr::Path {
                    occurrence: Default::default(),
                    segments: vec![PathSegment::new(name.clone(), span)],
                    meta: Meta::new(span),
                    ext: (),
                };
                (path, Some((name, Expr::synth_call(callee, args, span))))
            };
            let stages = plan
                .layers
                .iter()
                .flat_map(|layer| {
                    layer
                        .type_params
                        .iter()
                        .map(CallStage::Type)
                        .chain(layer.has_value_layer.then_some(CallStage::Value(layer)))
                })
                .collect::<Vec<_>>();
            let materialized = materialize_call_stages(&stages, 0, callee, span);
            match outer_binding {
                None => materialized,
                Some((name, value)) => Expr::Let {
                    occurrence: Default::default(),
                    name,
                    name_span: span,
                    ty: None,
                    pattern: (),
                    value: Box::new(value),
                    body: Box::new(materialized),
                    meta: Meta::new(span),
                },
            }
        }
    }
}

/// Lower a `Package<Lowered>` to `Package<Prime>` by substituting each
/// recorded elaboration in place of its surface node.
fn substitute_package(package: &Package<Lowered>, elaborations: &Elaborations) -> Package<Prime> {
    let pkg_name = package
        .package_file()
        .map(|e| e.package_name.as_str())
        .unwrap_or("");
    let module_list: Vec<_> = package.modules().collect();
    let entries: Vec<(String, ModuleEntry<Prime>)> = crate::maybe_par_iter!(module_list)
        .map(|(path, entry)| {
            let module = substitute_module_with_path_in_package(
                &entry.module,
                elaborations,
                path,
                Some(package),
            );
            // Substitution may append generated qualified imports and prepend
            // generated type aliases. Rebuild the derived declaration/import
            // index once at this phase boundary; copying the Lowered scope
            // would leave both its import edges and TopLevelIds stale.
            let scope = TopLevelScope::build(&module).unwrap_or_else(|error| {
                unreachable!(
                    "Lowered-to-Prime substitution produced an invalid top-level scope: {error:?}"
                )
            });
            (
                (*path).to_owned(),
                ModuleEntry::<Prime> {
                    file_path: entry.file_path.clone(),
                    module,
                    scope,
                },
            )
        })
        .collect();
    let new_modules = entries.into_iter().collect();
    let new_package_file = package
        .package_file()
        .map(|entry| PackageFileEntry::<Prime> {
            file_path: entry.file_path.clone(),
            package_name: entry.package_name.clone(),
            package_file: substitute_package_file_with_pkg(
                &entry.package_file,
                elaborations,
                pkg_name,
            ),
        });
    Package::<Prime>::from_parts(new_modules, new_package_file)
}

/// Substitute a package and re-establish its lexical alpha-normalization proof.
pub(super) fn substitute_normalized_package(
    normalized: AlphaNormalizedPackage<Lowered>,
    elaborations: &Elaborations,
) -> AlphaNormalizedPackage<Prime> {
    normalized.renormalize_after(|package| substitute_package(&package, elaborations))
}

/// Build the independently resolvable Kio'-shaped package used by
/// compile-time evaluation. Every surface elaboration must already be
/// recorded; ordinary irreducible Kio' terms remain available to the reducer,
/// and comptime helper calls are authorized by the artifact's explicit
/// primitive environment.
pub(crate) fn substitute_package_for_eval(
    normalized: &AlphaNormalizedPackage<Lowered>,
    elaborations: &Elaborations,
) -> Result<crate::normalization::EvalArtifactInput, LocatedError> {
    let package = normalized.package();
    let mut primitives = crate::normalization::EvalPrimitiveEnv::default();
    for (path, entry) in package.modules() {
        enable_module_primitives_for_eval(&mut primitives, &entry.module, path, elaborations);
    }
    let pkg_name = package
        .package_file()
        .map(|entry| entry.package_name.as_str())
        .unwrap_or("");
    let entries = package
        .modules()
        .map(|(path, entry)| -> Result<_, LocatedError> {
            let module =
                substitute_module_for_eval(&entry.module, elaborations, path, Some(package));
            let scope = TopLevelScope::build(&module).map_err(|error| LocatedError {
                file_path: entry.file_path.clone(),
                error,
            })?;
            Ok((
                path.to_owned(),
                ModuleEntry::<UncheckedPrime> {
                    file_path: entry.file_path.clone(),
                    module,
                    scope,
                },
            ))
        })
        .collect::<Result<_, _>>()?;
    let package_file = package
        .package_file()
        .map(|entry| PackageFileEntry::<UncheckedPrime> {
            file_path: entry.file_path.clone(),
            package_name: entry.package_name.clone(),
            package_file: substitute_package_file_for_eval(
                &entry.package_file,
                elaborations,
                pkg_name,
            ),
        });
    let package = Package::from_parts(entries, package_file);
    package.resolve_imports()?;
    Ok(crate::normalization::EvalArtifactInput::new(
        package, primitives,
    ))
}

pub(crate) fn primitive_env_for_eval_module(
    module: &Module<Lowered>,
    module_path: &str,
    elaborations: &Elaborations,
) -> crate::normalization::EvalPrimitiveEnv {
    let mut primitives = crate::normalization::EvalPrimitiveEnv::default();
    enable_module_primitives_for_eval(&mut primitives, module, module_path, elaborations);
    primitives
}

fn enable_module_primitives_for_eval(
    primitives: &mut crate::normalization::EvalPrimitiveEnv,
    module: &Module<Lowered>,
    module_path: &str,
    elaborations: &Elaborations,
) {
    for dependency in elaborations.replay_primitives_for(module_path) {
        for (owner, bindings) in dependency.bindings() {
            for (_, builtin) in bindings {
                primitives.grant(owner, builtin);
            }
        }
        for (owner, binding, constructor) in dependency.literal_bindings() {
            primitives.grant_literal(owner, binding, constructor.clone());
        }
    }
    for plan in elaborations.comptime_literals_for(module_path) {
        primitives.grant_literal(module_path, &plan.binding, plan.constructor.clone());
    }
    if module
        .imports
        .iter()
        .any(|import_| matches!(import_.kind, ImportKind::Comptime))
    {
        primitives.enable_comptime_module(module_path);
    }
}

pub(super) fn primitive_env_for_replayed_expr(
    expr: &mut Expr<PrePrime>,
    module: &Module<Lowered>,
    owner: &str,
    elabs: &Elaborations,
) -> Option<std::sync::Arc<crate::normalization::EvalPrimitiveEnv>> {
    use crate::pass::visit_mut::TypecheckVisitMut;
    #[derive(Default)]
    struct Names {
        bound: Vec<String>,
        referenced: std::collections::HashSet<String>,
    }
    impl TypecheckVisitMut<PrePrime> for Names {
        fn enter_type_binder(&mut self, param: &mut TypeParam, _scope: Span) {
            self.bound.push(param.name.clone());
        }
        fn exit_type_binder(&mut self, _param: &mut TypeParam, _scope: Span) {
            self.bound.pop();
        }
        fn enter_value_binder(&mut self, name: &mut String, _span: Span, _scope: Span) {
            self.bound.push(name.clone());
        }
        fn exit_value_binder(&mut self, _name: &mut String, _span: Span, _scope: Span) {
            self.bound.pop();
        }
        fn visit_type(&mut self, ty: &mut Type<PrePrime>) {
            if let Type::Path { segments, .. } = ty
                && let [name] = segments.as_slice()
                && !self.bound.iter().any(|bound| bound == name.as_str())
            {
                self.referenced.insert(name.as_str().to_owned());
            }
            crate::pass::visit_mut::walk_type(self, ty);
        }
        fn visit_expr(&mut self, expr: &mut Expr<PrePrime>) {
            if let Expr::Path { segments, .. } = expr
                && let [name] = segments.as_slice()
                && !self.bound.iter().any(|bound| bound == name.as_str())
            {
                self.referenced.insert(name.as_str().to_owned());
            }
            crate::pass::visit_mut::walk_expr(self, expr);
        }
    }
    let mut names = Names::default();
    names.visit_expr(expr);
    let mut dependencies = crate::normalization::EvalPrimitiveEnv::default();
    if module
        .imports
        .iter()
        .any(|import| matches!(import.kind, ImportKind::Comptime))
    {
        for name in &names.referenced {
            if let Some(builtin) = crate::comptime::ComptimeBuiltin::from_public_name(name) {
                dependencies.grant(owner, builtin);
            }
        }
    }
    for plan in elabs.comptime_literals_for(owner) {
        if names.referenced.contains(&plan.binding) {
            dependencies.grant_literal(owner, &plan.binding, plan.constructor.clone());
            dependencies.grant(owner, crate::comptime::ComptimeBuiltin::ComptimeProof);
            dependencies.grant(owner, plan.constructor.result_builtin());
        }
    }
    for prior in elabs.replay_primitives_for(owner) {
        for (dependency_owner, entries) in prior.bindings() {
            for (name, builtin) in entries {
                if names.referenced.contains(name) {
                    dependencies.grant(dependency_owner, builtin);
                }
            }
        }
        for (dependency_owner, name, constructor) in prior.literal_bindings() {
            if names.referenced.contains(name) {
                dependencies.grant_literal(dependency_owner, name, constructor.clone());
                dependencies.grant(
                    dependency_owner,
                    crate::comptime::ComptimeBuiltin::ComptimeProof,
                );
                dependencies.grant(dependency_owner, constructor.result_builtin());
            }
        }
    }
    (dependencies.bindings().next().is_some() || dependencies.literal_bindings().next().is_some())
        .then(|| std::sync::Arc::new(dependencies))
}

pub fn substitute_module_for_eval(
    module: &Module<Lowered>,
    elaborations: &Elaborations,
    module_path: &str,
    _package: Option<&Package<Lowered>>,
) -> Module<UncheckedPrime> {
    let pre_prime = SubstituteVisitor {
        elabs: elaborations,
        module_path: module_path.to_owned(),
        module: Some(module),
        prime_type_requalifier: PrimeRequalifier::Active(
            crate::pass::typecheck_core::PrimeTypeRequalifier::new(module),
        ),
        type_binders: Vec::new(),
        prime_requalifier_seeded: false,
        source_bindings: None,
        comptime_lowering_mode: ComptimeLoweringMode::EvalPreserve,
    }
    .walk_module(module)
    .expect(INFALLIBLE);
    pre_prime::finish_module_unchecked(pre_prime)
}

fn substitute_package_file_for_eval(
    package_file: &PackageFile<Lowered>,
    elaborations: &Elaborations,
    package_name: &str,
) -> PackageFile<UncheckedPrime> {
    let pre_prime = SubstituteVisitor {
        elabs: elaborations,
        module_path: format!("__package_file__:{package_name}"),
        module: None,
        prime_type_requalifier: PrimeRequalifier::Disabled,
        type_binders: Vec::new(),
        prime_requalifier_seeded: false,
        source_bindings: None,
        comptime_lowering_mode: ComptimeLoweringMode::EvalPreserve,
    }
    .walk_package_file(package_file)
    .expect(INFALLIBLE);
    pre_prime::finish_package_file_unchecked(pre_prime)
}

pub fn substitute_expr_for_eval(
    expr: &Expr<Lowered>,
    elabs: &Elaborations,
    module_path: &str,
    package: Option<&Package<Lowered>>,
    module: Option<&Module<Lowered>>,
) -> Expr<UncheckedPrime> {
    let module = module.or_else(|| {
        package.and_then(|package| package.module(module_path).map(|entry| &entry.module))
    });
    if package.is_some() && module.is_none() {
        unreachable!("evaluator substitution module `{module_path}` is absent from its package");
    }
    let mut visitor = SubstituteVisitor {
        elabs,
        module_path: module_path.to_owned(),
        module,
        prime_type_requalifier: PrimeRequalifier::Disabled,
        type_binders: Vec::new(),
        prime_requalifier_seeded: false,
        source_bindings: None,
        comptime_lowering_mode: ComptimeLoweringMode::EvalPreserve,
    };
    let pre_prime = visitor.walk_expr(expr).expect(INFALLIBLE);
    pre_prime::finish_expr_unchecked(pre_prime)
}

/// Stage a replayed user-macro result before the complete checked term is
/// consumed into the [`RecordedElaboration::UncheckedPrime`] artifact. Inferred
/// canonical type slots are re-spelled here, while explicit source-spelled type
/// arguments continue through `walk_expr` unchanged.
pub(super) fn substitute_expr_for_replayed_elaboration(
    expr: &Expr<Lowered>,
    ambient_type_names: &std::collections::HashSet<String>,
    elabs: &mut Elaborations,
    module_path: &str,
    package: Option<&Package<Lowered>>,
    module: &Module<Lowered>,
) -> Expr<PrePrime> {
    substitute_expr_with_completed_source_bindings(
        expr,
        None,
        ambient_type_names,
        elabs,
        module_path,
        package,
        module,
    )
}

/// Replay the retained checked source after its children have completed. The
/// exact occurrence keys also survive in recorded Lowered replacements, so
/// ordinary call plans and UFCS placement remain owned by substitution.
pub(super) fn substitute_expr_with_completed_source_bindings(
    expr: &Expr<Lowered>,
    source_bindings: Option<&std::collections::HashMap<crate::ast::ExpressionOccurrenceId, String>>,
    ambient_type_names: &std::collections::HashSet<String>,
    elabs: &mut Elaborations,
    module_path: &str,
    _package: Option<&Package<Lowered>>,
    module: &Module<Lowered>,
) -> Expr<PrePrime> {
    let (pre_prime, imports, aliases) = {
        let mut visitor = SubstituteVisitor {
            elabs,
            module_path: module_path.to_owned(),
            module: Some(module),
            prime_type_requalifier: PrimeRequalifier::Deferred,
            type_binders: ambient_type_names.iter().cloned().collect(),
            prime_requalifier_seeded: false,
            source_bindings,
            comptime_lowering_mode: ComptimeLoweringMode::EvalPreserve,
        };
        let pre_prime = visitor.walk_expr(expr).expect(INFALLIBLE);
        let (imports, aliases) = match visitor.prime_type_requalifier {
            PrimeRequalifier::Active(mut requalifier) => {
                (requalifier.take_imports(), requalifier.take_local_aliases())
            }
            PrimeRequalifier::Deferred => (Vec::new(), Vec::new()),
            PrimeRequalifier::Disabled => {
                unreachable!("replayed elaboration enables Prime type requalification")
            }
        };
        (pre_prime, imports, aliases)
    };
    record_replayed_prime_bindings(elabs, module_path, imports, aliases);
    pre_prime
}

pub(super) fn finish_replayed_elaboration(
    expr: Expr<PrePrime>,
    reserved_type_names: &std::collections::HashSet<String>,
) -> Expr<UncheckedPrime> {
    pre_prime::finish_expr_unchecked_with_reserved_type_names(expr, reserved_type_names)
}

#[cfg(test)]
pub(crate) fn finish_replayed_elaboration_for_test(expr: Expr<PrePrime>) -> Expr<UncheckedPrime> {
    finish_replayed_elaboration(expr, &std::collections::HashSet::new())
}

pub fn substitute_type_for_eval(
    ty: &Type<Lowered>,
    elabs: &Elaborations,
    module_path: &str,
    package: Option<&Package<Lowered>>,
    module: Option<&Module<Lowered>>,
) -> Type<UncheckedPrime> {
    let module = module.or_else(|| {
        package.and_then(|package| package.module(module_path).map(|entry| &entry.module))
    });
    if package.is_some() && module.is_none() {
        unreachable!("evaluator substitution module `{module_path}` is absent from its package");
    }
    let mut visitor = SubstituteVisitor {
        elabs,
        module_path: module_path.to_owned(),
        module,
        prime_type_requalifier: PrimeRequalifier::Disabled,
        type_binders: Vec::new(),
        prime_requalifier_seeded: false,
        source_bindings: None,
        comptime_lowering_mode: ComptimeLoweringMode::EvalPreserve,
    };
    let pre_prime = visitor.walk_type(ty).expect(INFALLIBLE);
    pre_prime::finish_type_unchecked(pre_prime)
}

pub fn substitute_signature_for_eval(
    sig: &Signature<Lowered>,
    elabs: &Elaborations,
    module_path: &str,
    package: Option<&Package<Lowered>>,
    module: Option<&Module<Lowered>>,
) -> Signature<UncheckedPrime> {
    let module = module.or_else(|| {
        package.and_then(|package| package.module(module_path).map(|entry| &entry.module))
    });
    if package.is_some() && module.is_none() {
        unreachable!("evaluator substitution module `{module_path}` is absent from its package");
    }
    let mut visitor = SubstituteVisitor {
        elabs,
        module_path: module_path.to_owned(),
        module,
        prime_type_requalifier: PrimeRequalifier::Disabled,
        type_binders: Vec::new(),
        prime_requalifier_seeded: false,
        source_bindings: None,
        comptime_lowering_mode: ComptimeLoweringMode::EvalPreserve,
    };
    let pre_prime = Signature::from_parts(
        visitor
            .walk_ordered_signature_params(&sig.params)
            .expect(INFALLIBLE),
        sig.groups.clone(),
    );
    convert_signature::<PrePrime, UncheckedPrime>(&pre_prime)
}

pub fn substitute_fn_def_for_eval(
    def: &FnDef<Lowered>,
    elabs: &Elaborations,
    module_path: &str,
    package: Option<&Package<Lowered>>,
    module: Option<&Module<Lowered>>,
) -> FnDef<UncheckedPrime> {
    let module = module.or_else(|| {
        package.and_then(|package| package.module(module_path).map(|entry| &entry.module))
    });
    if package.is_some() && module.is_none() {
        unreachable!("evaluator substitution module `{module_path}` is absent from its package");
    }
    let mut visitor = SubstituteVisitor {
        elabs,
        module_path: module_path.to_owned(),
        module,
        prime_type_requalifier: PrimeRequalifier::Disabled,
        type_binders: Vec::new(),
        prime_requalifier_seeded: false,
        source_bindings: None,
        comptime_lowering_mode: ComptimeLoweringMode::EvalPreserve,
    };
    let pre_prime = visitor.walk_fn_def(def).expect(INFALLIBLE);
    pre_prime::finish_fn_def_unchecked(pre_prime)
}

/// Substitute a single module. Used by [`substitute_package`] and
/// (in test code) by `backends::js::emit`'s per-module pipeline. Infallible
/// by construction — every elaboration site is recorded during
/// type-checking.
///
/// The test helper derives the module-path key from the
/// `Module<Lowered>` itself; production callers pass the canonical
/// slash path through [`substitute_module_with_path_in_package`].
#[cfg(test)]
pub(crate) fn substitute_module(module: &Module<Lowered>, elabs: &Elaborations) -> Module<Prime> {
    let path = module
        .path
        .segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("/");
    substitute_module_with_path_in_package(module, elabs, &path, None)
}

pub(crate) fn substitute_module_with_path_in_package(
    module: &Module<Lowered>,
    elabs: &Elaborations,
    module_path: &str,
    _package: Option<&Package<Lowered>>,
) -> Module<Prime> {
    let pre_prime = SubstituteVisitor {
        elabs,
        module_path: module_path.to_owned(),
        module: Some(module),
        prime_type_requalifier: PrimeRequalifier::Active(
            crate::pass::typecheck_core::PrimeTypeRequalifier::new(module),
        ),
        type_binders: Vec::new(),
        prime_requalifier_seeded: false,
        source_bindings: None,
        comptime_lowering_mode: ComptimeLoweringMode::RuntimeErase,
    }
    .walk_module(module)
    .expect(INFALLIBLE);
    pre_prime::finish_module(pre_prime)
}

#[cfg(test)]
pub(crate) fn substitute_package_file(
    package_file: &PackageFile<Lowered>,
    elabs: &Elaborations,
) -> PackageFile<Prime> {
    substitute_package_file_with_pkg(package_file, elabs, "")
}

pub(crate) fn substitute_package_file_with_pkg(
    package_file: &PackageFile<Lowered>,
    elabs: &Elaborations,
    package_name: &str,
) -> PackageFile<Prime> {
    let pre_prime = SubstituteVisitor {
        elabs,
        // The package file's elaboration keys are namespaced under the
        // `__package_file__:<pkg>` sentinel, keeping them disjoint from
        // every real module path (module names never begin with `__`).
        module_path: format!("__package_file__:{package_name}"),
        module: None,
        prime_type_requalifier: PrimeRequalifier::Disabled,
        type_binders: Vec::new(),
        prime_requalifier_seeded: false,
        source_bindings: None,
        comptime_lowering_mode: ComptimeLoweringMode::RuntimeErase,
    }
    .walk_package_file(package_file)
    .expect(INFALLIBLE);
    pre_prime::finish_package_file(pre_prime)
}

/// `SubstituteVisitor`'s `LoweredToPrePrime` impl is infallible by
/// construction (every rewrite case either substitutes a recorded
/// elaboration or hits an `unreachable!`). The `Result<_, Error>`
/// in the trait signature lets the trait stay reusable for other
/// Lowered-boundary walkers that may need fallible rewrites; here
/// every error path is statically unreachable, so we
/// `.expect(INFALLIBLE)` at the call sites.
const INFALLIBLE: &str = "substitute is infallible by construction — see SubstituteVisitor";

fn generated_import_path(module_path: &str) -> ModulePath {
    let span = Span::new(0, 0);
    ModulePath {
        segments: module_path
            .split('/')
            .map(|segment| PathSegment::new(segment.to_owned(), span))
            .collect(),
        span,
    }
}

fn generated_import_clause(import: &UserMacroTemplateGeneratedImport) -> Import {
    let span = Span::new(0, 0);
    Import {
        trailing_trivia: Vec::new(),
        kind: ImportKind::Qualified {
            path: generated_import_path(&import.module_path),
            alias: import.alias.clone(),
        },
        span,
        leading_trivia: Vec::new(),
    }
}

fn qualified_import_matches_generated(
    u: &Import,
    import: &UserMacroTemplateGeneratedImport,
) -> bool {
    let ImportKind::Qualified { path, alias, .. } = &u.kind else {
        return false;
    };
    alias == &import.alias && module_path_key(path) == import.module_path
}

fn record_replayed_prime_bindings(
    elabs: &mut Elaborations,
    module_path: &str,
    imports: Vec<Import>,
    aliases: Vec<crate::pass::typecheck_core::PrimeLocalTypeAlias>,
) {
    let imports = imports
        .into_iter()
        .filter_map(|import_| {
            let ImportKind::Qualified { path, alias, .. } = import_.kind else {
                return None;
            };
            Some(UserMacroTemplateGeneratedImport {
                module_path: module_path_key(&path),
                alias,
            })
        })
        .collect::<Vec<_>>();
    elabs.record_prime_requalification_imports(module_path, &imports);
    elabs.record_generated_type_aliases(module_path, &aliases);
}

#[allow(clippy::large_enum_variant)] // Preserve the visitor's existing inline requalifier ownership.
enum PrimeRequalifier {
    Disabled,
    Deferred,
    Active(crate::pass::typecheck_core::PrimeTypeRequalifier),
}

struct SubstituteVisitor<'a> {
    elabs: &'a Elaborations,
    source_bindings:
        Option<&'a std::collections::HashMap<crate::ast::ExpressionOccurrenceId, String>>,
    /// The enclosing module's slash path, used as the per-module
    /// disambiguator on every `Elaborations` lookup. Set once per
    /// visitor instance — substitution is invoked per module by
    /// `substitute_module_with_path` / `substitute_package_file_with_pkg`.
    /// See `Elaborations::type_resolutions` for the keying rationale.
    module_path: String,
    /// The lexical module supplies import identities and type binder names
    /// when replayed types are requalified for Prime.
    module: Option<&'a Module<Lowered>>,
    prime_type_requalifier: PrimeRequalifier,
    type_binders: Vec<String>,
    prime_requalifier_seeded: bool,
    comptime_lowering_mode: ComptimeLoweringMode,
}

#[derive(Clone, Copy)]
enum ComptimeLoweringMode {
    RuntimeErase,
    EvalPreserve,
}

fn module_path_key(path: &crate::ast::ModulePath) -> String {
    path.segments
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

fn comptime_erased_path_kind(
    segments: &[crate::ast::PathSegment],
    allow_unreserved_name: bool,
) -> Option<ComptimeEraseKind> {
    let [single] = segments else {
        return None;
    };
    if !single.as_str().starts_with("__") && !allow_unreserved_name {
        return None;
    }
    match crate::comptime::ComptimeBuiltin::from_public_name(single.as_str())?.runtime_erasure()? {
        crate::comptime::ComptimeRuntimeErasure::Bottom => Some(ComptimeEraseKind::Bottom),
    }
}

#[derive(Clone, Copy)]
enum ComptimeEraseKind {
    Bottom,
}

fn proof_param_name(d: &FnDef<Lowered>) -> Option<&str> {
    for param in &d.sig.params {
        match param {
            crate::ast::SignatureParam::Type(_) => {}
            crate::ast::SignatureParam::Value(value) => {
                if value
                    .ty
                    .as_ref()
                    .is_some_and(crate::comptime::type_is_comptime_proof)
                {
                    return Some(value.name.as_str());
                }
            }
        }
    }
    None
}

fn erased_proof_body(
    sig: &crate::ast::Signature<PrePrime>,
    proof_name: String,
    ret: Type<PrePrime>,
    span: Span,
) -> Expr<PrePrime> {
    assert!(
        sig.value_params()
            .any(|param| param.name == proof_name && matches!(param.ty, Some(Type::Bottom { .. }))),
        "a direct compile-time proof parameter must erase to Bottom"
    );
    Expr::synth_call(
        Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::new("__absurd__".to_owned(), span)],
            meta: Meta::new(span),
            ext: (),
        },
        vec![
            CallArg::Type(ret),
            CallArg::Value(Expr::Path {
                occurrence: Default::default(),
                segments: vec![PathSegment::new(proof_name, span)],
                meta: Meta::new(span),
                ext: (),
            }),
        ],
        span,
    )
}

fn erased_primitive_value(mut scheme: Type<PrePrime>, span: Span) -> Expr<PrePrime> {
    use crate::pass::visit_mut::TypecheckVisitMut;
    struct Binders {
        next: usize,
        stack: Vec<(String, String)>,
    }
    impl TypecheckVisitMut<PrePrime> for Binders {
        fn enter_type_binder(&mut self, param: &mut TypeParam, _scope: Span) {
            let fresh = format!("_Primitive_t{}", self.next);
            self.next += 1;
            self.stack.push((param.name.clone(), fresh.clone()));
            param.rename(fresh);
        }
        fn exit_type_binder(&mut self, _param: &mut TypeParam, _scope: Span) {
            self.stack.pop();
        }
        fn visit_type(&mut self, ty: &mut Type<PrePrime>) {
            if let Type::Path { segments, .. } = ty
                && let [name] = segments.as_mut_slice()
                && let Some((_, fresh)) = self
                    .stack
                    .iter()
                    .rev()
                    .find(|(old, _)| old == name.as_str())
            {
                *name = PathSegment::new(fresh.clone(), name.span);
            }
            crate::pass::visit_mut::walk_type(self, ty);
        }
    }
    Binders {
        next: 0,
        stack: Vec::new(),
    }
    .visit_type(&mut scheme);
    let mut groups = Vec::new();
    while let Type::Forall { param, body, .. } = scheme {
        groups.push(crate::ast::SignatureGroup::Type(vec![param]));
        scheme = *body;
    }
    let Type::Function { param, ret, .. } = scheme else {
        unreachable!("a compile-time value primitive has a proof-bearing function scheme")
    };
    let packet_name = "__primitive_packet__".to_owned();
    let packet = Expr::Path {
        occurrence: Default::default(),
        segments: vec![PathSegment::new(packet_name.clone(), span)],
        meta: Meta::new(span),
        ext: (),
    };
    fn bottom_projection(
        ty: &Type<PrePrime>,
        value: Expr<PrePrime>,
        span: Span,
    ) -> Option<Expr<PrePrime>> {
        match ty {
            Type::Bottom { .. } => Some(value),
            Type::Product { left, right, .. } => {
                for (name, component) in [("__fst__", left), ("__snd__", right)] {
                    let projection = Expr::synth_call(
                        Expr::Path {
                            occurrence: Default::default(),
                            segments: vec![PathSegment::new(name.to_owned(), span)],
                            meta: Meta::new(span),
                            ext: (),
                        },
                        vec![
                            CallArg::Type(*left.clone()),
                            CallArg::Type(*right.clone()),
                            CallArg::Value(value.clone()),
                        ],
                        span,
                    );
                    if let Some(bottom) = bottom_projection(component, projection, span) {
                        return Some(bottom);
                    }
                }
                None
            }
            _ => None,
        }
    }
    let proof = bottom_projection(&param, packet, span)
        .expect("every compiler-only primitive's first semantic packet carries its proof");
    groups.push(crate::ast::SignatureGroup::Value(vec![Param {
        name: packet_name,
        ty: Some(*param),
        pattern: (),
        meta: Meta::new(span),
    }]));
    let body = Expr::synth_call(
        Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::new("__absurd__".to_owned(), span)],
            meta: Meta::new(span),
            ext: (),
        },
        vec![CallArg::Type(*ret.clone()), CallArg::Value(proof)],
        span,
    );
    Expr::FnExpr {
        occurrence: Default::default(),
        sig: Signature::from_groups(groups),
        ret_ty: Some(*ret),
        body: Box::new(body),
        meta: Meta::new(span),
        caps: (),
    }
}

/// Consume the replay artifact's explicit compiler-only dependencies before it
/// enters persistent Prime. Ordinary lexical binders keep their own meaning.
fn erase_replayed_comptime(
    expr: &mut Expr<PrePrime>,
    primitives: &crate::normalization::EvalPrimitiveEnv,
    owner: &str,
    ambient: &[String],
) {
    use crate::pass::visit_mut::TypecheckVisitMut;
    struct Erase<'a> {
        primitives: &'a crate::normalization::EvalPrimitiveEnv,
        owner: &'a str,
        types: Vec<String>,
        values: Vec<String>,
        signature: bool,
    }
    impl TypecheckVisitMut<PrePrime> for Erase<'_> {
        fn enter_type_binder(&mut self, param: &mut TypeParam, _scope: Span) {
            self.types.push(param.name.clone());
        }
        fn exit_type_binder(&mut self, _param: &mut TypeParam, _scope: Span) {
            self.types.pop();
        }
        fn enter_value_binder(&mut self, name: &mut String, _span: Span, _scope: Span) {
            self.values.push(name.clone());
        }
        fn exit_value_binder(&mut self, _name: &mut String, _span: Span, _scope: Span) {
            self.values.pop();
        }
        fn visit_type(&mut self, ty: &mut Type<PrePrime>) {
            if let Type::Path {
                segments,
                args,
                meta,
            } = ty
                && args.is_empty()
                && let [name] = segments.as_slice()
                && !self.types.iter().any(|bound| bound == name.as_str())
                && (if self.signature {
                    crate::comptime::ComptimeBuiltin::from_public_name(name.as_str())
                } else {
                    self.primitives.resolve(self.owner, name.as_str())
                })
                .is_some_and(|builtin| builtin.is_type_name())
            {
                *ty = Type::Bottom { meta: meta.clone() };
                return;
            }
            crate::pass::visit_mut::walk_type(self, ty);
        }
        fn visit_expr(&mut self, expr: &mut Expr<PrePrime>) {
            if let Expr::Path { segments, meta, .. } = expr
                && let [name] = segments.as_slice()
                && !self.values.iter().any(|bound| bound == name.as_str())
            {
                let scheme =
                    if let Some(constructor) = self.primitives.literal(self.owner, name.as_str()) {
                        Some(constructor.signature::<PrePrime>(meta.span))
                    } else {
                        self.primitives
                            .resolve(self.owner, name.as_str())
                            .filter(|builtin| builtin.is_value_name())
                            .and_then(|builtin| {
                                crate::pass::typecheck_core::comptime_scheme::<PrePrime>(
                                    builtin, meta.span,
                                )
                                .expect("validated primitive scheme")
                            })
                            .map(|scheme| scheme.ty.clone_type())
                    };
                if let Some(mut scheme) = scheme {
                    self.signature = true;
                    self.visit_type(&mut scheme);
                    self.signature = false;
                    *expr = erased_primitive_value(scheme, meta.span);
                    return;
                }
            }
            crate::pass::visit_mut::walk_expr(self, expr);
        }
    }
    Erase {
        primitives,
        owner,
        types: ambient.to_vec(),
        values: Vec::new(),
        signature: false,
    }
    .visit_expr(expr);
}

impl<'a> SubstituteVisitor<'a> {
    fn walk_type_rec_group_with_binders(
        &mut self,
        group: &crate::ast::TypeRecGroup<Lowered>,
    ) -> Result<Item<PrePrime>, Error> {
        if group.deferred_rec_labels_diagnostic.is_some() {
            unreachable!(
                "an unmarked recursive labels declaration must be diagnosed before Lowered-to-Prime substitution"
            );
        }
        let mut members = Vec::with_capacity(group.members.len());
        for member in &group.members {
            let binder_mark = self.type_binders.len();
            let walked = match member {
                crate::ast::TypeRecMember::TypeAlias(alias) => {
                    self.type_binders
                        .extend(alias.type_params.iter().map(|param| param.name.clone()));
                    self.walk_alias(alias)
                        .map(crate::ast::TypeRecMember::TypeAlias)
                }
                crate::ast::TypeRecMember::Newtype(newtype) => {
                    self.type_binders.extend(
                        newtype
                            .type_params
                            .iter()
                            .chain(&newtype.existential_params)
                            .map(|param| param.name.clone()),
                    );
                    self.walk_newtype(newtype)
                        .map(crate::ast::TypeRecMember::Newtype)
                }
                crate::ast::TypeRecMember::Labels(_, ext) => match *ext {},
            };
            self.type_binders.truncate(binder_mark);
            members.push(walked?);
        }
        Ok(Item::TypeRecGroup(crate::ast::TypeRecGroup {
            members,
            doc: group.doc.clone(),
            source_layout: group.source_layout.clone(),
            rec_span: group.rec_span,
            open_brace_span: group.open_brace_span,
            close_brace_span: group.close_brace_span,
            deferred_rec_labels_diagnostic: None,
            meta: convert_meta(&group.meta),
        }))
    }

    fn comptime_import_binding_is_active(&self, name: &str) -> bool {
        if self.type_binders.iter().any(|binder| binder == name) {
            return false;
        }
        let Some(module) = self.module else {
            return false;
        };
        if !module
            .imports
            .iter()
            .any(|usage| matches!(usage.kind, ImportKind::Comptime))
        {
            return false;
        }
        true
    }

    fn runtime_comptime_erased_path_kind(
        &self,
        segments: &[crate::ast::PathSegment],
    ) -> Option<ComptimeEraseKind> {
        let allow_unreserved_name = segments
            .first()
            .is_some_and(|name| self.comptime_import_binding_is_active(name.as_str()));
        comptime_erased_path_kind(segments, allow_unreserved_name)
    }

    fn semantic_occurrence(
        &self,
        expr: &Expr<Lowered>,
    ) -> Option<crate::ast::ExpressionOccurrenceId> {
        match self.comptime_lowering_mode {
            ComptimeLoweringMode::RuntimeErase => Some(expr.site().id),
            ComptimeLoweringMode::EvalPreserve => expr.occurrence().assigned_key(),
        }
    }

    fn walk_recorded_type(
        &mut self,
        recorded: &crate::pass::typecheck_core::RecordedTypeResolution<Lowered>,
    ) -> Result<Type<PrePrime>, Error> {
        let rewritten = if recorded.identity_canonical {
            self.seed_prime_type_requalifier();
            if let PrimeRequalifier::Active(requalifier) = &mut self.prime_type_requalifier {
                let bound = self.type_binders.iter().cloned().collect();
                Some(requalifier.rewrite(&recorded.resolved, &bound))
            } else {
                None
            }
        } else {
            None
        };
        let mut ty = self.walk_type(rewritten.as_ref().unwrap_or(&recorded.resolved))?;
        if matches!(
            self.comptime_lowering_mode,
            ComptimeLoweringMode::RuntimeErase
        ) {
            crate::pass::typecheck_core::materialize_type_binder_names(&mut ty, &self.type_binders);
        }
        Ok(ty)
    }

    fn seed_prime_type_requalifier(&mut self) {
        if self.prime_requalifier_seeded {
            return;
        }
        if matches!(self.prime_type_requalifier, PrimeRequalifier::Deferred) {
            self.prime_type_requalifier =
                PrimeRequalifier::Active(crate::pass::typecheck_core::PrimeTypeRequalifier::new(
                    self.module
                        .expect("deferred requalification has its input module"),
                ));
        }
        if let PrimeRequalifier::Active(requalifier) = &mut self.prime_type_requalifier {
            for import in self.elabs.generated_imports_for(&self.module_path) {
                requalifier.add_generated_import(&import.module_path, &import.alias);
            }
            for import in self
                .elabs
                .prime_requalification_imports_for(&self.module_path)
            {
                requalifier.add_generated_import(&import.module_path, &import.alias);
            }
            for alias in self.elabs.generated_type_aliases_for(&self.module_path) {
                requalifier.reserve_local_alias(alias);
            }
        }
        self.prime_requalifier_seeded = true;
    }

    fn missing_elaboration_expr(
        &mut self,
        _e: &Expr<Lowered>,
        message: &'static str,
    ) -> Result<Expr<PrePrime>, Error> {
        unreachable!("{message}")
    }

    fn missing_type_infer(&self, _span: Span, message: &'static str) -> Type<PrePrime> {
        unreachable!("{message}")
    }

    fn rewrite_prime_import(&self, u: &Import) -> Option<Import> {
        match &u.kind {
            ImportKind::Comptime => matches!(
                self.comptime_lowering_mode,
                ComptimeLoweringMode::EvalPreserve
            )
            .then(|| u.clone()),
            ImportKind::Selective { items, .. } => {
                let kept: Vec<ImportItem> = items
                    .iter()
                    .filter(|item| match item {
                        ImportItem::Name { macro_span, .. } => macro_span.is_none(),
                        ImportItem::OperatorPattern { .. } => true,
                    })
                    .cloned()
                    .collect();
                if kept.is_empty() {
                    return None;
                }
                let mut out = u.clone();
                if let ImportKind::Selective { items, .. } = &mut out.kind {
                    *items = kept;
                }
                Some(out)
            }
            _ => Some(u.clone()),
        }
    }

    fn numeric_value(&self, expr: &Expr<Lowered>) -> Option<&str> {
        self.semantic_occurrence(expr)
            .and_then(|id| self.elabs.literal_resolution_for(&self.module_path, id))
            .and_then(|(_, value)| value.as_deref())
    }

    fn rewrite_literal(
        &mut self,
        annotation: &Option<Type<Lowered>>,
        meta: &Meta<Lowered>,
        occurrence: Option<crate::ast::ExpressionOccurrenceId>,
        keep: impl FnOnce(Type<PrePrime>, Meta<PrePrime>) -> Expr<PrePrime>,
    ) -> Result<Expr<PrePrime>, Error> {
        let annotation = self.walk_literal_annotation(annotation, meta.span, occurrence)?;
        let meta = convert_meta(meta);
        Ok(keep(annotation, meta))
    }

    fn materialize_call_stage(
        &mut self,
        callee: Expr<PrePrime>,
        args: &[CallArg<Lowered>],
        call_span: Span,
        occurrence: Option<crate::ast::ExpressionOccurrenceId>,
    ) -> Result<Expr<PrePrime>, Error> {
        // The table is external to the visitor: recursive conversion can
        // borrow its recorded types and call plan without copying their trees.
        let elabs = self.elabs;
        let residual_eta =
            occurrence.and_then(|id| elabs.call_residual_eta_type_for(&self.module_path, id));
        let implicit_unit_slots = occurrence
            .and_then(|id| elabs.call_implicit_unit_slots_for(&self.module_path, id))
            .unwrap_or_default();
        use crate::pass::typecheck_core::{CanonicalCallCursor, CanonicalCallSlot, WrittenCallArg};
        let mut new_args: Vec<CallArg<PrePrime>> = Vec::with_capacity(args.len() + 2);
        let mut cursor = CanonicalCallCursor::default();
        loop {
            let arg = args.get(cursor.written());
            let written = arg.map(|arg| match arg {
                CallArg::Type(Type::Infer { .. }) => WrittenCallArg::Infer,
                CallArg::Type(_) => WrittenCallArg::Type,
                CallArg::Value(_) => WrittenCallArg::Value,
            });
            let implicit = implicit_unit_slots
                .binary_search(&cursor.canonical())
                .is_ok();
            let resolution = (!implicit)
                .then(|| {
                    occurrence.and_then(|id| {
                        elabs.type_resolution_for(&self.module_path, id, cursor.canonical())
                    })
                })
                .flatten();
            match cursor.advance(written, implicit, resolution.is_some()) {
                Some(CanonicalCallSlot::ImplicitUnit) => {
                    new_args.push(CallArg::Value(Expr::Unit {
                        source_trivia: Default::default(),
                        occurrence: Default::default(),
                        meta: Meta::new(call_span),
                    }))
                }
                Some(CanonicalCallSlot::ResolvedType { .. }) => {
                    new_args.push(CallArg::Type(self.walk_recorded_type(
                        resolution.expect("resolved canonical slot has a recorded type"),
                    )?));
                }
                Some(CanonicalCallSlot::Written) => {
                    new_args.push(
                        match arg.expect("written canonical slot has a source argument") {
                            CallArg::Type(Type::Infer { meta, .. }) => {
                                CallArg::Type(self.missing_type_infer(
                                    meta.span,
                                    "every `Type::Infer` in a call's args list must have a \
                             recorded resolution — `apply_polymorphic_function` \
                             records one per `_`",
                                ))
                            }
                            CallArg::Type(ty) => CallArg::Type(self.walk_type(ty)?),
                            CallArg::Value(value) => CallArg::Value(self.walk_expr(value)?),
                        },
                    );
                }
                None => break,
            }
        }
        if args.is_empty() && implicit_unit_slots.is_empty() {
            // A source-empty packet is itself the ordinary spelling of one
            // Unit value application. Typed calls reach this boundary with an
            // implicit-Unit marker and are handled in the canonical-slot loop
            // above. Evaluator inputs can intentionally be untyped, so retain
            // the same source rule here without treating a written type-only
            // packet as an implicit value application.
            new_args.push(CallArg::Value(Expr::Unit {
                source_trivia: Default::default(),
                occurrence: Default::default(),
                meta: Meta::new(call_span),
            }));
        }
        for arg in &mut new_args {
            if let CallArg::Type(ty) = arg {
                crate::pass::typecheck_core::canonicalize_type_expression_function_abi(ty);
            }
        }
        let Some(residual_eta) = residual_eta else {
            return Ok(self.call_with_recorded_splits(callee, new_args, call_span, occurrence));
        };
        let residual_ty = self.walk_recorded_type(residual_eta)?;
        let Some(plan) = first_eta_layer_from_residual_type(&residual_ty) else {
            return Ok(self.call_with_recorded_splits(callee, new_args, call_span, occurrence));
        };
        let prefix = self.call_with_recorded_splits(callee, new_args, call_span, occurrence);

        // Every applied prefix is evaluated when the residual value is formed
        // exactly once. Looking up a named function may itself be pure, but a
        // type application is an ordinary application boundary: its body may
        // perform effects before returning the remaining callable layers.
        // Bind the result before eta-wrapping only those residual layers so a
        // later optimizer cannot move the application under the fresh lambda.
        let callee_name = "__eta_callee__".to_owned();
        let callee_path = Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::new(callee_name.clone(), call_span)],
            meta: Meta::new(call_span),
            ext: (),
        };
        Ok(Expr::Let {
            occurrence: Default::default(),
            name: callee_name,
            name_span: call_span,
            ty: Some(residual_ty),
            pattern: (),
            value: Box::new(prefix),
            body: Box::new(eta_materialize_pre_prime(
                &plan,
                EtaTerminal::Call {
                    callee: callee_path,
                    args: Vec::new(),
                },
                call_span,
            )),
            meta: Meta::new(call_span),
        })
    }

    fn call_with_recorded_splits(
        &self,
        callee: Expr<PrePrime>,
        args: Vec<CallArg<PrePrime>>,
        span: Span,
        occurrence: Option<crate::ast::ExpressionOccurrenceId>,
    ) -> Expr<PrePrime> {
        let Some(splits) =
            occurrence.and_then(|id| self.elabs.call_splits_for(&self.module_path, id))
        else {
            return Expr::synth_call(callee, args, span);
        };
        let mut current = callee;
        let mut start = 0usize;
        let arg_count = args.len();
        let mut args = args.into_iter();
        for split in splits {
            if *split <= start || *split >= arg_count {
                continue;
            }
            let layer = args.by_ref().take(*split - start).collect();
            current = Expr::synth_call(current, layer, span);
            start = *split;
        }
        Expr::synth_call(current, args.collect(), span)
    }

    fn walk_ordered_signature_params(
        &mut self,
        params: &[crate::ast::SignatureParam<Lowered>],
    ) -> Result<Vec<crate::ast::SignatureParam<PrePrime>>, Error> {
        let mut walked = Vec::with_capacity(params.len());
        for param in params {
            match param {
                crate::ast::SignatureParam::Type(param) => {
                    self.type_binders.push(param.name.clone());
                    walked.push(crate::ast::SignatureParam::Type(param.clone()));
                }
                crate::ast::SignatureParam::Value(param) => {
                    walked.push(crate::ast::SignatureParam::Value(self.walk_param(param)?));
                }
            }
        }
        Ok(walked)
    }

    fn walk_let_with_recorded_type(
        &mut self,
        e: &Expr<Lowered>,
        recorded_ty: Option<&crate::pass::typecheck_core::RecordedTypeResolution<Lowered>>,
    ) -> Result<Expr<PrePrime>, Error> {
        let Expr::Let {
            occurrence: _,
            name,
            name_span,
            ty,
            pattern: (),
            value,
            body,
            meta,
        } = e
        else {
            unreachable!()
        };
        let ty = match recorded_ty {
            Some(recorded_ty) => Some(self.walk_recorded_type(recorded_ty)?),
            None => match ty {
                Some(ty) if !crate::pass::typecheck_core::type_contains_infer(ty) => {
                    Some(self.walk_type(ty)?)
                }
                _ => None,
            },
        };
        Ok(Expr::Let {
            occurrence: Default::default(),
            name: name.clone(),
            name_span: *name_span,
            ty,
            pattern: (),
            value: Box::new(self.walk_expr(value)?),
            body: Box::new(self.walk_expr(body)?),
            meta: convert_meta(meta),
        })
    }

    fn walk_recorded_elaboration(
        &mut self,
        elaboration: &RecordedElaboration,
    ) -> Result<Expr<PrePrime>, Error> {
        match elaboration {
            RecordedElaboration::Lowered(expr) => self.walk_expr(expr),
            RecordedElaboration::UncheckedPrime {
                expression,
                primitives,
            } => {
                let mut expression = convert_expr::<UncheckedPrime, PrePrime>(expression);
                if matches!(
                    self.comptime_lowering_mode,
                    ComptimeLoweringMode::RuntimeErase
                ) && let Some(primitives) = primitives
                {
                    erase_replayed_comptime(
                        &mut expression,
                        primitives,
                        &self.module_path,
                        &self.type_binders,
                    );
                }
                Ok(expression)
            }
        }
    }
}

impl<'a> LoweredToPrePrime for SubstituteVisitor<'a> {
    fn rewrite_expr_comptime_literal(
        &mut self,
        e: &Expr<Lowered>,
    ) -> Result<Expr<PrePrime>, Error> {
        let Expr::ComptimeLiteral { proof, meta, .. } = e else {
            unreachable!()
        };
        let plan = self
            .elabs
            .comptime_literal_for(
                &self.module_path,
                self.semantic_occurrence(e)
                    .expect("checked literal has an occurrence"),
            )
            .expect("checked compile-time literal has a constructor plan");
        let callee = match self.comptime_lowering_mode {
            ComptimeLoweringMode::EvalPreserve => plan.binding.clone(),
            ComptimeLoweringMode::RuntimeErase => "__absurd__".to_owned(),
        };
        let mut args = Vec::new();
        if matches!(
            self.comptime_lowering_mode,
            ComptimeLoweringMode::RuntimeErase
        ) {
            let result = Type::synth_path(
                vec![plan.constructor.result_builtin().public_name().to_owned()],
                Vec::new(),
                meta.span,
            );
            args.push(CallArg::Type(self.walk_type(&result)?));
        }
        args.push(CallArg::Value(self.walk_expr(proof)?));
        Ok(Expr::synth_call(
            Expr::Path {
                occurrence: Default::default(),
                segments: vec![PathSegment::new(callee, meta.span)],
                meta: convert_meta(meta),
                ext: (),
            },
            args,
            meta.span,
        ))
    }

    fn completed_source_binding(&self, source: &Expr<Lowered>) -> Option<Expr<PrePrime>> {
        let name = self
            .source_bindings?
            .get(&source.occurrence().assigned_key()?)?;
        Some(Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::new(name.clone(), source.span())],
            meta: Meta::new(source.span()),
            ext: (),
        })
    }

    // The visit_* overrides below re-destructure their variant with a
    // let-else; the walk_* dispatcher routes each variant to its own
    // visit_*, so that let-else can fail only if a dispatcher routed
    // the wrong variant — a bug in this file, hence the bare
    // `unreachable!()`.
    fn walk_module(&mut self, module: &Module<Lowered>) -> Result<Module<PrePrime>, Error> {
        self.seed_prime_type_requalifier();
        let mut items: Vec<Item<PrePrime>> = Vec::with_capacity(module.items.len());
        for item in &module.items {
            let binder_mark = self.type_binders.len();
            match item {
                Item::TypeAlias(alias) => self
                    .type_binders
                    .extend(alias.type_params.iter().map(|param| param.name.clone())),
                Item::Newtype(newtype) => self.type_binders.extend(
                    newtype
                        .type_params
                        .iter()
                        .chain(&newtype.existential_params)
                        .map(|param| param.name.clone()),
                ),
                _ => {}
            }
            let walked = match item {
                Item::TypeRecGroup(group) => self
                    .walk_type_rec_group_with_binders(group)
                    .map(|group| vec![group]),
                _ => self.walk_item(item),
            };
            self.type_binders.truncate(binder_mark);
            items.extend(walked?);
        }
        let mut imports: Vec<Import> = module
            .imports
            .iter()
            .filter_map(|u| self.rewrite_prime_import(u))
            .collect();
        if (module
            .items
            .iter()
            .any(|item| matches!(item, Item::FnDef(d) if proof_param_name(d).is_some()))
            || self
                .elabs
                .comptime_literals_for(&self.module_path)
                .next()
                .is_some()
            || (matches!(
                self.comptime_lowering_mode,
                ComptimeLoweringMode::RuntimeErase
            ) && module
                .imports
                .iter()
                .any(|import| matches!(import.kind, ImportKind::Comptime))))
            && !imports
                .iter()
                .any(|u| matches!(u.kind, ImportKind::Intrinsics))
        {
            imports.push(Import {
                trailing_trivia: Vec::new(),
                kind: ImportKind::Intrinsics,
                span: Span::new(0, 0),
                leading_trivia: Vec::new(),
            });
        }
        for import in self.elabs.generated_imports_for(&self.module_path) {
            if !imports
                .iter()
                .any(|u| qualified_import_matches_generated(u, import))
            {
                imports.push(generated_import_clause(import));
            }
        }
        for import in self
            .elabs
            .prime_requalification_imports_for(&self.module_path)
        {
            if !imports
                .iter()
                .any(|import_| qualified_import_matches_generated(import_, import))
            {
                imports.push(generated_import_clause(import));
            }
        }
        if let PrimeRequalifier::Active(requalifier) = &mut self.prime_type_requalifier {
            imports.extend(requalifier.take_imports());
            let mut aliases = self
                .elabs
                .generated_type_aliases_for(&self.module_path)
                .to_vec();
            aliases.extend(requalifier.take_local_aliases());
            aliases.sort_by(|a, b| a.alias.cmp(&b.alias));
            aliases.dedup_by(|a, b| a.alias == b.alias);
            if !aliases.is_empty() {
                let host_names = items
                    .iter()
                    .filter_map(|item| match item {
                        Item::HostType(host) => Some(host.name.clone()),
                        _ => None,
                    })
                    .collect::<std::collections::HashSet<_>>();
                let mut aliases_after_hosts = std::collections::HashMap::<_, Vec<_>>::new();
                let mut ordered_items = Vec::with_capacity(items.len() + aliases.len());
                for plan in aliases {
                    let host_owner = host_names
                        .contains(&plan.nominal)
                        .then(|| plan.nominal.clone());
                    let args = plan
                        .params
                        .iter()
                        .map(|param| {
                            Type::synth_path(vec![param.name.clone()], Vec::new(), param.span)
                        })
                        .collect();
                    let alias = Item::TypeAlias(crate::ast::TypeAlias {
                        vis: crate::ast::Visibility::Private,
                        name: plan.alias,
                        name_span: plan.span,
                        type_params: plan.params,
                        body: Type::synth_path(vec![plan.nominal], args, plan.span),
                        meta: Meta::new(plan.span),
                        editable_span: None,
                        doc: None,
                    });
                    if let Some(host_owner) = host_owner {
                        aliases_after_hosts
                            .entry(host_owner)
                            .or_default()
                            .push(alias);
                    } else {
                        ordered_items.push(alias);
                    }
                }
                // A local host must precede every alias naming it in fresh Prime input.
                for item in items {
                    let aliases = match &item {
                        Item::HostType(host) => aliases_after_hosts.remove(&host.name),
                        _ => None,
                    };
                    ordered_items.push(item);
                    if let Some(aliases) = aliases {
                        ordered_items.extend(aliases);
                    }
                }
                items = ordered_items;
            }
        }

        Ok(Module {
            path: module.path.clone(),
            imports,
            items,
            meta: crate::ast::convert_meta(&module.meta),
            doc: module.doc.clone(),
        })
    }

    fn walk_fn_def(&mut self, d: &FnDef<Lowered>) -> Result<FnDef<PrePrime>, Error> {
        let binder_mark = self.type_binders.len();
        let result = (|| {
            let sig = crate::ast::Signature::from_parts(
                self.walk_ordered_signature_params(&d.sig.params)?,
                d.sig.groups.clone(),
            );
            let ret = self.walk_type(&d.ret)?;
            let proof_name = matches!(
                self.comptime_lowering_mode,
                ComptimeLoweringMode::RuntimeErase
            )
            .then(|| proof_param_name(d).map(str::to_owned))
            .flatten();
            let body = if let Some(proof_name) = proof_name {
                erased_proof_body(&sig, proof_name, ret.clone(), d.body.span())
            } else {
                self.walk_expr(&d.body)?
            };
            Ok(FnDef {
                vis: d.vis.clone(),
                purity: d.purity,
                name: d.name.clone(),
                sig,
                ret,
                ret_elided: (),
                body,
                meta: crate::ast::convert_meta(&d.meta),
                doc: d.doc.clone(),
            })
        })();
        self.type_binders.truncate(binder_mark);
        result
    }

    fn walk_host_fn(
        &mut self,
        h: &crate::ast::HostFn<Lowered>,
    ) -> Result<crate::ast::HostFn<PrePrime>, Error> {
        let binder_mark = self.type_binders.len();
        let result = (|| {
            let mut params = Vec::with_capacity(h.params.len());
            for param in &h.params {
                match param {
                    crate::ast::HostFnParam::Type(param) => {
                        self.type_binders.push(param.name.clone());
                        params.push(crate::ast::HostFnParam::Type(param.clone()));
                    }
                    crate::ast::HostFnParam::Value(param) => {
                        params.push(crate::ast::HostFnParam::Value(
                            crate::ast::HostFnValueParam {
                                name: param.name.clone(),
                                ty: self.walk_type(&param.ty)?,
                                meta: convert_meta(&param.meta),
                            },
                        ));
                    }
                }
            }
            Ok(crate::ast::HostFn {
                name: h.name.clone(),
                params,
                param_groups: h.param_groups.clone(),
                source_trivia: Default::default(),
                ret: self.walk_type(&h.ret)?,
                meta: convert_meta(&h.meta),
                doc: h.doc.clone(),
            })
        })();
        self.type_binders.truncate(binder_mark);
        result
    }

    fn visit_expr_path(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let Expr::Path { segments, meta, .. } = e else {
            unreachable!()
        };
        if matches!(
            self.comptime_lowering_mode,
            ComptimeLoweringMode::RuntimeErase
        ) && let Some(name) = self
            .semantic_occurrence(e)
            .and_then(|id| self.elabs.primitive_value_for(&self.module_path, id))
            && let Some(builtin) = crate::comptime::ComptimeBuiltin::from_public_name(name)
            && builtin.is_value_name()
        {
            let scheme =
                crate::pass::typecheck_core::comptime_scheme::<Lowered>(builtin, meta.span)?
                    .expect("a resolved compile-time primitive has a function scheme")
                    .ty;
            let scheme = self.walk_type(scheme.as_type())?;
            return Ok(erased_primitive_value(scheme, meta.span));
        }
        let path = Expr::Path {
            occurrence: Default::default(),
            segments: segments.clone(),
            meta: convert_meta(meta),
            ext: (),
        };
        if segments.len() == 1
            && matches!(
                segments[0].as_str(),
                "__if_then_else__" | "__and__" | "__or__"
            )
            && let Some(resolved) = self
                .semantic_occurrence(e)
                .and_then(|id| self.elabs.type_resolution_for(&self.module_path, id, 0))
        {
            let ty = self.walk_recorded_type(resolved)?;
            return Ok(Expr::synth_call(path, vec![CallArg::Type(ty)], meta.span));
        }
        Ok(path)
    }

    fn visit_expr_fn(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let occurrence = self.semantic_occurrence(e);
        let Expr::FnExpr {
            occurrence: _,
            sig,
            ret_ty,
            body,
            meta,
            caps: _,
        } = e
        else {
            unreachable!()
        };
        let binder_mark = self.type_binders.len();
        let result = (|| {
            let mut value_index = 0usize;
            let mut proof_name = None;
            let mut params = Vec::with_capacity(sig.params.len());
            for p in &sig.params {
                match p {
                    crate::ast::SignatureParam::Type(tp) => {
                        self.type_binders.push(tp.name.clone());
                        params.push(crate::ast::SignatureParam::Type(tp.clone()));
                    }
                    crate::ast::SignatureParam::Value(v) => {
                        let recorded = occurrence.and_then(|id| {
                            self.elabs
                                .fn_param_type_for(&self.module_path, id, value_index)
                        });
                        if proof_name.is_none()
                            && matches!(
                                self.comptime_lowering_mode,
                                ComptimeLoweringMode::RuntimeErase
                            )
                            && (v
                                .ty
                                .as_ref()
                                .is_some_and(crate::comptime::type_is_comptime_proof)
                                || recorded.is_some_and(|ty| {
                                    crate::comptime::type_is_comptime_proof(&ty.resolved)
                                }))
                        {
                            proof_name = Some(v.name.clone());
                        }
                        let ty = match &v.ty {
                            Some(t) if !crate::pass::typecheck_core::type_contains_infer(t) => {
                                Some(self.walk_type(t)?)
                            }
                            _ => occurrence
                                .and_then(|id| {
                                    self.elabs
                                        .fn_param_type_for(&self.module_path, id, value_index)
                                })
                                .cloned()
                                .map(|ty| self.walk_recorded_type(&ty))
                                .transpose()?,
                        };
                        params.push(crate::ast::SignatureParam::Value(crate::ast::Param {
                            name: v.name.clone(),
                            ty,
                            pattern: (),
                            meta: convert_meta(&v.meta),
                        }));
                        value_index += 1;
                    }
                }
            }
            let ret_ty = match ret_ty {
                Some(t) if !crate::pass::typecheck_core::type_contains_infer(t) => {
                    Some(self.walk_type(t)?)
                }
                _ => None,
            };
            let sig = crate::ast::Signature::from_parts(params, sig.groups.clone());
            let body = if let Some(proof_name) = proof_name {
                let result = match &ret_ty {
                    Some(ret) => ret.clone(),
                    None => {
                        let recorded = occurrence.and_then(|id| self.elabs.proof_fn_body_type_for(&self.module_path, id))
                            .expect("a checked proof-gated lambda must retain its body type for erasure");
                        self.walk_recorded_type(recorded)?
                    }
                };
                erased_proof_body(&sig, proof_name, result, body.span())
            } else {
                self.walk_expr(body)?
            };
            Ok(Expr::FnExpr {
                occurrence: Default::default(),
                sig,
                ret_ty,
                body: Box::new(body),
                meta: convert_meta(meta),
                caps: (),
            })
        })();
        self.type_binders.truncate(binder_mark);
        result
    }

    fn visit_expr_let(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        self.walk_let_with_recorded_type(e, None)
    }

    // ---- Elaboration substitutions ----------------------------------

    fn rewrite_expr_rec_order(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        if let Some((replacement, binding_type)) =
            self.elabs.rec_order_runtime_for(&self.module_path, e)
        {
            assert!(
                matches!(replacement, Expr::Let { ty: None, .. }),
                "runtime recursive-order replacement must leave its root let type to the paired artifact"
            );
            return self.walk_let_with_recorded_type(replacement, Some(binding_type));
        }
        if let Some(elab) = self.elabs.elaboration_for(&self.module_path, e) {
            return self.walk_recorded_elaboration(elab);
        }
        self.missing_elaboration_expr(
            e,
            "recursive ordering carrier reached substitution without a recorded runtime expression — typer bug",
        )
    }

    fn rewrite_expr_rec_quote(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        if let Some(elab) = self.elabs.elaboration_for(&self.module_path, e) {
            return self.walk_recorded_elaboration(elab);
        }
        self.missing_elaboration_expr(
            e,
            "recursive quotation reached substitution without a recorded runtime expression — typer bug",
        )
    }

    fn rewrite_expr_user_macro(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        if let Some(elab) = self.elabs.elaboration_for(&self.module_path, e) {
            let mut output = self.walk_recorded_elaboration(elab)?;
            if let Some(first) = self
                .semantic_occurrence(e)
                .and_then(|id| self.elabs.macro_output_stage_for(&self.module_path, id))
            {
                let Expr::UserMacro { stages, .. } = e else {
                    unreachable!()
                };
                for stage in &stages[first..] {
                    output = self.materialize_call_stage(
                        output,
                        &stage.args,
                        stage.meta.span,
                        Some(stage.occurrence.key()),
                    )?;
                }
            }
            return Ok(output);
        }
        self.missing_elaboration_expr(
            e,
            "user macro reached substitution without a recorded elaboration — typer bug",
        )
    }

    fn rewrite_expr_ufcs(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        if let Some(elab) = self.elabs.elaboration_for(&self.module_path, e) {
            return self.walk_recorded_elaboration(elab);
        }
        self.missing_elaboration_expr(
            e,
            "dot-splice call reached substitution without a recorded replacement — typer bug",
        )
    }

    fn rewrite_type_infer(&mut self, meta: &Meta<Lowered>) -> Result<Type<PrePrime>, Error> {
        Ok(self.missing_type_infer(
            meta.span,
            "Type::Infer reached substitute_type without a recorded resolution — \
             apply_polymorphic_function must record one per `_` placeholder",
        ))
    }

    fn walk_literal_annotation(
        &mut self,
        annotation: &Option<Type<Lowered>>,
        span: Span,
        occurrence: Option<crate::ast::ExpressionOccurrenceId>,
    ) -> Result<Type<PrePrime>, Error> {
        // The typer records the resolved nominal identity for every typed
        // literal, including one with an explicit source annotation. Use that
        // identity's checked lexical spelling at the substitution boundary so
        // explicit and inferred occurrences retain the same nominal type.
        if let Some(resolved) = self.resolved_literal(occurrence)? {
            return Ok(resolved);
        }
        if let Some(t) = annotation
            && !matches!(t, Type::Infer { .. })
        {
            return self.walk_type(t);
        }
        // Nothing pinned the host type. On the eval path
        // the literal is a still-explicit compile-time hole — the
        // evaluator reduces it by value, and the private staging term is
        // consumed into `UncheckedPrime`, where the hole becomes a bare
        // literal (`convert_expr` strips this marker). A
        // genuinely ambiguous or context-free literal (spine `err`
        // diagnostics, a hand-built eval term) legitimately stays
        // unpinned there; it never survives into a validated `Prime` module.
        // On the module path (`Strict`) the typer must have
        // pinned it, so an unpinned literal is a broken contract.
        match self.comptime_lowering_mode {
            ComptimeLoweringMode::EvalPreserve => Ok(crate::ast::residual_literal_type_hole(span)),
            ComptimeLoweringMode::RuntimeErase => unreachable!(
                "literal at {span:?} reached the Lowered → PrePrime substitution with no \
                 recorded resolution or `[Type]` annotation; the typer must pin every \
                 literal's host type before \
                 substitution"
            ),
        }
    }

    /// Read the typer's literal resolution. At the PrePrime boundary this is
    /// authoritative for explicitly annotated and contextually inferred literals:
    /// each carries one canonical nominal identity.
    fn resolved_literal(
        &mut self,
        occurrence: Option<crate::ast::ExpressionOccurrenceId>,
    ) -> Result<Option<Type<PrePrime>>, Error> {
        let recorded = occurrence
            .and_then(|id| self.elabs.literal_resolution_for(&self.module_path, id))
            .map(|(ty, _)| ty.clone());
        recorded.map(|ty| self.walk_recorded_type(&ty)).transpose()
    }

    fn visit_expr_str_lit(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let Expr::StrLit {
            occurrence: _,
            value,
            annotation,
            meta,
        } = e
        else {
            unreachable!()
        };
        self.rewrite_literal(
            annotation,
            meta,
            self.semantic_occurrence(e),
            |annotation, meta| Expr::StrLit {
                occurrence: Default::default(),
                value: value.clone(),
                annotation,
                meta,
            },
        )
    }

    fn visit_expr_int_lit(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let Expr::IntLit {
            occurrence: _,
            diagnostic: _,
            digits,
            annotation,
            meta,
        } = e
        else {
            unreachable!()
        };
        let digits = self.numeric_value(e).map(str::to_owned).unwrap_or_else(|| {
            crate::numeric_literal::Integer::parse(digits)
                .map_or_else(|| digits.clone(), crate::numeric_literal::Integer::decimal)
        });
        self.rewrite_literal(
            annotation,
            meta,
            self.semantic_occurrence(e),
            |annotation, meta| Expr::IntLit {
                occurrence: Default::default(),
                diagnostic: Default::default(),
                digits,
                annotation,
                meta,
            },
        )
    }

    fn visit_expr_float_lit(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let Expr::FloatLit {
            occurrence: _,
            digits,
            annotation,
            meta,
        } = e
        else {
            unreachable!()
        };
        let digits = self.numeric_value(e).unwrap_or(digits).to_owned();
        self.rewrite_literal(
            annotation,
            meta,
            self.semantic_occurrence(e),
            |annotation, meta| Expr::FloatLit {
                occurrence: Default::default(),
                digits,
                annotation,
                meta,
            },
        )
    }

    fn visit_expr_bool_lit(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let Expr::BoolLit {
            occurrence: _,
            value,
            annotation,
            meta,
        } = e
        else {
            unreachable!()
        };
        self.rewrite_literal(
            annotation,
            meta,
            self.semantic_occurrence(e),
            |annotation, meta| Expr::BoolLit {
                occurrence: Default::default(),
                value: *value,
                annotation,
                meta,
            },
        )
    }

    fn visit_type_path(&mut self, ty: &Type<Lowered>) -> Result<Type<PrePrime>, Error> {
        let Type::Path {
            segments,
            args,
            meta,
        } = ty
        else {
            unreachable!()
        };
        if matches!(
            self.comptime_lowering_mode,
            ComptimeLoweringMode::RuntimeErase
        ) && args.is_empty()
            && let Some(kind) = self.runtime_comptime_erased_path_kind(segments)
        {
            return Ok(match kind {
                ComptimeEraseKind::Bottom => Type::Bottom {
                    meta: crate::ast::convert_meta(meta),
                },
            });
        }
        let args: Vec<Type<PrePrime>> = args
            .iter()
            .map(|a| self.walk_type(a))
            .collect::<Result<_, _>>()?;
        Ok(Type::synth_path_segments(segments.clone(), args, meta.span))
    }

    fn visit_type_forall(&mut self, ty: &Type<Lowered>) -> Result<Type<PrePrime>, Error> {
        let Type::Forall { param, body, meta } = ty else {
            unreachable!()
        };
        let binder_mark = self.type_binders.len();
        self.type_binders.push(param.name.clone());
        let body = self.walk_type(body);
        self.type_binders.truncate(binder_mark);
        Ok(Type::Forall {
            param: param.clone(),
            body: Box::new(body?),
            meta: convert_meta(meta),
        })
    }

    fn rewrite_item_equiv(&mut self, _e: &Equiv<Lowered>) -> Result<Vec<Item<PrePrime>>, Error> {
        // `equiv` items are filtered here — they have no execution
        // semantics. The `kio test` runner reads them directly from
        // the typed Lowered module before this pass runs.
        Ok(vec![])
    }

    fn rewrite_item_macro(
        &mut self,
        _e: &crate::ast::UserMacroDef<Lowered>,
    ) -> Result<Vec<Item<PrePrime>>, Error> {
        Ok(vec![])
    }

    // ---- `Call`: canonical-slot infer-arg insertion -----------------

    fn visit_expr_call(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let occurrence = self.semantic_occurrence(e);
        let Expr::Call {
            callee, args, meta, ..
        } = e
        else {
            unreachable!()
        };
        let callee = self.walk_expr(callee)?;
        self.materialize_call_stage(callee, args, meta.span, occurrence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Item;
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::parse;
    use crate::pass::typecheck_core::{
        GoalContextCapability, GoalEscape, GoalOrigin, GoalOwnerKind, GoalRole, GoalSolutionPolicy,
        GoalStore, GoalTypeContext, RigidScope, ScopedType,
    };
    use crate::pass::typecheck_full::check_package_preserving_normalization;
    use crate::pipeline::Pipeline;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    /// Derive the on-disk file path for a module from its declared
    /// `module a/b;` path: `a/b.kio`. Per `specs/package.md`
    /// § Module-name rules, the declared segments equal the file's
    /// path relative to the package root — the package name is not
    /// prepended.
    fn module_file_path(module_path: &str) -> PathBuf {
        let segs: Vec<&str> = module_path.split('/').collect();
        let mut path = PathBuf::new();
        for seg in &segs[..segs.len().saturating_sub(1)] {
            path.push(seg);
        }
        let stem = segs.last().copied().unwrap_or("module");
        path.push(format!("{stem}.kio"));
        path
    }

    fn check_and_substitute(package: &Package<Lowered>) -> Package<Prime> {
        let checked = check_package_preserving_normalization(package).expect("package typechecks");
        checked.substituted().into_parts().0
    }

    /// Parse `src` as a single regular module, lower it, build the
    /// package, run the typer, and substitute. Returns the resulting
    /// `Module<Prime>` for the module at `module_path`. Panics on any
    /// pipeline failure — exercise those paths in dedicated tests.
    fn substitute_one(module_path: &str, src: &str) -> Module<Prime> {
        let parsed = parse(src).expect("parse");
        let (lowered_modules, _lowered_package_file) =
            FullPipeline::lower_package(vec![(module_file_path(module_path), parsed)], None)
                .expect("lower_package");
        let package = Package::build(Path::new(""), lowered_modules, None).expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        let prime = check_and_substitute(&package);
        prime
            .module(module_path)
            .expect("module present")
            .module
            .clone()
    }

    fn sequence_supported_package(module_path: &str, src: &str) -> Package<Lowered> {
        let parsed = parse(src).expect("parse fixture");
        let sequence = parse(include_str!(
            "../../../../test-data/poc/macros/workdir/sequence.kio"
        ))
        .expect("parse canonical sequence provider");
        let (lowered_modules, _) = FullPipeline::lower_package(
            vec![
                (module_file_path(module_path), parsed),
                (PathBuf::from("sequence.kio"), sequence),
            ],
            None,
        )
        .expect("lower sequence-supported package");
        let package = Package::build(Path::new(""), lowered_modules, None)
            .expect("build sequence-supported package");
        package.resolve_imports().expect("resolve sequence import");
        package
            .check_in_body_resolution()
            .expect("resolve sequence-supported bodies");
        package
    }

    fn substitute_sequence_one(module_path: &str, src: &str) -> Module<Prime> {
        let package = sequence_supported_package(module_path, src);
        check_and_substitute(&package)
            .module(module_path)
            .expect("fixture module present")
            .module
            .clone()
    }

    fn substitute_sequence_eval_fn(
        module_path: &str,
        src: &str,
        name: &str,
    ) -> FnDef<UncheckedPrime> {
        let package = sequence_supported_package(module_path, src);
        let checked = check_package_preserving_normalization(&package)
            .expect("sequence-supported package typechecks");
        let module = &checked
            .package()
            .module(module_path)
            .expect("fixture module present")
            .module;
        let def = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == name => Some(def),
                _ => None,
            })
            .unwrap_or_else(|| panic!("fixture function present"));
        substitute_fn_def_for_eval(
            def,
            checked.elaborations(),
            module_path,
            Some(checked.package()),
            None,
        )
    }

    fn substitute_many(sources: Vec<(&str, &str)>) -> Package<Prime> {
        let parsed = sources
            .into_iter()
            .map(|(module_path, src)| {
                (
                    module_file_path(module_path),
                    parse(src).expect("parse module"),
                )
            })
            .collect::<Vec<_>>();
        let (lowered_modules, _lowered_package_file) =
            FullPipeline::lower_package(parsed, None).expect("lower_package");
        let package = Package::build(Path::new(""), lowered_modules, None).expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        check_and_substitute(&package)
    }

    fn lowered_one(module_path: &str, src: &str) -> Module<Lowered> {
        let parsed = parse(src).expect("parse");
        let (mut lowered_modules, _) =
            FullPipeline::lower_package(vec![(module_file_path(module_path), parsed)], None)
                .expect("lower package");
        lowered_modules.pop().expect("one lowered module").1
    }

    #[test]
    fn substitution_erases_explicit_macro_imports_without_provider_discovery() {
        let mut module = lowered_one("consumer", "module consumer;");
        module.imports = parse(
            "module consumer; import provider(macro choose as pick, value); import other(macro build);",
        )
        .expect("surface imports")
        .imports;
        let prime = substitute_module(&module, &Elaborations::new());
        assert_eq!(prime.imports.len(), 1);
        let ImportKind::Selective { items, .. } = &prime.imports[0].kind else {
            panic!("ordinary selected import remains")
        };
        assert!(
            matches!(items.as_slice(), [ImportItem::Name { name, macro_span: None, .. }] if name == "value")
        );
    }

    fn set_fn_return(module: &mut Module<Lowered>, name: &str, ret: Type<Lowered>) {
        let definition = module.items.iter_mut().find_map(|item| match item {
            Item::FnDef(definition) if definition.name == name => Some(definition),
            _ => None,
        });
        definition.expect("function exists").ret = ret;
    }

    struct BoundaryGoalContext;

    impl GoalTypeContext for BoundaryGoalContext {
        fn capability(&self) -> GoalContextCapability {
            GoalContextCapability::Test(1)
        }

        fn canonicalize(
            &self,
            ty: &Type<Lowered>,
            _identity_canonical: bool,
            _scope: &RigidScope,
        ) -> (Type<Lowered>, bool) {
            (ty.clone(), true)
        }

        fn canonicalize_alias_frontier(
            &self,
            _ty: &Type<Lowered>,
            _identity_canonical: bool,
            _scope: &RigidScope,
        ) -> Option<(Type<Lowered>, bool)> {
            None
        }

        fn nominal_head_kind(
            &self,
            _segments: &[PathSegment],
            _supplied_args: usize,
            span: Span,
            _scope: &RigidScope,
            _identity_canonical: bool,
        ) -> Result<crate::ast::Kind, Error> {
            Err(Error::type_(
                span,
                "boundary-goal test has no nominal type declarations",
            ))
        }

        fn mismatch(&self, _found: &ScopedType, _expected: &ScopedType, span: Span) -> Error {
            Error::type_(span, "type mismatch")
        }
    }

    #[test]
    #[should_panic(
        expected = "an open type-inference goal reached Lowered-to-PrePrime substitution"
    )]
    fn substitution_rejects_a_production_allocated_open_goal() {
        let mut module = lowered_one("main", "module main; fn target() -> . { () }");
        let mut store = GoalStore::new();
        let owner = store
            .begin_owner(
                None,
                RigidScope::new(),
                GoalOwnerKind::Application,
                Span::new(0, 0),
            )
            .unwrap();
        let goal = store
            .alloc_goal(
                owner,
                crate::ast::Kind::Star,
                GoalSolutionPolicy::Monotype,
                GoalOrigin::named(Span::new(0, 0), GoalRole::TypeArgument, "T"),
            )
            .unwrap();
        let open = store
            .scoped_goal_at(goal, Vec::new(), owner, Span::new(0, 0))
            .unwrap();
        set_fn_return(&mut module, "target", open.ty().clone_type());

        let _ = substitute_module(&module, &Elaborations::new());
    }

    #[test]
    fn a_zonked_production_goal_crosses_substitution_as_an_ordinary_type() {
        let mut module = lowered_one("main", "module main; fn target() -> . { () }");
        let span = Span::new(0, 0);
        let mut store = GoalStore::new();
        let owner = store
            .begin_owner(None, RigidScope::new(), GoalOwnerKind::Application, span)
            .unwrap();
        let goal = store
            .alloc_goal(
                owner,
                crate::ast::Kind::Star,
                GoalSolutionPolicy::Monotype,
                GoalOrigin::named(span, GoalRole::TypeArgument, "T"),
            )
            .unwrap();
        let open = store.scoped_goal_at(goal, Vec::new(), owner, span).unwrap();
        let mut delta = store.begin_delta(owner, span).unwrap();
        store
            .constrain(
                &mut delta,
                open.clone(),
                store
                    .scoped_type(
                        owner,
                        crate::pass::typecheck_core::InternedType::fresh(Type::Unit {
                            meta: Meta::new(span),
                        }),
                        span,
                    )
                    .unwrap(),
                span,
                &BoundaryGoalContext,
            )
            .unwrap();
        let prepared = store
            .prepare_owner_with_publication(
                delta,
                vec![(open, GoalEscape::ClosedAt(owner))],
                Vec::new(),
                &BoundaryGoalContext,
            )
            .unwrap();
        let (commit, closed, publication) = prepared.into_parts();
        assert!(publication.into_outputs().is_empty());
        store.commit_prepared_owner(commit);
        let zonked = match closed
            .into_outputs()
            .into_iter()
            .next()
            .expect("one closed substitution-boundary output")
        {
            crate::pass::typecheck_core::ClosedGoalOutput::GoalFree(output) => {
                output.into_scoped_type().into_interned_type().clone_type()
            }
            crate::pass::typecheck_core::ClosedGoalOutput::Retained(_)
            | crate::pass::typecheck_core::ClosedGoalOutput::RetainedFunctionScheme(_) => {
                panic!("boundary substitution test expected a goal-free output")
            }
            crate::pass::typecheck_core::ClosedGoalOutput::FunctionScheme(_) => {
                panic!("boundary substitution test expected a value-position output")
            }
        };
        set_fn_return(&mut module, "target", zonked);

        let prime = substitute_module(&module, &Elaborations::new());
        let definition = prime.items.iter().find_map(|item| match item {
            Item::FnDef(definition) if definition.name == "target" => Some(definition),
            _ => None,
        });
        assert!(matches!(
            definition.expect("target survives").ret,
            Type::Unit { .. }
        ));
    }

    #[test]
    fn checked_substitution_keeps_outer_and_nested_forall_binders_distinct() {
        let module = substitute_one(
            "main",
            "module main;
             fn consume[A](seed: A, f: [X] (A & X) -> A) -> . { () }
             fn annotated[A](seed: A) -> . {
                 consume(seed, .[A](left, right: A) { seed })
             }",
        );
        let annotated = prime_fn(&module, "annotated");
        let Expr::Call { args, .. } = &annotated.body else {
            panic!("expected call body");
        };
        let Some(CallArg::Value(packet)) = args.last() else {
            panic!("expected the product value packet");
        };
        let (_, lambda) = pair_values(packet);
        let Expr::FnExpr { sig, .. } = lambda else {
            panic!("expected lambda as the second product component");
        };
        let [
            crate::ast::SignatureParam::Type(inner),
            crate::ast::SignatureParam::Value(left),
            crate::ast::SignatureParam::Value(right),
        ] = sig.params.as_slice()
        else {
            panic!("expected one type and two value parameters");
        };
        assert_ne!(
            left.ty.as_ref().expect("inferred left type"),
            right.ty.as_ref().expect("annotated right type"),
            "substitution must not replay a normalized outer-binder annotation onto the raw shadowing lambda"
        );
        assert_eq!(inner.name, "A_n2");
        assert_eq!(
            crate::pass::alpha_normalize::normalize_module(&module).module(),
            &module
        );
    }

    #[test]
    fn substitution_renormalizes_inserted_elaboration_binders() {
        let parsed = parse(
            "module main;
             import control(Bool, macro if);

             fn target[A](condition: Bool) -> [X] X -> X {
                 if (condition) { .[X](value: X) { value } }
                 else { .[X](value: X) { value } }
             }
             fn raw() -> [A] A -> A { .[A](value: A) { value } }",
        )
        .expect("parse");
        let (lowered_modules, _) = FullPipeline::lower_package(
            vec![
                (module_file_path("main"), parsed),
                (
                    PathBuf::from("control.kio"),
                    parse(include_str!(
                        "../../../../test-data/poc/macros/workdir/control.kio"
                    ))
                    .expect("parse canonical control provider"),
                ),
            ],
            None,
        )
        .expect("lower package");
        let package = Package::build(Path::new(""), lowered_modules, None).expect("build package");
        package.resolve_imports().expect("resolve imports");

        let normalized = crate::pass::alpha_normalize::normalize_package(&package);
        let normalized_module = &normalized
            .package()
            .module("main")
            .expect("main module")
            .module;
        let normalized_target = normalized_module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == "target" => Some(def),
                _ => None,
            })
            .expect("target function");
        let raw_replacement = normalized_module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == "raw" => Some(def.body.clone()),
                _ => None,
            })
            .expect("raw function");
        let Expr::FnExpr { sig, .. } = &raw_replacement else {
            panic!("expected a replacement lambda");
        };
        let [crate::ast::SignatureParam::Type(inner), ..] = sig.params.as_slice() else {
            panic!("expected the replacement type binder");
        };
        assert_eq!(inner.name, "A");
        let mut elaborations = check_package_preserving_normalization(&package)
            .expect("canonical support bodies typecheck before substitution")
            .into_elaborations();
        elaborations.record_elaboration("main", &normalized_target.body, raw_replacement);

        let substituted = substitute_normalized_package(normalized, &elaborations);
        let substituted_module = &substituted
            .package()
            .module("main")
            .expect("main module")
            .module;
        let substituted_target = substituted_module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == "target" => Some(def),
                _ => None,
            })
            .expect("target function");
        let Expr::FnExpr { sig, .. } = &substituted_target.body else {
            panic!("expected replacement lambda");
        };
        let [crate::ast::SignatureParam::Type(inner), ..] = sig.params.as_slice() else {
            panic!("expected replacement type binder");
        };
        assert_eq!(inner.name, "A_n2");
    }

    trait BinderInspectionPhase:
        Phase<
            ExprTuple = Never,
            ExprFnPlaceholder = Never,
            ExprOpChain = Never,
            ExprElab = Never,
            ExprBlockSyntax = Never,
            ExprRecCall = Never,
            ExprRecOrder = Never,
            ExprUfcs = Never,
            ExprEnriched = Never,
            ExprLow = Never,
            ExprResolved = (),
            ParamPatternExt = (),
            FnExprCapabilities = (),
        >
    {
    }

    impl BinderInspectionPhase for Prime {}
    impl BinderInspectionPhase for UncheckedPrime {}

    fn has_reserved_value_binder<P: BinderInspectionPhase>(expr: &Expr<P>) -> bool {
        match expr {
            Expr::ComptimeLiteral { ext, .. } => match *ext {},
            Expr::BlockCall { ext, .. } => match *ext {},
            Expr::Let {
                name, value, body, ..
            } => {
                name.starts_with("__")
                    || has_reserved_value_binder(value)
                    || has_reserved_value_binder(body)
            }
            Expr::Seq { value, body, .. } => {
                has_reserved_value_binder(value) || has_reserved_value_binder(body)
            }
            Expr::FnExpr { sig, body, .. } => {
                sig.params.iter().any(|param| {
                    matches!(param, crate::ast::SignatureParam::Value(param) if param.name.starts_with("__"))
                }) || has_reserved_value_binder(body)
            }
            Expr::Call { callee, args, .. } => {
                has_reserved_value_binder(callee)
                    || args.iter().any(|arg| {
                        matches!(arg, CallArg::Value(value) if has_reserved_value_binder(value))
                    })
            }
            Expr::Path { .. }
            | Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::BoolLit { .. } => false,
            Expr::Tuple { ext, .. }
            | Expr::FnPlaceholder { ext, .. }
                | Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. }
            | Expr::UserMacro { ext, .. }


            | Expr::Ufcs { ext, .. }
            | Expr::OpChain { ext, .. }
            | Expr::RecCall { ext, .. }
            | Expr::EnrichedTuple { ext, .. }
            | Expr::EnrichedProject { ext, .. }
            | Expr::EnrichedInject { ext, .. }
            | Expr::EnrichedMatch { ext, .. }
            | Expr::EnrichedConditional { ext, .. }
            | Expr::EnrichedFieldGet { ext, .. }
            | Expr::LowHostCall { ext, .. }
            | Expr::LowModuleCall { ext, .. }
            | Expr::LowQualifiedModuleCall { ext, .. }
            | Expr::LowQualifiedNewtypeMember { ext, .. }
            | Expr::LowNewtypeCtor { ext, .. }
            | Expr::LowNewtypeProj { ext, .. }
            | Expr::LowClosureCall { ext, .. }
            | Expr::LowIndirectCall { ext, .. }
            | Expr::LowTypeApplication { ext, .. }
            | Expr::LowAbsurdCall { ext, .. }
            | Expr::LowCpsProjectorApply { ext, .. }
            | Expr::LowBoundRef { ext, .. }
            | Expr::LowHostFnValueRef { ext, .. }
            | Expr::LowModuleFnValueRef { ext, .. } => match *ext {},
        }
    }

    fn recursive_generated_binder_fixture() -> &'static str {
        "module x/main; \
         host fn loop[S][R](step: S -> (S | R), state: S) -> R; \
         rec(loop) { fn update(value: .) -> . { recur update(value) }; }"
    }

    fn assert_recursive_fixture_generates_value_binders(source: &str) {
        use crate::pass::visit_mut::TypecheckVisitMut;

        #[derive(Default)]
        struct GeneratedBinders(usize);
        impl TypecheckVisitMut<Lowered> for GeneratedBinders {
            fn enter_value_binder(&mut self, name: &mut String, _: Span, _: Span) {
                self.0 += usize::from(name.starts_with("__rec"));
            }
        }
        let mut lowered = lowered_one("x/main", source);
        let mut generated = GeneratedBinders::default();
        let update = lowered
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == "update" => Some(def),
                _ => None,
            })
            .expect("lowered recursive wrapper");
        generated.visit_expr(&mut update.body);
        assert!(
            generated.0 > 0,
            "recursive lowering must create value binders"
        );
    }

    #[test]
    fn substitution_never_constructs_prime_with_reserved_value_binders() {
        let source = recursive_generated_binder_fixture();
        assert_recursive_fixture_generates_value_binders(source);
        let module = substitute_one("x/main", source);
        let update = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == "update" => Some(def),
                _ => None,
            })
            .expect("fn update");

        assert!(
            !has_reserved_value_binder(&update.body),
            "Lowered -> Prime substitution produced a compiler-only value binder: {:#?}",
            update.body
        );
    }

    #[test]
    fn eval_fn_substitution_materializes_generated_value_binders() {
        let source = recursive_generated_binder_fixture();
        assert_recursive_fixture_generates_value_binders(source);
        let update = substitute_eval_fn("x/main", source, "update");

        assert!(
            !has_reserved_value_binder(&update.body),
            "eval substitution produced a compiler-only value binder: {:#?}",
            update.body
        );
    }

    #[test]
    fn recursive_order_binding_types_are_keyed_by_carrier_identity() {
        let span = Span::new(3, 9);
        let first_id = crate::ast::NodeId(41);
        let second_id = crate::ast::NodeId(42);
        let unit_ty = || Type::<Lowered>::Unit {
            meta: Meta::new(span),
        };
        let bottom_ty = || Type::<Lowered>::Bottom {
            meta: Meta::new(span),
        };
        let carrier = |node_id| Expr::<Lowered>::RecOrder {
            occurrence: Default::default(),
            plan: Box::new(crate::ast::RecOrderPlan {
                source_call: None,
                tail_continuation: None,
                name: "same".to_owned(),
                disposition: crate::ast::RecOrderDisposition::Ordered(
                    crate::ast::RecOrderTypeFlow::ExpectedFromBody,
                ),
                annotation: None,
                value: Box::new(Expr::Unit {
                    source_trivia: Default::default(),
                    occurrence: Default::default(),
                    meta: Meta::new(span),
                }),
                body: Box::new(Expr::Unit {
                    source_trivia: Default::default(),
                    occurrence: Default::default(),
                    meta: Meta::new(span),
                }),
                runtime_ty: unit_ty(),
            }),
            meta: Meta::new(span),
            ext: node_id,
        };
        let replacement = || Expr::<Lowered>::Let {
            occurrence: Default::default(),
            name: "same".to_owned(),
            name_span: span,
            ty: None,
            pattern: (),
            value: Box::new(Expr::Unit {
                source_trivia: Default::default(),
                occurrence: Default::default(),
                meta: Meta::new(span),
            }),
            body: Box::new(Expr::Unit {
                source_trivia: Default::default(),
                occurrence: Default::default(),
                meta: Meta::new(span),
            }),
            meta: Meta::new(span),
        };
        let first = carrier(first_id);
        let second = carrier(second_id);
        let mut elaborations = Elaborations::new();
        elaborations.record_rec_order_elaboration(
            "x/main",
            &first,
            replacement(),
            crate::pass::typecheck_core::InternedType::fresh_canonical(unit_ty()),
        );
        elaborations.record_rec_order_elaboration(
            "x/main",
            &second,
            replacement(),
            crate::pass::typecheck_core::InternedType::fresh_canonical(bottom_ty()),
        );
        assert!(
            elaborations.elaboration_for("x/main", &first).is_none(),
            "compiler-private recursive-order data must not widen the public elaboration API"
        );
        assert!(
            elaborations
                .rec_order_runtime_for("x/main", &first)
                .is_some(),
            "recursive-order replacement and exact type must remain privately paired"
        );
        let pair = Expr::<Lowered>::Seq {
            occurrence: Default::default(),
            value: Box::new(first),
            body: Box::new(second),
            meta: Meta::new(span),
        };

        let substituted = substitute_expr_for_eval(&pair, &elaborations, "x/main", None, None);
        let Expr::Seq {
            value: first,
            body: second,
            ..
        } = substituted
        else {
            panic!("two recursive-order carriers should remain sequenced")
        };
        let Expr::Let {
            ty: Some(first_ty), ..
        } = *first
        else {
            panic!("first recursive-order carrier should become a typed let")
        };
        let Expr::Let {
            ty: Some(second_ty),
            ..
        } = *second
        else {
            panic!("second recursive-order carrier should become a typed let")
        };
        assert!(matches!(first_ty, Type::Unit { .. }));
        assert!(matches!(second_ty, Type::Bottom { .. }));
    }

    #[test]
    #[should_panic(
        expected = "runtime recursive-order replacement must leave its root let type to the paired artifact"
    )]
    fn recursive_order_pair_rejects_an_inline_root_let_type() {
        let span = Span::new(3, 9);
        let unit_ty = || Type::<Lowered>::Unit {
            meta: Meta::new(span),
        };
        let original = Expr::<Lowered>::RecOrder {
            occurrence: Default::default(),
            plan: Box::new(crate::ast::RecOrderPlan {
                source_call: None,
                tail_continuation: None,
                name: "same".to_owned(),
                disposition: crate::ast::RecOrderDisposition::Ordered(
                    crate::ast::RecOrderTypeFlow::ExpectedFromBody,
                ),
                annotation: None,
                value: Box::new(Expr::Unit {
                    source_trivia: Default::default(),
                    occurrence: Default::default(),
                    meta: Meta::new(span),
                }),
                body: Box::new(Expr::Unit {
                    source_trivia: Default::default(),
                    occurrence: Default::default(),
                    meta: Meta::new(span),
                }),
                runtime_ty: unit_ty(),
            }),
            meta: Meta::new(span),
            ext: crate::ast::NodeId(41),
        };
        let malformed_replacement = Expr::<Lowered>::Let {
            occurrence: Default::default(),
            name: "same".to_owned(),
            name_span: span,
            ty: Some(unit_ty()),
            pattern: (),
            value: Box::new(Expr::Unit {
                source_trivia: Default::default(),
                occurrence: Default::default(),
                meta: Meta::new(span),
            }),
            body: Box::new(Expr::Unit {
                source_trivia: Default::default(),
                occurrence: Default::default(),
                meta: Meta::new(span),
            }),
            meta: Meta::new(span),
        };

        Elaborations::new().record_rec_order_elaboration(
            "x/main",
            &original,
            malformed_replacement,
            crate::pass::typecheck_core::InternedType::fresh_canonical(unit_ty()),
        );
    }

    #[test]
    fn replayed_input_without_canonical_types_skips_requalifier_initialization() {
        let parsed = parse("module owner; host type Hidden[A]; type Kioq0 = .;")
            .expect("parse owner with reserved names");
        let (modules, _) =
            FullPipeline::lower_package(vec![(module_file_path("owner"), parsed)], None)
                .expect("lower owner");
        let module = &modules[0].1;
        let span = Span::new(0, 0);
        let expr = Expr::<Lowered>::Unit {
            source_trivia: Default::default(),
            occurrence: Default::default(),
            meta: Meta::new(span),
        };
        let mut elabs = Elaborations::new();
        let before =
            crate::pass::typecheck_core::PrimeTypeRequalifier::initialization_count_for_test();
        let result = substitute_expr_for_replayed_elaboration(
            &expr,
            &std::collections::HashSet::new(),
            &mut elabs,
            "owner",
            None,
            module,
        );
        assert_eq!(
            result,
            Expr::<PrePrime>::Unit {
                source_trivia: Default::default(),
                occurrence: Default::default(),
                meta: Meta::new(span),
            }
        );
        assert!(elabs.prime_requalification_imports_for("owner").is_empty());
        assert!(elabs.generated_type_aliases_for("owner").is_empty());
        assert_eq!(
            crate::pass::typecheck_core::PrimeTypeRequalifier::initialization_count_for_test()
                - before,
            0,
            "an input without canonical recorded types has no requalifier work"
        );
    }

    #[test]
    fn replayed_input_preserves_ambient_type_binders() {
        for (selection, binder) in [("String as A", "A"), ("String", "String")] {
            let parsed = parse(&format!(
                "module owner; import provider as host; import provider({selection});"
            ))
            .expect("parse selective import");
            let provider = parse("module provider; pub host type String;")
                .expect("parse imported nominal owner");
            let (modules, _) = FullPipeline::lower_package(
                vec![
                    (module_file_path("owner"), parsed),
                    (module_file_path("provider"), provider),
                ],
                None,
            )
            .expect("lower caller and provider");
            let module = &modules
                .iter()
                .find(|(_, module)| module_path_key(&module.path) == "owner")
                .expect("owner module")
                .1;
            let span = Span::new(0, 0);
            let call = Expr::synth_call(
                Expr::Path {
                    occurrence: Default::default(),
                    segments: vec![PathSegment::new("callee", span)],
                    meta: Meta::new(span),
                    ext: (),
                },
                vec![
                    CallArg::Type(Type::Infer {
                        meta: Meta::new(span),
                        ext: (),
                    }),
                    CallArg::Type(Type::synth_path(vec![binder.to_owned()], Vec::new(), span)),
                ],
                span,
            )
            .with_fresh_occurrence();
            let mut elabs = Elaborations::new();
            elabs.record_type_resolution(
                "owner",
                call.site().id,
                0,
                crate::pass::typecheck_core::InternedType::fresh_canonical(Type::synth_path(
                    vec!["provider".to_owned(), "String".to_owned()],
                    Vec::new(),
                    span,
                )),
            );
            for (ambient, expected) in [
                (
                    std::collections::HashSet::from([binder.to_owned()]),
                    vec!["host".to_owned(), "String".to_owned()],
                ),
                (std::collections::HashSet::new(), vec![binder.to_owned()]),
            ] {
                let replayed = substitute_expr_for_replayed_elaboration(
                    &call, &ambient, &mut elabs, "owner", None, module,
                );
                let Expr::Call { args, .. } = replayed else {
                    panic!("replayed call")
                };
                assert_eq!(
                    args[0],
                    CallArg::Type(Type::synth_path(expected, Vec::new(), span))
                );
                assert_eq!(
                    args[1],
                    CallArg::Type(Type::synth_path(vec![binder.to_owned()], Vec::new(), span,)),
                    "explicit source spelling retains its lexical binder"
                );
            }
        }
    }

    #[test]
    fn replayed_input_requalification_matches_eager_binding_boundaries() {
        let parsed = parse(
            "module owner; import provider as dep; host type Hidden[A]; \
             type Kioq0 = .; type Qp0_s0 = .;",
        )
        .expect("parse owner with import, local nominal and allocator collisions");
        let provider = parse("module provider; pub host type External;")
            .expect("parse imported nominal owner");
        let (modules, _) = FullPipeline::lower_package(
            vec![
                (module_file_path("owner"), parsed),
                (module_file_path("provider"), provider),
            ],
            None,
        )
        .expect("lower owner and dependency");
        let module = &modules
            .iter()
            .find(|(_, module)| module_path_key(&module.path) == "owner")
            .expect("owner module")
            .1;
        let span = Span::new(0, 0);
        let unit = Type::<Lowered>::Unit {
            meta: Meta::new(span),
        };
        let call = Expr::synth_call(
            Expr::Path {
                occurrence: Default::default(),
                segments: vec![PathSegment::new("__left__", span)],
                meta: Meta::new(span),
                ext: (),
            },
            vec![
                CallArg::Type(Type::Infer {
                    meta: Meta::new(span),
                    ext: (),
                }),
                CallArg::Type(Type::Infer {
                    meta: Meta::new(span),
                    ext: (),
                }),
                CallArg::Value(Expr::Unit {
                    source_trivia: Default::default(),
                    occurrence: Default::default(),
                    meta: Meta::new(span),
                }),
            ],
            span,
        )
        .with_fresh_occurrence();
        let shadowed = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(
                ["Hidden", "dep"]
                    .into_iter()
                    .map(|name| {
                        SignatureParam::Type(TypeParam {
                            name: name.to_owned(),
                            kind: None,
                            span,
                            presentation: Default::default(),
                        })
                    })
                    .collect(),
                vec![SignatureGroupKind::Type { len: 2 }],
            ),
            ret_ty: None,
            body: Box::new(call.clone()),
            meta: Meta::new(span),
            caps: (),
        };
        let mut lazy = Elaborations::new();
        let mut eager = Elaborations::new();
        for elabs in [&mut lazy, &mut eager] {
            for (slot, ty) in [
                Type::synth_path(
                    vec!["owner".to_owned(), "Hidden".to_owned()],
                    vec![unit.clone()],
                    span,
                ),
                Type::synth_path(
                    vec!["provider".to_owned(), "External".to_owned()],
                    Vec::new(),
                    span,
                ),
            ]
            .into_iter()
            .enumerate()
            {
                elabs.record_type_resolution(
                    "owner",
                    call.site().id,
                    slot,
                    crate::pass::typecheck_core::InternedType::fresh_canonical(ty),
                );
            }
        }
        // Repeating the shadowed input must seed the alias just published by
        // its predecessor; the final unshadowed input resets lexical binders.
        for input in [&shadowed, &shadowed, &call] {
            let before =
                crate::pass::typecheck_core::PrimeTypeRequalifier::initialization_count_for_test();
            let actual = substitute_expr_for_replayed_elaboration(
                input,
                &std::collections::HashSet::new(),
                &mut lazy,
                "owner",
                None,
                module,
            );
            assert_eq!(
                crate::pass::typecheck_core::PrimeTypeRequalifier::initialization_count_for_test()
                    - before,
                1,
                "multiple canonical slots initialize their one input requalifier once"
            );
            let (expected, imports, aliases) = {
                let mut visitor = SubstituteVisitor {
                    elabs: &eager,
                    source_bindings: None,
                    module_path: "owner".to_owned(),
                    module: Some(module),
                    prime_type_requalifier: PrimeRequalifier::Active(
                        crate::pass::typecheck_core::PrimeTypeRequalifier::new(module),
                    ),
                    type_binders: Vec::new(),
                    prime_requalifier_seeded: false,
                    comptime_lowering_mode: ComptimeLoweringMode::EvalPreserve,
                };
                let expected = visitor.walk_expr(input).expect(INFALLIBLE);
                visitor.seed_prime_type_requalifier();
                let PrimeRequalifier::Active(mut requalifier) = visitor.prime_type_requalifier
                else {
                    panic!("eager reference retains its initialized requalifier")
                };
                (
                    expected,
                    requalifier.take_imports(),
                    requalifier.take_local_aliases(),
                )
            };
            record_replayed_prime_bindings(&mut eager, "owner", imports, aliases);
            assert_eq!(actual, expected);
            assert_eq!(
                lazy.prime_requalification_imports_for("owner"),
                eager.prime_requalification_imports_for("owner"),
            );
            assert_eq!(
                lazy.generated_type_aliases_for("owner"),
                eager.generated_type_aliases_for("owner"),
            );
        }
        let [alias] = lazy.generated_type_aliases_for("owner") else {
            panic!("repeated input retains one generated local alias")
        };
        assert_eq!(alias.alias, "Kioq1");
        assert_eq!(alias.params[0].name, "Qp0_s1");
        let [import] = lazy.prime_requalification_imports_for("owner") else {
            panic!("shadowed authored import requires one generated import")
        };
        assert_eq!(import.module_path, "provider");
        assert_eq!(import.alias, "_q2");
    }

    #[test]
    fn eval_preserve_keeps_explicit_generated_binding_paths() {
        let span = Span::new(3, 9);
        let mut elabs = Elaborations::new();
        elabs.record_prime_requalification_imports(
            "x/main",
            &[UserMacroTemplateGeneratedImport {
                module_path: "target/mod".to_owned(),
                alias: "_qsite_0".to_owned(),
            }],
        );
        elabs.record_generated_type_aliases(
            "x/main",
            &[crate::pass::typecheck_core::PrimeLocalTypeAlias {
                alias: "Kiosite_1".to_owned(),
                nominal: "Box".to_owned(),
                params: vec![crate::ast::TypeParam {
                    presentation: Default::default(),
                    name: "Qp0_s0".to_owned(),
                    span,
                    kind: None,
                }],
                span,
            }],
        );
        let mut visitor = SubstituteVisitor {
            elabs: &elabs,
            module_path: "x/main".to_owned(),
            module: None,
            prime_type_requalifier: PrimeRequalifier::Disabled,
            type_binders: vec!["T".to_owned()],
            prime_requalifier_seeded: false,
            source_bindings: None,
            comptime_lowering_mode: ComptimeLoweringMode::EvalPreserve,
        };

        let canonical = crate::pass::typecheck_core::RecordedTypeResolution {
            resolved: Type::synth_path(
                vec!["_qsite_0".to_owned(), "original".to_owned(), "T".to_owned()],
                Vec::new(),
                span,
            ),
            identity_canonical: true,
        };
        let Type::Path { segments, .. } = visitor
            .walk_recorded_type(&canonical)
            .expect("canonical recorded type walks")
        else {
            panic!("expected canonical path")
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            vec!["_qsite_0", "original", "T"],
            "a canonical module-path head must not be reinterpreted as a generated alias"
        );
        assert!(matches!(
            visitor.prime_type_requalifier,
            PrimeRequalifier::Disabled
        ));

        visitor.type_binders.clear();
        let generated_import_ty = Type::synth_path(
            vec!["_qsite_0".to_owned(), "Leaf".to_owned()],
            Vec::new(),
            span,
        );
        let Type::Path { segments, .. } = visitor
            .walk_type(&generated_import_ty)
            .expect("generated import type walks")
        else {
            panic!("expected generated import path")
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            vec!["_qsite_0", "Leaf"],
            "the explicit evaluator environment, not substitution, resolves generated imports"
        );

        let generated_local_ty = Type::synth_path(
            vec!["Kiosite_1".to_owned()],
            vec![Type::Unit {
                meta: Meta::new(span),
            }],
            span,
        );
        let Type::Path { segments, args, .. } = visitor
            .walk_type(&generated_local_ty)
            .expect("generated local alias type walks")
        else {
            panic!("expected generated local alias path")
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            vec!["Kiosite_1"],
            "the explicit evaluator environment retains generated local aliases"
        );
        assert_eq!(args.len(), 1);

        let bound_alias = Type::Forall {
            param: crate::ast::TypeParam {
                presentation: Default::default(),
                name: "Kiosite_1".to_owned(),
                span,
                kind: None,
            },
            body: Box::new(Type::synth_path(
                vec!["Kiosite_1".to_owned()],
                Vec::new(),
                span,
            )),
            meta: Meta::new(span),
        };
        let Type::Forall { body, .. } = visitor
            .walk_type(&bound_alias)
            .expect("forall with colliding binder walks")
        else {
            panic!("expected forall")
        };
        let Type::Path { segments, .. } = *body else {
            panic!("expected bound path")
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            vec!["Kiosite_1"],
            "a genuine forall binder must not expand through generated alias metadata"
        );
    }

    fn count_sequence_calls<P: Phase>(expr: &Expr<P>, name: &str) -> usize {
        match expr {
            Expr::Call { callee, args, .. } => {
                usize::from(matches!(callee.as_ref(), Expr::Path { segments, .. }
                    if segments.last().is_some_and(|segment| segment.name == name)))
                    + count_sequence_calls(callee, name)
                    + args
                        .iter()
                        .map(|arg| match arg {
                            CallArg::Value(value) => count_sequence_calls(value, name),
                            CallArg::Type(_) => 0,
                        })
                        .sum::<usize>()
            }
            Expr::FnExpr { body, .. } => count_sequence_calls(body, name),
            Expr::Let { value, body, .. } => {
                count_sequence_calls(value, name) + count_sequence_calls(body, name)
            }
            Expr::Path { .. } | Expr::Unit { .. } => 0,
            _ => panic!("the ordinary fold contains only calls, lambdas, lets, paths and Unit"),
        }
    }

    #[test]
    fn do_contract_erasure_retains_one_receiver_and_fold_calls() {
        for (body, factories, touches) in [
            ("do (factory()) { () }", 1, 0),
            ("do (factory()) { let unused = touch(()); () }", 1, 1),
            (
                "do (factory()) { let first <- touch(()); let second <- touch(first); touch(second) }",
                1,
                3,
            ),
        ] {
            let source = format!(
                "module main;
                import sequence(macro do);
                host fn factory() -> [A][B](A & (A -> B)) -> B;
                host fn touch(value: .) -> .;
                fn run() -> . {{ {body} }}"
            );
            let module = substitute_sequence_one("main", &source);
            let def = module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::FnDef(def) if def.name == "run" => Some(def),
                    _ => None,
                })
                .expect("run");
            assert_eq!(count_sequence_calls(&def.body, "factory"), factories);
            assert_eq!(count_sequence_calls(&def.body, "touch"), touches);
            let eval = substitute_sequence_eval_fn("main", &source, "run");
            assert_eq!(count_sequence_calls(&eval.body, "factory"), factories);
            assert_eq!(count_sequence_calls(&eval.body, "touch"), touches);
        }
    }

    #[test]
    fn do_contract_zero_bind_retains_receiver_in_both_modes() {
        let source = "module main;
            import sequence(macro do);
            host fn factory() -> [A][B](A & (A -> B)) -> B;
            fn run() -> . { do (factory()) { () } }";
        let module = substitute_sequence_one("main", source);
        let def = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == "run" => Some(def),
                _ => None,
            })
            .expect("run");
        assert_eq!(count_sequence_calls(&def.body, "factory"), 1);
        assert!(!matches!(def.body, Expr::Unit { .. }));
        let def = substitute_sequence_eval_fn("main", source, "run");
        assert_eq!(count_sequence_calls(&def.body, "factory"), 1);
        assert!(!matches!(def.body, Expr::Unit { .. }));
    }

    #[cfg(feature = "prime")]
    #[test]
    fn generated_local_alias_materialization_uses_allocated_parameter_identity() {
        let source = "module owner; host type Hidden[A]; type Kioq0 = .; type Qp0_s0 = .;";
        let parsed = parse(source).expect("parse allocator-collision module");
        let (modules, _) =
            FullPipeline::lower_package(vec![(module_file_path("owner"), parsed)], None)
                .expect("lower allocator-collision module");
        let mut module = modules[0].1.clone();
        let span = Span::new(0, 0);
        let canonical = Type::<Lowered>::synth_path(
            vec!["owner".to_owned(), "Hidden".to_owned()],
            vec![Type::Unit {
                meta: Meta::new(span),
            }],
            span,
        );
        let mut requalifier = crate::pass::typecheck_core::PrimeTypeRequalifier::new(&module);
        let rewritten = requalifier.rewrite(&canonical, &HashSet::from(["Hidden".to_owned()]));
        let aliases = requalifier.take_local_aliases();
        assert!(matches!(
            &rewritten,
            Type::Path { segments, .. } if segments.as_slice() == ["Kioq1"]
        ));
        assert!(matches!(
            aliases.as_slice(),
            [alias] if alias.alias == "Kioq1" && alias.params[0].name == "Qp0_s1"
        ));
        module.items.push(Item::TypeAlias(crate::ast::TypeAlias {
            vis: crate::ast::Visibility::Private,
            name: "Uses_generated".to_owned(),
            name_span: span,
            type_params: Vec::new(),
            body: rewritten,
            meta: Meta::new(span),
            editable_span: None,
            doc: None,
        }));

        let mut elaborations = Elaborations::new();
        elaborations.record_generated_type_aliases("owner", &aliases);
        let prime = substitute_module(&module, &elaborations);
        let alias = prime
            .items
            .iter()
            .find_map(|item| match item {
                Item::TypeAlias(alias) if alias.name == "Kioq1" => Some(alias),
                _ => None,
            })
            .expect("materialized generated alias");
        assert_eq!(alias.type_params.len(), 1);
        assert_eq!(alias.type_params[0].name, "Qp0_s1");
        let Type::Path { segments, args, .. } = &alias.body else {
            panic!("materialized alias body must apply the source nominal")
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["Hidden"]
        );
        assert!(matches!(
            args.as_slice(),
            [Type::Path { segments, .. }] if segments.as_slice() == ["Qp0_s1"]
        ));

        let emitted = crate::backends::kio_prime::emit_module_for_test(&prime);
        assert!(
            emitted.contains("type Kioq1[Qp0_s1] = Hidden[Qp0_s1];"),
            "generated alias declaration and application must retain one parameter identity:\n{emitted}"
        );
        assert!(
            emitted.contains("type Uses_generated = Kioq1[.];"),
            "the emitted owner must use the generated alias reference:\n{emitted}"
        );
        let reparsed = parse(&emitted)
            .unwrap_or_else(|error| panic!("fresh Kio' parse failed: {error:?}\n{emitted}"));
        let (fresh_modules, _) = crate::prime::pipeline::PrimePipeline::lower_package(
            vec![(module_file_path("owner"), reparsed)],
            None,
        )
        .expect("emitted alias module remains Kio'-shaped");
        let fresh = Package::build(Path::new(""), fresh_modules, None)
            .expect("assemble freshly parsed alias package");
        fresh
            .resolve_imports()
            .expect("fresh alias package resolves");
        fresh
            .check_binding_origins()
            .expect("fresh alias declaration and application retain one binding origin");
        fresh
            .check_no_value_cycles()
            .expect("fresh alias package has no value cycle");
        fresh
            .check_in_body_resolution()
            .expect("fresh alias package resolves bodies");
        crate::prime::pipeline::PrimePipeline::typecheck(&fresh)
            .expect("standalone Prime validation accepts the generated alias");
    }

    #[test]
    fn generated_host_aliases_follow_their_owners_without_reordering_authored_items() {
        let source = "module owner; import provider as dep; \
            host type First[A]; type Uses_first = .; type Between = .; \
            host type Second[A]; type Uses_second = .;";
        let parsed = parse(source).expect("parse host owners and import");
        let provider =
            parse("module provider; pub host type Imported;").expect("parse imported host owner");
        let (modules, _) = FullPipeline::lower_package(
            vec![
                (module_file_path("owner"), parsed),
                (module_file_path("provider"), provider),
            ],
            None,
        )
        .expect("lower host owners and import");
        let mut module = modules
            .iter()
            .find(|(_, module)| module_path_key(&module.path) == "owner")
            .expect("owner module")
            .1
            .clone();
        let span = Span::new(0, 0);
        let mut requalifier = crate::pass::typecheck_core::PrimeTypeRequalifier::new(&module);
        let mut generated_names = Vec::new();
        for (owner, consumer) in [("First", "Uses_first"), ("Second", "Uses_second")] {
            let canonical = Type::<Lowered>::synth_path(
                vec!["owner".to_owned(), owner.to_owned()],
                vec![Type::Unit {
                    meta: Meta::new(span),
                }],
                span,
            );
            let rewritten = requalifier.rewrite(&canonical, &HashSet::from([owner.to_owned()]));
            let Type::Path { segments, .. } = &rewritten else {
                panic!("local host requalification must yield an alias path")
            };
            generated_names.push(segments[0].as_str().to_owned());
            let alias = module
                .items
                .iter_mut()
                .find_map(|item| match item {
                    Item::TypeAlias(alias) if alias.name == consumer => Some(alias),
                    _ => None,
                })
                .expect("authored alias consumer");
            alias.body = rewritten;
        }
        let imported = Type::<Lowered>::synth_path(
            vec!["provider".to_owned(), "Imported".to_owned()],
            Vec::new(),
            span,
        );
        let imported = requalifier.rewrite(&imported, &HashSet::from(["Imported".to_owned()]));
        assert!(matches!(&imported, Type::Path { segments, .. }
            if segments.as_slice() == ["dep", "Imported"]));
        assert!(requalifier.take_imports().is_empty());
        let aliases = requalifier.take_local_aliases();
        assert_eq!(aliases.len(), 2);
        let mut elaborations = Elaborations::new();
        elaborations.record_generated_type_aliases("owner", &aliases);
        let prime = substitute_module(&module, &elaborations);
        let names = prime
            .items
            .iter()
            .map(|item| match item {
                Item::HostType(host) => host.name.as_str(),
                Item::TypeAlias(alias) => alias.name.as_str(),
                _ => panic!("unexpected item in host alias placement control"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "First",
                &generated_names[0],
                "Uses_first",
                "Between",
                "Second",
                &generated_names[1],
                "Uses_second"
            ]
        );
        for (owner, generated) in ["First", "Second"].into_iter().zip(&generated_names) {
            assert!(prime.items.iter().any(|item| matches!(item,
                Item::TypeAlias(alias) if alias.name == *generated && matches!(&alias.body,
                    Type::Path { segments, .. } if segments.as_slice() == [owner]))));
        }
        assert_eq!(prime.imports.len(), 1);
        assert!(matches!(&prime.imports[0].kind,
            ImportKind::Qualified { path, alias, .. }
            if module_path_key(path) == "provider" && alias == "dep"));
    }

    #[test]
    fn substitute_keeps_concrete_let_annotation() {
        // A fully-concrete `let` annotation is load-bearing at the Kio'
        // boundary: the Prime typer cannot otherwise synthesize a
        // let-bound value's type, so `visit_expr_let` must carry it
        // through rather than drop it.
        let module = substitute_one(
            "x",
            "module x; fn f(g: (. -> .)) -> . { let .(h: (. -> .)) = g; h(()) }",
        );
        let f = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(d) if d.name == "f" => Some(d),
                _ => None,
            })
            .expect("fn f");
        let Expr::Let { ty, .. } = &f.body else {
            panic!("expected a let body, got {:?}", f.body);
        };
        assert!(
            ty.is_some(),
            "a concrete let annotation must survive into Prime"
        );
    }

    #[test]
    fn substitute_drops_infer_bearing_let_annotation() {
        // A `_`-bearing `let` annotation has no recorded resolution at
        // substitute time, so it must be dropped — baking a `Type::Infer`
        // into a Prime `let` would fail the standalone Prime verifier.
        let module = substitute_one(
            "x",
            "module x; \
             type { Box[A]: A { pub mk; pub get; }; }; \
             fn f[A](y: A) -> . { let .(h: Box[_]) = Box.mk[A](y); () }",
        );
        let f = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(d) if d.name == "f" => Some(d),
                _ => None,
            })
            .expect("fn f");
        let Expr::Let { ty, .. } = &f.body else {
            panic!("expected a let body, got {:?}", f.body);
        };
        assert!(
            ty.is_none(),
            "an infer-bearing let annotation must be dropped at the Prime boundary"
        );
    }

    fn substitute_eval_fn(module_path: &str, src: &str, name: &str) -> FnDef<UncheckedPrime> {
        let parsed = parse(src).expect("parse");
        let (lowered_modules, _lowered_package_file) =
            FullPipeline::lower_package(vec![(module_file_path(module_path), parsed)], None)
                .expect("lower_package");
        let package = Package::build(Path::new(""), lowered_modules, None).expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        let checked = check_package_preserving_normalization(&package).expect("package typechecks");
        let module = &checked
            .package()
            .module(module_path)
            .expect("module present")
            .module;
        let def = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == name => Some(def),
                _ => None,
            })
            .unwrap_or_else(|| panic!("fn `{name}` present"));
        substitute_fn_def_for_eval(
            def,
            checked.elaborations(),
            module_path,
            Some(checked.package()),
            None,
        )
    }

    fn pair_values<P: Phase>(packet: &Expr<P>) -> (&Expr<P>, &Expr<P>) {
        let Expr::Call { callee, args, .. } = packet else {
            panic!("expected one synthesized product packet")
        };
        assert!(matches!(callee.as_ref(), Expr::Path { segments, .. }
            if segments.len() == 1 && segments[0].as_str() == "__pair__"));
        let [
            CallArg::Type(_),
            CallArg::Type(_),
            CallArg::Value(first),
            CallArg::Value(second),
        ] = args.as_slice()
        else {
            panic!("a synthesized pair has exactly two type and two value operands")
        };
        (first, second)
    }

    fn prime_fn<'a>(module: &'a Module<Prime>, name: &str) -> &'a FnDef<Prime> {
        module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(d) if d.name == name => Some(d),
                _ => None,
            })
            .unwrap_or_else(|| panic!("fn `{name}` present"))
    }

    fn value_param_tys(d: &FnDef<Prime>) -> Vec<&Type<Prime>> {
        d.sig
            .params
            .iter()
            .filter_map(|param| match param {
                crate::ast::SignatureParam::Value(value) => value.ty.as_ref(),
                crate::ast::SignatureParam::Type(_) => None,
            })
            .collect()
    }

    fn assert_unit_type(ty: &Type<Prime>) {
        assert!(matches!(ty, Type::Unit { .. }), "expected (), got {ty:?}");
    }

    fn assert_bottom_type(ty: &Type<Prime>) {
        assert!(matches!(ty, Type::Bottom { .. }), "expected !, got {ty:?}");
    }

    fn assert_path_type<P: crate::ast::Phase + std::fmt::Debug>(ty: &Type<P>, name: &str) {
        assert!(
            matches!(
                ty,
                Type::Path { segments, args, .. }
                    if segments.len() == 1 && segments[0].as_str() == name && args.is_empty()
            ),
            "expected `{name}`, got {ty:?}"
        );
    }

    /// Existentials thread through unchanged across the `Lowered →
    /// Prime` boundary. The newtype keeps its `existential_params`
    /// field populated; the typer extends the constructor and
    /// projector schemes with those existentials so inference picks
    /// them up from the payload's structure.
    #[test]
    fn existentials_threaded_through_substitute() {
        let src = "module x/main; \
                   type { Pack[A]<U>: A & U { pub mk; pub get; }; };";
        let module = substitute_one("x/main", src);
        let Item::Newtype(d) = module
            .items
            .iter()
            .find(|item| matches!(item, Item::Newtype(_)))
            .expect("newtype present")
        else {
            unreachable!()
        };
        assert_eq!(d.name, "Pack");
        assert_eq!(
            d.type_params
                .iter()
                .map(|tp| tp.name.as_str())
                .collect::<Vec<_>>(),
            vec!["A"]
        );
        assert_eq!(
            d.existential_params
                .iter()
                .map(|tp| tp.name.as_str())
                .collect::<Vec<_>>(),
            vec!["U"]
        );
    }

    /// `Item::Equiv` is filtered at the `Lowered → Prime` boundary.
    /// Surface-only items have no execution semantics and don't reach
    /// codegen.
    #[test]
    fn equiv_items_filtered_at_substitution() {
        let src = "module x/main; \
                   fn id_unit() -> . { () } \
                   equiv id_unit_eq { id_unit(); () } \
                   fn after_equiv() -> . { () }";
        let module = substitute_one("x/main", src);
        // The equiv item dropped; the two fn_defs survive.
        assert_eq!(module.items.len(), 2);
        for item in &module.items {
            assert!(matches!(item, Item::FnDef(_)), "got: {item:?}");
        }
    }

    #[test]
    fn imported_comptime_dependent_helpers_survive_substitution() {
        let prime = substitute_many(vec![
            (
                "x/util",
                "module x/util; \
                 import __comptime__; \
                 pub type { Refl_box: __Type__ { pub mk; pub get; }; };",
            ),
            (
                "x/main",
                "module x/main; \
                 import x/util(Refl_box); \
                 fn helper(value: Refl_box) -> Refl_box { value } \
                 fn keep() -> . { () }",
            ),
        ]);
        let main = &prime.module("x/main").expect("main module").module;
        assert!(
            main.items
                .iter()
                .any(|item| matches!(item, Item::FnDef(d) if d.name == "helper")),
            "imported reflection-dependent helper should survive"
        );
        assert!(
            main.items
                .iter()
                .any(|item| matches!(item, Item::FnDef(d) if d.name == "keep")),
            "non-reflection helper should survive"
        );
    }

    #[test]
    #[should_panic(expected = "user macro reached substitution without a recorded elaboration")]
    fn eval_substitution_rejects_missing_user_macro() {
        let span = Span::new(0, 0);
        let expr: Expr<Lowered> = Expr::UserMacro {
            occurrence: Default::default(),
            form: crate::ast::UserMacroCallForm::Ordinary,
            name: "demo".to_owned(),
            stages: Vec::new(),
            meta: Meta::new(span),
            ext: crate::ast::NodeId(1),
        };
        let elabs = Elaborations::new();

        let _ = substitute_expr_for_eval(&expr, &elabs, "x/main", None, None);
    }

    #[test]
    fn eval_substitution_canonicalizes_only_a_source_empty_call_without_typer_markers() {
        let span = Span::new(0, 6);
        let call = |args: Vec<CallArg<Lowered>>| Expr::Call {
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
            callee: Box::new(Expr::Path {
                occurrence: Default::default(),
                segments: vec![PathSegment::new("callee", span)],
                meta: Meta::new(span),
                ext: (),
            }),
            args,
            meta: Meta::new(span),
            ext: (),
        };
        let elabs = Elaborations::new();

        let empty = substitute_expr_for_eval(&call(Vec::new()), &elabs, "x/main", None, None);
        let Expr::Call {
            args: empty_args, ..
        } = empty
        else {
            panic!("an empty call must remain a call")
        };
        assert!(
            matches!(empty_args.as_slice(), [CallArg::Value(Expr::Unit { .. })]),
            "a source-empty call must carry its canonical Unit value: {empty_args:?}"
        );

        let type_only = substitute_expr_for_eval(
            &call(vec![CallArg::Type(Type::Unit {
                meta: Meta::new(span),
            })]),
            &elabs,
            "x/main",
            None,
            None,
        );
        let Expr::Call {
            args: type_only_args,
            ..
        } = type_only
        else {
            panic!("a type-only call must remain a call")
        };
        assert!(
            matches!(
                type_only_args.as_slice(),
                [CallArg::Type(Type::Unit { .. })]
            ),
            "a written type-only packet must not gain Unit without a typer marker: \
             {type_only_args:?}"
        );
    }

    #[test]
    fn eval_substitution_canonicalizes_function_type_argument_abi() {
        let span = Span::new(0, 6);
        let atom = Type::<Lowered>::synth_path(vec!["A".to_owned()], Vec::new(), span);
        let function = Type::Function {
            param: Box::new(crate::ast::build_product_right_fold(
                vec![atom.clone(), atom.clone()],
                span,
            )),
            ret: Box::new(atom),
            meta: Meta::new(span),
            abi_arity: 2,
            caps: (),
        };
        let call = Expr::Call {
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
            callee: Box::new(Expr::Path {
                occurrence: Default::default(),
                segments: vec![PathSegment::new("callee", span)],
                meta: Meta::new(span),
                ext: (),
            }),
            args: vec![CallArg::Type(function)],
            meta: Meta::new(span),
            ext: (),
        };

        let rewritten = substitute_expr_for_eval(&call, &Elaborations::new(), "x/main", None, None);

        let Expr::Call { args, .. } = rewritten else {
            panic!("a type-only application must remain a call")
        };
        assert!(matches!(
            args.as_slice(),
            [CallArg::Type(Type::Function { abi_arity: 1, .. })]
        ));
    }

    #[test]
    fn inferred_function_type_argument_uses_prime_type_expression_abi() {
        let module = substitute_one(
            "main",
            "module main; \
             fn delay[X](value: X)[A] -> X { value } \
             fn caller() -> . { \
               let source = delay(.[B](left: B, _right: B) -> B { left }); \
               () \
             }",
        );
        let Expr::Let { value, .. } = &prime_fn(&module, "caller").body else {
            panic!("caller must bind delay's residual value")
        };
        let Expr::Call { args, .. } = value.as_ref() else {
            panic!("the bound value must be the delay call: {value:#?}")
        };
        let Some(CallArg::Type(Type::Forall { body, .. })) = args.first() else {
            panic!("delay must carry its inferred polymorphic function type: {args:#?}")
        };
        assert!(matches!(body.as_ref(), Type::Function { abi_arity: 1, .. }));
    }

    #[test]
    #[should_panic(
        expected = "every `Type::Infer` in a call's args list must have a recorded resolution"
    )]
    fn eval_substitution_rejects_missing_type_infer() {
        let span = Span::new(0, 0);
        let expr: Expr<Lowered> = Expr::Call {
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
            callee: Box::new(Expr::Path {
                occurrence: Default::default(),
                segments: vec![PathSegment::new("demo", span)],
                meta: Meta::new(span),
                ext: (),
            }),
            args: vec![CallArg::Type(Type::Infer {
                meta: Meta::new(span),
                ext: (),
            })],
            meta: Meta::new(span),
            ext: (),
        };
        let elabs = Elaborations::new();

        let _ = substitute_expr_for_eval(&expr, &elabs, "x/main", None, None);
    }

    #[test]
    fn numeric_literal_publication_keeps_lexical_aliases_and_canonical_values() {
        let sources = [
            ("other", "module other; pub host type Float role(f64);"),
            (
                "quoted",
                "module quoted; import __comptime__; pure fn preserve_impl(ct: __Comptime__, fills: __Fill_ctx__, typ: __Type__, value: __Checked_term__) -> (__Checked_term__ & __Fill_ctx__) { (value, fills) } pub macro preserve : [A] A -> A { impl(fills) preserve_impl; };",
            ),
            (
                "numbers",
                "module numbers; import other as numbers; pub host type Float role(f64); pub type Identity[A] = A; pub pure fn value() -> Float { 42.0[Float] }",
            ),
            (
                "main",
                "module main; import numbers as n; import other as numbers; import quoted(macro preserve); type Float = n.Identity[n.Float]; pure fn local() -> Float { let .(value: Float) = 42.0; value } pure fn id[A](value: A) -> A { value } equiv selected() { local(); n.value(); 42.0[n.Float]; id(42.0[n.Float]); preserve[n.Float](42.0[n.Float]) }",
            ),
        ];
        let parsed = sources
            .into_iter()
            .map(|(path, source)| (module_file_path(path), parse(source).expect("parse")))
            .collect();
        let (modules, _) = FullPipeline::lower_package(parsed, None).expect("lower");
        let package = Package::build(Path::new(""), modules, None).expect("package");
        package.resolve_imports().expect("imports");
        let completed = check_package_preserving_normalization(&package)
            .expect("check")
            .complete()
            .expect("validate ordinary runtime artifact");
        let source = completed.into_source();
        let checked = source.checked();
        let artifact = source.eval_artifact().expect("evaluation artifact");
        let ctx = crate::normalization::AuthenticatedEvalCtx::for_module(
            artifact
                .prepare_module("main")
                .expect("main evaluation scope"),
        );
        let module = &checked
            .package()
            .module("main")
            .expect("main source")
            .module;
        let equiv = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Equiv(equiv, _) => Some(equiv),
                _ => None,
            })
            .expect("equiv");
        for term in &equiv.terms {
            let root = substitute_expr_for_eval(
                &term.body,
                checked.elaborations(),
                "main",
                Some(checked.package()),
                Some(module),
            );
            let value = crate::normalization::eval_authenticated(
                &root,
                &crate::normalization::Env::new(),
                &ctx,
            );
            assert!(
                matches!(value, crate::normalization::Value::FloatAtom(ref digits, Some(ref identity))
                    if digits == "42.0" && identity == "numbers.Float"),
                "literal spelling changed its nominal identity: {value:?}"
            );
        }
    }

    #[test]
    fn numeric_literal_publication_shared_spans_keep_distinct_binding_targets() {
        use crate::pass::typecheck_core::{InternedType, ModuleEnv, TypeCtx};

        let sources = [
            (
                "provider",
                "module provider; pub host type External role(f64);",
            ),
            ("other", "module other; pub host type External role(f64);"),
            (
                "main",
                "module main; import provider(External); import other as provider; host type First role(f64); host type Second role(f32);",
            ),
        ];
        let parsed = sources
            .into_iter()
            .map(|(path, source)| (module_file_path(path), parse(source).expect("parse")))
            .collect();
        let (modules, _) = FullPipeline::lower_package(parsed, None).expect("lower");
        let package = Package::build(Path::new(""), modules, None).expect("package");
        package.resolve_imports().expect("imports");
        let module = &package.module("main").expect("main").module;
        let env = ModuleEnv::build(module, None, None, Some(&package)).expect("environment");
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        let span = Span::new(0, 1);
        for name in ["First", "Second", "External"] {
            tcx.push_type_param(name, span);
        }
        let site = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span,
        };
        let mut local_names = Vec::new();
        for name in ["First", "Second"] {
            let resolved = InternedType::fresh_canonical(Type::synth_path(
                vec!["main".to_owned(), name.to_owned()],
                Vec::new(),
                span,
            ));
            let (annotation, imports, aliases) =
                crate::pass::typecheck_full::prepare_literal_annotation(site, &resolved, &tcx);
            assert!(imports.is_empty());
            let [alias] = aliases.as_slice() else {
                panic!("shadowed local nominal needs one ordinary alias")
            };
            assert_eq!(alias.nominal, name);
            assert!(alias.params.is_empty());
            assert!(!annotation.identity_is_canonical());
            assert_path_type(annotation.as_type(), &alias.alias);
            local_names.push(alias.alias.clone());
        }
        assert_ne!(local_names[0], local_names[1]);
        let resolved = InternedType::fresh_canonical(Type::synth_path(
            vec!["provider".to_owned(), "External".to_owned()],
            Vec::new(),
            span,
        ));
        let (annotation, imports, aliases) =
            crate::pass::typecheck_full::prepare_literal_annotation(site, &resolved, &tcx);
        assert!(aliases.is_empty());
        let [import] = imports.as_slice() else {
            panic!("shadowed selective import needs one explicit module dependency")
        };
        assert_eq!(import.module_path, "provider");
        assert_ne!(import.alias, "provider");
        let Type::Path { segments, .. } = annotation.as_type() else {
            panic!("nominal annotation")
        };
        assert_eq!(segments[0].as_str(), import.alias);
        assert_eq!(segments[1].as_str(), "External");
    }

    #[test]
    fn numeric_literal_publication_bindings_follow_retained_owner_commit() {
        use crate::pass::typecheck_core::{ModuleEnv, TypeCtx, Typer};
        use crate::pass::typecheck_full::LoweredTyper;

        let module = lowered_one(
            "main",
            "module main; host type Float role(f64); fn consume(value: Float, later: .) -> . { () } fn good() -> . { consume(42.0, ()) } fn bad() -> . { consume(42.0, 0.0) }",
        );
        let package = Package::build(
            Path::new(""),
            vec![(module_file_path("main"), module)],
            None,
        )
        .expect("package");
        let package = crate::pass::alpha_normalize::normalize_package(&package)
            .into_parts()
            .0;
        let module = &package.module("main").expect("main").module;
        let env = ModuleEnv::build(module, None, None, Some(&package)).expect("environment");
        for (name, succeeds) in [("good", true), ("bad", false)] {
            let (index, def) = module
                .items
                .iter()
                .enumerate()
                .find_map(|(index, item)| match item {
                    Item::FnDef(def) if def.name == name => Some((index, def)),
                    _ => None,
                })
                .expect("witness function");
            let mut elaborations = Elaborations::new();
            let mut tcx = TypeCtx::new(&env, &mut elaborations).at_item(index);
            tcx.push_type_param("Float", def.meta.span);
            let result = <LoweredTyper as Typer<Lowered>>::synth_expr(&def.body, &mut tcx);
            assert_eq!(result.is_ok(), succeeds, "{name}: {:?}", result.err());
            let aliases = tcx.elaborations.generated_type_aliases_for("main");
            assert_eq!(aliases.len(), usize::from(succeeds), "{name} publication");
            if succeeds {
                assert_eq!(aliases[0].nominal, "Float");
            }
            let Expr::Call { args, .. } = &def.body else {
                panic!("call witness")
            };
            let [CallArg::Value(packet)] = args.as_slice() else {
                panic!("one public product packet")
            };
            let (literal, _) = pair_values(packet);
            assert!(
                matches!(literal, Expr::FloatLit { .. }),
                "first written component"
            );
            let annotation = tcx
                .elaborations
                .literal_resolution_for("main", literal.site().id);
            assert_eq!(annotation.is_some(), succeeds, "{name} literal publication");
            if let Some((annotation, _)) = annotation {
                assert!(!annotation.identity_canonical);
                assert_path_type(&annotation.resolved, &aliases[0].alias);
            }
        }
    }

    #[test]
    fn numeric_literal_publication_reuses_existing_spellings_without_module_scans() {
        for count in [16, 64] {
            let mut source =
                String::from("module numbers; host type Float role(f64); type Selected = Float;");
            for index in 0..count {
                source.push_str(&format!(
                    "fn value{index}() -> Float {{ let .(value: Float) = 42.0; value }}"
                ));
            }
            let module = lowered_one("numbers", &source);
            let package = Package::build(
                Path::new(""),
                vec![(module_file_path("numbers"), module)],
                None,
            )
            .expect("package");
            let before =
                crate::pass::typecheck_core::PrimeTypeRequalifier::initialization_count_for_test();
            check_package_preserving_normalization(&package).expect("check numeric declarations");
            assert_eq!(
                crate::pass::typecheck_core::PrimeTypeRequalifier::initialization_count_for_test()
                    - before,
                0,
                "{count} ordinary literal annotations need no module-wide requalifier"
            );
        }
    }

    #[test]
    fn recorded_literal_identity_canonicalizes_explicit_annotation() {
        let span = Span::new(5, 6);
        let occurrence = crate::ast::ExpressionOccurrence::fresh();
        let mut elabs = Elaborations::new();
        elabs.record_literal_resolution(
            "x/main",
            occurrence.key(),
            crate::pass::typecheck_core::InternedType::fresh(Type::<Lowered>::synth_path(
                vec!["I64".to_owned()],
                Vec::new(),
                span,
            )),
            None,
        );
        let mut visitor = SubstituteVisitor {
            elabs: &elabs,
            module_path: "x/main".to_owned(),
            module: None,
            prime_type_requalifier: PrimeRequalifier::Disabled,
            type_binders: Vec::new(),
            prime_requalifier_seeded: false,
            source_bindings: None,
            comptime_lowering_mode: ComptimeLoweringMode::RuntimeErase,
        };
        let expr = Expr::IntLit {
            occurrence,
            diagnostic: Default::default(),
            digits: "1".to_owned(),
            annotation: Some(Type::<Lowered>::synth_path(
                vec!["I32".to_owned()],
                Vec::new(),
                span,
            )),
            meta: Meta::new(span),
        };

        let rewritten = visitor.walk_expr(&expr).expect(INFALLIBLE);

        let Expr::IntLit { annotation, .. } = rewritten else {
            panic!("expected annotated int literal, got {rewritten:?}");
        };
        assert_path_type(&annotation, "I64");
    }

    #[cfg(feature = "prime")]
    #[test]
    fn comptime_literal_primitive_values_preserve_complete_schemes_in_prime() {
        let mut source = "module main; import __comptime__; import __intrinsics__; ".to_owned();
        for (index, name) in crate::comptime::PUBLIC_COMPTIME_NAMES.iter().enumerate() {
            let builtin = crate::comptime::ComptimeBuiltin::from_public_name(name).unwrap();
            if builtin.is_value_name() {
                source.push_str(&format!(
                    " fn value{index}() -> . {{ let primitive = {name}; () }}"
                ));
            }
        }
        source.push_str(" fn identity[A](value: A) -> A { value } fn transported(packet: __Comptime__ & .) -> __Checked_term__ { let primitive = identity(__term_unit__); primitive(__fst__(packet)) }");
        let module = substitute_one("main", &source);
        let emitted = crate::backends::kio_prime::emit_module_for_test(&module);
        assert!(!emitted.contains("__comptime__"), "{emitted}");
        let (modules, _) = crate::prime::pipeline::PrimePipeline::lower_package(
            vec![(
                module_file_path("main"),
                parse(&emitted).expect("ordinary shared syntax"),
            )],
            None,
        )
        .expect("core syntax");
        let package = Package::build(Path::new(""), modules, None).expect("package");
        package.resolve_imports().expect("imports");
        crate::prime::pipeline::PrimePipeline::typecheck(&package)
            .expect("independent primitive wrapper types");
    }

    #[test]
    fn comptime_literal_types_erase_to_bottom_at_substitution() {
        let mut src = "module x/main; import __comptime__;".to_owned();
        for (idx, name) in ["Comptime_bool", "Comptime_str", "__Diagnostic_text__"]
            .iter()
            .enumerate()
        {
            src.push_str(&format!(
                " fn keep{idx}(value: {name}) -> {name} {{ value }}"
            ));
        }
        let module = substitute_one("x/main", &src);
        for idx in 0..3 {
            let d = prime_fn(&module, &format!("keep{idx}"));
            assert!(matches!(value_param_tys(d)[0], Type::Bottom { .. }));
            assert!(matches!(&d.ret, Type::Bottom { .. }));
        }
    }

    #[cfg(feature = "prime")]
    #[test]
    fn literal_aliases_follow_comptime_erasure_and_reparse() {
        let source = "module main; import __comptime__; import __intrinsics__; host type Int role(i64); \
            type Identity[A] = A; type Flag = Identity[Comptime_bool]; \
            type Text = Comptime_str; \
            fn value(packet: __Comptime__ & .) -> . { let .(flag: Flag) = __.t__(__fst__(packet)); let .(text: Text) = __\"hello\"__(__fst__(packet)); let number = 42[Int]; () }";
        let prime = substitute_one("main", source);
        let emitted = crate::backends::kio_prime::emit_module_for_test(&prime);
        assert!(emitted.contains("host type Int role(i64)"), "{emitted}");
        assert!(
            emitted.contains("type Flag = Identity[!];") && emitted.contains("type Text = !;"),
            "{emitted}"
        );
        assert!(!emitted.contains("__comptime__"), "{emitted}");
        let reparsed = parse(&emitted).expect("runtime aliases reparse");
        let (modules, _) = crate::prime::pipeline::PrimePipeline::lower_package(
            vec![(module_file_path("main"), reparsed)],
            None,
        )
        .expect("runtime aliases remain Prime-shaped");
        let fresh = Package::build(Path::new(""), modules, None).expect("fresh package");
        fresh.resolve_imports().expect("fresh imports");
        fresh
            .check_binding_origins()
            .expect("fresh binding origins");
        fresh.check_in_body_resolution().expect("fresh references");
        crate::prime::pipeline::PrimePipeline::typecheck(&fresh).expect("fresh Prime validation");
    }

    #[test]
    fn user_bindings_named_like_comptime_roles_do_not_runtime_erase() {
        let nominal = substitute_one(
            "x/main",
            "module x/main; \
             type { Comptime_bool: . { mk; get; }; }; \
             fn keep(value: Comptime_bool) -> Comptime_bool { value }",
        );
        let keep = prime_fn(&nominal, "keep");
        let params = value_param_tys(keep);
        assert_eq!(params.len(), 1);
        assert_path_type(params[0], "Comptime_bool");
        assert_path_type(&keep.ret, "Comptime_bool");

        let binder = substitute_one(
            "x/main",
            "module x/main; \
             import __comptime__; \
             fn keep[Comptime_bool](value: Comptime_bool) -> Comptime_bool { value }",
        );
        let keep = prime_fn(&binder, "keep");
        let crate::ast::SignatureParam::Type(bound) = &keep.sig.params[0] else {
            panic!("keep retains its type binder");
        };
        let params = value_param_tys(keep);
        assert_eq!(params.len(), 1);
        assert_path_type(params[0], &bound.name);
        assert_path_type(&keep.ret, &bound.name);

        let declaration_binders = substitute_one(
            "x/main",
            "module x/main; \
             import __comptime__; \
             type Keep_alias[Comptime_bool] = Comptime_bool; \
             type { Keep_newtype[Comptime_str]: Comptime_str { mk; get; }; }; \
             rec { \
               type Keep_group_alias[Comptime_bool] = Keep_group_newtype[Comptime_bool]; \
               type { Keep_group_newtype[Comptime_str]: Keep_group_alias[Comptime_str] { mk; get; }; }; \
             } \
             host fn keep_host[Comptime_bool](value: Comptime_bool) -> Comptime_bool;",
        );
        let alias = declaration_binders
            .items
            .iter()
            .find_map(|item| match item {
                Item::TypeAlias(alias) if alias.name == "Keep_alias" => Some(alias),
                _ => None,
            })
            .expect("Keep_alias");
        assert_path_type(&alias.body, &alias.type_params[0].name);
        let newtype = declaration_binders
            .items
            .iter()
            .find_map(|item| match item {
                Item::Newtype(newtype) if newtype.name == "Keep_newtype" => Some(newtype),
                _ => None,
            })
            .expect("Keep_newtype");
        assert_path_type(&newtype.payload, &newtype.type_params[0].name);
        let group = declaration_binders
            .items
            .iter()
            .find_map(|item| match item {
                Item::TypeRecGroup(group) => Some(group),
                _ => None,
            })
            .expect("recursive declaration group");
        let crate::ast::TypeRecMember::TypeAlias(group_alias) = &group.members[0] else {
            panic!("first recursive member must be the alias")
        };
        let Type::Path { args, .. } = &group_alias.body else {
            panic!("recursive alias must retain its nominal application")
        };
        assert_path_type(&args[0], &group_alias.type_params[0].name);
        let crate::ast::TypeRecMember::Newtype(group_newtype) = &group.members[1] else {
            panic!("second recursive member must be the newtype")
        };
        let Type::Path { args, .. } = &group_newtype.payload else {
            panic!("recursive newtype must retain its alias application")
        };
        assert_path_type(&args[0], &group_newtype.type_params[0].name);
        let host = declaration_binders
            .items
            .iter()
            .find_map(|item| match item {
                Item::HostFn(host) if host.name == "keep_host" => Some(host),
                _ => None,
            })
            .expect("keep_host");
        let host_value_ty = host
            .params
            .iter()
            .find_map(|param| match param {
                crate::ast::HostFnParam::Value(value) => Some(&value.ty),
                crate::ast::HostFnParam::Type(_) => None,
            })
            .expect("keep_host value parameter");
        let crate::ast::HostFnParam::Type(bound) = &host.params[0] else {
            panic!("keep_host retains its type binder");
        };
        assert_path_type(host_value_ty, &bound.name);
        assert_path_type(&host.ret, &bound.name);
    }

    #[test]
    fn later_type_binders_do_not_shadow_earlier_comptime_paths() {
        let module = substitute_one(
            "x/main",
            "module x/main; \
             import __comptime__; \
             fn keep_ordered(before: Comptime_bool)[Comptime_bool](after: Comptime_bool) \
               -> Comptime_bool { after } \
             host fn keep_host_ordered(before: Comptime_bool)[Comptime_bool](after: Comptime_bool) \
               -> Comptime_bool;",
        );

        let keep = prime_fn(&module, "keep_ordered");
        let crate::ast::SignatureParam::Type(bound) = &keep.sig.params[1] else {
            panic!("ordered function retains its type binder");
        };
        let params = value_param_tys(keep);
        assert_eq!(params.len(), 2);
        assert_bottom_type(params[0]);
        assert_path_type(params[1], &bound.name);
        assert_path_type(&keep.ret, &bound.name);

        let host = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::HostFn(host) if host.name == "keep_host_ordered" => Some(host),
                _ => None,
            })
            .expect("keep_host_ordered");
        let params = host
            .params
            .iter()
            .filter_map(|param| match param {
                crate::ast::HostFnParam::Value(value) => Some(&value.ty),
                crate::ast::HostFnParam::Type(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(params.len(), 2);
        assert_bottom_type(params[0]);
        let crate::ast::HostFnParam::Type(bound) = &host.params[1] else {
            panic!("ordered host function retains its type binder");
        };
        assert_path_type(params[1], &bound.name);
        assert_path_type(&host.ret, &bound.name);
    }

    #[test]
    fn comptime_literal_carriers_have_no_runtime_roles() {
        let parsed = parse("module main; import __comptime__;").expect("parse");
        let (modules, _) =
            FullPipeline::lower_package(vec![(module_file_path("main"), parsed)], None)
                .expect("lower");
        let package = Package::build(Path::new(""), modules, None).expect("package");
        let module = &package.module("main").unwrap().module;
        let env = crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package))
            .expect("env");
        assert!(env.annotation_candidates.is_empty());
        for name in ["Comptime_bool", "Comptime_str", "__Diagnostic_text__"] {
            let ty = Type::synth_path(vec![name.to_owned()], Vec::new(), Span::new(0, 0));
            assert!(env.role_type_identity(&ty, false).is_none());
        }
    }

    #[test]
    fn comptime_non_role_opaque_types_erase_to_runtime_only_types_at_substitution() {
        let erased_types = crate::comptime::PUBLIC_COMPTIME_NAMES
            .iter()
            .filter_map(|name| {
                let builtin = crate::comptime::ComptimeBuiltin::from_public_name(name)
                    .expect("public comptime name should map to builtin");
                (builtin.is_type_name()
                    && builtin != crate::comptime::ComptimeBuiltin::ComptimeProof)
                    .then_some((builtin, *name))
            })
            .collect::<Vec<_>>();
        assert!(
            !erased_types.is_empty(),
            "expected at least one opaque comptime type"
        );

        let mut src = "module x/main; import __comptime__;".to_owned();
        for (idx, (_, name)) in erased_types.iter().enumerate() {
            src.push_str(&format!(
                " fn keep_erased{idx}(value: {name}) -> {name} {{ value }}"
            ));
        }
        let module = substitute_one("x/main", &src);
        for (idx, (builtin, name)) in erased_types.iter().enumerate() {
            assert!(
                builtin.runtime_erasure().is_some(),
                "`{name}` is an opaque comptime type, so it must erase before runtime"
            );
            let keep = prime_fn(&module, &format!("keep_erased{idx}"));
            let params = value_param_tys(keep);
            assert_eq!(params.len(), 1);
            assert_bottom_type(params[0]);
            assert_bottom_type(&keep.ret);
        }
    }

    #[test]
    fn comptime_proof_erases_to_bottom_at_substitution() {
        assert_eq!(
            crate::comptime::ComptimeBuiltin::ComptimeProof.runtime_erasure(),
            Some(crate::comptime::ComptimeRuntimeErasure::Bottom),
            "`__Comptime__` must be runtime-uninhabited"
        );
        let module = substitute_one(
            "x/main",
            "module x/main; \
             import __comptime__; \
             fn keep_proof(ct: __Comptime__) -> __Comptime__ { ct }",
        );
        let keep = prime_fn(&module, "keep_proof");
        let params = value_param_tys(keep);
        assert_eq!(params.len(), 1);
        assert_bottom_type(params[0]);
        assert_bottom_type(&keep.ret);
    }

    #[test]
    fn proof_gated_body_erases_to_absurd() {
        let module = substitute_one(
            "x/main",
            "module x/main; \
             import __comptime__; \
             fn proof_only(ct: __Comptime__) -> __Checked_term__ { __term_unit__(ct) }",
        );
        let proof_only = prime_fn(&module, "proof_only");
        assert!(
            contains_call_to(&proof_only.body, "__absurd__"),
            "proof-gated body should erase to __absurd__: {body:?}",
            body = proof_only.body
        );
        assert!(
            !contains_call_to(&proof_only.body, "__term_unit__"),
            "compile-time helper call survived proof-gated body erasure: {body:?}",
            body = proof_only.body
        );
    }

    #[test]
    fn eval_substitution_preserves_comptime_proof_body() {
        let proof_only = substitute_eval_fn(
            "x/main",
            "module x/main; \
             import __comptime__; \
             fn proof_only(ct: __Comptime__) -> __Checked_term__ { __term_unit__(ct) }",
            "proof_only",
        );
        let params = proof_only
            .sig
            .params
            .iter()
            .filter_map(|param| match param {
                crate::ast::SignatureParam::Value(value) => value.ty.as_ref(),
                crate::ast::SignatureParam::Type(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(params.len(), 1);
        assert_path_type(params[0], "__Comptime__");
        assert_path_type(&proof_only.ret, "__Checked_term__");
        assert!(
            contains_call_to(&proof_only.body, "__term_unit__"),
            "eval substitution must preserve comptime helper body: {body:?}",
            body = proof_only.body
        );
        assert!(
            !contains_call_to(&proof_only.body, "__absurd__"),
            "eval substitution must not runtime-erase proof-gated bodies: {body:?}",
            body = proof_only.body
        );
    }

    #[test]
    fn later_direct_proof_erases_only_runtime_body_and_retains_signature_order() {
        let source = "module x/main; import __comptime__; pure fn late(value: .)[A](ct: __Comptime__)(tail: A) -> __Type__ { __type_unit__(ct) }";
        let module = substitute_one("x/main", source);
        let runtime = prime_fn(&module, "late");
        let params = value_param_tys(runtime);
        assert_eq!(params.len(), 3);
        assert_unit_type(params[0]);
        assert_bottom_type(params[1]);
        assert_path_type(params[2], "A");
        assert!(contains_call_to(&runtime.body, "__absurd__"));
        assert!(!contains_call_to(&runtime.body, "__type_unit__"));
        let preserved = substitute_eval_fn("x/main", source, "late");
        assert_eq!(preserved.sig.groups, runtime.sig.groups);
        let params = preserved.sig.value_params().collect::<Vec<_>>();
        assert_eq!(
            params
                .iter()
                .map(|param| param.name.as_str())
                .collect::<Vec<_>>(),
            ["value", "ct", "tail"]
        );
        assert_path_type(
            params[1].ty.as_ref().expect("proof annotation"),
            "__Comptime__",
        );
        assert!(contains_call_to(&preserved.body, "__type_unit__"));
        assert!(!contains_call_to(&preserved.body, "__absurd__"));
    }

    #[test]
    fn proof_gated_lambdas_erase_checked_bodies_and_preserve_eval() {
        for lambda in [
            ".(ct: __Comptime__) -> __Type__ { __type_unit__(ct) }",
            ".(ct: __Comptime__) { __type_unit__(ct) }",
            ".(ct: __Comptime__) -> _ { __type_unit__(ct) }",
            ".(ct) { __type_unit__(ct) }",
        ] {
            let source = format!(
                "module x/main; import __comptime__; pure fn factory() -> __Comptime__ -> __Type__ {{ {lambda} }}"
            );
            let module = substitute_one("x/main", &source);
            let runtime = prime_fn(&module, "factory");
            let Expr::FnExpr { sig, body, .. } = &runtime.body else {
                panic!("the helper must still construct a lambda");
            };
            assert_bottom_type(sig.value_params().next().unwrap().ty.as_ref().unwrap());
            assert!(contains_call_to(body, "__absurd__"), "{lambda}");
            assert!(!contains_call_to(body, "__type_unit__"), "{lambda}");
            let preserved = substitute_eval_fn("x/main", &source, "factory");
            assert!(
                contains_call_to(&preserved.body, "__type_unit__"),
                "{lambda}"
            );
            assert!(!contains_call_to(&preserved.body, "__absurd__"), "{lambda}");
        }
    }

    #[test]
    fn proof_lambda_synthesis_and_retained_calls_record_erased_result() {
        for body in [
            "let value = .(ct: __Comptime__) { __type_unit__(ct) }; value",
            "keep(.(ct: __Comptime__) { __type_unit__(ct) })",
            "keep(.(ct) { __type_unit__(ct) })",
        ] {
            let source = format!(
                "module x/main; import __comptime__; pure fn keep[A](value: A) -> A {{ value }} pure fn factory() -> __Comptime__ -> __Type__ {{ {body} }}"
            );
            let module = substitute_one("x/main", &source);
            let runtime = prime_fn(&module, "factory");
            assert!(contains_call_to(&runtime.body, "__absurd__"), "{body}");
            assert!(!contains_call_to(&runtime.body, "__type_unit__"), "{body}");
            let preserved = substitute_eval_fn("x/main", &source, "factory");
            assert!(contains_call_to(&preserved.body, "__type_unit__"), "{body}");
            assert!(!contains_call_to(&preserved.body, "__absurd__"), "{body}");
        }
    }

    #[test]
    fn retained_polymorphic_proof_lambda_keeps_its_local_result_binder() {
        let source = "module x/main; import __comptime__; pure fn keep[F](value: F) -> F { value } \
            pure fn factory() -> [A] __Comptime__ -> A -> A { keep(.[A](ct: __Comptime__)(value: A) { value }) }";
        let module = substitute_one("x/main", source);
        let runtime = prime_fn(&module, "factory");
        assert!(contains_call_to(&runtime.body, "__absurd__"));
        let preserved = substitute_eval_fn("x/main", source, "factory");
        assert!(!contains_call_to(&preserved.body, "__absurd__"));
    }

    #[test]
    fn retained_lambda_proof_parameter_uses_its_completed_equations() {
        let source = "module x/main; import __comptime__; host fn proof() -> __Comptime__; \
            fn select[A](f: A -> __Type__, value: A) -> A -> __Type__ { f } \
            fn factory() -> . { let result = select(.(ct) { __type_unit__(ct) }, proof()); () }";
        let module = substitute_one("x/main", source);
        let runtime = prime_fn(&module, "factory");
        assert!(contains_call_to(&runtime.body, "__absurd__"));
        assert!(!contains_call_to(&runtime.body, "__type_unit__"));
        assert!(contains_call_to(&runtime.body, "proof"));
        let preserved = substitute_eval_fn("x/main", source, "factory");
        assert!(contains_call_to(&preserved.body, "__type_unit__"));
        assert!(!contains_call_to(&preserved.body, "__absurd__"));
    }

    #[test]
    fn later_lambda_proof_preserves_generic_result_and_creation_effects() {
        let source = "module x/main; import __comptime__; host fn effect() -> .; \
            fn factory() -> . -> [A] __Comptime__ -> A -> A { effect(); \
            .(first: .)[A](ct: __Comptime__)(last: A) { last } }";
        let module = substitute_one("x/main", source);
        let runtime = prime_fn(&module, "factory");
        assert!(contains_call_to(&runtime.body, "effect"));
        let Expr::Seq { body, .. } = &runtime.body else {
            panic!("creation effect retained")
        };
        let Expr::FnExpr { sig, body, .. } = body.as_ref() else {
            panic!("lambda retained")
        };
        let params = sig.value_params().collect::<Vec<_>>();
        assert_eq!(
            params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["first", "ct", "last"]
        );
        assert_unit_type(params[0].ty.as_ref().unwrap());
        assert_bottom_type(params[1].ty.as_ref().unwrap());
        assert_path_type(params[2].ty.as_ref().unwrap(), "A");
        let Expr::Call { args, .. } = body.as_ref() else {
            panic!("absurd application")
        };
        let CallArg::Type(result) = &args[0] else {
            panic!("explicit result type")
        };
        assert_path_type(result, "A");
        let CallArg::Value(Expr::Path { segments, .. }) = &args[1] else {
            panic!("proof argument")
        };
        assert_eq!(segments[0].as_str(), "ct");
    }

    #[test]
    fn nonproof_lambda_keeps_its_runtime_body() {
        let module = substitute_one(
            "x/main",
            "module x/main; fn factory() -> . -> . { .(value: .) { value } }",
        );
        let runtime = prime_fn(&module, "factory");
        let Expr::FnExpr { body, .. } = &runtime.body else {
            panic!("lambda retained")
        };
        assert!(
            matches!(body.as_ref(), Expr::Path { segments, .. } if segments[0].as_str() == "value")
        );
        assert!(!contains_call_to(&runtime.body, "__absurd__"));
    }

    #[test]
    fn user_macro_capture_replay_injects_generated_qualified_import() {
        let prime = substitute_many(vec![
            (
                "pkg/refl",
                "module pkg/refl; \
                 import __comptime__; \
                 pub pure fn make_rep_impl(ct: __Comptime__, _rep_type: __Type__, ctor: __Checked_term__, _source: __Type__, _unit: __Checked_term__) -> __Checked_term__ { \
                   __term_call__(ct, __term_type__(ct, ctor), ctor, __term_unit__(ct)) \
                 } \
                 pub type Rep_payload = .; \
                 pub type { Rep: Rep_payload { pub mk; pub get; }; }; \
                 pub macro make_rep : [T] . -> Rep { captures (Rep, Rep.mk); impl make_rep_impl; }; \
                 fn keep() -> . { () }",
            ),
            (
                "pkg/main",
                "module pkg/main; \
                 import pkg/refl(Rep); \
                 import pkg/refl(macro make_rep); \
                 fn main() -> Rep { make_rep[.](()) }",
            ),
        ]);
        let main_entry = prime.module("pkg/main").expect("main module");
        let main = &main_entry.module;
        let replay_alias = main
            .imports
            .iter()
            .find_map(|import_| match &import_.kind {
                ImportKind::Qualified { path, alias, .. }
                    if module_path_key(path) == "pkg/refl" =>
                {
                    Some(alias)
                }
                _ => None,
            })
            .expect("capture replay imports the captured declaration module");
        let qualified = crate::pass::resolve::qualify_type_segments_in_entry(
            &[
                PathSegment::new(replay_alias.clone(), Span::new(0, 0)),
                PathSegment::new("Rep".to_owned(), Span::new(0, 0)),
            ],
            main_entry,
            &std::collections::HashMap::new(),
        );
        assert_eq!(
            qualified
                .iter()
                .map(PathSegment::as_str)
                .collect::<Vec<_>>(),
            vec!["pkg", "refl", "Rep"],
            "the substituted package scope must index its generated qualified import"
        );
        assert!(
            contains_qualified_call_to(&prime_fn(main, "main").body, replay_alias.as_str(), "mk",),
            "the replayed constructor call must survive beneath any ABI-neutral source wrapper"
        );
        assert!(
            !format!("{main:?}").contains("UserMacro"),
            "Prime output should not retain a user macro surface node"
        );
    }

    /// A module with no macro / `if`/`else` positions has no recorded
    /// elaborations, so substitution is a pure phase-rebrand. Item
    /// count, item kinds, and module path / import list survive verbatim.
    #[test]
    fn no_elaborations_passes_through_unchanged() {
        let src = "module x/main; \
                   fn id[A](x: A) -> A { x } \
                   fn use_id(y: .) -> . { id[.](y) }";
        let module = substitute_one("x/main", src);
        assert_eq!(module.items.len(), 2);
        assert_eq!(module.imports.len(), 0);
        assert_eq!(module.path.segments, vec!["x", "main"]);
        for item in &module.items {
            let Item::FnDef(d) = item else {
                panic!("expected FnDef, got {item:?}");
            };
            assert!(d.body.span().start <= d.body.span().end);
        }
    }

    #[test]
    fn type_only_unit_domain_eta_materializes_until_unit_is_written() {
        let module = substitute_one(
            "main",
            "module main; \
             host type N; \
             type Unit_alias = .; \
             fn identity[A](value: A) -> A { value } \
             fn implicit() -> . { identity() } \
             fn explicit() -> . { identity(()) } \
             fn explicit_type_and_value() -> . { identity[.](()) } \
             fn selected_unit() -> . -> . { identity[.] } \
             fn selected_unit_alias() -> Unit_alias -> Unit_alias { identity[Unit_alias] } \
             fn residual() -> N -> N { identity[N] } \
             fn product[A](value: A, tail: .) -> . { tail } \
             fn product_residual() -> (. & .) -> . { product[.] }",
        );

        for name in ["implicit", "explicit", "explicit_type_and_value"] {
            let Expr::Call { args, .. } = &prime_fn(&module, name).body else {
                panic!("{name} must remain a call")
            };
            assert!(
                matches!(
                    args.as_slice(),
                    [
                        CallArg::Type(Type::Unit { .. }),
                        CallArg::Value(Expr::Unit { .. })
                    ]
                ),
                "{name} must have one canonical Unit type and one Unit value: {args:?}"
            );
        }

        for (name, callee) in [
            ("selected_unit", "identity"),
            ("selected_unit_alias", "identity"),
            ("residual", "identity"),
            ("product_residual", "product"),
        ] {
            let expression = &prime_fn(&module, name).body;
            let residual = match expression {
                Expr::FnExpr { body, .. } => body.as_ref(),
                Expr::Let { body, .. } => match body.as_ref() {
                    Expr::FnExpr { body, .. } => body.as_ref(),
                    other => {
                        panic!("{name} must eta-materialize after its staged prefix: {other:?}")
                    }
                },
                other => {
                    panic!("{name} must eta-materialize its first residual value layer: {other:?}")
                }
            };
            assert!(
                contains_call_to(expression, callee),
                "the staged prefix must specialize the named function: {expression:?}"
            );
            assert!(
                matches!(residual, Expr::Call { .. }),
                "the residual wrapper must apply its bound staged prefix: {residual:?}"
            );
        }
    }

    #[test]
    fn residual_unit_eta_forwards_its_semantic_argument() {
        let module = substitute_one(
            "main",
            "module main; \
             fn producer[A] -> . -> . { .(value: .) { value } } \
             fn selected() -> . -> . { producer[.] }",
        );
        let Expr::Let { value, body, .. } = &prime_fn(&module, "selected").body else {
            panic!("the consumed type stage must remain an outer binding")
        };
        assert!(matches!(value.as_ref(), Expr::Call { args, .. }
            if matches!(args.as_slice(), [CallArg::Type(Type::Unit { .. })])));
        let Expr::FnExpr { sig, body, .. } = body.as_ref() else {
            panic!("the residual value stage must be an ordinary lambda")
        };
        let [SignatureParam::Value(param)] = sig.params.as_slice() else {
            panic!("runtime-erased Unit must retain its semantic binder: {sig:?}")
        };
        assert!(matches!(param.ty, Some(Type::Unit { .. })));
        let Expr::Call { args, .. } = body.as_ref() else {
            panic!("the residual lambda must call its captured prefix")
        };
        assert!(
            matches!(args.as_slice(), [CallArg::Value(Expr::Path { segments, .. })]
            if segments.len() == 1 && segments[0].name == param.name)
        );
    }

    #[test]
    fn empty_call_preserves_a_returned_forall_layer_before_contextual_instantiation() {
        let module = substitute_one(
            "main",
            "module main; \
             host type N; \
             fn return_poly(_unit: .)[A](value: A) -> A { value } \
             fn residual() -> N -> N { return_poly() }",
        );

        let Expr::Let {
            value,
            body: residual_body,
            ..
        } = &prime_fn(&module, "residual").body
        else {
            panic!("the consumed Unit prefix must be forced before residual formation")
        };
        assert!(
            matches!(residual_body.as_ref(), Expr::FnExpr { .. }),
            "the returned function must eta-materialize after the forced prefix"
        );
        let Expr::Call {
            callee: outer_callee,
            args: outer_args,
            ..
        } = value.as_ref()
        else {
            panic!("expected returned-forall type application")
        };
        assert!(
            matches!(outer_args.as_slice(), [CallArg::Type(Type::Path { segments, .. })]
                if segments.last().is_some_and(|segment| segment.as_str() == "N")),
            "the returned forall must be instantiated by N: {outer_args:?}"
        );
        let Expr::Call {
            callee: inner_callee,
            args: inner_args,
            ..
        } = outer_callee.as_ref()
        else {
            panic!(
                "expected Unit application before returned-forall instantiation: {outer_callee:?}"
            )
        };
        assert!(
            matches!(inner_callee.as_ref(), Expr::Path { segments, .. }
                if segments.last().is_some_and(|segment| segment.as_str() == "return_poly")),
            "expected return_poly as the inner callee: {inner_callee:?}"
        );
        assert!(
            matches!(inner_args.as_slice(), [CallArg::Value(Expr::Unit { .. })]),
            "the first callable layer must receive Unit before N is applied: {inner_args:?}"
        );
    }

    #[test]
    fn multi_layer_staged_call_substitutes_to_nested_prime_calls() {
        let src = "module main; \
                   type { A: . { pub mk; pub get; }; }; \
                   type { B: . { pub mk; pub get; }; }; \
                   fn foo[A](v1: A)[B](v2: B) -> . { () } \
                   fn flat() -> . { foo[A](A.mk(()))[B](B.mk(())) }";
        let module = substitute_one("main", src);
        let flat = prime_fn(&module, "flat");

        let Expr::Call {
            callee: outer_callee,
            args: outer_args,
            ..
        } = &flat.body
        else {
            panic!("expected outer call, got {body:?}", body = flat.body);
        };
        assert_eq!(outer_args.len(), 2);
        let Expr::Call {
            callee: inner_callee,
            args: inner_args,
            ..
        } = outer_callee.as_ref()
        else {
            panic!("expected nested callee call, got {outer_callee:?}");
        };
        assert_eq!(inner_args.len(), 2);
        assert!(
            matches!(
                inner_callee.as_ref(),
                Expr::Path { segments, .. }
                    if segments.len() == 1 && segments[0].as_str() == "foo"
            ),
            "expected inner callee to be `foo`, got {inner_callee:?}"
        );
    }

    #[test]
    fn inferred_two_layer_call_preserves_written_stage_boundaries() {
        let src = "module main; \
                   host type I32; \
                   host type String; \
                   fn curried[T](first: T)(second: T) -> T { first } \
                   fn curried_two[A][B](first: A)(second: A) -> A { first } \
                   fn flat(first: I32, second: I32) -> I32 { curried(first)(second) } \
                   fn flat_two(first: I32, second: I32) -> I32 { \
                     curried_two[_][String](first)(second) \
                   } \
                   fn ufcs(first: I32, second: I32) -> I32 { (first.>curried)(second) }";
        let module = substitute_one("main", src);
        let flat = prime_fn(&module, "flat");

        let Expr::Call {
            callee: outer_callee,
            args: outer_args,
            ..
        } = &flat.body
        else {
            panic!("expected outer call, got {body:?}", body = flat.body);
        };
        let [
            CallArg::Value(Expr::Path {
                segments: outer_segments,
                ..
            }),
        ] = outer_args.as_slice()
        else {
            panic!("the outer layer should receive only `second`, got {outer_args:?}");
        };
        assert_eq!(
            outer_segments.last().map(PathSegment::as_str),
            Some("second")
        );
        let Expr::Call {
            callee: inner_callee,
            args: inner_args,
            ..
        } = outer_callee.as_ref()
        else {
            panic!("expected nested callee call, got {outer_callee:?}");
        };
        let [
            CallArg::Type(Type::Path {
                segments: inferred_segments,
                ..
            }),
            CallArg::Value(Expr::Path {
                segments: first_segments,
                ..
            }),
        ] = inner_args.as_slice()
        else {
            panic!("the inner layer should receive inferred `I32` and `first`: {inner_args:?}");
        };
        assert_eq!(
            inferred_segments.last().map(PathSegment::as_str),
            Some("I32")
        );
        assert_eq!(
            first_segments.last().map(PathSegment::as_str),
            Some("first")
        );
        assert!(
            matches!(
                inner_callee.as_ref(),
                Expr::Path { segments, .. }
                    if segments.len() == 1 && segments[0].as_str() == "curried"
            ),
            "expected inner callee to be `curried`, got {inner_callee:?}"
        );

        let flat_two = prime_fn(&module, "flat_two");
        let Expr::Call {
            callee: outer_callee,
            args: outer_args,
            ..
        } = &flat_two.body
        else {
            panic!(
                "expected multi-binder outer call, got {body:?}",
                body = flat_two.body
            );
        };
        assert!(
            matches!(
                outer_args.as_slice(),
                [CallArg::Value(Expr::Path { segments, .. })]
                    if segments.last().map(PathSegment::as_str) == Some("second")
            ),
            "the multi-binder outer layer should receive only `second`: {outer_args:?}"
        );
        let Expr::Call {
            callee: inner_callee,
            args: inner_args,
            ..
        } = outer_callee.as_ref()
        else {
            panic!("expected multi-binder nested callee call, got {outer_callee:?}");
        };
        assert!(
            matches!(
                inner_args.as_slice(),
                [
                    CallArg::Type(Type::Path {
                        segments: inferred,
                        ..
                    }),
                    CallArg::Type(Type::Path {
                        segments: explicit,
                        ..
                    }),
                    CallArg::Value(Expr::Path {
                        segments: first,
                        ..
                    }),
                ] if inferred.last().map(PathSegment::as_str) == Some("I32")
                    && explicit.last().map(PathSegment::as_str) == Some("String")
                    && first.last().map(PathSegment::as_str) == Some("first")
            ),
            "the global split after [A, B, first] must retain every inner slot: {inner_args:?}"
        );
        assert!(
            matches!(
                inner_callee.as_ref(),
                Expr::Path { segments, .. }
                    if segments.len() == 1 && segments[0].as_str() == "curried_two"
            ),
            "expected multi-binder inner callee to be `curried_two`, got {inner_callee:?}"
        );

        let ufcs = prime_fn(&module, "ufcs");
        let Expr::Call {
            callee: outer_callee,
            args: outer_args,
            ..
        } = &ufcs.body
        else {
            panic!(
                "the real UFCS rewrite should substitute to an outer call, got {body:?}",
                body = ufcs.body
            );
        };
        assert!(
            matches!(
                outer_args.as_slice(),
                [CallArg::Value(Expr::Path { segments, .. })]
                    if segments.last().map(PathSegment::as_str) == Some("second")
            ),
            "the normalized UFCS outer layer should receive only `second`: {outer_args:?}"
        );
        let Expr::Call {
            callee: inner_callee,
            args: inner_args,
            ..
        } = outer_callee.as_ref()
        else {
            panic!("expected normalized UFCS nested call, got {outer_callee:?}");
        };
        assert!(
            matches!(
                inner_args.as_slice(),
                [
                    CallArg::Type(Type::Path { segments: inferred, .. }),
                    CallArg::Value(Expr::Path { segments: receiver, .. }),
                ] if inferred.last().map(PathSegment::as_str) == Some("I32")
                    && receiver.last().map(PathSegment::as_str) == Some("first")
            ),
            "the normalized UFCS inner layer must retain [I32, first]: {inner_args:?}"
        );
        assert!(
            matches!(
                inner_callee.as_ref(),
                Expr::Path { segments, .. }
                    if segments.len() == 1 && segments[0].as_str() == "curried"
            ),
            "expected normalized UFCS inner callee to be `curried`, got {inner_callee:?}"
        );
    }

    #[test]
    fn bare_and_explicit_unit_ufcs_lower_to_explicit_prime_calls() {
        let src = "module main; \
                   fn one(value: .) -> . { value } \
                   fn two(first: ., _second: .) -> . { first } \
                   fn bare_receiver_first() -> . { ().>one } \
                   fn bare_receiver_last() -> . { ().>>one } \
                   fn bare_argument_last() -> . { one.<() } \
                   fn bare_argument_first() -> . { one.<<() } \
                   fn unit_receiver_first() -> . { ().>two(()) } \
                   fn unit_receiver_last() -> . { ().>>two(()) } \
                   fn unit_argument_last() -> . { two(()).<() } \
                   fn unit_argument_first() -> . { two(()).<<() }";
        let module = substitute_one("main", src);

        for (name, expected_args) in [
            ("bare_receiver_first", 1),
            ("bare_receiver_last", 1),
            ("bare_argument_last", 1),
            ("bare_argument_first", 1),
            ("unit_receiver_first", 2),
            ("unit_receiver_last", 2),
            ("unit_argument_last", 2),
            ("unit_argument_first", 2),
        ] {
            let function = prime_fn(&module, name);
            let Expr::Call { callee, args, .. } = &function.body else {
                panic!(
                    "{name} did not lower to an explicit Prime call: {:?}",
                    function.body
                );
            };
            assert!(
                matches!(callee.as_ref(), Expr::Path { segments, .. }
                if segments.last().is_some_and(|segment| {
                    segment.as_str() == if expected_args == 1 { "one" } else { "two" }
                })),
                "{name} lowered to the wrong callee: {callee:?}"
            );
            let [CallArg::Value(packet)] = args.as_slice() else {
                panic!("{name} must retain exactly one public value packet: {args:?}");
            };
            if expected_args == 1 {
                assert!(matches!(packet, Expr::Unit { .. }), "{name}: {packet:?}");
            } else {
                let (first, second) = pair_values(packet);
                assert!(
                    matches!(first, Expr::Unit { .. }),
                    "{name}: first written Unit"
                );
                assert!(
                    matches!(second, Expr::Unit { .. }),
                    "{name}: second written Unit"
                );
            }
        }
    }

    #[test]
    fn imported_conditional_recipe_substitutes_to_an_ordinary_intrinsic() {
        let src = "module x; \
                   import control(Bool, macro if); \
                    \
                   fn pick(c: Bool) -> Bool { if (c) { .t } else { .f } } \
                   ";
        let parsed_module = parse(src).expect("parse module");
        let control = parse(include_str!(
            "../../../../test-data/poc/macros/workdir/control.kio"
        ))
        .expect("parse ordinary control provider");
        let (lowered_modules, _lowered_package_file) = FullPipeline::lower_package(
            vec![
                (module_file_path("x"), parsed_module),
                (module_file_path("control"), control),
            ],
            None,
        )
        .expect("lower_package");
        let package = Package::build(Path::new(""), lowered_modules, None).expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        let prime = check_and_substitute(&package);
        let module = &prime.module("x").expect("module present").module;
        let pick = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(d) if d.name == "pick" => Some(d),
                _ => None,
            })
            .expect("expected FnDef `pick`");
        assert!(
            contains_call_to(&pick.body, "__if_then_else__"),
            "expected __if_then_else__ call in elaborated body: {body:?}",
            body = pick.body,
        );
    }

    /// The package file's `bridge` glob list is phase-independent and
    /// survives the Lowered → Prime substitution; the bridged module's
    /// `pub fn` (the derived export) survives as an ordinary module item.
    #[test]
    fn package_file_walks_through_bridge_block() {
        let src = "module x/main; \
                   pub fn one() -> . { () }";
        let package_file_src = "package x; \
                                bridge { x/main; }";
        let parsed_root = parse("module x;").expect("parse root module");
        let parsed_module = parse(src).expect("parse module");
        let parsed_package_file = crate::pass::parser::parse_package_file(package_file_src, None)
            .expect("parse package file");
        let package_file_entry = PackageFileEntry {
            file_path: PathBuf::from("x.pkg.kio"),
            package_name: "x".to_owned(),
            package_file: parsed_package_file,
        };
        let (lowered_modules, lowered_package_file) = FullPipeline::lower_package(
            vec![
                (module_file_path("x"), parsed_root),
                (module_file_path("x/main"), parsed_module),
            ],
            Some(package_file_entry.package_file),
        )
        .expect("lower_package");
        let lowered_package_file_entry =
            lowered_package_file.map(|package_file| PackageFileEntry {
                file_path: package_file_entry.file_path.clone(),
                package_name: package_file_entry.package_name.clone(),
                package_file,
            });
        let package = Package::build(Path::new(""), lowered_modules, lowered_package_file_entry)
            .expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        let prime = check_and_substitute(&package);
        let entry = prime.package_file().expect("package file present");
        let bridge = entry.package_file.bridge.as_ref().expect("bridge block");
        assert_eq!(
            bridge.globs.len(),
            1,
            "bridge glob did not survive substitution"
        );
        let (_, main_entry) = prime
            .modules()
            .find(|(path, _)| *path == "x/main")
            .expect("x/main module present post-substitution");
        assert!(
            main_entry
                .module
                .items
                .iter()
                .any(|item| matches!(item, Item::FnDef(d) if d.name == "one")),
            "pub fn `one` (derived export) not found post-substitution"
        );
    }

    fn contains_call_to<P: crate::ast::Phase>(e: &Expr<P>, name: &str) -> bool {
        match e {
            Expr::Call { callee, args, .. } => {
                if let Expr::Path { segments, .. } = callee.as_ref()
                    && segments.last().map(|s| s.as_str()) == Some(name)
                {
                    return true;
                }
                contains_call_to(callee, name)
                    || args.iter().any(|a| match a {
                        CallArg::Value(v) => contains_call_to(v, name),
                        CallArg::Type(_) => false,
                    })
            }
            Expr::FnExpr { body, .. } => contains_call_to(body, name),
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                contains_call_to(value, name) || contains_call_to(body, name)
            }
            _ => false,
        }
    }

    fn contains_qualified_call_to<P: crate::ast::Phase>(
        e: &Expr<P>,
        qualifier: &str,
        name: &str,
    ) -> bool {
        match e {
            Expr::Call { callee, args, .. } => {
                if let Expr::Path { segments, .. } = callee.as_ref()
                    && segments.first().map(PathSegment::as_str) == Some(qualifier)
                    && segments.last().map(PathSegment::as_str) == Some(name)
                {
                    return true;
                }
                contains_qualified_call_to(callee, qualifier, name)
                    || args.iter().any(|arg| match arg {
                        CallArg::Value(value) => contains_qualified_call_to(value, qualifier, name),
                        CallArg::Type(_) => false,
                    })
            }
            Expr::FnExpr { body, .. } => contains_qualified_call_to(body, qualifier, name),
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                contains_qualified_call_to(value, qualifier, name)
                    || contains_qualified_call_to(body, qualifier, name)
            }
            _ => false,
        }
    }
}
#[test]
fn pre_prime_forbids_open_type_goals() {
    fn assert_forbidden<P: Phase<TypeGoal = Never>>() {}
    assert_forbidden::<PrePrime>();
}
