//! Kio' → TypeScript `.d.ts` skin renderer.
//!
//! TypeScript is a pure-skin backend (`super` module docs): the runtime
//! `<ns>.js` is the JS backend's, byte-identical, and the only new
//! artifact is the `<ns>.d.ts` sidecar this module renders. The
//! `.d.ts` is *type annotations for that JS* — it declares the typed
//! FFI skin a TypeScript host calls against, with no runtime code.
//!
//! Because the runtime body is the JS backend's verbatim, every value
//! crosses the boundary in the JS backend's shape (`specs/backends/js.md`
//! § FFI surface): a product `(A & B)` is a `{ _0, _1 }` object keyed by
//! the right-spine slot, a sum is a single-keyed object, a payload-visible
//! newtype is `{ <ffi_key>: payload }`, a member-hidden newtype is a private
//! nominal handle, and an atomic role type is a JS-native
//! primitive, unit is `null`, a function value is a JS function. This
//! renderer emits the *TypeScript types* of exactly those shapes. Public
//! callables, nominal identities, semantic structural keys, and paired source
//! parameter partitions all come from the shared
//! [`PreparedBoundaryCallableSites`] transaction. The JavaScript emitter
//! consumes that same prepared contract for its runtime adapters, so neither
//! backend independently reconstructs the public boundary from raw types.
//!
//! What the `.d.ts` declares (`specs/backends/ts.md`) — every name
//! branded off the artifact stem `<Handle>` (PascalCase of the
//! namespace, `specs/backends/README.md` § Branded naming), so two
//! packages' skins never collide in one host program:
//!
//! - `<Handle>HostTypes` — the declaration-keyed selection map for every
//!   roleless host type. Nullary slots select a concrete type directly;
//!   positive-arity slots select a structural `<Handle>TypeLambda` witness.
//! - `<Handle>Host<B>` — one readonly callable property per `host fn`, nested
//!   by the declaring module's runtime namespace (rung 1, the same
//!   `__host__.<NS>.<fn>` nesting the JS factory expects), with every
//!   roleless type resolved through `B`.
//! - `create<Handle><B>(host)` — the JS factory, with hostful inference and
//!   hostless explicit selection governed by the generated binding validator.
//! - `<Handle><B>` and `<Handle>Types<B>` — the exported value surface and
//!   readable public type catalogue, both retaining the root selection.
//! - `<Handle>TypeLambda`, `<Handle>Apply`, and `<Handle>Bind` — the
//!   package-branded but structurally interoperable natural-exact protocol.
//! - Structural shapes are anonymous TS object / union types matching what the
//!   JS `.js` produces. A recursive reference behind an atomic hidden carrier
//!   may use a private, unbranded structural interface only to keep that shape
//!   finite. Member-hidden and existential newtypes, plus payload-visible
//!   recursive cycles not cut by an atomic carrier, use generated
//!   declaration-private nominal carriers, per `specs/backends/ts.md` § FFI
//!   surface.

use crate::ast::{Kind, Role, Routed};
use crate::backends::boundary_facade::{
    BoundaryCallableHeadStage, BoundaryFacadeExecutionPlan, BoundaryFacadeExecutionUse,
    BoundaryFacadePlan, BoundaryFacadeSiteOwner, BoundaryHostBinding, BoundaryHostBindingOrigin,
    BoundaryHostTypeBinding, BoundaryNewtypePayloadPlan, BoundaryNominalDeclaration,
    BoundaryNominalDependencies, BoundaryPublicNewtypeInventoryEntry, CallableExecutionStage,
    CallablePresentationLayout, CallableSourceParamAdapter, CallableValueStageLayout,
    FacadeBinderId, FacadeUse, FacadeUseId, PreparedBoundaryCallableSite,
    PreparedBoundaryCallableSites, QualifiedTypeName, SemanticKey,
};
use crate::backends::js::emit as js;
use crate::backends::public_names::{
    FacadeSelector, host_module_key, host_name_core, module_facade_path,
};
use crate::pass::resolve::Package;
use std::collections::{BTreeMap, BTreeSet};

#[cfg(all(test, feature = "surface"))]
use crate::host_descriptor::exact_host_type_role;
#[cfg(all(test, feature = "surface"))]
type Type = crate::ast::Type<Routed>;
#[derive(Default)]
struct OpaqueTsTypes {
    nominal: BTreeMap<String, Vec<Kind>>,
    nominal_retained_at: BTreeMap<String, u32>,
    recursive_structural: BTreeSet<QualifiedTypeName>,
    recursive_structural_pending: BTreeSet<QualifiedTypeName>,
    recursive_structural_retained_at: BTreeMap<QualifiedTypeName, u32>,
    #[cfg(test)]
    recursive_structural_selection_steps: usize,
    structural_stack: Vec<String>,
    host_binding_origins: BTreeMap<QualifiedTypeName, BoundaryHostBindingOrigin>,
    live_semantic_nominals: BTreeSet<QualifiedTypeName>,
    active_retained_at: Option<u32>,
}

#[derive(Clone, Copy)]
struct TsNominalPresentation<'a> {
    declaration: &'a BoundaryNominalDeclaration,
    dependencies: &'a BoundaryNominalDependencies,
    layout: &'a CallablePresentationLayout,
}

impl OpaqueTsTypes {
    fn insert(&mut self, rendered: String, kinds: Vec<Kind>, semantic: &QualifiedTypeName) {
        self.nominal.insert(rendered.clone(), kinds);
        if let Some(version) = self
            .active_retained_at
            .filter(|_| !self.live_semantic_nominals.contains(semantic))
        {
            self.mark_nominal_retained(&rendered, version);
        } else if self.live_semantic_nominals.contains(semantic) {
            self.nominal_retained_at.remove(&rendered);
        }
    }

    fn mark_nominal_retained(&mut self, name: &str, version: u32) {
        self.nominal_retained_at
            .entry(name.to_owned())
            .and_modify(|retained_at| *retained_at = (*retained_at).min(version))
            .or_insert(version);
    }

    fn insert_recursive_structural(&mut self, name: &QualifiedTypeName) {
        if self.recursive_structural.insert(name.clone()) {
            self.recursive_structural_pending.insert(name.clone());
        }
        if let Some(version) = self
            .active_retained_at
            .filter(|_| !self.live_semantic_nominals.contains(name))
        {
            self.recursive_structural_retained_at
                .entry(name.clone())
                .and_modify(|retained_at| *retained_at = (*retained_at).min(version))
                .or_insert(version);
        } else if self.live_semantic_nominals.contains(name) {
            self.recursive_structural_retained_at.remove(name);
        }
    }

    fn pop_next_recursive_structural(&mut self) -> Option<QualifiedTypeName> {
        let next = self.recursive_structural_pending.pop_first();
        #[cfg(test)]
        if next.is_some() {
            self.recursive_structural_selection_steps += 1;
        }
        next
    }
}

struct TsNames {
    handle: String,
    host_types: String,
    type_lambda: String,
    partial_type_lambda: String,
    apply: String,
    bind: String,
    type_error: String,
    types: String,
}

impl TsNames {
    fn from_brand(brand: &js::Brand) -> Self {
        Self {
            handle: brand.handle.clone(),
            host_types: format!("{}HostTypes", brand.handle),
            type_lambda: format!("{}TypeLambda", brand.handle),
            partial_type_lambda: format!("{}PartialTypeLambda", brand.handle),
            apply: format!("{}Apply", brand.handle),
            bind: format!("{}Bind", brand.handle),
            type_error: format!("{}TypeError", brand.handle),
            types: format!("{}Types", brand.handle),
        }
    }
}

/// A `.d.ts` render failure. Mirrors the JS emitter's `EmitError`
/// shape so the build dispatcher reports both targets uniformly.
#[derive(Debug)]
pub struct EmitError {
    pub message: String,
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EmitError {}

/// Render the package's `<ns>.d.ts` skin: the natural-exact type protocol,
/// `<Handle>HostTypes`, the validated `<Handle>Host<B>` and `<Handle><B>`
/// aliases, `<Handle>Types<B>`, and the `create<Handle>` factory.
/// Every public name is branded off the artifact stem `ns` (the
/// `namespace` key, else the package name) via [`js::Brand`], the same
/// derivation the JS factory uses, so the `.d.ts` `create<Handle>`
/// declaration and the `.js` factory always agree.
///
/// The `.js` that this `.d.ts` annotates is produced separately by the
/// JS emitter ([`crate::backends::js::emit::lower_package_to_factory_module`]),
/// byte-identical to `target=js`; the build dispatcher writes both.
pub fn lower_package_to_dts(package: &Package<Routed>, ns: &str) -> Result<String, EmitError> {
    lower_package_to_dts_with_signature(package, ns, None)
}

/// Render a declaration skin whose sealed removals remain as optional,
/// deprecated Host properties with their exact frozen type dependencies.
/// Retained sites never contribute a package member or JavaScript dispatch
/// path.
pub fn lower_package_to_dts_with_signature(
    package: &Package<Routed>,
    ns: &str,
    sig: Option<&(u32, crate::sig::ReplayedInterface)>,
) -> Result<String, EmitError> {
    let brand = js::Brand::derive(ns);
    let names = TsNames::from_brand(&brand);
    let mut out = String::new();
    out.push_str("// Generated by kio — do not edit by hand.\n");

    let prepared =
        PreparedBoundaryCallableSites::collect(package, sig.map(|(_, replayed)| replayed))
            .map_err(|error| {
                internal_type_error(&format!("TypeScript facade collection failed: {error}"))
            })?;
    let mut opaque_types = OpaqueTsTypes {
        host_binding_origins: prepared
            .host_bindings()
            .map(|binding| (binding.name().clone(), binding.origin()))
            .collect(),
        live_semantic_nominals: prepared
            .public_newtypes()
            .map(|declaration| declaration.name().clone())
            .chain(
                prepared
                    .sites()
                    .filter(|site| site.execution().is_some())
                    .flat_map(|site| site.nominals().declarations().map(|(name, _)| name.clone())),
            )
            .collect(),
        ..OpaqueTsTypes::default()
    };
    let source_stable_host_types = source_stable_host_types(&prepared);
    let nominal_presentations = index_nominal_presentations(&prepared);

    render_type_lambda_protocol(&brand, &names, &mut out);
    render_host_types(&source_stable_host_types, &brand, &names, &mut out);
    render_host_interface(&prepared, &brand, &names, &mut opaque_types, &mut out)?;
    render_package_interface(&prepared, &brand, &names, &mut opaque_types, &mut out)?;
    render_type_catalog(
        &prepared,
        &source_stable_host_types,
        &nominal_presentations,
        &names,
        &mut opaque_types,
        &mut out,
    )?;
    render_recursive_structural_interfaces(
        &nominal_presentations,
        &names,
        &mut opaque_types,
        &mut out,
    )?;
    render_opaque_type_declarations(&opaque_types, &names, &mut out);
    render_create_package(&prepared, &brand, &names, &mut out);

    Ok(out)
}

fn render_type_lambda_protocol(brand: &js::Brand, names: &TsNames, out: &mut String) {
    let private = format!("__{}", brand.handle);
    out.push_str(&format!(
        r#"
declare const {private}TypeErrorBrand: unique symbol;

export interface {error}<Message extends string> {{
  readonly [{private}TypeErrorBrand]: (message: Message) => Message;
}}

export interface {lambda}<Args extends readonly unknown[]> {{
  readonly arguments: Args;
  readonly __kio_type_lambda_variance?: (args: Args) => Args;
}}

interface {private}TypeLambdaLike {{
  readonly arguments: readonly unknown[];
}}

export interface {partial}<
  Root extends {private}TypeLambdaLike,
  Prefix extends readonly unknown[],
  Remaining extends readonly unknown[],
> extends {lambda}<Remaining> {{
  readonly __kio_type_lambda_partial: readonly [Root, Prefix];
}}

interface {private}MissingTypeLambdaResult {{
  readonly {private}MissingTypeLambdaResult: unique symbol;
}}

interface {private}WrongArityTypeLambdaResult {{
  readonly {private}WrongArityTypeLambdaResult: unique symbol;
}}

interface {private}BindingArgument<Index extends number> {{
  readonly {private}BindingArgument: Index;
}}

type {private}IsAny<T> = 0 extends (1 & T) ? true : false;
type {private}IsNever<T> = [T] extends [never] ? true : false;
type {private}IsUnknown<T> = {private}IsAny<T> extends true
  ? false
  : unknown extends T
    ? true
    : false;

type {private}RawApply<F, Args extends readonly unknown[]> =
  F extends {{ readonly __kio_type_lambda_partial: readonly [infer Root, infer Prefix] }}
    ? {private}IsAny<Root> extends true
      ? {private}WrongArityTypeLambdaResult
      : {private}IsNever<Root> extends true
        ? {private}WrongArityTypeLambdaResult
        : Root extends {private}TypeLambdaLike
          ? {private}IsAny<Prefix> extends true
            ? {private}WrongArityTypeLambdaResult
            : {private}IsNever<Prefix> extends true
              ? {private}WrongArityTypeLambdaResult
              : Prefix extends readonly unknown[]
                ? {private}HasSameFlattenedArity<Root, readonly [...Prefix, ...Args]> extends true
                  ? {private}RawApply<Root, readonly [...Prefix, ...Args]>
                  : {private}WrongArityTypeLambdaResult
                : {private}WrongArityTypeLambdaResult
          : {private}WrongArityTypeLambdaResult
    : (F & {{ readonly arguments: Args }}) extends {{ readonly type: infer Result }}
      ? Result
      : {private}MissingTypeLambdaResult;

type {private}SameArity<
  Expected extends readonly unknown[],
  Actual extends readonly unknown[],
> = Expected extends readonly []
  ? Actual extends readonly []
    ? true
    : false
  : Expected extends readonly [unknown, ...infer ExpectedTail]
    ? Actual extends readonly [unknown, ...infer ActualTail]
      ? {private}SameArity<ExpectedTail, ActualTail>
      : false
    : false;

type {private}ArgumentSatisfies<Expected, Actual> = unknown extends Expected
  ? true
  : [Actual] extends [Expected]
    ? true
    : false;

type {private}ArgumentsSatisfy<
  Expected extends readonly unknown[],
  Actual extends readonly unknown[],
> = Expected extends readonly []
  ? Actual extends readonly []
    ? true
    : false
  : Expected extends readonly [infer ExpectedHead, ...infer ExpectedTail]
    ? Actual extends readonly [infer ActualHead, ...infer ActualTail]
      ? {private}ArgumentSatisfies<ExpectedHead, ActualHead> extends true
        ? {private}ArgumentsSatisfy<ExpectedTail, ActualTail>
        : false
      : false
    : false;

type {private}CheckedApplyResult<Result> = {private}IsAny<Result> extends true
  ? {error}<"type-lambda application resolved to any">
  : {private}IsNever<Result> extends true
    ? {error}<"type-lambda application resolved to never">
    : {private}IsUnknown<Result> extends true
      ? {error}<"type-lambda application resolved to unknown">
      : Result extends {private}WrongArityTypeLambdaResult
        ? {error}<"type-lambda application has the wrong argument arity">
        : Result extends {private}MissingTypeLambdaResult
          ? {error}<"type lambda has no concrete result">
          : Result;

export type {apply}<
  F,
  Args,
> = {private}IsAny<F> extends true
  ? {error}<"cannot apply any as a type lambda">
  : {private}IsNever<F> extends true
    ? {error}<"cannot apply never as a type lambda">
    : {private}IsUnknown<F> extends true
      ? {error}<"cannot apply unknown as a type lambda">
      : F extends {private}TypeLambdaLike
        ? {private}IsAny<Args> extends true
          ? {error}<"type-lambda arguments cannot be any">
          : {private}IsNever<Args> extends true
            ? {error}<"type-lambda arguments cannot be never">
            : {private}IsUnknown<Args> extends true
              ? {error}<"type-lambda arguments cannot be unknown">
              : Args extends readonly unknown[]
                ? {private}HasSameFlattenedArity<F, Args> extends true
                  ? {private}FlattenedArgumentsAccept<F, Args> extends true
                    ? {private}CheckedApplyResult<{private}RawApply<F, Args>>
                    : {error}<"type-lambda arguments do not satisfy the constructor constraints">
                  : {error}<"type-lambda application has the wrong argument arity">
                : {error}<"type-lambda arguments must be a readonly tuple">
        : {error}<"value is not a type lambda">;

type {private}DropPrefix<
  Remaining extends readonly unknown[],
  Prefix extends readonly unknown[],
> = Prefix extends readonly []
  ? Remaining
  : Prefix extends readonly [infer PrefixHead, ...infer PrefixTail]
    ? Remaining extends readonly [infer RemainingHead, ...infer RemainingTail]
      ? {private}ArgumentSatisfies<RemainingHead, PrefixHead> extends true
        ? {private}DropPrefix<RemainingTail, PrefixTail>
        : never
      : never
    : never;

type {private}PrefixWithinArity<
  Remaining extends readonly unknown[],
  Prefix extends readonly unknown[],
> = Prefix extends readonly []
  ? true
  : Prefix extends readonly [unknown, ...infer PrefixTail]
    ? Remaining extends readonly [unknown, ...infer RemainingTail]
      ? {private}PrefixWithinArity<RemainingTail, PrefixTail>
      : false
    : false;

type {private}ArgumentsOf<F> = F extends {{ readonly arguments: infer Args }}
  ? Args extends readonly unknown[]
    ? Args
    : never
  : never;

type {private}FlattenedArgumentsOf<F> = {private}IsAny<F> extends true
  ? never
  : {private}IsNever<F> extends true
    ? never
    : F extends {{ readonly __kio_type_lambda_partial: readonly [infer Root, infer Prefix] }}
      ? {private}IsAny<Prefix> extends true
        ? never
        : {private}IsNever<Prefix> extends true
          ? never
          : Prefix extends readonly unknown[]
            ? {private}DropPrefix<{private}FlattenedArgumentsOf<Root>, Prefix>
            : never
      : {private}ArgumentsOf<F>;

type {private}HasSameFlattenedArity<
  F,
  Args extends readonly unknown[],
> = {private}IsNever<{private}FlattenedArgumentsOf<F>> extends true
  ? false
  : {private}SameArity<{private}FlattenedArgumentsOf<F>, Args>;

type {private}FlattenedArgumentsAccept<
  F,
  Args extends readonly unknown[],
> = {private}IsNever<{private}FlattenedArgumentsOf<F>> extends true
  ? false
  : {private}ArgumentsSatisfy<{private}FlattenedArgumentsOf<F>, Args>;

type {private}FlattenedConstraintsEqual<
  F,
  Args extends readonly unknown[],
> = {private}FlattenedArgumentsAccept<F, Args> extends true
  ? {private}ArgumentsSatisfy<Args, {private}FlattenedArgumentsOf<F>>
  : false;

export type {bind}<
  F,
  Prefix,
> = {private}IsAny<F> extends true
  ? {error}<"cannot bind any as a type lambda">
  : {private}IsNever<F> extends true
    ? {error}<"cannot bind never as a type lambda">
    : {private}IsUnknown<F> extends true
      ? {error}<"cannot bind unknown as a type lambda">
      : F extends {private}TypeLambdaLike
        ? {private}IsAny<Prefix> extends true
          ? {error}<"type-lambda prefix cannot be any">
          : {private}IsNever<Prefix> extends true
            ? {error}<"type-lambda prefix cannot be never">
            : {private}IsUnknown<Prefix> extends true
              ? {error}<"type-lambda prefix cannot be unknown">
              : Prefix extends readonly unknown[]
                ? {private}PrefixWithinArity<{private}FlattenedArgumentsOf<F>, Prefix> extends true
                  ? {private}DropPrefix<{private}FlattenedArgumentsOf<F>, Prefix> extends infer Remaining
                    ? {private}IsNever<Remaining> extends true
                      ? {error}<"type-lambda prefix does not satisfy the constructor constraints">
                      : Remaining extends readonly [unknown, ...unknown[]]
                        ? {partial}<F, Prefix, Remaining>
                        : {error}<"type-lambda prefix must leave at least one argument">
                    : {error}<"invalid type-lambda prefix">
                  : {error}<"type-lambda prefix exceeds the constructor arity">
                : {error}<"type-lambda prefix must be a readonly tuple">
        : {error}<"value is not a type lambda">;
"#,
        error = names.type_error,
        lambda = names.type_lambda,
        partial = names.partial_type_lambda,
        apply = names.apply,
        bind = names.bind,
    ));
}

fn roleless_host_types<'a>(
    source_stable: &[&'a BoundaryHostBinding],
) -> Vec<&'a BoundaryHostBinding> {
    source_stable
        .iter()
        .copied()
        .filter(|declaration| matches!(declaration.binding(), BoundaryHostTypeBinding::Roleless))
        .collect()
}

