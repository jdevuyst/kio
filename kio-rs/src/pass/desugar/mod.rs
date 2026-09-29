//! Early surface lowering from `Surface` to `Desugared`.
//!
//! Runs after source-registry validation and operator folding, before ordinary
//! name resolution and typechecking. Converts the AST from [`Surface`] to
//! [`Desugared`], stripping surface forms whose lowering needs no completed
//! type information:
//!
//! - [`Expr::Tuple`] → nested `__pair__` calls. The
//!   call's leading type arguments are omitted; the typer's
//!   intrinsic-type-arg inference recovers them from value-arg types.
//! - [`Expr::FnPlaceholder`] → plain `Expr::FnExpr` with hygienic
//!   value-param names. The parser stamped each `#N` / bare `#` in
//!   the body as an internal sentinel `Expr::Path { __pN__ }`; the
//!   desugar walks the body, picks fresh `p1`, `p2`, … names that
//!   don't shadow anything already in scope inside the body (bumping
//!   to `pN_2`, `pN_3`, … as needed), rewrites the sentinels, and
//!   wraps the body in `.(p1, …, pN) { body }`.
//!
//! When the desugared module emits an intrinsic call site, the pass
//! auto-injects `import __intrinsics__;` at the top of the module if it
//! isn't already there. Idiomatic surface Kio never references
//! intrinsics directly, so the user shouldn't have to import them
//! just to write `(a, b)`.
//!
//! Surface forms whose lowering depends on a package-wide label table
//! ([`Type::LabelSugar`], [`Item::Labels`], [`Expr::LabelValue`]) are
//! handled by `pass/label_elab` and stripped at the [`Desugared`] →
//! [`Lowered`] boundary.
//!
//! Surface forms that survive every pre-typer pass and live on into
//! [`Lowered`]: user-elaborator calls ([`Expr::UserElaborator`]), field syntax
//! ([`Expr::Elaborator`] for `x.?{foo}` access and
//! `x.!{foo = y}` update). Both need
//! synthesized type information the typer produces, so the typer records each
//! elaboration in the [`crate::pass::typecheck_full::Elaborations`] side
//! table and the substitute pass swaps the recorded form into the
//! AST at the [`Lowered`] → [`Prime`] boundary.
//!
//! [`Expr::Ufcs`] also survives this boundary. Dot-splice calls need the
//! resolved callee type to decide which value-argument slot the receiver
//! fills, so the typer records their equivalent prefix call or
//! user-elaborator expansion and substitution erases the node before
//! Prime.
//!
//! The per-variant clone-and-recurse boilerplate lives on the
//! [`crate::pass::desugar::walk::SurfaceToDesugared`] visitor trait; this
//! module's [`DesugarVisitor`] supplies the rewrite rules and
//! threads the [`DesugarState`] (intrinsic flag, alias-substitution
//! map, binder-scope stack) through the walk.
//!
//! ## kio-prime
//!
//! The kio-prime binary does not route through this pass. Its
//! lowering goes through [`crate::prime::lower`] — a single
//! Surface → Prime walk that rejects every surface-only variant
//! with a parse error, skipping the intermediate [`Desugared`]
//! phase and the `label_elab` step that follows. See
//! [`crate::prime::pipeline::PrimePipeline`] for the wiring.

pub mod walk;

use crate::ast::{
    CallArg, Desugared, ElaboratorCall, ElaboratorKind, Equiv, Expr, FieldAccessLabel,
    FieldUpdateLabel, FnDef, Import, ImportKind, Item, LabelValueLabel, LiteralAlias,
    LiteralAliasValue, Meta, Module, ModulePath, Newtype, NodeId, Op, PackageFile, Param,
    ParamPattern, ParamPatternElem, PathSegment, PlaceholderState, RecCallMode, RecGroup,
    RecOrderDisposition, RecOrderPlan, RecOrderTailRequirement, RecOrderTypeFlow, RowLetEntry,
    Signature, SignatureParam, Surface, Type, TypeMember, TypeParam, TypeRecGroup, TypeRecMember,
    VariadicOperator,
};
use crate::error::Error;
use crate::pass::desugar::walk::SurfaceToDesugared;
use crate::pass::resolve::LocatedError;
use crate::span::Span;
#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// Per-package `pub literal` table, keyed by slash module-path
/// string (`"pkg/helper"`).
pub type PackageLiteralAliases =
    std::collections::HashMap<String, std::collections::HashMap<String, LiteralAliasDef>>;

/// A registered literal alias.
#[derive(Debug, Clone)]
pub struct LiteralAliasDef {
    pub value: LiteralAliasValue,
}

/// Walk a parsed Surface package and collect every `pub literal`
/// declaration into a per-module table indexed by the module's slash
/// path string.
pub fn collect_package_literal_aliases(
    modules: &[(std::path::PathBuf, Module<Surface>)],
    _package_name: Option<&str>,
) -> PackageLiteralAliases {
    let mut tables: PackageLiteralAliases = std::collections::HashMap::new();
    for (_, module) in modules {
        let declared: String = module
            .path
            .segments
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("/");
        let mod_path = declared;
        let module_table = tables.entry(mod_path).or_default();
        for item in &module.items {
            if let Item::LiteralAlias(lit, _) = item
                && lit.vis.is_pub()
            {
                module_table.insert(
                    lit.name.clone(),
                    LiteralAliasDef {
                        value: lit.value.clone(),
                    },
                );
            }
        }
    }
    tables
}

/// Lower a parsed [`Module<Surface>`] into [`Module<Desugared>`].
/// Equivalent to [`desugar_module_with_imports`] with an empty
/// package-literal table; cross-module `literal` references won't
/// resolve. Used by unit tests and single-module compilation paths
/// that don't go through `FullPipeline::lower_package`.
pub fn desugar_module(module: Module<Surface>) -> Result<Module<Desugared>, Error> {
    desugar_module_with_imports(module, &PackageLiteralAliases::default())
}

#[allow(clippy::type_complexity)]
pub fn desugar_package_with_imports(
    modules: Vec<(std::path::PathBuf, Module<Surface>)>,
    package_literal_aliases: &PackageLiteralAliases,
) -> Result<Vec<(std::path::PathBuf, Module<Desugared>)>, LocatedError> {
    let indexed: Vec<_> = modules.into_iter().enumerate().collect();
    let mut results: Vec<ModuleDesugarResult> = crate::maybe_into_par_iter!(indexed)
        .map(|(index, (file_path, module))| {
            let module_path = module_path_to_string(&module.path);
            let result = desugar_module_with_imports(module, package_literal_aliases);
            ModuleDesugarResult {
                index,
                module_path,
                file_path,
                result,
            }
        })
        .collect();
    if let Some(error) = first_desugar_error(&results) {
        return Err(error);
    }
    results.sort_by_key(|result| result.index);
    Ok(results
        .into_iter()
        .map(|result| {
            (
                result.file_path,
                result
                    .result
                    .expect("desugar errors returned before module collection"),
            )
        })
        .collect())
}

struct ModuleDesugarResult {
    index: usize,
    module_path: String,
    file_path: std::path::PathBuf,
    result: Result<Module<Desugared>, Error>,
}

fn first_desugar_error(results: &[ModuleDesugarResult]) -> Option<LocatedError> {
    results
        .iter()
        .filter_map(|result| {
            result.result.as_ref().err().map(|error| {
                let (span, _) = error.diag();
                (
                    result.module_path.as_str(),
                    span.start,
                    span.end,
                    LocatedError {
                        file_path: result.file_path.clone(),
                        error: error.clone(),
                    },
                )
            })
        })
        .min_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)))
        .map(|(_, _, _, located)| located)
}

fn module_path_to_string(p: &ModulePath) -> String {
    p.segments.join("/")
}

fn record_surface_value_name<'a>(
    local_names: &mut std::collections::HashMap<&'a str, (Span, bool)>,
    name: &'a str,
    span: Span,
    is_literal: bool,
) -> Result<(), Error> {
    if let Some(&(first_span, first_is_literal)) = local_names.get(name) {
        if is_literal || first_is_literal {
            return Err(Error::name_res(
                span,
                format!("duplicate top-level declaration `{name}`"),
            )
            .with_secondary(first_span, format!("`{name}` first declared here"))
            .with_help(format!(
                "each top-level name may be declared once — rename this `{name}` or remove the \
                 earlier declaration"
            )));
        }
    } else {
        local_names.insert(name, (span, is_literal));
    }
    Ok(())
}

fn rec_module_type_names(
    items: &[Item<Surface>],
    imports: &[Import],
) -> std::collections::HashSet<String> {
    if !items
        .iter()
        .any(|item| matches!(item, Item::RecGroup(_, _)))
    {
        return std::collections::HashSet::new();
    }
    // A recursive-state wrapper is an ordinary private module type. Reserve
    // every owner-local spelling that could resolve to such a type (including
    // a currently unresolved use) so lowering cannot make authored code bind
    // to the generated owner. The census depends only on this module's items
    // and explicit imports; declarations added in other modules cannot change
    // the chosen name.
    let mut names = std::collections::HashSet::new();
    for import_ in imports {
        if let ImportKind::Selective { items, .. } = &import_.kind {
            for item in items {
                if let crate::ast::ImportItem::Name { name, .. } = item {
                    names.insert(name.clone());
                }
            }
        }
    }
    for item in items {
        collect_surface_item_type_names(item, &mut names);
    }
    names
}

/// Lower a parsed [`Module<Surface>`] into [`Module<Desugared>`],
/// resolving cross-module `pub literal` references via the supplied
/// package-literal table.
pub fn desugar_module_with_imports(
    module: Module<Surface>,
    package_literal_aliases: &PackageLiteralAliases,
) -> Result<Module<Desugared>, Error> {
    let Module {
        path,
        mut imports,
        items,
        meta: Meta { span, .. },
        doc,
    } = module;
    let mut state = DesugarState {
        module_type_names: rec_module_type_names(&items, &imports),
        ..DesugarState::default()
    };
    {
        let mut local_names: std::collections::HashMap<&str, (Span, bool)> =
            std::collections::HashMap::new();
        for item in &items {
            match item {
                Item::FnDef(d) => {
                    record_surface_value_name(&mut local_names, &d.name, d.meta.span, false)?;
                }
                Item::RecGroup(g, _) => {
                    for member in &g.members {
                        record_surface_value_name(
                            &mut local_names,
                            &member.name,
                            member.meta.span,
                            false,
                        )?;
                    }
                }
                Item::Labels(labels, _) => {
                    for entry in &labels.entries {
                        record_surface_value_name(
                            &mut local_names,
                            &entry.name,
                            entry.name_span,
                            false,
                        )?;
                    }
                }
                Item::TypeRecGroup(group) => {
                    for member in &group.members {
                        if let crate::ast::TypeRecMember::Labels(labels, _) = member {
                            for entry in &labels.entries {
                                record_surface_value_name(
                                    &mut local_names,
                                    &entry.name,
                                    entry.name_span,
                                    false,
                                )?;
                            }
                        }
                    }
                }
                Item::LiteralAlias(lit, _) => {
                    record_surface_value_name(&mut local_names, &lit.name, lit.meta.span, true)?;
                }
                Item::HostFn(h) => {
                    record_surface_value_name(&mut local_names, &h.name, h.meta.span, false)?;
                }
                Item::TypeAlias(_)
                | Item::LabelForward(_, _)
                | Item::Newtype(_)
                | Item::Equiv(_, _)
                | Item::Elaborator(_, _)
                | Item::Op(_, _)
                | Item::VariadicOperator(_, _)
                | Item::HostType(_) => {}
            }
        }
    }
    imports.retain_mut(|u| {
        let ImportKind::Selective { items, from } = &mut u.kind else {
            return true;
        };
        let from_path: String = from
            .segments
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("/");
        let Some(table) = package_literal_aliases.get(&from_path) else {
            return true;
        };
        items.retain(|item| {
            let Some(name) = item.as_name() else {
                return true;
            };
            if let Some(lit) = table.get(name) {
                state.literal_aliases.insert(name.to_owned(), lit.clone());
                false
            } else {
                true
            }
        });
        !items.is_empty()
    });
    for item in &items {
        if let Item::LiteralAlias(lit, _) = item {
            state.literal_aliases.insert(
                lit.name.clone(),
                LiteralAliasDef {
                    value: lit.value.clone(),
                },
            );
        }
    }
    let mut visitor = DesugarVisitor { state };
    let lowered_items: Vec<Item<Desugared>> = items
        .into_iter()
        .map(|item| visitor.walk_item(item))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    if visitor.state.needs_intrinsics && !uses_intrinsics(&imports) {
        // Synthesize `import __intrinsics__;` at the head of the file's
        // import list. The synthesized statement carries a zero-width
        // span at the file's start; diagnostics never point at this
        // node (it can't fail name resolution), so the synthetic span
        // is fine.
        let head = imports
            .first()
            .map(|u| u.span)
            .unwrap_or(Span::new(span.start, span.start));
        let synth_span = Span::new(head.start, head.start);
        imports.insert(
            0,
            Import {
                trailing_trivia: Vec::new(),
                kind: ImportKind::Intrinsics,
                span: synth_span,
                leading_trivia: Vec::new(),
            },
        );
    }
    Ok(Module {
        path,
        imports,
        items: lowered_items,
        meta: Meta::new(span),
        doc,
    })
}

/// Lower a package file's body. The package file carries only the
/// phase-independent `bridge` glob list (plus `name` / `build`), so
/// desugaring is a structural pass-through with no surface forms to
/// remove.
pub fn desugar_package_file(
    package_file: PackageFile<Surface>,
) -> Result<PackageFile<Desugared>, Error> {
    let mut visitor = DesugarVisitor {
        state: DesugarState::default(),
    };
    visitor.walk_package_file(package_file)
}

/// Mutable state threaded through one module's desugaring.
#[derive(Default)]
struct DesugarState {
    needs_intrinsics: bool,
    next_synthesized_node_id: u64,
    /// Recursive-group lowering copies a member body into multiple generated
    /// step functions. Each copy needs its own replacement-table identity:
    /// alpha normalization may rename its binders differently in each lexical
    /// wrapper, so sharing the source NodeId would let one typed elaboration
    /// overwrite another.
    refresh_cloned_elaboration_ids: bool,
    /// Monotonic source-occurrence count populated by the ordinary type
    /// lowering walk. RecOrder construction compares snapshots around the
    /// one annotation it already lowers instead of running a second semantic
    /// `Type` prewalk.
    source_type_infer_observations: usize,
    literal_aliases: std::collections::HashMap<String, LiteralAliasDef>,
    /// Every authored type spelling that could bind to a same-module type
    /// owner, predictably label-generated owner, and recursive-state owner
    /// allocated earlier in source order. Recursive lowering clones this
    /// owner-local set, which already includes every group's lexical type
    /// names from the initial borrowed census.
    module_type_names: std::collections::HashSet<String>,
    type_bound: Vec<String>,
    /// Stack of currently in-scope local-binder names. Duplicates are
    /// allowed (an inner binder shadows an outer of the same name);
    /// [`Self::is_bound`] does a linear scan, which is cheap at the
    /// scope depths Kio programs reach in practice.
    bound: Vec<String>,
}

impl DesugarState {
    fn fresh_node_id(&mut self) -> NodeId {
        let offset = self.next_synthesized_node_id;
        self.next_synthesized_node_id = self
            .next_synthesized_node_id
            .checked_add(1)
            .expect("synthesized NodeId counter overflow");
        NodeId(
            crate::ast::SYNTHESIZED_NODE_ID_START
                .checked_add(offset)
                .expect("synthesized NodeId space exhausted"),
        )
    }

    fn cloned_elaboration_id(&mut self, source: NodeId) -> NodeId {
        if self.refresh_cloned_elaboration_ids {
            self.fresh_node_id()
        } else {
            source
        }
    }

    /// True iff `name` is currently in scope as a local binder. Used
    /// by literal expansion to skip aliases shadowed by a local name.
    fn is_bound(&self, name: &str) -> bool {
        self.bound.iter().any(|n| n == name)
    }

    fn save_bound(&self) -> usize {
        self.bound.len()
    }

    fn restore_bound(&mut self, mark: usize) {
        self.bound.truncate(mark);
    }

    fn save_type_bound(&self) -> usize {
        self.type_bound.len()
    }

    fn restore_type_bound(&mut self, mark: usize) {
        self.type_bound.truncate(mark);
    }
}

/// Visitor that drives the surface lowering: rewrites
/// `Expr::Tuple` / `Expr::FnPlaceholder` / `Item::Op` /
/// `Item::LiteralAlias`, substitutes literal references at unshadowed
/// `Expr::Path` sites, flags `needs_intrinsics` for variants that
/// elaborate to intrinsic calls, and threads the binder-scope stack
/// through `FnDef` / `Equiv` / `FnExpr` / `Let` / `Match` clauses.
struct DesugarVisitor {
    state: DesugarState,
}

#[derive(Clone)]
struct RecMemberPlan {
    index: usize,
    name: String,
    /// The member's full surface visibility, carried through verbatim. A
    /// `rec`-group member may be `pub(<module-path>)`-scoped (see
    /// [`specs/language.md`](../../../../specs/language.md) § Visibility),
    /// so collapsing it to a `pub`/private boolean here would drop the
    /// scope restriction from the wrapper `fn`; it is preserved so the
    /// restriction is enforced at name resolution like any other `fn`.
    vis: crate::ast::Visibility,
    wrapper_sig: Signature<Desugared>,
    ret: Type<Desugared>,
    wrapper_ret: Type<Desugared>,
    ret_elided: bool,
    body: Expr<Surface>,
    meta: Meta<Desugared>,
    doc: Option<crate::ast::DocComment>,
    type_params: Vec<TypeParam>,
    value_params: Vec<Param<Desugared>>,
    wrapper_value_params: Vec<Param<Desugared>>,
    phantom_witness_indices: Vec<usize>,
    state_payload_ty: Type<Desugared>,
    state_payload_decl_ty: Type<Desugared>,
    state_type_params: Vec<TypeParam>,
    state_existential_params: Vec<TypeParam>,
    state_existential_indices: Vec<usize>,
    state_arm_ty: Type<Desugared>,
    wrapper_type_name: String,
    wrapper_ctor_name: String,
    wrapper_projector_name: String,
}

struct RecLoweringCtx {
    member_names: std::collections::HashSet<String>,
    members: Vec<RecMemberPlan>,
    state_ty: Type<Desugared>,
    ret_ty: Type<Desugared>,
    step_result_ty: Type<Desugared>,
    uses_cont: bool,
    resume: Option<RecResumePlan>,
    state_type_args: Vec<Type<Desugared>>,
    span: Span,
}

struct RecResumePlan {
    name: String,
    ty: Type<Desugared>,
}

fn rec_state_arm_types(ctx: &RecLoweringCtx) -> Vec<Type<Desugared>> {
    ctx.members
        .iter()
        .map(|member| member.state_arm_ty.clone())
        .chain(ctx.resume.iter().map(|resume| resume.ty.clone()))
        .collect()
}

type RecCallParts = (Vec<Type<Desugared>>, Vec<Expr<Surface>>);

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecCallPosition {
    Tail,
    NonTail,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecCpsApplication {
    Ordinary,
    ExpectedFromBody,
    SynthesizedValue,
}

#[derive(Clone)]
struct RecCpsContinuation {
    expr: Expr<Desugared>,
    application: RecCpsApplication,
}

impl RecCpsContinuation {
    fn ordinary(expr: Expr<Desugared>) -> Self {
        Self {
            expr,
            application: RecCpsApplication::Ordinary,
        }
    }

    fn into_expr(self) -> Expr<Desugared> {
        self.expr
    }
}

// =====================================================================
// Parameter-pattern desugaring (Surface → Surface rewrite, run inline
// from the `FnDef` / `Equiv` / `FnExpr` / `ExportFn` overrides before
// the regular walk descends into the body).
// =====================================================================

fn source_type_from_pattern(pat: &ParamPattern) -> Type<Surface> {
    // Keep the written slot annotations, including elided `_` leaves, on the
    // one generated outer binding. The shared annotation plan validates that
    // source occurrence before pattern projection lowering; generated
    // projection lets do not reinterpret or duplicate its authority.
    pat.outer_type()
}

/// Flatten a [`ParamPattern`]'s elements into a list of
/// `SignatureParam`s. Nested patterns become params that still carry
/// their patterns; the recursive `desugar` walk eliminates them when it
/// visits the generated projection-binding chain.
fn pattern_elems_to_clause_params(elems: Vec<ParamPatternElem>) -> Vec<SignatureParam<Surface>> {
    elems
        .into_iter()
        .map(|e| {
            let p = match e {
                ParamPatternElem::Bind {
                    name,
                    name_span,
                    ty,
                } => Param {
                    name,
                    ty: Some(ty),
                    pattern: None,
                    meta: Meta::new(name_span),
                },
                ParamPatternElem::Tuple(pat) => {
                    // Nested bare tuple at clause-param position. The
                    // outer name is synthesized from the pattern's
                    // `match_id` (which the parser issued; unique
                    // across the parse). The recursive desugar walk
                    // projects the nested slots when it visits the
                    // generated binding chain.
                    let synth_name = format!("__pat_nested_n{}__", pat.match_id.0);
                    let span = pat.span;
                    Param {
                        name: synth_name,
                        ty: None,
                        pattern: Some(pat),
                        meta: Meta::new(span),
                    }
                }
                ParamPatternElem::BindTuple {
                    name,
                    name_span,
                    inner,
                } => {
                    let span = Span::new(name_span.start, inner.span.end);
                    Param {
                        name,
                        ty: None,
                        pattern: Some(inner),
                        meta: Meta::new(span),
                    }
                }
            };
            SignatureParam::Value(p)
        })
        .collect()
}

#[derive(Clone)]
struct PatternBinding {
    name: String,
    name_span: Span,
    value: Expr<Surface>,
    span: Span,
}

fn surface_call_intrinsic(
    name: &str,
    type_args: Vec<Type<Surface>>,
    value_args: Vec<Expr<Surface>>,
    span: Span,
) -> Expr<Surface> {
    let mut args = Vec::with_capacity(type_args.len() + value_args.len());
    args.extend(type_args.into_iter().map(CallArg::Type));
    args.extend(value_args.into_iter().map(CallArg::Value));
    Expr::synth_call(
        Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::new(name.to_owned(), span)],
            meta: Meta::new(span),
            ext: (),
        },
        args,
        span,
    )
}

fn append_pattern_bindings_from_params(
    params: &[SignatureParam<Surface>],
    scrutinee: Expr<Surface>,
    span: Span,
    out: &mut Vec<PatternBinding>,
) {
    let slot_count = params
        .iter()
        .filter(|param| matches!(param, SignatureParam::Value(_)))
        .count();
    let mut current = scrutinee;
    let mut slot_index = 0usize;
    for param in params {
        let SignatureParam::Value(param) = param else {
            continue;
        };
        let remaining = slot_count.saturating_sub(slot_index);
        let slot_expr = if remaining <= 1 {
            current.clone()
        } else {
            surface_call_intrinsic("__fst__", vec![], vec![current.clone()], span)
        };
        match &param.pattern {
            Some(inner) => {
                if param.name != "_" {
                    out.push(PatternBinding {
                        name: param.name.clone(),
                        name_span: param.meta.span,
                        value: slot_expr.clone(),
                        span: param.meta.span,
                    });
                    append_pattern_bindings_from_pattern(
                        inner,
                        surface_path_expr(param.name.clone(), param.meta.span),
                        out,
                    );
                } else {
                    append_pattern_bindings_from_pattern(inner, slot_expr, out);
                }
            }
            None => {
                if param.name != "_" {
                    out.push(PatternBinding {
                        name: param.name.clone(),
                        name_span: param.meta.span,
                        value: slot_expr,
                        span: param.meta.span,
                    });
                }
            }
        }
        if remaining > 1 {
            current = surface_call_intrinsic("__snd__", vec![], vec![current], span);
        }
        slot_index += 1;
    }
}

fn append_pattern_bindings_from_pattern(
    pat: &ParamPattern,
    scrutinee: Expr<Surface>,
    out: &mut Vec<PatternBinding>,
) {
    let params = pattern_elems_to_clause_params(pat.elems.clone());
    append_pattern_bindings_from_params(&params, scrutinee, pat.span, out);
}

fn append_pattern_bound_names(pat: &ParamPattern, out: &mut Vec<String>) {
    for elem in &pat.elems {
        match elem {
            ParamPatternElem::Bind { name, .. } => {
                if name != "_" {
                    out.push(name.clone());
                }
            }
            ParamPatternElem::Tuple(inner) => append_pattern_bound_names(inner, out),
            ParamPatternElem::BindTuple { name, inner, .. } => {
                if name != "_" {
                    out.push(name.clone());
                }
                append_pattern_bound_names(inner, out);
            }
        }
    }
}

fn build_pattern_destructure_wrap(
    scrutinee: Expr<Surface>,
    clause_params: Vec<SignatureParam<Surface>>,
    body: Expr<Surface>,
    wrap_span: Span,
) -> Expr<Surface> {
    let mut bindings = Vec::new();
    append_pattern_bindings_from_params(&clause_params, scrutinee, wrap_span, &mut bindings);
    bindings
        .into_iter()
        .rev()
        .fold(body, |body, binding| Expr::Let {
            occurrence: Default::default(),
            name: binding.name,
            name_span: binding.name_span,
            ty: None,
            pattern: None,
            value: Box::new(binding.value),
            body: Box::new(body),
            meta: Meta::new(binding.span),
        })
}

/// Pre-process a Surface signature's params: for every pattern-
/// bearing value-param, synthesize the outer product type from the
/// pattern's structure, replace the param with a flat `name:
/// <synth-product-type>` slot, and wrap `body` in projection lets
/// over `name`. Nested elements still carry patterns until the
/// recursive desugar walk visits their generated bindings.
fn expand_pattern_params(
    params: Vec<SignatureParam<Surface>>,
    body: Expr<Surface>,
    _outer_ret_ty: Option<Type<Surface>>,
) -> (Vec<SignatureParam<Surface>>, Expr<Surface>) {
    let mut new_params = Vec::with_capacity(params.len());
    let mut new_body = body;
    for sp in params {
        match sp {
            SignatureParam::Type(tp) => new_params.push(SignatureParam::Type(tp)),
            SignatureParam::Value(p) => {
                let Some(pat) = p.pattern else {
                    new_params.push(SignatureParam::Value(p));
                    continue;
                };
                let outer_ty = pat.outer_type();
                let span = pat.span;
                let clause_params = pattern_elems_to_clause_params(pat.elems);
                new_body = build_pattern_destructure_wrap(
                    surface_path_expr(p.name.clone(), span),
                    clause_params,
                    new_body,
                    span,
                );
                new_params.push(SignatureParam::Value(Param {
                    name: p.name,
                    ty: Some(outer_ty),
                    pattern: None,
                    meta: p.meta,
                }));
            }
        }
    }
    (new_params, new_body)
}

/// `equiv`-variant of [`expand_pattern_params`]: one param list,
/// many arm-bodies. Each pattern-bearing param wraps every arm
/// separately, with the parser-issued `match_id` used for the first
/// arm and fresh desugar-issued ids for the rest. The same param
/// list (post-rewrite) is returned alongside the per-arm wrapped
/// bodies.
fn expand_pattern_params_for_arms(
    params: Vec<SignatureParam<Surface>>,
    arms: Vec<Expr<Surface>>,
    _state: &mut DesugarState,
) -> (Vec<SignatureParam<Surface>>, Vec<Expr<Surface>>) {
    // First pass: separate the pattern-bearing params from the rest,
    // building the new (flat) param list and recording the wraps to
    // apply to each arm.
    struct Wrap {
        scrutinee_name: String,
        span: Span,
        clause_params: Vec<SignatureParam<Surface>>,
    }
    let mut new_params = Vec::with_capacity(params.len());
    let mut wraps: Vec<Wrap> = Vec::new();
    for sp in params {
        match sp {
            SignatureParam::Type(tp) => new_params.push(SignatureParam::Type(tp)),
            SignatureParam::Value(p) => {
                let Some(pat) = p.pattern else {
                    new_params.push(SignatureParam::Value(p));
                    continue;
                };
                let outer_ty = pat.outer_type();
                let span = pat.span;
                let scrutinee_name = p.name.clone();
                let clause_params = pattern_elems_to_clause_params(pat.elems);
                wraps.push(Wrap {
                    scrutinee_name,
                    span,
                    clause_params,
                });
                new_params.push(SignatureParam::Value(Param {
                    name: p.name,
                    ty: Some(outer_ty),
                    pattern: None,
                    meta: p.meta,
                }));
            }
        }
    }
    // Second pass: wrap each arm with all the recorded wraps,
    // outermost first (mirrors the single-body case where left-most
    // pattern-param ends up outer).
    let new_arms = arms
        .into_iter()
        .map(|arm| {
            let mut wrapped = arm;
            for w in &wraps {
                wrapped = build_pattern_destructure_wrap(
                    surface_path_expr(w.scrutinee_name.clone(), w.span),
                    w.clause_params.clone(),
                    wrapped,
                    w.span,
                );
            }
            wrapped
        })
        .collect();
    (new_params, new_arms)
}

fn signature_type_params<P: crate::ast::Phase>(sig: &Signature<P>) -> Vec<TypeParam> {
    sig.params
        .iter()
        .filter_map(|p| match p {
            SignatureParam::Type(tp) => Some(tp.clone()),
            SignatureParam::Value(_) => None,
        })
        .collect()
}

fn signature_value_params<P: crate::ast::Phase>(sig: &Signature<P>) -> Vec<Param<P>>
where
    Param<P>: Clone,
{
    sig.params
        .iter()
        .filter_map(|p| match p {
            SignatureParam::Type(_) => None,
            SignatureParam::Value(vp) => Some(vp.clone()),
        })
        .collect()
}

fn signature_has_pattern_params(params: &[SignatureParam<Surface>]) -> bool {
    params.iter().any(|param| {
        matches!(
            param,
            SignatureParam::Value(Param {
                pattern: Some(_),
                ..
            })
        )
    })
}

fn generated_projection_intrinsic(name: &str) -> bool {
    matches!(name, "__fst__" | "__snd__")
}

fn expand_row_let(
    entries: Vec<RowLetEntry<Surface>>,
    value: Box<Expr<Surface>>,
    body: Box<Expr<Surface>>,
    temp_name: String,
    meta: Meta<Surface>,
) -> Expr<Surface> {
    let span = meta.span;
    let mut body = *body;
    for entry in entries.into_iter().rev() {
        let RowLetEntry {
            label,
            label_span,
            local,
            local_span,
            access_ext,
            meta: Meta {
                span: entry_span, ..
            },
            ..
        } = entry;
        let access = Expr::Elaborator {
            occurrence: Default::default(),
            kind: ElaboratorKind::Access,
            call: ElaboratorCall::FieldAccess {
                receiver: Box::new(surface_path_expr(temp_name.clone(), entry_span)),
                labels: vec![FieldAccessLabel {
                    label,
                    label_span,
                    label_type: None,
                    meta: Meta::new(entry_span),
                }],
            },
            meta: Meta::new(entry_span),
            ext: access_ext,
        };
        let let_span = Span::new(entry_span.start, body.span().end);
        body = Expr::Let {
            occurrence: Default::default(),
            name: local,
            name_span: local_span,
            ty: None,
            pattern: None,
            value: Box::new(access),
            body: Box::new(body),
            meta: Meta::new(let_span),
        };
    }
    Expr::Let {
        occurrence: Default::default(),
        name: temp_name,
        name_span: span,
        ty: None,
        pattern: None,
        value,
        body: Box::new(body),
        meta: Meta::new(span),
    }
}

fn deferred_tail_tree_has_required(expr: &Expr<Surface>) -> bool {
    match expr {
        Expr::FnExpr { body, .. } | Expr::FnPlaceholder { body, .. } => {
            expr_contains_rec_call(body)
        }
        Expr::Tuple { items, .. } => items.iter().any(deferred_tail_tree_has_required),
        _ => false,
    }
}

fn deferred_tail_args_have_required(args: &[CallArg<Surface>]) -> bool {
    args.iter()
        .any(|arg| matches!(arg, CallArg::Value(value) if deferred_tail_tree_has_required(value)))
}

fn deferred_tail_ufcs_has_required(receiver: &Expr<Surface>, args: &[CallArg<Surface>]) -> bool {
    deferred_tail_tree_has_required(receiver) || deferred_tail_args_have_required(args)
}

fn rec_group_uses_mode(group: &RecGroup<Surface>, mode: RecCallMode) -> bool {
    group
        .members
        .iter()
        .any(|member| expr_uses_rec_mode(&member.body, mode))
}