fn source_stable_host_types(prepared: &PreparedBoundaryCallableSites) -> Vec<&BoundaryHostBinding> {
    let retained_nominals = prepared
        .sites()
        .filter(|site| site.retained().is_some())
        .flat_map(|site| site.nominals().declarations().map(|(name, _)| name.clone()))
        .collect::<BTreeSet<_>>();
    prepared
        .host_bindings()
        .filter(|binding| {
            matches!(binding.origin(), BoundaryHostBindingOrigin::Live)
                || retained_nominals.contains(binding.name())
        })
        .collect()
}

fn index_nominal_presentations(
    prepared: &PreparedBoundaryCallableSites,
) -> BTreeMap<QualifiedTypeName, TsNominalPresentation<'_>> {
    let mut indexed = BTreeMap::new();
    // A live declaration owns shared support presentation when history reaches
    // the same exact identity; the retained pass fills only missing entries.
    for live in [true, false] {
        for site in prepared
            .sites()
            .filter(|site| site.execution().is_some() == live)
        {
            for (name, declaration) in site.nominals().declarations() {
                indexed
                    .entry(name.clone())
                    .or_insert(TsNominalPresentation {
                        declaration,
                        dependencies: site.nominals(),
                        layout: site.presentation(),
                    });
            }
        }
    }
    indexed
}

fn retained_host_binding_version(binding: &BoundaryHostBinding) -> Option<u32> {
    match binding.origin() {
        BoundaryHostBindingOrigin::Live => None,
        BoundaryHostBindingOrigin::Retained { removed_at_version } => Some(removed_at_version),
    }
}

fn render_deprecated_jsdoc(out: &mut String, indent: &str, message: &str) {
    out.push_str(indent);
    out.push_str("/** @deprecated ");
    out.push_str(message);
    out.push_str(" */\n");
}

fn render_host_types(
    source_stable: &[&BoundaryHostBinding],
    brand: &js::Brand,
    names: &TsNames,
    out: &mut String,
) {
    let declarations = roleless_host_types(source_stable);
    let mut by_module: BTreeMap<String, Vec<&BoundaryHostBinding>> = BTreeMap::new();
    for declaration in &declarations {
        // The descriptor already owns the canonical slash-separated module
        // path. It is exact declaration identity, unlike the runtime ABI key
        // whose spelling is optimized for property access. Nesting only
        // declaration leaves under this path makes additions monotone: a new
        // module or host type adds one key and cannot retarget an existing one.
        by_module
            .entry(ts_host_module_path(
                &declaration.name().module_segments().join("/"),
            ))
            .or_default()
            .push(declaration);
    }

    out.push_str(&format!("\nexport interface {} {{", names.host_types));
    if by_module.is_empty() {
        out.push_str("}\n");
    } else {
        out.push('\n');
        for (module, declarations) in &by_module {
            let module_is_retained = declarations
                .iter()
                .all(|declaration| retained_host_binding_version(declaration).is_some());
            if module_is_retained {
                render_deprecated_jsdoc(
                    out,
                    "  ",
                    &format!("Host module `{module}` is retained for removed host types."),
                );
            }
            out.push_str(&format!(
                "  readonly {}{}: {{\n",
                ts_property_key(module),
                if module_is_retained { "?" } else { "" }
            ));
            for declaration in declarations {
                let retained_at = retained_host_binding_version(declaration);
                let binding = if declaration.type_params().is_empty() {
                    "unknown".to_owned()
                } else {
                    let args = declaration
                        .type_params()
                        .iter()
                        .map(|parameter| type_lambda_argument_constraint(parameter.kind(), names))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{}<readonly [{args}]>", names.type_lambda)
                };
                if let Some(version) = retained_at {
                    render_deprecated_jsdoc(
                        out,
                        "    ",
                        &format!(
                            "Host type `{}.{}` was removed at v({version}).",
                            module,
                            declaration.name().name()
                        ),
                    );
                }
                out.push_str(&format!(
                    "    readonly {}{}: {binding};\n",
                    ts_property_key(&host_name_core(declaration.name().name())),
                    if retained_at.is_some() { "?" } else { "" }
                ));
            }
            out.push_str("  };\n");
        }
        out.push_str("}\n");
    }

    let private = format!("__{}", brand.handle);
    out.push_str(&format!(
        "\ntype {private}RejectBinding<Value, Name extends string> = {private}IsAny<Value> extends true\n  ? {}<`binding ${{Name}} cannot be any`>\n  : {private}IsNever<Value> extends true\n    ? {}<`binding ${{Name}} cannot be never`>\n    : {private}IsUnknown<Value> extends true\n      ? {}<`binding ${{Name}} must select a concrete type`>\n      : never;\n",
        names.type_error, names.type_error, names.type_error
    ));
    out.push_str(&format!(
        "\ntype {private}RejectConstructor<Value, Args extends readonly unknown[], Name extends string> =\n  {private}RejectBinding<Value, Name> extends never\n    ? Value extends {}<Args>\n      ? {private}FlattenedConstraintsEqual<Value, Args> extends true\n        ? {private}RawApply<Value, {{ [Index in keyof Args]: Index extends `${{infer N extends number}}` ? {private}BindingArgument<N> : Args[Index] }}> extends infer Result\n          ? Result extends {private}WrongArityTypeLambdaResult\n            ? {}<`binding ${{Name}} has the wrong constructor arity`>\n            : Result extends {private}MissingTypeLambdaResult\n              ? {}<`binding ${{Name}} has no concrete result`>\n              : {private}IsAny<Result> extends true\n                ? {}<`binding ${{Name}} resolves to any`>\n                : {private}IsNever<Result> extends true\n                  ? {}<`binding ${{Name}} resolves to never`>\n                  : {private}IsUnknown<Result> extends true\n                    ? {}<`binding ${{Name}} resolves to unknown`>\n                    : never\n          : never\n        : {}<`binding ${{Name}} has incompatible constructor constraints`>\n      : {}<`binding ${{Name}} has the wrong constructor arity`>\n    : {private}RejectBinding<Value, Name>;\n",
        names.type_lambda,
        names.type_error,
        names.type_error,
        names.type_error,
        names.type_error,
        names.type_error,
        names.type_error,
        names.type_error,
    ));

    out.push_str(&format!(
        "\ntype {private}BindingErrors<B extends {}> = ",
        names.host_types
    ));
    let live_declarations = declarations
        .iter()
        .copied()
        .filter(|declaration| matches!(declaration.origin(), BoundaryHostBindingOrigin::Live))
        .collect::<Vec<_>>();
    if live_declarations.is_empty() {
        out.push_str("never;\n");
    } else {
        let errors = live_declarations
            .iter()
            .map(|declaration| {
                let module = declaration.name().module_segments().join("/");
                let access = format!(
                    "B[{}][{}]",
                    ts_string_literal(&ts_host_module_path(&module)),
                    ts_string_literal(&host_name_core(declaration.name().name()))
                );
                let identity = format!("{}.{}", module, declaration.name().name());
                if declaration.type_params().is_empty() {
                    format!(
                        "{private}RejectBinding<{access}, {}>",
                        ts_string_literal(&identity)
                    )
                } else {
                    let args = declaration
                        .type_params()
                        .iter()
                        .map(|parameter| type_lambda_argument_constraint(parameter.kind(), names))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        "{private}RejectConstructor<{access}, readonly [{args}], {}>",
                        ts_string_literal(&identity)
                    )
                }
            })
            .collect::<Vec<_>>();
        out.push_str(&errors.join(" |\n  "));
        out.push_str(";\n");
    }
    out.push_str(&format!(
        "\ntype {private}Errors<B extends {}> = {private}IsAny<B> extends true\n  ? {}<\"host-type bindings cannot be any\">\n  : {private}IsNever<B> extends true\n    ? {}<\"host-type bindings must be selected\">\n    : {private}BindingErrors<B>;\n",
        names.host_types, names.type_error, names.type_error
    ));
    if declarations.iter().any(|declaration| {
        matches!(
            declaration.origin(),
            BoundaryHostBindingOrigin::Retained { .. }
        )
    }) {
        render_deprecated_jsdoc(
            out,
            "\n",
            "Internal compatibility helper for removed host declarations.",
        );
        out.push_str(&format!(
            "type {private}RetainedHostBinding<B, Module extends PropertyKey, Name extends PropertyKey> =\n  Module extends keyof B\n    ? NonNullable<B[Module]> extends infer Bindings\n      ? Name extends keyof Bindings\n        ? NonNullable<Bindings[Name]>\n        : unknown\n      : unknown\n    : unknown;\n"
        ));
    }
}

fn render_type_catalog(
    prepared: &PreparedBoundaryCallableSites,
    source_stable: &[&BoundaryHostBinding],
    nominal_presentations: &BTreeMap<QualifiedTypeName, TsNominalPresentation<'_>>,
    names: &TsNames,
    opaque_types: &mut OpaqueTsTypes,
    out: &mut String,
) -> Result<(), EmitError> {
    let private = format!("__{}", names.handle);
    let mut by_module: BTreeMap<String, BTreeMap<String, (String, Option<u32>)>> = BTreeMap::new();
    for declaration in source_stable {
        let module = declaration.name().module_segments().join("/");
        let retained_at = retained_host_binding_version(declaration);
        let ty = match declaration.binding() {
            BoundaryHostTypeBinding::Role(role) => role_to_ts_type(role).to_owned(),
            BoundaryHostTypeBinding::Roleless if retained_at.is_some() => format!(
                "{private}RetainedHostBinding<B, {}, {}>",
                ts_string_literal(&ts_host_module_path(&module)),
                ts_string_literal(&host_name_core(declaration.name().name()))
            ),
            BoundaryHostTypeBinding::Roleless => format!(
                "B[{}][{}]",
                ts_string_literal(&ts_host_module_path(&module)),
                ts_string_literal(&host_name_core(declaration.name().name()))
            ),
        };
        by_module
            .entry(ts_host_module_path(&module))
            .or_default()
            .insert(host_name_core(declaration.name().name()), (ty, retained_at));
    }

    let catalog_newtypes = prepared
        .public_newtypes()
        .map(|declaration| (declaration, None))
        .chain(prepared.retained_public_newtypes().map(|declaration| {
            (
                declaration,
                prepared.retained_public_newtype_removed_at(declaration.name()),
            )
        }))
        .collect::<Vec<_>>();
    for (declaration, retained_at) in catalog_newtypes {
        let module = declaration.name().module_segments().join("/");
        let ty = if declaration.type_params().is_empty() {
            render_catalog_newtype(
                declaration,
                &[],
                retained_at,
                nominal_presentations,
                names,
                opaque_types,
            )?
        } else {
            let witness = newtype_witness_name(names, declaration.name());
            let alias = format!(
                "__{}Newtype_H{}",
                names.handle,
                js::encoded_newtype_identity(&module, declaration.name().name())
            );
            let args = (0..declaration.type_params().len())
                .map(|index| format!("T{index}"))
                .collect::<Vec<_>>();
            let result = render_catalog_newtype(
                declaration,
                &args,
                retained_at,
                nominal_presentations,
                names,
                opaque_types,
            )?;
            let constraints = declaration
                .type_params()
                .iter()
                .map(|parameter| type_lambda_argument_constraint(parameter.kind(), names))
                .collect::<Vec<_>>()
                .join(", ");
            let generic_parameters = declaration
                .type_params()
                .iter()
                .enumerate()
                .map(|(index, parameter)| {
                    let binder = format!("T{index}");
                    if matches!(parameter.kind(), Kind::Star) {
                        binder
                    } else {
                        format!(
                            "{binder} extends {}",
                            type_lambda_argument_constraint(parameter.kind(), names)
                        )
                    }
                })
                .collect::<Vec<_>>();
            let witness_arguments = (0..declaration.type_params().len())
                .map(|index| format!("this[\"arguments\"][{index}]"))
                .collect::<Vec<_>>()
                .join(", ");
            if let Some(version) = retained_at {
                out.push('\n');
                render_deprecated_jsdoc(
                    out,
                    "",
                    &format!(
                        "Host type carrier `{}.{}` was removed at v({version}).",
                        module,
                        declaration.name().name()
                    ),
                );
            }
            out.push_str(&format!(
                "{}type {alias}<B extends {}, {}> = {result};\n\n",
                if retained_at.is_some() { "" } else { "\n" },
                names.host_types,
                generic_parameters.join(", "),
            ));
            if let Some(version) = retained_at {
                render_deprecated_jsdoc(
                    out,
                    "",
                    &format!(
                        "Host type carrier `{}.{}` was removed at v({version}).",
                        module,
                        declaration.name().name()
                    ),
                );
            }
            out.push_str(&format!(
                "interface {witness}<B extends {}> extends {}<readonly [{constraints}]> {{\n  readonly type: {alias}<B, {witness_arguments}>;\n}}\n",
                names.host_types,
                names.type_lambda,
            ));
            format!("{witness}<B>")
        };
        by_module
            .entry(ts_host_module_path(&module))
            .or_default()
            .insert(host_name_core(declaration.name().name()), (ty, retained_at));
    }

    out.push_str(&format!(
        "\ninterface {private}Types<B extends {}> {{",
        names.host_types
    ));
    if by_module.is_empty() {
        out.push_str("}\n");
    } else {
        out.push('\n');
        for (module, declarations) in by_module {
            let module_is_retained = declarations
                .values()
                .all(|(_, retained_at)| retained_at.is_some());
            if module_is_retained {
                render_deprecated_jsdoc(
                    out,
                    "  ",
                    &format!("Module `{module}` is retained only for removed host signatures."),
                );
            }
            out.push_str(&format!("  readonly {}: {{\n", ts_property_key(&module)));
            for (name, (ty, retained_at)) in declarations {
                if let Some(version) = retained_at {
                    render_deprecated_jsdoc(
                        out,
                        "    ",
                        &format!(
                            "Type `{module}.{name}` is retained only for host declarations removed at v({version})."
                        ),
                    );
                }
                out.push_str(&format!("    readonly {}: {ty};\n", ts_property_key(&name)));
            }
            out.push_str("  };\n");
        }
        out.push_str("}\n");
    }
    out.push_str(&format!(
        "\nexport type {}<B extends {} = {}> =\n  {private}Errors<B> extends never ? {private}Types<B> : {private}Errors<B>;\n",
        names.types, names.host_types, names.host_types
    ));
    Ok(())
}

fn render_catalog_newtype(
    inventory: &BoundaryPublicNewtypeInventoryEntry,
    args: &[String],
    retained_at: Option<u32>,
    nominal_presentations: &BTreeMap<QualifiedTypeName, TsNominalPresentation<'_>>,
    names: &TsNames,
    opaque_types: &mut OpaqueTsTypes,
) -> Result<String, EmitError> {
    let name = inventory.name();
    if args.len() != inventory.type_params().len() {
        return Err(internal_type_error(
            "type catalog newtype application disagrees with its declaration arity",
        ));
    }
    let carrier = opaque_ts_type_name(&name.module_segments().join("/"), name.name());
    let dependency = nominal_presentations.get(name).copied();
    let recursive = dependency.is_some_and(|dependency| match dependency.declaration {
        BoundaryNominalDeclaration::Newtype {
            transparent_payload: Some(payload),
            ..
        } => transparent_payload_is_recursive(name, payload, dependency.dependencies),
        _ => false,
    });
    if inventory.surface().uses_nominal_carrier()
        || !inventory.existential_params().is_empty()
        || recursive
    {
        opaque_types.insert(
            carrier.clone(),
            inventory
                .type_params()
                .iter()
                .map(|parameter| parameter.kind().clone())
                .collect(),
            name,
        );
        if let Some(version) =
            retained_at.filter(|_| !opaque_types.live_semantic_nominals.contains(name))
        {
            opaque_types.mark_nominal_retained(&carrier, version);
        }
        let mut all = vec!["B".to_owned()];
        all.extend(args.iter().cloned());
        return Ok(format!("{carrier}<{}>", all.join(", ")));
    }
    let Some(dependency) = dependency else {
        return Err(internal_type_error(
            "payload-visible public newtype has no prepared member dependency",
        ));
    };
    let BoundaryNominalDeclaration::Newtype {
        transparent_payload: Some(payload),
        ..
    } = dependency.declaration
    else {
        return Err(internal_type_error(
            "payload-visible public newtype has no prepared member dependency",
        ));
    };
    let dependencies = dependency.dependencies;
    let presentation_layout = dependency.layout;
    let payload_presentation = presentation_layout
        .transparent_payload(name)
        .ok_or_else(|| {
            internal_type_error("type catalog newtype has no payload presentation layout")
        })?;
    opaque_types.structural_stack.push(carrier.clone());
    let scope = TsFacadeScope::default();
    let context = TsFacadeRenderContext {
        plan: payload.facade(),
        execution: payload_presentation,
        execution_layout: presentation_layout,
        nominals: dependencies,
        names,
        scope: &scope,
    };
    let rendered =
        render_newtype_payload(context, payload, args, payload_presentation, opaque_types);
    let popped = opaque_types.structural_stack.pop();
    debug_assert_eq!(popped.as_deref(), Some(carrier.as_str()));
    Ok(format!(
        "{{ {}: {} }}",
        ts_property_key(&host_name_core(name.name())),
        rendered?
    ))
}

fn internal_type_error(message: &str) -> EmitError {
    EmitError {
        message: format!("internal TypeScript emitter error: {message}"),
    }
}

fn type_lambda_argument_constraint(kind: &Kind, names: &TsNames) -> String {
    match kind {
        Kind::Star => "unknown".to_owned(),
        Kind::Arrow(_, _) => {
            let args = kind_argument_domains(kind)
                .into_iter()
                .map(|domain| type_lambda_argument_constraint(domain, names))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}<readonly [{args}]>", names.type_lambda)
        }
    }
}

fn kind_argument_domains(kind: &Kind) -> Vec<&Kind> {
    let mut domains = Vec::new();
    let mut remaining = kind;
    while let Kind::Arrow(domain, codomain) = remaining {
        domains.push(domain.as_ref());
        remaining = codomain;
    }
    domains
}

// =========================================================================
/// The TS type for a `role(...)` atomic, per `specs/backends/ts.md`
/// § FFI surface. Narrow integers and floats are JS `number`; wide
/// integers (`i64` / `i128` / `u64` / `u128`) are JS `bigint`; `str` is
/// `string`; `bool` is `boolean`. This is the TS view of the JS value
/// shape in `specs/backends/js.md` § Atomic types.
fn role_to_ts_type(role: Role) -> &'static str {
    match role {
        Role::Str => "string",
        Role::Bool => "boolean",
        Role::I8
        | Role::I16
        | Role::I32
        | Role::U8
        | Role::U16
        | Role::U32
        | Role::F32
        | Role::F64 => "number",
        Role::I64 | Role::I128 | Role::U64 | Role::U128 => "bigint",
    }
}

// =========================================================================
// Surface tree — nested package namespaces
// =========================================================================

/// A leaf of the package surface: either an exported `pub fn` (a typed
/// arrow) or a `pub newtype` namespace object (a map of typed members).
enum SurfaceLeaf {
    /// An exported `pub fn`'s arrow type, e.g. `(p0: string) => null`.
    Fn(String),
    /// A `pub newtype`'s namespace — member name → its arrow type.
    Namespace(BTreeMap<String, String>),
}

/// Tree of the package's exported surface: each path segment (the
/// module path, then the item leaf) nests a level. Leaves are typed
/// members; interior nodes are nested object types. Mirrors the JS
/// emitter's `ReturnTree`, byte-stable via `BTreeMap` ordering.
#[derive(Default)]
struct SurfaceTree {
    entries: BTreeMap<String, SurfaceEntry>,
}

enum SurfaceEntry {
    Leaf(SurfaceLeaf),
    Subtree(SurfaceTree),
}

impl SurfaceTree {
    /// Insert `leaf` at a role-framed facade `path`.
    fn insert(&mut self, path: &[String], leaf: SurfaceLeaf) {
        debug_assert!(!path.is_empty());
        if path.len() == 1 {
            assert!(
                self.entries
                    .insert(path[0].clone(), SurfaceEntry::Leaf(leaf))
                    .is_none(),
                "resolved facade path is unique"
            );
            return;
        }
        let entry = self
            .entries
            .entry(path[0].clone())
            .or_insert_with(|| SurfaceEntry::Subtree(SurfaceTree::default()));
        let SurfaceEntry::Subtree(sub) = entry else {
            unreachable!("role-framed facade path cannot cross a value leaf")
        };
        sub.insert(&path[1..], leaf);
    }

    /// Render the tree as a TS object type literal at `depth`
    /// indentation. Leaves render as members; subtrees nest.
    fn render(&self, out: &mut String, depth: usize) {
        if self.entries.is_empty() {
            out.push_str("{}");
            return;
        }
        let indent = "  ".repeat(depth + 1);
        let close_indent = "  ".repeat(depth);
        out.push_str("{\n");
        for (key, entry) in &self.entries {
            out.push_str(&indent);
            out.push_str(&ts_property_key(key));
            out.push_str(": ");
            match entry {
                SurfaceEntry::Leaf(SurfaceLeaf::Fn(arrow)) => {
                    out.push_str(arrow);
                    out.push_str(";\n");
                }
                SurfaceEntry::Leaf(SurfaceLeaf::Namespace(members)) => {
                    out.push_str("{\n");
                    let member_indent = "  ".repeat(depth + 2);
                    for (name, arrow) in members {
                        out.push_str(&member_indent);
                        out.push_str(&ts_property_key(name));
                        out.push_str(": ");
                        out.push_str(arrow);
                        out.push_str(";\n");
                    }
                    out.push_str(&indent);
                    out.push_str("};\n");
                }
                SurfaceEntry::Subtree(sub) => {
                    sub.render(out, depth + 1);
                    out.push_str(";\n");
                }
            }
        }
        out.push_str(&close_indent);
        out.push('}');
    }
}

// =========================================================================
// TS identifier / key rendering
// =========================================================================