fn expr_uses_rec_mode(expr: &Expr<Surface>, mode: RecCallMode) -> bool {
    match expr {
        crate::ast::Expr::BlockCall { .. } => {
            unreachable!("trailing blocks are projected before desugaring")
        }
        Expr::RecCall { modes, args, .. } => {
            modes.contains(&mode) || args.iter().any(|arg| call_arg_uses_rec_mode(arg, mode))
        }
        Expr::Call { callee, args, .. } => {
            expr_uses_rec_mode(callee, mode)
                || args.iter().any(|arg| call_arg_uses_rec_mode(arg, mode))
        }
        Expr::FnExpr { body, .. } | Expr::FnPlaceholder { body, .. }
            if mode == RecCallMode::Cont =>
        {
            expr_contains_rec_call(body)
        }
        Expr::FnExpr { body, .. } | Expr::FnPlaceholder { body, .. } => {
            expr_uses_rec_mode(body, mode)
        }
        Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
            expr_uses_rec_mode(value, mode) || expr_uses_rec_mode(body, mode)
        }
        Expr::RowLet { value, body, .. } => {
            expr_uses_rec_mode(value, mode) || expr_uses_rec_mode(body, mode)
        }
        Expr::Elaborator { call, .. } => elaborator_call_uses_rec_mode(call, mode),
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::UserElaborator { args, .. } => {
            args.iter().any(|arg| call_arg_uses_rec_mode(arg, mode))
        }
        Expr::LabelValue { labels, .. } => labels
            .iter()
            .any(|label| expr_uses_rec_mode(&label.value, mode)),
        Expr::Tuple { items, .. } => items.iter().any(|item| expr_uses_rec_mode(item, mode)),
        Expr::Ufcs { receiver, args, .. } => {
            expr_uses_rec_mode(receiver, mode)
                || args.iter().any(|arg| call_arg_uses_rec_mode(arg, mode))
        }
        Expr::OpChain { kind, .. } => match kind {
            crate::ast::OpChainKind::Normal { slots, .. } => {
                slots.iter().any(|slot| expr_uses_rec_mode(slot, mode))
            }
            crate::ast::OpChainKind::Variadic { elements, .. } => {
                elements.iter().any(|slot| expr_uses_rec_mode(slot, mode))
            }
        },
        Expr::Path { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => false,
        Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
        | Expr::EnrichedRecord { ext, .. }
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

fn call_arg_uses_rec_mode(arg: &CallArg<Surface>, mode: RecCallMode) -> bool {
    match arg {
        CallArg::Type(_) => false,
        CallArg::Value(value) => expr_uses_rec_mode(value, mode),
    }
}

fn elaborator_call_uses_rec_mode(call: &ElaboratorCall<Surface>, mode: RecCallMode) -> bool {
    match call {
        ElaboratorCall::FieldAccess { receiver, .. } => expr_uses_rec_mode(receiver, mode),
        ElaboratorCall::FieldUpdate { receiver, updates } => {
            expr_uses_rec_mode(receiver, mode)
                || updates
                    .iter()
                    .any(|update| expr_uses_rec_mode(&update.value, mode))
        }
    }
}

fn collect_surface_signature_type_names(
    sig: &Signature<Surface>,
    names: &mut std::collections::HashSet<String>,
) {
    for param in &sig.params {
        match param {
            SignatureParam::Type(param) => {
                names.insert(param.name.clone());
            }
            SignatureParam::Value(param) => {
                if let Some(ty) = &param.ty {
                    collect_all_type_names(ty, names);
                }
                if let Some(pattern) = &param.pattern {
                    collect_all_type_names(&pattern.outer_type(), names);
                }
            }
        }
    }
}

fn collect_surface_call_arg_type_names(
    arg: &CallArg<Surface>,
    names: &mut std::collections::HashSet<String>,
) {
    match arg {
        CallArg::Type(ty) => collect_all_type_names(ty, names),
        CallArg::Value(value) => collect_surface_expr_type_names(value, names),
    }
}

fn collect_surface_type_member_head<'a>(
    segments: impl IntoIterator<Item = &'a str>,
    names: &mut std::collections::HashSet<String>,
) {
    let mut segments = segments.into_iter();
    let Some(head) = segments.next() else {
        return;
    };
    if segments.next().is_some() && crate::naming::is_type_name(head) {
        names.insert(head.to_owned());
    }
}

fn collect_surface_capture_type_head(
    capture: &crate::ast::UserElaboratorCapture,
    names: &mut std::collections::HashSet<String>,
) {
    if let [head] = capture.segments.as_slice()
        && crate::naming::is_type_name(head.as_str())
    {
        names.insert(head.name.clone());
        return;
    }
    collect_surface_type_member_head(capture.segments.iter().map(PathSegment::as_str), names);
}

fn collect_surface_expr_type_names(
    expr: &Expr<Surface>,
    names: &mut std::collections::HashSet<String>,
) {
    match expr {
        crate::ast::Expr::BlockCall { .. } => {
            unreachable!("trailing blocks are projected before desugaring")
        }
        Expr::Call { callee, args, .. } => {
            collect_surface_expr_type_names(callee, names);
            for arg in args {
                collect_surface_call_arg_type_names(arg, names);
            }
        }
        Expr::RecCall { args, .. } | Expr::UserElaborator { args, .. } => {
            for arg in args {
                collect_surface_call_arg_type_names(arg, names);
            }
        }
        Expr::FnExpr {
            sig, ret_ty, body, ..
        } => {
            collect_surface_signature_type_names(sig, names);
            if let Some(ret_ty) = ret_ty {
                collect_all_type_names(ret_ty, names);
            }
            collect_surface_expr_type_names(body, names);
        }
        Expr::FnPlaceholder { body, .. } => {
            collect_surface_expr_type_names(body, names);
        }
        Expr::Let {
            ty,
            pattern,
            value,
            body,
            ..
        } => {
            if let Some(ty) = ty {
                collect_all_type_names(ty, names);
            }
            if let Some(pattern) = pattern {
                collect_all_type_names(&pattern.outer_type(), names);
            }
            collect_surface_expr_type_names(value, names);
            collect_surface_expr_type_names(body, names);
        }
        Expr::RowLet { value, body, .. } | Expr::Seq { value, body, .. } => {
            collect_surface_expr_type_names(value, names);
            collect_surface_expr_type_names(body, names);
        }
        Expr::StrLit { annotation, .. }
        | Expr::IntLit { annotation, .. }
        | Expr::FloatLit { annotation, .. }
        | Expr::BoolLit { annotation, .. } => {
            if let Some(annotation) = annotation {
                collect_all_type_names(annotation, names);
            }
        }
        Expr::Tuple { items, .. } => {
            for item in items {
                collect_surface_expr_type_names(item, names);
            }
        }
        Expr::LabelValue { labels, .. } => {
            for label in labels {
                collect_surface_expr_type_names(&label.value, names);
            }
        }
        Expr::Elaborator { call, .. } => match call {
            ElaboratorCall::FieldAccess { receiver, .. } => {
                collect_surface_expr_type_names(receiver, names);
            }
            ElaboratorCall::FieldUpdate { receiver, updates } => {
                collect_surface_expr_type_names(receiver, names);
                for update in updates {
                    collect_surface_expr_type_names(&update.value, names);
                }
            }
        },
        Expr::Ufcs {
            receiver,
            callee_segments,
            args,
            ..
        } => {
            collect_surface_expr_type_names(receiver, names);
            collect_surface_type_member_head(
                callee_segments.iter().map(PathSegment::as_str),
                names,
            );
            for arg in args {
                collect_surface_call_arg_type_names(arg, names);
            }
        }
        Expr::OpChain { kind, .. } => match kind {
            crate::ast::OpChainKind::Normal { slots, .. } => {
                for slot in slots {
                    collect_surface_expr_type_names(slot, names);
                }
            }
            crate::ast::OpChainKind::Variadic { elements, .. } => {
                for slot in elements {
                    collect_surface_expr_type_names(slot, names);
                }
            }
        },
        Expr::Path { segments, .. } => {
            collect_surface_type_member_head(segments.iter().map(PathSegment::as_str), names);
        }
        Expr::Unit { .. } => {}
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
        | Expr::EnrichedRecord { ext, .. }
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

fn collect_surface_label_entry_type_names(
    entry: &crate::ast::LabelEntry<Surface>,
    names: &mut std::collections::HashSet<String>,
) {
    names.extend(entry.type_params.iter().map(|param| param.name.clone()));
    names.extend(
        entry
            .existential_params
            .iter()
            .map(|param| param.name.clone()),
    );
    collect_all_type_names(&entry.payload, names);
}

fn collect_surface_item_type_names(
    item: &Item<Surface>,
    names: &mut std::collections::HashSet<String>,
) {
    match item {
        Item::FnDef(def) => {
            collect_surface_signature_type_names(&def.sig, names);
            collect_all_type_names(&def.ret, names);
            collect_surface_expr_type_names(&def.body, names);
        }
        Item::RecGroup(group, _) => collect_rec_group_type_names(group, names),
        Item::TypeRecGroup(group) => {
            for member in &group.members {
                match member {
                    crate::ast::TypeRecMember::TypeAlias(alias) => {
                        names.insert(alias.name.clone());
                        names.extend(alias.type_params.iter().map(|param| param.name.clone()));
                        collect_all_type_names(&alias.body, names);
                    }
                    crate::ast::TypeRecMember::Newtype(newtype) => {
                        names.insert(newtype.name.clone());
                        names.extend(
                            newtype
                                .type_params
                                .iter()
                                .chain(&newtype.existential_params)
                                .map(|param| param.name.clone()),
                        );
                        collect_all_type_names(&newtype.payload, names);
                    }
                    crate::ast::TypeRecMember::Labels(labels, _) => {
                        names.extend(labels.type_alias_name.iter().cloned());
                        names.extend(
                            labels
                                .entries
                                .iter()
                                .filter(|entry| !entry.is_reuse_marker())
                                .map(|entry| crate::ast::mint_label_newtype_name(&entry.name)),
                        );
                        names.extend(
                            labels
                                .type_alias_params
                                .iter()
                                .map(|param| param.name.clone()),
                        );
                        if let Some(arms) = &labels.type_alias_arms {
                            for arm in arms {
                                for entry in &arm.entries {
                                    collect_surface_label_entry_type_names(entry, names);
                                }
                            }
                        }
                        for entry in &labels.entries {
                            collect_surface_label_entry_type_names(entry, names);
                        }
                    }
                }
            }
        }
        Item::TypeAlias(alias) => {
            names.insert(alias.name.clone());
            names.extend(alias.type_params.iter().map(|param| param.name.clone()));
            collect_all_type_names(&alias.body, names);
        }
        Item::LiteralAlias(_, _) | Item::LabelForward(_, _) => {}
        Item::Newtype(newtype) => {
            names.insert(newtype.name.clone());
            names.extend(
                newtype
                    .type_params
                    .iter()
                    .chain(&newtype.existential_params)
                    .map(|param| param.name.clone()),
            );
            collect_all_type_names(&newtype.payload, names);
        }
        Item::Labels(labels, _) => {
            names.extend(labels.type_alias_name.iter().cloned());
            names.extend(
                labels
                    .entries
                    .iter()
                    .filter(|entry| !entry.is_reuse_marker())
                    .map(|entry| crate::ast::mint_label_newtype_name(&entry.name)),
            );
            names.extend(
                labels
                    .type_alias_params
                    .iter()
                    .map(|param| param.name.clone()),
            );
            if let Some(arms) = &labels.type_alias_arms {
                for arm in arms {
                    for entry in &arm.entries {
                        collect_surface_label_entry_type_names(entry, names);
                    }
                }
            }
            for entry in &labels.entries {
                collect_surface_label_entry_type_names(entry, names);
            }
        }
        Item::Equiv(equiv, _) => {
            collect_surface_signature_type_names(&equiv.sig, names);
            for term in &equiv.terms {
                collect_surface_expr_type_names(&term.body, names);
            }
        }
        Item::Elaborator(elaborator, _) => {
            collect_all_type_names(&elaborator.call_ty, names);
            for capture in &elaborator.captures {
                collect_surface_capture_type_head(capture, names);
            }
            collect_surface_type_member_head(
                elaborator
                    .implementation
                    .segments()
                    .iter()
                    .map(PathSegment::as_str),
                names,
            );
        }
        Item::Op(op, _) => match &op.body {
            crate::ast::OpBody::Normal { function, .. } => {
                collect_surface_type_member_head(function.iter().map(PathSegment::as_str), names);
            }
        },
        Item::VariadicOperator(fold, _) => {
            let spec = fold.spec.as_ref();
            collect_surface_type_member_head(
                spec.initializer.path.iter().map(PathSegment::as_str),
                names,
            );
            collect_surface_type_member_head(spec.step.path.iter().map(PathSegment::as_str), names);
            if let Some(finalize) = &spec.finalize {
                collect_surface_type_member_head(
                    finalize.path.iter().map(PathSegment::as_str),
                    names,
                );
            }
        }
        Item::HostType(host) => {
            names.insert(host.name.clone());
            names.extend(host.type_params.iter().map(|param| param.name.clone()));
        }
        Item::HostFn(host) => {
            for param in &host.params {
                match param {
                    crate::ast::HostFnParam::Type(param) => {
                        names.insert(param.name.clone());
                    }
                    crate::ast::HostFnParam::Value(param) => {
                        collect_all_type_names(&param.ty, names);
                    }
                }
            }
            collect_all_type_names(&host.ret, names);
        }
    }
}

fn collect_rec_group_type_names(
    group: &RecGroup<Surface>,
    names: &mut std::collections::HashSet<String>,
) {
    for member in &group.members {
        collect_surface_signature_type_names(&member.sig, names);
        collect_all_type_names(&member.ret, names);
        collect_surface_expr_type_names(&member.body, names);
    }
}

fn rec_state_outer_type_params(
    params: &[TypeParam],
    span: Span,
    reserved: &mut std::collections::HashSet<String>,
) -> Vec<TypeParam> {
    params
        .iter()
        .enumerate()
        .map(|(index, param)| {
            let base = format!(
                "R{}_i{}_p{}",
                span.start,
                index,
                crate::naming::encode_name_component(&param.name)
            );
            let name = crate::pass::typecheck_core::fresh_type_var(&base, reserved);
            assert!(reserved.insert(name.clone()));
            TypeParam {
                name,
                span: param.span,
                kind: param.kind.clone(),
            }
        })
        .collect()
}

fn rec_wrapper_outer_signature(
    sig: &Signature<Desugared>,
    ret: &Type<Desugared>,
    type_params: &[TypeParam],
    outer_params: &[TypeParam],
) -> (Signature<Desugared>, Type<Desugared>, Vec<Param<Desugared>>) {
    let subst: std::collections::HashMap<String, Type<Desugared>> = type_params
        .iter()
        .zip(outer_params)
        .map(|(source, outer)| {
            (
                source.name.clone(),
                Type::synth_path(vec![outer.name.clone()], Vec::new(), outer.span),
            )
        })
        .collect();
    let mut outer_iter = outer_params.iter();
    let params = sig
        .params
        .iter()
        .map(|param| match param {
            SignatureParam::Type(_) => SignatureParam::Type(
                outer_iter
                    .next()
                    .expect("outer param count matches")
                    .clone(),
            ),
            SignatureParam::Value(value) => SignatureParam::Value(Param {
                name: value.name.clone(),
                ty: value.ty.as_ref().map(|ty| subst_rec_type(ty, &subst)),
                pattern: (),
                meta: value.meta.clone(),
            }),
        })
        .collect::<Vec<_>>();
    let wrapper_sig = Signature::from_parts(params, sig.groups.clone());
    let wrapper_ret = subst_rec_type(ret, &subst);
    let wrapper_value_params = signature_value_params(&wrapper_sig);
    (wrapper_sig, wrapper_ret, wrapper_value_params)
}

fn finish_cont_rec_member_plans(
    plans: &mut [RecMemberPlan],
    ret_ty: &Type<Desugared>,
    step_result_ty: &Type<Desugared>,
    resume_ty: Option<&Type<Desugared>>,
    span: Span,
) {
    let outer_params = plans
        .first()
        .map(|member| member.state_type_params.clone())
        .unwrap_or_default();
    let outer_args = type_param_paths(&outer_params, span);
    let mut state_decl_arms = plans
        .iter()
        .map(|member| {
            Type::synth_path(
                vec![member.wrapper_type_name.clone()],
                outer_args.clone(),
                member.meta.span,
            )
        })
        .collect::<Vec<_>>();
    state_decl_arms.extend(resume_ty.cloned());
    let state_decl_ty = build_sum_right_fold(state_decl_arms, span);
    let subst: std::collections::HashMap<String, Type<Desugared>> = plans
        .first()
        .map(|member| {
            member
                .type_params
                .iter()
                .zip(&outer_params)
                .map(|(source, outer)| {
                    (
                        source.name.clone(),
                        Type::synth_path(vec![outer.name.clone()], Vec::new(), outer.span),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let ret_decl_ty = subst_rec_type(ret_ty, &subst);
    let step_result_decl_ty = sum_type(state_decl_ty, ret_decl_ty, span);
    for member in plans {
        let mut value_tys = member
            .value_params
            .iter()
            .map(|p| {
                p.ty.as_ref()
                    .expect("rec value params were validated to have types")
                    .clone()
            })
            .collect::<Vec<_>>();
        let cont_ty = rec_cont_fn_type(&member.ret, step_result_ty, member.meta.span);
        let mut phantom_base_tys = value_tys.clone();
        phantom_base_tys.push(cont_ty.clone());
        member.phantom_witness_indices = rec_phantom_witness_indices(
            &member.type_params,
            &member.state_existential_indices,
            &phantom_base_tys,
        );
        value_tys.extend(rec_phantom_witness_tys(
            &type_param_paths(&member.type_params, member.meta.span),
            &member.phantom_witness_indices,
            member.meta.span,
        ));
        value_tys.push(cont_ty);
        member.state_payload_ty = crate::ast::build_product_right_fold(value_tys, member.meta.span);

        let mut decl_value_tys = member
            .value_params
            .iter()
            .map(|p| {
                p.ty.as_ref()
                    .expect("rec value params were validated to have types")
                    .clone()
            })
            .collect::<Vec<_>>();
        decl_value_tys.extend(rec_phantom_witness_tys(
            &type_param_paths(&member.type_params, member.meta.span),
            &member.phantom_witness_indices,
            member.meta.span,
        ));
        decl_value_tys.push(rec_cont_fn_type(
            &member.ret,
            &step_result_decl_ty,
            member.meta.span,
        ));
        member.state_payload_decl_ty =
            crate::ast::build_product_right_fold(decl_value_tys, member.meta.span);
    }
}

impl DesugarVisitor {
    fn lower_rec_group(
        &mut self,
        mut group: RecGroup<Surface>,
    ) -> Result<Vec<Item<Desugared>>, Error> {
        self.state.needs_intrinsics = true;
        for member in &mut group.members {
            let body = std::mem::replace(
                &mut member.body,
                Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(member.meta.span),
                },
            );
            let groups = member.sig.groups.clone();
            let (params, body) =
                expand_pattern_params(member.sig.params.clone(), body, Some(member.ret.clone()));
            member.sig = Signature::from_parts(params, groups);
            member.body = body;
        }
        crate::pass::rec_headers::validate(&group)?;
        let mut reserved_type_names = self.state.module_type_names.clone();
        let wrapper_type_names = group
            .members
            .iter()
            .map(|member| {
                let base = rec_wrapper_type_name(group.meta.span, &member.name);
                let name = crate::pass::typecheck_core::fresh_type_var(&base, &reserved_type_names);
                assert!(reserved_type_names.insert(name.clone()));
                name
            })
            .collect::<Vec<_>>();
        self.state
            .module_type_names
            .extend(wrapper_type_names.iter().cloned());
        let uses_cont = rec_group_uses_mode(&group, RecCallMode::Cont);
        let mut plans = self.rec_member_plans(
            &group,
            uses_cont,
            &wrapper_type_names,
            &mut reserved_type_names,
        )?;
        let ret_ty = plans
            .first()
            .expect("rec group has at least one member")
            .wrapper_ret
            .clone();
        let mut state_arms: Vec<Type<Desugared>> = plans
            .iter()
            .map(|member| member.state_arm_ty.clone())
            .collect();
        let state_type_args = type_param_paths(
            &plans
                .first()
                .expect("rec group has at least one member")
                .state_type_params,
            group.meta.span,
        );
        let resume = uses_cont.then(|| {
            let base = format!("Rec_resume{}", group.meta.span.start);
            let name = crate::pass::typecheck_core::fresh_type_var(&base, &reserved_type_names);
            self.state.module_type_names.insert(name.clone());
            RecResumePlan {
                ty: Type::synth_path(vec![name.clone()], state_type_args.clone(), group.meta.span),
                name,
            }
        });
        state_arms.extend(resume.iter().map(|resume| resume.ty.clone()));
        let state_ty = build_sum_right_fold(state_arms, group.meta.span);
        let step_result_ty = sum_type(state_ty.clone(), ret_ty.clone(), group.meta.span);
        if uses_cont {
            finish_cont_rec_member_plans(
                &mut plans,
                &ret_ty,
                &step_result_ty,
                resume.as_ref().map(|resume| &resume.ty),
                group.meta.span,
            );
        }
        let ctx = RecLoweringCtx {
            member_names: plans.iter().map(|member| member.name.clone()).collect(),
            members: plans.clone(),
            state_ty: state_ty.clone(),
            ret_ty: ret_ty.clone(),
            step_result_ty: step_result_ty.clone(),
            uses_cont,
            resume,
            state_type_args,
            span: group.meta.span,
        };
        let mut out = self.synthetic_rec_state_items(&ctx);
        for member in &ctx.members {
            out.push(Item::FnDef(self.rec_wrapper_fn(
                member,
                &ctx,
                group.loop_path.clone(),
            )?));
        }
        Ok(out)
    }

    fn rec_member_plans(
        &mut self,
        group: &RecGroup<Surface>,
        uses_cont: bool,
        wrapper_type_names: &[String],
        reserved_type_names: &mut std::collections::HashSet<String>,
    ) -> Result<Vec<RecMemberPlan>, Error> {
        let first = group.members.first().expect("non-empty rec group");
        let first_type_params = signature_type_params(&first.sig);
        let (state_type_params, state_existential_indices) = if uses_cont {
            (
                rec_state_outer_type_params(
                    &first_type_params,
                    group.meta.span,
                    reserved_type_names,
                ),
                (0..first_type_params.len()).collect::<Vec<_>>(),
            )
        } else {
            let type_mark = self.state.save_type_bound();
            self.bind_signature_type_params(&first.sig.params);
            let first_ret = self.walk_type(first.ret.clone());
            self.state.restore_type_bound(type_mark);
            let first_ret = first_ret?;
            let state_type_params = first_type_params
                .iter()
                .filter(|param| rec_type_mentions_param(&first_ret, &param.name))
                .cloned()
                .collect::<Vec<_>>();
            let state_existential_indices = first_type_params
                .iter()
                .enumerate()
                .filter_map(|(index, param)| {
                    (!rec_type_mentions_param(&first_ret, &param.name)).then_some(index)
                })
                .collect::<Vec<_>>();
            (state_type_params, state_existential_indices)
        };
        let state_type_args = type_param_paths(&state_type_params, group.meta.span);
        group
            .members
            .iter()
            .enumerate()
            .map(|(index, member)| {
                let type_mark = self.state.save_type_bound();
                self.bind_signature_type_params(&member.sig.params);
                let sig_params: Vec<SignatureParam<Desugared>> = member
                    .sig
                    .params
                    .iter()
                    .cloned()
                    .map(|p| self.walk_signature_param(p))
                    .collect::<Result<_, _>>()?;
                let sig = Signature::from_parts(sig_params, member.sig.groups.clone());
                let ret = self.walk_type(member.ret.clone())?;
                self.state.restore_type_bound(type_mark);
                let type_params = signature_type_params(&member.sig);
                let value_params = signature_value_params(&sig);
                let (wrapper_sig, wrapper_ret, wrapper_value_params) = if uses_cont {
                    rec_wrapper_outer_signature(&sig, &ret, &type_params, &state_type_params)
                } else {
                    (sig.clone(), ret.clone(), value_params.clone())
                };
                let value_tys: Vec<Type<Desugared>> = value_params
                    .iter()
                    .map(|p| {
                        p.ty.as_ref()
                            .expect("rec value params were validated to have types")
                            .clone()
                    })
                    .collect();
                let type_param_paths = type_param_paths(&type_params, member.meta.span);
                let state_existential_params = state_existential_indices
                    .iter()
                    .map(|index| type_params[*index].clone())
                    .collect::<Vec<_>>();
                let phantom_witness_indices = if uses_cont {
                    Vec::new()
                } else {
                    rec_phantom_witness_indices(
                        &type_params,
                        &state_existential_indices,
                        &value_tys,
                    )
                };
                let mut state_tys = value_tys;
                state_tys.extend(rec_phantom_witness_tys(
                    &type_param_paths,
                    &phantom_witness_indices,
                    member.meta.span,
                ));
                let state_payload_ty =
                    crate::ast::build_product_right_fold(state_tys, member.meta.span);
                let wrapper_type_name = wrapper_type_names[index].clone();
                let wrapper_ctor_name = rec_wrapper_ctor_name(group.meta.span, &member.name);
                let wrapper_projector_name =
                    rec_wrapper_projector_name(group.meta.span, &member.name);
                let state_arm_ty = Type::synth_path(
                    vec![wrapper_type_name.clone()],
                    state_type_args.clone(),
                    member.meta.span,
                );
                Ok(RecMemberPlan {
                    index,
                    name: member.name.clone(),
                    vis: member.vis.clone(),
                    wrapper_sig,
                    ret,
                    wrapper_ret,
                    ret_elided: member.ret_elided,
                    body: member.body.clone(),
                    meta: Meta::new(member.meta.span),
                    doc: member.doc.clone(),
                    type_params,
                    value_params,
                    wrapper_value_params,
                    phantom_witness_indices,
                    state_payload_decl_ty: state_payload_ty.clone(),
                    state_payload_ty,
                    state_type_params: state_type_params.clone(),
                    state_existential_params,
                    state_existential_indices: state_existential_indices.clone(),
                    state_arm_ty,
                    wrapper_type_name,
                    wrapper_ctor_name,
                    wrapper_projector_name,
                })
            })
            .collect()
    }

    fn synthetic_rec_state_items(&self, ctx: &RecLoweringCtx) -> Vec<Item<Desugared>> {
        let mut newtypes = ctx
            .members
            .iter()
            .map(|member| Newtype {
                vis: crate::ast::Visibility::Private,
                rec_span: None,
                name: member.wrapper_type_name.clone(),
                name_span: member.meta.span,
                type_params: member.state_type_params.clone(),
                existential_params: member.state_existential_params.clone(),
                payload: member.state_payload_decl_ty.clone(),
                constructor: TypeMember {
                    vis: crate::ast::Visibility::Private,
                    name: member.wrapper_ctor_name.clone(),
                    span: member.meta.span,
                    leading_trivia: (),
                },
                projector: TypeMember {
                    vis: crate::ast::Visibility::Private,
                    name: member.wrapper_projector_name.clone(),
                    span: member.meta.span,
                    leading_trivia: (),
                },
                meta: member.meta.clone(),
                editable_span: None,
                doc: None,
            })
            .collect::<Vec<_>>();

        if !ctx.uses_cont {
            return newtypes.into_iter().map(Item::Newtype).collect();
        }
        let resume = ctx.resume.as_ref().expect("CPS groups have a resume arm");
        newtypes.push(Newtype {
            vis: crate::ast::Visibility::Private,
            rec_span: None,
            name: resume.name.clone(),
            name_span: ctx.span,
            type_params: ctx.members[0].state_type_params.clone(),
            existential_params: Vec::new(),
            payload: Type::synth_function(Vec::new(), ctx.step_result_ty.clone(), ctx.span),
            constructor: TypeMember {
                vis: crate::ast::Visibility::Private,
                name: "mk_resume".into(),
                span: ctx.span,
                leading_trivia: (),
            },
            projector: TypeMember {
                vis: crate::ast::Visibility::Private,
                name: "un_resume".into(),
                span: ctx.span,
                leading_trivia: (),
            },
            meta: Meta::new(ctx.span),
            editable_span: None,
            doc: None,
        });

        // Every CPS continuation can resume at every member, making the
        // generated state wrappers one genuinely mutual SCC.  Represent the
        // scope explicitly: implicit module-wide newtype visibility is not a
        // property compiler-generated Kio' may rely on either.
        vec![Item::TypeRecGroup(TypeRecGroup {
            members: newtypes.into_iter().map(TypeRecMember::Newtype).collect(),
            doc: None,
            source_layout: None,
            rec_span: Some(rec_generated_span(ctx.span, 30_000)),
            open_brace_span: None,
            close_brace_span: None,
            deferred_rec_labels_diagnostic: None,
            meta: Meta::new(ctx.span),
        })]
    }

    fn rec_wrapper_fn(
        &mut self,
        member: &RecMemberPlan,
        ctx: &RecLoweringCtx,
        loop_path: Vec<PathSegment>,
    ) -> Result<FnDef<Desugared>, Error> {
        let initial_state = self.rec_initial_state(member, ctx)?;
        let step = self.rec_step_fn(ctx)?;
        let body = Expr::synth_call(
            Expr::Path {
                occurrence: Default::default(),
                segments: loop_path,
                meta: Meta::new(ctx.span),
                ext: (),
            },
            vec![
                CallArg::Type(ctx.state_ty.clone()),
                CallArg::Type(ctx.ret_ty.clone()),
                CallArg::Value(step),
                CallArg::Value(initial_state),
            ],
            member.meta.span,
        );
        Ok(FnDef {
            // Carry the member's full visibility — including a
            // `pub(<module-path>)` scope — onto the wrapper `fn`, so the
            // restriction is enforced at name resolution rather than being
            // widened to a bare `pub`.
            vis: member.vis.clone(),
            purity: crate::ast::Purity::Impure,
            name: member.name.clone(),
            sig: member.wrapper_sig.clone(),
            ret: member.wrapper_ret.clone(),
            ret_elided: member.ret_elided,
            body,
            meta: member.meta.clone(),
            doc: member.doc.clone(),
        })
    }

    fn rec_step_fn(&mut self, ctx: &RecLoweringCtx) -> Result<Expr<Desugared>, Error> {
        let state_name = format!("__rec_state{}__", ctx.span.start);
        let state_param = SignatureParam::Value(Param {
            name: state_name.clone(),
            ty: Some(ctx.state_ty.clone()),
            pattern: (),
            meta: Meta::new(ctx.span),
        });
        let body = self.rec_state_dispatch(&ctx.members, path_expr(state_name, ctx.span), ctx)?;
        Ok(Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::new(vec![state_param]),
            ret_ty: Some(ctx.step_result_ty.clone()),
            body: Box::new(body),
            meta: Meta::new(ctx.span),
            caps: (),
        })
    }

    fn rec_state_dispatch(
        &mut self,
        members: &[RecMemberPlan],
        state: Expr<Desugared>,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        if members.is_empty() {
            let resume = ctx.resume.as_ref().expect("the final state arm is Resume");
            let mut args = ctx
                .state_type_args
                .iter()
                .cloned()
                .map(CallArg::Type)
                .collect::<Vec<_>>();
            args.push(CallArg::Value(state));
            let thunk = call_path(
                vec![
                    PathSegment::new(resume.name.clone(), ctx.span),
                    PathSegment::new("un_resume", ctx.span),
                ],
                args,
                ctx.span,
            );
            return Ok(Expr::synth_call(thunk, Vec::new(), ctx.span));
        }
        if members.len() == 1 && ctx.resume.is_none() {
            return self.rec_member_step_from_state(&members[0], state, ctx);
        }
        let left = &members[0];
        let right = &members[1..];
        let left_name = format!("__rec_case{}_i{}__", ctx.span.start, left.index);
        let right_name = format!("__rec_case{}_tail{}__", ctx.span.start, left.index);
        let left_ref = path_expr(left_name.clone(), left.meta.span);
        let left_body = self.rec_member_step_from_state(left, left_ref, ctx)?;
        let right_tys = right
            .iter()
            .map(|member| member.state_arm_ty.clone())
            .chain(ctx.resume.iter().map(|resume| resume.ty.clone()))
            .collect::<Vec<_>>();
        let right_ty = build_sum_right_fold(right_tys, ctx.span);
        let right_ref = path_expr(right_name.clone(), ctx.span);
        let right_body = self.rec_state_dispatch(right, right_ref, ctx)?;
        let left_fn = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::new(vec![SignatureParam::Value(Param {
                name: left_name,
                ty: Some(left.state_arm_ty.clone()),
                pattern: (),
                meta: Meta::new(left.meta.span),
            })]),
            ret_ty: Some(ctx.step_result_ty.clone()),
            body: Box::new(left_body),
            meta: left.meta.clone(),
            caps: (),
        };
        let right_fn = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::new(vec![SignatureParam::Value(Param {
                name: right_name,
                ty: Some(right_ty.clone()),
                pattern: (),
                meta: Meta::new(ctx.span),
            })]),
            ret_ty: Some(ctx.step_result_ty.clone()),
            body: Box::new(right_body),
            meta: Meta::new(ctx.span),
            caps: (),
        };
        Ok(call_intrinsic_with_type_args(
            "__either__",
            vec![
                left.state_arm_ty.clone(),
                right_ty,
                ctx.step_result_ty.clone(),
            ],
            vec![state, left_fn, right_fn],
            ctx.span,
        ))
    }

    fn rec_member_step_from_state(
        &mut self,
        member: &RecMemberPlan,
        state: Expr<Desugared>,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        let payload_name = format!("__rec_payload{}_i{}__", ctx.span.start, member.index);
        let outer_span = rec_generated_span(member.meta.span, 10 + member.index as u32 * 4);
        let inner_span = rec_generated_span(member.meta.span, 11 + member.index as u32 * 4);
        let mut outer_args: Vec<CallArg<Desugared>> = ctx
            .state_type_args
            .iter()
            .cloned()
            .map(CallArg::Type)
            .collect();
        outer_args.push(CallArg::Value(state));
        let outer = call_path(
            vec![
                PathSegment::new(member.wrapper_type_name.clone(), outer_span),
                PathSegment::new(member.wrapper_projector_name.clone(), outer_span),
            ],
            outer_args,
            outer_span,
        );
        if member.state_existential_params.is_empty() {
            return self.rec_member_step_body(member, outer, ctx);
        }
        let payload_ref = path_expr(payload_name.clone(), member.meta.span);
        let body = self.rec_member_step_body(member, payload_ref, ctx)?;
        let mut params: Vec<SignatureParam<Desugared>> = member
            .state_existential_params
            .iter()
            .cloned()
            .map(SignatureParam::Type)
            .collect();
        params.push(SignatureParam::Value(Param {
            name: payload_name,
            ty: Some(member.state_payload_ty.clone()),
            pattern: (),
            meta: Meta::new(member.meta.span),
        }));
        let continuation = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::new(params),
            ret_ty: Some(ctx.step_result_ty.clone()),
            body: Box::new(body),
            meta: member.meta.clone(),
            caps: (),
        };
        Ok(Expr::synth_call(
            outer,
            vec![CallArg::Value(continuation)],
            inner_span,
        ))
    }

    fn rec_member_step_body(
        &mut self,
        member: &RecMemberPlan,
        payload: Expr<Desugared>,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        let mark = self.state.save_bound();
        for param in &member.value_params {
            self.state.bound.push(param.name.clone());
        }
        let previous_refresh = self.state.refresh_cloned_elaboration_ids;
        self.state.refresh_cloned_elaboration_ids = true;
        let body = if ctx.uses_cont {
            let cont_name = rec_current_cont_name(ctx.span, member.index);
            let cont_ref =
                RecCpsContinuation::ordinary(path_expr(cont_name.clone(), member.meta.span));
            self.transform_rec_cps_expr(
                member.body.clone(),
                member,
                cont_ref,
                RecCallPosition::Tail,
                ctx,
            )
        } else {
            self.transform_rec_tail_expr(member.body.clone(), member, ctx)
        };
        self.state.refresh_cloned_elaboration_ids = previous_refresh;
        let body = body?;
        self.state.restore_bound(mark);
        let params = if ctx.uses_cont {
            let mut params = member.value_params.clone();
            params.extend(rec_phantom_witness_params(member));
            params.push(Param {
                name: rec_current_cont_name(ctx.span, member.index),
                ty: Some(rec_cont_fn_type(
                    &member.ret,
                    &ctx.step_result_ty,
                    member.meta.span,
                )),
                pattern: (),
                meta: Meta::new(member.meta.span),
            });
            params
        } else {
            let mut params = member.value_params.clone();
            params.extend(rec_phantom_witness_params(member));
            params
        };
        Ok(bind_params_from_state(
            &params,
            payload,
            body,
            member.meta.span,
            0,
        ))
    }

    fn rec_cps_pending_let(
        &mut self,
        name: String,
        span: Span,
        type_flow: RecOrderTypeFlow,
        value: Expr<Desugared>,
        body: Expr<Desugared>,
        ctx: &RecLoweringCtx,
    ) -> Expr<Desugared> {
        Expr::RecOrder {
            occurrence: Default::default(),
            plan: Box::new(RecOrderPlan {
                tail_continuation: None,
                name,
                disposition: RecOrderDisposition::Ordered(type_flow),
                annotation: None,
                value: Box::new(value),
                body: Box::new(body),
                runtime_ty: ctx.step_result_ty.clone(),
            }),
            meta: Meta::new(span),
            ext: self.state.fresh_node_id(),
        }
    }

    fn apply_rec_cont(
        &mut self,
        cont: RecCpsContinuation,
        value: Expr<Desugared>,
        span: Span,
    ) -> Expr<Desugared> {
        let call_span = rec_generated_span(span, 20_000);
        // Generated continuation applications carry their local type-flow
        // direction explicitly. The ordering carrier either obtains the
        // pending value's exact type from its use in the continuation body or
        // synthesizes the value's type before checking that body, while
        // preserving call-by-value evaluation order.
        let carrier_flow = match cont.application {
            RecCpsApplication::ExpectedFromBody => Some(RecOrderTypeFlow::ExpectedFromBody),
            RecCpsApplication::SynthesizedValue => Some(RecOrderTypeFlow::SynthesizedValue),
            RecCpsApplication::Ordinary => None,
        };
        let needs_order_carrier = match (&cont.application, &cont.expr) {
            (
                application,
                Expr::FnExpr {
                    sig,
                    ret_ty: Some(_),
                    ..
                },
            ) => matches!(
                sig.params.as_slice(),
                [SignatureParam::Value(Param { ty, .. })]
                    if match application {
                        RecCpsApplication::Ordinary => false,
                        RecCpsApplication::ExpectedFromBody => ty.is_none(),
                        RecCpsApplication::SynthesizedValue => true,
                    }
            ),
            _ => false,
        };
        if needs_order_carrier {
            let Expr::FnExpr {
                sig,
                ret_ty: Some(runtime_ty),
                body,
                ..
            } = cont.expr
            else {
                unreachable!("the generated-continuation shape was checked above")
            };
            let SignatureParam::Value(param) = sig
                .params
                .into_iter()
                .next()
                .expect("the generated continuation has one parameter")
            else {
                unreachable!("the generated continuation parameter is a value")
            };
            return Expr::RecOrder {
                occurrence: Default::default(),
                plan: Box::new(RecOrderPlan {
                    tail_continuation: None,
                    name: param.name,
                    disposition: RecOrderDisposition::Ordered(
                        carrier_flow
                            .expect("an order-sensitive generated continuation has a carrier flow"),
                    ),
                    annotation: param.ty,
                    value: Box::new(value),
                    body,
                    runtime_ty,
                }),
                meta: Meta::new(call_span),
                ext: self.state.fresh_node_id(),
            };
        }
        Expr::synth_call(cont.expr, vec![CallArg::Value(value)], call_span)
    }

    fn rec_initial_state(
        &mut self,
        member: &RecMemberPlan,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        let value_exprs: Vec<Expr<Desugared>> = member
            .wrapper_value_params
            .iter()
            .map(|p| path_expr(p.name.clone(), p.meta.span))
            .collect();
        let value_tys: Vec<Type<Desugared>> = member
            .wrapper_value_params
            .iter()
            .map(|p| {
                p.ty.as_ref()
                    .expect("rec value params were validated to have types")
                    .clone()
            })
            .collect();
        let (payload, type_args) = if ctx.uses_cont {
            let cont = self.rec_initial_cont(member, ctx);
            let cont_ty =
                rec_cont_fn_type(&member.wrapper_ret, &ctx.step_result_ty, member.meta.span);
            let mut values = value_exprs;
            let mut tys = value_tys;
            let type_args = type_param_paths(&member.state_type_params, member.meta.span);
            values.extend(rec_phantom_witness_values(
                &type_args,
                &member.phantom_witness_indices,
                member.meta.span,
            ));
            tys.extend(rec_phantom_witness_tys(
                &type_args,
                &member.phantom_witness_indices,
                member.meta.span,
            ));
            values.push(cont);
            tys.push(cont_ty);
            (
                product_value_from_values(values, tys, member.meta.span),
                type_args,
            )
        } else {
            let type_args = type_param_paths(&member.type_params, member.meta.span);
            let mut values = value_exprs;
            let mut tys = value_tys;
            values.extend(rec_phantom_witness_values(
                &type_args,
                &member.phantom_witness_indices,
                member.meta.span,
            ));
            tys.extend(rec_phantom_witness_tys(
                &type_args,
                &member.phantom_witness_indices,
                member.meta.span,
            ));
            (
                product_value_from_values(values, tys, member.meta.span),
                type_args,
            )
        };
        let state = self.rec_wrap_state(member, ctx.state_type_args.clone(), type_args, payload);
        let state = inject_sum_arm(
            state,
            &rec_state_arm_types(ctx),
            member.index,
            member.meta.span,
        );
        Ok(state)
    }

    fn rec_wrap_state(
        &self,
        member: &RecMemberPlan,
        state_type_args: Vec<Type<Desugared>>,
        type_args: Vec<Type<Desugared>>,
        payload: Expr<Desugared>,
    ) -> Expr<Desugared> {
        let mut args: Vec<CallArg<Desugared>> = state_type_args
            .into_iter()
            .chain(
                member
                    .state_existential_indices
                    .iter()
                    .map(|index| type_args[*index].clone()),
            )
            .map(CallArg::Type)
            .collect();
        args.push(CallArg::Value(payload));
        call_path(
            vec![
                PathSegment::new(member.wrapper_type_name.clone(), member.meta.span),
                PathSegment::new(member.wrapper_ctor_name.clone(), member.meta.span),
            ],
            args,
            member.meta.span,
        )
    }

    fn transform_rec_tail_expr(
        &mut self,
        expr: Expr<Surface>,
        current_member: &RecMemberPlan,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        match expr {
            Expr::RecCall {
                modes,
                callee,
                args,
                meta,
                ..
            } => self.transform_rec_call(modes, callee, args, meta.span, current_member, ctx),
            Expr::RowLet {
                entries,
                value,
                body,
                temp_name,
                meta,
                ..
            } => {
                self.state.needs_intrinsics = true;
                self.transform_rec_tail_expr(
                    expand_row_let(entries, value, body, temp_name, meta),
                    current_member,
                    ctx,
                )
            }
            Expr::Let {
                occurrence: _,
                name,
                name_span,
                ty,
                pattern,
                value,
                body,
                meta,
            } => {
                if let Some(pat) = pattern {
                    self.state.needs_intrinsics = true;
                    debug_assert!(
                        ty.is_none(),
                        "a source pattern cannot carry a separate outer annotation"
                    );
                    let outer_ty = source_type_from_pattern(&pat);
                    let clause_params = pattern_elems_to_clause_params(pat.elems);
                    let body = build_pattern_destructure_wrap(
                        surface_path_expr(name.clone(), pat.span),
                        clause_params,
                        *body,
                        pat.span,
                    );
                    return self.transform_rec_tail_expr(
                        Expr::Let {
                            occurrence: Default::default(),
                            name,
                            name_span,
                            ty: Some(outer_ty),
                            pattern: None,
                            value,
                            body: Box::new(body),
                            meta,
                        },
                        current_member,
                        ctx,
                    );
                }
                self.validate_no_bad_rec(&value, ctx)?;
                let value = self.walk_expr(*value)?;
                let ty = ty.map(|t| self.walk_type(t)).transpose()?;
                let mark = self.state.save_bound();
                self.state.bound.push(name.clone());
                let body = self.transform_rec_tail_expr(*body, current_member, ctx)?;
                self.state.restore_bound(mark);
                Ok(Expr::Let {
                    occurrence: Default::default(),
                    name,
                    name_span,
                    ty,
                    pattern: (),
                    value: Box::new(value),
                    body: Box::new(body),
                    meta: Meta::new(meta.span),
                })
            }
            Expr::Seq {
                occurrence: _,
                value,
                body,
                meta,
            } => {
                self.validate_no_bad_rec(&value, ctx)?;
                let value = self.walk_expr(*value)?;
                let body = self.transform_rec_tail_expr(*body, current_member, ctx)?;
                Ok(Expr::Seq {
                    occurrence: Default::default(),
                    value: Box::new(value),
                    body: Box::new(body),
                    meta: Meta::new(meta.span),
                })
            }
            Expr::UserElaborator {
                occurrence: _,
                name,
                form,
                args,
                meta,
                ext,
            } if deferred_tail_args_have_required(&args)
                || args.iter().any(call_arg_contains_rec) =>
            {
                let continuation = self.rec_initial_cont(current_member, ctx);
                self.transform_deferred_tail_user_elaborator(
                    name,
                    form,
                    args,
                    meta,
                    ext,
                    current_member,
                    RecCpsContinuation::ordinary(continuation),
                    Some(current_member.ret.clone()),
                    RecCallPosition::Tail,
                    ctx,
                )
            }
            Expr::Ufcs {
                occurrence: _,
                receiver,
                callee_segments,
                callee_span,
                args,
                flavor,
                bang: Some(bang),
                meta,
                ext,
            } if deferred_tail_ufcs_has_required(&receiver, &args)
                || expr_contains_rec_call(&receiver)
                || args.iter().any(call_arg_contains_rec) =>
            {
                let continuation = self.rec_initial_cont(current_member, ctx);
                self.transform_deferred_tail_ufcs(
                    *receiver,
                    callee_segments,
                    callee_span,
                    args,
                    flavor,
                    bang,
                    meta,
                    ext,
                    current_member,
                    RecCpsContinuation::ordinary(continuation),
                    Some(current_member.ret.clone()),
                    RecCallPosition::Tail,
                    ctx,
                )
            }
            other => {
                self.validate_no_bad_rec(&other, ctx)?;
                let value = self.walk_expr(other)?;
                Ok(inject_step_return(
                    value,
                    &ctx.state_ty,
                    &ctx.ret_ty,
                    ctx.span,
                ))
            }
        }
    }

    #[allow(clippy::too_many_arguments)] // recursive lowering carries the active CPS continuation
    fn transform_deferred_tail_user_elaborator(
        &mut self,
        name: String,
        form: crate::ast::UserElaboratorCallForm,
        args: Vec<CallArg<Surface>>,
        meta: Meta<Surface>,
        ext: NodeId,
        current_member: &RecMemberPlan,
        continuation: RecCpsContinuation,
        known_continuation_input: Option<Type<Desugared>>,
        position: RecCallPosition,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        let span = meta.span;
        let continuation_name = rec_tmp_name("tail_cont", span, 0);
        let continuation_ref =
            RecCpsContinuation::ordinary(path_expr(continuation_name.clone(), span));
        let defer_callbacks = deferred_tail_args_have_required(&args);
        let args = self.transform_deferred_tail_args(
            args,
            current_member,
            &continuation_ref,
            position,
            defer_callbacks,
            ctx,
        )?;
        let call = Expr::UserElaborator {
            occurrence: Default::default(),
            name,
            form,
            args,
            meta: Meta::new(span),
            ext: self.state.cloned_elaboration_id(ext),
        };
        Ok(self.bind_deferred_tail_continuation(
            continuation_name,
            continuation,
            call,
            known_continuation_input,
            span,
            ctx,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_deferred_tail_ufcs(
        &mut self,
        receiver: Expr<Surface>,
        callee_segments: Vec<PathSegment>,
        callee_span: Span,
        args: Vec<CallArg<Surface>>,
        flavor: crate::ast::UfcsFlavor,
        bang: Span,
        meta: Meta<Surface>,
        ext: NodeId,
        current_member: &RecMemberPlan,
        continuation: RecCpsContinuation,
        known_continuation_input: Option<Type<Desugared>>,
        position: RecCallPosition,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        let span = meta.span;
        let continuation_name = rec_tmp_name("tail_cont", span, 0);
        let continuation_ref =
            RecCpsContinuation::ordinary(path_expr(continuation_name.clone(), span));
        let defer_callbacks = deferred_tail_ufcs_has_required(&receiver, &args);
        let receiver = self.transform_deferred_tail_tree(
            receiver,
            current_member,
            &continuation_ref,
            position,
            defer_callbacks,
            ctx,
        )?;
        let args = self.transform_deferred_tail_args(
            args,
            current_member,
            &continuation_ref,
            position,
            defer_callbacks,
            ctx,
        )?;
        let call = Expr::Ufcs {
            occurrence: Default::default(),
            receiver: Box::new(receiver),
            callee_segments,
            callee_span,
            args,
            flavor,
            bang: Some(bang),
            meta: Meta::new(span),
            ext: self.state.cloned_elaboration_id(ext),
        };
        Ok(self.bind_deferred_tail_continuation(
            continuation_name,
            continuation,
            call,
            known_continuation_input,
            span,
            ctx,
        ))
    }

    fn bind_deferred_tail_continuation(
        &mut self,
        name: String,
        continuation: RecCpsContinuation,
        body: Expr<Desugared>,
        known_continuation_input: Option<Type<Desugared>>,
        span: Span,
        ctx: &RecLoweringCtx,
    ) -> Expr<Desugared> {
        let continuation = continuation.into_expr();
        let result_name = rec_tmp_name("tail_result", span, 1);
        let body = if desugared_contains_rec_quote(&body) {
            Expr::RecQuote {
                occurrence: Default::default(),
                plan: Box::new(crate::ast::RecQuotePlan::Expansion {
                    runtime_ty: ctx.step_result_ty.clone(),
                    continuation: Box::new(path_expr(name.clone(), span)),
                    value: Box::new(body),
                }),
                meta: Meta::new(span),
                ext: self.state.fresh_node_id(),
            }
        } else {
            Expr::RecOrder {
                occurrence: Default::default(),
                plan: Box::new(RecOrderPlan {
                    tail_continuation: None,
                    name: result_name.clone(),
                    disposition: RecOrderDisposition::Ordered(RecOrderTypeFlow::ElaboratedValue),
                    annotation: known_continuation_input.clone(),
                    value: Box::new(body),
                    body: Box::new(path_expr(result_name, span)),
                    runtime_ty: ctx.step_result_ty.clone(),
                }),
                meta: Meta::new(span),
                ext: self.state.fresh_node_id(),
            }
        };
        match known_continuation_input {
            Some(input) => Expr::Let {
                occurrence: Default::default(),
                name,
                name_span: span,
                ty: Some(rec_cont_fn_type(&input, &ctx.step_result_ty, span)),
                pattern: (),
                value: Box::new(continuation),
                body: Box::new(body),
                meta: Meta::new(span),
            },
            None => Expr::RecOrder {
                occurrence: Default::default(),
                plan: Box::new(RecOrderPlan {
                    tail_continuation: None,
                    name,
                    disposition: RecOrderDisposition::Ordered(RecOrderTypeFlow::ExpectedFromBody),
                    annotation: Some(rec_cont_fn_type(
                        &Type::Infer {
                            meta: Meta::new(span),
                            ext: (),
                        },
                        &ctx.step_result_ty,
                        span,
                    )),
                    value: Box::new(continuation),
                    body: Box::new(body),
                    runtime_ty: ctx.step_result_ty.clone(),
                }),
                meta: Meta::new(span),
                ext: self.state.fresh_node_id(),
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_deferred_tail_args(
        &mut self,
        args: Vec<CallArg<Surface>>,
        current_member: &RecMemberPlan,
        continuation: &RecCpsContinuation,
        position: RecCallPosition,
        defer_callbacks: bool,
        ctx: &RecLoweringCtx,
    ) -> Result<Vec<CallArg<Desugared>>, Error> {
        args.into_iter()
            .map(|arg| match arg {
                CallArg::Type(ty) => Ok(CallArg::Type(self.walk_type(ty)?)),
                CallArg::Value(value) => self
                    .transform_deferred_tail_tree(
                        value,
                        current_member,
                        continuation,
                        position,
                        defer_callbacks,
                        ctx,
                    )
                    .map(CallArg::Value),
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_deferred_tail_tree(
        &mut self,
        value: Expr<Surface>,
        current_member: &RecMemberPlan,
        continuation: &RecCpsContinuation,
        position: RecCallPosition,
        defer_callbacks: bool,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        let needs_local_completion = match &value {
            Expr::UserElaborator { args, .. } => deferred_tail_args_have_required(args),
            Expr::Ufcs {
                receiver,
                args,
                bang: Some(_),
                ..
            } => deferred_tail_ufcs_has_required(receiver, args),
            _ => false,
        };
        if needs_local_completion {
            let span = value.span();
            let name = rec_tmp_name("quoted_local_cont", span, 0);
            let body = self.transform_rec_cps_expr(
                value,
                current_member,
                RecCpsContinuation::ordinary(path_expr(name.clone(), span)),
                RecCallPosition::NonTail,
                ctx,
            )?;
            let computation = rec_cps_lambda(
                name,
                Some(rec_cont_fn_type(
                    &Type::Infer {
                        meta: Meta::new(span),
                        ext: (),
                    },
                    &ctx.step_result_ty,
                    span,
                )),
                body,
                ctx,
                span,
                RecCpsApplication::Ordinary,
            )
            .into_expr();
            return Ok(Expr::RecQuote {
                occurrence: Default::default(),
                plan: Box::new(crate::ast::RecQuotePlan::Operand {
                    public_ty: None,
                    runtime_ty: ctx.step_result_ty.clone(),
                    computation: Box::new(computation),
                }),
                meta: Meta::new(span),
                ext: self.state.fresh_node_id(),
            });
        }
        if !defer_callbacks
            && matches!(value, Expr::FnExpr { .. } | Expr::FnPlaceholder { .. })
            && !expr_contains_rec_call(&value)
        {
            return self.walk_expr(value);
        }
        match value {
            value @ Expr::RecCall { .. } => {
                let public = self.rec_expr_type_hint(&value, current_member, ctx);
                let span = value.span();
                let name = rec_tmp_name("quoted_cont", span, 0);
                let body = self.transform_rec_cps_expr(
                    value,
                    current_member,
                    RecCpsContinuation::ordinary(path_expr(name.clone(), span)),
                    RecCallPosition::NonTail,
                    ctx,
                )?;
                let public = public.expect("validated recursive member has a public result type");
                let computation = rec_cps_lambda(
                    name,
                    Some(rec_cont_fn_type(&public, &ctx.step_result_ty, span)),
                    body,
                    ctx,
                    span,
                    RecCpsApplication::Ordinary,
                )
                .into_expr();
                Ok(Expr::RecQuote {
                    occurrence: Default::default(),
                    plan: Box::new(crate::ast::RecQuotePlan::Operand {
                        public_ty: Some(public),
                        runtime_ty: ctx.step_result_ty.clone(),
                        computation: Box::new(computation),
                    }),
                    meta: Meta::new(span),
                    ext: self.state.fresh_node_id(),
                })
            }
            Expr::Call {
                callee, args, meta, ..
            } => {
                let callee = self.transform_deferred_tail_tree(
                    *callee,
                    current_member,
                    continuation,
                    RecCallPosition::NonTail,
                    defer_callbacks,
                    ctx,
                )?;
                let args = self.transform_deferred_tail_args(
                    args,
                    current_member,
                    continuation,
                    RecCallPosition::NonTail,
                    defer_callbacks,
                    ctx,
                )?;
                Ok(Expr::synth_call(callee, args, meta.span))
            }
            Expr::FnExpr {
                occurrence: _,
                sig,
                ret_ty,
                body,
                meta,
                caps: _,
            } => self.transform_deferred_tail_lambda(
                sig,
                ret_ty,
                *body,
                meta,
                current_member,
                continuation.clone(),
                position,
                ctx,
            ),
            Expr::FnPlaceholder {
                mut body,
                stem,
                mut state,
                meta,
                ..
            } => {
                let slot_count =
                    crate::pass::placeholder::prepare(&stem, &mut state, &mut body, meta.span)?;
                self.transform_deferred_tail_placeholder(
                    *body,
                    slot_count,
                    stem.span,
                    meta,
                    current_member,
                    continuation.clone(),
                    position,
                    ctx,
                )
            }
            Expr::Tuple { items, .. } => {
                let items = items
                    .into_iter()
                    .map(|item| {
                        self.transform_deferred_tail_tree(
                            item,
                            current_member,
                            continuation,
                            position,
                            defer_callbacks,
                            ctx,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(self.fold_desugared_tuple(items))
            }
            Expr::RowLet {
                entries,
                value,
                body,
                temp_name,
                meta,
                ..
            } => {
                self.state.needs_intrinsics = true;
                self.transform_deferred_tail_tree(
                    expand_row_let(entries, value, body, temp_name, meta),
                    current_member,
                    continuation,
                    position,
                    defer_callbacks,
                    ctx,
                )
            }
            Expr::Let {
                name,
                name_span,
                ty,
                pattern,
                value,
                body,
                meta,
                ..
            } => {
                if let Some(pattern) = pattern {
                    self.state.needs_intrinsics = true;
                    let outer_ty = source_type_from_pattern(&pattern);
                    let body = build_pattern_destructure_wrap(
                        surface_path_expr(name.clone(), pattern.span),
                        pattern_elems_to_clause_params(pattern.elems),
                        *body,
                        pattern.span,
                    );
                    return self.transform_deferred_tail_tree(
                        Expr::Let {
                            occurrence: Default::default(),
                            name,
                            name_span,
                            ty: Some(outer_ty),
                            pattern: None,
                            value,
                            body: Box::new(body),
                            meta,
                        },
                        current_member,
                        continuation,
                        position,
                        defer_callbacks,
                        ctx,
                    );
                }
                let value = self.transform_deferred_tail_tree(
                    *value,
                    current_member,
                    continuation,
                    RecCallPosition::NonTail,
                    defer_callbacks,
                    ctx,
                )?;
                let ty = ty.map(|ty| self.walk_type(ty)).transpose()?;
                let mark = self.state.save_bound();
                self.state.bound.push(name.clone());
                let body = self.transform_deferred_tail_tree(
                    *body,
                    current_member,
                    continuation,
                    position,
                    defer_callbacks,
                    ctx,
                )?;
                self.state.restore_bound(mark);
                Ok(Expr::Let {
                    occurrence: Default::default(),
                    name,
                    name_span,
                    ty,
                    pattern: (),
                    value: Box::new(value),
                    body: Box::new(body),
                    meta: Meta::new(meta.span),
                })
            }
            Expr::Seq {
                value, body, meta, ..
            } => {
                let value = self.transform_deferred_tail_tree(
                    *value,
                    current_member,
                    continuation,
                    RecCallPosition::NonTail,
                    defer_callbacks,
                    ctx,
                )?;
                let body = self.transform_deferred_tail_tree(
                    *body,
                    current_member,
                    continuation,
                    position,
                    defer_callbacks,
                    ctx,
                )?;
                Ok(Expr::Seq {
                    occurrence: Default::default(),
                    value: Box::new(value),
                    body: Box::new(body),
                    meta: Meta::new(meta.span),
                })
            }
            Expr::LabelValue {
                labels, meta, ext, ..
            } => {
                self.state.needs_intrinsics |= labels.len() >= 2;
                let labels = labels
                    .into_iter()
                    .map(|label| {
                        Ok(LabelValueLabel {
                            label: label.label,
                            label_span: label.label_span,
                            value: self.transform_deferred_tail_tree(
                                label.value,
                                current_member,
                                continuation,
                                RecCallPosition::NonTail,
                                defer_callbacks,
                                ctx,
                            )?,
                            meta: Meta::new(label.meta.span),
                        })
                    })
                    .collect::<Result<_, Error>>()?;
                Ok(Expr::LabelValue {
                    occurrence: Default::default(),
                    labels,
                    meta: Meta::new(meta.span),
                    ext: self.state.cloned_elaboration_id(ext),
                })
            }
            Expr::Elaborator {
                kind,
                call,
                meta,
                ext,
                ..
            } => {
                self.state.needs_intrinsics = true;
                let call = match call {
                    ElaboratorCall::FieldAccess { receiver, labels } => {
                        ElaboratorCall::FieldAccess {
                            receiver: Box::new(self.transform_deferred_tail_tree(
                                *receiver,
                                current_member,
                                continuation,
                                RecCallPosition::NonTail,
                                defer_callbacks,
                                ctx,
                            )?),
                            labels: labels
                                .into_iter()
                                .map(|label| FieldAccessLabel {
                                    label: label.label,
                                    label_span: label.label_span,
                                    label_type: label.label_type,
                                    meta: Meta::new(label.meta.span),
                                })
                                .collect(),
                        }
                    }
                    ElaboratorCall::FieldUpdate { receiver, updates } => {
                        ElaboratorCall::FieldUpdate {
                            receiver: Box::new(self.transform_deferred_tail_tree(
                                *receiver,
                                current_member,
                                continuation,
                                RecCallPosition::NonTail,
                                defer_callbacks,
                                ctx,
                            )?),
                            updates: updates
                                .into_iter()
                                .map(|update| {
                                    Ok(FieldUpdateLabel {
                                        label: update.label,
                                        label_span: update.label_span,
                                        label_type: update.label_type,
                                        value: self.transform_deferred_tail_tree(
                                            update.value,
                                            current_member,
                                            continuation,
                                            RecCallPosition::NonTail,
                                            defer_callbacks,
                                            ctx,
                                        )?,
                                        meta: Meta::new(update.meta.span),
                                    })
                                })
                                .collect::<Result<_, Error>>()?,
                        }
                    }
                };
                Ok(Expr::Elaborator {
                    occurrence: Default::default(),
                    kind,
                    call,
                    meta: Meta::new(meta.span),
                    ext: self.state.cloned_elaboration_id(ext),
                })
            }
            Expr::Ufcs {
                receiver,
                callee_segments,
                callee_span,
                args,
                flavor,
                bang,
                meta,
                ext,
                ..
            } => {
                self.state.needs_intrinsics |= bang.is_some();
                let receiver = self.transform_deferred_tail_tree(
                    *receiver,
                    current_member,
                    continuation,
                    RecCallPosition::NonTail,
                    defer_callbacks,
                    ctx,
                )?;
                let args = self.transform_deferred_tail_args(
                    args,
                    current_member,
                    continuation,
                    RecCallPosition::NonTail,
                    defer_callbacks,
                    ctx,
                )?;
                Ok(Expr::Ufcs {
                    occurrence: Default::default(),
                    receiver: Box::new(receiver),
                    callee_segments,
                    callee_span,
                    args,
                    flavor,
                    bang,
                    meta: Meta::new(meta.span),
                    ext: self.state.cloned_elaboration_id(ext),
                })
            }
            Expr::UserElaborator {
                name,
                form,
                args,
                meta,
                ext,
                ..
            } => {
                self.state.needs_intrinsics = true;
                let args = self.transform_deferred_tail_args(
                    args,
                    current_member,
                    continuation,
                    position,
                    defer_callbacks,
                    ctx,
                )?;
                Ok(Expr::UserElaborator {
                    occurrence: Default::default(),
                    name,
                    form,
                    args,
                    meta: Meta::new(meta.span),
                    ext: self.state.cloned_elaboration_id(ext),
                })
            }
            value => {
                self.validate_no_bad_rec(&value, ctx)?;
                self.walk_expr(value)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_deferred_tail_lambda(
        &mut self,
        sig: Signature<Surface>,
        ret_ty: Option<Type<Surface>>,
        body: Expr<Surface>,
        meta: Meta<Surface>,
        current_member: &RecMemberPlan,
        continuation: RecCpsContinuation,
        position: RecCallPosition,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        if signature_has_pattern_params(&sig.params) {
            self.state.needs_intrinsics = true;
        }
        let groups = sig.groups;
        let (sig_params, body) = expand_pattern_params(sig.params, body, ret_ty.clone());
        let type_mark = self.state.save_type_bound();
        self.bind_signature_type_params(&sig_params);
        let params: Vec<SignatureParam<Desugared>> = sig_params
            .into_iter()
            .map(|param| self.walk_signature_param(param))
            .collect::<Result<_, _>>()?;
        // An anonymous `-> _` is equivalent to an omitted return annotation.
        // Normalize only that root form; nested inferred slots remain authored
        // annotation structure for the synthesized-value direction.
        let written_return = ret_ty
            .map(|ty| self.walk_type(ty))
            .transpose()?
            .filter(|ty| !matches!(ty, Type::Infer { .. }));
        let mark = self.state.save_bound();
        for param in &params {
            if let SignatureParam::Value(param) = param {
                self.state.bound.push(param.name.clone());
            }
        }
        let body = self.transform_deferred_tail_body(
            body,
            written_return.clone(),
            current_member,
            continuation,
            position,
            ctx,
        )?;
        let public_return = matches!(
            &body,
            Expr::RecOrder { plan, .. }
                if matches!(plan.disposition, RecOrderDisposition::DeferredTail {
                    requirement: RecOrderTailRequirement::Required, ..
                })
        )
        .then_some(written_return)
        .flatten();
        self.state.restore_bound(mark);
        self.state.restore_type_bound(type_mark);
        Ok(Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(params, groups),
            ret_ty: public_return,
            body: Box::new(body),
            meta: Meta::new(meta.span),
            caps: (),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_deferred_tail_body(
        &mut self,
        body: Expr<Surface>,
        written_return: Option<Type<Desugared>>,
        current_member: &RecMemberPlan,
        continuation: RecCpsContinuation,
        position: RecCallPosition,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        let required = expr_contains_rec_call(&body);
        let tail_continuation = Box::new(continuation.expr.clone());
        let token = self.state.fresh_node_id();
        let value_name = format!("__rec_tail_value{}__", token.0);
        let value_span = body.span();
        let (value, compact_body, runtime_ty, annotation, requirement) = if required {
            let value =
                self.transform_rec_cps_expr(body, current_member, continuation, position, ctx)?;
            (
                value,
                path_expr(value_name.clone(), value_span),
                ctx.step_result_ty.clone(),
                None,
                RecOrderTailRequirement::Required,
            )
        } else {
            self.validate_no_bad_rec(&body, ctx)?;
            let value = self.walk_expr(body)?;
            // The authored value may be a call whose span-keyed call plan is
            // replayed during substitution. Keep that metadata off the
            // generated continuation application.
            let compact_span = rec_generated_span(value_span, 20_000);
            let compact_body = Expr::synth_call(
                continuation.into_expr(),
                vec![CallArg::Value(path_expr(value_name.clone(), value_span))],
                compact_span,
            );
            (
                value,
                compact_body,
                ctx.step_result_ty.clone(),
                written_return,
                RecOrderTailRequirement::Optional,
            )
        };
        let lifted_flow =
            if requirement == RecOrderTailRequirement::Optional && annotation.is_none() {
                RecOrderTypeFlow::ExpectedFromBody
            } else {
                RecOrderTypeFlow::SynthesizedValue
            };
        Ok(Expr::RecOrder {
            occurrence: Default::default(),
            plan: Box::new(RecOrderPlan {
                tail_continuation: Some(tail_continuation),
                name: value_name,
                disposition: RecOrderDisposition::DeferredTail {
                    requirement,
                    lifted_flow,
                },
                annotation,
                value: Box::new(value),
                body: Box::new(compact_body),
                runtime_ty,
            }),
            meta: Meta::new(value_span),
            ext: token,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_deferred_tail_placeholder(
        &mut self,
        body: Expr<Surface>,
        slot_count: usize,
        stem_span: Span,
        meta: Meta<Surface>,
        current_member: &RecMemberPlan,
        continuation: RecCpsContinuation,
        position: RecCallPosition,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        let body = self.transform_deferred_tail_body(
            body,
            None,
            current_member,
            continuation,
            position,
            ctx,
        )?;
        Ok(lower_fn_placeholder(body, slot_count, stem_span, meta))
    }

    fn fold_desugared_tuple(&mut self, values: Vec<Expr<Desugared>>) -> Expr<Desugared> {
        debug_assert!(values.len() >= 2);
        self.state.needs_intrinsics = true;
        let mut values = values.into_iter().rev();
        let mut value = values.next().expect("non-empty");
        for left in values {
            // The source-item start makes every nested synthetic call span
            // unique, so intrinsic type-argument resolution keys cannot
            // collide within the fold.
            let span = Span::new(left.span().start, value.span().end);
            value = call_intrinsic("__pair__", vec![left, value], span);
        }
        value
    }

    fn rec_call_parts(
        &mut self,
        member: &RecMemberPlan,
        args: Vec<CallArg<Surface>>,
        span: Span,
        ctx: &RecLoweringCtx,
    ) -> Result<RecCallParts, Error> {
        self.rec_header_call_parts(
            (&member.name, &member.type_params, member.value_params.len()),
            args,
            span,
            &ctx.member_names,
        )
    }

    fn rec_header_call_parts(
        &mut self,
        (name, type_params, value_param_count): (&str, &[TypeParam], usize),
        args: Vec<CallArg<Surface>>,
        span: Span,
        member_names: &std::collections::HashSet<String>,
    ) -> Result<RecCallParts, Error> {
        let type_param_count = type_params.len();
        let explicit_type_args =
            type_param_count > 0 && args.len() == type_param_count + value_param_count;
        if args.len() != value_param_count && !explicit_type_args {
            return Err(Error::parse(
                span,
                format!(
                    "`rec {}` expects {} value argument(s), or {} type argument(s) followed by \
                     {} value argument(s), but {} argument(s) were supplied",
                    name,
                    value_param_count,
                    type_param_count,
                    value_param_count,
                    args.len()
                ),
            ));
        }
        let mut args = args.into_iter();
        let type_args = if explicit_type_args {
            (0..type_param_count)
                .map(|_| {
                    let arg = args
                        .next()
                        .expect("explicit type-arg count was checked before splitting");
                    if let CallArg::Value(value) = &arg {
                        self.validate_no_bad_rec_names(value, member_names)?;
                    }
                    let ty = crate::pass::typecheck_core::call_arg_to_type_arg(&arg)?;
                    self.walk_type(ty)
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            type_param_paths(type_params, span)
        };
        let values = args
            .map(|arg| match arg {
                CallArg::Value(value) => Ok(value),
                CallArg::Type(ty) => Err(Error::parse(
                    ty.span(),
                    format!(
                        "`rec {}` value argument positions must contain expressions",
                        name
                    ),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((type_args, values))
    }

    fn validate_rec_poly_mode(
        &self,
        member: &RecMemberPlan,
        current_member: &RecMemberPlan,
        type_args: &[Type<Desugared>],
        modes: &[RecCallMode],
        span: Span,
    ) -> Result<(), Error> {
        self.validate_rec_header_poly_mode(
            &member.name,
            &current_member.type_params,
            type_args,
            modes,
            span,
        )
    }

    fn validate_rec_header_poly_mode(
        &self,
        name: &str,
        current_type_params: &[TypeParam],
        type_args: &[Type<Desugared>],
        modes: &[RecCallMode],
        span: Span,
    ) -> Result<(), Error> {
        let is_poly = !rec_type_args_are_current(current_type_params, type_args, span);
        let has_poly = modes.contains(&RecCallMode::Poly);
        if is_poly && !has_poly {
            return Err(Error::parse(
                span,
                format!("recursive call to `{}` changes type parameters", name),
            )
            .with_help("write `rec(poly) name(T, ...)` for polymorphic recursion"));
        }
        if has_poly && !is_poly {
            return Err(Error::parse(
                span,
                "`rec(poly)` is only needed when the recursive call changes type parameters",
            )
            .with_help("remove `poly` from this recursive call"));
        }
        Ok(())
    }

    fn rec_expr_type_hint(
        &mut self,
        expr: &Expr<Surface>,
        current_member: &RecMemberPlan,
        ctx: &RecLoweringCtx,
    ) -> Option<Type<Desugared>> {
        match expr {
            Expr::RecCall {
                modes,
                callee,
                args,
                meta,
                ..
            } => {
                validate_rec_escape_absent(modes, meta.span).ok()?;
                let member = ctx
                    .members
                    .iter()
                    .find(|member| member.name == callee.name)?;
                let (type_args, _) = self
                    .rec_call_parts(member, args.clone(), meta.span, ctx)
                    .ok()?;
                self.validate_rec_poly_mode(member, current_member, &type_args, modes, meta.span)
                    .ok()?;
                Some(instantiate_rec_ret_ty(member, &type_args))
            }
            Expr::Tuple { items, meta, .. } => {
                let tys = items
                    .iter()
                    .map(|item| self.rec_expr_type_hint(item, current_member, ctx))
                    .collect::<Option<Vec<_>>>()?;
                Some(crate::ast::build_product_right_fold(tys, meta.span))
            }
            Expr::Unit {
                occurrence: _,
                meta,
            } => Some(Type::Unit {
                meta: Meta::new(meta.span),
            }),
            Expr::IntLit {
                annotation: Some(ty),
                ..
            }
            | Expr::FloatLit {
                annotation: Some(ty),
                ..
            }
            | Expr::StrLit {
                annotation: Some(ty),
                ..
            } => self.walk_type(ty.clone()).ok(),
            _ => None,
        }
    }

    fn transform_rec_call(
        &mut self,
        modes: Vec<RecCallMode>,
        callee: PathSegment,
        args: Vec<CallArg<Surface>>,
        span: Span,
        current_member: &RecMemberPlan,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        validate_rec_escape_absent(&modes, span)?;
        if modes.contains(&RecCallMode::Cont) {
            return Err(Error::parse(
                span,
                "`rec(cont)` is only needed for recursive calls that are not in tail position",
            )
            .with_help("remove `cont` from this tail-position recursive call"));
        }
        let Some(member) = ctx.members.iter().find(|member| member.name == callee.name) else {
            return Err(Error::name_res(
                callee.span,
                format!(
                    "`rec {}` is not a member of this `rec(loop)` group",
                    callee.name
                ),
            ));
        };
        let (type_args, values) = self.rec_call_parts(member, args, span, ctx)?;
        self.validate_rec_poly_mode(member, current_member, &type_args, &modes, span)?;
        let values = values
            .into_iter()
            .map(|value| {
                self.validate_no_bad_rec(&value, ctx)?;
                self.walk_expr(value)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let value_tys = instantiate_rec_value_tys(member, &type_args);
        let mut values = values;
        let mut value_tys = value_tys;
        values.extend(rec_phantom_witness_values(
            &type_args,
            &member.phantom_witness_indices,
            span,
        ));
        value_tys.extend(rec_phantom_witness_tys(
            &type_args,
            &member.phantom_witness_indices,
            span,
        ));
        let payload = product_value_from_values(values, value_tys, span);
        let member_state =
            self.rec_wrap_state(member, ctx.state_type_args.clone(), type_args, payload);
        let state = inject_sum_arm(member_state, &rec_state_arm_types(ctx), member.index, span);
        Ok(inject_step_continue(
            state,
            &ctx.state_ty,
            &ctx.ret_ty,
            span,
        ))
    }

    fn transform_rec_cps_expr(
        &mut self,
        expr: Expr<Surface>,
        current_member: &RecMemberPlan,
        cont: RecCpsContinuation,
        position: RecCallPosition,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        match expr {
            Expr::RecCall {
                modes,
                callee,
                args,
                meta,
                ..
            } => self.transform_rec_call_cps(
                modes,
                callee,
                args,
                meta.span,
                current_member,
                cont,
                position,
                ctx,
            ),
            Expr::RowLet {
                entries,
                value,
                body,
                temp_name,
                meta,
                ..
            } => {
                self.state.needs_intrinsics = true;
                self.transform_rec_cps_expr(
                    expand_row_let(entries, value, body, temp_name, meta),
                    current_member,
                    cont,
                    position,
                    ctx,
                )
            }
            Expr::Let {
                occurrence: _,
                name,
                name_span,
                ty,
                pattern,
                value,
                body,
                meta,
            } => {
                if let Some(pat) = pattern {
                    self.state.needs_intrinsics = true;
                    debug_assert!(
                        ty.is_none(),
                        "a source pattern cannot carry a separate outer annotation"
                    );
                    let outer_ty = source_type_from_pattern(&pat);
                    let clause_params = pattern_elems_to_clause_params(pat.elems);
                    let body = build_pattern_destructure_wrap(
                        surface_path_expr(name.clone(), pat.span),
                        clause_params,
                        *body,
                        pat.span,
                    );
                    return self.transform_rec_cps_expr(
                        Expr::Let {
                            occurrence: Default::default(),
                            name,
                            name_span,
                            ty: Some(outer_ty),
                            pattern: None,
                            value,
                            body: Box::new(body),
                            meta,
                        },
                        current_member,
                        cont,
                        position,
                        ctx,
                    );
                }
                if expr_contains_rec_call(&value) {
                    // Preserve the source let's annotation flow on the
                    // generated continuation. The ordinary source-to-
                    // Desugared type walk records whether this exact
                    // annotation contained a source occurrence; RecOrder
                    // never rescans a copied type to select its semantic path.
                    let infer_before = self.state.source_type_infer_observations;
                    let annotation_missing = ty.is_none();
                    let ty = ty.map(|t| self.walk_type(t)).transpose()?;
                    let source_has_infer =
                        self.state.source_type_infer_observations != infer_before;
                    let application = if annotation_missing || source_has_infer {
                        RecCpsApplication::SynthesizedValue
                    } else {
                        RecCpsApplication::Ordinary
                    };
                    let mark = self.state.save_bound();
                    self.state.bound.push(name.clone());
                    let next = self.transform_rec_cps_expr(
                        *body,
                        current_member,
                        cont,
                        RecCallPosition::Tail,
                        ctx,
                    )?;
                    self.state.restore_bound(mark);
                    let k = rec_cps_lambda(name, ty, next, ctx, meta.span, application);
                    self.transform_rec_cps_expr(
                        *value,
                        current_member,
                        k,
                        RecCallPosition::NonTail,
                        ctx,
                    )
                } else {
                    self.validate_no_bad_rec(&value, ctx)?;
                    let value = self.walk_expr(*value)?;
                    let ty = ty.map(|t| self.walk_type(t)).transpose()?;
                    let mark = self.state.save_bound();
                    self.state.bound.push(name.clone());
                    let body = self.transform_rec_cps_expr(
                        *body,
                        current_member,
                        cont,
                        RecCallPosition::Tail,
                        ctx,
                    )?;
                    self.state.restore_bound(mark);
                    Ok(Expr::Let {
                        occurrence: Default::default(),
                        name,
                        name_span,
                        ty,
                        pattern: (),
                        value: Box::new(value),
                        body: Box::new(body),
                        meta: Meta::new(meta.span),
                    })
                }
            }
            Expr::Seq {
                occurrence: _,
                value,
                body,
                meta,
            } => {
                if expr_contains_rec_call(&value) {
                    let name = rec_tmp_name("seq", meta.span, 0);
                    let value_ty = Type::Unit {
                        meta: Meta::new(value.span()),
                    };
                    let next = self.transform_rec_cps_expr(
                        *body,
                        current_member,
                        cont,
                        RecCallPosition::Tail,
                        ctx,
                    )?;
                    let k = rec_cps_lambda(
                        name,
                        Some(value_ty),
                        next,
                        ctx,
                        meta.span,
                        RecCpsApplication::Ordinary,
                    );
                    self.transform_rec_cps_expr(
                        *value,
                        current_member,
                        k,
                        RecCallPosition::NonTail,
                        ctx,
                    )
                } else {
                    self.validate_no_bad_rec(&value, ctx)?;
                    let value = self.walk_expr(*value)?;
                    let body = self.transform_rec_cps_expr(
                        *body,
                        current_member,
                        cont,
                        RecCallPosition::Tail,
                        ctx,
                    )?;
                    Ok(Expr::Seq {
                        occurrence: Default::default(),
                        value: Box::new(value),
                        body: Box::new(body),
                        meta: Meta::new(meta.span),
                    })
                }
            }
            Expr::Call {
                callee, args, meta, ..
            } => self.transform_rec_cps_call(*callee, args, meta.span, current_member, cont, ctx),
            Expr::Tuple { items, meta, .. } => self.transform_rec_cps_tuple_items(
                items,
                Vec::new(),
                meta.span,
                current_member,
                cont,
                ctx,
            ),
            Expr::LabelValue {
                occurrence: _,
                labels,
                meta,
                ext,
            } => self.transform_rec_cps_label_values(
                labels,
                Vec::new(),
                meta,
                ext,
                current_member,
                cont,
                ctx,
            ),
            Expr::Elaborator {
                occurrence: _,
                kind,
                call,
                meta,
                ext,
            } => {
                let expr = Expr::Elaborator {
                    occurrence: Default::default(),
                    kind,
                    call,
                    meta,
                    ext,
                };
                self.validate_no_bad_rec(&expr, ctx)?;
                let value = self.walk_expr(expr)?;
                Ok(self.apply_rec_cont(cont, value, ctx.span))
            }
            Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match ext {},
            // The carried continuation already includes work after this call;
            // the checked recipe, not the call's source position, decides
            // whether each staged lambda is terminal.
            Expr::UserElaborator {
                occurrence: _,
                name,
                form,
                args,
                meta,
                ext,
            } if deferred_tail_args_have_required(&args)
                || args.iter().any(call_arg_contains_rec) =>
            {
                self.transform_deferred_tail_user_elaborator(
                    name,
                    form,
                    args,
                    meta,
                    ext,
                    current_member,
                    cont,
                    None,
                    position,
                    ctx,
                )
            }
            Expr::UserElaborator {
                occurrence: _,
                name,
                form,
                args,
                meta,
                ext,
            } => self.transform_rec_cps_user_elaborator_args(
                name,
                form,
                args,
                Vec::new(),
                meta,
                ext,
                current_member,
                cont,
                ctx,
            ),
            // Bang-UFCS has the same structural tail authority as a direct
            // elaborator call; receiver placement does not narrow it.
            Expr::Ufcs {
                occurrence: _,
                receiver,
                callee_segments,
                callee_span,
                args,
                flavor,
                bang: Some(bang),
                meta,
                ext,
            } if deferred_tail_ufcs_has_required(&receiver, &args)
                || expr_contains_rec_call(&receiver)
                || args.iter().any(call_arg_contains_rec) =>
            {
                self.transform_deferred_tail_ufcs(
                    *receiver,
                    callee_segments,
                    callee_span,
                    args,
                    flavor,
                    bang,
                    meta,
                    ext,
                    current_member,
                    cont,
                    None,
                    position,
                    ctx,
                )
            }
            Expr::Ufcs {
                occurrence: _,
                receiver,
                callee_segments,
                callee_span,
                mut args,
                flavor,
                bang,
                meta,
                ext,
            } => {
                let receiver_contains_rec = expr_contains_rec_call(&receiver);
                let first_rec_arg = args.iter().position(call_arg_contains_rec);
                let receiver_is_first = flavor.inserts_first();
                if bang.is_none()
                    && receiver_is_first
                    && !receiver_contains_rec
                    && first_rec_arg.is_some()
                    && !rec_cps_operand_is_inert(&receiver)
                {
                    let span = receiver.span();
                    let name = rec_tmp_name("ufcs_receiver", span, 0);
                    let receiver_value = *receiver;
                    self.validate_no_bad_rec(&receiver_value, ctx)?;
                    let receiver_value = self.walk_expr(receiver_value)?;
                    let type_span = receiver_value.span();
                    let body = self.transform_rec_cps_expr(
                        Expr::Ufcs {
                            occurrence: Default::default(),
                            receiver: Box::new(surface_path_expr(name.clone(), span)),
                            callee_segments,
                            callee_span,
                            args,
                            flavor,
                            bang,
                            meta,
                            ext,
                        },
                        current_member,
                        cont,
                        position,
                        ctx,
                    )?;
                    Ok(self.rec_cps_pending_let(
                        name,
                        type_span,
                        RecOrderTypeFlow::ExpectedFromBody,
                        receiver_value,
                        body,
                        ctx,
                    ))
                } else if let Some(earlier_index) = if bang.is_none() {
                    let earlier_end =
                        match (receiver_is_first, receiver_contains_rec, first_rec_arg) {
                            (true, true, _) => None,
                            (_, _, Some(index)) => Some(index),
                            (false, true, None) => Some(args.len()),
                            _ => None,
                        };
                    earlier_end.and_then(|end| {
                        args[..end].iter().position(|arg| {
                            matches!(
                                arg,
                                CallArg::Value(value) if !rec_cps_operand_is_inert(value)
                            )
                        })
                    })
                } else {
                    None
                } {
                    let span = args[earlier_index].meta().span;
                    let name = rec_tmp_name("ufcs_arg", span, earlier_index);
                    let CallArg::Value(arg_value) = std::mem::replace(
                        &mut args[earlier_index],
                        CallArg::Value(surface_path_expr(name.clone(), span)),
                    ) else {
                        unreachable!("the earlier UFCS operand was selected as a value")
                    };
                    self.validate_no_bad_rec(&arg_value, ctx)?;
                    let arg_value = self.walk_expr(arg_value)?;
                    let type_span = arg_value.span();
                    let body = self.transform_rec_cps_expr(
                        Expr::Ufcs {
                            occurrence: Default::default(),
                            receiver,
                            callee_segments,
                            callee_span,
                            args,
                            flavor,
                            bang,
                            meta,
                            ext,
                        },
                        current_member,
                        cont,
                        position,
                        ctx,
                    )?;
                    Ok(self.rec_cps_pending_let(
                        name,
                        type_span,
                        RecOrderTypeFlow::ExpectedFromBody,
                        arg_value,
                        body,
                        ctx,
                    ))
                } else if receiver_contains_rec
                    && (bang.is_some() || receiver_is_first || first_rec_arg.is_none())
                {
                    let span = receiver.span();
                    let name = rec_tmp_name("ufcs_receiver", span, 0);
                    let receiver_value = *receiver;
                    let param_ty = self.rec_expr_type_hint(&receiver_value, current_member, ctx);
                    let body = self.transform_rec_cps_expr(
                        Expr::Ufcs {
                            occurrence: Default::default(),
                            receiver: Box::new(surface_path_expr(name.clone(), span)),
                            callee_segments,
                            callee_span,
                            args,
                            flavor,
                            bang,
                            meta,
                            ext,
                        },
                        current_member,
                        cont,
                        position,
                        ctx,
                    )?;
                    let k = rec_cps_lambda(
                        name,
                        param_ty,
                        body,
                        ctx,
                        span,
                        RecCpsApplication::ExpectedFromBody,
                    );
                    self.transform_rec_cps_expr(
                        receiver_value,
                        current_member,
                        k,
                        RecCallPosition::NonTail,
                        ctx,
                    )
                } else if let Some(index) = first_rec_arg {
                    let span = args[index].meta().span;
                    let name = rec_tmp_name("ufcs_arg", span, index);
                    let CallArg::Value(arg_value) = std::mem::replace(
                        &mut args[index],
                        CallArg::Value(surface_path_expr(name.clone(), span)),
                    ) else {
                        unreachable!("call_arg_contains_rec only matches value arguments")
                    };
                    let param_ty = self.rec_expr_type_hint(&arg_value, current_member, ctx);
                    let body = self.transform_rec_cps_expr(
                        Expr::Ufcs {
                            occurrence: Default::default(),
                            receiver,
                            callee_segments,
                            callee_span,
                            args,
                            flavor,
                            bang,
                            meta,
                            ext,
                        },
                        current_member,
                        cont,
                        position,
                        ctx,
                    )?;
                    let k = rec_cps_lambda(
                        name,
                        param_ty,
                        body,
                        ctx,
                        span,
                        RecCpsApplication::ExpectedFromBody,
                    );
                    self.transform_rec_cps_expr(
                        arg_value,
                        current_member,
                        k,
                        RecCallPosition::NonTail,
                        ctx,
                    )
                } else {
                    let expr = Expr::Ufcs {
                        occurrence: Default::default(),
                        receiver,
                        callee_segments,
                        callee_span,
                        args,
                        flavor,
                        bang,
                        meta,
                        ext,
                    };
                    self.validate_no_bad_rec(&expr, ctx)?;
                    let value = self.walk_expr(expr)?;
                    Ok(self.apply_rec_cont(cont, value, ctx.span))
                }
            }
            Expr::FnExpr {
                occurrence: _,
                sig,
                ret_ty,
                body,
                meta,
                caps,
            } => {
                let has_rec = expr_contains_rec_call(&body);
                let expr = Expr::FnExpr {
                    occurrence: Default::default(),
                    sig,
                    ret_ty,
                    body,
                    meta,
                    caps,
                };
                if has_rec {
                    Err(Error::parse(
                        expr.span(),
                        "`rec` inside a nested function requires `rec(escape)`, which is reserved",
                    ))
                } else {
                    self.validate_no_bad_rec(&expr, ctx)?;
                    let value = self.walk_expr(expr)?;
                    Ok(self.apply_rec_cont(cont, value, ctx.span))
                }
            }
            Expr::FnPlaceholder {
                occurrence: _,
                body,
                stem,
                state,
                meta,
                ext,
            } => {
                let has_rec = expr_contains_rec_call(&body);
                let expr = Expr::FnPlaceholder {
                    occurrence: Default::default(),
                    body,
                    stem,
                    state,
                    meta,
                    ext,
                };
                if has_rec {
                    Err(Error::parse(
                        expr.span(),
                        "`rec` inside a nested function requires `rec(escape)`, which is reserved",
                    ))
                } else {
                    self.validate_no_bad_rec(&expr, ctx)?;
                    let value = self.walk_expr(expr)?;
                    Ok(self.apply_rec_cont(cont, value, ctx.span))
                }
            }
            other => {
                self.validate_no_bad_rec(&other, ctx)?;
                let value = self.walk_expr(other)?;
                Ok(self.apply_rec_cont(cont, value, ctx.span))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_rec_cps_user_elaborator_args(
        &mut self,
        name: String,
        form: crate::ast::UserElaboratorCallForm,
        mut args: Vec<CallArg<Surface>>,
        mut out: Vec<CallArg<Desugared>>,
        meta: Meta<Surface>,
        ext: NodeId,
        current_member: &RecMemberPlan,
        cont: RecCpsContinuation,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        if args.is_empty() {
            let ext = self.state.cloned_elaboration_id(ext);
            let value = Expr::UserElaborator {
                occurrence: Default::default(),
                name,
                form,
                args: out,
                meta: Meta::new(meta.span),
                ext,
            };
            return Ok(self.apply_rec_cont(cont, value, meta.span));
        }
        let first = args.remove(0);
        match first {
            CallArg::Type(ty) => {
                out.push(CallArg::Type(self.walk_type(ty)?));
                self.transform_rec_cps_user_elaborator_args(
                    name,
                    form,
                    args,
                    out,
                    meta,
                    ext,
                    current_member,
                    cont,
                    ctx,
                )
            }
            CallArg::Value(value) if expr_contains_rec_call(&value) => {
                let span = value.span();
                let arg_name = rec_tmp_name("elaborator_arg", span, out.len());
                let param_ty = self.rec_expr_type_hint(&value, current_member, ctx);
                let mut next_out = out;
                next_out.push(CallArg::Value(path_expr(arg_name.clone(), span)));
                let body = self.transform_rec_cps_user_elaborator_args(
                    name,
                    form,
                    args,
                    next_out,
                    meta,
                    ext,
                    current_member,
                    cont,
                    ctx,
                )?;
                let k = rec_cps_lambda(
                    arg_name,
                    param_ty,
                    body,
                    ctx,
                    span,
                    RecCpsApplication::ExpectedFromBody,
                );
                self.transform_rec_cps_expr(value, current_member, k, RecCallPosition::NonTail, ctx)
            }
            CallArg::Value(value) => {
                self.validate_no_bad_rec(&value, ctx)?;
                out.push(CallArg::Value(self.walk_expr(value)?));
                self.transform_rec_cps_user_elaborator_args(
                    name,
                    form,
                    args,
                    out,
                    meta,
                    ext,
                    current_member,
                    cont,
                    ctx,
                )
            }
        }
    }

    fn transform_rec_cps_call(
        &mut self,
        callee: Expr<Surface>,
        args: Vec<CallArg<Surface>>,
        span: Span,
        current_member: &RecMemberPlan,
        cont: RecCpsContinuation,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        if expr_contains_rec_call(&callee) {
            let callee_span = callee.span();
            let name = rec_tmp_name("callee", callee_span, 0);
            let callee_ref = path_expr(name.clone(), callee_span);
            let body = self.transform_rec_cps_call_args(
                callee_ref,
                args,
                Vec::new(),
                span,
                current_member,
                cont,
                ctx,
            )?;
            let k = rec_cps_lambda(
                name,
                None,
                body,
                ctx,
                callee_span,
                RecCpsApplication::SynthesizedValue,
            );
            self.transform_rec_cps_expr(callee, current_member, k, RecCallPosition::NonTail, ctx)
        } else {
            let bind_before_args =
                args.iter().any(call_arg_contains_rec) && !rec_cps_operand_is_inert(&callee);
            let callee_span = callee.span();
            self.validate_no_bad_rec(&callee, ctx)?;
            let callee = self.walk_expr(callee)?;
            let type_span = callee.span();
            if bind_before_args {
                let name = rec_tmp_name("callee", callee_span, 0);
                let body = self.transform_rec_cps_call_args(
                    path_expr(name.clone(), callee_span),
                    args,
                    Vec::new(),
                    span,
                    current_member,
                    cont,
                    ctx,
                )?;
                return Ok(self.rec_cps_pending_let(
                    name,
                    type_span,
                    RecOrderTypeFlow::SynthesizedValue,
                    callee,
                    body,
                    ctx,
                ));
            }
            self.transform_rec_cps_call_args(
                callee,
                args,
                Vec::new(),
                span,
                current_member,
                cont,
                ctx,
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_rec_cps_call_args(
        &mut self,
        callee: Expr<Desugared>,
        mut args: Vec<CallArg<Surface>>,
        mut out: Vec<CallArg<Desugared>>,
        span: Span,
        current_member: &RecMemberPlan,
        cont: RecCpsContinuation,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        if args.is_empty() {
            let value = Expr::synth_call(callee, out, span);
            return Ok(self.apply_rec_cont(cont, value, span));
        }
        let first = args.remove(0);
        match first {
            CallArg::Type(ty) => {
                out.push(CallArg::Type(self.walk_type(ty)?));
                self.transform_rec_cps_call_args(callee, args, out, span, current_member, cont, ctx)
            }
            CallArg::Value(value) if expr_contains_rec_call(&value) => {
                let name = rec_tmp_name("arg", value.span(), out.len());
                let mut next_out = out;
                next_out.push(CallArg::Value(path_expr(name.clone(), value.span())));
                let body = self.transform_rec_cps_call_args(
                    callee,
                    args,
                    next_out,
                    span,
                    current_member,
                    cont,
                    ctx,
                )?;
                let k = rec_cps_lambda(
                    name,
                    None,
                    body,
                    ctx,
                    span,
                    RecCpsApplication::ExpectedFromBody,
                );
                self.transform_rec_cps_expr(value, current_member, k, RecCallPosition::NonTail, ctx)
            }
            CallArg::Value(value) => {
                let bind_before_rest =
                    args.iter().any(call_arg_contains_rec) && !rec_cps_operand_is_inert(&value);
                let value_span = value.span();
                self.validate_no_bad_rec(&value, ctx)?;
                let value = self.walk_expr(value)?;
                let type_span = value.span();
                if !bind_before_rest {
                    out.push(CallArg::Value(value));
                    return self.transform_rec_cps_call_args(
                        callee,
                        args,
                        out,
                        span,
                        current_member,
                        cont,
                        ctx,
                    );
                }
                let name = rec_tmp_name("arg", value_span, out.len());
                out.push(CallArg::Value(path_expr(name.clone(), value_span)));
                let body = self.transform_rec_cps_call_args(
                    callee,
                    args,
                    out,
                    span,
                    current_member,
                    cont,
                    ctx,
                )?;
                Ok(self.rec_cps_pending_let(
                    name,
                    type_span,
                    RecOrderTypeFlow::ExpectedFromBody,
                    value,
                    body,
                    ctx,
                ))
            }
        }
    }

    fn transform_rec_cps_tuple_items(
        &mut self,
        mut items: Vec<Expr<Surface>>,
        mut out: Vec<Expr<Desugared>>,
        span: Span,
        current_member: &RecMemberPlan,
        cont: RecCpsContinuation,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        if items.is_empty() {
            let value = product_value_from_values_infer(out, span);
            return Ok(self.apply_rec_cont(cont, value, span));
        }
        let first = items.remove(0);
        if expr_contains_rec_call(&first) {
            let name = rec_tmp_name("tuple", first.span(), out.len());
            out.push(path_expr(name.clone(), first.span()));
            let body =
                self.transform_rec_cps_tuple_items(items, out, span, current_member, cont, ctx)?;
            let k = rec_cps_lambda(
                name,
                None,
                body,
                ctx,
                span,
                RecCpsApplication::ExpectedFromBody,
            );
            self.transform_rec_cps_expr(first, current_member, k, RecCallPosition::NonTail, ctx)
        } else {
            let bind_before_rest =
                items.iter().any(expr_contains_rec_call) && !rec_cps_operand_is_inert(&first);
            let first_span = first.span();
            self.validate_no_bad_rec(&first, ctx)?;
            let first = self.walk_expr(first)?;
            let type_span = first.span();
            if !bind_before_rest {
                out.push(first);
                return self.transform_rec_cps_tuple_items(
                    items,
                    out,
                    span,
                    current_member,
                    cont,
                    ctx,
                );
            }
            let name = rec_tmp_name("tuple", first_span, out.len());
            out.push(path_expr(name.clone(), first_span));
            let body =
                self.transform_rec_cps_tuple_items(items, out, span, current_member, cont, ctx)?;
            Ok(self.rec_cps_pending_let(
                name,
                type_span,
                RecOrderTypeFlow::ExpectedFromBody,
                first,
                body,
                ctx,
            ))
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_rec_cps_label_values(
        &mut self,
        mut labels: Vec<LabelValueLabel<Surface>>,
        mut out: Vec<LabelValueLabel<Desugared>>,
        meta: Meta<Surface>,
        ext: NodeId,
        current_member: &RecMemberPlan,
        cont: RecCpsContinuation,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        let tag_span = meta.span;
        if labels.is_empty() {
            let value = Expr::LabelValue {
                occurrence: Default::default(),
                labels: out,
                meta: Meta::new(tag_span),
                ext: self.state.cloned_elaboration_id(ext),
            };
            return Ok(self.apply_rec_cont(cont, value, tag_span));
        }
        let label = labels.remove(0);
        if expr_contains_rec_call(&label.value) {
            let name = rec_tmp_name("label", label.value.span(), out.len());
            out.push(LabelValueLabel {
                label: label.label,
                label_span: label.label_span,
                value: path_expr(name.clone(), label.value.span()),
                meta: Meta::new(label.meta.span),
            });
            let body = self.transform_rec_cps_label_values(
                labels,
                out,
                meta,
                ext,
                current_member,
                cont,
                ctx,
            )?;
            let k = rec_cps_lambda(
                name,
                None,
                body,
                ctx,
                tag_span,
                RecCpsApplication::ExpectedFromBody,
            );
            self.transform_rec_cps_expr(
                label.value,
                current_member,
                k,
                RecCallPosition::NonTail,
                ctx,
            )
        } else {
            let bind_before_rest = labels
                .iter()
                .any(|remaining| expr_contains_rec_call(&remaining.value))
                && !rec_cps_operand_is_inert(&label.value);
            let value_span = label.value.span();
            self.validate_no_bad_rec(&label.value, ctx)?;
            let value = self.walk_expr(label.value)?;
            let type_span = value.span();
            if bind_before_rest {
                let name = rec_tmp_name("label", value_span, out.len());
                out.push(LabelValueLabel {
                    label: label.label,
                    label_span: label.label_span,
                    value: path_expr(name.clone(), value_span),
                    meta: Meta::new(label.meta.span),
                });
                let body = self.transform_rec_cps_label_values(
                    labels,
                    out,
                    meta,
                    ext,
                    current_member,
                    cont,
                    ctx,
                )?;
                return Ok(self.rec_cps_pending_let(
                    name,
                    type_span,
                    RecOrderTypeFlow::ExpectedFromBody,
                    value,
                    body,
                    ctx,
                ));
            }
            out.push(LabelValueLabel {
                label: label.label,
                label_span: label.label_span,
                value,
                meta: Meta::new(label.meta.span),
            });
            self.transform_rec_cps_label_values(labels, out, meta, ext, current_member, cont, ctx)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_rec_call_cps(
        &mut self,
        modes: Vec<RecCallMode>,
        callee: PathSegment,
        args: Vec<CallArg<Surface>>,
        span: Span,
        current_member: &RecMemberPlan,
        cont: RecCpsContinuation,
        position: RecCallPosition,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        validate_rec_escape_absent(&modes, span)?;
        match (position, modes.contains(&RecCallMode::Cont)) {
            (RecCallPosition::Tail, true) => {
                return Err(Error::parse(
                    span,
                    "`rec(cont)` is only needed for recursive calls that are not in tail position",
                )
                .with_help("remove `cont` from this tail-position recursive call"));
            }
            (RecCallPosition::NonTail, false) => {
                return Err(Error::parse(span, "recursive call is not in tail position")
                    .with_help("write `rec(cont) name(...)` for a non-tail recursive call"));
            }
            _ => {}
        }
        let Some(member) = ctx.members.iter().find(|member| member.name == callee.name) else {
            return Err(Error::name_res(
                callee.span,
                format!(
                    "`rec {}` is not a member of this `rec(loop)` group",
                    callee.name
                ),
            ));
        };
        let (type_args, values) = self.rec_call_parts(member, args, span, ctx)?;
        self.validate_rec_poly_mode(member, current_member, &type_args, &modes, span)?;
        self.transform_rec_call_cps_values(
            member,
            type_args,
            values,
            Vec::new(),
            cont,
            position == RecCallPosition::NonTail,
            span,
            current_member,
            ctx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_rec_call_cps_values(
        &mut self,
        member: &RecMemberPlan,
        type_args: Vec<Type<Desugared>>,
        mut values: Vec<Expr<Surface>>,
        mut out: Vec<Expr<Desugared>>,
        cont: RecCpsContinuation,
        schedule_return: bool,
        span: Span,
        current_member: &RecMemberPlan,
        ctx: &RecLoweringCtx,
    ) -> Result<Expr<Desugared>, Error> {
        if values.is_empty() {
            let mut value_tys = instantiate_rec_value_tys(member, &type_args);
            let cont_input_ty = instantiate_rec_ret_ty(member, &type_args);
            out.extend(rec_phantom_witness_values(
                &type_args,
                &member.phantom_witness_indices,
                span,
            ));
            value_tys.extend(rec_phantom_witness_tys(
                &type_args,
                &member.phantom_witness_indices,
                span,
            ));
            value_tys.push(rec_cont_fn_type(&cont_input_ty, &ctx.step_result_ty, span));
            let cont = if schedule_return {
                self.rec_scheduled_cont(cont, &cont_input_ty, span, ctx)
            } else {
                cont.into_expr()
            };
            out.push(cont);
            let payload = product_value_from_values(out, value_tys, span);
            let member_state =
                self.rec_wrap_state(member, ctx.state_type_args.clone(), type_args, payload);
            let state = inject_sum_arm(member_state, &rec_state_arm_types(ctx), member.index, span);
            return Ok(inject_step_continue(
                state,
                &ctx.state_ty,
                &ctx.ret_ty,
                span,
            ));
        }
        let first = values.remove(0);
        let value_ty = instantiate_rec_value_tys(member, &type_args)
            .into_iter()
            .nth(out.len())
            .expect("recursive value position has a declared slot type");
        if expr_contains_rec_call(&first) {
            let first_span = first.span();
            let name = rec_tmp_name("rec_arg", first_span, out.len());
            out.push(path_expr(name.clone(), first_span));
            let body = self.transform_rec_call_cps_values(
                member,
                type_args,
                values,
                out,
                cont,
                schedule_return,
                span,
                current_member,
                ctx,
            )?;
            let k = rec_cps_lambda(
                name,
                Some(value_ty),
                body,
                ctx,
                first_span,
                RecCpsApplication::Ordinary,
            );
            self.transform_rec_cps_expr(first, current_member, k, RecCallPosition::NonTail, ctx)
        } else {
            let bind_before_rest =
                values.iter().any(expr_contains_rec_call) && !rec_cps_operand_is_inert(&first);
            let first_span = first.span();
            self.validate_no_bad_rec(&first, ctx)?;
            let first = self.walk_expr(first)?;
            if !bind_before_rest {
                out.push(first);
                return self.transform_rec_call_cps_values(
                    member,
                    type_args,
                    values,
                    out,
                    cont,
                    schedule_return,
                    span,
                    current_member,
                    ctx,
                );
            }
            let name = rec_tmp_name("rec_arg", first_span, out.len());
            out.push(path_expr(name.clone(), first_span));
            let body = self.transform_rec_call_cps_values(
                member,
                type_args,
                values,
                out,
                cont,
                schedule_return,
                span,
                current_member,
                ctx,
            )?;
            Ok(Expr::Let {
                occurrence: Default::default(),
                name,
                name_span: first_span,
                ty: Some(value_ty),
                pattern: (),
                value: Box::new(first),
                body: Box::new(body),
                meta: Meta::new(first_span),
            })
        }
    }

    fn rec_scheduled_cont(
        &mut self,
        mut cont: RecCpsContinuation,
        input: &Type<Desugared>,
        span: Span,
        ctx: &RecLoweringCtx,
    ) -> Expr<Desugared> {
        let resume = ctx
            .resume
            .as_ref()
            .expect("non-tail calls have a resume state");
        if cont.application == RecCpsApplication::ExpectedFromBody
            && let Expr::FnExpr { sig, .. } = &mut cont.expr
            && let [SignatureParam::Value(param)] = sig.params.as_mut_slice()
            && param.ty.is_none()
        {
            // The recursive handoff already supplies this exact input type;
            // scheduling must preserve it before checking the continuation body.
            param.ty = Some(input.clone());
        }
        let value_name = rec_tmp_name("resume_value", span, 0);
        let body = self.apply_rec_cont(cont, path_expr(value_name.clone(), span), span);
        let thunk = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_groups(vec![crate::ast::SignatureGroup::Value(Vec::new())]),
            ret_ty: Some(ctx.step_result_ty.clone()),
            body: Box::new(body),
            meta: Meta::new(span),
            caps: (),
        };
        let mut args = ctx
            .state_type_args
            .iter()
            .cloned()
            .map(CallArg::Type)
            .collect::<Vec<_>>();
        args.push(CallArg::Value(thunk));
        let state = call_path(
            vec![
                PathSegment::new(resume.name.clone(), span),
                PathSegment::new("mk_resume", span),
            ],
            args,
            span,
        );
        let state = inject_sum_arm(state, &rec_state_arm_types(ctx), ctx.members.len(), span);
        let body = inject_step_continue(state, &ctx.state_ty, &ctx.ret_ty, span);
        rec_cps_lambda(
            value_name,
            Some(input.clone()),
            body,
            ctx,
            span,
            RecCpsApplication::Ordinary,
        )
        .into_expr()
    }

    fn rec_initial_cont(&self, member: &RecMemberPlan, ctx: &RecLoweringCtx) -> Expr<Desugared> {
        let result_name = format!("__rec_result{}_i{}__", ctx.span.start, member.index);
        let result_ref = path_expr(result_name.clone(), member.meta.span);
        Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::new(vec![SignatureParam::Value(Param {
                name: result_name,
                ty: Some(member.wrapper_ret.clone()),
                pattern: (),
                meta: Meta::new(member.meta.span),
            })]),
            ret_ty: Some(ctx.step_result_ty.clone()),
            body: Box::new(inject_step_return(
                result_ref,
                &ctx.state_ty,
                &ctx.ret_ty,
                member.meta.span,
            )),
            meta: member.meta.clone(),
            caps: (),
        }
    }

    fn validate_no_bad_rec(
        &mut self,
        expr: &Expr<Surface>,
        ctx: &RecLoweringCtx,
    ) -> Result<(), Error> {
        self.validate_no_bad_rec_names(expr, &ctx.member_names)
    }

    fn validate_no_bad_rec_names(
        &mut self,
        expr: &Expr<Surface>,
        ctx: &std::collections::HashSet<String>,
    ) -> Result<(), Error> {
        match expr {
            crate::ast::Expr::BlockCall { .. } => {
                unreachable!("trailing blocks are projected before desugaring")
            }
            Expr::RecCall { modes, meta, .. } => {
                validate_rec_escape_absent(modes, meta.span)?;
                if modes.contains(&RecCallMode::Cont) {
                    Ok(())
                } else {
                    Err(
                        Error::parse(meta.span, "recursive call is not in tail position")
                            .with_help("write `rec(cont) name(...)` for a non-tail recursive call"),
                    )
                }
            }
            Expr::Path { segments, meta, .. } => {
                if segments.len() == 1 && self.is_unshadowed_rec_member(&segments[0].name, ctx) {
                    return Err(Error::parse(
                        meta.span,
                        format!(
                            "recursive function `{}` cannot be used as a value; use \
                             an annotated `rec {}(...)` call",
                            segments[0].name, segments[0].name
                        ),
                    ));
                }
                Ok(())
            }
            Expr::Call {
                callee, args, meta, ..
            } => {
                if let Expr::Path { segments, .. } = callee.as_ref()
                    && segments.len() == 1
                    && self.is_unshadowed_rec_member(&segments[0].name, ctx)
                {
                    return Err(Error::parse(
                        meta.span,
                        format!(
                            "recursive call to `{}` must be written as a tail-position \
                             `rec {}(...)` call",
                            segments[0].name, segments[0].name
                        ),
                    ));
                }
                self.validate_no_bad_rec_names(callee, ctx)?;
                for arg in args {
                    if let CallArg::Value(value) = arg {
                        self.validate_no_bad_rec_names(value, ctx)?;
                    }
                }
                Ok(())
            }
            Expr::FnExpr { sig, body, .. } => {
                let mark = self.state.save_bound();
                for p in &sig.params {
                    if let SignatureParam::Value(vp) = p {
                        self.state.bound.push(vp.name.clone());
                    }
                }
                let result = self.validate_no_bad_rec_names(body, ctx);
                self.state.restore_bound(mark);
                result
            }
            Expr::Let {
                name,
                pattern,
                value,
                body,
                ..
            } => {
                self.validate_no_bad_rec_names(value, ctx)?;
                let mark = self.state.save_bound();
                self.state.bound.push(name.clone());
                if let Some(pat) = pattern {
                    append_pattern_bound_names(pat, &mut self.state.bound);
                }
                let result = self.validate_no_bad_rec_names(body, ctx);
                self.state.restore_bound(mark);
                result
            }
            Expr::RowLet {
                entries,
                value,
                body,
                ..
            } => {
                self.validate_no_bad_rec_names(value, ctx)?;
                let mark = self.state.save_bound();
                for entry in entries {
                    self.state.bound.push(entry.local.clone());
                }
                let result = self.validate_no_bad_rec_names(body, ctx);
                self.state.restore_bound(mark);
                result
            }
            Expr::Seq { value, body, .. } => {
                self.validate_no_bad_rec_names(value, ctx)?;
                self.validate_no_bad_rec_names(body, ctx)
            }
            Expr::Elaborator { call, .. } => match call {
                ElaboratorCall::FieldAccess { receiver, .. } => {
                    self.validate_no_bad_rec_names(receiver, ctx)
                }
                ElaboratorCall::FieldUpdate { receiver, updates } => {
                    self.validate_no_bad_rec_names(receiver, ctx)?;
                    for update in updates {
                        self.validate_no_bad_rec_names(&update.value, ctx)?;
                    }
                    Ok(())
                }
            },
            Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
            Expr::UserElaborator { args, .. } => {
                for arg in args {
                    if let CallArg::Value(value) = arg {
                        self.validate_no_bad_rec_names(value, ctx)?;
                    }
                }
                Ok(())
            }
            Expr::LabelValue { labels, .. } => {
                for label in labels {
                    self.validate_no_bad_rec_names(&label.value, ctx)?;
                }
                Ok(())
            }
            Expr::Tuple { items, .. } => {
                for item in items {
                    self.validate_no_bad_rec_names(item, ctx)?;
                }
                Ok(())
            }
            Expr::FnPlaceholder { body, .. } => self.validate_no_bad_rec_names(body, ctx),
            Expr::Ufcs { receiver, args, .. } => {
                self.validate_no_bad_rec_names(receiver, ctx)?;
                for arg in args {
                    if let CallArg::Value(value) = arg {
                        self.validate_no_bad_rec_names(value, ctx)?;
                    }
                }
                Ok(())
            }
            Expr::OpChain { kind, .. } => {
                match kind {
                    crate::ast::OpChainKind::Normal { slots, .. } => {
                        for slot in slots {
                            self.validate_no_bad_rec_names(slot, ctx)?;
                        }
                    }
                    crate::ast::OpChainKind::Variadic { elements, .. } => {
                        for element in elements {
                            self.validate_no_bad_rec_names(element, ctx)?;
                        }
                    }
                }
                Ok(())
            }
            Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::BoolLit { .. } => Ok(()),
            Expr::EnrichedTuple { ext, .. }
            | Expr::EnrichedProject { ext, .. }
            | Expr::EnrichedInject { ext, .. }
            | Expr::EnrichedMatch { ext, .. }
            | Expr::EnrichedConditional { ext, .. }
            | Expr::EnrichedRecord { ext, .. }
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

    fn is_unshadowed_rec_member(
        &self,
        name: &str,
        ctx: &std::collections::HashSet<String>,
    ) -> bool {
        ctx.contains(name) && !self.state.is_bound(name)
    }

    fn bind_signature_type_params(&mut self, params: &[SignatureParam<Surface>]) {
        for param in params {
            if let SignatureParam::Type(tp) = param {
                self.state.type_bound.push(tp.name.clone());
            }
        }
    }

    fn expand_literal(
        &mut self,
        def: &LiteralAliasDef,
        annotation: Option<Type<Surface>>,
        use_span: Span,
    ) -> Result<Expr<Desugared>, Error> {
        let mut expr = def.value.to_expr::<Surface>();
        match &mut expr {
            Expr::StrLit {
                annotation: a,
                meta,
                ..
            }
            | Expr::IntLit {
                annotation: a,
                meta,
                ..
            }
            | Expr::FloatLit {
                annotation: a,
                meta,
                ..
            }
            | Expr::BoolLit {
                annotation: a,
                meta,
                ..
            } => {
                *a = annotation;
                *meta = Meta::new(use_span);
            }
            _ => unreachable!("LiteralAliasValue::to_expr returned a non-literal expression"),
        }
        self.walk_expr(expr)
    }

    fn literal_call_annotation(
        &mut self,
        name: &str,
        args: Vec<CallArg<Surface>>,
        use_span: Span,
    ) -> Result<Type<Surface>, Error> {
        match args.as_slice() {
            [arg] => crate::pass::typecheck_core::call_arg_to_type_arg(arg).map_err(|_| {
                Error::name_res(
                    use_span,
                    format!("literal `{name}` accepts only a type annotation argument"),
                )
            }),
            _ => Err(Error::name_res(
                use_span,
                format!("literal `{name}` accepts exactly one type annotation argument"),
            )),
        }
    }
}

impl SurfaceToDesugared for DesugarVisitor {
    // These visit_* methods override the trait defaults. Each
    // re-destructures its variant with a let-else; the walk_*
    // dispatcher routes each variant to its own visit_*, so that
    // let-else can fail only if a dispatcher routed the wrong variant —
    // a bug in this file, hence the bare `unreachable!()`.
    //
    // ---- Surface forms rewritten here -------------------------------

    fn visit_type_path(&mut self, ty: Type<Surface>) -> Result<Type<Desugared>, Error> {
        let Type::Path {
            segments,
            args,
            meta: Meta { span, .. },
        } = ty
        else {
            unreachable!()
        };
        let args: Vec<Type<Desugared>> = args
            .into_iter()
            .map(|a| self.walk_type(a))
            .collect::<Result<_, _>>()?;
        Ok(Type::synth_path_segments(segments, args, span))
    }

    fn visit_type_forall(&mut self, ty: Type<Surface>) -> Result<Type<Desugared>, Error> {
        let Type::Forall {
            param,
            body,
            meta: Meta { span, .. },
        } = ty
        else {
            unreachable!()
        };
        let mark = self.state.save_type_bound();
        self.state.type_bound.push(param.name.clone());
        let body = self.walk_type(*body)?;
        self.state.restore_type_bound(mark);
        Ok(Type::Forall {
            param,
            body: Box::new(body),
            meta: Meta::new(span),
        })
    }

    fn visit_type_infer(&mut self, ty: Type<Surface>) -> Result<Type<Desugared>, Error> {
        let Type::Infer {
            meta: Meta { span, .. },
            ext,
        } = ty
        else {
            unreachable!()
        };
        self.state.source_type_infer_observations = self
            .state
            .source_type_infer_observations
            .checked_add(1)
            .expect("one module's source-placeholder count fits in usize");
        Ok(Type::Infer {
            meta: Meta::new(span),
            ext,
        })
    }

    fn rewrite_expr_tuple(
        &mut self,
        items: Vec<Expr<Surface>>,
        _meta: Meta<Surface>,
    ) -> Result<Expr<Desugared>, Error> {
        let items: Vec<Expr<Desugared>> = items
            .into_iter()
            .map(|i| self.walk_expr(i))
            .collect::<Result<_, _>>()?;
        Ok(self.fold_desugared_tuple(items))
    }

    fn rewrite_expr_fn_placeholder(
        &mut self,
        mut body: Box<Expr<Surface>>,
        stem: PathSegment,
        mut state: PlaceholderState,
        meta: Meta<Surface>,
    ) -> Result<Expr<Desugared>, Error> {
        let slot_count =
            crate::pass::placeholder::prepare(&stem, &mut state, &mut body, meta.span)?;
        // Allocate names after nested sugar has introduced its own bindings.
        let body = self.walk_expr(*body)?;
        Ok(lower_fn_placeholder(body, slot_count, stem.span, meta))
    }

    fn rewrite_item_op(&mut self, _d: Op<Surface>) -> Result<Vec<Item<Desugared>>, Error> {
        // `op` items are consumed at this boundary: the parser
        // already used them to fold operator usages into plain
        // `Expr::Call` nodes.
        Ok(vec![])
    }

    fn rewrite_item_fold(
        &mut self,
        _d: VariadicOperator<Surface>,
    ) -> Result<Vec<Item<Desugared>>, Error> {
        Ok(vec![])
    }

    fn rewrite_item_rec_group(
        &mut self,
        d: RecGroup<Surface>,
    ) -> Result<Vec<Item<Desugared>>, Error> {
        self.lower_rec_group(d)
    }

    fn rewrite_expr_rec_call(
        &mut self,
        _modes: Vec<RecCallMode>,
        _callee: PathSegment,
        _args: Vec<CallArg<Surface>>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Desugared>, Error> {
        Err(Error::parse(
            meta.span,
            "`rec` calls are only allowed inside a `rec(loop)` function",
        ))
    }

    fn rewrite_expr_op_chain(
        &mut self,
        _kind: crate::ast::OpChainKind<Surface>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Desugared>, Error> {
        // OpChain placeholders must be resolved before desugar
        // runs — the operator-fold pass, itself before ordinary name
        // resolution, converts them to plain `Expr::Call`. An
        // OpChain reaching here means the fold pass didn't run,
        // which is an internal-bug.
        unreachable!(
            "OpChain at span {:?} reached desugar — operator-fold pass didn't run",
            meta.span
        )
    }

    fn rewrite_item_literal_alias(
        &mut self,
        _a: LiteralAlias<Surface>,
    ) -> Result<Vec<Item<Desugared>>, Error> {
        Ok(vec![])
    }

    // ---- Per-variant overrides --------------------------------------

    fn visit_expr_path(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
        let Expr::Path {
            occurrence: _,
            segments,
            meta: Meta { span, .. },
            ext: (),
        } = e
        else {
            unreachable!()
        };
        if segments.len() == 1
            && !self.state.is_bound(segments[0].as_str())
            && let Some(def) = self
                .state
                .literal_aliases
                .get(segments[0].as_str())
                .cloned()
        {
            return self.expand_literal(&def, None, span);
        }
        Ok(Expr::Path {
            occurrence: Default::default(),
            segments,
            meta: Meta::new(span),
            ext: (),
        })
    }

    fn visit_expr_call(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
        let Expr::Call {
            occurrence: _,
            callee,
            args,
            meta: Meta { span, .. },
            ext: (),
        } = e
        else {
            unreachable!()
        };
        if let Expr::Path { segments, .. } = callee.as_ref()
            && segments.len() == 1
            && generated_projection_intrinsic(&segments[0].name)
        {
            self.state.needs_intrinsics = true;
        }
        if let Expr::Path { segments, .. } = callee.as_ref()
            && segments.len() == 1
            && !self.state.is_bound(segments[0].as_str())
            && let Some(def) = self
                .state
                .literal_aliases
                .get(segments[0].as_str())
                .cloned()
        {
            let Expr::Path { segments, .. } = *callee else {
                unreachable!()
            };
            let annotation = self.literal_call_annotation(&segments[0], args, span)?;
            return self.expand_literal(&def, Some(annotation), span);
        }
        let callee = self.walk_expr(*callee)?;
        let args: Vec<CallArg<Desugared>> = args
            .into_iter()
            .map(|a| self.walk_call_arg(a))
            .collect::<Result<_, _>>()?;
        Ok(Expr::synth_call(callee, args, span))
    }

    fn visit_expr_fn(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
        let Expr::FnExpr {
            occurrence: _,
            sig,
            ret_ty,
            body,
            meta: Meta { span, .. },
            caps: _,
        } = e
        else {
            unreachable!()
        };
        // Pattern params first: rewrite the signature and inject
        // projection bindings into the body before the regular walk.
        if signature_has_pattern_params(&sig.params) {
            self.state.needs_intrinsics = true;
        }
        let groups = sig.groups;
        let (sig_params, body_expr) = expand_pattern_params(sig.params, *body, ret_ty.clone());
        let type_mark = self.state.save_type_bound();
        self.bind_signature_type_params(&sig_params);
        let params: Vec<SignatureParam<Desugared>> = sig_params
            .into_iter()
            .map(|p| self.walk_signature_param(p))
            .collect::<Result<_, _>>()?;
        let ret_ty = ret_ty.map(|t| self.walk_type(t)).transpose()?;
        let mark = self.state.save_bound();
        for p in &params {
            if let SignatureParam::Value(vp) = p {
                self.state.bound.push(vp.name.clone());
            }
        }
        let body = self.walk_expr(body_expr)?;
        self.state.restore_bound(mark);
        self.state.restore_type_bound(type_mark);
        Ok(Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(params, groups),
            ret_ty,
            body: Box::new(body),
            meta: Meta::new(span),
            caps: (),
        })
    }

    fn visit_expr_let(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
        let Expr::Let {
            occurrence: _,
            name,
            name_span,
            ty,
            pattern,
            value,
            body,
            meta: Meta { span, .. },
        } = e
        else {
            unreachable!()
        };
        if let Some(pat) = pattern {
            self.state.needs_intrinsics = true;
            let pat_span = pat.span;
            let outer_ty = source_type_from_pattern(&pat);
            let clause_params = pattern_elems_to_clause_params(pat.elems);
            let body = build_pattern_destructure_wrap(
                surface_path_expr(name.clone(), pat_span),
                clause_params,
                *body,
                pat_span,
            );
            return self.walk_expr(Expr::Let {
                occurrence: Default::default(),
                name,
                name_span,
                ty: Some(outer_ty),
                pattern: None,
                value,
                body: Box::new(body),
                meta: Meta::new(span),
            });
        }
        // The RHS is in the enclosing scope (the new binding isn't
        // visible to itself); only `body` sees `name` as bound.
        let value = self.walk_expr(*value)?;
        let ty = ty.map(|t| self.walk_type(t)).transpose()?;
        let mark = self.state.save_bound();
        self.state.bound.push(name.clone());
        let body = self.walk_expr(*body)?;
        self.state.restore_bound(mark);
        Ok(Expr::Let {
            occurrence: Default::default(),
            name,
            name_span,
            ty,
            pattern: (),
            value: Box::new(value),
            body: Box::new(body),
            meta: Meta::new(span),
        })
    }

    fn rewrite_expr_row_let(
        &mut self,
        entries: Vec<RowLetEntry<Surface>>,
        value: Box<Expr<Surface>>,
        body: Box<Expr<Surface>>,
        temp_name: String,
        meta: Meta<Surface>,
    ) -> Result<Expr<Desugared>, Error> {
        self.state.needs_intrinsics = true;
        self.walk_expr(expand_row_let(entries, value, body, temp_name, meta))
    }

    fn visit_expr_label_value(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
        let Expr::LabelValue {
            occurrence: _,
            labels,
            meta: Meta { span, .. },
            ext,
        } = e
        else {
            unreachable!()
        };
        // Multi-label values lower to `__pair__` chains
        // during label_elab, so flag the intrinsic import here.
        if labels.len() >= 2 {
            self.state.needs_intrinsics = true;
        }
        let ext = self.state.cloned_elaboration_id(ext);
        let labels: Vec<LabelValueLabel<Desugared>> = labels
            .into_iter()
            .map(|l| {
                let LabelValueLabel {
                    label,
                    label_span,
                    value,
                    meta: Meta { span: lspan, .. },
                } = l;
                Ok(LabelValueLabel {
                    label,
                    label_span,
                    value: self.walk_expr(value)?,
                    meta: Meta::new(lspan),
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Expr::LabelValue {
            occurrence: Default::default(),
            labels,
            meta: Meta::new(span),
            ext,
        })
    }

    // Elaborator / `if`/`else` survive into Lowered but their
    // typer-time elaboration emits intrinsic call sites. Flag the
    // intrinsic import conservatively so the post-substitute Kio'
    // source carries `import __intrinsics__;` and round-trips through
    // re-parse. The overrides duplicate the trait's default
    // clone-and-recurse body to preserve the variant's fields;
    // calling `super::trait::visit_*` from an override isn't
    // expressible in Rust without specialization, so inline.

    fn visit_expr_elaborator(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
        self.state.needs_intrinsics = true;
        let Expr::Elaborator {
            occurrence: _,
            kind,
            call,
            meta: Meta { span, .. },
            ext,
        } = e
        else {
            unreachable!()
        };
        let call = match call {
            ElaboratorCall::FieldAccess { receiver, labels } => ElaboratorCall::FieldAccess {
                receiver: Box::new(self.walk_expr(*receiver)?),
                labels: labels
                    .into_iter()
                    .map(|label| {
                        let FieldAccessLabel {
                            label,
                            label_span,
                            label_type,
                            meta: Meta { span, .. },
                        } = label;
                        Ok(FieldAccessLabel {
                            label,
                            label_span,
                            label_type,
                            meta: Meta::new(span),
                        })
                    })
                    .collect::<Result<_, Error>>()?,
            },
            ElaboratorCall::FieldUpdate { receiver, updates } => ElaboratorCall::FieldUpdate {
                receiver: Box::new(self.walk_expr(*receiver)?),
                updates: updates
                    .into_iter()
                    .map(|update| {
                        let FieldUpdateLabel {
                            label,
                            label_span,
                            label_type,
                            value,
                            meta: Meta { span, .. },
                        } = update;
                        Ok(FieldUpdateLabel {
                            label,
                            label_span,
                            label_type,
                            value: self.walk_expr(value)?,
                            meta: Meta::new(span),
                        })
                    })
                    .collect::<Result<_, Error>>()?,
            },
        };
        Ok(Expr::Elaborator {
            occurrence: Default::default(),
            kind,
            call,
            meta: Meta::new(span),
            ext: self.state.cloned_elaboration_id(ext),
        })
    }

    fn visit_expr_user_elaborator(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
        self.state.needs_intrinsics = true;
        let Expr::UserElaborator {
            occurrence: _,
            name,
            form,
            args,
            meta: Meta { span, .. },
            ext,
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::UserElaborator {
            occurrence: Default::default(),
            name,
            form,
            args: args
                .into_iter()
                .map(|arg| self.walk_call_arg(arg))
                .collect::<Result<_, _>>()?,
            meta: Meta::new(span),
            ext: self.state.cloned_elaboration_id(ext),
        })
    }

    fn visit_expr_ufcs(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
        let Expr::Ufcs {
            occurrence: _,
            receiver,
            callee_segments,
            callee_span,
            args,
            flavor,
            bang,
            meta: Meta { span, .. },
            ext,
        } = e
        else {
            unreachable!()
        };
        if bang.is_some() {
            self.state.needs_intrinsics = true;
        }
        Ok(Expr::Ufcs {
            occurrence: Default::default(),
            receiver: Box::new(self.walk_expr(*receiver)?),
            callee_segments,
            callee_span,
            args: args
                .into_iter()
                .map(|arg| self.walk_call_arg(arg))
                .collect::<Result<_, _>>()?,
            flavor,
            bang,
            meta: Meta::new(span),
            ext: self.state.cloned_elaboration_id(ext),
        })
    }

    // ---- Top-level binder scope-management overrides ----------------

    fn walk_fn_def(&mut self, d: FnDef<Surface>) -> Result<FnDef<Desugared>, Error> {
        let FnDef {
            vis,
            purity,
            name,
            sig,
            ret,
            ret_elided,
            body,
            meta: Meta { span, .. },
            doc,
        } = d;
        // Pattern params first: rewrite the signature and inject
        // projection bindings into the body before the regular walk.
        if signature_has_pattern_params(&sig.params) {
            self.state.needs_intrinsics = true;
        }
        let groups = sig.groups;
        let (sig_params, body) = expand_pattern_params(sig.params, body, Some(ret.clone()));
        let type_mark = self.state.save_type_bound();
        self.bind_signature_type_params(&sig_params);
        let params: Vec<SignatureParam<Desugared>> = sig_params
            .into_iter()
            .map(|p| self.walk_signature_param(p))
            .collect::<Result<_, _>>()?;
        let ret = self.walk_type(ret)?;
        let mark = self.state.save_bound();
        for p in &params {
            if let SignatureParam::Value(vp) = p {
                self.state.bound.push(vp.name.clone());
            }
        }
        let body = self.walk_expr(body)?;
        self.state.restore_bound(mark);
        self.state.restore_type_bound(type_mark);
        Ok(FnDef {
            vis,
            purity,
            name,
            sig: Signature::from_parts(params, groups),
            ret,
            ret_elided,
            body,
            meta: Meta::new(span),
            doc,
        })
    }

    fn walk_equiv(&mut self, e: Equiv<Surface>) -> Result<Equiv<Desugared>, Error> {
        let Equiv {
            name,
            name_span,
            sig,
            terms,
            meta: Meta { span, .. },
        } = e;
        // Pattern params: each arm gets the same set of projection
        // bindings.
        if signature_has_pattern_params(&sig.params) {
            self.state.needs_intrinsics = true;
        }
        let (arm_meta_spans, arm_bodies): (Vec<Span>, Vec<Expr<Surface>>) = terms
            .into_iter()
            .map(|t| {
                let crate::ast::EquivTerm {
                    body,
                    meta: Meta { span: tspan, .. },
                } = t;
                (tspan, body)
            })
            .unzip();
        let groups = sig.groups;
        let (sig_params, arm_bodies) =
            expand_pattern_params_for_arms(sig.params, arm_bodies, &mut self.state);
        let type_mark = self.state.save_type_bound();
        self.bind_signature_type_params(&sig_params);
        let params: Vec<SignatureParam<Desugared>> = sig_params
            .into_iter()
            .map(|p| self.walk_signature_param(p))
            .collect::<Result<_, _>>()?;
        let mark = self.state.save_bound();
        for p in &params {
            if let SignatureParam::Value(vp) = p {
                self.state.bound.push(vp.name.clone());
            }
        }
        let terms: Vec<crate::ast::EquivTerm<Desugared>> = arm_bodies
            .into_iter()
            .zip(arm_meta_spans)
            .map(|(body, tspan)| {
                Ok::<_, Error>(crate::ast::EquivTerm {
                    body: self.walk_expr(body)?,
                    meta: Meta::new(tspan),
                })
            })
            .collect::<Result<_, _>>()?;
        self.state.restore_bound(mark);
        self.state.restore_type_bound(type_mark);
        Ok(Equiv {
            name,
            name_span,
            sig: Signature::from_parts(params, groups),
            terms,
            meta: Meta::new(span),
        })
    }
}

// ---- Helpers ----------------------------------------------------------

fn uses_intrinsics(imports: &[Import]) -> bool {
    imports
        .iter()
        .any(|u| matches!(u.kind, ImportKind::Intrinsics))
}

fn sum_type(left: Type<Desugared>, right: Type<Desugared>, span: Span) -> Type<Desugared> {
    Type::Sum {
        left: Box::new(left),
        right: Box::new(right),
        meta: Meta::new(span),
    }
}

fn build_sum_right_fold(mut arms: Vec<Type<Desugared>>, span: Span) -> Type<Desugared> {
    if arms.is_empty() {
        return Type::Bottom {
            meta: Meta::new(span),
        };
    }
    if arms.len() == 1 {
        return arms.pop().expect("len > 0");
    }
    let last = arms.pop().expect("len > 1");
    let mut acc = last;
    while let Some(prev) = arms.pop() {
        acc = sum_type(prev, acc, span);
    }
    acc
}

fn type_param_paths(params: &[TypeParam], span: Span) -> Vec<Type<Desugared>> {
    params
        .iter()
        .map(|tp| Type::synth_path(vec![tp.name.clone()], Vec::new(), tp.span))
        .map(|ty| with_type_span(ty, span))
        .collect()
}

fn rec_generated_span(base: Span, offset: u32) -> Span {
    let pos = base.end.saturating_add(offset);
    Span::new(pos, pos)
}

fn instantiate_rec_value_tys(
    member: &RecMemberPlan,
    type_args: &[Type<Desugared>],
) -> Vec<Type<Desugared>> {
    debug_assert_eq!(member.type_params.len(), type_args.len());
    let subst: std::collections::HashMap<String, Type<Desugared>> = member
        .type_params
        .iter()
        .zip(type_args.iter())
        .map(|(param, arg)| (param.name.clone(), arg.clone()))
        .collect();
    member
        .value_params
        .iter()
        .map(|p| {
            subst_rec_type(
                p.ty.as_ref()
                    .expect("rec value params were validated to have types"),
                &subst,
            )
        })
        .collect()
}

fn instantiate_rec_ret_ty(
    member: &RecMemberPlan,
    type_args: &[Type<Desugared>],
) -> Type<Desugared> {
    debug_assert_eq!(member.type_params.len(), type_args.len());
    let subst: std::collections::HashMap<String, Type<Desugared>> = member
        .type_params
        .iter()
        .zip(type_args.iter())
        .map(|(param, arg)| (param.name.clone(), arg.clone()))
        .collect();
    subst_rec_type(&member.ret, &subst)
}

fn collect_all_type_names<P>(ty: &Type<P>, names: &mut std::collections::HashSet<String>)
where
    P: crate::ast::Phase<TypeGoal = crate::ast::Never>,
{
    match ty {
        Type::Path { segments, args, .. } => {
            if segments.len() == 1 {
                names.insert(segments[0].name.clone());
            }
            for arg in args {
                collect_all_type_names(arg, names);
            }
        }
        Type::Function { param, ret, .. } => {
            collect_all_type_names(param, names);
            collect_all_type_names(ret, names);
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            collect_all_type_names(left, names);
            collect_all_type_names(right, names);
        }
        Type::Forall { param, body, .. } => {
            names.insert(param.name.clone());
            collect_all_type_names(body, names);
        }
        Type::LabelSugar { labels, .. } => {
            for label in labels {
                if let Some(payload) = &label.payload {
                    collect_all_type_names(payload, names);
                }
            }
        }
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => {}
        Type::Goal { ext, .. } => match *ext {},
    }
}

fn collect_free_rec_type_names(
    ty: &Type<Desugared>,
    names: &mut std::collections::HashSet<String>,
) {
    match ty {
        Type::Forall { param, body, .. } => {
            let mut inner = std::collections::HashSet::new();
            collect_free_rec_type_names(body, &mut inner);
            inner.remove(&param.name);
            names.extend(inner);
        }
        Type::Path { segments, args, .. } => {
            if segments.len() == 1 {
                names.insert(segments[0].name.clone());
            }
            for arg in args {
                collect_free_rec_type_names(arg, names);
            }
        }
        Type::Function { param, ret, .. } => {
            collect_free_rec_type_names(param, names);
            collect_free_rec_type_names(ret, names);
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            collect_free_rec_type_names(left, names);
            collect_free_rec_type_names(right, names);
        }
        Type::LabelSugar { labels, .. } => {
            for label in labels {
                if let Some(payload) = &label.payload {
                    collect_free_rec_type_names(payload, names);
                }
            }
        }
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => {}
        Type::Goal { ext, .. } => match *ext {},
    }
}

fn subst_rec_type(
    ty: &Type<Desugared>,
    subst: &std::collections::HashMap<String, Type<Desugared>>,
) -> Type<Desugared> {
    match ty {
        Type::Path {
            segments,
            args,
            meta,
        } => {
            let subst_args = args
                .iter()
                .map(|arg| subst_rec_type(arg, subst))
                .collect::<Vec<_>>();
            if segments.len() == 1
                && let Some(replacement) = subst.get(segments[0].as_str())
            {
                if subst_args.is_empty() {
                    return with_type_span(replacement.clone(), meta.span);
                }
                if let Type::Path {
                    segments,
                    args: replacement_args,
                    ..
                } = replacement
                {
                    let mut args = replacement_args.clone();
                    args.extend(subst_args);
                    return Type::synth_path_segments(segments.clone(), args, meta.span);
                }
            }
            Type::Path {
                segments: segments.clone(),
                args: subst_args,
                meta: meta.clone(),
            }
        }
        Type::Function {
            param,
            ret,
            meta,
            abi_arity,
            ..
        } => {
            let param = subst_rec_type(param, subst);
            Type::Function {
                param: Box::new(param),
                ret: Box::new(subst_rec_type(ret, subst)),
                abi_arity: *abi_arity,
                meta: meta.clone(),
                caps: (),
            }
        }
        Type::Product { left, right, meta } => Type::Product {
            left: Box::new(subst_rec_type(left, subst)),
            right: Box::new(subst_rec_type(right, subst)),
            meta: meta.clone(),
        },
        Type::Sum { left, right, meta } => Type::Sum {
            left: Box::new(subst_rec_type(left, subst)),
            right: Box::new(subst_rec_type(right, subst)),
            meta: meta.clone(),
        },
        Type::Forall { param, body, meta } => {
            let mut scoped = subst.clone();
            scoped.remove(&param.name);
            let mut replacement_free = std::collections::HashSet::new();
            for replacement in scoped.values() {
                collect_free_rec_type_names(replacement, &mut replacement_free);
            }
            let param = if replacement_free.contains(&param.name) {
                let mut taken = replacement_free;
                collect_all_type_names(body, &mut taken);
                taken.extend(scoped.keys().cloned());
                let fresh = crate::pass::typecheck_core::fresh_type_var(&param.name, &taken);
                scoped.insert(
                    param.name.clone(),
                    Type::synth_path(vec![fresh.clone()], Vec::new(), param.span),
                );
                TypeParam {
                    name: fresh,
                    span: param.span,
                    kind: param.kind.clone(),
                }
            } else {
                param.clone()
            };
            Type::Forall {
                param,
                body: Box::new(subst_rec_type(body, &scoped)),
                meta: meta.clone(),
            }
        }
        Type::LabelSugar { labels, meta, ext } => Type::LabelSugar {
            labels: labels
                .iter()
                .map(|label| {
                    let mut label = label.clone();
                    label.payload = label
                        .payload
                        .as_ref()
                        .map(|payload| subst_rec_type(payload, subst));
                    label
                })
                .collect(),
            meta: meta.clone(),
            ext: *ext,
        },
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => ty.clone(),
        Type::Goal { ext, .. } => match *ext {},
    }
}

fn with_type_span(mut ty: Type<Desugared>, span: Span) -> Type<Desugared> {
    ty.meta_mut().span = span;
    ty
}

fn rec_type_args_are_current(
    current_type_params: &[TypeParam],
    type_args: &[Type<Desugared>],
    span: Span,
) -> bool {
    let current_args = type_param_paths(current_type_params, span);
    current_args.len() == type_args.len()
        && current_args
            .iter()
            .zip(type_args)
            .all(|(current, actual)| rec_type_shape_eq(current, actual))
}

fn rec_type_shape_eq(a: &Type<Desugared>, b: &Type<Desugared>) -> bool {
    match (a, b) {
        (
            Type::Path {
                segments: asg,
                args: aa,
                ..
            },
            Type::Path {
                segments: bsg,
                args: ba,
                ..
            },
        ) => {
            asg.iter()
                .map(|s| s.as_str())
                .eq(bsg.iter().map(|s| s.as_str()))
                && aa.len() == ba.len()
                && aa.iter().zip(ba).all(|(a, b)| rec_type_shape_eq(a, b))
        }
        (Type::Unit { .. }, Type::Unit { .. }) => true,
        (Type::Bottom { .. }, Type::Bottom { .. }) => true,
        (
            Type::Function {
                param: ap, ret: ar, ..
            },
            Type::Function {
                param: bp, ret: br, ..
            },
        ) => rec_type_shape_eq(ap, bp) && rec_type_shape_eq(ar, br),
        (
            Type::Product {
                left: al,
                right: ar,
                ..
            },
            Type::Product {
                left: bl,
                right: br,
                ..
            },
        )
        | (
            Type::Sum {
                left: al,
                right: ar,
                ..
            },
            Type::Sum {
                left: bl,
                right: br,
                ..
            },
        ) => rec_type_shape_eq(al, bl) && rec_type_shape_eq(ar, br),
        (
            Type::Forall {
                param: ap,
                body: ab,
                ..
            },
            Type::Forall {
                param: bp,
                body: bb,
                ..
            },
        ) => ap.name == bp.name && ap.kind == bp.kind && rec_type_shape_eq(ab, bb),
        (Type::Infer { .. }, Type::Infer { .. }) => true,
        _ => false,
    }
}

fn rec_phantom_witness_indices(
    type_params: &[TypeParam],
    existential_indices: &[usize],
    payload_tys: &[Type<Desugared>],
) -> Vec<usize> {
    existential_indices
        .iter()
        .copied()
        .filter(|index| {
            let tp = &type_params[*index];
            !payload_tys
                .iter()
                .any(|ty| rec_type_mentions_param(ty, &tp.name))
        })
        .collect()
}

fn rec_type_mentions_param(ty: &Type<Desugared>, name: &str) -> bool {
    match ty {
        Type::Path { segments, args, .. } => {
            (segments.len() == 1 && segments[0].as_str() == name)
                || args.iter().any(|arg| rec_type_mentions_param(arg, name))
        }
        Type::Function { param, ret, .. } => {
            rec_type_mentions_param(param, name) || rec_type_mentions_param(ret, name)
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            rec_type_mentions_param(left, name) || rec_type_mentions_param(right, name)
        }
        Type::Forall { param, body, .. } => {
            param.name != name && rec_type_mentions_param(body, name)
        }
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => false,
        Type::Goal { ext, .. } => match *ext {},
        Type::LabelSugar { labels, .. } => labels.iter().any(|label| {
            label
                .payload
                .as_ref()
                .is_some_and(|payload| rec_type_mentions_param(payload, name))
        }),
    }
}

fn rec_phantom_witness_tys(
    type_args: &[Type<Desugared>],
    indices: &[usize],
    span: Span,
) -> Vec<Type<Desugared>> {
    indices
        .iter()
        .map(|index| {
            let ty = with_type_span(type_args[*index].clone(), span);
            rec_phantom_witness_ty(&ty, span)
        })
        .collect()
}

fn rec_phantom_witness_values(
    type_args: &[Type<Desugared>],
    indices: &[usize],
    span: Span,
) -> Vec<Expr<Desugared>> {
    indices
        .iter()
        .enumerate()
        .map(|(offset, index)| {
            let ty = with_type_span(type_args[*index].clone(), span);
            rec_phantom_witness_value(ty, span, offset)
        })
        .collect()
}

fn rec_phantom_witness_params(member: &RecMemberPlan) -> Vec<Param<Desugared>> {
    rec_phantom_witness_tys(
        &type_param_paths(&member.type_params, member.meta.span),
        &member.phantom_witness_indices,
        member.meta.span,
    )
    .into_iter()
    .enumerate()
    .map(|(index, ty)| Param {
        name: format!("__rec_witness{}_i{}__", member.meta.span.start, index),
        ty: Some(ty),
        pattern: (),
        meta: Meta::new(member.meta.span),
    })
    .collect()
}

fn rec_phantom_witness_ty(ty: &Type<Desugared>, span: Span) -> Type<Desugared> {
    sum_type(ty.clone(), unit_type(span), span)
}

fn rec_phantom_witness_value(ty: Type<Desugared>, span: Span, _index: usize) -> Expr<Desugared> {
    inject_sum_arm(unit_expr(span), &[ty, unit_type(span)], 1, span)
}

fn validate_rec_escape_absent(modes: &[RecCallMode], span: Span) -> Result<(), Error> {
    if modes.contains(&RecCallMode::Escape) {
        return Err(rec_escape_error(span));
    }
    Ok(())
}

pub(crate) fn rec_escape_error(span: Span) -> Error {
    Error::parse(
        span,
        "`rec(escape)` is reserved for recursive calls captured by escaping continuations",
    )
    .with_help(
        "restructure the recursion so the recursive call is not captured by an escaping closure",
    )
}

fn rec_cont_fn_type(
    input: &Type<Desugared>,
    step_result: &Type<Desugared>,
    span: Span,
) -> Type<Desugared> {
    Type::synth_function(
        vec![with_type_span(input.clone(), span)],
        step_result.clone(),
        span,
    )
}

fn rec_current_cont_name(span: Span, member_index: usize) -> String {
    format!("__rec_cont{}_i{}__", span.start, member_index)
}

fn rec_tmp_name(kind: &str, span: Span, index: usize) -> String {
    format!("__rec_{}_s{}_i{}__", kind, span.start, index)
}

fn rec_cps_lambda(
    name: String,
    ty: Option<Type<Desugared>>,
    body: Expr<Desugared>,
    ctx: &RecLoweringCtx,
    span: Span,
    application: RecCpsApplication,
) -> RecCpsContinuation {
    RecCpsContinuation {
        expr: Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::new(vec![SignatureParam::Value(Param {
                name,
                ty,
                pattern: (),
                meta: Meta::new(span),
            })]),
            ret_ty: Some(ctx.step_result_ty.clone()),
            body: Box::new(body),
            meta: Meta::new(span),
            caps: (),
        },
        application,
    }
}

fn path_expr(name: impl Into<String>, span: Span) -> Expr<Desugared> {
    Expr::Path {
        occurrence: Default::default(),
        segments: vec![PathSegment::new(name.into(), span)],
        meta: Meta::new(span),
        ext: (),
    }
}

fn surface_path_expr(name: impl Into<String>, span: Span) -> Expr<Surface> {
    Expr::Path {
        occurrence: Default::default(),
        segments: vec![PathSegment::new(name.into(), span)],
        meta: Meta::new(span),
        ext: (),
    }
}

fn path_expr_segments(segments: Vec<PathSegment>, span: Span) -> Expr<Desugared> {
    Expr::Path {
        occurrence: Default::default(),
        segments,
        meta: Meta::new(span),
        ext: (),
    }
}

fn unit_expr(span: Span) -> Expr<Desugared> {
    Expr::Unit {
        occurrence: Default::default(),
        meta: Meta::new(span),
    }
}

fn unit_type(span: Span) -> Type<Desugared> {
    Type::Unit {
        meta: Meta::new(span),
    }
}

fn call_path(
    segments: Vec<PathSegment>,
    args: Vec<CallArg<Desugared>>,
    span: Span,
) -> Expr<Desugared> {
    Expr::synth_call(path_expr_segments(segments, span), args, span)
}

fn call_intrinsic_with_type_args(
    name: &str,
    type_args: Vec<Type<Desugared>>,
    value_args: Vec<Expr<Desugared>>,
    span: Span,
) -> Expr<Desugared> {
    let mut args: Vec<CallArg<Desugared>> = Vec::with_capacity(type_args.len() + value_args.len());
    args.extend(type_args.into_iter().map(CallArg::Type));
    args.extend(value_args.into_iter().map(CallArg::Value));
    Expr::synth_call(
        Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::new(name.to_owned(), span)],
            meta: Meta::new(span),
            ext: (),
        },
        args,
        span,
    )
}

fn product_value_from_values(
    values: Vec<Expr<Desugared>>,
    types: Vec<Type<Desugared>>,
    span: Span,
) -> Expr<Desugared> {
    if values.is_empty() {
        return unit_expr(span);
    }
    if values.len() == 1 {
        return values.into_iter().next().expect("one value");
    }
    let first = values[0].clone();
    let rest_values = values[1..].to_vec();
    let first_ty = types[0].clone();
    let rest_ty = crate::ast::build_product_right_fold(types[1..].to_vec(), span);
    let rest = product_value_from_values(rest_values, types[1..].to_vec(), span);
    call_intrinsic_with_type_args("__pair__", vec![first_ty, rest_ty], vec![first, rest], span)
}

fn product_value_from_values_infer(values: Vec<Expr<Desugared>>, span: Span) -> Expr<Desugared> {
    product_value_from_values_infer_at(values, span, 0)
}

fn product_value_from_values_infer_at(
    values: Vec<Expr<Desugared>>,
    span: Span,
    depth: u32,
) -> Expr<Desugared> {
    if values.is_empty() {
        return unit_expr(span);
    }
    if values.len() == 1 {
        return values.into_iter().next().expect("one value");
    }
    let first = values[0].clone();
    let rest = product_value_from_values_infer_at(values[1..].to_vec(), span, depth + 1);
    call_intrinsic(
        "__pair__",
        vec![first, rest],
        rec_generated_span(span, 20_100 + depth),
    )
}

fn bind_params_from_state(
    params: &[Param<Desugared>],
    state_value: Expr<Desugared>,
    body: Expr<Desugared>,
    span: Span,
    depth: usize,
) -> Expr<Desugared> {
    if params.is_empty() {
        return body;
    }
    if params.len() == 1 {
        return Expr::Let {
            occurrence: Default::default(),
            name: params[0].name.clone(),
            name_span: params[0].meta.span,
            ty: None,
            pattern: (),
            value: Box::new(state_value),
            body: Box::new(body),
            meta: Meta::new(span),
        };
    }
    let left_ty = params[0]
        .ty
        .as_ref()
        .expect("recursive value params are validated to have types")
        .clone();
    let right_tys: Vec<Type<Desugared>> = params[1..]
        .iter()
        .map(|p| {
            p.ty.as_ref()
                .expect("recursive value params are validated to have types")
                .clone()
        })
        .collect();
    let right_ty = crate::ast::build_product_right_fold(right_tys, span);
    let rest_name = format!("__rec_rest{}_d{}__", span.start, depth);
    // Generated spans, distinct from each other and from any user text. Giving
    // the binding and its reference the rec member's own span made the
    // reference look like the declaration re-stating itself, which the "used"
    // filter discards — so a binding that *is* used read as unused, and the
    // warning landed on the whole `fn` in the user's source.
    let rest_ref = path_expr(rest_name.clone(), rec_generated_span(span, 1));
    let rest_body = bind_params_from_state(&params[1..], rest_ref, body, span, depth + 1);
    let first_value = call_intrinsic_with_type_args(
        "__fst__",
        vec![left_ty.clone(), right_ty.clone()],
        vec![state_value.clone()],
        span,
    );
    let first_body = Expr::Let {
        occurrence: Default::default(),
        name: params[0].name.clone(),
        name_span: params[0].meta.span,
        ty: None,
        pattern: (),
        value: Box::new(first_value),
        body: Box::new(rest_body),
        meta: Meta::new(span),
    };
    let rest_value =
        call_intrinsic_with_type_args("__snd__", vec![left_ty, right_ty], vec![state_value], span);
    Expr::Let {
        occurrence: Default::default(),
        name: rest_name,
        name_span: rec_generated_span(span, 0),
        ty: None,
        pattern: (),
        value: Box::new(rest_value),
        body: Box::new(first_body),
        meta: Meta::new(span),
    }
}

fn inject_sum_arm(
    value: Expr<Desugared>,
    arms: &[Type<Desugared>],
    index: usize,
    span: Span,
) -> Expr<Desugared> {
    if arms.len() == 1 {
        return value;
    }
    let left_ty = arms[0].clone();
    let right_ty = build_sum_right_fold(arms[1..].to_vec(), span);
    if index == 0 {
        call_intrinsic_with_type_args("__left__", vec![left_ty, right_ty], vec![value], span)
    } else {
        let nested = inject_sum_arm(value, &arms[1..], index - 1, span);
        call_intrinsic_with_type_args("__right__", vec![left_ty, right_ty], vec![nested], span)
    }
}

fn inject_step_continue(
    state_value: Expr<Desugared>,
    state_ty: &Type<Desugared>,
    ret_ty: &Type<Desugared>,
    span: Span,
) -> Expr<Desugared> {
    call_intrinsic_with_type_args(
        "__left__",
        vec![state_ty.clone(), ret_ty.clone()],
        vec![state_value],
        span,
    )
}

fn inject_step_return(
    result_value: Expr<Desugared>,
    state_ty: &Type<Desugared>,
    ret_ty: &Type<Desugared>,
    span: Span,
) -> Expr<Desugared> {
    call_intrinsic_with_type_args(
        "__right__",
        vec![state_ty.clone(), ret_ty.clone()],
        vec![result_value],
        span,
    )
}

fn rec_wrapper_type_name(group_span: Span, member: &str) -> String {
    format!(
        "Rec_state{}_m{}",
        group_span.start,
        crate::naming::encode_name_component(member)
    )
}

fn rec_wrapper_ctor_name(group_span: Span, member: &str) -> String {
    format!(
        "mk_rec_state{}_m{}",
        group_span.start,
        crate::naming::encode_name_component(member)
    )
}

fn rec_wrapper_projector_name(group_span: Span, member: &str) -> String {
    format!(
        "un_rec_state{}_m{}",
        group_span.start,
        crate::naming::encode_name_component(member)
    )
}

/// Emit a call to a Kio'-intrinsic by name, with `n_type_args`
/// leading `Type::Infer` placeholders followed by the value
/// arguments. The placeholders signal "infer these from value-arg
/// types" without leaving the call's syntactic shape collapsed
/// to value-args only — `apply_polymorphic_function`'s inference
/// path can't disambiguate adjacent type binders from value-arg
/// types alone. Surface forms that desugar here — tuple literals,
/// multi-label values, the `if`/`else` elaboration trees, the
/// `__either__` chains in match-clause dispatch, etc. — produce
/// calls that go through the explicit path by being honest about
/// which type-args the desugar layer is leaving for the typer to
/// fill in.
fn call_intrinsic(name: &str, args: Vec<Expr<Desugared>>, span: Span) -> Expr<Desugared> {
    call_intrinsic_with_n_type_args(name, intrinsic_arity(name), args, span)
}

/// The number of leading type-parameter binders for each
/// Kio' intrinsic. Used by `call_intrinsic` to emit the right
/// number of `Type::Infer` placeholders so the resulting call
/// reaches `apply_polymorphic_function`'s explicit path rather than
/// the inference path. Hard-coded against the intrinsic table in
/// `specs/prime.md`; if the table grows or shifts, mirror here.
fn intrinsic_arity(name: &str) -> usize {
    match name {
        // `__pair__(A, B, x: A, y: B) -> A & B`
        // `__fst__(A, B, p: A & B) -> A`
        // `__snd__(A, B, p: A & B) -> B`
        // `__left__(A, B, x: A) -> A | B`
        // `__right__(A, B, x: B) -> A | B`
        "__pair__" | "__fst__" | "__snd__" | "__left__" | "__right__" => 2,
        // `__either__(A, B, C, s: A | B, fl: A -> C, fr: B -> C) -> C`
        "__either__" => 3,
        // `__if_then_else__(A, cond: Bool, t: . -> A, e: . -> A) -> A`
        "__if_then_else__" => 1,
        // `__absurd__(A, x: !) -> A`
        "__absurd__" => 1,
        _ => 0,
    }
}

fn call_intrinsic_with_n_type_args(
    name: &str,
    n_type_args: usize,
    args: Vec<Expr<Desugared>>,
    span: Span,
) -> Expr<Desugared> {
    let mut call_args: Vec<CallArg<Desugared>> = Vec::with_capacity(n_type_args + args.len());
    for _ in 0..n_type_args {
        call_args.push(CallArg::Type(Type::Infer {
            meta: Meta::new(span),
            ext: (),
        }));
    }
    call_args.extend(args.into_iter().map(CallArg::Value));
    Expr::synth_call(
        Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::new(name.to_owned(), span)],
            meta: Meta::new(span),
            ext: (),
        },
        call_args,
        span,
    )
}

fn expr_contains_rec_call(expr: &Expr<Surface>) -> bool {
    match expr {
        crate::ast::Expr::BlockCall { .. } => {
            unreachable!("trailing blocks are projected before desugaring")
        }
        Expr::RecCall { .. } => true,
        Expr::Call { callee, args, .. } => {
            expr_contains_rec_call(callee) || args.iter().any(call_arg_contains_rec)
        }
        Expr::FnExpr { body, .. } | Expr::FnPlaceholder { body, .. } => {
            expr_contains_rec_call(body)
        }
        Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
            expr_contains_rec_call(value) || expr_contains_rec_call(body)
        }
        Expr::RowLet { value, body, .. } => {
            expr_contains_rec_call(value) || expr_contains_rec_call(body)
        }
        Expr::Elaborator { call, .. } => elaborator_call_contains_rec(call),
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::UserElaborator { args, .. } => args.iter().any(call_arg_contains_rec),
        Expr::LabelValue { labels, .. } => labels
            .iter()
            .any(|label| expr_contains_rec_call(&label.value)),
        Expr::Tuple { items, .. } => items.iter().any(expr_contains_rec_call),
        Expr::Ufcs { receiver, args, .. } => {
            expr_contains_rec_call(receiver) || args.iter().any(call_arg_contains_rec)
        }
        Expr::OpChain { kind, .. } => match kind {
            crate::ast::OpChainKind::Normal { slots, .. } => {
                slots.iter().any(expr_contains_rec_call)
            }
            crate::ast::OpChainKind::Variadic { elements, .. } => {
                elements.iter().any(expr_contains_rec_call)
            }
        },
        Expr::Path { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => false,
        Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
        | Expr::EnrichedRecord { ext, .. }
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

fn call_arg_contains_rec(arg: &CallArg<Surface>) -> bool {
    match arg {
        CallArg::Type(_) => false,
        CallArg::Value(value) => expr_contains_rec_call(value),
    }
}

fn rec_cps_operand_is_inert(expr: &Expr<Surface>) -> bool {
    match expr {
        Expr::Path { .. }
        | Expr::FnExpr { .. }
        | Expr::FnPlaceholder { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => true,
        Expr::Tuple { items, .. } => items.iter().all(rec_cps_operand_is_inert),
        Expr::LabelValue { labels, .. } => labels
            .iter()
            .all(|label| rec_cps_operand_is_inert(&label.value)),
        _ => false,
    }
}

fn elaborator_call_contains_rec(call: &ElaboratorCall<Surface>) -> bool {
    match call {
        ElaboratorCall::FieldAccess { receiver, .. } => expr_contains_rec_call(receiver),
        ElaboratorCall::FieldUpdate { receiver, updates } => {
            expr_contains_rec_call(receiver)
                || updates
                    .iter()
                    .any(|update| expr_contains_rec_call(&update.value))
        }
    }
}

/// Turn a desugared placeholder body into an ordinary function while picking
/// parameter names that cannot shadow identifiers already used by the body.
fn lower_fn_placeholder(
    body: Expr<Desugared>,
    slot_count: usize,
    stem_span: Span,
    meta: Meta<Surface>,
) -> Expr<Desugared> {
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    collect_identifiers(&body, &mut used);
    // The body's own `__pN__` sentinels don't count as "used" —
    // they're what we're about to rename.
    for slot in 1..=slot_count {
        used.remove(&crate::pass::placeholder::local_name(slot as u32));
    }
    let mut renames: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut params: Vec<SignatureParam<Desugared>> = Vec::with_capacity(slot_count);
    for slot in 1..=slot_count {
        let mut candidate = format!("p{slot}");
        let mut bump = 2u32;
        while used.contains(&candidate) {
            candidate = format!("p{slot}_v{bump}");
            bump += 1;
        }
        used.insert(candidate.clone());
        renames.insert(
            crate::pass::placeholder::local_name(slot as u32),
            candidate.clone(),
        );
        params.push(SignatureParam::Value(Param {
            name: candidate,
            ty: None,
            pattern: (),
            meta: Meta::new(stem_span),
        }));
    }
    let body = rename_paths(body, &renames);
    Expr::FnExpr {
        occurrence: Default::default(),
        sig: Signature::new(params),
        ret_ty: None,
        body: Box::new(body),
        meta: Meta::new(meta.span),
        caps: (),
    }
}

fn desugared_contains_rec_quote(source: &Expr<Desugared>) -> bool {
    let mut pending = vec![source];
    while let Some(source) = pending.pop() {
        match source {
            Expr::RecQuote { plan, .. } => {
                if matches!(plan.as_ref(), crate::ast::RecQuotePlan::Operand { .. }) {
                    return true;
                }
            }
            Expr::Call { callee, args, .. } => {
                pending.push(callee);
                pending.extend(args.iter().filter_map(|arg| match arg {
                    CallArg::Value(value) => Some(value),
                    CallArg::Type(_) => None,
                }));
            }
            Expr::FnExpr { .. } => {}
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                pending.push(value);
                pending.push(body);
            }
            Expr::RecOrder { plan, .. } => {
                pending.push(&plan.value);
                pending.push(&plan.body);
                pending.extend(plan.tail_continuation.iter().map(|value| value.as_ref()));
            }
            Expr::UserElaborator { args, .. } => {
                pending.extend(args.iter().filter_map(|arg| match arg {
                    CallArg::Value(value) => Some(value),
                    CallArg::Type(_) => None,
                }))
            }
            Expr::Ufcs { receiver, args, .. } => {
                pending.push(receiver);
                pending.extend(args.iter().filter_map(|arg| match arg {
                    CallArg::Value(value) => Some(value),
                    CallArg::Type(_) => None,
                }));
            }
            Expr::LabelValue { labels, .. } => {
                pending.extend(labels.iter().map(|label| &label.value))
            }
            Expr::Elaborator { call, .. } => match call {
                ElaboratorCall::FieldAccess { receiver, .. } => pending.push(receiver),
                ElaboratorCall::FieldUpdate { receiver, updates } => {
                    pending.push(receiver);
                    pending.extend(updates.iter().map(|update| &update.value));
                }
            },
            Expr::Path { .. }
            | Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::BoolLit { .. } => {}
            Expr::BlockCall { ext, .. }
            | Expr::Tuple { ext, .. }
            | Expr::FnPlaceholder { ext, .. }
            | Expr::RowLet { ext, .. }
            | Expr::OpChain { ext, .. }
            | Expr::RecCall { ext, .. }
            | Expr::EnrichedTuple { ext, .. }
            | Expr::EnrichedProject { ext, .. }
            | Expr::EnrichedInject { ext, .. }
            | Expr::EnrichedMatch { ext, .. }
            | Expr::EnrichedConditional { ext, .. }
            | Expr::EnrichedRecord { ext, .. }
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
    false
}

/// Collect every identifier appearing in `e` into `out`. Used by
/// the `Expr::FnPlaceholder` desugar to pick hygienic placeholder
/// names that don't shadow anything the body already references.
fn collect_identifiers(e: &Expr<Desugared>, out: &mut std::collections::HashSet<String>) {
    match e {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Path { segments, .. } => {
            if let Some(head) = segments.first() {
                out.insert(head.name.clone());
            }
        }
        Expr::Call { callee, args, .. } => {
            collect_identifiers(callee, out);
            for a in args {
                if let CallArg::Value(v) = a {
                    collect_identifiers(v, out);
                }
            }
        }
        Expr::FnExpr { sig, body, .. } => {
            for p in &sig.params {
                if let SignatureParam::Value(vp) = p {
                    out.insert(vp.name.clone());
                }
            }
            collect_identifiers(body, out);
        }
        Expr::Let {
            name, value, body, ..
        } => {
            out.insert(name.clone());
            collect_identifiers(value, out);
            collect_identifiers(body, out);
        }
        Expr::Seq { value, body, .. } => {
            collect_identifiers(value, out);
            collect_identifiers(body, out);
        }
        Expr::Elaborator { call, .. } => match call {
            ElaboratorCall::FieldAccess { receiver, .. } => collect_identifiers(receiver, out),
            ElaboratorCall::FieldUpdate { receiver, updates } => {
                collect_identifiers(receiver, out);
                for update in updates {
                    collect_identifiers(&update.value, out);
                }
            }
        },
        Expr::RecQuote { plan, .. } => {
            for expression in plan.expressions() {
                collect_identifiers(expression, out);
            }
        }
        Expr::RecOrder { plan, .. } => {
            out.insert(plan.name.clone());
            if let Some(continuation) = &plan.tail_continuation {
                collect_identifiers(continuation, out);
            }
            collect_identifiers(&plan.value, out);
            collect_identifiers(&plan.body, out);
        }
        Expr::UserElaborator { name, args, .. } => {
            out.insert(name.clone());
            for arg in args {
                if let CallArg::Value(value) = arg {
                    collect_identifiers(value, out);
                }
            }
        }
        Expr::LabelValue { labels, .. } => {
            for l in labels {
                collect_identifiers(&l.value, out);
            }
        }
        Expr::RowLet { ext, .. } => match *ext {},
        Expr::Ufcs {
            receiver,
            callee_segments,
            args,
            ..
        } => {
            collect_identifiers(receiver, out);
            if let Some(head) = callee_segments.first() {
                out.insert(head.name.clone());
            }
            for a in args {
                if let CallArg::Value(v) = a {
                    collect_identifiers(v, out);
                }
            }
        }
        Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => {}
        // Statically uninhabited at `Desugared`.
        Expr::Tuple { ext, .. } => match *ext {},
        Expr::FnPlaceholder { ext, .. } => match *ext {},
        Expr::OpChain { ext, .. } => match *ext {},
        Expr::RecCall { ext, .. } => match *ext {},
        // Enriched structural variants only exist post-typecheck.
        Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
        | Expr::EnrichedRecord { ext, .. }
        | Expr::EnrichedFieldGet { ext, .. } => match *ext {},
        Expr::LowHostCall { ext, .. }
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
fn rename_paths(
    e: Expr<Desugared>,
    renames: &std::collections::HashMap<String, String>,
) -> Expr<Desugared> {
    match e {
        Expr::Path {
            occurrence: _,
            segments,
            meta,
            ext: _,
        } => {
            if segments.len() == 1
                && let Some(new) = renames.get(segments[0].as_str())
            {
                let span = segments[0].span;
                Expr::Path {
                    occurrence: Default::default(),
                    segments: vec![PathSegment::new(new.clone(), span)],
                    meta,
                    ext: (),
                }
            } else {
                Expr::Path {
                    occurrence: Default::default(),
                    segments,
                    meta,
                    ext: (),
                }
            }
        }
        Expr::Call {
            occurrence: _,
            callee,
            args,
            meta,
            ext: _,
        } => Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(rename_paths(*callee, renames)),
            args: args
                .into_iter()
                .map(|a| match a {
                    CallArg::Value(v) => CallArg::Value(rename_paths(v, renames)),
                    CallArg::Type(t) => CallArg::Type(t),
                })
                .collect(),
            meta,
            ext: (),
        },
        Expr::RecCall { ext, .. } => match ext {},
        Expr::FnExpr {
            occurrence: _,
            sig,
            ret_ty,
            body,
            meta,
            caps,
        } => Expr::FnExpr {
            occurrence: Default::default(),
            sig,
            ret_ty,
            body: Box::new(rename_paths(*body, renames)),
            meta,
            caps,
        },
        Expr::Let {
            occurrence: _,
            name,
            name_span,
            ty,
            pattern,
            value,
            body,
            meta,
        } => Expr::Let {
            occurrence: Default::default(),
            name,
            name_span,
            ty,
            pattern,
            value: Box::new(rename_paths(*value, renames)),
            body: Box::new(rename_paths(*body, renames)),
            meta,
        },
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta,
        } => Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(rename_paths(*value, renames)),
            body: Box::new(rename_paths(*body, renames)),
            meta,
        },
        Expr::Elaborator {
            occurrence: _,
            kind,
            call,
            meta,
            ext,
        } => Expr::Elaborator {
            occurrence: Default::default(),
            kind,
            call: rename_elaborator_call(call, renames),
            meta,
            ext,
        },
        Expr::RecOrder {
            occurrence: _,
            mut plan,
            meta,
            ext,
        } => {
            if let Some(renamed) = renames.get(&plan.name) {
                plan.name = renamed.clone();
            }
            plan.value = Box::new(rename_paths(*plan.value, renames));
            plan.body = Box::new(rename_paths(*plan.body, renames));
            plan.tail_continuation = plan
                .tail_continuation
                .map(|expr| Box::new(rename_paths(*expr, renames)));
            Expr::RecOrder {
                occurrence: Default::default(),
                plan,
                meta,
                ext,
            }
        }
        Expr::RecQuote {
            plan, meta, ext, ..
        } => {
            let plan = match *plan {
                crate::ast::RecQuotePlan::Operand {
                    public_ty,
                    runtime_ty,
                    computation,
                } => crate::ast::RecQuotePlan::Operand {
                    public_ty,
                    runtime_ty,
                    computation: Box::new(rename_paths(*computation, renames)),
                },
                crate::ast::RecQuotePlan::Expansion {
                    runtime_ty,
                    continuation,
                    value,
                } => crate::ast::RecQuotePlan::Expansion {
                    runtime_ty,
                    continuation: Box::new(rename_paths(*continuation, renames)),
                    value: Box::new(rename_paths(*value, renames)),
                },
            };
            Expr::RecQuote {
                occurrence: Default::default(),
                plan: Box::new(plan),
                meta,
                ext,
            }
        }
        Expr::UserElaborator {
            occurrence: _,
            name,
            form,
            args,
            meta,
            ext,
        } => Expr::UserElaborator {
            occurrence: Default::default(),
            name,
            form,
            args: args
                .into_iter()
                .map(|arg| match arg {
                    CallArg::Value(value) => CallArg::Value(rename_paths(value, renames)),
                    CallArg::Type(ty) => CallArg::Type(ty),
                })
                .collect(),
            meta,
            ext,
        },
        Expr::LabelValue {
            occurrence: _,
            labels,
            meta,
            ext,
        } => Expr::LabelValue {
            occurrence: Default::default(),
            labels: labels
                .into_iter()
                .map(|l| crate::ast::LabelValueLabel {
                    label: l.label,
                    label_span: l.label_span,
                    value: rename_paths(l.value, renames),
                    meta: l.meta,
                })
                .collect(),
            meta,
            ext,
        },
        Expr::Ufcs {
            occurrence: _,
            receiver,
            mut callee_segments,
            callee_span,
            args,
            flavor,
            bang,
            meta,
            ext,
        } => {
            if bang.is_none()
                && callee_segments.len() == 1
                && let Some(renamed) = renames.get(&callee_segments[0].name)
            {
                callee_segments[0].name = renamed.clone();
            }
            Expr::Ufcs {
                occurrence: Default::default(),
                receiver: Box::new(rename_paths(*receiver, renames)),
                callee_segments,
                callee_span,
                args: args
                    .into_iter()
                    .map(|a| match a {
                        CallArg::Value(v) => CallArg::Value(rename_paths(v, renames)),
                        CallArg::Type(t) => CallArg::Type(t),
                    })
                    .collect(),
                flavor,
                bang,
                meta,
                ext,
            }
        }
        Expr::Unit {
            occurrence: _,
            meta,
        } => Expr::Unit {
            occurrence: Default::default(),
            meta,
        },
        Expr::StrLit {
            occurrence: _,
            value,
            annotation,
            meta,
        } => Expr::StrLit {
            occurrence: Default::default(),
            value,
            annotation,
            meta,
        },
        Expr::IntLit {
            occurrence: _,
            digits,
            annotation,
            meta,
        } => Expr::IntLit {
            occurrence: Default::default(),
            digits,
            annotation,
            meta,
        },
        Expr::FloatLit {
            occurrence: _,
            digits,
            annotation,
            meta,
        } => Expr::FloatLit {
            occurrence: Default::default(),
            digits,
            annotation,
            meta,
        },
        Expr::BoolLit {
            occurrence: _,
            value,
            annotation,
            meta,
        } => Expr::BoolLit {
            occurrence: Default::default(),
            value,
            annotation,
            meta,
        },
        // Statically uninhabited at `Desugared`.
        Expr::Tuple { ext, .. } => match ext {},
        Expr::FnPlaceholder { ext, .. } => match ext {},
        Expr::OpChain { ext, .. } => match ext {},
    }
}

fn rename_elaborator_call(
    call: ElaboratorCall<Desugared>,
    renames: &std::collections::HashMap<String, String>,
) -> ElaboratorCall<Desugared> {
    match call {
        ElaboratorCall::FieldAccess { receiver, labels } => ElaboratorCall::FieldAccess {
            receiver: Box::new(rename_paths(*receiver, renames)),
            labels,
        },
        ElaboratorCall::FieldUpdate { receiver, updates } => ElaboratorCall::FieldUpdate {
            receiver: Box::new(rename_paths(*receiver, renames)),
            updates: updates
                .into_iter()
                .map(|mut update| {
                    update.value = rename_paths(update.value, renames);
                    update
                })
                .collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Visibility;
    use crate::pass::parser::parse;

    fn d(src: &str) -> Module<Desugared> {
        let m = parse(src).expect("parse");
        desugar_module(m).expect("desugar")
    }

    fn parsed(src: &str) -> Module<Surface> {
        parse(src).expect("parse")
    }

    fn visit_desugared_expr<'a>(
        expr: &'a Expr<Desugared>,
        visit: &mut impl FnMut(&'a Expr<Desugared>),
    ) {
        visit(expr);
        match expr {
            Expr::BlockCall { ext, .. } => match *ext {},
            Expr::Call { callee, args, .. } => {
                visit_desugared_expr(callee, visit);
                visit_desugared_call_args(args, visit);
            }
            Expr::FnExpr { body, .. } => visit_desugared_expr(body, visit),
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                visit_desugared_expr(value, visit);
                visit_desugared_expr(body, visit);
            }
            Expr::Elaborator { call, .. } => match call {
                ElaboratorCall::FieldAccess { receiver, .. } => {
                    visit_desugared_expr(receiver, visit);
                }
                ElaboratorCall::FieldUpdate { receiver, updates } => {
                    visit_desugared_expr(receiver, visit);
                    for update in updates {
                        visit_desugared_expr(&update.value, visit);
                    }
                }
            },
            Expr::RecOrder { plan, .. } => {
                visit_desugared_expr(&plan.value, visit);
                visit_desugared_expr(&plan.body, visit);
            }
            Expr::RecQuote { plan, .. } => {
                for expression in plan.expressions() {
                    visit_desugared_expr(expression, visit);
                }
            }
            Expr::UserElaborator { args, .. } => visit_desugared_call_args(args, visit),
            Expr::LabelValue { labels, .. } => {
                for label in labels {
                    visit_desugared_expr(&label.value, visit);
                }
            }
            Expr::Ufcs { receiver, args, .. } => {
                visit_desugared_expr(receiver, visit);
                visit_desugared_call_args(args, visit);
            }
            Expr::Path { .. }
            | Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::BoolLit { .. } => {}
            Expr::RowLet { ext, .. }
            | Expr::Tuple { ext, .. }
            | Expr::FnPlaceholder { ext, .. }
            | Expr::OpChain { ext, .. }
            | Expr::RecCall { ext, .. }
            | Expr::EnrichedTuple { ext, .. }
            | Expr::EnrichedProject { ext, .. }
            | Expr::EnrichedInject { ext, .. }
            | Expr::EnrichedMatch { ext, .. }
            | Expr::EnrichedConditional { ext, .. }
            | Expr::EnrichedRecord { ext, .. }
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

    fn visit_desugared_call_args<'a>(
        args: &'a [CallArg<Desugared>],
        visit: &mut impl FnMut(&'a Expr<Desugared>),
    ) {
        for arg in args {
            if let CallArg::Value(value) = arg {
                visit_desugared_expr(value, visit);
            }
        }
    }

    fn desugared_fn_body<'a>(module: &'a Module<Desugared>, name: &str) -> &'a Expr<Desugared> {
        module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == name => Some(&def.body),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected function {name}"))
    }

    fn is_call_to(expr: &Expr<Desugared>, name: &str) -> bool {
        matches!(
            expr,
            Expr::Call { callee, .. }
                if matches!(
                    callee.as_ref(),
                    Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0].as_str() == name
                )
        )
    }

    fn desugared_expr_node_count(expr: &Expr<Desugared>) -> usize {
        let mut count = 0;
        visit_desugared_expr(expr, &mut |_| count += 1);
        count
    }

    fn desugared_path_use_count(expr: &Expr<Desugared>, name: &str) -> usize {
        let mut count = 0;
        visit_desugared_expr(expr, &mut |expr| {
            if matches!(
                expr,
                Expr::Path { segments, .. }
                    if segments.len() == 1 && segments[0].as_str() == name
            ) {
                count += 1;
            }
        });
        count
    }

    fn is_deferred_continuation_function_shell(plan: &RecOrderPlan<Desugared>) -> bool {
        plan.disposition == RecOrderDisposition::Ordered(RecOrderTypeFlow::ExpectedFromBody)
            && matches!(
                plan.annotation.as_ref(),
                Some(Type::Function {
                    param,
                    ret,
                    abi_arity: 1,
                    ..
                }) if matches!(param.as_ref(), Type::Infer { .. })
                    && ret.as_ref() == &plan.runtime_ty
            )
    }

    fn deferred_continuation_elaborated_value(shell: &RecOrderPlan<Desugared>) -> &Expr<Desugared> {
        let Expr::RecOrder { plan: carrier, .. } = shell.body.as_ref() else {
            panic!("a deferred continuation shell lost its elaborated-value carrier")
        };
        assert_eq!(
            carrier.disposition,
            RecOrderDisposition::Ordered(RecOrderTypeFlow::ElaboratedValue)
        );
        assert!(
            carrier.annotation.is_none(),
            "an unannotated deferred call gained a carrier annotation"
        );
        assert_eq!(carrier.runtime_ty, shell.runtime_ty);
        assert!(matches!(
            carrier.body.as_ref(),
            Expr::Path { segments, .. }
                if segments.len() == 1 && segments[0].as_str() == carrier.name.as_str()
        ));
        carrier.value.as_ref()
    }

    fn assert_one_optional_and_one_required_deferred_tail(expr: &Expr<Desugared>) {
        let mut requirements = Vec::new();
        visit_desugared_expr(expr, &mut |expr| {
            if let Expr::RecOrder { plan, .. } = expr
                && let RecOrderDisposition::DeferredTail { requirement, .. } = plan.disposition
            {
                requirements.push(requirement);
            }
        });
        assert_eq!(requirements.len(), 2);
        assert_eq!(
            requirements
                .iter()
                .filter(|requirement| **requirement == RecOrderTailRequirement::Optional)
                .count(),
            1
        );
        assert_eq!(
            requirements
                .iter()
                .filter(|requirement| **requirement == RecOrderTailRequirement::Required)
                .count(),
            1,
            "both clauses must remain recipe-authenticated candidates"
        );
    }

    #[test]
    fn package_desugar_error_orders_by_module_path_then_span() {
        let modules = vec![
            (
                std::path::PathBuf::from("pkg/z.kio"),
                parsed("module pkg/z; literal dup = 1; literal dup = 2;"),
            ),
            (
                std::path::PathBuf::from("pkg/a.kio"),
                parsed("module pkg/a; literal dup = 1; literal dup = 2;"),
            ),
        ];

        let err = desugar_package_with_imports(modules, &PackageLiteralAliases::default())
            .expect_err("expected desugar error");

        assert_eq!(err.file_path, std::path::PathBuf::from("pkg/a.kio"));
    }

    #[test]
    fn literal_decl_expands_bare_reference() {
        let m = d("module x; literal answer = 42; fn main() -> I32 { answer }");
        assert_eq!(m.items.len(), 1);
        let Item::FnDef(f) = &m.items[0] else {
            panic!("expected fn, got {:?}", m.items[0]);
        };
        assert!(matches!(&f.body, Expr::IntLit { digits, annotation: None, .. } if digits == "42"));
    }

    #[test]
    fn literal_decl_expands_annotation_call() {
        let m =
            d(r#"module x; literal greeting = "abc"; fn main() -> String { greeting(String) }"#);
        let Item::FnDef(f) = &m.items[0] else {
            panic!("expected fn, got {:?}", m.items[0]);
        };
        let Expr::StrLit {
            value,
            annotation: Some(Type::Path { segments, .. }),
            ..
        } = &f.body
        else {
            panic!("expected annotated literal body, got {:?}", f.body);
        };
        assert_eq!(value, "abc");
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].as_str(), "String");
    }

    #[test]
    fn literal_alias_expansions_keep_distinct_use_spans() {
        let m = d("module x; literal one = 1; \
             fn one_i32() -> I32 { one(I32) } \
             fn one_i64() -> I64 { one(I64) }");
        let spans = m
            .items
            .iter()
            .filter_map(|item| match item {
                Item::FnDef(def) => Some(def.body.span()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(spans.len(), 2);
        assert_ne!(spans[0], spans[1]);
    }

    #[test]
    fn literal_decl_call_requires_one_type_annotation() {
        let m = parsed("module x; literal answer = 42; fn main() -> I32 { answer() }");
        let err = desugar_module(m).expect_err("expected annotation arity error");
        let (_, msg) = err.diag();
        assert!(
            msg.contains("accepts exactly one type annotation argument"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn literal_decl_call_rejects_value_argument() {
        let m = parsed("module x; literal answer = 42; fn main() -> I32 { answer(0) }");
        let err = desugar_module(m).expect_err("expected annotation kind error");
        let (_, msg) = err.diag();
        assert!(
            msg.contains("accepts only a type annotation argument"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn literal_decl_respects_local_shadowing() {
        let m = d("module x; literal answer = 42; fn main(answer: I32) -> I32 { answer }");
        let Item::FnDef(f) = &m.items[0] else {
            panic!("expected fn, got {:?}", m.items[0]);
        };
        assert!(matches!(&f.body, Expr::Path { segments, .. } if segments[0].as_str() == "answer"));
    }

    #[test]
    fn imported_literal_decl_drops_resolved_use() {
        let modules = vec![
            (
                std::path::PathBuf::from("pkg/util.kio"),
                parsed(
                    "module pkg/util; \
                     pub literal zero = 0;",
                ),
            ),
            (
                std::path::PathBuf::from("pkg/main.kio"),
                parsed(
                    "module pkg/main; \
                     import pkg/util(zero); \
                     fn main() -> I32 { zero(I32) }",
                ),
            ),
        ];
        let package_aliases = collect_package_literal_aliases(&modules, None);
        let lowered =
            desugar_package_with_imports(modules, &package_aliases).expect("desugar package");
        let main = lowered
            .iter()
            .find(|(path, _)| path == &std::path::PathBuf::from("pkg/main.kio"))
            .map(|(_, module)| module)
            .expect("main module");
        assert!(main.imports.is_empty());
    }

    #[test]
    fn no_op_on_kio_prime_module() {
        // A pure-Kio' module passes through unchanged.
        let m = d("module x; fn id[A](x: A) -> A { x }");
        assert!(
            !m.imports
                .iter()
                .any(|u| matches!(u.kind, ImportKind::Intrinsics))
        );
        // No items got rewritten.
        assert_eq!(m.items.len(), 1);
    }

    #[test]
    fn lower_fn_placeholder_avoids_authored_ufcs_callee_name() {
        let module = d("module x; fn f(p1: . -> .) -> . -> . { .x. { x1.>p1 } }");
        let Expr::FnExpr { sig, body, .. } = desugared_fn_body(&module, "f") else {
            panic!("expected lowered placeholder function")
        };
        let [SignatureParam::Value(param)] = sig.params.as_slice() else {
            panic!("expected one placeholder parameter")
        };
        assert_eq!(param.name, "p1_v2");
        let Expr::Ufcs {
            receiver,
            callee_segments,
            ..
        } = body.as_ref()
        else {
            panic!("expected UFCS placeholder body")
        };
        assert_eq!(callee_segments.len(), 1);
        assert_eq!(callee_segments[0].as_str(), "p1");
        assert!(matches!(
            receiver.as_ref(),
            Expr::Path { segments, .. }
                if segments.len() == 1 && segments[0].as_str() == "p1_v2"
        ));
    }

    #[test]
    fn lower_fn_placeholder_avoids_authored_bang_callee_name() {
        let module = d("module x; fn f() -> . -> . { .x. { p1!(x1) } }");
        let Expr::FnExpr { sig, body, .. } = desugared_fn_body(&module, "f") else {
            panic!("expected lowered placeholder function")
        };
        let [SignatureParam::Value(param)] = sig.params.as_slice() else {
            panic!("expected one placeholder parameter")
        };
        assert_eq!(param.name, "p1_v2");
        let Expr::UserElaborator { name, args, .. } = body.as_ref() else {
            panic!("expected bang-call placeholder body")
        };
        assert_eq!(name, "p1");
        assert!(matches!(
            args.as_slice(),
            [CallArg::Value(Expr::Path { segments, .. })]
                if segments.len() == 1 && segments[0].as_str() == "p1_v2"
        ));
    }

    #[test]
    fn lower_fn_placeholder_retries_past_authored_outer_names() {
        let module = d("module x; \
             fn f(p1: ., p1_v2: .) -> . -> . { \
               .x. { let keep_p1 = p1; let keep_p1_v2 = p1_v2; x1 } \
             }");
        let Expr::FnExpr { sig, body, .. } = desugared_fn_body(&module, "f") else {
            panic!("expected lowered placeholder function")
        };
        let [SignatureParam::Value(param)] = sig.params.as_slice() else {
            panic!("expected one placeholder parameter")
        };
        assert_eq!(param.name, "p1_v3");
        assert_eq!(desugared_path_use_count(body, "p1"), 1);
        assert_eq!(desugared_path_use_count(body, "p1_v2"), 1);
        assert_eq!(desugared_path_use_count(body, "p1_v3"), 1);
        assert_eq!(
            desugared_path_use_count(body, &crate::pass::placeholder::local_name(1)),
            0
        );
    }

    #[test]
    fn rec_group_lowers_every_member_to_an_ordinary_wrapper_with_its_visibility() {
        let m = d("module x/sub; \
             rec(loop) { \
               fn local(value: .) -> . { value }; \
               pub(x) fn scoped(value: .) -> . { value }; \
               pub fn exported(value: .) -> . { value } \
             }");
        let wrappers = m
            .items
            .iter()
            .filter_map(|item| match item {
                Item::FnDef(def) => Some(def),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            wrappers
                .iter()
                .map(|def| def.name.as_str())
                .collect::<Vec<_>>(),
            vec!["local", "scoped", "exported"]
        );
        assert_eq!(wrappers[0].vis, Visibility::Private);
        assert!(matches!(
            &wrappers[1].vis,
            Visibility::PublicIn(path) if path.segments == ["x"]
        ));
        assert_eq!(wrappers[2].vis, Visibility::Public);
    }

    #[test]
    fn rec_type_substitution_alpha_renames_a_capturing_forall_binder() {
        let span = Span::new(10, 20);
        let binder_span = Span::new(11, 12);
        let path = |name: &str| Type::synth_path(vec![name.to_owned()], Vec::new(), span);
        let source = Type::Forall {
            param: TypeParam {
                name: "B".to_owned(),
                span: binder_span,
                kind: None,
            },
            body: Box::new(Type::Product {
                left: Box::new(path("A")),
                right: Box::new(path("B")),
                meta: Meta::new(span),
            }),
            meta: Meta::new(span),
        };
        let subst = std::collections::HashMap::from([("A".to_owned(), path("B"))]);

        let Type::Forall { param, body, .. } = subst_rec_type(&source, &subst) else {
            panic!("expected forall result")
        };
        assert_eq!(param.name, "B_n2");
        assert_eq!(param.span, binder_span);
        assert_eq!(param.kind, None);
        let Type::Product { left, right, .. } = body.as_ref() else {
            panic!("expected product body")
        };
        assert!(matches!(
            left.as_ref(),
            Type::Path { segments, args, .. }
                if args.is_empty() && segments.len() == 1 && segments[0].as_str() == "B"
        ));
        assert!(matches!(
            right.as_ref(),
            Type::Path { segments, args, .. }
                if args.is_empty() && segments.len() == 1 && segments[0].as_str() == "B_n2"
        ));
    }

    #[test]
    fn rec_type_substitution_alpha_renames_an_applied_forall_binder() {
        let span = Span::new(10, 20);
        let unit = || Type::Unit {
            meta: Meta::new(span),
        };
        let path = |name: &str| Type::synth_path(vec![name.to_owned()], Vec::new(), span);
        let applied = |name: &str| Type::synth_path(vec![name.to_owned()], vec![unit()], span);
        let source = Type::Forall {
            param: TypeParam {
                name: "B".to_owned(),
                span,
                kind: Some(crate::ast::Kind::arrow_chain(1)),
            },
            body: Box::new(Type::Product {
                left: Box::new(applied("A")),
                right: Box::new(applied("B")),
                meta: Meta::new(span),
            }),
            meta: Meta::new(span),
        };
        let subst = std::collections::HashMap::from([("A".to_owned(), path("B"))]);

        let Type::Forall { param, body, .. } = subst_rec_type(&source, &subst) else {
            panic!("expected forall result")
        };
        assert_eq!(param.name, "B_n2");
        let Type::Product { left, right, .. } = body.as_ref() else {
            panic!("expected product body")
        };
        assert!(matches!(
            left.as_ref(),
            Type::Path { segments, args, .. }
                if segments.len() == 1 && segments[0].as_str() == "B"
                    && matches!(args.as_slice(), [Type::Unit { .. }])
        ));
        assert!(matches!(
            right.as_ref(),
            Type::Path { segments, args, .. }
                if segments.len() == 1 && segments[0].as_str() == "B_n2"
                    && matches!(args.as_slice(), [Type::Unit { .. }])
        ));
    }

    #[test]
    fn rec_type_substitution_preserves_an_applied_head_for_an_inference_replacement() {
        let span = Span::new(10, 20);
        let source = Type::synth_path(
            vec!["F".to_owned()],
            vec![Type::synth_path(vec!["A".to_owned()], Vec::new(), span)],
            span,
        );
        let subst = std::collections::HashMap::from([
            (
                "F".to_owned(),
                Type::Infer {
                    meta: Meta::new(span),
                    ext: (),
                },
            ),
            (
                "A".to_owned(),
                Type::synth_path(vec!["B".to_owned()], Vec::new(), span),
            ),
        ]);

        assert!(matches!(
            subst_rec_type(&source, &subst),
            Type::Path { segments, args, .. }
                if segments.len() == 1 && segments[0].as_str() == "F"
                    && matches!(args.as_slice(), [Type::Path { segments, args, .. }]
                        if args.is_empty() && segments.len() == 1
                            && segments[0].as_str() == "B")
        ));
    }

    fn rec_group_start() -> u32 {
        let module = parsed("module x;\nrec(loop) fn go[A](value: A) -> A { value }");
        let [Item::RecGroup(group, _)] = module.items.as_slice() else {
            panic!("expected one recursive group")
        };
        group.meta.span.start
    }

    #[test]
    fn cont_rec_state_types_form_an_explicit_recursive_scope() {
        let source = concat!(
            "module x;\n",
            "host fn loop[S][R](step: S -> S | R, state: S) -> R;\n",
            "rec(loop) {\n",
            "  fn first(value: .) -> . { let next = rec(cont) second(value); next };\n",
            "  fn second(value: .) -> . { rec first(value) }\n",
            "}\n",
        );
        let module = d(source);
        let group_start = source.find("rec(loop)").expect("recursive group") as u32;

        let Some(group) = module.items.iter().find_map(|item| match item {
            Item::TypeRecGroup(group) => Some(group),
            _ => None,
        }) else {
            panic!(
                "continuation state wrappers must share one explicit type-recursive scope: {:?}",
                module.items
            )
        };
        assert!(group.rec_span.is_some());
        assert_eq!(
            group
                .members
                .iter()
                .map(|member| match member {
                    crate::ast::TypeRecMember::Newtype(newtype) => newtype.name.clone(),
                    crate::ast::TypeRecMember::TypeAlias(_) => {
                        panic!("rec state wrappers are nominal")
                    }
                    crate::ast::TypeRecMember::Labels(_, _) => {
                        panic!("rec state wrappers are not labels")
                    }
                })
                .collect::<Vec<_>>(),
            [
                format!("Rec_state{group_start}_mgggjhchdhe"),
                format!("Rec_state{group_start}_mhdgfgdgpgoge"),
                format!("Rec_resume{group_start}"),
            ]
        );
        let Expr::Call { args, .. } = desugared_fn_body(&module, "first") else {
            panic!("the wrapper invokes the supplied loop")
        };
        let Some(CallArg::Type(Type::Sum { left, right, .. })) = args.first() else {
            panic!("the loop state is a sum of all work and resume arms")
        };
        assert!(matches!(left.as_ref(), Type::Path { .. }));
        assert!(matches!(right.as_ref(), Type::Sum { left, right, .. }
            if matches!(left.as_ref(), Type::Path { .. })
                && matches!(right.as_ref(), Type::Path { segments, .. }
                    if segments[0].as_str() == format!("Rec_resume{group_start}"))));
    }

    #[test]
    fn resume_state_schedules_non_tail_handoffs_but_not_tail_forwarding() {
        for (body, expected) in [
            ("rec outer(rec(cont) leaf(value))", 1),
            (
                "let result = rec(cont) outer(rec(cont) leaf(value)); result",
                2,
            ),
        ] {
            let module = d(&format!(
                "module x; rec(loop) {{ fn outer(value: .) -> . {{ {body} }}; \
                 fn leaf(value: .) -> . {{ value }} }}"
            ));
            let mut schedules = 0;
            visit_desugared_expr(desugared_fn_body(&module, "outer"), &mut |expr| {
                if matches!(expr, Expr::Call { callee, .. }
                    if matches!(callee.as_ref(), Expr::Path { segments, .. }
                        if segments.last().is_some_and(|segment| segment.as_str() == "mk_resume")))
                {
                    schedules += 1;
                    let Expr::Call { args, .. } = expr else {
                        unreachable!()
                    };
                    let Some(CallArg::Value(Expr::FnExpr { sig, .. })) = args.last() else {
                        panic!("a scheduling state carries its nullary thunk")
                    };
                    assert!(sig.params.is_empty());
                    assert_eq!(
                        sig.groups,
                        vec![crate::ast::SignatureGroupKind::Value { len: 0 }]
                    );
                }
            });
            assert_eq!(schedules, expected);
        }
        let module = d("module x; rec(loop) fn tail(value: .) -> . { rec tail(value) }");
        assert!(
            !module
                .items
                .iter()
                .any(|item| matches!(item, Item::TypeRecGroup(_)))
        );
    }

    fn generated_rec_state_newtype(module: &Module<Desugared>) -> &Newtype<Desugared> {
        for item in &module.items {
            match item {
                Item::Newtype(newtype) if newtype.name.starts_with("Rec_state") => {
                    return newtype;
                }
                Item::TypeRecGroup(group) => {
                    if let Some(newtype) = group.members.iter().find_map(|member| match member {
                        crate::ast::TypeRecMember::Newtype(newtype)
                            if newtype.name.starts_with("Rec_state") =>
                        {
                            Some(newtype)
                        }
                        _ => None,
                    }) {
                        return newtype;
                    }
                }
                _ => {}
            }
        }
        panic!("recursive lowering emits a state wrapper")
    }

    fn assert_rec_wrapper_avoids_outside_source(outside: impl FnOnce(&str, &str) -> String) {
        let group_start = rec_group_start();
        let wrapper = format!("Rec_state{group_start}_mghgp");
        let constructor = format!("mk_rec_state{group_start}_mghgp");
        let module = d(&format!(
            "module x;\n\
             rec(loop) fn go[A](value: A) -> A {{ value }}\n\
             {}\n",
            outside(&wrapper, &constructor)
        ));
        let generated = generated_rec_state_newtype(&module);

        assert_eq!(generated.name, format!("{wrapper}_n2"));
    }

    #[test]
    fn rec_wrapper_type_name_avoids_an_outside_group_type_head() {
        assert_rec_wrapper_avoids_outside_source(|wrapper, _| {
            format!("fn outside(value: {wrapper}) -> {wrapper} {{ value }}")
        });
    }

    #[test]
    fn rec_wrapper_type_name_avoids_an_outside_group_member_path_head() {
        assert_rec_wrapper_avoids_outside_source(|wrapper, constructor| {
            format!("fn outside(value: .) -> . {{ {wrapper}.{constructor}(value) }}")
        });
    }

    #[test]
    fn rec_wrapper_type_name_avoids_an_outside_group_ufcs_member_head() {
        assert_rec_wrapper_avoids_outside_source(|wrapper, constructor| {
            format!("fn outside(value: .) -> . {{ value.>{wrapper}.{constructor} }}")
        });
    }

    #[test]
    fn rec_wrapper_type_name_avoids_an_outside_group_bare_type_capture() {
        assert_rec_wrapper_avoids_outside_source(|wrapper, _| {
            format!("elab demo : . -> . {{ captures {wrapper}; impl implementation; }};")
        });
    }

    #[test]
    fn rec_generated_type_names_retry_past_module_namespace_collisions() {
        let group_start = rec_group_start();
        let wrapper = format!("Rec_state{group_start}_mghgp");
        let outer = format!("R{group_start}_i0_peb");
        let module = d(&format!(
            "module x;\n\
             rec(loop) fn go[A](value: A, nominal: {outer}) -> A {{\n\
               let next = rec(cont) go(value, nominal);\n\
               next\n\
             }}\n\
             type {wrapper} = .;\n\
             type {wrapper}_n2 = .;\n\
             type {outer} = .;\n\
             type {outer}_n2 = .;\n"
        ));
        let generated = generated_rec_state_newtype(&module);

        assert_eq!(generated.name, format!("{wrapper}_n3"));
        assert_eq!(generated.type_params.len(), 1);
        assert_eq!(generated.type_params[0].name, format!("{outer}_n3"));
    }

    #[cfg(feature = "prime")]
    #[test]
    fn rec_wrapper_type_name_round_trips_through_fresh_prime_validation() {
        use crate::backends::kio_prime::emit_module;
        use crate::pass::full::FullPipeline;
        use crate::pass::resolve::Package;
        use crate::pipeline::Pipeline;
        use crate::prime::pipeline::PrimePipeline;
        use std::path::{Path, PathBuf};

        let prefix = "module x;\n\
             host fn loop[S][R](step: S -> S | R, state: S) -> R;\n\
             rec(loop) fn go(value: .) -> . { rec go(value) }\n";
        let parsed_prefix = parse(prefix).expect("parse preceding host and recursive group");
        let group_start = parsed_prefix
            .items
            .iter()
            .find_map(|item| match item {
                Item::RecGroup(group, _) => Some(group.meta.span.start),
                _ => None,
            })
            .expect("recursive group span");
        let wrapper = format!("Rec_state{group_start}_mghgp");
        let constructor = format!("mk_rec_state{group_start}_mghgp");
        let projector = format!("un_rec_state{group_start}_mghgp");
        let source = format!(
            "{prefix}\
             type {wrapper} = .;\n\
             type {wrapper}_n2 = .;\n"
        );
        let parsed = parse(&source).expect("parse recursive source");
        let (lowered, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("x.kio"), parsed)], None)
                .expect("lower recursive source");
        let package = Package::build(Path::new(""), lowered, None)
            .expect("assemble recursive source package");
        package
            .resolve_imports()
            .expect("resolve recursive source uses");
        package
            .check_binding_origins()
            .expect("recursive source bindings remain distinct");
        package
            .check_no_value_cycles()
            .expect("recursive source has no value cycle");
        package
            .check_in_body_resolution()
            .expect("resolve recursive source bodies");
        let prime = FullPipeline::typecheck(&package).expect("typecheck recursive source");
        let emitted = emit_module(&prime.module("x").expect("module x").module);
        assert!(
            emitted.contains(&format!("newtype {wrapper}_n3")),
            "emitted Kio' must retain the allocated recursive wrapper: {emitted}"
        );
        assert!(
            emitted.contains(&format!("{wrapper}_n3.{constructor}"))
                && emitted.contains(&format!("{wrapper}_n3.{projector}")),
            "every generated wrapper use must name the allocated owner: {emitted}"
        );

        let reparsed = parse(&emitted).expect("emitted recursive Kio' re-parses");
        let (fresh_prime, _) =
            PrimePipeline::lower_package(vec![(PathBuf::from("x.kio"), reparsed)], None)
                .expect("emitted recursive source remains Kio'-shaped");
        let fresh_package = Package::build(Path::new(""), fresh_prime, None)
            .expect("assemble freshly parsed recursive Kio'");
        fresh_package
            .resolve_imports()
            .expect("fresh recursive Kio' uses resolve");
        fresh_package
            .check_binding_origins()
            .expect("fresh recursive Kio' bindings remain distinct");
        fresh_package
            .check_no_value_cycles()
            .expect("fresh recursive Kio' has no value cycle");
        fresh_package
            .check_in_body_resolution()
            .expect("fresh recursive Kio' bodies resolve");
        PrimePipeline::typecheck(&fresh_package)
            .expect("standalone Prime validation accepts the recursive artifact");
    }

    #[test]
    fn rec_outer_type_name_avoids_nested_group_binders() {
        let group_start = rec_group_start();
        let outer = format!("R{group_start}_i0_peb");
        let fresh_outer = format!("{outer}_n2");
        let module = d(&format!(
            "module x;\n\
             rec(loop) fn go[A](value: A) -> A {{\n\
               let escaped = .[{outer}](shadow: {outer}) -> A {{\n\
                 value\n\
               }};\n\
               let next = rec(cont) go(value);\n\
               next\n\
             }}\n"
        ));
        let generated = generated_rec_state_newtype(&module);

        assert_eq!(generated.type_params.len(), 1);
        assert_eq!(generated.type_params[0].name, fresh_outer);

        let wrapper = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == "go" => Some(def),
                _ => None,
            })
            .expect("recursive lowering emits the ordinary wrapper function");
        let [SignatureParam::Type(param), SignatureParam::Value(value)] =
            wrapper.sig.params.as_slice()
        else {
            panic!("wrapper retains one type and one value parameter")
        };
        assert_eq!(param.name, fresh_outer);
        assert!(matches!(
            value.ty.as_ref(),
            Some(Type::Path { segments, args, .. })
                if args.is_empty() && segments.len() == 1
                    && segments[0].as_str() == fresh_outer
        ));
        assert!(matches!(
            &wrapper.ret,
            Type::Path { segments, args, .. }
                if args.is_empty() && segments.len() == 1
                    && segments[0].as_str() == fresh_outer
        ));
    }

    #[test]
    fn tuple_two_lowers_to_mk_pair() {
        let m = d("module x; fn f() -> . { (.t, .f) }");
        // Auto-injected `import __intrinsics__;`.
        assert!(
            m.imports
                .iter()
                .any(|u| matches!(u.kind, ImportKind::Intrinsics))
        );
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Call { callee, args, .. } => {
                    if let Expr::Path { segments, .. } = callee.as_ref() {
                        assert_eq!(segments, &vec!["__pair__".to_string()]);
                    } else {
                        panic!("expected __pair__ Path callee");
                    }
                    // Two `Type::Infer` placeholders make the tuple's
                    // omitted intrinsic type arguments explicit in Lowered,
                    // followed by its two ordinary value arguments. Call
                    // planning resolves both placeholders structurally from
                    // those values before substitution produces Prime.
                    assert_eq!(args.len(), 4);
                    assert!(matches!(
                        args[0],
                        CallArg::Type(crate::ast::Type::Infer { .. })
                    ));
                    assert!(matches!(
                        args[1],
                        CallArg::Type(crate::ast::Type::Infer { .. })
                    ));
                    assert!(matches!(args[2], CallArg::Value(_)));
                    assert!(matches!(args[3], CallArg::Value(_)));
                }
                other => panic!("expected Call, got {other:?}"),
            }
        }
    }

    #[test]
    fn tuple_three_right_folds_to_mk_pair_chain() {
        let m = d("module x; fn f() -> . { (.t, .f, .t) }");
        if let Item::FnDef(d) = &m.items[0] {
            // Outer call: __pair__(_, _, .t, [inner])
            let Expr::Call { args: outer, .. } = &d.body else {
                panic!("expected outer Call");
            };
            assert_eq!(outer.len(), 4); // 2 type-arg placeholders + 2 value args
            // Inner call sits at the last arg position.
            let CallArg::Value(inner) = &outer[3] else {
                panic!("expected value arg at slot 3");
            };
            assert!(matches!(inner, Expr::Call { .. }));
        }
    }

    #[test]
    fn projected_user_call_survives_desugar_with_ordinary_children() {
        let mut module = parsed(
            "module x; elab invoke : (. -> .) -> . { trailing thunk; impl implementation } fn f() -> . { invoke! { (); () } }",
        );
        let scope = crate::pass::surface_registry::PackageBlockScope::from_modules(&[(
            std::path::PathBuf::from("x.kio"),
            module.clone(),
        )])
        .expect("descriptor scope");
        crate::pass::block_projection::project_module(&mut module, &scope).expect("projection");
        let module = desugar_module(module).expect("desugar projected ordinary call");
        let Expr::UserElaborator { args, form, .. } = desugared_fn_body(&module, "f") else {
            panic!("ordinary user call")
        };
        assert_eq!(*form, crate::ast::UserElaboratorCallForm::TrailingBlocks);
        let [CallArg::Value(Expr::FnExpr { body, .. })] = args.as_slice() else {
            panic!("one projected thunk")
        };
        assert!(matches!(body.as_ref(), Expr::Seq { .. }));
    }

    #[test]
    fn match_ufcs_survives_desugar() {
        let m = d("module x; fn f(v: I32 | String) -> . { \
             v.>match!((.(n: I32) { () }, .(s: String) { () }), ()) }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        let Expr::Ufcs {
            receiver,
            callee_segments,
            args,
            flavor,
            bang,
            ..
        } = &d.body
        else {
            panic!("expected match UFCS, got {:?}", d.body);
        };
        assert!(matches!(receiver.as_ref(), Expr::Path { .. }));
        assert_eq!(callee_segments, &vec!["match".to_owned()]);
        assert_eq!(*flavor, crate::ast::UfcsFlavor::ReceiverFirst);
        assert!(bang.is_some());
        assert_eq!(args.len(), 2);
        assert!(matches!(args[0], CallArg::Value(Expr::Call { .. })));
        assert!(matches!(args[1], CallArg::Value(Expr::Unit { .. })));
    }

    // A recursive direct-lambda argument to a user elaborator is staged for
    // checked-recipe tail classification. Desugaring grants no authority to
    // the `match!` spelling, so this fixture reaches that generic path.
    #[test]
    fn rec_call_in_match_clause_lowers_in_tail_position() {
        d("module x; rec(loop) fn go(v: I32 | String) -> I32 { \
             match!(v, (.(n: I32) { n }, .(_s: String) { rec go(__left__(I32, String, 0)) })) }");
    }

    #[test]
    fn placeholder_clauses_in_direct_bang_call_remain_deferred_candidates() {
        let module = d("module x; rec(loop) fn go(value: .) -> . { \
             choose!((.x. { x1 }, .x. { rec go(x1) })) }");
        assert_one_optional_and_one_required_deferred_tail(desugared_fn_body(&module, "go"));
    }

    #[test]
    fn deferred_tail_rec_lambda_requires_continuation_state() {
        let module = parsed(
            "module x; rec(loop) fn go(value: .) -> . { \
             hold!(.(_unit: .) { rec go(value) }) }",
        );
        let Item::RecGroup(group, _) = &module.items[0] else {
            panic!("expected rec group")
        };
        assert!(rec_group_uses_mode(group, RecCallMode::Cont));
    }

    #[test]
    fn placeholder_lambda_rec_requires_continuation_state() {
        let module = parsed(
            "module x; rec(loop) fn go(value: .) -> . { \
             apply(.x. { rec go(x1) }) }",
        );
        let Item::RecGroup(group, _) = &module.items[0] else {
            panic!("expected rec group")
        };
        assert!(rec_group_uses_mode(group, RecCallMode::Cont));
    }

    #[test]
    fn nested_rec_call_argument_requires_continuation_state() {
        let module = parsed(
            "module x; rec(loop) { \
             fn outer(value: .) -> . { rec outer(rec(cont) leaf(value)) }; \
             fn leaf(value: .) -> . { value } \
             }",
        );
        let Item::RecGroup(group, _) = &module.items[0] else {
            panic!("expected rec group")
        };
        assert!(rec_group_uses_mode(group, RecCallMode::Cont));
    }

    #[test]
    fn ordinary_tail_rec_group_does_not_require_continuation_state() {
        let module = parsed("module x; rec(loop) fn go(value: .) -> . { rec go(value) }");
        let Item::RecGroup(group, _) = &module.items[0] else {
            panic!("expected rec group")
        };
        assert!(!rec_group_uses_mode(group, RecCallMode::Cont));
    }

    fn projected_sequence_desugar(src: &str) -> Result<Module<Desugared>, Error> {
        let mut module = parsed(src);
        let provider = parsed(include_str!(
            "../../../../test-data/poc/elab/workdir/sequence.kio"
        ));
        let module_file = format!("{}.kio", module.path.segments.join("/"));
        let scope = crate::pass::surface_registry::PackageBlockScope::from_modules(&[
            (std::path::PathBuf::from(module_file), module.clone()),
            (std::path::PathBuf::from("sequence.kio"), provider),
        ])
        .expect("sequence descriptor scope");
        crate::pass::block_projection::project_module(&mut module, &scope)
            .expect("project ordinary sequence call");
        desugar_module(module)
    }

    #[test]
    fn monadic_do_recursion_before_first_continuation_lowers() {
        for body in [
            "do! bind { let next <- rec(cont) go(value); next }",
            "do! bind { let next = rec(cont) go(value); let out <- next; out }",
            "do! bind { rec(cont) go(value); value }",
            "do! bind { do! bind { let next <- rec(cont) go(value); next } }",
        ] {
            projected_sequence_desugar(&format!(
                "module x; import sequence(do); rec(loop) fn go(value: .) -> . {{ {body} }}"
            ))
            .expect("desugar");
        }
    }

    #[test]
    fn do_contract_receiver_retains_recursion_syntax_checks() {
        for (receiver, expected) in [
            ("rec(cont) missing(value)", "not a member"),
            ("rec factory(value)", "not in tail position"),
            ("rec(cont, poly) factory(T, value)", "only needed"),
            ("rec(cont) factory(., ())", "changes type parameters"),
            ("rec(cont) factory()", "expects 1 value"),
            ("rec(cont) factory(factory)", "cannot be used as a value"),
            (
                "rec(cont) factory(rec(cont) missing(value))",
                "not a member",
            ),
        ] {
            let source = format!(
                "module main; import sequence(do);
                rec(loop) fn factory[T](value: T) -> [A][B](A & (A -> B)) -> B {{
                    do! ({receiver}) {{ .[A][B](value: A, next: A -> B) {{ next(value) }} }}
                }}"
            );
            let error = projected_sequence_desugar(&source).expect_err(receiver);
            assert!(error.diag().1.contains(expected), "{receiver}: {error:?}");
        }
    }

    #[test]
    fn monadic_do_receiver_closures_cannot_capture_recursive_calls() {
        for body in [
            "let ignored = rec(cont) go(value); next(Box.unbox(input))",
            "(.x.{ let ignored = rec(cont) go(value); next(x1) })(Box.unbox(input))",
        ] {
            let source = format!(
                "module main; import sequence(do);
                newtype Box[A] : A {{ constructor box; projector unbox; }};
                rec(loop) fn go(value: .) -> Box(.) {{
                    let receiver = .[A][B](input: Box(A), next: A -> Box(B)) -> Box(B) {{ {body} }};
                    do! receiver {{ Box.box(()) }}
                }}"
            );
            let error = projected_sequence_desugar(&source).expect_err(body);
            assert!(error.diag().1.contains("rec(escape)"), "{body}: {error:?}");
        }
    }

    #[test]
    fn monadic_do_recursion_inside_continuation_stays_rejected() {
        for body in [
            "do! bind { let first <- value; rec(cont) go(first) }",
            "do! bind { let first <- value; let next <- rec(cont) go(first); next }",
            "do! bind { value; rec(cont) go(value) }",
            "do! bind { let next <- .(arg: .) { rec(cont) go(arg) }; value }",
        ] {
            let source = format!(
                "module x; import sequence(do); rec(loop) fn go(value: .) -> . {{ {body} }}"
            );
            let err = projected_sequence_desugar(&source).expect_err("escaping recursive call");
            let (_, message) = err.diag();
            assert!(message.contains("rec(escape)"), "{body}: {message}");
        }
    }

    #[test]
    fn monadic_do_first_rhs_recursion_still_requires_cont_annotation() {
        let source = "module x; import sequence(do); rec(loop) fn go(value: .) -> . { \
            do! bind { let next <- rec go(value); next } }";
        let err = projected_sequence_desugar(source).expect_err("non-tail recursive call");
        let (_, message) = err.diag();
        assert!(message.contains("not in tail position"), "{message}");
    }

    #[test]
    fn rec_group_clones_refresh_replacement_node_ids() {
        let module = d("module x; \
            host fn loop[S][R](step: S -> S | R, state: S) -> R; \
            rec(loop) { \
              fn first(value: .) -> . { rec second(value) }; \
              fn second(value: .) -> . { \
                choose!(.(local: .) { choose!(value.?{field}.>identity) }) \
              } \
            }");
        let mut field_ids = Vec::new();
        let mut user_ids = Vec::new();
        let mut ufcs_ids = Vec::new();
        for item in &module.items {
            let Item::FnDef(def) = item else {
                continue;
            };
            visit_desugared_expr(&def.body, &mut |expr| match expr {
                Expr::Elaborator { ext, .. } => field_ids.push(*ext),
                Expr::UserElaborator { ext, .. } => user_ids.push(*ext),
                Expr::Ufcs { ext, .. } => ufcs_ids.push(*ext),
                _ => {}
            });
        }
        for (kind, ids) in [
            ("field", field_ids),
            ("user elaborator", user_ids),
            ("UFCS", ufcs_ids),
        ] {
            assert!(
                ids.len() > 1,
                "recursive lowering must clone the {kind} node"
            );
            assert_eq!(
                ids.iter()
                    .copied()
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                ids.len(),
                "each cloned {kind} node needs its own replacement identity"
            );
        }
    }

    fn assert_rec_group_clones_refresh_label_value_node_ids(source: &str) {
        let module = d(source);
        let mut ids = Vec::new();
        for item in &module.items {
            let Item::FnDef(def) = item else {
                continue;
            };
            visit_desugared_expr(&def.body, &mut |expr| {
                if let Expr::LabelValue { ext, .. } = expr {
                    ids.push(*ext);
                }
            });
        }
        assert!(
            ids.len() > 1,
            "recursive lowering must clone the label value"
        );
        assert_eq!(
            ids.iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            ids.len(),
            "each cloned label value needs its own replacement identity"
        );
    }

    #[test]
    fn tail_rec_group_clones_refresh_label_value_node_ids() {
        assert_rec_group_clones_refresh_label_value_node_ids(
            "module x; \
             host fn loop[S][R](step: S -> S | R, state: S) -> R; \
             rec(loop) { \
               fn first(value: .) -> . { rec second(value) }; \
               fn second(value: .) -> . { \
                 let wrapped = {field = value}; \
                 rec first(wrapped.?{field}) \
               } \
             }",
        );
    }

    #[test]
    fn continuation_rec_group_clones_refresh_label_value_node_ids() {
        assert_rec_group_clones_refresh_label_value_node_ids(
            "module x; \
             host fn loop[S][R](step: S -> S | R, state: S) -> R; \
             rec(loop) { \
               fn first(value: .) -> . { rec second(value) }; \
               fn second(value: .) -> . { \
                 let wrapped = {field = rec(cont) first(value)}; \
                 wrapped.?{field} \
               } \
             }",
        );
    }

    // Bang-UFCS reaches the same structural staging path; receiver placement
    // does not distinguish the elaborator or its lambda arguments.
    #[test]
    fn rec_call_in_match_ufcs_clause_lowers_in_tail_position() {
        d("module x; rec(loop) fn go(v: I32 | String) -> I32 { \
             v.>match!((.(n: I32) { n }, .(_s: String) { rec go(__left__(I32, String, 0)) })) }");
    }

    #[test]
    fn placeholder_clauses_in_ufcs_bang_call_remain_deferred_candidates() {
        let module = d("module x; rec(loop) fn go(value: .) -> . { \
             value.>choose!((.x. { x1 }, .x. { rec go(x1) })) }");
        assert_one_optional_and_one_required_deferred_tail(desugared_fn_body(&module, "go"));
    }

    // A `rec(cont)` occurrence in the staged lambda uses the same CPS source
    // lowering before the checked recipe decides the candidate's final role.
    #[test]
    fn rec_cont_call_in_match_clause_lowers() {
        d("module x; rec(loop) fn go(v: I32 | String) -> I32 { \
             match!(v, (.(n: I32) { n }, .(_s: String) { add(1, rec(cont) go(__left__(I32, String, 0))) })) }");
    }

    #[test]
    fn deferred_tail_parameter_patterns_keep_their_projection_bindings() {
        for (pattern, names, recur) in [
            (
                "((left: I32, right: I32), tail: I32)",
                vec!["left", "right", "tail"],
                "((left, right), tail)",
            ),
            (
                "whole: (nested: (left: I32, right: I32), tail: I32)",
                vec!["nested", "left", "right", "tail"],
                "whole",
            ),
            (
                "((left: I32, _: I32), tail: I32)",
                vec!["left", "tail"],
                "((left, left), tail)",
            ),
        ] {
            let module = d(&format!(
                "module x; rec(loop) fn go(value: (I32 & I32) & I32) -> I32 {{ \
                 choose!(.({pattern}) {{ rec go({recur}) }}) }}"
            ));
            let mut bound = std::collections::HashSet::new();
            visit_desugared_expr(desugared_fn_body(&module, "go"), &mut |expr| match expr {
                Expr::Let { name, .. } => {
                    bound.insert(name.as_str());
                }
                Expr::RecOrder { plan, .. } => {
                    bound.insert(plan.name.as_str());
                }
                _ => {}
            });
            for name in names {
                assert!(
                    bound.contains(name),
                    "{pattern}: missing projection binder {name}"
                );
            }
        }
    }

    #[test]
    fn deferred_tail_parameter_patterns_do_not_permit_escaping_recursion() {
        let module = parsed(
            "module x; rec(loop) fn go(value: I32 & I32) -> I32 { \
             choose!(.((left: I32, right: I32)) { \
               .() { rec go((left, right)) } \
             }) }",
        );
        let error = desugar_module(module).expect_err("nested closure cannot capture recursion");
        assert!(error.diag().1.contains("rec(escape)"), "{error:?}");
    }

    #[test]
    fn non_tail_deferred_lambdas_share_one_capture_safe_continuation_shell() {
        let module = d("module x;
            rec(loop) fn go(outer: I32, step: Many) -> I32 {
              let selected = match!(step, (
                .(outer: Zero) { zero() },
                .(_first: First) { add(rec(cont) go(10, step), 1) },
                .(_second: Second) { add(rec(cont) go(10, step), 2) },
                .(_third: Third) { add(rec(cont) go(10, step), 3) },
                .(_fourth: Fourth) { add(rec(cont) go(10, step), 4) },
                .(_fifth: Fifth) { add(rec(cont) go(10, step), 5) }
              ));
              add(outer, selected)
            }");
        let body = desugared_fn_body(&module, "go");
        let mut shells = Vec::new();
        visit_desugared_expr(body, &mut |expr| {
            let Expr::RecOrder { plan, .. } = expr else {
                return;
            };
            if is_deferred_continuation_function_shell(plan) {
                shells.push(plan.as_ref().clone());
            }
        });
        assert_eq!(
            shells.len(),
            1,
            "the marked call retains one outer continuation"
        );
        let shell = &shells[0];
        assert!(matches!(shell.value.as_ref(), Expr::FnExpr { .. }));
        let elaborated = deferred_continuation_elaborated_value(shell);
        assert!(matches!(
            elaborated,
            Expr::UserElaborator { name, .. } if name == "match"
        ));

        let mut candidates = Vec::new();
        visit_desugared_expr(elaborated, &mut |expr| {
            let Expr::RecOrder { plan, .. } = expr else {
                return;
            };
            if matches!(plan.disposition, RecOrderDisposition::DeferredTail { .. }) {
                candidates.push(plan.as_ref().clone());
            }
        });
        assert_eq!(candidates.len(), 6);
        assert_eq!(
            candidates
                .iter()
                .filter(|plan| matches!(
                    plan.disposition,
                    RecOrderDisposition::DeferredTail {
                        requirement: RecOrderTailRequirement::Optional,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(
            candidates
                .iter()
                .filter(|plan| matches!(
                    plan.disposition,
                    RecOrderDisposition::DeferredTail {
                        requirement: RecOrderTailRequirement::Required,
                        ..
                    }
                ))
                .count(),
            5
        );
        for candidate in candidates {
            let value_uses = desugared_path_use_count(&candidate.value, &shell.name);
            let body_uses = desugared_path_use_count(&candidate.body, &shell.name);
            match candidate.disposition {
                RecOrderDisposition::DeferredTail {
                    requirement: RecOrderTailRequirement::Required,
                    ..
                } => assert!(
                    value_uses > 0,
                    "a required candidate must retain the shared continuation in its recursive payload"
                ),
                RecOrderDisposition::DeferredTail {
                    requirement: RecOrderTailRequirement::Optional,
                    ..
                } => assert!(
                    body_uses > 0,
                    "an optional candidate must apply the shared continuation in its compact body"
                ),
                RecOrderDisposition::Ordered(_) => unreachable!(),
            }
        }
        assert_eq!(
            desugared_path_use_count(&shell.value, &shell.name),
            0,
            "the retained continuation body must not recursively capture its own generated binding"
        );
    }

    #[test]
    fn non_tail_deferred_ufcs_lambdas_use_the_same_function_shell() {
        let module = d("module x;
            rec(loop) fn go(outer: I32, step: Left | Right) -> I32 {
              let selected = step.>match!((
                .(_left: Left) { 0 },
                .(_right: Right) { add(rec(cont) go(outer, step), 1) }
              ));
              add(outer, selected)
            }");
        let body = desugared_fn_body(&module, "go");
        let mut shells = Vec::new();
        visit_desugared_expr(body, &mut |expr| {
            let Expr::RecOrder { plan, .. } = expr else {
                return;
            };
            if is_deferred_continuation_function_shell(plan) {
                shells.push(plan.as_ref().clone());
            }
        });
        assert_eq!(shells.len(), 1);
        let elaborated = deferred_continuation_elaborated_value(&shells[0]);
        assert!(matches!(
            elaborated,
            Expr::Ufcs { callee_segments, bang: Some(_), .. }
                if callee_segments.len() == 1 && callee_segments[0].as_str() == "match"
        ));
        let mut deferred = 0;
        visit_desugared_expr(elaborated, &mut |expr| {
            if matches!(
                expr,
                Expr::RecOrder { plan, .. }
                    if matches!(plan.disposition, RecOrderDisposition::DeferredTail { .. })
            ) {
                deferred += 1;
            }
        });
        assert_eq!(deferred, 2);
    }

    #[derive(Clone, Copy, Debug)]
    struct DeferredContinuationScalingSample {
        width: usize,
        continuation_size: usize,
        depth: usize,
        /// Exact `Expr<Desugared>` node count in the final function-body
        /// tree. This excludes type nodes, heap bytes/allocations, and
        /// desugaring visits or other transformation work.
        final_nodes: usize,
        shell_carriers: usize,
        elaborated_value_carriers: usize,
        deferred_candidates: usize,
        retained_continuation_roots: usize,
        downstream_calls: usize,
    }

    fn deferred_continuation_scaling_source(
        width: usize,
        continuation_size: usize,
        depth: usize,
    ) -> String {
        let candidates = (0..width)
            .map(|index| format!(".(value{index}: I32) {{ rec(cont) go(value{index}) }}"))
            .collect::<Vec<_>>();
        let staged = if width == 1 {
            format!("hold!({})", candidates[0])
        } else {
            format!("hold!(({}))", candidates.join(", "))
        };
        let bindings = (0..depth)
            .map(|index| format!("let selected{index} = {staged};"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut downstream = format!("selected{}", depth - 1);
        for _ in 0..continuation_size {
            downstream = format!("after({downstream})");
        }
        format!("module x; rec(loop) fn go(value: I32) -> I32 {{ {bindings} {downstream} }}")
    }

    fn deferred_continuation_scaling_sample(
        width: usize,
        continuation_size: usize,
        depth: usize,
    ) -> DeferredContinuationScalingSample {
        let module = d(&deferred_continuation_scaling_source(
            width,
            continuation_size,
            depth,
        ));
        let body = desugared_fn_body(&module, "go");
        let mut shell_carriers = 0;
        let mut elaborated_value_carriers = 0;
        let mut deferred_candidates = 0;
        let mut retained_continuation_roots = 0;
        let mut downstream_calls = 0;
        visit_desugared_expr(body, &mut |expr| match expr {
            Expr::RecOrder { plan, .. } if is_deferred_continuation_function_shell(plan) => {
                shell_carriers += 1;
                retained_continuation_roots +=
                    usize::from(matches!(plan.value.as_ref(), Expr::FnExpr { .. }));
            }
            Expr::RecOrder { plan, .. }
                if matches!(plan.disposition, RecOrderDisposition::DeferredTail { .. }) =>
            {
                deferred_candidates += 1;
            }
            Expr::RecOrder { plan, .. }
                if plan.disposition
                    == RecOrderDisposition::Ordered(RecOrderTypeFlow::ElaboratedValue) =>
            {
                elaborated_value_carriers += 1;
            }
            expr if is_call_to(expr, "after") => downstream_calls += 1,
            _ => {}
        });
        DeferredContinuationScalingSample {
            width,
            continuation_size,
            depth,
            final_nodes: desugared_expr_node_count(body),
            shell_carriers,
            elaborated_value_carriers,
            deferred_candidates,
            retained_continuation_roots,
            downstream_calls,
        }
    }

    #[test]
    fn deferred_continuation_generated_tree_scales_without_candidate_times_body_copying() {
        let mut samples = Vec::new();
        for width in [1, 8, 32, 128] {
            for continuation_size in [1, 16] {
                for depth in [1, 2, 4] {
                    let sample =
                        deferred_continuation_scaling_sample(width, continuation_size, depth);
                    assert_eq!(sample.shell_carriers, depth);
                    assert_eq!(sample.elaborated_value_carriers, depth);
                    assert_eq!(sample.deferred_candidates, width * depth);
                    assert_eq!(sample.retained_continuation_roots, depth);
                    assert_eq!(
                        sample.downstream_calls, continuation_size,
                        "the sole downstream continuation body must occur once in the final tree"
                    );
                    samples.push(sample);
                }
            }
        }
        for sample in samples {
            // The final tree has twenty-five nodes per staged candidate at each
            // nesting level, three nodes per continuation shell (the outer
            // shell, its elaborated-value carrier, and that carrier's result
            // path), forty fixed wrapper/dispatch nodes, and two nodes (callee
            // path + call) per downstream continuation step. In particular,
            // there is no downstream-size × width/depth term and no nonlinear
            // nesting beyond the authored width × depth candidate count.
            let expected_final_nodes = 25 * sample.width * sample.depth
                + 3 * sample.depth
                + 40
                + 2 * sample.continuation_size;
            assert_eq!(
                sample.final_nodes, expected_final_nodes,
                "the exact final-tree bound changed for {sample:?}"
            );
        }
    }

    #[test]
    fn annotation_free_recursive_order_bindings_have_one_lexical_use() {
        for (label, body, copied_shell) in [
            ("call operand", "take(supply(), rec(cont) go(value))", false),
            (
                "UFCS receiver",
                "supply().>take(rec(cont) go(value))",
                false,
            ),
            (
                "UFCS argument",
                "().>take(supply(), rec(cont) go(value))",
                false,
            ),
            (
                "tuple operand",
                "take((supply(), rec(cont) go(value)))",
                false,
            ),
            (
                "label operand",
                "take({first = supply(), second = rec(cont) go(value)})",
                false,
            ),
            (
                "deferred branches",
                "take(hold!((.() { rec(cont) go(value) }, .() { supply() })))",
                true,
            ),
        ] {
            let module = d(&format!(
                "module x; rec(loop) fn go(value: .) -> . {{ {body} }}"
            ));
            let mut pending_bindings = 0;
            let mut copied_shells = 0;
            visit_desugared_expr(desugared_fn_body(&module, "go"), &mut |expr| {
                let Expr::RecOrder { plan, .. } = expr else {
                    return;
                };
                let uses = desugared_path_use_count(&plan.body, &plan.name);
                if plan.annotation.is_none()
                    && matches!(
                        plan.disposition,
                        RecOrderDisposition::Ordered(RecOrderTypeFlow::ExpectedFromBody)
                            | RecOrderDisposition::DeferredTail {
                                lifted_flow: RecOrderTypeFlow::ExpectedFromBody,
                                ..
                            }
                    )
                {
                    pending_bindings += 1;
                    assert_eq!(uses, 1, "{label}: one pending value has one lexical use");
                } else if is_deferred_continuation_function_shell(plan) && uses > 1 {
                    copied_shells += 1;
                }
            });
            assert!(
                pending_bindings > 0,
                "{label}: the pending-binding witness must fire"
            );
            assert_eq!(
                copied_shells > 0,
                copied_shell,
                "{label}: copied continuations use typed shells"
            );
        }
    }

    fn quoted_source_with_one_operand<'a>(
        body: &'a Expr<Desugared>,
        effect_name: &str,
    ) -> &'a Expr<Desugared> {
        let mut expansions = Vec::new();
        let mut operands = Vec::new();
        let mut effect_calls = 0;
        visit_desugared_expr(body, &mut |expr| {
            effect_calls += usize::from(is_call_to(expr, effect_name));
            if let Expr::RecQuote { plan, .. } = expr {
                match plan.as_ref() {
                    crate::ast::RecQuotePlan::Expansion { value, .. } => {
                        expansions.push(value.as_ref())
                    }
                    crate::ast::RecQuotePlan::Operand { computation, .. } => {
                        operands.push(computation.as_ref())
                    }
                }
            }
        });
        assert_eq!(
            expansions.len(),
            1,
            "one quoted source has one adoption boundary"
        );
        assert_eq!(
            operands.len(),
            1,
            "the recursive computation is retained once"
        );
        assert_eq!(effect_calls, 1, "the authored effect must not be copied");
        let source = expansions[0];
        let mut quoted_effect_calls = 0;
        visit_desugared_expr(source, &mut |expr| {
            quoted_effect_calls += usize::from(is_call_to(expr, effect_name));
        });
        assert_eq!(
            quoted_effect_calls, 1,
            "the effect must not be hoisted outside quotation"
        );
        let Expr::FnExpr { sig, body, .. } = operands[0] else {
            panic!("a suspended operand retains its ordinary CPS function")
        };
        let [SignatureParam::Value(param)] = sig.params.as_slice() else {
            panic!("a suspended operand receives one local continuation")
        };
        assert_eq!(
            desugared_path_use_count(body, &param.name),
            1,
            "the recursive state receives the local continuation exactly once"
        );
        source
    }

    #[test]
    fn quoted_nested_call_retains_source_order_and_one_recursive_computation() {
        let module = d("module x; rec(loop) fn go(value: .) -> . { \
             hold!(take(supply(), rec(cont) go(value))) }");
        let source = quoted_source_with_one_operand(desugared_fn_body(&module, "go"), "supply");
        let Expr::UserElaborator { args, .. } = source else {
            panic!("the bang call remains the quoted source")
        };
        let [CallArg::Value(value)] = args.as_slice() else {
            panic!("one ordinary call is quoted")
        };
        assert!(is_call_to(value, "take"));
        let Expr::Call { args, .. } = value else {
            unreachable!()
        };
        assert!(
            matches!(args.as_slice(), [CallArg::Value(first), CallArg::Value(Expr::RecQuote { plan, .. })]
            if is_call_to(first, "supply") && matches!(plan.as_ref(), crate::ast::RecQuotePlan::Operand { .. })),
            "ordinary call operands retain their written order until typed call replay"
        );
    }

    #[test]
    fn deferred_tail_lifted_flow_respects_authored_return_information() {
        let module = d("module x;
             rec(loop) fn go(value: .) -> . {
               hold!((
                 .(_unit: .) { rec go(value) },
                 .(_unit: .) -> _ { rec go(value) },
                 .(_unit: .) { () },
                 .(_unit: .) -> _ { () },
                 .(_unit: .) -> Box(_) { make_box() },
                 .(_unit: .) -> . { () }
               ))
             }");
        let mut plans = Vec::new();
        visit_desugared_expr(desugared_fn_body(&module, "go"), &mut |expr| {
            let Expr::RecOrder { plan, .. } = expr else {
                return;
            };
            if matches!(plan.disposition, RecOrderDisposition::DeferredTail { .. }) {
                plans.push(plan.as_ref().clone());
            }
        });
        assert_eq!(plans.len(), 6);

        for plan in &plans[..=1] {
            assert!(matches!(
                plan.disposition,
                RecOrderDisposition::DeferredTail {
                    requirement: RecOrderTailRequirement::Required,
                    lifted_flow: RecOrderTypeFlow::SynthesizedValue,
                }
            ));
            assert!(plan.annotation.is_none());
        }
        assert_eq!(
            plans[0].runtime_ty, plans[1].runtime_ty,
            "a Required whole-inferred return must retain the omitted form's step-result runtime"
        );
        assert_eq!(plans[1].runtime_ty, plans[2].runtime_ty);

        for plan in &plans[2..=3] {
            assert!(matches!(
                plan.disposition,
                RecOrderDisposition::DeferredTail {
                    requirement: RecOrderTailRequirement::Optional,
                    lifted_flow: RecOrderTypeFlow::ExpectedFromBody,
                }
            ));
            assert!(
                plan.annotation.is_none(),
                "an omitted or whole-inferred return is one elided annotation class"
            );
        }

        for plan in &plans[2..] {
            assert_eq!(
                plan.body.span(),
                rec_generated_span(plan.value.span(), 20_000),
                "the compact continuation must use its generated call-plan key"
            );
            assert_ne!(
                plan.body.span(),
                plan.value.span(),
                "generated continuation metadata must not collide with the authored value"
            );
        }
        assert_ne!(
            plans[2].body.span(),
            plans[3].body.span(),
            "omitted and whole-inferred return twins retain distinct generated call-plan keys"
        );

        assert!(matches!(
            plans[4].disposition,
            RecOrderDisposition::DeferredTail {
                requirement: RecOrderTailRequirement::Optional,
                lifted_flow: RecOrderTypeFlow::SynthesizedValue,
            }
        ));
        assert!(matches!(
            plans[4].annotation.as_ref(),
            Some(Type::Path { args, .. })
                if matches!(args.as_slice(), [Type::Infer { .. }])
        ));

        assert!(matches!(
            plans[5].disposition,
            RecOrderDisposition::DeferredTail {
                requirement: RecOrderTailRequirement::Optional,
                lifted_flow: RecOrderTypeFlow::SynthesizedValue,
            }
        ));
        assert!(matches!(
            plans[5].annotation.as_ref(),
            Some(Type::Unit { .. })
        ));
    }

    #[test]
    fn deferred_continuation_edges_follow_the_constructed_binding_through_copy_and_rename() {
        fn check(body: &Expr<Desugared>) -> (usize, usize) {
            let mut count = 0;
            let mut bytes = 0;
            visit_desugared_expr(body, &mut |expr| {
                let Expr::RecOrder { plan: shell, .. } = expr else {
                    return;
                };
                if !is_deferred_continuation_function_shell(shell) {
                    return;
                }
                visit_desugared_expr(deferred_continuation_elaborated_value(shell), &mut |expr| {
                    let Expr::RecOrder { plan, .. } = expr else {
                        return;
                    };
                    if !matches!(plan.disposition, RecOrderDisposition::DeferredTail { .. }) {
                        return;
                    }
                    let edge = plan
                        .tail_continuation
                        .as_deref()
                        .expect("exact continuation edge");
                    let Expr::Path { segments, .. } = edge else {
                        panic!("lexical path edge")
                    };
                    assert_eq!(segments.len(), 1);
                    assert_eq!(segments[0].as_str(), shell.name);
                    assert!(
                        desugared_path_use_count(&plan.value, &shell.name)
                            + desugared_path_use_count(&plan.body, &shell.name)
                            > 0,
                        "the recorded edge must be the continuation actually used in the transformed body"
                    );
                    count += 1;
                    bytes += std::mem::size_of_val(edge)
                        + segments.capacity() * std::mem::size_of::<PathSegment>()
                        + segments
                            .iter()
                            .map(|segment| segment.name.capacity())
                            .sum::<usize>();
                });
            });
            (count, bytes)
        }
        let module = d(&deferred_continuation_scaling_source(2, 1, 1));
        let body = desugared_fn_body(&module, "go");
        let mut renames = std::collections::HashMap::new();
        visit_desugared_expr(body, &mut |expr| {
            if let Expr::RecOrder { plan, .. } = expr
                && is_deferred_continuation_function_shell(plan)
            {
                renames.insert(plan.name.clone(), format!("copied_{}", plan.name));
            }
        });
        assert!(!renames.is_empty());
        let copied = rename_paths(body.clone(), &renames);
        let original = check(body);
        let copied = check(&copied);
        assert_eq!(original.0, 2);
        assert_eq!(copied.0, 2);
        eprintln!(
            "two deferred edges: original payload bytes={}, copied/renamed payload bytes={}",
            original.1, copied.1
        );
    }

    #[test]
    fn rec_order_synthesized_value_preserves_the_written_partial_annotation() {
        let source = "module x;
            host type I32;
            host fn loop[S][R](step: S -> S | R, state: S) -> R;
            newtype Box[A] : A { pub constructor mk_box; pub projector un_box; };
            fn choose_box[A](tag: I32, value: Box(A)) -> Box(A) { value }
            rec(loop) fn value(n: I32) -> Box(I32) {
              let .(box: Box(_)) = choose_box(0, rec(cont) value(0));
              box
            }";
        let module = d(source);
        let source_infer = source
            .find("Box(_)")
            .expect("the fixture has one partial local annotation")
            + "Box(".len();
        let mut observations = Vec::new();
        visit_desugared_expr(desugared_fn_body(&module, "value"), &mut |expr| {
            let Expr::RecOrder { plan, .. } = expr else {
                return;
            };
            if plan.disposition != RecOrderDisposition::Ordered(RecOrderTypeFlow::SynthesizedValue)
            {
                return;
            }
            let Some(Type::Path { args, .. }) = plan.annotation.as_ref() else {
                panic!("SynthesizedValue must retain the source local annotation");
            };
            let [Type::Infer { meta, .. }] = args.as_slice() else {
                panic!("the retained Box annotation must contain its one source placeholder");
            };
            observations.push(meta.span);
        });
        assert_eq!(
            observations,
            [Span::new(
                u32::try_from(source_infer).expect("test source offset fits in u32"),
                u32::try_from(source_infer + 1).expect("test source offset fits in u32"),
            )],
            "RecOrder must transport the one written source occurrence, not synthesize another annotation category",
        );
    }

    #[test]
    fn rec_cps_bang_ufcs_keeps_non_inert_receiver_quoted() {
        let module = d("module x; rec(loop) fn go(value: .) -> . { \
             effect(()).>hold!(rec(cont) go(())) }");
        quoted_source_with_one_operand(desugared_fn_body(&module, "go"), "effect");
        let mut quoted_receiver_count = 0;
        visit_desugared_expr(desugared_fn_body(&module, "go"), &mut |expr| match expr {
            Expr::Ufcs {
                receiver,
                callee_segments,
                bang: Some(_),
                ..
            } if callee_segments.len() == 1 && callee_segments[0].as_str() == "hold" => {
                assert!(
                    is_call_to(receiver, "effect"),
                    "the non-inert bang-UFCS receiver must remain quoted"
                );
                quoted_receiver_count += 1;
                assert!(matches!(expr, Expr::Ufcs { args, .. }
                    if matches!(args.as_slice(), [CallArg::Value(Expr::RecQuote { plan, .. })]
                        if matches!(plan.as_ref(), crate::ast::RecQuotePlan::Operand { .. }))));
            }
            _ => {}
        });
        assert_eq!(quoted_receiver_count, 1, "expected one hold! UFCS call");
    }

    #[test]
    fn rec_cps_bang_ufcs_keeps_non_inert_written_arg_quoted() {
        let module = d("module x; rec(loop) fn go(value: .) -> . { \
             value.>hold!(effect(()), rec(cont) go(())) }");
        quoted_source_with_one_operand(desugared_fn_body(&module, "go"), "effect");
        let mut quoted_arg_count = 0;
        visit_desugared_expr(desugared_fn_body(&module, "go"), &mut |expr| match expr {
            Expr::Ufcs {
                callee_segments,
                args,
                bang: Some(_),
                ..
            } if callee_segments.len() == 1 && callee_segments[0].as_str() == "hold" => {
                assert!(
                    matches!(args.first(), Some(CallArg::Value(value)) if is_call_to(value, "effect")),
                    "the non-inert written bang-UFCS argument must remain quoted"
                );
                quoted_arg_count += 1;
                assert!(
                    matches!(args.as_slice(), [CallArg::Value(first), CallArg::Value(Expr::RecQuote { plan, .. })]
                    if is_call_to(first, "effect") && matches!(plan.as_ref(), crate::ast::RecQuotePlan::Operand { .. })),
                    "the effect and suspension retain their written argument order"
                );
            }
            _ => {}
        });
        assert_eq!(quoted_arg_count, 1, "expected one hold! UFCS call");
    }

    // A `rec` call in the `match!` scrutinee is *not* tail; the rec
    // lowering rejects a plain (non-`cont`) call there.
    #[test]
    fn rec_call_in_match_scrutinee_is_rejected() {
        let m = parse(
            "module x; rec(loop) fn go(v: I32 | String) -> I32 { \
             match!(rec go(__left__(I32, String, 0)), (.(n: I32) { n }, .(_s: String) { 0 })) }",
        )
        .expect("parse");
        let err = desugar_module(m).expect_err("expected non-tail rejection");
        let (_span, message) = err.diag();
        assert!(
            message.contains("not in tail position"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn derive_ufcs_survives_desugar() {
        let m = d("module x; fn f() -> Monad(Maybe) { \
             maybe_monad.>derive!(Monad(Maybe)) }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        let Expr::Ufcs {
            receiver,
            callee_segments,
            args,
            flavor,
            bang,
            ..
        } = &d.body
        else {
            panic!("expected derive UFCS, got {:?}", d.body);
        };
        assert!(matches!(receiver.as_ref(), Expr::Path { .. }));
        assert_eq!(callee_segments, &vec!["derive".to_owned()]);
        assert_eq!(*flavor, crate::ast::UfcsFlavor::ReceiverFirst);
        assert!(bang.is_some());
        assert_eq!(args.len(), 1);
        assert!(matches!(args[0], CallArg::Value(Expr::Call { .. })));
    }

    #[test]
    fn match_elaborator_triggers_intrinsics_injection() {
        // `match!` always elaborates to intrinsic calls (decision-tree
        // `__either__` / `__fst__` / `__snd__`); flag the import. The
        // catch-all `.()` clause exercises the smallest surface
        // shape that triggers the injection.
        let m = d("module x; fn f[A](v: A) -> . { match!(v, .() { () }) }");
        assert!(
            m.imports
                .iter()
                .any(|u| matches!(u.kind, ImportKind::Intrinsics))
        );
    }

    #[test]
    fn lift_triggers_intrinsics_injection() {
        // `into!` / `onto!` may elaborate to identity, but the
        // injection is conservative (an unused import is harmless).
        let m = d("module x; fn f() -> . { into!(()) }");
        assert!(
            m.imports
                .iter()
                .any(|u| matches!(u.kind, ImportKind::Intrinsics))
        );
    }

    #[test]
    fn multi_label_value_triggers_intrinsics_injection() {
        // Multi-label `{f = e, g = e'}` lowers to `__pair__(...)`
        // during label_elab; flag the import here in desugar, where the
        // surface form is still visible.
        let m = d("module x; fn f() -> . { {f = (), g = ()} }");
        assert!(
            m.imports
                .iter()
                .any(|u| matches!(u.kind, ImportKind::Intrinsics))
        );
    }

    #[test]
    fn single_label_value_does_not_trigger_intrinsics_injection() {
        // Single-label `{f = e}` lowers to a newtype constructor call;
        // which is not an intrinsic — no injection.
        let m = d("module x; fn f() -> . { {f = ()} }");
        assert!(
            !m.imports
                .iter()
                .any(|u| matches!(u.kind, ImportKind::Intrinsics))
        );
    }
}