/// Render an object-property key for a TS type literal: a bare
/// identifier when legal, else a quoted string-literal key. The
/// step-2 qualified FFI spelling (`<modulepath>.<F>`) carries a `.`,
/// which TS accepts only as a quoted key — the same rule the JS
/// emitter applies to object keys (`specs/backends/js.md` § FFI keys for
/// newtype slots).
fn ts_property_key(key: &str) -> String {
    if is_legal_ts_identifier(key) {
        key.to_owned()
    } else {
        format!("\"{}\"", key.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn ts_string_literal(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}

/// Render a member name in a property position. An item whose Kio name is not
/// a legal bare TypeScript identifier is quoted.
fn ts_member_name(name: &str) -> String {
    ts_property_key(&host_name_core(name))
}

fn ts_host_module_path(source: &str) -> String {
    source
        .split('/')
        .map(host_name_core)
        .collect::<Vec<_>>()
        .join("/")
}

/// `true` when `s` is a legal bare TypeScript identifier (the ASCII
/// subset Kio item names occupy). TS additionally accepts reserved
/// words in property / member positions, so no reserved-word mangling
/// is needed — the same property-position rule as the JS backend
/// (`specs/backends/js.md` § Item naming).
fn is_legal_ts_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::pass::parser::{parse, parse_package_file};
    use crate::pass::resolve::PackageFileEntry;
    use crate::pass::structural_recovery;
    use std::path::{Path, PathBuf};

    fn module_file_path(module: &crate::ast::Module<crate::ast::Surface>) -> PathBuf {
        let segs = &module.path.segments;
        let mut path = PathBuf::new();
        for seg in &segs[..segs.len().saturating_sub(1)] {
            path.push(&seg.name);
        }
        let stem = segs.last().map(|s| s.name.as_str()).unwrap_or("module");
        path.push(format!("{stem}.kio"));
        path
    }

    /// Compile module sources + a package file through the full
    /// pipeline to `Package<Routed>` — the phase
    /// [`lower_package_to_dts`] consumes. The TS renderer needs the
    /// typed form (host types, exports), so this runs the real typer
    /// rather than the lowering-only bypass the JS unit tests use.
    fn build_package(srcs: &[&str], package_file_src: &str) -> Package<Routed> {
        use crate::pass::full::FullPipeline;
        use crate::pass::typecheck_full::check_package;
        use crate::pipeline::Pipeline;

        let parsed_modules: Vec<(PathBuf, _)> = srcs
            .iter()
            .map(|src| {
                let parsed = parse(src).expect("parse");
                (module_file_path(&parsed), parsed)
            })
            .collect();
        let parsed_package_file =
            parse_package_file(&format!("package pkg;\n{package_file_src}"), None)
                .expect("parse package file");
        let (lowered_modules, lowered_package_file) =
            FullPipeline::lower_package(parsed_modules, Some(parsed_package_file))
                .expect("lower_package");
        let package_file_entry = lowered_package_file.map(|e| PackageFileEntry {
            file_path: PathBuf::from("x.pkg.kio"),
            package_name: "x".to_owned(),
            package_file: e,
        });
        let pkg = crate::pass::resolve::Package::build(
            Path::new(""),
            lowered_modules,
            package_file_entry,
        )
        .expect("build");
        pkg.resolve_imports().expect("resolve_imports");
        pkg.check_in_body_resolution().expect("body resolution");
        let prime = check_package(&pkg).expect("typecheck");
        let enriched = structural_recovery::recover_package(&prime);
        crate::pass::recover_to_low::lower(&enriched)
    }

    #[test]
    fn recursive_structural_worklist_eliminates_ordered_rescans() {
        const DECLARATIONS: usize = 128;
        let mut types = OpaqueTsTypes::default();
        let names = (0..DECLARATIONS)
            .map(|index| {
                QualifiedTypeName::new(
                    vec!["recursive".to_owned()],
                    format!("Declaration{index:03}"),
                )
                .expect("test identity is valid")
            })
            .collect::<Vec<_>>();

        let former_selection = || {
            let mut discovered = names.iter().step_by(2).cloned().collect::<BTreeSet<_>>();
            let mut emitted = BTreeSet::new();
            let mut selected = Vec::new();
            let mut candidate_visits = 0;
            loop {
                let next = discovered.iter().find_map(|name| {
                    candidate_visits += 1;
                    (!emitted.contains(name)).then(|| name.clone())
                });
                let Some(name) = next else {
                    break;
                };
                emitted.insert(name.clone());
                selected.push(name);
                if selected.len() == 1 {
                    // This is the former rescan path's equivalent of one
                    // rendered interface discovering the complete closure.
                    discovered.extend(names.iter().cloned());
                }
            }
            (selected, candidate_visits)
        };

        let (former_selected, former_candidate_visits) = former_selection();
        for name in names.iter().step_by(2) {
            types.insert_recursive_structural(name);
        }

        let mut selected = Vec::new();
        while let Some(name) = types.pop_next_recursive_structural() {
            selected.push(name);
            if selected.len() == 1 {
                // Rendering one interface may discover more recursive
                // declarations, including identities already seen.
                for name in &names {
                    types.insert_recursive_structural(name);
                }
            }
        }

        assert_eq!(selected, names);
        assert_eq!(former_selected, selected);
        assert_eq!(former_candidate_visits, 8_384);
        assert_eq!(
            types.recursive_structural_selection_steps, DECLARATIONS,
            "each recursive declaration must take exactly one worklist extraction",
        );
    }

    fn retained_carrier_signature() -> (u32, crate::sig::ReplayedInterface) {
        let source = r#"signature pkg v(3);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Box[A];
        host type Unused[A];
        newtype Token[A] : A { pub constructor make_token; projector read_token; };
        host fn old(value: Box(Token(.))) -> Box(Token(.));
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Box;
        Unused;
        Token;
        old;
      }
    }
  }
}
"#;
        retained_signature(source, "retained TypeScript fixture")
    }

    fn retained_transparent_payload_signature() -> (u32, crate::sig::ReplayedInterface) {
        let source = r#"signature pkg v(3);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Str role(str);
        newtype Token[A] : A { pub constructor make_token; pub projector read_token; };
        host fn old(value: Token(Str)) -> Token(Str);
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Token;
        old;
      }
    }
  }
}
"#;
        retained_signature(source, "retained transparent TypeScript fixture")
    }

    fn retained_unsaturated_transparent_signature() -> (u32, crate::sig::ReplayedInterface) {
        let source = r#"signature pkg v(3);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Str role(str);
        newtype Wrap[A] : A { pub constructor make_wrap; pub projector read_wrap; };
        newtype Lift[*F] : F(Str) { pub constructor make_lift; pub projector read_lift; };
        host fn old(value: Lift(Wrap)) -> Lift(Wrap);
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Wrap;
        Lift;
        old;
      }
    }
  }
}
"#;
        retained_signature(source, "retained unsaturated TypeScript fixture")
    }

    fn reintroduced_host_signature() -> (u32, crate::sig::ReplayedInterface) {
        let source = r#"signature pkg v(3);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Str role(str);
        host fn old(value: Str) -> Str;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#;
        retained_signature(source, "reintroduced TypeScript fixture")
    }

    fn retained_signature(source: &str, fixture: &str) -> (u32, crate::sig::ReplayedInterface) {
        let file = crate::pass::parser::parse_signature_file(source, None)
            .unwrap_or_else(|error| panic!("parse {fixture}: {error:?}"));
        crate::sig::replay(&file).unwrap_or_else(|error| panic!("replay {fixture}: {error:?}"));
        let version = file.version.saturating_sub(1);
        let replayed = crate::sig::replay_through(&file, version)
            .unwrap_or_else(|error| panic!("replay sealed {fixture}: {error:?}"));
        (version, replayed)
    }

    #[test]
    fn retained_signature_emits_optional_deprecated_exact_surface_only() {
        let pkg = build_package(
            &["module api; pub fn unit() -> . { () }"],
            "bridge { api; }",
        );
        let live = lower_package_to_dts(&pkg, "facade").expect("render live skin");
        let sig = retained_carrier_signature();
        let retained = lower_package_to_dts_with_signature(&pkg, "facade", Some(&sig))
            .expect("render retained skin");

        let token = opaque_ts_type_name("api", "Token");
        assert!(!live.contains(&token), "{live}");
        assert!(
            retained.contains(&format!("declare class {token}<")),
            "{retained}"
        );
        assert!(
            retained.contains("/** @deprecated Host type `api.Box` was removed at v(2). */")
                && retained.contains("readonly api?: {")
                && retained.contains("readonly Box?: FacadeTypeLambda<"),
            "{retained}"
        );
        assert!(
            retained.contains(
                "/** @deprecated Host module `api` is retained for removed host types. */\n  readonly api?: {"
            ),
            "{retained}"
        );
        assert!(
            retained.contains("/** @deprecated Host fn `api.old` was removed at v(2). */")
                && retained.contains("readonly old?:"),
            "{retained}"
        );
        assert!(
            retained.contains(
                "/** @deprecated Host module `api` is retained for removed host functions. */\n  readonly api?: {"
            ),
            "{retained}"
        );
        assert!(
            retained.contains(
                "/** @deprecated Internal compatibility helper for removed host declarations. */\ntype __FacadeRetainedHostBinding<"
            ),
            "{retained}"
        );
        assert!(
            retained.contains(&format!(
                "/** @deprecated Retained only for host declarations removed at v(2). */\ndeclare class {token}<"
            )),
            "{retained}"
        );
        assert_eq!(
            retained
                .matches("/** @deprecated Host type carrier `api.Token` was removed at v(2). */")
                .count(),
            2,
            "{retained}"
        );
        assert!(!retained.contains("Unused"), "{retained}");
        assert!(!retained.contains("KioType_Token"), "{retained}");
    }

    #[test]
    fn retained_signature_uses_frozen_transparent_payload_presentation() {
        let pkg = build_package(
            &["module api; host type Str role(str); pub fn unit() -> . { () }"],
            "bridge { api; }",
        );
        let sig = retained_transparent_payload_signature();
        let dts = lower_package_to_dts_with_signature(&pkg, "facade", Some(&sig))
            .expect("render retained transparent skin");

        assert!(
            dts.contains("readonly old?: (p0: { Token: string }) => { Token: string };"),
            "{dts}"
        );
        assert!(
            !dts.contains(&format!(
                "declare class {}<",
                opaque_ts_type_name("api", "Token")
            )),
            "{dts}"
        );
    }

    #[test]
    fn retained_unsaturated_transparent_dependency_declares_deprecated_witness() {
        let pkg = build_package(
            &["module api; host type Str role(str); pub fn unit() -> . { () }"],
            "bridge { api; }",
        );
        let sig = retained_unsaturated_transparent_signature();
        let dts = lower_package_to_dts_with_signature(&pkg, "facade", Some(&sig))
            .expect("render retained unsaturated transparent skin");
        let wrap_witness = format!(
            "__FacadeNewtypeK_H{}",
            js::encoded_newtype_identity("api", "Wrap")
        );

        assert!(
            dts.contains(&format!(
                "FacadeApply<{wrap_witness}<B>, readonly [string]>"
            )),
            "{dts}"
        );
        assert!(
            dts.contains(&format!(
                "/** @deprecated Host type carrier `api.Wrap` was removed at v(2). */\ninterface {wrap_witness}<"
            )),
            "{dts}"
        );
    }

    #[test]
    fn live_callable_provenance_dominates_same_identity_history() {
        let pkg = build_package(
            &["module api; host type Str role(str); host fn old(value: Str) -> Str;"],
            "bridge { api; }",
        );
        let sig = reintroduced_host_signature();
        let dts = lower_package_to_dts_with_signature(&pkg, "facade", Some(&sig))
            .expect("render live-dominant skin");

        assert!(
            dts.contains("readonly old: (p0: string) => string;"),
            "{dts}"
        );
        assert!(dts.contains("readonly api: {"), "{dts}");
        assert!(!dts.contains("readonly old?:"), "{dts}");
        assert!(!dts.contains("Host fn `api.old` was removed"), "{dts}");
    }

    #[test]
    fn role_to_ts_type_table() {
        assert_eq!(role_to_ts_type(Role::Str), "string");
        assert_eq!(role_to_ts_type(Role::Bool), "boolean");
        assert_eq!(role_to_ts_type(Role::I32), "number");
        assert_eq!(role_to_ts_type(Role::U32), "number");
        assert_eq!(role_to_ts_type(Role::F64), "number");
        // Wide integers are JS `bigint`, not `number`.
        assert_eq!(role_to_ts_type(Role::I64), "bigint");
        assert_eq!(role_to_ts_type(Role::I128), "bigint");
        assert_eq!(role_to_ts_type(Role::U64), "bigint");
        assert_eq!(role_to_ts_type(Role::U128), "bigint");
    }

    #[test]
    fn ts_property_key_quotes_dotted_and_passes_bare() {
        assert_eq!(ts_property_key("Foo"), "Foo");
        assert_eq!(ts_property_key("_0"), "_0");
        // The step-2 qualified FFI key carries a `.` — quoted.
        assert_eq!(ts_property_key("m.Foo"), "\"m.Foo\"");
    }

    #[test]
    fn natural_exact_protocol_is_structural_fail_closed_and_kind_exact() {
        let brand = js::Brand::derive("greeter");
        let names = TsNames::from_brand(&brand);
        let mut protocol = String::new();
        render_type_lambda_protocol(&brand, &names, &mut protocol);

        let base = protocol
            .split("export interface GreeterTypeLambda")
            .nth(1)
            .and_then(|tail| tail.split("interface __GreeterTypeLambdaLike").next())
            .expect("base protocol");
        assert!(base.contains("readonly arguments: Args;"), "{protocol}");
        assert!(
            base.contains("readonly __kio_type_lambda_variance?: (args: Args) => Args;")
                && !base.contains("readonly type:"),
            "{protocol}"
        );
        assert!(
            protocol.contains("readonly __kio_type_lambda_partial: readonly [Root, Prefix];")
                && protocol.contains("__GreeterRawApply<Root, readonly [...Prefix, ...Args]>")
                && protocol.contains("type-lambda application has the wrong argument arity")
                && protocol.contains("type lambda has no concrete result")
                && protocol.contains("type-lambda prefix must leave at least one argument"),
            "{protocol}"
        );
        assert!(
            protocol
                .contains("__GreeterHasSameFlattenedArity<Root, readonly [...Prefix, ...Args]>")
                && protocol.contains("type __GreeterFlattenedArgumentsOf<F>")
                && protocol.contains("type __GreeterFlattenedArgumentsAccept<")
                && protocol.contains(
                    "type __GreeterArgumentSatisfies<Expected, Actual> = unknown extends Expected\n  ? true\n  : [Actual] extends [Expected]"
                )
                && protocol
                    .contains("type-lambda arguments do not satisfy the constructor constraints")
                && protocol.contains("__GreeterWrongArityTypeLambdaResult"),
            "{protocol}"
        );

        let higher_order_domain = Kind::Arrow(
            Box::new(Kind::Arrow(Box::new(Kind::Star), Box::new(Kind::Star))),
            Box::new(Kind::Star),
        );
        assert_eq!(
            type_lambda_argument_constraint(&higher_order_domain, &names),
            "GreeterTypeLambda<readonly [GreeterTypeLambda<readonly [unknown]>]>"
        );
    }

    #[test]
    fn returned_forall_values_reach_the_type_surface() {
        let pkg = build_package(
            &["module stages; host fn produce(_unit: .) -> [A] A;"],
            "bridge { stages; }",
        );
        let dts = lower_package_to_dts(&pkg, "facade").expect("render returned forall");

        assert!(dts.contains("readonly produce: () => <A>() => A;"), "{dts}");
    }

    #[test]
    fn host_type_keys_are_readable_exact_and_additive() {
        let base_sources = [
            "module plain; \
             host type Token; host type Box[A]; \
             host fn keep(value: Token) -> Token;",
            "module nested/with_key; \
             host type Token; \
             host fn keep_nested(value: Token) -> Token;",
        ];
        let base = build_package(&base_sources, "bridge { plain; nested/with_key; }");
        let base_dts = lower_package_to_dts(&base, "greeter").expect("render");
        let host_types = base_dts
            .split("export interface GreeterHostTypes {")
            .nth(1)
            .and_then(|tail| tail.split("type __GreeterRejectBinding").next())
            .expect("HostTypes body");

        assert!(
            host_types.contains("readonly plain: {")
                && host_types.contains("readonly Token: unknown;")
                && host_types.contains("readonly Box: GreeterTypeLambda<readonly [unknown]>;")
                && host_types.contains("readonly \"nested/withKey\": {")
                && !host_types.contains("KioModule_"),
            "{base_dts}"
        );
        // Type-only selection cases each canonical path component while the
        // runtime Host retains the collision-proof JS ABI key.
        assert!(
            base_dts.contains("readonly plain: {")
                && base_dts.contains("readonly KioModule_nested_swithKey: {")
                && base_dts.contains("B[\"plain\"][\"Token\"]")
                && base_dts.contains("B[\"nested/withKey\"][\"Token\"]"),
            "{base_dts}"
        );

        let added = build_package(
            &[
                "module plain; \
                 host type Token; host type Box[A]; host type Added; \
                 host fn keep(value: Token) -> Token;",
                base_sources[1],
            ],
            "bridge { plain; nested/with_key; }",
        );
        let added_dts = lower_package_to_dts(&added, "greeter").expect("render");
        let existing_line = |dts: &str| {
            dts.lines()
                .find(|line| line.contains("readonly keep:"))
                .expect("existing host callable")
                .to_owned()
        };
        assert_eq!(existing_line(&base_dts), existing_line(&added_dts));
    }

    #[test]
    fn prepared_sites_drive_exact_host_bindings_and_direct_generics() {
        let pkg = build_package(
            &["module api; \
               host type Token; \
               host type Box[A]; \
               host fn round_token(value: Token) -> Token; \
               host fn box[A](value: A) -> Box(A); \
               pub fn keep[A](value: A) -> A { value }"],
            "bridge { api; }",
        );
        let dts = lower_package_to_dts(&pkg, "facade").expect("render");

        assert!(
            dts.contains("export interface FacadeHostTypes")
                && dts.contains("readonly Token: unknown;")
                && dts.contains("readonly Box: FacadeTypeLambda<readonly [unknown]>;"),
            "{dts}"
        );
        assert!(
            dts.contains("export type FacadeHost<B extends FacadeHostTypes")
                && dts.contains(
                    "readonly roundToken: (p0: B[\"api\"][\"Token\"]) => B[\"api\"][\"Token\"];"
                )
                && dts.contains(
                    "readonly box: <A>(p0: A) => FacadeApply<B[\"api\"][\"Box\"], readonly [A]>;"
                ),
            "{dts}"
        );
        assert!(
            dts.contains("keep: <A>(p0: A) => A;")
                && dts.contains("export function createFacade<B extends FacadeHostTypes"),
            "{dts}"
        );
    }

    #[test]
    fn host_type_maps_roles_and_nests_by_runtime_module() {
        // A `host fn` reaches the host nested under its module's JS
        // namespace; each role maps to its TS primitive.
        let pkg = build_package(
            &["module main; \
               host type Str role(str); host type I64 role(i64); \
               host fn emit(p0: Str, p1: I64) -> .; \
               fn dummy() -> . { () }"],
            "bridge { main; }",
        );
        let dts = lower_package_to_dts(&pkg, "greeter").expect("render");
        assert!(
            dts.contains("interface __GreeterHost<B extends GreeterHostTypes> {"),
            "{dts}"
        );
        assert!(dts.contains("readonly main: {"), "{dts}");
        // str → string, i64 → bigint, () → null; the host callable is a
        // readonly property so strict function-parameter variance applies.
        assert!(
            dts.contains("readonly emit: (p0: string, p1: bigint) => null;"),
            "{dts}"
        );
        // Host functions require the validated generic host argument.
        assert!(
            dts.contains("? [host: GreeterHost<B>]") && dts.contains("): Greeter<B>;"),
            "{dts}"
        );
    }

    #[test]
    fn same_leaf_host_types_keep_exact_roles() {
        let pkg = build_package(
            &[
                "module left; \
                 host type Shared role(i32); \
                 host fn keep_left(value: Shared) -> Shared;",
                "module right; \
                 host type Shared role(str); \
                 host fn keep_right(value: Shared) -> Shared;",
            ],
            "bridge { left; right; }",
        );
        let ty = |segments: &[&str]| {
            Type::synth_path(
                segments
                    .iter()
                    .map(|segment| (*segment).to_owned())
                    .collect(),
                Vec::new(),
                crate::span::Span::new(0, 0),
            )
        };

        assert_eq!(
            exact_host_type_role(&pkg, &ty(&["left", "Shared"])),
            Some(Role::I32)
        );
        assert_eq!(
            exact_host_type_role(&pkg, &ty(&["right", "Shared"])),
            Some(Role::Str)
        );
        assert_eq!(exact_host_type_role(&pkg, &ty(&["Shared"])), None);

        let dts = lower_package_to_dts(&pkg, "facade").expect("render");
        assert!(
            dts.contains("readonly keepLeft: (p0: number) => number;"),
            "{dts}"
        );
        assert!(
            dts.contains("readonly keepRight: (p0: string) => string;"),
            "{dts}"
        );
    }

    #[test]
    fn comptime_prefixed_user_types_keep_exact_boundary_identity() {
        let pkg = build_package(
            &["module main; \
               host type I32 role(i32); \
               host type Comptime_host role(i32); \
               pub type Comptime_alias = I32; \
               pub newtype Comptime_bool : I32 { \
                 pub constructor make_bool; pub projector read_bool; \
               }; \
               pub newtype Wrapped : Comptime_host { \
                 pub constructor make_wrapped; pub projector read_wrapped; \
               }; \
               pub fn round_nominal(value: Comptime_bool) -> Comptime_bool { value } \
               pub fn round_host(value: Comptime_host) -> Comptime_host { value } \
               pub fn round_wrapped(value: Wrapped) -> Wrapped { value } \
               pub fn round_alias(value: Comptime_alias) -> Comptime_alias { value } \
               pub fn keep[Comptime_str](value: Comptime_str) -> Comptime_str { value }"],
            "bridge { main; }",
        );
        let dts = lower_package_to_dts(&pkg, "facade").expect("render");

        assert!(
            dts.contains(
                "roundNominal: (p0: { ComptimeBool: number }) => { ComptimeBool: number };"
            ),
            "{dts}"
        );
        assert!(
            dts.contains("roundHost: (p0: number) => number;")
                && dts.contains("roundWrapped: (p0: { Wrapped: number }) => { Wrapped: number };")
                && dts.contains("roundAlias: (p0: number) => number;")
                && dts.contains("keep: <ComptimeStr>(p0: ComptimeStr) => ComptimeStr;"),
            "{dts}"
        );
    }

    #[test]
    fn returned_function_preserves_product_source_parameter() {
        let pkg = build_package(
            &["module main; \
               host type I32 role(i32); \
               pub fn make() -> (& I32 & I32) -> I32 { \
                 .(, left: I32, right: I32) { left } \
               }"],
            "bridge { main; }",
        );
        let dts = lower_package_to_dts(&pkg, "facade").expect("render");

        assert!(
            dts.contains("make: () => (p0: { _0: number; _1: number }) => number;"),
            "{dts}"
        );
        assert!(
            !dts.contains("make: () => (p0: number, p1: number) => number;"),
            "{dts}"
        );
    }

    #[test]
    fn visible_newtype_payloads_resolve_nested_nominals_in_their_module() {
        let pkg = build_package(
            &[
                "module aaa; \
                 pub newtype Nested : . { constructor make_nested; projector read_nested; };",
                "module zed; \
                 host type I32 role(i32); \
                 pub newtype Nested : I32 { \
                   pub constructor make_nested; pub projector read_nested; \
                 }; \
                 pub newtype Outer : Nested { \
                   pub constructor make_outer; pub projector read_outer; \
                 }; \
                 pub fn round(value: Outer) -> Outer { value }",
            ],
            "bridge { zed; }",
        );
        let js = js::lower_package_to_factory_module(&pkg, "facade").expect("render JS");
        let dts = lower_package_to_dts(&pkg, "facade").expect("render TypeScript");
        let unrelated = opaque_ts_type_name("aaa", "Nested");

        assert!(
            js.contains("return { Outer: { Nested: __body__ } };"),
            "{js}"
        );
        assert!(
            dts.contains(
                "round: (p0: { Outer: { Nested: number } }) => { Outer: { Nested: number } };"
            ) && !dts.contains(&unrelated),
            "{dts}"
        );
    }

    #[test]
    fn exact_type_variables_observe_ordered_and_nested_binder_scope() {
        let pkg = build_package(
            &["module main; \
               host type Str role(str); \
               pub newtype T : Str { pub constructor make_t; pub projector un_t; }; \
               pub newtype Box[A] : A { pub constructor make_box; pub projector un_box; }; \
               pub newtype Shadow[T] : T & ([T] T -> T) { pub constructor make_shadow; pub projector un_shadow; }; \
               pub newtype Pack <T> : T { pub constructor make_pack; pub projector un_pack; }; \
               host fn apply_poly(f: [T] T -> T) -> Str; \
               host fn round_shadow(value: Shadow(Box(Str))) -> Shadow(Box(Str)); \
               host fn round_pack(value: Pack) -> Pack; \
               pub fn ordered(value: T)[T](later: T) -> T { later }"],
            "bridge { main; }",
        );
        let dts = lower_package_to_dts(&pkg, "greeter").expect("render");

        assert!(
            dts.contains("readonly applyPoly: (p0: <T>(p0: T) => T) => string;"),
            "{dts}"
        );
        // The first value group sees the module newtype T; the later binder
        // shadows only its own source occurrences. The shared presentation layout
        // combines the declaration-head groups into one host call.
        assert!(
            dts.contains("ordered: <T>(p0: { T: string }, p1: T) => T;"),
            "{dts}"
        );
        // A declaration binder wins before slot-level nominal resolution,
        // while the nested forall receives its own collision-free TS binder.
        assert!(
            dts.contains(
                "makeShadow: <T>(p0: { _0: T; _1: <TN2>(p0: TN2) => TN2 }) => { Shadow: { _0: T; _1: <TN2>(p0: TN2) => TN2 } };"
            ),
            "{dts}"
        );
        assert!(
            dts.contains(
                "readonly roundShadow: (p0: { Shadow: { _0: { Box: string }; _1: <TN2>(p0: TN2) => TN2 } }) => { Shadow: { _0: { Box: string }; _1: <TN2>(p0: TN2) => TN2 } };"
            ),
            "{dts}"
        );
        assert!(
            dts.contains("readonly roundPack: (p0: __KioOpaque_H6d61696e005061636b<B>) => __KioOpaque_H6d61696e005061636b<B>;"),
            "{dts}"
        );
    }

    #[test]
    fn hostless_package_has_zero_argument_factory_branch() {
        // No package-declared host fns or roleless host types means an empty
        // Host/HostTypes pair and a zero-argument factory branch.
        let pkg = build_package(
            &["module main; pub fn run() -> . { () }"],
            "bridge { main; }",
        );
        let dts = lower_package_to_dts(&pkg, "greeter").expect("render");
        assert!(
            dts.contains("export interface GreeterHostTypes {}"),
            "{dts}"
        );
        assert!(
            dts.contains("interface __GreeterHost<B extends GreeterHostTypes> {}"),
            "{dts}"
        );
        assert!(
            dts.contains(
                "export function createGreeter<B extends GreeterHostTypes = GreeterHostTypes>("
            ) && dts.contains("? []")
                && dts.contains("): Greeter<B>;"),
            "{dts}"
        );
    }

    #[test]
    fn structural_product_and_sum_export_shapes() {
        // A product-typed source parameter remains one structural JS object;
        // the returned sum keeps its discriminated JS shape.
        let pkg = build_package(
            &["module main; \
               import __intrinsics__; \
               host type I32 role(i32); host type Str role(str); \
               pub fn split(p: (I32 & Str)) -> (I32 | Str) { \
                 __left__(I32, Str, __fst__(I32, Str, p)) }"],
            "bridge { main; }",
        );
        let dts = lower_package_to_dts(&pkg, "greeter").expect("render");
        assert!(
            dts.contains(
                "split: (p0: { _0: number; _1: string }) => { _0: number } | { _1: string };"
            ),
            "{dts}"
        );
    }

    #[test]
    fn newtype_namespace_members_named_and_typed() {
        // A pub newtype exposes only its public constructor / projector.
        // The members consume and produce the same structural newtype shape
        // as an exported fn. An opaque newtype still has an empty namespace.
        let pkg = build_package(
            &["module main; \
               host type I32 role(i32); \
               pub newtype Opaque : I32 { constructor hide; projector reveal; }; \
               pub fn make_opaque(value: I32) -> Opaque { Opaque.hide(value) } \
               pub fn read_opaque(value: Opaque) -> I32 { Opaque.reveal(value) } \
               pub newtype Constructor_only : I32 { pub constructor make_only; projector read_only; }; \
               pub newtype Projector_only : I32 { constructor make_hidden; pub projector read_public; }; \
               pub newtype Counter : I32 { pub constructor fresh; pub projector current; };"],
            "bridge { main; }",
        );
        let dts = lower_package_to_dts(&pkg, "greeter").expect("render");
        assert!(dts.contains("Opaque: {\n    };"), "{dts}");
        assert!(
            dts.contains("declare class __KioOpaque_H6d61696e004f7061717565"),
            "{dts}"
        );
        assert!(
            dts.contains("makeOpaque: (p0: number) => __KioOpaque_H6d61696e004f7061717565<B>;"),
            "{dts}"
        );
        assert!(
            dts.contains("readOpaque: (p0: __KioOpaque_H6d61696e004f7061717565<B>) => number;"),
            "{dts}"
        );
        assert!(dts.contains("Counter: {"), "{dts}");
        assert!(
            dts.contains("fresh: (p0: number) => { Counter: number };"),
            "{dts}"
        );
        assert!(
            dts.contains("current: (p0: { Counter: number }) => number;"),
            "{dts}"
        );
        let constructor_only = "__KioOpaque_H6d61696e00436f6e7374727563746f725f6f6e6c79";
        let projector_only = "__KioOpaque_H6d61696e0050726f6a6563746f725f6f6e6c79";
        assert!(
            dts.contains(&format!("makeOnly: (p0: number) => {constructor_only}<B>;")),
            "{dts}"
        );
        assert!(!dts.contains("readOnly:"), "{dts}");
        assert!(
            dts.contains(&format!("readPublic: (p0: {projector_only}<B>) => number;")),
            "{dts}"
        );
        assert!(!dts.contains("makeHidden:"), "{dts}");
        assert!(
            dts.contains(&format!("declare class {constructor_only}")),
            "{dts}"
        );
        assert!(
            dts.contains(&format!("declare class {projector_only}")),
            "{dts}"
        );
    }

    #[test]
    fn transparent_public_wrapper_keys_case_words_in_every_declaration_surface() {
        let pkg = build_package(
            &["module main; \
               pub newtype _Word_box__[Item_type] : Item_type { \
                 pub constructor wrap_word; pub projector unwrap_word; \
               }; \
               pub newtype Hidden[Item_type] : . { constructor hide; projector reveal; }; \
               pub rec newtype _Root_word_ : Hidden(_Root_word_) { \
                 pub constructor make_root; pub projector read_root; \
               }; \
               pub fn keep_word(value: _Root_word_) -> _Root_word_ { value }"],
            "bridge { main; }",
        );
        let dts = lower_package_to_dts(&pkg, "facade").expect("render");
        assert!(
            dts.contains("wrapWord: <ItemType>(p0: ItemType) => { _WordBox__: ItemType };"),
            "{dts}"
        );
        assert!(dts.contains("readonly _WordBox__:"), "{dts}");
        assert!(dts.contains("keepWord: (p0: { _RootWord_:"), "{dts}");
        assert!(dts.contains("\n  _RootWord_:"), "{dts}");
        assert!(!dts.contains("_Word_box__:"), "{dts}");
        assert!(!dts.contains("_Root_word_:"), "{dts}");
    }

    #[test]
    fn newtype_namespace_types_follow_parametric_existential_nested_and_recursive_schemes() {
        let pkg = build_package(
            &["module shapes; \
               host type I32 role(i32); \
               pub newtype Nested : I32 & (I32 | I32) { \
                 pub constructor make_nested; pub projector read_nested; \
               }; \
               pub newtype Box[A] : A { \
                 pub constructor make_box; pub projector read_box; \
               }; \
               pub newtype Packed <A> : A { \
                 pub constructor make_packed; pub projector read_packed; \
               }; \
               pub labels { field: I32 }; \
               pub rec newtype Recursive : . | (I32 & Recursive) { \
                 pub constructor make_recursive; pub projector read_recursive; \
               };"],
            "bridge { shapes; }",
        );
        let dts = lower_package_to_dts(&pkg, "facade").expect("render");

        assert!(
            dts.contains(
                "makeNested: (p0: { _0: number; _1: { _0: number } | { _1: number } }) => { Nested: { _0: number; _1: { _0: number } | { _1: number } } };"
            ),
            "{dts}"
        );
        assert!(dts.contains("makeBox: <A>(p0: A) => { Box: A };"), "{dts}");
        assert!(dts.contains("readBox: <A>(p0: { Box: A }) => A;"), "{dts}");
        let packed = "__KioOpaque_H736861706573005061636b6564<B>";
        assert!(
            dts.contains(&format!("makePacked: <A>(p0: A) => {packed};")),
            "{dts}"
        );
        assert!(
            dts.contains(&format!(
                "readPacked: (p0: {packed}) => <__r__>(p0: <A>(p0: A) => __r__) => __r__;"
            )),
            "{dts}"
        );
        assert!(
            dts.contains("mk: (p0: number) => { Field: number };"),
            "{dts}"
        );
        assert!(
            dts.contains("get: (p0: { Field: number }) => number;"),
            "{dts}"
        );
        let recursive = "__KioOpaque_H73686170657300526563757273697665<B>";
        assert!(
            dts.contains(&format!(
                "makeRecursive: (p0: {{ _0: null }} | {{ _1: {{ _0: number; Recursive: {recursive} }} }}) => {recursive};"
            )),
            "{dts}"
        );
        assert!(
            dts.contains(&format!(
                "readRecursive: (p0: {recursive}) => {{ _0: null }} | {{ _1: {{ _0: number; Recursive: {recursive} }} }};"
            )),
            "{dts}"
        );
    }

    #[test]
    fn opaque_types_are_exact_nominals_and_nested_payloads_stay_hidden() {
        let pkg = build_package(
            &[
                "module left; \
                 newtype Secret : . { constructor make_secret; projector read_secret; }; \
                 pub newtype Token : Secret { constructor make_token; projector read_token; }; \
                 pub fn echo_token(value: Token) -> Token { value } \
                 pub fn echo_pair(value: Token & .) -> Token & . { value }",
                "module right; \
                 pub newtype Token : . { constructor make_token; projector read_token; }; \
                 pub fn echo_token(value: Token) -> Token { value }",
            ],
            "bridge { left; right; }",
        );
        let dts = lower_package_to_dts(&pkg, "facade").expect("render");
        let left = "__KioOpaque_H6c65667400546f6b656e";
        let right = "__KioOpaque_H726967687400546f6b656e";

        assert!(dts.contains(&format!("declare class {left}")), "{dts}");
        assert!(dts.contains(&format!("declare class {right}")), "{dts}");
        assert!(
            dts.contains(&format!("echoToken: (p0: {left}<B>) => {left}<B>;")),
            "{dts}"
        );
        assert!(
            dts.contains(&format!("echoToken: (p0: {right}<B>) => {right}<B>;")),
            "{dts}"
        );
        assert!(
            dts.contains(&format!(
                "echoPair: (p0: {{ Token: {left}<B>; _1: null }}) => {{ Token: {left}<B>; _1: null }};"
            )),
            "{dts}"
        );
        assert!(!dts.contains("Secret"), "{dts}");
        assert_eq!(
            dts.matches("private readonly __kioOpaque: (value: readonly [B]) => readonly [B];")
                .count(),
            2
        );
        assert_eq!(dts.matches("private constructor();").count(), 2);
    }

    #[test]
    fn hidden_generic_argument_keeps_enclosing_newtype_structural() {
        let pkg = build_package(
            &["module main; \
               pub newtype Hidden[A] : . { constructor make_hidden; projector read_hidden; }; \
               pub rec newtype Root : Hidden(Root) { \
                 pub constructor make_root; pub projector read_root; \
               }; \
               pub fn keep(value: Root) -> Root { value }"],
            "bridge { main; }",
        );
        let dts = lower_package_to_dts(&pkg, "facade").expect("render");

        assert!(dts.contains("keep: (p0: { Root:"), "{dts}");
        assert!(!dts.contains("keep: (p0: unknown) => unknown;"), "{dts}");
        let structural = "__KioStructural_H6d61696e00526f6f74";
        let hidden = "__KioOpaque_H6d61696e0048696464656e";
        assert!(
            dts.contains(&format!(
                "interface {structural}<B extends FacadeHostTypes> {{\n  Root: {hidden}<B, {structural}<B>>;\n}}"
            )),
            "{dts}"
        );
    }

    #[test]
    fn package_without_host_items_renders_complete_skin() {
        let pkg = build_package(
            &["module main; pub fn run() -> . { () }"],
            "bridge { main; }",
        );
        let dts = lower_package_to_dts(&pkg, "greeter").expect("render");
        assert!(dts.starts_with("// Generated by kio"), "{dts}");
        assert!(dts.contains("export interface GreeterHostTypes"), "{dts}");
        assert!(
            dts.contains("export type GreeterHost<B extends GreeterHostTypes"),
            "{dts}"
        );
        assert!(
            dts.contains("export type Greeter<B extends GreeterHostTypes"),
            "{dts}"
        );
        assert!(dts.contains("export function createGreeter"), "{dts}");
    }
}

// Host type
// =========================================================================

/// Render the internal `<Handle>Host<B>` object from the descriptor's host
/// fns, nested by each fn's declaring-module runtime namespace (the JS
/// `__host__.<NS>.<fn>` rung-1 nesting). A `host type` carries no runtime
/// property; roleless types resolve through `B`, while a role fixes the
/// TypeScript primitive of every value-position use.
///
/// A prepared surface with no live or retained host fns produces an empty
/// internal host object. Retained members and retained-only module paths are
/// optional and deprecated. The exported conditional alias withholds all
/// members when `B` fails validation.
fn render_host_interface(
    prepared: &PreparedBoundaryCallableSites,
    brand: &js::Brand,
    names: &TsNames,
    opaque_types: &mut OpaqueTsTypes,
    out: &mut String,
) -> Result<(), EmitError> {
    let private = format!("__{}", brand.handle);
    out.push_str(&format!(
        "\ninterface {private}Host<B extends {}> {{",
        names.host_types
    ));

    // Group host fns by their declaring module's JS namespace, in the
    // descriptor's module-path-sorted order for byte-stable output —
    // the same grouping `render_js_host_factory_prelude` uses.
    let mut by_namespace: BTreeMap<String, Vec<PreparedBoundaryCallableSite<'_>>> = BTreeMap::new();
    for site in prepared.sites() {
        if matches!(
            site.site().owner(),
            BoundaryFacadeSiteOwner::HostFunction { .. }
        ) {
            let module_path = site.site().module_segments().join("/");
            by_namespace
                .entry(host_module_key(&module_path))
                .or_default()
                .push(site);
        }
    }

    if by_namespace.is_empty() {
        out.push_str("}\n");
    } else {
        out.push('\n');
        for (ns, fns) in &by_namespace {
            let module_is_retained = fns.iter().all(|site| site.retained().is_some());
            if module_is_retained {
                let module = fns
                    .first()
                    .map(|site| site.site().module_segments().join("/"))
                    .unwrap_or_default();
                render_deprecated_jsdoc(
                    out,
                    "  ",
                    &format!("Host module `{module}` is retained for removed host functions."),
                );
            }
            out.push_str(&format!(
                "  readonly {}{}: {{\n",
                ts_property_key(ns),
                if module_is_retained { "?" } else { "" }
            ));
            for site in fns {
                let BoundaryFacadeSiteOwner::HostFunction { name } = site.site().owner() else {
                    unreachable!("host namespace catalog contains only host functions")
                };
                if let Some(metadata) = site.retained() {
                    render_deprecated_jsdoc(
                        out,
                        "    ",
                        &format!(
                            "Host fn `{}.{name}` was removed at v({}).",
                            site.site().module_segments().join("/"),
                            metadata.removed_at_version()
                        ),
                    );
                }
                out.push_str(&format!(
                    "    readonly {}{}: {};",
                    ts_member_name(name),
                    if site.retained().is_some() { "?" } else { "" },
                    render_prepared_callable(*site, names, opaque_types)?
                ));
                out.push('\n');
            }
            out.push_str("  };\n");
        }
        out.push_str("}\n");
    }
    out.push_str(&format!(
        "\nexport type {}<B extends {} = {}> =\n  {private}Errors<B> extends never ? {private}Host<B> : {private}Errors<B>;\n",
        brand.host_ty, names.host_types, names.host_types
    ));
    Ok(())
}

// =========================================================================
// Package surface
// =========================================================================

/// Render the internal `<Handle><B>` object: the bridged modules' `pub` items,
/// nested by the exporting module's role-framed path, matching the package
/// surface the JS factory returns (`specs/backends/js.md` § Package API).
///
/// A package with no `bridge` block exposes nothing —
/// the internal handle is empty. The exported conditional alias withholds all
/// members when `B` fails validation.
fn render_package_interface(
    prepared: &PreparedBoundaryCallableSites,
    brand: &js::Brand,
    names: &TsNames,
    opaque_types: &mut OpaqueTsTypes,
    out: &mut String,
) -> Result<(), EmitError> {
    let mut tree = SurfaceTree::default();
    let mut newtype_members: BTreeMap<Vec<String>, BTreeMap<String, String>> = BTreeMap::new();

    // Inventory owns even memberless public newtypes; callable sites own every
    // public member scheme. Neither branch scans Routed declarations again.
    for declaration in prepared.public_newtypes() {
        let module_path = declaration.name().module_segments().join("/");
        let mut path = ts_facade_module_path(&module_path);
        path.push(FacadeSelector::Type(declaration.name().name().to_owned()).facade_name(false));
        newtype_members.entry(path).or_default();
    }
    for site in prepared.sites().filter(|site| site.execution().is_some()) {
        let module_path = site.site().module_segments().join("/");
        let mut path = ts_facade_module_path(&module_path);
        match site.site().owner() {
            BoundaryFacadeSiteOwner::ExportedFunction { name } => {
                path.push(host_name_core(name));
                tree.insert(
                    &path,
                    SurfaceLeaf::Fn(render_prepared_callable(site, names, opaque_types)?),
                );
            }
            BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member }
            | BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => {
                path.push(FacadeSelector::Type(newtype.clone()).facade_name(false));
                newtype_members.entry(path).or_default().insert(
                    ts_member_name(member),
                    render_prepared_callable(site, names, opaque_types)?,
                );
            }
            BoundaryFacadeSiteOwner::HostFunction { .. } => {}
        }
    }
    for (path, members) in newtype_members {
        tree.insert(&path, SurfaceLeaf::Namespace(members));
    }

    let private = format!("__{}", brand.handle);
    out.push_str(&format!(
        "\ninterface {private}Handle<B extends {}> ",
        names.host_types
    ));
    tree.render(out, 0);
    out.push('\n');
    out.push_str(&format!(
        "\nexport type {}<B extends {} = {}> =\n  {private}Errors<B> extends never ? {private}Handle<B> : {private}Errors<B>;\n",
        brand.handle, names.host_types, names.host_types
    ));
    Ok(())
}

fn ts_facade_module_path(module_path: &str) -> Vec<String> {
    module_facade_path(module_path)
        .iter()
        .enumerate()
        .map(|(index, selector)| selector.facade_name(index == 0))
        .collect()
}

fn opaque_ts_type_name(module_path: &str, name: &str) -> String {
    format!(
        "__KioOpaque_H{}",
        js::encoded_newtype_identity(module_path, name)
    )
}

fn recursive_structural_ts_type_name(name: &QualifiedTypeName) -> String {
    format!(
        "__KioStructural_H{}",
        js::encoded_newtype_identity(&name.module_segments().join("/"), name.name())
    )
}

fn render_recursive_structural_interfaces(
    nominal_presentations: &BTreeMap<QualifiedTypeName, TsNominalPresentation<'_>>,
    names: &TsNames,
    types: &mut OpaqueTsTypes,
    out: &mut String,
) -> Result<(), EmitError> {
    loop {
        let Some(name) = types.pop_next_recursive_structural() else {
            return Ok(());
        };
        let Some(dependency) = nominal_presentations.get(&name).copied() else {
            return Err(internal_type_error(
                "recursive structural request has no prepared transparent payload",
            ));
        };
        let BoundaryNominalDeclaration::Newtype {
            type_params,
            transparent_payload: Some(payload),
            ..
        } = dependency.declaration
        else {
            return Err(internal_type_error(
                "recursive structural request has no prepared transparent payload",
            ));
        };
        let dependencies = dependency.dependencies;
        let presentation_layout = dependency.layout;
        let arguments = (0..type_params.len())
            .map(|index| format!("T{index}"))
            .collect::<Vec<_>>();
        let carrier = opaque_ts_type_name(&name.module_segments().join("/"), name.name());
        let payload_presentation =
            presentation_layout
                .transparent_payload(&name)
                .ok_or_else(|| {
                    internal_type_error(
                        "recursive structural type has no payload presentation layout",
                    )
                })?;
        types.structural_stack.push(carrier.clone());
        let scope = TsFacadeScope::default();
        let context = TsFacadeRenderContext {
            plan: payload.facade(),
            execution: payload_presentation,
            execution_layout: presentation_layout,
            nominals: dependencies,
            names,
            scope: &scope,
        };
        let payload =
            render_newtype_payload(context, payload, &arguments, payload_presentation, types);
        let popped = types.structural_stack.pop();
        debug_assert_eq!(popped.as_deref(), Some(carrier.as_str()));

        let mut binders = vec![format!("B extends {}", names.host_types)];
        binders.extend(type_params.iter().enumerate().map(|(index, parameter)| {
            let binder = format!("T{index}");
            if matches!(parameter.kind(), Kind::Star) {
                binder
            } else {
                format!(
                    "{binder} extends {}",
                    type_lambda_argument_constraint(parameter.kind(), names)
                )
            }
        }));
        out.push('\n');
        if let Some(version) = types.recursive_structural_retained_at.get(&name) {
            render_deprecated_jsdoc(
                out,
                "",
                &format!("Retained only for host declarations removed at v({version})."),
            );
        }
        out.push_str(&format!(
            "interface {}<{}> {{\n  {}: {};\n}}\n",
            recursive_structural_ts_type_name(&name),
            binders.join(", "),
            ts_property_key(&host_name_core(name.name())),
            payload?,
        ));
    }
}

/// Type-only nominal witnesses for the runtime's private-state-backed opaque handles.
/// The classes are module-private and have no public instance members, so a
/// consumer can infer and shuttle the type but cannot construct it or use a
/// payload-shaped object in its place. A distinct private member origin per
/// exact declaration makes same-shaped newtypes incompatible in TypeScript.
fn render_opaque_type_declarations(types: &OpaqueTsTypes, names: &TsNames, out: &mut String) {
    for (name, kinds) in &types.nominal {
        let params = kinds
            .iter()
            .enumerate()
            .map(|(index, kind)| {
                let name = format!("T{index}");
                if kind.arity() == 0 {
                    name
                } else {
                    format!(
                        "{name} extends {}",
                        type_lambda_argument_constraint(kind, names)
                    )
                }
            })
            .collect::<Vec<_>>();
        let mut binders = vec![format!("B extends {}", names.host_types)];
        binders.extend(params.iter().cloned());
        let mut invariant = vec!["B".to_owned()];
        invariant.extend((0..kinds.len()).map(|index| format!("T{index}")));
        let invariant = format!("readonly [{}]", invariant.join(", "));
        out.push('\n');
        if let Some(version) = types.nominal_retained_at.get(name) {
            render_deprecated_jsdoc(
                out,
                "",
                &format!("Retained only for host declarations removed at v({version})."),
            );
        }
        out.push_str(&format!(
            "declare class {name}<{}> {{\n  private readonly __kioOpaque: (value: {invariant}) => {invariant};\n  private constructor();\n}}\n",
            binders.join(", ")
        ));
    }
}

// =========================================================================
// Shared prepared-facade realization
// =========================================================================

#[derive(Clone, Default)]
struct TsFacadeScope {
    rendered: BTreeMap<FacadeBinderId, String>,
    occupied: BTreeSet<String>,
}

impl TsFacadeScope {
    fn bind(&mut self, id: FacadeBinderId, source: &str, kind: &Kind, names: &TsNames) -> String {
        let base = if source == "B" || source.is_empty() {
            "KioB".to_owned()
        } else {
            host_name_core(source)
        };
        let rendered = if self.occupied.contains(&base) {
            (self.occupied.len()..)
                .map(|index| format!("Kio{base}{index}"))
                .find(|candidate| !self.occupied.contains(candidate))
                .expect("an unbounded suffix space has a free TypeScript binder")
        } else {
            base
        };
        self.occupied.insert(rendered.clone());
        self.rendered.insert(id, rendered.clone());
        if matches!(kind, Kind::Star) {
            rendered
        } else {
            format!(
                "{rendered} extends {}",
                type_lambda_argument_constraint(kind, names)
            )
        }
    }

    fn lookup(&self, id: FacadeBinderId) -> Result<&str, EmitError> {
        self.rendered
            .get(&id)
            .map(String::as_str)
            .ok_or_else(|| internal_type_error("prepared facade refers to an unbound binder"))
    }
}

#[derive(Clone, Copy)]
struct TsFacadeRenderContext<'a> {
    plan: &'a BoundaryFacadePlan,
    execution: &'a BoundaryFacadeExecutionPlan,
    execution_layout: &'a CallablePresentationLayout,
    nominals: &'a BoundaryNominalDependencies,
    names: &'a TsNames,
    scope: &'a TsFacadeScope,
}

impl<'a> TsFacadeRenderContext<'a> {
    fn with_scope<'b>(self, scope: &'b TsFacadeScope) -> TsFacadeRenderContext<'b>
    where
        'a: 'b,
    {
        TsFacadeRenderContext {
            plan: self.plan,
            execution: self.execution,
            execution_layout: self.execution_layout,
            nominals: self.nominals,
            names: self.names,
            scope,
        }
    }
}

fn render_prepared_callable(
    site: PreparedBoundaryCallableSite<'_>,
    names: &TsNames,
    opaque_types: &mut OpaqueTsTypes,
) -> Result<String, EmitError> {
    let previous = std::mem::replace(
        &mut opaque_types.active_retained_at,
        site.retained()
            .map(|metadata| metadata.removed_at_version()),
    );
    let rendered = render_prepared_callable_inner(site, names, opaque_types);
    opaque_types.active_retained_at = previous;
    rendered
}

fn render_prepared_callable_inner(
    site: PreparedBoundaryCallableSite<'_>,
    names: &TsNames,
    opaque_types: &mut OpaqueTsTypes,
) -> Result<String, EmitError> {
    let plan = site.plan();
    let entry = plan.entry();
    let presentation = site.presentation();
    let mut scope = TsFacadeScope::default();
    let mut binders = Vec::new();
    let mut params = Vec::new();
    for (stage, presentation_stage) in entry
        .head_stages
        .into_iter()
        .zip(presentation.head_stages())
    {
        match (stage, presentation_stage) {
            (
                BoundaryCallableHeadStage::Type { id, binder },
                CallableExecutionStage::Type { .. },
            ) => binders.push(scope.bind(id, &binder.name, &binder.kind, names)),
            (BoundaryCallableHeadStage::Value { slots }, CallableExecutionStage::Value(layout)) => {
                let context = TsFacadeRenderContext {
                    plan: plan.facade(),
                    execution: presentation.root_uses(),
                    execution_layout: presentation,
                    nominals: site.nominals(),
                    names,
                    scope: &scope,
                };
                params.extend(render_callable_value_params(
                    context,
                    slots,
                    layout,
                    opaque_types,
                )?)
            }
            _ => {
                return Err(internal_type_error(
                    "prepared TypeScript callable stages disagree with presentation layout",
                ));
            }
        }
    }
    let context = TsFacadeRenderContext {
        plan: plan.facade(),
        execution: presentation.root_uses(),
        execution_layout: presentation,
        nominals: site.nominals(),
        names,
        scope: &scope,
    };
    let result = render_facade_use(context, entry.returned, opaque_types)?;
    let generics = if binders.is_empty() {
        String::new()
    } else {
        format!("<{}>", binders.join(", "))
    };
    let params = params
        .into_iter()
        .enumerate()
        .map(|(index, ty)| format!("p{index}: {ty}"))
        .collect::<Vec<_>>();
    Ok(format!("{generics}({}) => {result}", params.join(", ")))
}

fn render_callable_value_params(
    context: TsFacadeRenderContext<'_>,
    slots: &[FacadeUseId],
    layout: &CallableValueStageLayout,
    opaque_types: &mut OpaqueTsTypes,
) -> Result<Vec<String>, EmitError> {
    let mut params = Vec::with_capacity(layout.source_param_count());
    for source in layout.source_params() {
        let range = source.facade_slots();
        if range.end > slots.len() {
            return Err(internal_type_error(
                "prepared TypeScript source parameter exceeds its semantic slots",
            ));
        }
        let ty = match source.adapter() {
            CallableSourceParamAdapter::UnitValue => continue,
            CallableSourceParamAdapter::Identity => {
                let [use_id] = &slots[range] else {
                    return Err(internal_type_error(
                        "identity TypeScript source parameter does not own one semantic slot",
                    ));
                };
                render_facade_use(context, *use_id, opaque_types)?
            }
            CallableSourceParamAdapter::RightNest => {
                let shell = source.product_shell().ok_or_else(|| {
                    internal_type_error("right-nested TypeScript source parameter has no shell")
                })?;
                let uses = &slots[range];
                if shell.ordered_keys().len() != uses.len() {
                    return Err(internal_type_error(
                        "right-nested TypeScript source parameter shell has the wrong width",
                    ));
                }
                let fields = shell
                    .ordered_keys()
                    .iter()
                    .zip(uses)
                    .map(|(key, use_id)| {
                        Ok(format!(
                            "{}: {}",
                            ts_property_key(&render_semantic_key(key)),
                            render_facade_slot(context, *use_id, opaque_types)?
                        ))
                    })
                    .collect::<Result<Vec<_>, EmitError>>()?;
                format!("{{ {} }}", fields.join("; "))
            }
        };
        params.push(ty);
    }
    Ok(params)
}

fn render_facade_use(
    context: TsFacadeRenderContext<'_>,
    use_id: FacadeUseId,
    opaque_types: &mut OpaqueTsTypes,
) -> Result<String, EmitError> {
    match context.plan.use_at(use_id) {
        FacadeUse::Unit { .. } => Ok("null".to_owned()),
        FacadeUse::Bottom { .. } => Ok("never".to_owned()),
        FacadeUse::Bound { binder, .. } => Ok(context.scope.lookup(*binder)?.to_owned()),
        FacadeUse::Nominal { name, .. } => {
            render_facade_nominal(context, name, &[], opaque_types, false)
        }
        FacadeUse::Apply {
            constructor, args, ..
        } => {
            if let FacadeUse::Nominal { name, .. } = context.plan.use_at(*constructor) {
                return render_facade_nominal(context, name, args, opaque_types, false);
            }
            let head = render_facade_use(context, *constructor, opaque_types)?;
            let args = render_facade_args(context, args, opaque_types)?;
            Ok(format!(
                "{}<{head}, readonly [{}]>",
                context.names.apply,
                args.join(", ")
            ))
        }
        FacadeUse::Product { shell, args, .. } => {
            let fields = shell
                .ordered_keys()
                .iter()
                .zip(args)
                .map(|(key, value)| {
                    Ok(format!(
                        "{}: {}",
                        ts_property_key(&render_semantic_key(key)),
                        render_facade_slot(context, *value, opaque_types)?
                    ))
                })
                .collect::<Result<Vec<_>, EmitError>>()?;
            Ok(format!("{{ {} }}", fields.join("; ")))
        }
        FacadeUse::Sum { shell, args, .. } => {
            let arms = shell
                .ordered_keys()
                .iter()
                .zip(args)
                .map(|(key, value)| {
                    Ok(format!(
                        "{{ {}: {} }}",
                        ts_property_key(&render_semantic_key(key)),
                        render_facade_slot(context, *value, opaque_types)?
                    ))
                })
                .collect::<Result<Vec<_>, EmitError>>()?;
            Ok(arms.join(" | "))
        }
        FacadeUse::Function { slots, result, .. } => {
            let BoundaryFacadeExecutionUse::Function(layout) = context.execution.use_at(use_id)
            else {
                return Err(internal_type_error(
                    "prepared TypeScript function has no presentation layout",
                ));
            };
            let params = render_callable_value_params(context, slots, layout, opaque_types)?
                .into_iter()
                .enumerate()
                .map(|(index, ty)| format!("p{index}: {ty}"))
                .collect::<Vec<_>>();
            Ok(format!(
                "({}) => {}",
                params.join(", "),
                render_facade_use(context, *result, opaque_types)?
            ))
        }
        FacadeUse::Forall { .. } => render_facade_forall(context, use_id, opaque_types),
    }
}

fn render_facade_forall(
    context: TsFacadeRenderContext<'_>,
    root: FacadeUseId,
    opaque_types: &mut OpaqueTsTypes,
) -> Result<String, EmitError> {
    let mut nested = context.scope.clone();
    let mut binders = Vec::new();
    let mut current = root;
    while let FacadeUse::Forall { binder, result, .. } = context.plan.use_at(current) {
        let metadata = context.plan.binder(*binder);
        binders.push(nested.bind(*binder, &metadata.name, &metadata.kind, context.names));
        current = *result;
    }
    let rendered = render_facade_use(context.with_scope(&nested), current, opaque_types)?;
    if matches!(context.plan.use_at(current), FacadeUse::Function { .. }) {
        Ok(format!("<{}>{rendered}", binders.join(", ")))
    } else {
        Ok(format!("<{}>() => {rendered}", binders.join(", ")))
    }
}

fn render_facade_slot(
    context: TsFacadeRenderContext<'_>,
    use_id: FacadeUseId,
    opaque_types: &mut OpaqueTsTypes,
) -> Result<String, EmitError> {
    match context.plan.use_at(use_id) {
        FacadeUse::Nominal { name, .. } => {
            render_facade_nominal(context, name, &[], opaque_types, true)
        }
        FacadeUse::Apply {
            constructor, args, ..
        } if matches!(context.plan.use_at(*constructor), FacadeUse::Nominal { .. }) => {
            let FacadeUse::Nominal { name, .. } = context.plan.use_at(*constructor) else {
                unreachable!()
            };
            render_facade_nominal(context, name, args, opaque_types, true)
        }
        _ => render_facade_use(context, use_id, opaque_types),
    }
}

#[derive(Clone)]
enum TsRuntimeShapeExpr {
    Atom,
    Nominal(QualifiedTypeName, Vec<TsRuntimeShapeExpr>),
    Compound(Vec<TsRuntimeShapeExpr>),
}

fn materialize_runtime_shape(
    plan: &BoundaryFacadePlan,
    use_id: FacadeUseId,
    substitutions: &BTreeMap<FacadeBinderId, TsRuntimeShapeExpr>,
) -> TsRuntimeShapeExpr {
    match plan.use_at(use_id) {
        FacadeUse::Bound { binder, .. } => substitutions
            .get(binder)
            .cloned()
            .unwrap_or(TsRuntimeShapeExpr::Atom),
        FacadeUse::Nominal { name, .. } => TsRuntimeShapeExpr::Nominal(name.clone(), Vec::new()),
        FacadeUse::Apply {
            constructor, args, ..
        } => {
            if let FacadeUse::Nominal { name, .. } = plan.use_at(*constructor) {
                TsRuntimeShapeExpr::Nominal(
                    name.clone(),
                    args.iter()
                        .map(|arg| materialize_runtime_shape(plan, *arg, substitutions))
                        .collect(),
                )
            } else {
                TsRuntimeShapeExpr::Atom
            }
        }
        FacadeUse::Product { args, .. } | FacadeUse::Sum { args, .. } => {
            TsRuntimeShapeExpr::Compound(
                args.iter()
                    .map(|arg| materialize_runtime_shape(plan, *arg, substitutions))
                    .collect(),
            )
        }
        FacadeUse::Unit { .. }
        | FacadeUse::Bottom { .. }
        | FacadeUse::Function { .. }
        | FacadeUse::Forall { .. } => TsRuntimeShapeExpr::Atom,
    }
}

fn transparent_shape_reaches(
    expression: &TsRuntimeShapeExpr,
    root: &QualifiedTypeName,
    nominals: &BoundaryNominalDependencies,
    active: &mut BTreeSet<QualifiedTypeName>,
) -> bool {
    match expression {
        TsRuntimeShapeExpr::Atom => false,
        TsRuntimeShapeExpr::Compound(children) => children
            .iter()
            .any(|child| transparent_shape_reaches(child, root, nominals, active)),
        TsRuntimeShapeExpr::Nominal(name, args) => {
            if name == root {
                return true;
            }
            if !active.insert(name.clone()) {
                return false;
            }
            let reaches = match nominals.declaration(name) {
                Some(BoundaryNominalDeclaration::Newtype {
                    transparent_payload: Some(payload),
                    ..
                }) if payload.declaration_binders().len() == args.len() => {
                    let substitutions = payload
                        .declaration_binders()
                        .iter()
                        .copied()
                        .zip(args.iter().cloned())
                        .collect();
                    let expanded = materialize_runtime_shape(
                        payload.facade(),
                        payload.payload_root(),
                        &substitutions,
                    );
                    transparent_shape_reaches(&expanded, root, nominals, active)
                }
                _ => false,
            };
            active.remove(name);
            reaches
        }
    }
}

fn transparent_payload_is_recursive(
    name: &QualifiedTypeName,
    payload: &BoundaryNewtypePayloadPlan,
    nominals: &BoundaryNominalDependencies,
) -> bool {
    let substitutions = payload
        .declaration_binders()
        .iter()
        .copied()
        .map(|binder| (binder, TsRuntimeShapeExpr::Atom))
        .collect();
    let payload =
        materialize_runtime_shape(payload.facade(), payload.payload_root(), &substitutions);
    transparent_shape_reaches(&payload, name, nominals, &mut BTreeSet::new())
}

fn render_facade_nominal(
    context: TsFacadeRenderContext<'_>,
    name: &QualifiedTypeName,
    args: &[FacadeUseId],
    opaque_types: &mut OpaqueTsTypes,
    absorb_transparent_wrapper: bool,
) -> Result<String, EmitError> {
    let declaration = context.nominals.declaration(name).ok_or_else(|| {
        internal_type_error(&format!(
            "prepared facade omitted nominal dependency `{}.{}`",
            name.module_segments().join("/"),
            name.name()
        ))
    })?;
    match declaration {
        BoundaryNominalDeclaration::HostType {
            type_params,
            binding,
        } => {
            let rendered_args = render_facade_args(context, args, opaque_types)?;
            match binding {
                BoundaryHostTypeBinding::Role(role) => {
                    if !args.is_empty() || !type_params.is_empty() {
                        return Err(internal_type_error(
                            "a role-bearing host type reached TypeScript at positive arity",
                        ));
                    }
                    Ok(role_to_ts_type(*role).to_owned())
                }
                BoundaryHostTypeBinding::Roleless => {
                    let module = name.module_segments().join("/");
                    let head = if matches!(
                        opaque_types.host_binding_origins.get(name),
                        Some(BoundaryHostBindingOrigin::Retained { .. })
                    ) {
                        format!(
                            "__{}RetainedHostBinding<B, {}, {}>",
                            context.names.handle,
                            ts_string_literal(&ts_host_module_path(&module)),
                            ts_string_literal(&host_name_core(name.name()))
                        )
                    } else {
                        format!(
                            "B[{}][{}]",
                            ts_string_literal(&ts_host_module_path(&module)),
                            ts_string_literal(&host_name_core(name.name()))
                        )
                    };
                    render_facade_application(
                        &head,
                        type_params.len(),
                        &rendered_args,
                        context.names,
                    )
                }
            }
        }
        BoundaryNominalDeclaration::Newtype {
            type_params,
            existential_params,
            transparent_payload,
            surface,
        } => {
            let rendered_args = render_facade_args(context, args, opaque_types)?;
            if rendered_args.len() > type_params.len() {
                return Err(internal_type_error(
                    "newtype application exceeds its declared arity",
                ));
            }
            if rendered_args.len() < type_params.len() {
                let witness = newtype_witness_name(context.names, name);
                return render_facade_application(
                    &format!("{witness}<B>"),
                    type_params.len(),
                    &rendered_args,
                    context.names,
                );
            }
            let nominal = !existential_params.is_empty()
                || surface.uses_nominal_carrier()
                || transparent_payload.is_none()
                || transparent_payload.as_ref().is_some_and(|payload| {
                    transparent_payload_is_recursive(name, payload, context.nominals)
                });
            let carrier = opaque_ts_type_name(&name.module_segments().join("/"), name.name());
            if nominal {
                opaque_types.insert(
                    carrier.clone(),
                    type_params
                        .iter()
                        .map(|param| param.kind().clone())
                        .collect(),
                    name,
                );
                let mut all = vec!["B".to_owned()];
                all.extend(rendered_args);
                return Ok(format!("{carrier}<{}>", all.join(", ")));
            }
            if opaque_types.structural_stack.contains(&carrier) {
                opaque_types.insert_recursive_structural(name);
                let mut all = vec!["B".to_owned()];
                all.extend(rendered_args);
                return Ok(format!(
                    "{}<{}>",
                    recursive_structural_ts_type_name(name),
                    all.join(", ")
                ));
            }
            let payload = transparent_payload
                .as_ref()
                .expect("a transparent declaration owns a prepared payload");
            let payload_presentation =
                context
                    .execution_layout
                    .transparent_payload(name)
                    .ok_or_else(|| {
                        internal_type_error(
                            "prepared transparent TypeScript nominal has no payload presentation layout",
                        )
                    })?;
            opaque_types.structural_stack.push(carrier.clone());
            let rendered_payload = render_newtype_payload(
                context,
                payload,
                &rendered_args,
                payload_presentation,
                opaque_types,
            );
            let popped = opaque_types.structural_stack.pop();
            debug_assert_eq!(popped.as_deref(), Some(carrier.as_str()));
            let rendered_payload = rendered_payload?;
            if absorb_transparent_wrapper {
                Ok(rendered_payload)
            } else {
                Ok(format!(
                    "{{ {}: {rendered_payload} }}",
                    ts_property_key(&host_name_core(name.name()))
                ))
            }
        }
    }
}

fn render_newtype_payload(
    context: TsFacadeRenderContext<'_>,
    payload: &BoundaryNewtypePayloadPlan,
    args: &[String],
    payload_execution: &BoundaryFacadeExecutionPlan,
    opaque_types: &mut OpaqueTsTypes,
) -> Result<String, EmitError> {
    if payload.declaration_binders().len() != args.len() {
        return Err(internal_type_error(
            "prepared newtype payload binder arity disagrees with its application",
        ));
    }
    let mut nested = context.scope.clone();
    for (binder, argument) in payload.declaration_binders().iter().zip(args) {
        nested.occupied.insert(argument.clone());
        nested.rendered.insert(*binder, argument.clone());
    }
    let payload_context = TsFacadeRenderContext {
        plan: payload.facade(),
        execution: payload_execution,
        execution_layout: context.execution_layout,
        nominals: context.nominals,
        names: context.names,
        scope: context.scope,
    }
    .with_scope(&nested);
    render_facade_use(payload_context, payload.payload_root(), opaque_types)
}

fn render_facade_args(
    context: TsFacadeRenderContext<'_>,
    args: &[FacadeUseId],
    opaque_types: &mut OpaqueTsTypes,
) -> Result<Vec<String>, EmitError> {
    args.iter()
        .map(|arg| render_facade_use(context, *arg, opaque_types))
        .collect()
}

fn render_facade_application(
    head: &str,
    arity: usize,
    args: &[String],
    names: &TsNames,
) -> Result<String, EmitError> {
    if args.len() > arity {
        return Err(internal_type_error(
            "prepared type application exceeds constructor arity",
        ));
    }
    if arity == 0 {
        return Ok(head.to_owned());
    }
    if args.is_empty() {
        return Ok(head.to_owned());
    }
    let op = if args.len() == arity {
        &names.apply
    } else {
        &names.bind
    };
    Ok(format!("{op}<{head}, readonly [{}]>", args.join(", ")))
}

fn newtype_witness_name(names: &TsNames, name: &QualifiedTypeName) -> String {
    format!(
        "__{}NewtypeK_H{}",
        names.handle,
        js::encoded_newtype_identity(&name.module_segments().join("/"), name.name())
    )
}

fn render_semantic_key(key: &SemanticKey) -> String {
    match key {
        SemanticKey::Bare { name } => host_name_core(name),
        SemanticKey::Qualified {
            module_segments,
            name,
        } => format!(
            "{}.{}",
            module_segments
                .iter()
                .map(|part| host_name_core(part))
                .collect::<Vec<_>>()
                .join("/"),
            host_name_core(name)
        ),
        SemanticKey::Positional { index } => format!("_{index}"),
    }
}

// =========================================================================
// create<Handle> factory
// =========================================================================

/// Render the validated `create<Handle><B>` declaration. A hostful package
/// takes `<Handle>Host<B>`, allowing `B` to infer from an annotation. A
/// hostless package takes no runtime argument but requires explicit `B` while
/// live roleless slots remain. With no slots, `B` defaults to the empty base
/// map; the root parameter stays present for removal stability. The factory
/// name matches the JS module's export through the shared [`js::Brand`].
fn render_create_package(
    prepared: &PreparedBoundaryCallableSites,
    brand: &js::Brand,
    names: &TsNames,
    out: &mut String,
) {
    let host_required = prepared.sites().any(|site| {
        site.execution().is_some()
            && matches!(
                site.site().owner(),
                BoundaryFacadeSiteOwner::HostFunction { .. }
            )
    });
    let bindings_required = prepared.host_bindings().any(|binding| {
        matches!(binding.binding(), BoundaryHostTypeBinding::Roleless)
            && matches!(binding.origin(), BoundaryHostBindingOrigin::Live)
    });
    let private = format!("__{}", brand.handle);
    let default = if bindings_required {
        "never"
    } else {
        names.host_types.as_str()
    };
    let valid_args = if host_required {
        format!("[host: {}<B>]", brand.host_ty)
    } else {
        "[]".to_owned()
    };
    out.push('\n');
    out.push_str(&format!(
        "export function {}<B extends {} = {default}>(\n  ...args: [B] extends [never]\n    ? [error: {}<\"host-type bindings must be selected\">]\n    : {private}Errors<B> extends never\n      ? {valid_args}\n      : [error: {private}Errors<B>]\n): {}<B>;\n",
        brand.factory, names.host_types, names.type_error, brand.handle
    ));
}

// =========================================================================
