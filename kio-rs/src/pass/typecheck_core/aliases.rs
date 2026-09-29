//! TypeAlias unfolding, structural type equivalence, and the
//! `PayloadCtx` view that `newtype`-payload validation needs.
//!
//! [`AliasCtx`] bundles the two alias-lookup maps the structural
//! type-equality helpers need; [`unfold_top`] iterates alias
//! unfolding at the head of a type; [`type_equiv`] /
//! [`require_type_equiv`] decide structural equality up to that
//! unfolding. [`PayloadCtx`] composes [`AliasCtx`] with the
//! intra-module `newtype` map and is the input to
//! [`super::check_newtype_payload`] / [`super::compute_variance_env`]
//! in the variance sub-module.
//!
//! Extracted from [`super::typecheck_core`] for navigability;
//! the host types that build these views (`ModuleEnv` and the
//! `populate_*` helpers) still live in the umbrella.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::error::Error;
use crate::pass::resolve::{
    NominalProvider, NominalRoute, NominalScope as AliasNominalScope, NominalSelection,
};
use crate::span::Span;

use super::persistent_exact::{PersistentExactIndex, PersistentExactKey, PersistentExactSet};
use super::{display_type, source_type_spelling_with_lookup};

/// Read-only proof that a bare type head is a rigid binder in the caller's
/// exact scope.
///
/// Structural consumers that already own a proof-bearing scope can implement
/// this narrow query without copying every binder spelling into an
/// [`AliasCtx`]. The alias materializer uses it only where it already consults
/// [`AliasCtx::binder_locals`]; it grants no declaration-resolution authority.
pub(crate) trait AliasBinderLookup {
    fn contains_alias_binder(&self, name: &str) -> bool;
    fn for_each_alias_binder(&self, visit: &mut dyn FnMut(&str));
}

impl AliasBinderLookup for HashSet<String> {
    fn contains_alias_binder(&self, name: &str) -> bool {
        self.contains(name)
    }

    fn for_each_alias_binder(&self, visit: &mut dyn FnMut(&str)) {
        for name in self {
            visit(name);
        }
    }
}

struct OverlayAliasBinderLookup<'a> {
    ambient: Option<&'a dyn AliasBinderLookup>,
    lexical: &'a HashSet<String>,
}

impl AliasBinderLookup for OverlayAliasBinderLookup<'_> {
    fn contains_alias_binder(&self, name: &str) -> bool {
        self.lexical.contains(name)
            || self
                .ambient
                .is_some_and(|ambient| ambient.contains_alias_binder(name))
    }

    fn for_each_alias_binder(&self, visit: &mut dyn FnMut(&str)) {
        if let Some(ambient) = self.ambient {
            ambient.for_each_alias_binder(visit);
        }
        for name in self.lexical {
            if !self
                .ambient
                .is_some_and(|ambient| ambient.contains_alias_binder(name))
            {
                visit(name);
            }
        }
    }
}

impl<T: AliasBinderLookup + ?Sized> crate::pass::resolve::ContractBinderLookup for T {
    fn contains_contract_binder(&self, name: &str) -> bool {
        self.contains_alias_binder(name)
    }
}

/// Bundle the alias-lookup maps and intern pool the structural
/// type-equality helpers need: intra-module aliases declared in the
/// module being checked, plus cross-module type aliases brought in via
/// `import m(Foo);` where `Foo` is a `pub type`.
///
/// Both `typecheck_full` and the standalone Kio'-only walker in
/// [`crate::prime::typer`] build their own per-module env with these
/// two maps; passing them through this thin wrapper lets the shared
/// helpers `unfold_top` / `type_equiv` / `require_type_equiv` stay
/// phase-polymorphic without an env-shape dependency.
///
/// `'a` borrows the maps; `'m` is the lifetime of the data inside
/// (the module the typer is walking).
pub struct AliasCtx<'a, 'm, P>
where
    P: crate::ast::Phase,
{
    pub local: &'a HashMap<&'m str, AliasDef<'m, P>>,
    pub cross_module: &'a HashMap<&'m str, AliasDef<'m, P>>,
    pub type_interner: Option<&'a super::TypeInterner<P>>,
    /// The source module being checked, used to canonicalize a still-bare
    /// nominal head to its identity-exact `(module, name)` form before
    /// the exact comparison in [`type_equiv`]. A bare head
    /// resolves through this module's `import` clauses / local declarations
    /// via [`crate::pass::resolve::exported_contract_type_segments`]; an
    /// already-qualified head (the typer's synthesized self-type, an
    /// FFI path) is left untouched. This is deliberately independent of
    /// package membership: standalone and compiler-created auxiliary modules
    /// still have a source scope. `None` is reserved for ad-hoc contexts
    /// whose inputs are already canonical.
    pub source_module: Option<&'m crate::ast::Module<P>>,
    /// The whole package, so a **qualified** cross-module alias head
    /// (`widget.store.I32`) can be unfolded through its **declaring**
    /// module's own definition even when the checking module never
    /// selectively-imports it — `cross_module` only holds aliases the
    /// current scope imports by name. A `<module>.<name>`
    /// head whose bare-name lookup misses is resolved by `(module, name)`
    /// against this package: the leading segments name the declaring
    /// module, the last segment is the alias. `None` for ad-hoc contexts
    /// with no package (some unit tests), where a qualified head is left
    /// as is.
    ///
    /// Open-world-safe: the alias body resolves in its own declaring
    /// module's scope (`owner_module`), so adding a declaration to any
    /// *other* module cannot change what a qualified head unfolds to, and
    /// two same-named aliases from different modules stay distinct by
    /// `(module, name)` — the same property [`unfold_and_qualify`]'s head
    /// qualification and the rest of the cross-module qualification model already have.
    pub package: Option<&'m crate::pass::resolve::Package<P>>,
    /// Scheme type-parameter binders that are in scope at the comparison
    /// site and must be left **bare** by head canonicalization. A bare
    /// nominal head is normally qualified to its declaring module's
    /// `(module, name)` form; but a single-segment head that
    /// names one of these binders is a type **variable**, not a nominal,
    /// so qualifying it would mis-resolve it to a same-named module type.
    /// That mis-resolution bites when a generic binder shadows a
    /// label-generated type alias of the same name (a `labels { a: … }`
    /// declaration synthesizes a type `A`, and `fn f[A](x: A)` /
    /// `newtype W[A] : A` rebinds `A`): the bare binder in the scheme's
    /// param/payload would otherwise resolve to the label's `A`, so the
    /// binder-unification arm in
    /// [`crate::pass::typecheck_core::synth::unify_pattern`]
    /// never fires and the call surfaces a spurious type mismatch. The
    /// set protects both a still-bare head and caller-supplied alias
    /// arguments while they are canonicalized before substitution. Alias
    /// bodies protect their own parameter set independently, so owner names
    /// and caller arguments retain separate provenance. `None` (the common
    /// case) means no binders are in scope.
    pub binder_locals: Option<&'a HashSet<String>>,
}

// `AliasCtx` holds only shared references, so it is `Copy` for any phase
// `P` — the derived impls would spuriously require `P: Copy`. Manual
// impls let a caller re-scope one field with functional update
// (`AliasCtx { binder_locals: …, ..ctx }`).
impl<P: crate::ast::Phase> Clone for AliasCtx<'_, '_, P> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<P: crate::ast::Phase> Copy for AliasCtx<'_, '_, P> {}

impl<P: crate::ast::Phase> AliasCtx<'_, '_, P> {
    pub(crate) fn binder_local_names(&self) -> HashSet<String> {
        self.binder_locals.cloned().unwrap_or_default()
    }
}

/// A `type`'s unfoldable content — its type-parameter
/// list and `Type` body — borrowed from the module the typer is
/// walking. Extracted from [`crate::ast::TypeAlias`] at map-build time
/// (in [`crate::pass::typecheck_core::ModuleEnv`]).
#[derive(Debug)]
pub struct AliasDef<'m, P: crate::ast::Phase> {
    pub type_params: &'m [crate::ast::TypeParam],
    pub body: &'m crate::ast::Type<P>,
    pub owner_module: Option<&'m crate::ast::Module<P>>,
}

// `AliasDef` holds only shared references, so it is `Copy` for any
// phase `P` — the derived impls would spuriously require `P: Copy`.
impl<P: crate::ast::Phase> Clone for AliasDef<'_, P> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<P: crate::ast::Phase> Copy for AliasDef<'_, P> {}

#[cfg(all(test, feature = "surface"))]
fn owner_alias_def<'m, P>(owner: &'m crate::ast::Module<P>, name: &str) -> Option<AliasDef<'m, P>>
where
    P: crate::ast::Phase,
{
    let mut found = None;
    for item in &owner.items {
        crate::pass::resolve::for_each_item_declaration(item, |declaration| {
            if let Some(alias) = declaration.type_alias()
                && alias.name == name
            {
                found = Some(AliasDef {
                    type_params: &alias.type_params,
                    body: alias.type_body(),
                    owner_module: Some(owner),
                });
            }
        });
        if found.is_some() {
            break;
        }
    }
    found
}

fn nominal_scope_follows_type_reexport<P>(
    segments: &[crate::ast::PathSegment],
    scope: AliasNominalScope<'_, P>,
    package: Option<&crate::pass::resolve::Package<P>>,
) -> Vec<crate::ast::PathSegment>
where
    P: crate::ast::Phase,
{
    let Some(package) = package else {
        return segments.to_vec();
    };
    if let AliasNominalScope::CheckingRoot { module, .. } = scope
        && let Some((_, owner_segments)) = segments.split_last()
        && owner_segments.len() == module.path.segments.len()
        && owner_segments
            .iter()
            .zip(&module.path.segments)
            .all(|(left, right)| left.as_str() == right.as_str())
    {
        return segments.to_vec();
    }
    crate::pass::resolve::follow_type_reexport(segments, package)
}

/// One transparent-alias declaration selected for a path head, paired with
/// the exact module whose lexical scope owns its raw body.
///
/// This is declaration lookup only: it neither resolves lexical/substitution
/// binders nor grants source visibility. Callers whose input may name a binder
/// must rule that out with their operation's complete binder authority first.
/// In particular, [`AliasCtx::binder_locals`] is not complete authority for
/// embedded `forall` or virtual alias views. [`unfold_top`] retains its
/// historical nominal-head precondition and is not such a binder-aware caller.
pub(crate) struct ResolvedAliasDeclarationHead<'m, P>
where
    P: crate::ast::Phase,
{
    pub(crate) def: AliasDef<'m, P>,
    pub(crate) owner_module: Option<&'m crate::ast::Module<P>>,
    pub(crate) owner_scope: AliasNominalScope<'m, P>,
}

// The result holds only shared references. Manual implementations avoid the
// irrelevant `P: Clone + Copy` bounds that deriving would add.
impl<P: crate::ast::Phase> Clone for ResolvedAliasDeclarationHead<'_, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: crate::ast::Phase> Copy for ResolvedAliasDeclarationHead<'_, P> {}

/// Resolve an admitted nominal path head to its transparent-alias declaration.
/// Binder-aware callers must rule out lexical/formal binders before entering;
/// this helper deliberately performs no binder or visibility resolution.
///
/// A noncanonical bare head follows the checking module's explicit selective
/// import edge before consulting its spelling-indexed alias maps. Repeated
/// ordinary introductions are rejected independently of canonical binding
/// identity. Following the written edge selects the alias declaration rather
/// than its terminal host/newtype, which can have different partial-application
/// rules. Every other head
/// resolves by exact module identity; the source module is terminal even if a
/// package index contains another module with the same path. The returned
/// owner applies only to the raw alias body. Caller-owned arguments remain in
/// [`AliasCtx::source_module`] and must never be canonicalized through this
/// owner.
#[cfg(all(test, feature = "surface"))]
pub(crate) fn resolve_alias_declaration_head<'m, P>(
    segments: &[crate::ast::PathSegment],
    ctx: &AliasCtx<'_, 'm, P>,
    identity_canonical: bool,
) -> Option<ResolvedAliasDeclarationHead<'m, P>>
where
    P: crate::ast::Phase,
{
    let provider = NominalProvider::new(ctx.source_module, ctx.package);
    resolve_alias_declaration_head_with_provider(
        segments,
        provider.root(),
        &provider,
        Some(ctx.local),
        Some(ctx.cross_module),
        identity_canonical,
    )
}

fn resolve_alias_declaration_head_with_provider<'m, P>(
    segments: &[crate::ast::PathSegment],
    source_scope: AliasNominalScope<'m, P>,
    provider: &NominalProvider<'m, P>,
    local: Option<&HashMap<&str, AliasDef<'m, P>>>,
    cross_module: Option<&HashMap<&str, AliasDef<'m, P>>>,
    identity_canonical: bool,
) -> Option<ResolvedAliasDeclarationHead<'m, P>>
where
    P: crate::ast::Phase,
{
    #[cfg(test)]
    update_alias_materializer_work(|work| work.declaration_head_probes += 1);
    let head = segments.last()?;
    let selection = provider.select(source_scope, segments, identity_canonical);
    let (def, owner_scope) = match selection {
        NominalSelection::Opaque => return None,
        NominalSelection::Selected(selected) => {
            let Some(alias) = selected.declaration.type_alias() else {
                // A selected written edge is authoritative. A host/newtype at
                // that edge cannot fall through to a same-spelled local alias.
                return None;
            };
            let overlay = match selected.route {
                NominalRoute::Selective if segments.len() == 1 => {
                    cross_module.and_then(|aliases| aliases.get(head.as_str()).copied())
                }
                NominalRoute::Local => {
                    local.and_then(|aliases| aliases.get(head.as_str()).copied())
                }
                NominalRoute::Qualified | NominalRoute::Exact | NominalRoute::Selective => None,
            };
            (
                overlay.unwrap_or(AliasDef {
                    type_params: &alias.type_params,
                    body: alias.type_body(),
                    owner_module: selected.owner.module(),
                }),
                selected.owner,
            )
        }
        NominalSelection::Missing => {
            // Explicit maps remain the admitted authority for ad-hoc contexts
            // that carry no module/package provider. They never override a
            // selected non-alias or malformed exact edge.
            let def = if segments.len() == 1 {
                cross_module
                    .and_then(|aliases| aliases.get(head.as_str()).copied())
                    .or_else(|| local.and_then(|aliases| aliases.get(head.as_str()).copied()))
            } else {
                None
            }?;
            let owner_scope = provider.scope_for_owner(def.owner_module.or(source_scope.module()));
            (def, owner_scope)
        }
    };
    Some(ResolvedAliasDeclarationHead {
        def,
        owner_module: owner_scope.module(),
        owner_scope,
    })
}

fn alias_name_route_hash(name: &str) -> u64 {
    name.as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

#[derive(Clone, Copy)]
struct AliasBorrowedName<'a>(&'a str);

impl PersistentExactKey for AliasBorrowedName<'_> {
    fn persistent_route_hash(&self) -> u64 {
        alias_name_route_hash(self.0)
    }

    fn persistent_exact_eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

#[derive(Clone)]
struct AliasOwnedName(String);

impl PersistentExactKey for AliasOwnedName {
    fn persistent_route_hash(&self) -> u64 {
        alias_name_route_hash(&self.0)
    }

    fn persistent_exact_eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

#[derive(Clone, Copy)]
struct ActiveAliasHead {
    body: usize,
    owner: usize,
}

impl PersistentExactKey for ActiveAliasHead {
    fn persistent_route_hash(&self) -> u64 {
        (self.body as u64).rotate_left(19) ^ (self.owner as u64).rotate_left(43)
    }

    fn persistent_exact_eq(&self, other: &Self) -> bool {
        self.body == other.body && self.owner == other.owner
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct AliasViewId(usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct AliasViewContextId(usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct AliasArgRopeId(usize);

#[derive(Clone)]
enum AliasArgRopeNode {
    Leaf(Box<[AliasViewId]>),
    Concat {
        left: AliasArgRopeId,
        right: AliasArgRopeId,
        len: usize,
    },
}

impl AliasArgRopeNode {
    fn len(&self) -> usize {
        match self {
            Self::Leaf(values) => values.len(),
            Self::Concat { len, .. } => *len,
        }
    }
}

enum AliasViewScope<'c, 'm, P>
where
    P: crate::ast::Phase,
{
    Caller {
        ctx: AliasCtx<'c, 'm, P>,
        nominal: AliasNominalScope<'m, P>,
    },
    Owner(AliasNominalScope<'m, P>),
}

impl<P: crate::ast::Phase> Clone for AliasViewScope<'_, '_, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: crate::ast::Phase> Copy for AliasViewScope<'_, '_, P> {}

impl<'m, P: crate::ast::Phase> AliasViewScope<'_, 'm, P> {
    fn nominal(self) -> AliasNominalScope<'m, P> {
        match self {
            Self::Caller { nominal, .. } | Self::Owner(nominal) => nominal,
        }
    }
}

struct AliasViewContext<'v, 'c, 'm, P>
where
    P: crate::ast::Phase,
{
    scope: AliasViewScope<'c, 'm, P>,
    formals: Option<Arc<HashMap<&'v str, AliasViewId>>>,
    ambient_protected: AliasAmbientProtected<'c>,
    protected: PersistentExactIndex<AliasBorrowedName<'v>, String>,
    emitted_binders: PersistentExactSet<AliasOwnedName>,
    next_emitted_suffix: PersistentExactIndex<AliasOwnedName, usize>,
    formal_free_names: Option<Arc<AliasFormalFreeNames>>,
    active_aliases: PersistentExactSet<ActiveAliasHead>,
    identity_canonical: bool,
}

#[derive(Clone, Copy)]
enum AliasAmbientProtected<'v> {
    None,
    Lookup(&'v dyn AliasBinderLookup),
}

impl AliasAmbientProtected<'_> {
    fn contains(self, name: &str) -> bool {
        match self {
            Self::None => false,
            Self::Lookup(lookup) => lookup.contains_alias_binder(name),
        }
    }
}

impl<'v, 'c, 'm, P> Clone for AliasViewContext<'v, 'c, 'm, P>
where
    P: crate::ast::Phase,
{
    fn clone(&self) -> Self {
        Self {
            scope: self.scope,
            formals: self.formals.clone(),
            ambient_protected: self.ambient_protected,
            protected: self.protected.clone(),
            emitted_binders: self.emitted_binders.clone(),
            next_emitted_suffix: self.next_emitted_suffix.clone(),
            formal_free_names: self.formal_free_names.clone(),
            active_aliases: self.active_aliases.clone(),
            identity_canonical: self.identity_canonical,
        }
    }
}

#[derive(Default)]
struct AliasFormalFreeNames {
    by_formal: HashMap<String, Arc<HashSet<String>>>,
}

impl AliasFormalFreeNames {
    fn contains_except(&self, name: &str, shadowed_formal: &str) -> bool {
        self.by_formal
            .iter()
            .any(|(formal, free)| formal != shadowed_formal && free.contains(name))
    }
}

struct AliasVirtualView<'v, P>
where
    P: crate::ast::Phase,
{
    ty: &'v crate::ast::Type<P>,
    context: AliasViewContextId,
    appended: AliasArgRopeId,
    application_span: Span,
}

impl<P: crate::ast::Phase> Clone for AliasVirtualView<'_, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: crate::ast::Phase> Copy for AliasVirtualView<'_, P> {}

enum AliasPathRedirect<'v, 'm, P>
where
    P: crate::ast::Phase,
{
    Formal {
        formal_ty: &'v crate::ast::Type<P>,
        formal: &'v str,
        selected: AliasVirtualView<'v, P>,
        selected_scope: AliasNominalScope<'m, P>,
    },
    Alias {
        application: &'v crate::ast::Type<P>,
        body: AliasVirtualView<'v, P>,
        owner_scope: AliasNominalScope<'m, P>,
    },
}

struct AliasMaterialized<P, N = ()>
where
    P: crate::ast::Phase,
{
    ty: crate::ast::Type<P>,
    identity_canonical: bool,
    transport: N,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct AliasSourceOccurrenceId(std::num::NonZeroU32);

impl AliasSourceOccurrenceId {
    pub(crate) fn from_index(index: usize) -> Self {
        let value = u32::try_from(index)
            .expect("a source occurrence id fits in u32")
            .checked_add(1)
            .expect("a source occurrence id reserves zero for Option");
        Self(std::num::NonZeroU32::new(value).expect("a source occurrence id is nonzero"))
    }

    pub(crate) fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct AliasOutputOccurrenceId(std::num::NonZeroU32);

impl AliasOutputOccurrenceId {
    fn from_index(index: usize) -> Self {
        let value = u32::try_from(index)
            .expect("an output occurrence id fits in u32")
            .checked_add(1)
            .expect("an output occurrence id reserves zero for Option");
        Self(std::num::NonZeroU32::new(value).expect("an output occurrence id is nonzero"))
    }

    pub(crate) fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
struct AliasBinderOriginId(std::num::NonZeroU32);

impl AliasBinderOriginId {
    fn from_index(index: usize) -> Self {
        let value = u32::try_from(index)
            .expect("a binder-origin id fits in u32")
            .checked_add(1)
            .expect("a binder-origin id reserves zero for Option");
        Self(std::num::NonZeroU32::new(value).expect("a binder-origin id is nonzero"))
    }

    fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AliasTransportEdge {
    FunctionParam,
    FunctionReturn,
    ProductLeft,
    ProductRight,
    SumLeft,
    SumRight,
    ForallBody,
    TypeArgument(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AliasBinderOrigin {
    pub(crate) name: String,
    pub(crate) span: Span,
    pub(crate) provider_file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AliasSourceDisposition {
    Dropped,
    /// The source occurrence survives without crossing an alias redirect.
    RetainedDirect,
    /// One or more exact alias-output occurrences retain this source.
    RetainedTransported,
}

impl AliasSourceDisposition {
    pub(crate) fn is_retained(self) -> bool {
        !matches!(self, Self::Dropped)
    }
}

#[derive(Debug)]
pub(crate) struct AliasSourceOccurrence {
    pub(crate) span: Span,
    pub(crate) is_infer: bool,
    /// Retention is classified only for source `Infer` occurrences. Path
    /// frontiers exist solely as exact alias-root identity anchors and keep
    /// the default `Dropped` state without owning an output list.
    pub(crate) disposition: AliasSourceDisposition,
    direct_first_forall: Option<AliasBinderOriginId>,
    first_forall: Option<AliasOutputOccurrenceId>,
}

#[derive(Debug)]
pub(crate) struct AliasOutputOccurrence {
    #[cfg(feature = "surface")]
    pub(crate) span: Span,
    binder: Option<AliasBinderOriginId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AliasTransportContribution {
    Continue {
        parent: AliasOutputOccurrenceId,
        edge: AliasTransportEdge,
        child: AliasOutputOccurrenceId,
    },
    Graft {
        formal: String,
        selected: AliasOutputOccurrenceId,
        provider_file: Option<PathBuf>,
    },
    Root {
        alias_application: Option<AliasSourceOccurrenceId>,
        body_root: AliasOutputOccurrenceId,
        provider_file: Option<PathBuf>,
    },
}

/// Exact source-occurrence indexing and sparse source-to-output transport for
/// one admitted source-annotation planning request.
///
/// The ordinary alias materializer uses [`NoAliasTransport`] and has no
/// sidecar field or allocation. A direct non-alias source indexes only its
/// path/`Infer` frontiers, direct emitted-`Infer` order, and binder-only
/// provenance arena; output ids and contribution nodes begin lazily at an
/// exact alias redirect. This value is transient Lowered planning evidence; it
/// is never serialized or carried into Prime.
#[derive(Debug)]
pub(crate) struct AliasSourceOccurrenceTransport {
    pub(crate) sources: Vec<AliasSourceOccurrence>,
    binders: Vec<AliasBinderOrigin>,
    pub(crate) outputs: Vec<AliasOutputOccurrence>,
    #[cfg(all(test, feature = "surface"))]
    pub(crate) contributions: Vec<AliasTransportContribution>,
    /// Materialized `Infer` leaves in the exact left-to-right order of the
    /// emitted ordinary `Type` walk. `None` is the direct source-occurrence
    /// index: no alias expansion was crossed, so no output occurrence id or
    /// transport DAG node is needed. Duplicate alias output repeats one source
    /// id; a dropped source contributes no entry.
    #[cfg(feature = "surface")]
    pub(crate) emitted_infers: Vec<(Option<AliasOutputOccurrenceId>, AliasSourceOccurrenceId)>,
    #[cfg(all(test, feature = "surface"))]
    pub(crate) root: Option<AliasOutputOccurrenceId>,
}

pub(crate) struct AliasSourceMaterialized<P>
where
    P: crate::ast::Phase,
{
    pub(crate) ty: crate::ast::Type<P>,
    pub(crate) identity_canonical: bool,
    pub(crate) transport: AliasSourceOccurrenceTransport,
}

pub(crate) struct AliasCompleteSchemeAnalysis {
    pub(crate) summary: super::kind_scheme::CompleteScheme,
    /// Zero-appended source/raw-declaration `forall` occurrences proven by
    /// this classification. Selected/formal-instantiated views are excluded.
    /// These call-local pointer keys are never persisted or reused across a
    /// type tree/allocation boundary.
    pub(crate) proved_bare_spines: Vec<usize>,
}

impl AliasSourceOccurrenceTransport {
    /// First written source `_` whose retained materialized output is below an
    /// annotation-local structural `forall`. Source registration order is the
    /// stable diagnostic order; a dropped occurrence has no binder route and
    /// is therefore admitted by this predicate.
    pub(crate) fn first_forall_nested_infer(
        &self,
    ) -> Option<(
        AliasSourceOccurrenceId,
        &AliasSourceOccurrence,
        &AliasBinderOrigin,
    )> {
        self.sources.iter().enumerate().find_map(|(index, source)| {
            if !source.is_infer {
                return None;
            }
            let id = AliasSourceOccurrenceId::from_index(index);
            self.first_enclosing_forall(id)
                .map(|binder| (id, source, binder))
        })
    }

    /// The first materialized structural `forall` enclosing one retained
    /// source occurrence. A direct index uses its written structural ancestry.
    /// Alias-output relations are retained in deterministic materialization
    /// order; within one route the outermost binder wins.
    pub(crate) fn first_enclosing_forall(
        &self,
        source: AliasSourceOccurrenceId,
    ) -> Option<&AliasBinderOrigin> {
        let source = &self.sources[source.index()];
        if matches!(&source.disposition, AliasSourceDisposition::Dropped) {
            return None;
        }
        if let Some(binder) = source.first_forall {
            return self.outputs[binder.index()]
                .binder
                .map(|binder| &self.binders[binder.index()]);
        }
        source
            .direct_first_forall
            .map(|binder| &self.binders[binder.index()])
    }
}

#[derive(Clone, Copy)]
enum AliasTransportNodeKind<'a> {
    Ordinary,
    Forall(&'a crate::ast::TypeParam),
}

trait AliasTransportSink<P>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    type Node: Copy + Default;
    type Children: Default;

    const RECORDS_TRANSPORT: bool;

    fn replace_recording(&mut self, enabled: bool) -> bool;

    fn replace_alias_transporting(&mut self, enabled: bool) -> bool;

    fn push_child(
        &mut self,
        children: &mut Self::Children,
        edge: AliasTransportEdge,
        child: Self::Node,
    );

    fn append_children(&mut self, target: &mut Self::Children, source: Self::Children);

    fn finish_node(
        &mut self,
        ty: &crate::ast::Type<P>,
        scope: AliasNominalScope<'_, P>,
        kind: AliasTransportNodeKind<'_>,
        children: Self::Children,
    ) -> Self::Node;

    fn graft(
        &mut self,
        _formal_ty: &crate::ast::Type<P>,
        formal: &str,
        scope: AliasNominalScope<'_, P>,
        selected: Self::Node,
    ) -> Self::Node;

    fn alias_root(
        &mut self,
        alias_application: &crate::ast::Type<P>,
        owner_scope: AliasNominalScope<'_, P>,
        body_root: Self::Node,
    ) -> Self::Node;
}

#[derive(Clone, Copy, Default)]
struct NoAliasTransport;

impl<P> AliasTransportSink<P> for NoAliasTransport
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    type Node = ();
    type Children = ();

    const RECORDS_TRANSPORT: bool = false;

    fn replace_recording(&mut self, _: bool) -> bool {
        false
    }

    fn replace_alias_transporting(&mut self, _: bool) -> bool {
        false
    }

    fn push_child(&mut self, _: &mut (), _: AliasTransportEdge, _: ()) {}

    fn append_children(&mut self, _: &mut (), _: ()) {}

    fn finish_node(
        &mut self,
        _: &crate::ast::Type<P>,
        _: AliasNominalScope<'_, P>,
        _: AliasTransportNodeKind<'_>,
        _: (),
    ) {
    }

    fn graft(&mut self, _: &crate::ast::Type<P>, _: &str, _: AliasNominalScope<'_, P>, _: ()) {}

    fn alias_root(&mut self, _: &crate::ast::Type<P>, _: AliasNominalScope<'_, P>, _: ()) {}
}

#[derive(Default)]
struct SourceOccurrenceTransportSink {
    recording: bool,
    alias_transporting: bool,
    source_by_address: HashMap<usize, AliasSourceOccurrenceId>,
    sources: Vec<AliasSourceOccurrence>,
    binders: Vec<AliasBinderOrigin>,
    outputs: Vec<AliasOutputOccurrence>,
    contributions: Vec<AliasTransportContribution>,
    emitted_infers: Vec<(Option<AliasOutputOccurrenceId>, AliasSourceOccurrenceId)>,
}

impl SourceOccurrenceTransportSink {
    fn new<P>(root: &crate::ast::Type<P>) -> Self
    where
        P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
    {
        let mut sink = Self {
            recording: true,
            ..Self::default()
        };
        sink.register_source(root, None);
        sink
    }

    fn source_id<P>(&self, ty: &crate::ast::Type<P>) -> Option<AliasSourceOccurrenceId>
    where
        P: crate::ast::Phase,
    {
        self.source_by_address
            .get(&(ty as *const crate::ast::Type<P> as usize))
            .copied()
    }

    fn register_source<P>(
        &mut self,
        ty: &crate::ast::Type<P>,
        direct_first_forall: Option<AliasBinderOriginId>,
    ) where
        P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
    {
        let introduced_forall = match ty {
            crate::ast::Type::Forall { param, .. } => {
                let binder = AliasBinderOriginId::from_index(self.binders.len());
                self.binders.push(AliasBinderOrigin {
                    name: param.name.clone(),
                    span: param.span,
                    provider_file: None,
                });
                Some(binder)
            }
            _ => None,
        };
        if matches!(
            ty,
            crate::ast::Type::Path { .. } | crate::ast::Type::Infer { .. }
        ) {
            let id = AliasSourceOccurrenceId::from_index(self.sources.len());
            let previous = self
                .source_by_address
                .insert(ty as *const crate::ast::Type<P> as usize, id);
            assert!(previous.is_none(), "a source type tree has unique nodes");
            self.sources.push(AliasSourceOccurrence {
                span: ty.span(),
                is_infer: matches!(ty, crate::ast::Type::Infer { .. }),
                disposition: AliasSourceDisposition::Dropped,
                direct_first_forall,
                first_forall: None,
            });
        }
        let child_first_forall = direct_first_forall.or(introduced_forall);
        let mut child = |ty: &crate::ast::Type<P>| {
            self.register_source(ty, child_first_forall);
        };
        match ty {
            crate::ast::Type::Function { param, ret, .. } => {
                child(param);
                child(ret);
            }
            crate::ast::Type::Product { left, right, .. } => {
                child(left);
                child(right);
            }
            crate::ast::Type::Sum { left, right, .. } => {
                child(left);
                child(right);
            }
            crate::ast::Type::Forall { body, .. } => {
                child(body);
            }
            crate::ast::Type::Path { args, .. } => {
                for argument in args {
                    child(argument);
                }
            }
            crate::ast::Type::Goal { args, .. } => {
                for argument in args {
                    child(argument);
                }
            }
            crate::ast::Type::Unit { .. }
            | crate::ast::Type::Bottom { .. }
            | crate::ast::Type::Infer { .. } => {}
            crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        }
    }

    fn finish(mut self, root: Option<AliasOutputOccurrenceId>) -> AliasSourceOccurrenceTransport {
        #[cfg(not(all(test, feature = "surface")))]
        let _ = root;
        let mut children = vec![Vec::new(); self.outputs.len()];
        let mut is_child = vec![false; self.outputs.len()];
        for contribution in &self.contributions {
            if let AliasTransportContribution::Continue { parent, child, .. } = contribution {
                children[parent.index()].push(*child);
                is_child[child.index()] = true;
            }
        }
        let mut first_forall_by_output = vec![None; self.outputs.len()];
        let mut visited = vec![false; self.outputs.len()];
        let mut pending = self
            .outputs
            .iter()
            .enumerate()
            .filter(|(index, _)| !is_child[*index])
            .rev()
            .map(|(index, _)| (AliasOutputOccurrenceId::from_index(index), None))
            .collect::<Vec<_>>();
        while let Some((output, outer_forall)) = pending.pop() {
            let first_forall = outer_forall
                .or_else(|| self.outputs[output.index()].binder.as_ref().map(|_| output));
            if visited[output.index()] {
                continue;
            }
            visited[output.index()] = true;
            first_forall_by_output[output.index()] = first_forall;
            pending.extend(
                children[output.index()]
                    .iter()
                    .rev()
                    .map(|child| (*child, first_forall)),
            );
        }
        for (output, source) in &self.emitted_infers {
            let Some(output) = output else {
                continue;
            };
            let source = &mut self.sources[source.index()];
            if source.first_forall.is_none() {
                source.first_forall = first_forall_by_output[output.index()];
            }
        }
        AliasSourceOccurrenceTransport {
            sources: self.sources,
            binders: self.binders,
            outputs: self.outputs,
            #[cfg(all(test, feature = "surface"))]
            contributions: self.contributions,
            #[cfg(feature = "surface")]
            emitted_infers: self.emitted_infers,
            #[cfg(all(test, feature = "surface"))]
            root,
        }
    }
}

impl<P> AliasTransportSink<P> for SourceOccurrenceTransportSink
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never>,
{
    type Node = Option<AliasOutputOccurrenceId>;
    type Children = Vec<(AliasTransportEdge, AliasOutputOccurrenceId)>;

    const RECORDS_TRANSPORT: bool = true;

    fn replace_recording(&mut self, enabled: bool) -> bool {
        std::mem::replace(&mut self.recording, enabled)
    }

    fn replace_alias_transporting(&mut self, enabled: bool) -> bool {
        std::mem::replace(&mut self.alias_transporting, enabled)
    }

    fn push_child(
        &mut self,
        children: &mut Self::Children,
        edge: AliasTransportEdge,
        child: Self::Node,
    ) {
        if !self.recording {
            return;
        }
        if let Some(child) = child {
            children.push((edge, child));
        }
    }

    fn append_children(&mut self, target: &mut Self::Children, source: Self::Children) {
        if !self.recording {
            return;
        }
        target.extend(source);
    }

    fn finish_node(
        &mut self,
        ty: &crate::ast::Type<P>,
        scope: AliasNominalScope<'_, P>,
        kind: AliasTransportNodeKind<'_>,
        children: Self::Children,
    ) -> Self::Node {
        if !self.recording {
            return None;
        }
        let source = self.source_id(ty);
        let tracked_leaf = source.is_some_and(|source| self.sources[source.index()].is_infer);
        if !self.alias_transporting && children.is_empty() {
            if let Some(source) = source.filter(|_| tracked_leaf) {
                match self.sources[source.index()].disposition {
                    AliasSourceDisposition::RetainedDirect
                    | AliasSourceDisposition::RetainedTransported => {
                        unreachable!("a direct source occurrence was emitted more than once")
                    }
                    AliasSourceDisposition::Dropped => {
                        self.sources[source.index()].disposition =
                            AliasSourceDisposition::RetainedDirect;
                    }
                }
                self.emitted_infers.push((None, source));
            }
            return None;
        }
        if children.is_empty() && !tracked_leaf {
            return None;
        }
        let binder = match kind {
            AliasTransportNodeKind::Ordinary => None,
            AliasTransportNodeKind::Forall(param) => {
                let binder = AliasBinderOriginId::from_index(self.binders.len());
                self.binders.push(AliasBinderOrigin {
                    name: param.name.clone(),
                    span: param.span,
                    provider_file: scope.provider_file().map(std::path::Path::to_path_buf),
                });
                Some(binder)
            }
        };
        let output = AliasOutputOccurrenceId::from_index(self.outputs.len());
        self.outputs.push(AliasOutputOccurrence {
            #[cfg(feature = "surface")]
            span: ty.span(),
            binder,
        });
        if tracked_leaf {
            self.emitted_infers.push((
                Some(output),
                source.expect("a tracked materialized infer has one source occurrence"),
            ));
        }
        for (edge, child) in children {
            self.contributions
                .push(AliasTransportContribution::Continue {
                    parent: output,
                    edge,
                    child,
                });
        }
        if let Some(source) = source.filter(|_| tracked_leaf) {
            match self.sources[source.index()].disposition {
                AliasSourceDisposition::RetainedTransported => {}
                AliasSourceDisposition::Dropped => {
                    self.sources[source.index()].disposition =
                        AliasSourceDisposition::RetainedTransported;
                }
                AliasSourceDisposition::RetainedDirect => unreachable!(
                    "one source occurrence cannot be emitted both directly and through an alias"
                ),
            }
        }
        Some(output)
    }

    fn graft(
        &mut self,
        _formal_ty: &crate::ast::Type<P>,
        formal: &str,
        scope: AliasNominalScope<'_, P>,
        selected: Self::Node,
    ) -> Self::Node {
        if !self.recording {
            return None;
        }
        if let Some(selected) = selected {
            self.contributions.push(AliasTransportContribution::Graft {
                formal: formal.to_owned(),
                selected,
                provider_file: scope.provider_file().map(std::path::Path::to_path_buf),
            });
        }
        selected
    }

    fn alias_root(
        &mut self,
        alias_application: &crate::ast::Type<P>,
        owner_scope: AliasNominalScope<'_, P>,
        body_root: Self::Node,
    ) -> Self::Node {
        if !self.recording {
            return None;
        }
        if let Some(body_root) = body_root {
            let alias_application_id = self.source_id(alias_application);
            self.contributions.push(AliasTransportContribution::Root {
                alias_application: alias_application_id,
                body_root,
                provider_file: owner_scope
                    .provider_file()
                    .map(std::path::Path::to_path_buf),
            });
        }
        body_root
    }
}

struct AliasRootMaterialized<P>
where
    P: crate::ast::Phase,
{
    ty: crate::ast::Type<P>,
    identity_canonical: bool,
    defensive_cycle_cutoff: bool,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AliasMaterializerWork {
    materializer_roots: usize,
    ambient_binder_copies: usize,
    source_alias_index_builds: usize,
    source_alias_items_scanned: usize,
    materialize_calls: usize,
    formal_summary_builds: usize,
    free_name_summary_builds: usize,
    free_name_summary_type_visits: usize,
    free_name_summary_retained_names: usize,
    alpha_candidate_checks: usize,
    declaration_head_probes: usize,
    exact_module_entry_lookups: usize,
    exact_module_entry_path_segments: usize,
    edge_target_lookups: usize,
    edge_target_path_segments: usize,
    owner_alias_index_builds: usize,
    owner_alias_items_scanned: usize,
    arg_rope_nodes: usize,
    arg_rope_walks: usize,
    arg_rope_value_visits: usize,
    path_redirects: usize,
    scheme_frontier_steps: usize,
}

#[cfg(test)]
thread_local! {
    static ALIAS_MATERIALIZER_WORK: std::cell::Cell<AliasMaterializerWork> =
        const { std::cell::Cell::new(AliasMaterializerWork {
            materializer_roots: 0,
            ambient_binder_copies: 0,
            source_alias_index_builds: 0,
            source_alias_items_scanned: 0,
            materialize_calls: 0,
            formal_summary_builds: 0,
            free_name_summary_builds: 0,
            free_name_summary_type_visits: 0,
            free_name_summary_retained_names: 0,
            alpha_candidate_checks: 0,
            declaration_head_probes: 0,
            exact_module_entry_lookups: 0,
            exact_module_entry_path_segments: 0,
            edge_target_lookups: 0,
            edge_target_path_segments: 0,
            owner_alias_index_builds: 0,
            owner_alias_items_scanned: 0,
            arg_rope_nodes: 0,
            arg_rope_walks: 0,
            arg_rope_value_visits: 0,
            path_redirects: 0,
            scheme_frontier_steps: 0,
        }) };
}

#[cfg(all(test, feature = "surface"))]
thread_local! {
    static DEEP_CANONICALIZATION_CALLS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn reset_deep_canonicalization_calls_for_test() {
    DEEP_CANONICALIZATION_CALLS.with(|calls| calls.set(0));
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn deep_canonicalization_calls_for_test() -> usize {
    DEEP_CANONICALIZATION_CALLS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn update_alias_materializer_work(update: impl FnOnce(&mut AliasMaterializerWork)) {
    ALIAS_MATERIALIZER_WORK.with(|work| {
        let mut current = work.get();
        update(&mut current);
        work.set(current);
    });
}

#[cfg(all(test, feature = "surface"))]
fn reset_alias_materializer_work() {
    ALIAS_MATERIALIZER_WORK.with(|work| work.set(AliasMaterializerWork::default()));
    crate::pass::resolve::reset_nominal_provider_work();
}

#[cfg(all(test, feature = "surface"))]
fn alias_materializer_work() -> AliasMaterializerWork {
    let mut work = ALIAS_MATERIALIZER_WORK.with(std::cell::Cell::get);
    let provider = crate::pass::resolve::nominal_provider_work();
    work.owner_alias_index_builds += provider.checking_scope_builds;
    work.owner_alias_items_scanned += provider.declaration_items_indexed;
    work.exact_module_entry_lookups += provider.exact_target_lookups;
    work.exact_module_entry_path_segments += provider.exact_target_path_segments;
    work.edge_target_lookups += provider.edge_target_lookups;
    work.edge_target_path_segments += provider.edge_target_path_segments;
    work
}

struct AliasMaterializer<'p, 'v, 'c, 'm, P, S = NoAliasTransport>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
    S: AliasTransportSink<P>,
    'm: 'v,
{
    provider: &'p NominalProvider<'m, P>,
    // Context/view/rope ID zero is represented inline. The backing arenas are
    // reserved for alias/formal/binder transport, so a canonical structural
    // type can use this same traversal without an otherwise-empty heap arena.
    root_context: AliasViewContext<'v, 'c, 'm, P>,
    contexts: Vec<AliasViewContext<'v, 'c, 'm, P>>,
    views: Vec<AliasVirtualView<'v, P>>,
    free_name_summaries: Vec<Option<Arc<HashSet<String>>>>,
    free_name_summary_active: Vec<bool>,
    arg_ropes: Vec<AliasArgRopeNode>,
    transport: S,
    arena_active: bool,
    defensive_cycle_cutoff: bool,
}

impl<'p, 'v, 'c, 'm, P> AliasMaterializer<'p, 'v, 'c, 'm, P, NoAliasTransport>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
    'm: 'v,
{
    fn for_caller(
        ty: &'v crate::ast::Type<P>,
        ctx: AliasCtx<'c, 'm, P>,
        nominal: AliasNominalScope<'m, P>,
        provider: &'p NominalProvider<'m, P>,
        ambient_protected: AliasAmbientProtected<'c>,
        identity_canonical: bool,
    ) -> (Self, AliasVirtualView<'v, P>) {
        Self::for_caller_with_transport(
            ty,
            ctx,
            nominal,
            provider,
            ambient_protected,
            identity_canonical,
            NoAliasTransport,
        )
    }

    fn for_owner_body(
        alias: AliasDef<'m, P>,
        owner: &'m crate::ast::Module<P>,
        provider: &'p NominalProvider<'m, P>,
    ) -> (Self, AliasVirtualView<'v, P>) {
        Self::for_owner_body_with_transport(alias, owner, provider, NoAliasTransport)
    }

    fn for_nominal_scope(
        ty: &'v crate::ast::Type<P>,
        nominal: AliasNominalScope<'m, P>,
        provider: &'p NominalProvider<'m, P>,
        ambient_protected: AliasAmbientProtected<'c>,
        identity_canonical: bool,
    ) -> (Self, AliasVirtualView<'v, P>) {
        let root_context = AliasViewContext {
            scope: AliasViewScope::Owner(nominal),
            formals: None,
            ambient_protected,
            protected: PersistentExactIndex::default(),
            emitted_binders: PersistentExactSet::default(),
            next_emitted_suffix: PersistentExactIndex::default(),
            formal_free_names: None,
            active_aliases: PersistentExactSet::default(),
            identity_canonical,
        };
        let materializer = Self {
            provider,
            root_context,
            contexts: Vec::new(),
            views: Vec::new(),
            free_name_summaries: Vec::new(),
            free_name_summary_active: Vec::new(),
            arg_ropes: Vec::new(),
            transport: NoAliasTransport,
            arena_active: false,
            defensive_cycle_cutoff: false,
        };
        let root = AliasVirtualView {
            ty,
            context: AliasViewContextId(0),
            appended: AliasArgRopeId(0),
            application_span: ty.span(),
        };
        (materializer, root)
    }

    /// Count the nodes in the fully redirected alias view without first
    /// constructing that potentially deep materialized type. Structural
    /// recursion fuel needs only this measure; keeping the traversal over the
    /// same virtual views as ordinary materialization preserves exact alias,
    /// formal-argument, owner-scope, and binder semantics while making a
    /// finite type's depth a heap-work concern rather than a Rust stack bound.
    #[cfg(feature = "surface")]
    fn materialized_node_measure(&mut self, root: AliasVirtualView<'v, P>) -> Option<usize> {
        let mut total = 0usize;
        let mut pending = vec![root];
        while let Some(mut view) = pending.pop() {
            loop {
                match view.ty {
                    crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => {
                        debug_assert_eq!(self.arg_rope_len(view.appended), 0);
                        total = total.checked_add(1)?;
                        break;
                    }
                    crate::ast::Type::Infer { .. } => return None,
                    crate::ast::Type::Function { param, ret, .. } => {
                        debug_assert_eq!(self.arg_rope_len(view.appended), 0);
                        total = total.checked_add(1)?;
                        pending.push(AliasVirtualView {
                            ty: ret,
                            context: view.context,
                            appended: AliasArgRopeId(0),
                            application_span: ret.span(),
                        });
                        pending.push(AliasVirtualView {
                            ty: param,
                            context: view.context,
                            appended: AliasArgRopeId(0),
                            application_span: param.span(),
                        });
                        break;
                    }
                    crate::ast::Type::Product { left, right, .. }
                    | crate::ast::Type::Sum { left, right, .. } => {
                        debug_assert_eq!(self.arg_rope_len(view.appended), 0);
                        total = total.checked_add(1)?;
                        pending.push(AliasVirtualView {
                            ty: right,
                            context: view.context,
                            appended: AliasArgRopeId(0),
                            application_span: right.span(),
                        });
                        pending.push(AliasVirtualView {
                            ty: left,
                            context: view.context,
                            appended: AliasArgRopeId(0),
                            application_span: left.span(),
                        });
                        break;
                    }
                    crate::ast::Type::Forall { param, body, .. } => {
                        debug_assert_eq!(self.arg_rope_len(view.appended), 0);
                        total = total.checked_add(1)?;
                        // Measurement does not emit binder spellings, but the
                        // written binder must still block alias/formal lookup
                        // beneath its body. Substituted formal views retain
                        // their caller context and therefore remain capture
                        // free without alpha-renaming an output tree.
                        let mut context = self.context(view.context).clone();
                        let key = AliasBorrowedName(param.name.as_str());
                        context.protected = context
                            .protected
                            .remove(&key)
                            .insert_with(key, param.name.clone(), |left, right| left == right)
                            .expect("a structural binder has one protected spelling");
                        let context = self.alloc_context(context);
                        pending.push(AliasVirtualView {
                            ty: body,
                            context,
                            appended: AliasArgRopeId(0),
                            application_span: body.span(),
                        });
                        break;
                    }
                    crate::ast::Type::Path { segments, args, .. } => {
                        if let Some(redirect) = self.redirect_path(view, segments, args) {
                            view = match redirect {
                                AliasPathRedirect::Formal { selected, .. } => selected,
                                AliasPathRedirect::Alias { body, .. } => body,
                            };
                            continue;
                        }
                        total = total.checked_add(1)?;
                        for id in self.collect_arg_views(view.appended).into_iter().rev() {
                            pending.push(self.views[id.0]);
                        }
                        for argument in args.iter().rev() {
                            pending.push(AliasVirtualView {
                                ty: argument,
                                context: view.context,
                                appended: AliasArgRopeId(0),
                                application_span: argument.span(),
                            });
                        }
                        break;
                    }
                    crate::ast::Type::Goal { args, .. } => {
                        total = total.checked_add(1)?;
                        for id in self.collect_arg_views(view.appended).into_iter().rev() {
                            pending.push(self.views[id.0]);
                        }
                        for argument in args.iter().rev() {
                            pending.push(AliasVirtualView {
                                ty: argument,
                                context: view.context,
                                appended: AliasArgRopeId(0),
                                application_span: argument.span(),
                            });
                        }
                        break;
                    }
                    crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
                }
            }
        }
        Some(total)
    }
}

impl<'p, 'v, 'c, 'm, P, S> AliasMaterializer<'p, 'v, 'c, 'm, P, S>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
    S: AliasTransportSink<P>,
    'm: 'v,
{
    fn for_caller_with_transport(
        ty: &'v crate::ast::Type<P>,
        ctx: AliasCtx<'c, 'm, P>,
        nominal: AliasNominalScope<'m, P>,
        provider: &'p NominalProvider<'m, P>,
        ambient_protected: AliasAmbientProtected<'c>,
        identity_canonical: bool,
        transport: S,
    ) -> (Self, AliasVirtualView<'v, P>) {
        let root_context = AliasViewContext {
            scope: AliasViewScope::Caller { ctx, nominal },
            formals: None,
            ambient_protected,
            protected: PersistentExactIndex::default(),
            emitted_binders: PersistentExactSet::default(),
            next_emitted_suffix: PersistentExactIndex::default(),
            formal_free_names: None,
            active_aliases: PersistentExactSet::default(),
            identity_canonical,
        };
        let materializer = Self {
            provider,
            root_context,
            contexts: Vec::new(),
            views: Vec::new(),
            free_name_summaries: Vec::new(),
            free_name_summary_active: Vec::new(),
            arg_ropes: Vec::new(),
            transport,
            arena_active: false,
            defensive_cycle_cutoff: false,
        };
        let root = AliasVirtualView {
            ty,
            context: AliasViewContextId(0),
            appended: AliasArgRopeId(0),
            application_span: ty.span(),
        };
        (materializer, root)
    }

    fn for_owner_body_with_transport(
        alias: AliasDef<'m, P>,
        owner: &'m crate::ast::Module<P>,
        provider: &'p NominalProvider<'m, P>,
        transport: S,
    ) -> (Self, AliasVirtualView<'v, P>) {
        let mut protected = PersistentExactIndex::default();
        for param in alias.type_params {
            let name: &'v str = param.name.as_str();
            let key = AliasBorrowedName(name);
            protected = protected
                .remove(&key)
                .insert_with(key, param.name.clone(), |left, right| left == right)
                .expect("an alias formal has one protected spelling");
        }
        let active = ActiveAliasHead {
            body: alias.body as *const crate::ast::Type<P> as usize,
            owner: owner as *const crate::ast::Module<P> as usize,
        };
        let root_context = AliasViewContext {
            scope: AliasViewScope::Owner(provider.scope_for_owner(Some(owner))),
            formals: None,
            ambient_protected: AliasAmbientProtected::None,
            protected,
            emitted_binders: PersistentExactSet::default(),
            next_emitted_suffix: PersistentExactIndex::default(),
            formal_free_names: None,
            active_aliases: PersistentExactSet::default().insert(active),
            identity_canonical: false,
        };
        let mut materializer = Self {
            provider,
            root_context,
            contexts: Vec::new(),
            views: Vec::new(),
            free_name_summaries: Vec::new(),
            free_name_summary_active: Vec::new(),
            arg_ropes: Vec::new(),
            transport,
            arena_active: false,
            defensive_cycle_cutoff: false,
        };
        materializer.activate_arena();
        let body: &'v crate::ast::Type<P> = alias.body;
        let root = AliasVirtualView {
            ty: body,
            context: AliasViewContextId(0),
            appended: AliasArgRopeId(0),
            application_span: body.span(),
        };
        (materializer, root)
    }

    fn activate_arena(&mut self) {
        if self.arena_active {
            return;
        }
        self.arena_active = true;
        #[cfg(test)]
        update_alias_materializer_work(|work| work.materializer_roots += 1);
    }

    fn context(&self, id: AliasViewContextId) -> &AliasViewContext<'v, 'c, 'm, P> {
        if id.0 == 0 {
            &self.root_context
        } else {
            &self.contexts[id.0 - 1]
        }
    }

    fn context_mut(&mut self, id: AliasViewContextId) -> &mut AliasViewContext<'v, 'c, 'm, P> {
        if id.0 == 0 {
            &mut self.root_context
        } else {
            &mut self.contexts[id.0 - 1]
        }
    }

    fn arg_rope_len(&self, id: AliasArgRopeId) -> usize {
        if id.0 == 0 {
            0
        } else {
            self.arg_ropes[id.0 - 1].len()
        }
    }

    fn alloc_context(&mut self, context: AliasViewContext<'v, 'c, 'm, P>) -> AliasViewContextId {
        self.activate_arena();
        let id = AliasViewContextId(self.contexts.len() + 1);
        self.contexts.push(context);
        id
    }

    fn alloc_view(
        &mut self,
        ty: &'v crate::ast::Type<P>,
        context: AliasViewContextId,
        appended: AliasArgRopeId,
        application_span: Span,
    ) -> AliasViewId {
        self.activate_arena();
        let id = AliasViewId(self.views.len());
        self.views.push(AliasVirtualView {
            ty,
            context,
            appended,
            application_span,
        });
        self.free_name_summaries.push(None);
        self.free_name_summary_active.push(false);
        id
    }

    fn alloc_native_args(
        &mut self,
        args: &'v [crate::ast::Type<P>],
        context: AliasViewContextId,
    ) -> Vec<AliasViewId> {
        args.iter()
            .map(|arg| self.alloc_view(arg, context, AliasArgRopeId(0), arg.span()))
            .collect()
    }

    fn arg_leaf(&mut self, values: Vec<AliasViewId>) -> AliasArgRopeId {
        if values.is_empty() {
            return AliasArgRopeId(0);
        }
        self.activate_arena();
        #[cfg(test)]
        update_alias_materializer_work(|work| work.arg_rope_nodes += 1);
        let id = AliasArgRopeId(self.arg_ropes.len() + 1);
        self.arg_ropes
            .push(AliasArgRopeNode::Leaf(values.into_boxed_slice()));
        id
    }

    fn concat_args(&mut self, left: AliasArgRopeId, right: AliasArgRopeId) -> AliasArgRopeId {
        let left_len = self.arg_rope_len(left);
        if left_len == 0 {
            return right;
        }
        let right_len = self.arg_rope_len(right);
        if right_len == 0 {
            return left;
        }
        self.activate_arena();
        #[cfg(test)]
        update_alias_materializer_work(|work| work.arg_rope_nodes += 1);
        let id = AliasArgRopeId(self.arg_ropes.len() + 1);
        self.arg_ropes.push(AliasArgRopeNode::Concat {
            left,
            right,
            len: left_len
                .checked_add(right_len)
                .expect("a type application argument count fits in usize"),
        });
        id
    }

    fn collect_arg_views(&self, root: AliasArgRopeId) -> Vec<AliasViewId> {
        if root.0 == 0 {
            return Vec::new();
        }
        #[cfg(test)]
        update_alias_materializer_work(|work| work.arg_rope_walks += 1);
        let mut out = Vec::with_capacity(self.arg_rope_len(root));
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            if id.0 == 0 {
                continue;
            }
            match &self.arg_ropes[id.0 - 1] {
                AliasArgRopeNode::Leaf(values) => {
                    #[cfg(test)]
                    update_alias_materializer_work(|work| {
                        work.arg_rope_value_visits += values.len();
                    });
                    out.extend(values.iter().copied());
                }
                AliasArgRopeNode::Concat { left, right, .. } => {
                    pending.push(*right);
                    pending.push(*left);
                }
            }
        }
        out
    }

    fn split_arg_prefix(
        &mut self,
        root: AliasArgRopeId,
        take: usize,
    ) -> (Vec<AliasViewId>, AliasArgRopeId) {
        let len = self.arg_rope_len(root);
        assert!(take <= len, "an argument prefix cannot exceed its rope");
        if take == 0 {
            return (Vec::new(), root);
        }
        if take == len {
            return (self.collect_arg_views(root), AliasArgRopeId(0));
        }
        let mut prefix = Vec::with_capacity(take);
        let mut current = root;
        let mut remaining_take = take;
        let mut right_siblings = Vec::new();
        let mut remaining = loop {
            match self.arg_ropes[current.0 - 1].clone() {
                AliasArgRopeNode::Leaf(values) => {
                    prefix.extend_from_slice(&values[..remaining_take]);
                    break self.arg_leaf(values[remaining_take..].to_vec());
                }
                AliasArgRopeNode::Concat { left, right, .. } => {
                    let left_len = self.arg_rope_len(left);
                    if remaining_take < left_len {
                        right_siblings.push(right);
                        current = left;
                    } else if remaining_take == left_len {
                        prefix.extend(self.collect_arg_views(left));
                        break right;
                    } else {
                        prefix.extend(self.collect_arg_views(left));
                        remaining_take -= left_len;
                        current = right;
                    }
                }
            }
        };
        for sibling in right_siblings.into_iter().rev() {
            remaining = self.concat_args(remaining, sibling);
        }
        (prefix, remaining)
    }

    fn resolve_alias(
        &mut self,
        segments: &[crate::ast::PathSegment],
        context: &AliasViewContext<'v, 'c, 'm, P>,
    ) -> Option<ResolvedAliasDeclarationHead<'m, P>> {
        match context.scope {
            AliasViewScope::Caller { ctx, nominal } => {
                resolve_alias_declaration_head_with_provider(
                    segments,
                    nominal,
                    self.provider,
                    Some(ctx.local),
                    Some(ctx.cross_module),
                    context.identity_canonical,
                )
            }
            AliasViewScope::Owner(nominal) => resolve_alias_declaration_head_with_provider(
                segments,
                nominal,
                self.provider,
                None,
                None,
                context.identity_canonical,
            ),
        }
    }

    fn qualify_nominal_segments(
        &self,
        segments: &[crate::ast::PathSegment],
        context: &AliasViewContext<'v, 'c, 'm, P>,
    ) -> (Vec<crate::ast::PathSegment>, bool) {
        self.provider.qualify(
            context.scope.nominal(),
            segments,
            context.identity_canonical,
        )
    }

    fn active_alias_key(resolved: ResolvedAliasDeclarationHead<'m, P>) -> ActiveAliasHead {
        ActiveAliasHead {
            body: resolved.def.body as *const crate::ast::Type<P> as usize,
            owner: resolved
                .owner_module
                .map_or(0, |owner| owner as *const crate::ast::Module<P> as usize),
        }
    }

    fn materialize_arg_rope(
        &mut self,
        args: AliasArgRopeId,
        start_index: usize,
    ) -> (Vec<crate::ast::Type<P>>, bool, S::Children) {
        let views = self.collect_arg_views(args);
        let mut canonical = true;
        let mut values = Vec::with_capacity(views.len());
        let mut children = S::Children::default();
        for (offset, view) in views.into_iter().enumerate() {
            let materialized = self.materialize_stored(view);
            canonical &= materialized.identity_canonical;
            self.transport.push_child(
                &mut children,
                AliasTransportEdge::TypeArgument(start_index + offset),
                materialized.transport,
            );
            values.push(materialized.ty);
        }
        (values, canonical, children)
    }

    fn materialize_args(
        &mut self,
        native: &'v [crate::ast::Type<P>],
        context: AliasViewContextId,
        appended: AliasArgRopeId,
    ) -> (Vec<crate::ast::Type<P>>, bool, S::Children) {
        let mut canonical = true;
        let mut values = Vec::with_capacity(
            native
                .len()
                .checked_add(self.arg_rope_len(appended))
                .expect("a materialized type argument count fits in usize"),
        );
        let mut children = S::Children::default();
        for (index, argument) in native.iter().enumerate() {
            let materialized = self.materialize(AliasVirtualView {
                ty: argument,
                context,
                appended: AliasArgRopeId(0),
                application_span: argument.span(),
            });
            canonical &= materialized.identity_canonical;
            self.transport.push_child(
                &mut children,
                AliasTransportEdge::TypeArgument(index),
                materialized.transport,
            );
            values.push(materialized.ty);
        }
        let (appended, appended_canonical, appended_children) =
            self.materialize_arg_rope(appended, native.len());
        canonical &= appended_canonical;
        values.extend(appended);
        self.transport
            .append_children(&mut children, appended_children);
        (values, canonical, children)
    }

    fn forwarded_formal_view(&self, id: AliasViewId) -> Option<AliasViewId> {
        let view = self.views[id.0];
        if self.arg_rope_len(view.appended) != 0 {
            return None;
        }
        let crate::ast::Type::Path { segments, args, .. } = view.ty else {
            return None;
        };
        if segments.len() != 1 || !args.is_empty() {
            return None;
        }
        let name = segments[0].as_str();
        let context = self.context(view.context);
        if context.protected.get(&AliasBorrowedName(name)).is_some()
            || context.ambient_protected.contains(name)
        {
            return None;
        }
        context
            .formals
            .as_deref()
            .and_then(|formals| formals.get(name))
            .copied()
    }

    fn collect_materialized_free_names<'t>(
        ty: &'t crate::ast::Type<P>,
        bound: &mut HashMap<&'t str, usize>,
        free: &mut HashSet<String>,
    ) {
        #[cfg(test)]
        update_alias_materializer_work(|work| work.free_name_summary_type_visits += 1);
        match ty {
            crate::ast::Type::Unit { .. }
            | crate::ast::Type::Bottom { .. }
            | crate::ast::Type::Infer { .. } => {}
            crate::ast::Type::Path { segments, args, .. } => {
                if segments.len() == 1 && !bound.contains_key(segments[0].as_str()) {
                    free.insert(segments[0].name.clone());
                }
                for argument in args {
                    Self::collect_materialized_free_names(argument, bound, free);
                }
            }
            crate::ast::Type::Function { param, ret, .. } => {
                Self::collect_materialized_free_names(param, bound, free);
                Self::collect_materialized_free_names(ret, bound, free);
            }
            crate::ast::Type::Product { left, right, .. }
            | crate::ast::Type::Sum { left, right, .. } => {
                Self::collect_materialized_free_names(left, bound, free);
                Self::collect_materialized_free_names(right, bound, free);
            }
            crate::ast::Type::Forall { param, body, .. } => {
                *bound.entry(param.name.as_str()).or_default() += 1;
                Self::collect_materialized_free_names(body, bound, free);
                let depth = bound
                    .get_mut(param.name.as_str())
                    .expect("the active type binder was recorded");
                *depth -= 1;
                if *depth == 0 {
                    bound.remove(param.name.as_str());
                }
            }
            crate::ast::Type::Goal { args, .. } => {
                for argument in args {
                    Self::collect_materialized_free_names(argument, bound, free);
                }
            }
            crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        }
    }

    fn free_names_stored(&mut self, id: AliasViewId) -> Arc<HashSet<String>> {
        if let Some(summary) = self.free_name_summaries[id.0].clone() {
            return summary;
        }
        if self.free_name_summary_active[id.0] {
            unreachable!("a virtual alias formal cannot recursively select itself");
        }
        self.free_name_summary_active[id.0] = true;
        #[cfg(test)]
        update_alias_materializer_work(|work| work.free_name_summary_builds += 1);
        let summary = if let Some(forwarded) = self.forwarded_formal_view(id) {
            self.free_names_stored(forwarded)
        } else {
            let materialized = self.materialize_stored(id);
            let mut free = HashSet::new();
            Self::collect_materialized_free_names(&materialized.ty, &mut HashMap::new(), &mut free);
            #[cfg(test)]
            update_alias_materializer_work(|work| {
                work.free_name_summary_retained_names += free.len();
            });
            Arc::new(free)
        };
        self.free_name_summary_active[id.0] = false;
        self.free_name_summaries[id.0] = Some(summary.clone());
        summary
    }

    fn formal_free_names(&mut self, context: AliasViewContextId) -> Arc<AliasFormalFreeNames> {
        if let Some(free) = &self.context(context).formal_free_names {
            return free.clone();
        }
        #[cfg(test)]
        update_alias_materializer_work(|work| work.formal_summary_builds += 1);
        let mut formal_views = self
            .context(context)
            .formals
            .as_deref()
            .into_iter()
            .flat_map(HashMap::iter)
            .map(|(name, view)| ((*name).to_owned(), *view))
            .collect::<Vec<_>>();
        formal_views.sort_by(|(left, _), (right, _)| left.cmp(right));
        let recording = self.transport.replace_recording(false);
        let mut summary = AliasFormalFreeNames::default();
        for (formal, view) in formal_views {
            let free = self.free_names_stored(view);
            summary.by_formal.insert(formal, free);
        }
        self.transport.replace_recording(recording);
        let summary = Arc::new(summary);
        self.context_mut(context).formal_free_names = Some(summary.clone());
        summary
    }

    fn fresh_emitted_binder(
        base: &str,
        free: &AliasFormalFreeNames,
        context: &mut AliasViewContext<'v, 'c, 'm, P>,
    ) -> String {
        if !free.contains_except(base, base) {
            return base.to_owned();
        }
        let base_key = AliasOwnedName(base.to_owned());
        let mut suffix = context
            .next_emitted_suffix
            .get(&base_key)
            .copied()
            .unwrap_or(2);
        loop {
            #[cfg(test)]
            update_alias_materializer_work(|work| work.alpha_candidate_checks += 1);
            let candidate = crate::naming::indexed_name(base, suffix);
            let key = AliasOwnedName(candidate.clone());
            if !free.contains_except(&candidate, base) && !context.emitted_binders.contains(&key) {
                let next = suffix
                    .checked_add(1)
                    .expect("an alpha-renamed type binder suffix fits in usize");
                context.next_emitted_suffix = context
                    .next_emitted_suffix
                    .remove(&base_key)
                    .insert_with(base_key, next, |left, right| left == right)
                    .expect("an emitted binder base has one next suffix");
                return candidate;
            }
            suffix = suffix
                .checked_add(1)
                .expect("an alpha-renamed type binder suffix fits in usize");
        }
    }

    fn materialize_stored(&mut self, id: AliasViewId) -> AliasMaterialized<P, S::Node> {
        self.materialize(self.views[id.0])
    }

    fn materialize(&mut self, view: AliasVirtualView<'v, P>) -> AliasMaterialized<P, S::Node> {
        #[cfg(test)]
        update_alias_materializer_work(|work| work.materialize_calls += 1);
        let nominal = self.context(view.context).scope.nominal();
        match view.ty {
            crate::ast::Type::Unit { .. }
            | crate::ast::Type::Bottom { .. }
            | crate::ast::Type::Infer { .. } => {
                assert_eq!(
                    self.arg_rope_len(view.appended),
                    0,
                    "only a path-like type head accepts appended type arguments"
                );
                let transport = self.transport.finish_node(
                    view.ty,
                    nominal,
                    AliasTransportNodeKind::Ordinary,
                    S::Children::default(),
                );
                AliasMaterialized {
                    ty: (*view.ty).clone(),
                    identity_canonical: true,
                    transport,
                }
            }
            crate::ast::Type::Function {
                param,
                ret,
                meta,
                abi_arity,
                caps,
            } => {
                assert_eq!(
                    self.arg_rope_len(view.appended),
                    0,
                    "a function type cannot receive appended type arguments"
                );
                let param = self.materialize(AliasVirtualView {
                    ty: param,
                    context: view.context,
                    appended: AliasArgRopeId(0),
                    application_span: param.span(),
                });
                let ret = self.materialize(AliasVirtualView {
                    ty: ret,
                    context: view.context,
                    appended: AliasArgRopeId(0),
                    application_span: ret.span(),
                });
                let mut children = S::Children::default();
                self.transport.push_child(
                    &mut children,
                    AliasTransportEdge::FunctionParam,
                    param.transport,
                );
                self.transport.push_child(
                    &mut children,
                    AliasTransportEdge::FunctionReturn,
                    ret.transport,
                );
                let transport = self.transport.finish_node(
                    view.ty,
                    nominal,
                    AliasTransportNodeKind::Ordinary,
                    children,
                );
                AliasMaterialized {
                    ty: crate::ast::Type::Function {
                        param: Box::new(param.ty),
                        ret: Box::new(ret.ty),
                        meta: meta.clone(),
                        abi_arity: *abi_arity,
                        caps: caps.clone(),
                    },
                    identity_canonical: param.identity_canonical && ret.identity_canonical,
                    transport,
                }
            }
            crate::ast::Type::Product { left, right, meta } => {
                assert_eq!(
                    self.arg_rope_len(view.appended),
                    0,
                    "a product type cannot receive appended type arguments"
                );
                let left = self.materialize(AliasVirtualView {
                    ty: left,
                    context: view.context,
                    appended: AliasArgRopeId(0),
                    application_span: left.span(),
                });
                let right = self.materialize(AliasVirtualView {
                    ty: right,
                    context: view.context,
                    appended: AliasArgRopeId(0),
                    application_span: right.span(),
                });
                let mut children = S::Children::default();
                self.transport.push_child(
                    &mut children,
                    AliasTransportEdge::ProductLeft,
                    left.transport,
                );
                self.transport.push_child(
                    &mut children,
                    AliasTransportEdge::ProductRight,
                    right.transport,
                );
                let transport = self.transport.finish_node(
                    view.ty,
                    nominal,
                    AliasTransportNodeKind::Ordinary,
                    children,
                );
                AliasMaterialized {
                    ty: crate::ast::Type::Product {
                        left: Box::new(left.ty),
                        right: Box::new(right.ty),
                        meta: meta.clone(),
                    },
                    identity_canonical: left.identity_canonical && right.identity_canonical,
                    transport,
                }
            }
            crate::ast::Type::Sum { left, right, meta } => {
                assert_eq!(
                    self.arg_rope_len(view.appended),
                    0,
                    "a sum type cannot receive appended type arguments"
                );
                let left = self.materialize(AliasVirtualView {
                    ty: left,
                    context: view.context,
                    appended: AliasArgRopeId(0),
                    application_span: left.span(),
                });
                let right = self.materialize(AliasVirtualView {
                    ty: right,
                    context: view.context,
                    appended: AliasArgRopeId(0),
                    application_span: right.span(),
                });
                let mut children = S::Children::default();
                self.transport.push_child(
                    &mut children,
                    AliasTransportEdge::SumLeft,
                    left.transport,
                );
                self.transport.push_child(
                    &mut children,
                    AliasTransportEdge::SumRight,
                    right.transport,
                );
                let transport = self.transport.finish_node(
                    view.ty,
                    nominal,
                    AliasTransportNodeKind::Ordinary,
                    children,
                );
                AliasMaterialized {
                    ty: crate::ast::Type::Sum {
                        left: Box::new(left.ty),
                        right: Box::new(right.ty),
                        meta: meta.clone(),
                    },
                    identity_canonical: left.identity_canonical && right.identity_canonical,
                    transport,
                }
            }
            crate::ast::Type::Forall { param, body, meta } => {
                assert_eq!(
                    self.arg_rope_len(view.appended),
                    0,
                    "a quantified value type cannot receive appended type arguments"
                );
                let free = self.formal_free_names(view.context);
                let mut context = self.context(view.context).clone();
                let emitted = Self::fresh_emitted_binder(&param.name, &free, &mut context);
                let key = AliasBorrowedName(param.name.as_str());
                context.protected = context
                    .protected
                    .remove(&key)
                    .insert_with(key, emitted.clone(), |left, right| left == right)
                    .expect("a structural binder has one emitted spelling");
                context.emitted_binders = context
                    .emitted_binders
                    .insert(AliasOwnedName(emitted.clone()));
                let context = self.alloc_context(context);
                let body = self.materialize(AliasVirtualView {
                    ty: body,
                    context,
                    appended: AliasArgRopeId(0),
                    application_span: body.span(),
                });
                let mut children = S::Children::default();
                self.transport.push_child(
                    &mut children,
                    AliasTransportEdge::ForallBody,
                    body.transport,
                );
                let transport = self.transport.finish_node(
                    view.ty,
                    nominal,
                    AliasTransportNodeKind::Forall(param),
                    children,
                );
                AliasMaterialized {
                    ty: crate::ast::Type::Forall {
                        param: crate::ast::TypeParam {
                            name: emitted,
                            span: param.span,
                            kind: param.kind.clone(),
                        },
                        body: Box::new(body.ty),
                        meta: meta.clone(),
                    },
                    identity_canonical: body.identity_canonical,
                    transport,
                }
            }
            crate::ast::Type::Path {
                segments,
                args,
                meta,
            } => self.materialize_path(view, segments, args, meta),
            crate::ast::Type::Goal {
                goal,
                args,
                meta,
                ext,
            } => {
                let (args, canonical, children) =
                    self.materialize_args(args, view.context, view.appended);
                let transport = self.transport.finish_node(
                    view.ty,
                    nominal,
                    AliasTransportNodeKind::Ordinary,
                    children,
                );
                AliasMaterialized {
                    ty: crate::ast::Type::Goal {
                        goal: *goal,
                        args,
                        meta: meta.clone(),
                        ext: ext.clone(),
                    },
                    identity_canonical: canonical,
                    transport,
                }
            }
            crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        }
    }

    /// Resolve one virtual path frontier without materializing it.
    ///
    /// Both ordinary materialization and complete-scheme demand use this one
    /// transition. A formal selection restores the caller view it captured;
    /// an exact saturated alias enters its raw body under the selected owner.
    /// Structural/ambient binders and terminal nominals deliberately return
    /// `None` and remain opaque to alias redirection.
    fn redirect_path(
        &mut self,
        view: AliasVirtualView<'v, P>,
        segments: &'v [crate::ast::PathSegment],
        args: &'v [crate::ast::Type<P>],
    ) -> Option<AliasPathRedirect<'v, 'm, P>> {
        let context = self.context(view.context).clone();
        let head = segments.last().map(crate::ast::PathSegment::as_str);
        if segments.len() == 1
            && let Some(name) = head
            && (context.protected.get(&AliasBorrowedName(name)).is_some()
                || context.ambient_protected.contains(name))
        {
            return None;
        }

        if segments.len() == 1
            && let Some(name) = head
            && let Some(bound) = context
                .formals
                .as_deref()
                .and_then(|formals| formals.get(name))
                .copied()
        {
            let bound_view = self.views[bound.0];
            let native_views = self.alloc_native_args(args, view.context);
            let native = self.arg_leaf(native_views);
            let appended = self.concat_args(bound_view.appended, native);
            let appended = self.concat_args(appended, view.appended);
            let selected = if appended.0 == bound_view.appended.0 {
                bound_view
            } else {
                AliasVirtualView {
                    ty: bound_view.ty,
                    context: bound_view.context,
                    appended,
                    application_span: view.application_span,
                }
            };
            let selected_scope = self.context(selected.context).scope.nominal();
            return Some(AliasPathRedirect::Formal {
                formal_ty: view.ty,
                formal: name,
                selected,
                selected_scope,
            });
        }

        let resolved = self.resolve_alias(segments, &context)?;
        let required = resolved.def.type_params.len();
        let supplied = args
            .len()
            .checked_add(self.arg_rope_len(view.appended))
            .expect("a virtual type application argument count fits in usize");
        if args.len() > required || supplied < required {
            return None;
        }
        let active = Self::active_alias_key(resolved);
        if context.active_aliases.contains(&active) {
            // Valid source and phase artifacts reject recursive transparent
            // aliases before semantic consumption. Retain the cutoff only so
            // a malformed internal graph cannot recurse indefinitely.
            self.defensive_cycle_cutoff = true;
            return None;
        }

        let missing = required - args.len();
        let mut formal_arguments = self.alloc_native_args(args, view.context);
        let (appended_formals, remaining) = self.split_arg_prefix(view.appended, missing);
        formal_arguments.extend(appended_formals);
        let mut formals = HashMap::with_capacity(required);
        for (param, argument) in resolved.def.type_params.iter().zip(formal_arguments) {
            let name: &'v str = param.name.as_str();
            formals.insert(name, argument);
        }
        let scope = resolved.owner_module.map_or(context.scope, |_| {
            AliasViewScope::Owner(resolved.owner_scope)
        });
        let body_context = AliasViewContext {
            scope,
            formals: (!formals.is_empty()).then(|| Arc::new(formals)),
            ambient_protected: AliasAmbientProtected::None,
            protected: PersistentExactIndex::default(),
            emitted_binders: context.emitted_binders.clone(),
            next_emitted_suffix: context.next_emitted_suffix.clone(),
            formal_free_names: None,
            active_aliases: context.active_aliases.insert(active),
            identity_canonical: false,
        };
        let body_context = self.alloc_context(body_context);
        let body: &'v crate::ast::Type<P> = resolved.def.body;
        Some(AliasPathRedirect::Alias {
            application: view.ty,
            body: AliasVirtualView {
                ty: body,
                context: body_context,
                appended: remaining,
                application_span: view.application_span,
            },
            owner_scope: resolved.owner_scope,
        })
    }

    fn complete_function_scheme(
        &mut self,
        root: AliasVirtualView<'v, P>,
    ) -> AliasCompleteSchemeAnalysis {
        let mut proved_bare_spines = Vec::new();
        let result = super::kind_scheme::classify_complete_scheme(root, |view| {
            #[cfg(test)]
            update_alias_materializer_work(|work| work.scheme_frontier_steps += 1);
            let context = self.context(view.context);
            if self.arg_rope_len(view.appended) == 0
                && context.formals.is_none()
                && context.active_aliases.is_empty()
                && matches!(view.ty, crate::ast::Type::Forall { .. })
            {
                proved_bare_spines.push(view.ty as *const crate::ast::Type<P> as usize);
            }
            let frontier = match view.ty {
                crate::ast::Type::Forall { param, body, .. }
                    if self.arg_rope_len(view.appended) == 0 =>
                {
                    let mut context = self.context(view.context).clone();
                    let key = AliasBorrowedName(param.name.as_str());
                    context.protected = context
                        .protected
                        .remove(&key)
                        .insert_with(key, param.name.clone(), |left, right| left == right)
                        .expect("a structural binder has one protected spelling");
                    let context = self.alloc_context(context);
                    super::kind_scheme::SchemeFrontier::Continue(AliasVirtualView {
                        ty: body,
                        context,
                        appended: AliasArgRopeId(0),
                        application_span: body.span(),
                    })
                }
                crate::ast::Type::Function { .. } if self.arg_rope_len(view.appended) == 0 => {
                    super::kind_scheme::SchemeFrontier::Complete
                }
                crate::ast::Type::Path { segments, args, .. } => {
                    match self.redirect_path(view, segments, args) {
                        Some(AliasPathRedirect::Formal { selected, .. }) => {
                            super::kind_scheme::SchemeFrontier::Continue(selected)
                        }
                        Some(AliasPathRedirect::Alias { body, .. }) => {
                            super::kind_scheme::SchemeFrontier::Continue(body)
                        }
                        None => super::kind_scheme::SchemeFrontier::Incomplete,
                    }
                }
                crate::ast::Type::Infer { .. } => {
                    super::kind_scheme::SchemeFrontier::SourcePlaceholder
                }
                crate::ast::Type::Unit { .. }
                | crate::ast::Type::Bottom { .. }
                | crate::ast::Type::Product { .. }
                | crate::ast::Type::Sum { .. }
                | crate::ast::Type::Forall { .. }
                | crate::ast::Type::Function { .. } => {
                    super::kind_scheme::SchemeFrontier::Incomplete
                }
                crate::ast::Type::Goal { .. } => super::kind_scheme::SchemeFrontier::Deferred,
                crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
            };
            Ok::<_, std::convert::Infallible>(frontier)
        });
        let summary = match result {
            Ok(summary) => summary,
            Err(never) => match never {},
        };
        if !summary.is_complete() {
            proved_bare_spines.clear();
        }
        AliasCompleteSchemeAnalysis {
            summary,
            proved_bare_spines,
        }
    }

    fn materialize_path(
        &mut self,
        mut view: AliasVirtualView<'v, P>,
        mut segments: &'v [crate::ast::PathSegment],
        mut args: &'v [crate::ast::Type<P>],
        mut _meta: &'v crate::ast::Meta<P>,
    ) -> AliasMaterialized<P, S::Node> {
        loop {
            let context = self.context(view.context).clone();
            let head = segments.last().map(crate::ast::PathSegment::as_str);

            if segments.len() == 1
                && let Some(name) = head
                && let Some(emitted) = context
                    .protected
                    .get(&AliasBorrowedName(name))
                    .cloned()
                    .or_else(|| {
                        context
                            .ambient_protected
                            .contains(name)
                            .then(|| name.to_owned())
                    })
            {
                let (args, args_canonical, children) =
                    self.materialize_args(args, view.context, view.appended);
                let transport = self.transport.finish_node(
                    view.ty,
                    context.scope.nominal(),
                    AliasTransportNodeKind::Ordinary,
                    children,
                );
                return AliasMaterialized {
                    ty: crate::ast::Type::synth_path(vec![emitted], args, view.application_span),
                    identity_canonical: args_canonical,
                    transport,
                };
            }

            if let Some(redirect) = self.redirect_path(view, segments, args) {
                let (redirected, contribution) = match redirect {
                    AliasPathRedirect::Formal {
                        formal_ty,
                        formal,
                        selected,
                        selected_scope,
                    } => (
                        selected,
                        AliasPathRedirect::Formal {
                            formal_ty,
                            formal,
                            selected,
                            selected_scope,
                        },
                    ),
                    AliasPathRedirect::Alias {
                        application,
                        body,
                        owner_scope,
                    } => (
                        body,
                        AliasPathRedirect::Alias {
                            application,
                            body,
                            owner_scope,
                        },
                    ),
                };
                if !S::RECORDS_TRANSPORT
                    && let crate::ast::Type::Path {
                        segments: redirected_segments,
                        args: redirected_args,
                        meta: redirected_meta,
                    } = redirected.ty
                {
                    #[cfg(test)]
                    update_alias_materializer_work(|work| work.path_redirects += 1);
                    view = redirected;
                    segments = redirected_segments;
                    args = redirected_args;
                    _meta = redirected_meta;
                    continue;
                }
                let alias_transporting = self.transport.replace_alias_transporting(true);
                let mut materialized = self.materialize(redirected);
                materialized.transport = match contribution {
                    AliasPathRedirect::Formal {
                        formal_ty,
                        formal,
                        selected_scope,
                        ..
                    } => self.transport.graft(
                        formal_ty,
                        formal,
                        selected_scope,
                        materialized.transport,
                    ),
                    AliasPathRedirect::Alias {
                        application,
                        owner_scope,
                        ..
                    } => {
                        self.transport
                            .alias_root(application, owner_scope, materialized.transport)
                    }
                };
                self.transport
                    .replace_alias_transporting(alias_transporting);
                return materialized;
            }

            let (args, args_canonical, children) =
                self.materialize_args(args, view.context, view.appended);
            let (segments, head_canonical) = self.qualify_nominal_segments(segments, &context);
            let transport = self.transport.finish_node(
                view.ty,
                context.scope.nominal(),
                AliasTransportNodeKind::Ordinary,
                children,
            );
            return AliasMaterialized {
                ty: crate::ast::Type::synth_path_segments(segments, args, view.application_span),
                identity_canonical: head_canonical && args_canonical,
                transport,
            };
        }
    }
}

fn materialize_alias_root_state<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    identity_canonical: bool,
) -> Option<AliasRootMaterialized<P>>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
    'm: 'v,
{
    let ambient_protected = ctx
        .binder_locals
        .map_or(AliasAmbientProtected::None, |binders| {
            AliasAmbientProtected::Lookup(binders)
        });
    materialize_alias_root_state_with_ambient(ty, ctx, ambient_protected, identity_canonical)
}

fn materialize_alias_root_state_with_ambient<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    ambient_protected: AliasAmbientProtected<'a>,
    identity_canonical: bool,
) -> Option<AliasRootMaterialized<P>>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
    'm: 'v,
{
    let provider = NominalProvider::new(ctx.source_module, ctx.package);
    materialize_alias_root_state_with_provider(
        ty,
        ctx,
        &provider,
        provider.root(),
        ambient_protected,
        identity_canonical,
    )
}

fn materialize_alias_root_state_with_provider<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    provider: &NominalProvider<'m, P>,
    nominal: AliasNominalScope<'m, P>,
    ambient_protected: AliasAmbientProtected<'a>,
    identity_canonical: bool,
) -> Option<AliasRootMaterialized<P>>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
    'm: 'v,
{
    let crate::ast::Type::Path { segments, args, .. } = ty else {
        return None;
    };
    if segments.is_empty()
        || matches!(segments.as_slice(), [only]
            if ambient_protected.contains(only.as_str()))
    {
        return None;
    }
    let resolved = resolve_alias_declaration_head_with_provider(
        segments,
        nominal,
        provider,
        Some(ctx.local),
        Some(ctx.cross_module),
        identity_canonical,
    )?;
    if resolved.def.type_params.len() != args.len() {
        return None;
    }
    let (mut materializer, root) = AliasMaterializer::for_caller(
        ty,
        *ctx,
        nominal,
        provider,
        ambient_protected,
        identity_canonical,
    );
    let materialized = materializer.materialize(root);
    Some(AliasRootMaterialized {
        ty: materialized.ty,
        identity_canonical: materialized.identity_canonical,
        defensive_cycle_cutoff: materializer.defensive_cycle_cutoff,
    })
}

/// Materialize every transparent-alias frontier in `ty` through one exact
/// source-module/package scope.
///
/// This is the validated-artifact adapter used by semantic consumers that do
/// not retain the typer's local/cross-module spelling maps. It deliberately
/// reuses the same occurrence-aware cursor as structural type checking; it
/// does not infer a declaration from a leaf outside `source_module` or the
/// exact qualified package path carried by `ty`.
#[cfg(feature = "surface")]
pub(crate) fn materialize_aliases_in_scope<P>(
    ty: &crate::ast::Type<P>,
    source_module: Option<&crate::ast::Module<P>>,
    package: Option<&crate::pass::resolve::Package<P>>,
    binder_locals: Option<&HashSet<String>>,
    identity_canonical: bool,
) -> crate::ast::Type<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    if matches!(
        ty,
        crate::ast::Type::Unit { .. }
            | crate::ast::Type::Bottom { .. }
            | crate::ast::Type::Infer { .. }
    ) {
        return ty.clone();
    }
    let provider = NominalProvider::new(source_module, package);
    let source_scope = provider.root();
    match ty {
        crate::ast::Type::Path { segments, args, .. } if args.is_empty() => {
            let protected_binder = matches!(segments.as_slice(), [only]
                if binder_locals.is_some_and(|binders| binders.contains(only.as_str())));
            let exact_non_alias = identity_canonical
                && segments.len() > 1
                && resolve_alias_declaration_head_with_provider(
                    segments,
                    source_scope,
                    &provider,
                    None,
                    None,
                    true,
                )
                .is_none();
            if protected_binder || exact_non_alias {
                return ty.clone();
            }
        }
        _ => {}
    }

    let local = HashMap::new();
    let cross_module = HashMap::new();
    let ctx = AliasCtx {
        local: &local,
        cross_module: &cross_module,
        type_interner: None,
        source_module,
        package,
        binder_locals,
    };
    let ambient_protected = ctx
        .binder_locals
        .map_or(AliasAmbientProtected::None, |binders| {
            AliasAmbientProtected::Lookup(binders)
        });
    let (mut materializer, root) = AliasMaterializer::for_caller(
        ty,
        ctx,
        source_scope,
        &provider,
        ambient_protected,
        identity_canonical,
    );
    materializer.materialize(root).ty
}

/// Measure a type after exact transparent-alias redirection without building
/// the materialized output. This is the stack-safe structural fold used by
/// compile-time recursion fuel; it intentionally shares the ordinary alias
/// materializer's virtual cursor and resolution authority.
#[cfg(feature = "surface")]
pub(crate) fn measure_materialized_aliases_in_scope<P>(
    ty: &crate::ast::Type<P>,
    source_module: Option<&crate::ast::Module<P>>,
    package: Option<&crate::pass::resolve::Package<P>>,
    binder_locals: Option<&HashSet<String>>,
    identity_canonical: bool,
) -> Option<usize>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    let provider = NominalProvider::new(source_module, package);
    let source_scope = provider.root();
    let local = HashMap::new();
    let cross_module = HashMap::new();
    let ctx = AliasCtx {
        local: &local,
        cross_module: &cross_module,
        type_interner: None,
        source_module,
        package,
        binder_locals,
    };
    let ambient_protected = ctx
        .binder_locals
        .map_or(AliasAmbientProtected::None, |binders| {
            AliasAmbientProtected::Lookup(binders)
        });
    let (mut materializer, root) = AliasMaterializer::for_caller(
        ty,
        ctx,
        source_scope,
        &provider,
        ambient_protected,
        identity_canonical,
    );
    materializer.materialized_node_measure(root)
}

/// Materialize one admitted source annotation while indexing its direct source
/// occurrences and lazily recording exact source-to-output alias transport.
///
/// This is the planning-only policy of the same materializer used by ordinary
/// structural consumers. Resolution, owner restoration, qualification,
/// capture avoidance and argument saturation are identical; only this policy
/// retains the transient sidecar.
#[cfg(all(test, feature = "surface"))]
pub(crate) fn materialize_aliases_with_source_transport<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    identity_canonical: bool,
) -> AliasSourceMaterialized<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
    'm: 'v,
{
    let ambient = ctx
        .binder_locals
        .map_or(AliasAmbientProtected::None, |binders| {
            AliasAmbientProtected::Lookup(binders)
        });
    materialize_aliases_with_source_transport_and_ambient(ty, ctx, ambient, identity_canonical)
}

pub(crate) fn materialize_aliases_with_source_transport_and_binder_lookup<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    binders: &'a dyn AliasBinderLookup,
    identity_canonical: bool,
) -> AliasSourceMaterialized<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
    'm: 'v,
    'a: 'v,
{
    materialize_aliases_with_source_transport_and_ambient(
        ty,
        ctx,
        AliasAmbientProtected::Lookup(binders),
        identity_canonical,
    )
}

fn materialize_aliases_with_source_transport_and_ambient<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    ambient_protected: AliasAmbientProtected<'a>,
    identity_canonical: bool,
) -> AliasSourceMaterialized<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
    'm: 'v,
{
    let provider = NominalProvider::new(ctx.source_module, ctx.package);
    let nominal = provider.root();
    let sink = SourceOccurrenceTransportSink::new(ty);
    let (mut materializer, root) = AliasMaterializer::for_caller_with_transport(
        ty,
        *ctx,
        nominal,
        &provider,
        ambient_protected,
        identity_canonical,
        sink,
    );
    let AliasMaterialized {
        ty,
        identity_canonical,
        transport: root,
    } = materializer.materialize(root);
    AliasSourceMaterialized {
        ty,
        identity_canonical,
        transport: materializer.transport.finish(root),
    }
}

/// Classify the exact virtual alias frontier as `forall* -> function`
/// without allocating a substituted/materialized type.
#[cfg(all(test, feature = "surface"))]
pub(crate) fn analyze_complete_function_scheme_in_scope<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    identity_canonical: bool,
) -> AliasCompleteSchemeAnalysis
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
    'm: 'v,
{
    let provider = NominalProvider::new(ctx.source_module, ctx.package);
    let nominal = provider.root();
    let ambient_protected = ctx
        .binder_locals
        .map_or(AliasAmbientProtected::None, |binders| {
            AliasAmbientProtected::Lookup(binders)
        });
    let (mut materializer, root) = AliasMaterializer::for_caller(
        ty,
        *ctx,
        nominal,
        &provider,
        ambient_protected,
        identity_canonical,
    );
    materializer.complete_function_scheme(root)
}

pub(crate) fn analyze_complete_function_scheme_in_scope_with_binder_lookup<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    binders: &'a dyn AliasBinderLookup,
    identity_canonical: bool,
) -> AliasCompleteSchemeAnalysis
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
    'a: 'v,
    'm: 'v,
{
    let provider = NominalProvider::new(ctx.source_module, ctx.package);
    let nominal = provider.root();
    let (mut materializer, root) = AliasMaterializer::for_caller(
        ty,
        *ctx,
        nominal,
        &provider,
        AliasAmbientProtected::Lookup(binders),
        identity_canonical,
    );
    materializer.complete_function_scheme(root)
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn classify_complete_function_scheme_in_scope<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    identity_canonical: bool,
) -> super::kind_scheme::CompleteScheme
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
    'm: 'v,
{
    analyze_complete_function_scheme_in_scope(ty, ctx, identity_canonical).summary
}

pub(crate) fn analyze_complete_function_scheme_in_nominal_scope<'v, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    provider: &NominalProvider<'m, P>,
    nominal: AliasNominalScope<'m, P>,
    binders: &'v dyn AliasBinderLookup,
    identity_canonical: bool,
) -> AliasCompleteSchemeAnalysis
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
    'm: 'v,
{
    let (mut materializer, root) = AliasMaterializer::for_nominal_scope(
        ty,
        nominal,
        provider,
        AliasAmbientProtected::Lookup(binders),
        identity_canonical,
    );
    materializer.complete_function_scheme(root)
}

/// If `ty` is a `Type::Path` resolving to a type alias — either an
/// intra-module one (declared in this module) or a cross-module one
/// (imported via `import m(Foo);`) — substitute the alias's
/// body with type-args bound. Repeats until a non-alias head is
/// reached. Returns the (possibly-unfolded) type.
///
/// The typer's kind walk checks exact alias saturation against the
/// identity-resolved declaration in its owner module. `unfold_top` stays
/// structural: an arity mismatch is left unchanged rather than diagnosed here.
///
/// Phase-polymorphic across the resolved typer phases. Alias bodies and
/// supplied arguments are canonicalized in their own scopes *before*
/// substitution, so a caller argument cannot be captured by a same-named
/// declaration in the alias owner.
pub fn unfold_top<P>(ty: &crate::ast::Type<P>, ctx: &AliasCtx<'_, '_, P>) -> crate::ast::Type<P>
where
    P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
{
    materialize_alias_root_state(ty, ctx, false)
        .map_or_else(|| ty.clone(), |materialized| materialized.ty)
}

fn binder_set(binders: Option<&dyn AliasBinderLookup>) -> HashSet<String> {
    let mut set = HashSet::new();
    if let Some(binders) = binders {
        binders.for_each_alias_binder(&mut |name| {
            set.insert(name.to_owned());
        });
    }
    set
}

fn canonical_alias_body<P>(
    alias: AliasDef<'_, P>,
    owner: &crate::ast::Module<P>,
    _head: &str,
) -> crate::ast::Type<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    let provider = NominalProvider::new(Some(owner), None);
    let (mut materializer, root) = AliasMaterializer::for_owner_body(alias, owner, &provider);
    materializer.materialize(root).ty
}

/// Canonicalize a raw [`crate::ast::Item::TypeAlias`] body in its declaration
/// module before caller-owned arguments are substituted into it.
pub(crate) fn canonical_declared_alias_body<P>(
    alias: &crate::ast::TypeAlias<P>,
    owner: &crate::ast::Module<P>,
) -> crate::ast::Type<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    canonical_alias_body(
        AliasDef {
            type_params: &alias.type_params,
            body: alias.type_body(),
            owner_module: Some(owner),
        },
        owner,
        &alias.name,
    )
}

fn follow_type_reexports_deep_in_scope<P>(
    ty: &crate::ast::Type<P>,
    nominal: AliasNominalScope<'_, P>,
    package: Option<&crate::pass::resolve::Package<P>>,
) -> crate::ast::Type<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    let Some(package) = package else {
        return ty.clone();
    };
    match ty {
        crate::ast::Type::Path {
            segments,
            args,
            meta,
        } => crate::ast::Type::Path {
            segments: nominal_scope_follows_type_reexport(segments, nominal, Some(package)),
            args: args
                .iter()
                .map(|arg| follow_type_reexports_deep_in_scope(arg, nominal, Some(package)))
                .collect(),
            meta: meta.clone(),
        },
        crate::ast::Type::Function {
            param,
            ret,
            meta,
            abi_arity,
            caps,
        } => crate::ast::Type::Function {
            param: Box::new(follow_type_reexports_deep_in_scope(
                param,
                nominal,
                Some(package),
            )),
            ret: Box::new(follow_type_reexports_deep_in_scope(
                ret,
                nominal,
                Some(package),
            )),
            meta: meta.clone(),
            abi_arity: *abi_arity,
            caps: caps.clone(),
        },
        crate::ast::Type::Product { left, right, meta } => crate::ast::Type::Product {
            left: Box::new(follow_type_reexports_deep_in_scope(
                left,
                nominal,
                Some(package),
            )),
            right: Box::new(follow_type_reexports_deep_in_scope(
                right,
                nominal,
                Some(package),
            )),
            meta: meta.clone(),
        },
        crate::ast::Type::Sum { left, right, meta } => crate::ast::Type::Sum {
            left: Box::new(follow_type_reexports_deep_in_scope(
                left,
                nominal,
                Some(package),
            )),
            right: Box::new(follow_type_reexports_deep_in_scope(
                right,
                nominal,
                Some(package),
            )),
            meta: meta.clone(),
        },
        crate::ast::Type::Forall { param, body, meta } => crate::ast::Type::Forall {
            param: param.clone(),
            body: Box::new(follow_type_reexports_deep_in_scope(
                body,
                nominal,
                Some(package),
            )),
            meta: meta.clone(),
        },
        crate::ast::Type::Unit { .. }
        | crate::ast::Type::Bottom { .. }
        | crate::ast::Type::Infer { .. } => ty.clone(),
        crate::ast::Type::Goal {
            goal,
            args,
            meta,
            ext,
        } => crate::ast::Type::Goal {
            goal: *goal,
            args: args
                .iter()
                .map(|arg| follow_type_reexports_deep_in_scope(arg, nominal, Some(package)))
                .collect(),
            meta: meta.clone(),
            ext: ext.clone(),
        },
        crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
    }
}

pub(crate) fn follow_type_reexports_deep<P>(
    ty: &crate::ast::Type<P>,
    package: Option<&crate::pass::resolve::Package<P>>,
) -> crate::ast::Type<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    follow_type_reexports_deep_in_scope(ty, AliasNominalScope::Absent, package)
}

/// Whether a `Type::Path`'s head is a single bare segment naming one of
/// the scheme binders in scope at the comparison site
/// ([`AliasCtx::binder_locals`]). Such a head is a type variable that
/// head canonicalization must leave bare; a multi-segment (already-
/// qualified) head, or any head when no binders are in scope, is not.
fn head_names_in_scope_binder<P>(
    segments: &[crate::ast::PathSegment],
    ctx: &AliasCtx<'_, '_, P>,
) -> bool
where
    P: crate::ast::Phase,
{
    head_names_in_scope_binder_with_lookup(
        segments,
        ctx.binder_locals
            .map(|binders| binders as &dyn AliasBinderLookup),
    )
}

fn head_names_in_scope_binder_with_lookup(
    segments: &[crate::ast::PathSegment],
    binders: Option<&dyn AliasBinderLookup>,
) -> bool {
    matches!(segments, [only]
        if binders.is_some_and(|binders| binders.contains_alias_binder(only.as_str())))
}

/// Whether a path frontier names a saturated transparent alias.
///
/// This is the borrowed, head-only probe used by structural consumers that
/// already canonicalized the enclosing type. It deliberately does not clone
/// or walk `args`: a non-alias nominal frontier can therefore be rejected
/// without rebuilding the remaining type spine.
#[cfg(feature = "surface")]
fn path_has_unfoldable_alias_head<P>(
    segments: &[crate::ast::PathSegment],
    supplied_args: usize,
    ctx: &AliasCtx<'_, '_, P>,
    already_canonical: bool,
) -> bool
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    let provider = NominalProvider::new(ctx.source_module, ctx.package);
    path_has_unfoldable_alias_head_with_provider(
        segments,
        supplied_args,
        ctx,
        &provider,
        provider.root(),
        already_canonical,
    )
}

fn path_has_unfoldable_alias_head_with_provider<'m, P>(
    segments: &[crate::ast::PathSegment],
    supplied_args: usize,
    ctx: &AliasCtx<'_, 'm, P>,
    provider: &NominalProvider<'m, P>,
    nominal: AliasNominalScope<'m, P>,
    already_canonical: bool,
) -> bool
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    if segments.is_empty() {
        return false;
    }
    if head_names_in_scope_binder(segments, ctx) {
        return false;
    }
    resolve_alias_declaration_head_with_provider(
        segments,
        nominal,
        provider,
        Some(ctx.local),
        Some(ctx.cross_module),
        already_canonical,
    )
    .is_some_and(|resolved| resolved.def.type_params.len() == supplied_args)
}

/// Whether `ty` has one saturated transparent-alias head at this structural
/// frontier. This probe borrows the path and never traverses its arguments;
/// callers can therefore avoid constructing binder snapshots for the common
/// nominal miss while retaining the full binder-aware expansion on a hit.
#[cfg(feature = "surface")]
pub(crate) fn has_unfoldable_alias_frontier_for_comparison<P>(
    ty: &crate::ast::Type<P>,
    ctx: &AliasCtx<'_, '_, P>,
    already_canonical: bool,
) -> bool
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    let crate::ast::Type::Path { segments, args, .. } = ty else {
        return false;
    };
    path_has_unfoldable_alias_head(segments, args.len(), ctx, already_canonical)
}

/// Unfold one transparent-alias frontier for a structural consumer.
///
/// `None` preserves the caller's borrowed view. A successful hit returns the
/// owned alias expansion; unlike [`canonicalize_for_comparison`], the
/// canonical fast path does not walk that expansion a second time after
/// [`unfold_and_qualify_state`] has already made the provider body and caller
/// arguments identity-exact in their respective scopes. A malformed recursive
/// graph that reaches the materializer's defensive active-alias guard also
/// returns `None`: reopening its opaque cutoff in a fresh frontier would turn
/// one bounded defensive normalization into unbounded structural recursion.
#[cfg(feature = "surface")]
pub(crate) fn unfold_alias_frontier_for_comparison<P>(
    ty: &crate::ast::Type<P>,
    ctx: &AliasCtx<'_, '_, P>,
    already_canonical: bool,
) -> Option<(crate::ast::Type<P>, bool)>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    let provider = NominalProvider::new(ctx.source_module, ctx.package);
    unfold_alias_frontier_for_comparison_with_provider(
        ty,
        ctx,
        &provider,
        provider.root(),
        already_canonical,
    )
}

fn unfold_alias_frontier_for_comparison_with_provider<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    provider: &NominalProvider<'m, P>,
    nominal: AliasNominalScope<'m, P>,
    already_canonical: bool,
) -> Option<(crate::ast::Type<P>, bool)>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
    'a: 'v,
    'm: 'v,
{
    let crate::ast::Type::Path { segments, args, .. } = ty else {
        return None;
    };
    if !path_has_unfoldable_alias_head_with_provider(
        segments,
        args.len(),
        ctx,
        provider,
        nominal,
        already_canonical,
    ) {
        return None;
    }
    let ambient = ctx
        .binder_locals
        .map_or(AliasAmbientProtected::None, |binders| {
            AliasAmbientProtected::Lookup(binders)
        });
    let materialized = materialize_alias_root_state_with_provider(
        ty,
        ctx,
        provider,
        nominal,
        ambient,
        already_canonical,
    )
    .expect("an unfoldable alias frontier materializes");
    if materialized.defensive_cycle_cutoff {
        return None;
    }
    if materialized.identity_canonical {
        Some((materialized.ty, true))
    } else {
        let ambient = ctx
            .binder_locals
            .map_or(AliasAmbientProtected::None, |binders| {
                AliasAmbientProtected::Lookup(binders)
            });
        Some(canonicalize_for_comparison_with_provider(
            ty,
            ctx,
            provider,
            nominal,
            ambient,
            already_canonical,
        ))
    }
}

/// Unfold one transparent-alias frontier for a consumer that already owns an
/// exact rigid-scope membership proof.
///
/// The same alias materializer performs the expansion. Only the ambient
/// binder query differs: selected caller arguments consult `binders` lazily,
/// so a successful alias hit does not first clone the consumer's entire rigid
/// scope into a spelling set.
pub(crate) fn unfold_alias_frontier_for_comparison_with_binder_lookup<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    binders: &'v dyn AliasBinderLookup,
    already_canonical: bool,
) -> Option<(crate::ast::Type<P>, bool)>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
    'a: 'v,
    'm: 'v,
{
    let materialized = materialize_alias_root_state_with_ambient(
        ty,
        ctx,
        AliasAmbientProtected::Lookup(binders),
        already_canonical,
    )?;
    (!materialized.defensive_cycle_cutoff)
        .then_some((materialized.ty, materialized.identity_canonical))
}

/// [`unfold_top`] with identity-exact nominal canonicalization. Before each
/// substitution, the alias body is deeply canonicalized in its declaring
/// module with its parameters protected, while supplied arguments are
/// canonicalized in the caller's scope. Substitution therefore cannot capture
/// a caller type through a same-leaf declaration in the owner. The resulting
/// mixed tree is already identity-exact: owner references and caller arguments
/// each retain their own provenance without a side table.
pub(crate) fn unfold_and_qualify_state<P>(
    ty: &crate::ast::Type<P>,
    ctx: &AliasCtx<'_, '_, P>,
    current_is_canonical: bool,
) -> (crate::ast::Type<P>, bool)
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    let provider = NominalProvider::new(ctx.source_module, ctx.package);
    let ambient = ctx
        .binder_locals
        .map_or(AliasAmbientProtected::None, |binders| {
            AliasAmbientProtected::Lookup(binders)
        });
    unfold_and_qualify_state_with_provider(
        ty,
        ctx,
        &provider,
        provider.root(),
        ambient,
        current_is_canonical,
    )
}

#[cfg(feature = "surface")]
pub(crate) fn unfold_and_qualify_state_with_binder_lookup<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    binders: &'a dyn AliasBinderLookup,
    current_is_canonical: bool,
) -> (crate::ast::Type<P>, bool)
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
    'a: 'v,
    'm: 'v,
{
    let provider = NominalProvider::new(ctx.source_module, ctx.package);
    unfold_and_qualify_state_with_provider(
        ty,
        ctx,
        &provider,
        provider.root(),
        AliasAmbientProtected::Lookup(binders),
        current_is_canonical,
    )
}

fn unfold_and_qualify_state_with_provider<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    provider: &NominalProvider<'m, P>,
    nominal: AliasNominalScope<'m, P>,
    ambient: AliasAmbientProtected<'a>,
    current_is_canonical: bool,
) -> (crate::ast::Type<P>, bool)
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
    'a: 'v,
    'm: 'v,
{
    if let Some(materialized) = materialize_alias_root_state_with_provider(
        ty,
        ctx,
        provider,
        nominal,
        ambient,
        current_is_canonical,
    ) {
        return (materialized.ty, materialized.identity_canonical);
    }
    if current_is_canonical {
        return (ty.clone(), true);
    }
    let crate::ast::Type::Path { segments, .. } = ty else {
        return (ty.clone(), false);
    };
    if head_names_in_scope_binder_with_lookup(
        segments,
        match ambient {
            AliasAmbientProtected::None => None,
            AliasAmbientProtected::Lookup(binders) => Some(binders),
        },
    ) {
        return (ty.clone(), false);
    }
    let (segments, canonical) = provider.qualify(nominal, segments, false);
    let crate::ast::Type::Path { args, meta, .. } = ty else {
        unreachable!("a path was matched above")
    };
    // `qualify` proves only the path head. Untouched arguments remain in the
    // caller's context and prevent this result from being a whole-tree proof.
    let canonical = canonical && args.is_empty();
    (
        crate::ast::Type::Path {
            segments,
            args: args.clone(),
            meta: meta.clone(),
        },
        canonical,
    )
}

/// Normalize an alias head and make every descendant identity-exact. The
/// boolean records whether `ty` already came from an owner/caller split that
/// was canonicalized before substitution; callers propagate it through their
/// own structural recursion so an owner module path is never re-read as a
/// qualified-import alias in the consumer.
pub(crate) fn canonicalize_for_comparison<P>(
    ty: &crate::ast::Type<P>,
    ctx: &AliasCtx<'_, '_, P>,
    already_canonical: bool,
) -> (crate::ast::Type<P>, bool)
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    let provider = NominalProvider::new(ctx.source_module, ctx.package);
    let ambient = ctx
        .binder_locals
        .map_or(AliasAmbientProtected::None, |binders| {
            AliasAmbientProtected::Lookup(binders)
        });
    canonicalize_for_comparison_with_provider(
        ty,
        ctx,
        &provider,
        provider.root(),
        ambient,
        already_canonical,
    )
}

pub(crate) fn canonicalize_for_comparison_with_binder_lookup<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    binders: &'a dyn AliasBinderLookup,
    already_canonical: bool,
) -> (crate::ast::Type<P>, bool)
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
    'a: 'v,
    'm: 'v,
{
    let provider = NominalProvider::new(ctx.source_module, ctx.package);
    canonicalize_for_comparison_with_provider(
        ty,
        ctx,
        &provider,
        provider.root(),
        AliasAmbientProtected::Lookup(binders),
        already_canonical,
    )
}

fn qualify_contract_type_for_alias_ctx_with_lookup<P>(
    ty: &crate::ast::Type<P>,
    module: &crate::ast::Module<P>,
    binders: Option<&dyn AliasBinderLookup>,
) -> crate::ast::Type<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    if let Some(binders) = binders {
        crate::pass::resolve::qualify_contract_type_in_module_with_binder_lookup(
            ty, module, binders,
        )
    } else {
        crate::pass::resolve::qualify_contract_type_in_module(ty, module, &HashMap::new())
    }
}

fn canonicalize_for_comparison_with_provider<'v, 'a, 'm, P>(
    ty: &'v crate::ast::Type<P>,
    ctx: &AliasCtx<'a, 'm, P>,
    provider: &NominalProvider<'m, P>,
    nominal: AliasNominalScope<'m, P>,
    ambient: AliasAmbientProtected<'a>,
    already_canonical: bool,
) -> (crate::ast::Type<P>, bool)
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
    'a: 'v,
    'm: 'v,
{
    let (normalized, descendants_are_canonical) = unfold_and_qualify_state_with_provider(
        ty,
        ctx,
        provider,
        nominal,
        ambient,
        already_canonical,
    );
    if descendants_are_canonical {
        return (
            follow_type_reexports_deep_in_scope(&normalized, nominal, ctx.package),
            true,
        );
    }
    let Some(module) = ctx.source_module else {
        return (normalized, false);
    };
    let canonical = match &normalized {
        crate::ast::Type::Path {
            segments,
            args,
            meta,
        } => crate::ast::Type::Path {
            // `unfold_and_qualify_state` has already resolved this head. Only
            // its arguments still belong to the caller scope.
            segments: segments.clone(),
            args: args
                .iter()
                .map(|arg| {
                    let qualified = qualify_contract_type_for_alias_ctx_with_lookup(
                        arg,
                        module,
                        match ambient {
                            AliasAmbientProtected::None => None,
                            AliasAmbientProtected::Lookup(binders) => Some(binders),
                        },
                    );
                    follow_type_reexports_deep_in_scope(&qualified, nominal, ctx.package)
                })
                .collect(),
            meta: meta.clone(),
        },
        _ => {
            let qualified = qualify_contract_type_for_alias_ctx_with_lookup(
                &normalized,
                module,
                match ambient {
                    AliasAmbientProtected::None => None,
                    AliasAmbientProtected::Lookup(binders) => Some(binders),
                },
            );
            follow_type_reexports_deep_in_scope(&qualified, nominal, ctx.package)
        }
    };
    (canonical, true)
}

/// Canonicalize every transparent-alias frontier in a type tree.
///
/// Structural equality unfolds aliases one frontier at a time while it
/// descends through the two inputs. Consumers that need one stable type
/// *representation*, rather than only an equality answer, use this matching
/// traversal so an alias nested below a function, product, sum, `forall`, or
/// type argument cannot remain dependent on which equivalent input arrived
/// first.
///
/// The initial comparison canonicalization qualifies the input tree once. The
/// subsequent structural walk uses only the borrowed, head-only alias-frontier
/// probe and visits each produced node once; it never re-canonicalizes a whole
/// subtree at each descendant. An exact ambient binder lookup is queried in
/// place, while bound `forall` names extend a small lexical overlay. This
/// preserves the same nominal-versus-binder distinction as [`type_equiv_state`]
/// without enumerating an ambient rigid scope.
pub(crate) fn canonicalize_deep_for_comparison<P>(
    ty: &crate::ast::Type<P>,
    ctx: &AliasCtx<'_, '_, P>,
    already_canonical: bool,
) -> (crate::ast::Type<P>, bool)
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    canonicalize_deep_for_comparison_with_optional_binder_lookup(
        ty,
        ctx,
        ctx.binder_locals
            .map(|binders| binders as &dyn AliasBinderLookup),
        already_canonical,
    )
}

#[cfg(feature = "surface")]
pub(crate) fn canonicalize_deep_for_comparison_with_binder_lookup<P>(
    ty: &crate::ast::Type<P>,
    ctx: &AliasCtx<'_, '_, P>,
    binders: &dyn AliasBinderLookup,
    already_canonical: bool,
) -> (crate::ast::Type<P>, bool)
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    canonicalize_deep_for_comparison_with_optional_binder_lookup(
        ty,
        ctx,
        Some(binders),
        already_canonical,
    )
}

fn canonicalize_deep_for_comparison_with_optional_binder_lookup<P>(
    ty: &crate::ast::Type<P>,
    ctx: &AliasCtx<'_, '_, P>,
    ambient: Option<&dyn AliasBinderLookup>,
    already_canonical: bool,
) -> (crate::ast::Type<P>, bool)
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    #[cfg(all(test, feature = "surface"))]
    DEEP_CANONICALIZATION_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));

    fn go<P>(
        ty: &crate::ast::Type<P>,
        ctx: &AliasCtx<'_, '_, P>,
        ambient: Option<&dyn AliasBinderLookup>,
        protected: &mut HashSet<String>,
        identity_canonical: bool,
    ) -> (crate::ast::Type<P>, bool)
    where
        P: crate::pass::resolve::ExportContractPhase + Clone,
    {
        let (ty, descendants_canonical) = match ty {
            crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => (ty.clone(), true),
            crate::ast::Type::Function {
                param,
                ret,
                meta,
                abi_arity,
                caps,
            } => {
                let (param, param_canonical) =
                    go(param, ctx, ambient, protected, identity_canonical);
                let (ret, ret_canonical) = go(ret, ctx, ambient, protected, identity_canonical);
                (
                    crate::ast::Type::Function {
                        param: Box::new(param),
                        ret: Box::new(ret),
                        meta: meta.clone(),
                        abi_arity: *abi_arity,
                        caps: caps.clone(),
                    },
                    param_canonical && ret_canonical,
                )
            }
            crate::ast::Type::Product { left, right, meta } => {
                let (left, left_canonical) = go(left, ctx, ambient, protected, identity_canonical);
                let (right, right_canonical) =
                    go(right, ctx, ambient, protected, identity_canonical);
                (
                    crate::ast::Type::Product {
                        left: Box::new(left),
                        right: Box::new(right),
                        meta: meta.clone(),
                    },
                    left_canonical && right_canonical,
                )
            }
            crate::ast::Type::Sum { left, right, meta } => {
                let (left, left_canonical) = go(left, ctx, ambient, protected, identity_canonical);
                let (right, right_canonical) =
                    go(right, ctx, ambient, protected, identity_canonical);
                (
                    crate::ast::Type::Sum {
                        left: Box::new(left),
                        right: Box::new(right),
                        meta: meta.clone(),
                    },
                    left_canonical && right_canonical,
                )
            }
            crate::ast::Type::Path {
                segments,
                args,
                meta,
            } => {
                let expanded = {
                    let binders = OverlayAliasBinderLookup {
                        ambient,
                        lexical: protected,
                    };
                    match unfold_alias_frontier_for_comparison_with_binder_lookup(
                        ty,
                        ctx,
                        &binders,
                        identity_canonical,
                    ) {
                        Some((expanded, true)) => Some((expanded, true)),
                        Some((_, false)) => Some(canonicalize_for_comparison_with_binder_lookup(
                            ty,
                            ctx,
                            &binders,
                            identity_canonical,
                        )),
                        None => None,
                    }
                };
                if let Some((expanded, expanded_canonical)) = expanded {
                    match &expanded {
                        crate::ast::Type::Path {
                            segments: expanded_segments,
                            args: expanded_args,
                            meta: expanded_meta,
                        } if expanded_segments == segments => {
                            // A transparent `type A = m.A` can deliberately
                            // terminate at the same canonical head. Keep its
                            // transformed arguments without probing that
                            // already-selected terminal head again.
                            let head_identity_canonical = expanded_canonical || {
                                let binders = OverlayAliasBinderLookup {
                                    ambient,
                                    lexical: protected,
                                };
                                head_names_in_scope_binder_with_lookup(
                                    expanded_segments,
                                    Some(&binders),
                                )
                            };
                            let mut args_canonical = true;
                            let args = expanded_args
                                .iter()
                                .map(|arg| {
                                    let (arg, canonical) =
                                        go(arg, ctx, ambient, protected, expanded_canonical);
                                    args_canonical &= canonical;
                                    arg
                                })
                                .collect();
                            return (
                                crate::ast::Type::Path {
                                    segments: expanded_segments.clone(),
                                    args,
                                    meta: expanded_meta.clone(),
                                },
                                head_identity_canonical && args_canonical,
                            );
                        }
                        _ => {
                            return go(&expanded, ctx, ambient, protected, expanded_canonical);
                        }
                    }
                }
                let head_identity_canonical = identity_canonical || {
                    let binders = OverlayAliasBinderLookup {
                        ambient,
                        lexical: protected,
                    };
                    head_names_in_scope_binder_with_lookup(segments, Some(&binders))
                };
                let mut args_canonical = true;
                let args = args
                    .iter()
                    .map(|arg| {
                        let (arg, canonical) = go(arg, ctx, ambient, protected, identity_canonical);
                        args_canonical &= canonical;
                        arg
                    })
                    .collect();
                (
                    crate::ast::Type::Path {
                        segments: segments.clone(),
                        args,
                        meta: meta.clone(),
                    },
                    head_identity_canonical && args_canonical,
                )
            }
            crate::ast::Type::Goal {
                goal,
                args,
                meta,
                ext,
            } => {
                let mut args_canonical = true;
                let args = args
                    .iter()
                    .map(|arg| {
                        let (arg, canonical) = go(arg, ctx, ambient, protected, identity_canonical);
                        args_canonical &= canonical;
                        arg
                    })
                    .collect();
                (
                    crate::ast::Type::Goal {
                        goal: *goal,
                        args,
                        meta: meta.clone(),
                        ext: ext.clone(),
                    },
                    args_canonical,
                )
            }
            crate::ast::Type::Forall { param, body, meta } => {
                let inserted = protected.insert(param.name.clone());
                let (body, body_canonical) = go(body, ctx, ambient, protected, identity_canonical);
                if inserted {
                    protected.remove(&param.name);
                }
                (
                    crate::ast::Type::Forall {
                        param: param.clone(),
                        body: Box::new(body),
                        meta: meta.clone(),
                    },
                    body_canonical,
                )
            }
            crate::ast::Type::Infer { .. } => (ty.clone(), true),
            crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
        };
        (ty, descendants_canonical)
    }

    let mut protected = HashSet::new();
    let (canonical, identity_canonical) = {
        let binders = OverlayAliasBinderLookup {
            ambient,
            lexical: &protected,
        };
        canonicalize_for_comparison_with_binder_lookup(ty, ctx, &binders, already_canonical)
    };
    go(&canonical, ctx, ambient, &mut protected, identity_canonical)
}

pub fn unfold_and_qualify<P>(
    ty: &crate::ast::Type<P>,
    ctx: &AliasCtx<'_, '_, P>,
) -> crate::ast::Type<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    unfold_and_qualify_state(ty, ctx, false).0
}

/// Structural type-equivalence up to alias unfolding. Two types
/// are equivalent iff their unfolded heads share constructor and
/// every recursive position is itself equivalent. `Rec` types compare
/// by name + args (they live inside `newtype` payloads and are
/// nominal).
///
/// Phase-polymorphic with `P::TypeLabelSugar = Never`. The bound is
/// [`crate::pass::resolve::ExportContractPhase`] (which adds
/// `ItemLiteralAlias`/`ItemOp`/`ItemRecGroup` `= Never` over the
/// looser `unfold_top` bound) so the head canonicalizer can resolve a
/// bare nominal head through a module's imports — both typer input
/// phases (Lowered, Prime) satisfy it.
pub fn type_equiv<P>(
    a: &crate::ast::Type<P>,
    b: &crate::ast::Type<P>,
    ctx: &AliasCtx<'_, '_, P>,
) -> bool
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    type_equiv_state(a, b, ctx, false, false)
}

#[derive(Default)]
struct AlphaBinderIndex {
    left: HashMap<String, Vec<usize>>,
    right: HashMap<String, Vec<usize>>,
    depth: usize,
}

impl AlphaBinderIndex {
    fn push(&mut self, left: &str, right: &str) {
        let depth = self.depth;
        self.left.entry(left.to_owned()).or_default().push(depth);
        self.right.entry(right.to_owned()).or_default().push(depth);
        self.depth = self
            .depth
            .checked_add(1)
            .expect("a type-binder nesting depth fits in usize");
    }

    fn pop(&mut self, left: &str, right: &str) {
        self.depth = self
            .depth
            .checked_sub(1)
            .expect("an alpha-binder frame was pushed before it was popped");
        Self::pop_name(&mut self.left, left, self.depth);
        Self::pop_name(&mut self.right, right, self.depth);
    }

    fn pop_name(index: &mut HashMap<String, Vec<usize>>, name: &str, depth: usize) {
        let remove = {
            let levels = index
                .get_mut(name)
                .expect("an alpha-binder name was indexed before it was popped");
            assert_eq!(
                levels.pop(),
                Some(depth),
                "alpha-binder frames pop in lexical order"
            );
            levels.is_empty()
        };
        if remove {
            index.remove(name);
        }
    }

    fn left_level(&self, name: &str) -> Option<usize> {
        self.left
            .get(name)
            .and_then(|levels| levels.last())
            .copied()
    }

    fn right_level(&self, name: &str) -> Option<usize> {
        self.right
            .get(name)
            .and_then(|levels| levels.last())
            .copied()
    }
}

pub(crate) fn type_equiv_state<P>(
    a: &crate::ast::Type<P>,
    b: &crate::ast::Type<P>,
    ctx: &AliasCtx<'_, '_, P>,
    a_is_canonical: bool,
    b_is_canonical: bool,
) -> bool
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    // Raw/interner identity is a sound fast path only when both handles carry
    // the same canonical-provenance state. Equal text under mixed states may
    // denote an exact package path on one side and a contextual written path
    // on the other, so the state is part of this operation-local handle.
    if a_is_canonical == b_is_canonical && std::ptr::eq(a, b) {
        return true;
    }
    if a_is_canonical == b_is_canonical
        && let Some(interner) = ctx.type_interner
    {
        let a_handle = interner.intern(a);
        let b_handle = interner.intern(b);
        if a_handle == b_handle {
            return true;
        }
    }

    struct EquivalenceState<'a, 'm, P>
    where
        P: crate::pass::resolve::ExportContractPhase + Clone,
    {
        ctx: AliasCtx<'a, 'm, P>,
        provider: NominalProvider<'m, P>,
        alpha: AlphaBinderIndex,
        left_binders: HashSet<String>,
        right_binders: HashSet<String>,
    }

    impl<'a, 'm, P> EquivalenceState<'a, 'm, P>
    where
        P: crate::pass::resolve::ExportContractPhase + Clone,
    {
        fn compare(
            &mut self,
            left: &crate::ast::Type<P>,
            right: &crate::ast::Type<P>,
            left_canonical: bool,
            right_canonical: bool,
        ) -> bool {
            if left_canonical == right_canonical && std::ptr::eq(left, right) {
                return true;
            }
            if matches!(left, crate::ast::Type::Infer { .. })
                || matches!(right, crate::ast::Type::Infer { .. })
            {
                unreachable!(
                    "type_equiv: `Type::Infer` reached the equality check — \
                     placeholders should be resolved by the typer's substitution / \
                     expected-type rules before `type_equiv` is asked"
                );
            }

            // Equal path heads with equivalent arguments are equal whether
            // the head is nominal, a binder, or a transparent alias. Try that
            // affirmative route before any declaration lookup. If the
            // arguments differ, a non-injective alias may still erase the
            // difference, so only then fall through to alias unfolding.
            if let (
                crate::ast::Type::Path {
                    segments: left_segments,
                    args: left_args,
                    ..
                },
                crate::ast::Type::Path {
                    segments: right_segments,
                    args: right_args,
                    ..
                },
            ) = (left, right)
            {
                let left_level = match left_segments.as_slice() {
                    [name] => self.alpha.left_level(name.as_str()),
                    _ => None,
                };
                let right_level = match right_segments.as_slice() {
                    [name] => self.alpha.right_level(name.as_str()),
                    _ => None,
                };
                let heads_match = match (left_level, right_level) {
                    (Some(left), Some(right)) => left == right,
                    (None, None) => {
                        left_canonical == right_canonical && left_segments == right_segments
                    }
                    _ => false,
                };
                if heads_match
                    && left_args.len() == right_args.len()
                    && left_args
                        .iter()
                        .zip(right_args)
                        .all(|(left_arg, right_arg)| {
                            self.compare(left_arg, right_arg, left_canonical, right_canonical)
                        })
                {
                    return true;
                }
                if left_level.is_some() || right_level.is_some() {
                    return false;
                }
            }

            let left_ctx = AliasCtx {
                binder_locals: Some(&self.left_binders),
                ..self.ctx
            };
            let right_ctx = AliasCtx {
                binder_locals: Some(&self.right_binders),
                ..self.ctx
            };

            // Normalize only a path frontier that actually names an alias.
            // Structural nodes and non-alias paths stay borrowed, so a
            // depth-N spine is never cloned once for every suffix.
            if let crate::ast::Type::Path { segments, .. } = left
                && !head_names_in_scope_binder(segments, &left_ctx)
                && let Some((expanded, canonical)) =
                    unfold_alias_frontier_for_comparison_with_provider(
                        left,
                        &left_ctx,
                        &self.provider,
                        self.provider.root(),
                        left_canonical,
                    )
            {
                return self.compare(&expanded, right, canonical, right_canonical);
            }
            if let crate::ast::Type::Path { segments, .. } = right
                && !head_names_in_scope_binder(segments, &right_ctx)
                && let Some((expanded, canonical)) =
                    unfold_alias_frontier_for_comparison_with_provider(
                        right,
                        &right_ctx,
                        &self.provider,
                        self.provider.root(),
                        right_canonical,
                    )
            {
                return self.compare(left, &expanded, left_canonical, canonical);
            }

            match (left, right) {
                (crate::ast::Type::Unit { .. }, crate::ast::Type::Unit { .. }) => true,
                (crate::ast::Type::Bottom { .. }, crate::ast::Type::Bottom { .. }) => true,
                (
                    crate::ast::Type::Function {
                        param: left_param,
                        ret: left_ret,
                        ..
                    },
                    crate::ast::Type::Function {
                        param: right_param,
                        ret: right_ret,
                        ..
                    },
                ) => {
                    self.compare(left_param, right_param, left_canonical, right_canonical)
                        && self.compare(left_ret, right_ret, left_canonical, right_canonical)
                }
                (
                    crate::ast::Type::Product {
                        left: left_left,
                        right: left_right,
                        ..
                    },
                    crate::ast::Type::Product {
                        left: right_left,
                        right: right_right,
                        ..
                    },
                )
                | (
                    crate::ast::Type::Sum {
                        left: left_left,
                        right: left_right,
                        ..
                    },
                    crate::ast::Type::Sum {
                        left: right_left,
                        right: right_right,
                        ..
                    },
                ) => {
                    self.compare(left_left, right_left, left_canonical, right_canonical)
                        && self.compare(left_right, right_right, left_canonical, right_canonical)
                }
                (
                    crate::ast::Type::Path {
                        segments: left_segments,
                        args: left_args,
                        ..
                    },
                    crate::ast::Type::Path {
                        segments: right_segments,
                        args: right_args,
                        ..
                    },
                ) => {
                    let canonical_head =
                        |segments: &[crate::ast::PathSegment],
                         context: &AliasCtx<'_, '_, P>,
                         already_canonical: bool| {
                            if head_names_in_scope_binder(segments, context) {
                                segments.to_vec()
                            } else {
                                self.provider
                                    .qualify(self.provider.root(), segments, already_canonical)
                                    .0
                            }
                        };
                    let left_head = canonical_head(left_segments, &left_ctx, left_canonical);
                    let right_head = canonical_head(right_segments, &right_ctx, right_canonical);
                    left_head == right_head
                        && left_args.len() == right_args.len()
                        && left_args
                            .iter()
                            .zip(right_args)
                            .all(|(left_arg, right_arg)| {
                                self.compare(left_arg, right_arg, left_canonical, right_canonical)
                            })
                }
                (
                    crate::ast::Type::Forall {
                        param: left_param,
                        body: left_body,
                        ..
                    },
                    crate::ast::Type::Forall {
                        param: right_param,
                        body: right_body,
                        ..
                    },
                ) => {
                    if left_param.effective_kind() != right_param.effective_kind() {
                        return false;
                    }
                    self.alpha.push(&left_param.name, &right_param.name);
                    let left_inserted = self.left_binders.insert(left_param.name.clone());
                    let right_inserted = self.right_binders.insert(right_param.name.clone());
                    let equivalent =
                        self.compare(left_body, right_body, left_canonical, right_canonical);
                    if left_inserted {
                        self.left_binders.remove(&left_param.name);
                    }
                    if right_inserted {
                        self.right_binders.remove(&right_param.name);
                    }
                    self.alpha.pop(&left_param.name, &right_param.name);
                    equivalent
                }
                (crate::ast::Type::LabelSugar { ext, .. }, _) => match *ext {},
                (_, crate::ast::Type::LabelSugar { ext, .. }) => match *ext {},
                _ => false,
            }
        }
    }

    let initial_binders = binder_set(
        ctx.binder_locals
            .map(|binders| binders as &dyn AliasBinderLookup),
    );
    let provider = NominalProvider::new(ctx.source_module, ctx.package);
    let mut state = EquivalenceState {
        ctx: *ctx,
        provider,
        alpha: AlphaBinderIndex::default(),
        left_binders: initial_binders.clone(),
        right_binders: initial_binders,
    };
    state.compare(a, b, a_is_canonical, b_is_canonical)
}

/// `type_equiv` with diagnostic packaging. Returns a clear `expected …
/// found …` `Error::Type` pinned to `span` when the types differ,
/// or `Ok(())` when they don't.
pub fn require_type_equiv<P>(
    found: &crate::ast::Type<P>,
    expected: &crate::ast::Type<P>,
    span: Span,
    ctx: &AliasCtx<'_, '_, P>,
) -> Result<(), Error>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    require_type_equiv_state(found, expected, span, ctx, false, false)
}

pub(crate) fn require_type_equiv_state<P>(
    found: &crate::ast::Type<P>,
    expected: &crate::ast::Type<P>,
    span: Span,
    ctx: &AliasCtx<'_, '_, P>,
    found_is_canonical: bool,
    expected_is_canonical: bool,
) -> Result<(), Error>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    require_type_equiv_state_with_source(
        found,
        expected,
        span,
        ctx,
        found_is_canonical,
        expected_is_canonical,
        None,
    )
}

pub(crate) fn require_type_equiv_state_with_source<P>(
    found: &crate::ast::Type<P>,
    expected: &crate::ast::Type<P>,
    span: Span,
    ctx: &AliasCtx<'_, '_, P>,
    found_is_canonical: bool,
    expected_is_canonical: bool,
    requirement_source: Option<&super::intern::RequirementSource>,
) -> Result<(), Error>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    if type_equiv_state(
        found,
        expected,
        ctx,
        found_is_canonical,
        expected_is_canonical,
    ) {
        return Ok(());
    }
    let empty_bound = HashSet::new();
    let bound = ctx.binder_locals.unwrap_or(&empty_bound);
    Err(type_mismatch_error_state_with_binder_lookup(
        found,
        expected,
        span,
        ctx,
        found_is_canonical,
        expected_is_canonical,
        bound,
        requirement_source,
    ))
}

pub(crate) fn require_interned_type_equiv<P>(
    found: &super::InternedType<P>,
    expected: &super::InternedType<P>,
    span: Span,
    ctx: &AliasCtx<'_, '_, P>,
) -> Result<(), Error>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    require_type_equiv_state_with_source(
        found.as_type(),
        expected.as_type(),
        span,
        ctx,
        found.identity_is_canonical(),
        expected.identity_is_canonical(),
        expected.requirement_source().map(AsRef::as_ref),
    )
}

pub(crate) fn type_mismatch_error_state<P>(
    found: &crate::ast::Type<P>,
    expected: &crate::ast::Type<P>,
    span: Span,
    ctx: &AliasCtx<'_, '_, P>,
    found_is_canonical: bool,
    expected_is_canonical: bool,
) -> Error
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    let empty_bound = HashSet::new();
    let bound = ctx.binder_locals.unwrap_or(&empty_bound);
    type_mismatch_error_state_with_binder_lookup(
        found,
        expected,
        span,
        ctx,
        found_is_canonical,
        expected_is_canonical,
        bound,
        None,
    )
}

#[allow(clippy::too_many_arguments)] // one mismatch carries both identity flags, lexical binders, and its optional qualified requirement
pub(crate) fn type_mismatch_error_state_with_binder_lookup<P>(
    found: &crate::ast::Type<P>,
    expected: &crate::ast::Type<P>,
    span: Span,
    ctx: &AliasCtx<'_, '_, P>,
    found_is_canonical: bool,
    expected_is_canonical: bool,
    bound: &dyn AliasBinderLookup,
    requirement_source: Option<&super::intern::RequirementSource>,
) -> Error
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    let found_display = ctx.source_module.map_or_else(
        || found.clone(),
        |module| source_type_spelling_with_lookup(found, module, found_is_canonical, bound),
    );
    let expected_display = ctx.source_module.map_or_else(
        || expected.clone(),
        |module| source_type_spelling_with_lookup(expected, module, expected_is_canonical, bound),
    );
    let expected_text = display_type(&expected_display);
    let found_text = display_type(&found_display);
    let mut err = Error::type_(
        span,
        format!(
            "type mismatch: expected `{}`, found `{}`",
            expected_text, found_text,
        ),
    );
    // Point a secondary label at the source of the expected type (the
    // annotation or signature the caller is being checked against) when
    // that span is real and distinct from the mismatch site, so the user
    // sees both ends of the conflict at once (`specs/diagnostics.md`
    // § What each field is *for*).
    let exp_span = requirement_source.map_or_else(|| expected.span(), |source| source.span);
    if exp_span.start != exp_span.end && (requirement_source.is_some() || exp_span != span) {
        let label = format!(
            "expected `{}` because of this",
            display_type(&expected_display)
        );
        err = match requirement_source {
            Some(source) => err.with_secondary_in_file(source.file.clone(), exp_span, label),
            None => err.with_secondary(exp_span, label),
        };
    }
    err
}

/// View of the per-module typer state needed by
/// [`super::check_newtype_payload`] and
/// [`super::compute_variance_env`]: alias unfolding plus the
/// intra-module `newtype` lookup table. `typecheck_full` builds it
/// via `ModuleEnv::payload_ctx()`; the standalone Kio'-only walker
/// in [`crate::prime::typer`] builds an equivalent over
/// `Package<Prime>` directly.
pub struct PayloadCtx<'a, 'm, P>
where
    P: crate::ast::Phase,
{
    pub aliases: AliasCtx<'a, 'm, P>,
    pub newtypes: &'a HashMap<&'m str, &'m crate::ast::Newtype<P>>,
}

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::super::kind_scheme::CompleteScheme;
    use super::*;
    use crate::ast::{Item, Lowered, Meta, Type, TypeParam};
    use std::path::PathBuf;

    fn parse_module(src: &str) -> crate::ast::Module<Lowered> {
        let surface = crate::pass::parser::parse(src).expect("parse");
        let desugared = crate::pass::desugar::desugar_module(surface).expect("desugar");
        let (mut lowered, _) = crate::pass::label_elab::elaborate_package(
            vec![(PathBuf::from("test.kio"), desugared)],
            None,
        )
        .expect("label_elab");
        lowered.pop().expect("at least one module").1
    }

    fn span() -> Span {
        Span::new(0, 0)
    }

    fn path(name: &str) -> Type<Lowered> {
        Type::synth_path(vec![name.to_string()], Vec::new(), span())
    }

    fn qualified(alias: &str, name: &str) -> Type<Lowered> {
        Type::synth_path(
            vec![alias.to_string(), name.to_string()],
            Vec::new(),
            span(),
        )
    }

    fn unfold_top_with_original_public_bound<P>(ty: &Type<P>, ctx: &AliasCtx<'_, '_, P>) -> Type<P>
    where
        P: crate::ast::Phase<TypeLabelSugar = crate::ast::Never> + Clone,
    {
        unfold_top(ty, ctx)
    }

    #[test]
    fn unfold_top_retains_its_phase_polymorphic_public_bound() {
        let ty = path("N");
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };

        assert_eq!(unfold_top_with_original_public_bound(&ty, &ctx), ty);
    }

    fn sum(left: Type<Lowered>, right: Type<Lowered>) -> Type<Lowered> {
        Type::Sum {
            left: Box::new(left),
            right: Box::new(right),
            meta: Meta::new(span()),
        }
    }

    fn product(left: Type<Lowered>, right: Type<Lowered>) -> Type<Lowered> {
        Type::Product {
            left: Box::new(left),
            right: Box::new(right),
            meta: Meta::new(span()),
        }
    }

    fn func(param: Type<Lowered>, ret: Type<Lowered>) -> Type<Lowered> {
        Type::synth_function(vec![param], ret, span())
    }

    fn forall(binder: &str, body: Type<Lowered>) -> Type<Lowered> {
        forall_kinded(binder, None, body)
    }

    fn forall_kinded(
        binder: &str,
        kind: Option<crate::ast::Kind>,
        body: Type<Lowered>,
    ) -> Type<Lowered> {
        Type::Forall {
            param: TypeParam {
                name: binder.to_owned(),
                span: span(),
                kind,
            },
            body: Box::new(body),
            meta: Meta::new(span()),
        }
    }

    fn infer() -> Type<Lowered> {
        Type::Infer {
            meta: Meta::new(Span::new(7, 8)),
            ext: (),
        }
    }

    fn path_args(name: &str, args: Vec<Type<Lowered>>) -> Type<Lowered> {
        Type::synth_path(vec![name.to_owned()], args, span())
    }

    fn transport_alias<'a>(
        source: &'a Type<Lowered>,
        aliases: &'a HashMap<&'a str, AliasDef<'a, Lowered>>,
    ) -> AliasSourceMaterialized<Lowered> {
        let cross_module = HashMap::new();
        materialize_aliases_with_source_transport(
            source,
            &AliasCtx {
                local: aliases,
                cross_module: &cross_module,
                type_interner: None,
                source_module: None,
                package: None,
                binder_locals: None,
            },
            false,
        )
    }

    fn classify_alias<'a>(
        source: &'a Type<Lowered>,
        aliases: &'a HashMap<&'a str, AliasDef<'a, Lowered>>,
    ) -> CompleteScheme {
        let cross_module = HashMap::new();
        classify_complete_function_scheme_in_scope(
            source,
            &AliasCtx {
                local: aliases,
                cross_module: &cross_module,
                type_interner: None,
                source_module: None,
                package: None,
                binder_locals: None,
            },
            false,
        )
    }

    fn only_infer_source(transport: &AliasSourceOccurrenceTransport) -> AliasSourceOccurrenceId {
        let mut sources = transport
            .sources
            .iter()
            .enumerate()
            .filter(|(_, source)| source.is_infer)
            .map(|(index, _)| AliasSourceOccurrenceId::from_index(index));
        let source = sources.next().expect("one source infer occurrence");
        assert!(sources.next().is_none(), "only one source infer occurrence");
        source
    }

    #[test]
    fn complete_scheme_demand_uses_the_virtual_alias_frontier_without_materializing() {
        let maker_params = vec![TypeParam {
            name: "X".to_owned(),
            span: span(),
            kind: None,
        }];
        let maker_body = forall("Y", func(path("Y"), path("Y")));
        let apply_params = vec![
            TypeParam {
                name: "F".to_owned(),
                span: span(),
                kind: Some(crate::ast::Kind::arrow_chain(1)),
            },
            TypeParam {
                name: "A".to_owned(),
                span: span(),
                kind: None,
            },
        ];
        let apply_body = path_args("F", vec![path("A")]);
        let aliases = HashMap::from([
            (
                "Maker",
                AliasDef {
                    type_params: maker_params.as_slice(),
                    body: &maker_body,
                    owner_module: None,
                },
            ),
            (
                "Apply",
                AliasDef {
                    type_params: apply_params.as_slice(),
                    body: &apply_body,
                    owner_module: None,
                },
            ),
        ]);
        let source = path_args("Apply", vec![path("Maker"), path("Unit")]);

        reset_alias_materializer_work();
        assert!(classify_alias(&source, &aliases).is_complete());
        let work = alias_materializer_work();
        assert_eq!(work.materialize_calls, 0);
        assert_eq!(
            classify_alias(&path("Maker"), &aliases),
            CompleteScheme::Incomplete
        );
    }

    #[test]
    fn complete_scheme_demand_keeps_embedded_binders_ahead_of_aliases() {
        let alias_body = forall("B", func(path("B"), path("B")));
        let aliases = HashMap::from([(
            "A",
            AliasDef {
                type_params: &[],
                body: &alias_body,
                owner_module: None,
            },
        )]);
        let source = forall("A", path("A"));
        assert_eq!(
            classify_alias(&source, &aliases),
            CompleteScheme::Incomplete
        );
    }

    #[test]
    fn complete_scheme_frontier_visits_a_deep_spine_once() {
        let depth = 96;
        let unit = || Type::Unit {
            meta: Meta::new(span()),
        };
        let mut source = func(unit(), unit());
        for index in (0..depth).rev() {
            source = forall(&format!("A{index}"), source);
        }
        let aliases = HashMap::new();
        reset_alias_materializer_work();

        assert!(classify_alias(&source, &aliases).is_complete());
        let work = alias_materializer_work();
        assert_eq!(work.scheme_frontier_steps, depth + 1);
        assert_eq!(work.materialize_calls, 0);
    }

    #[test]
    fn source_transport_preserves_direct_forall_ancestry() {
        let source = forall("A", func(path("A"), infer()));
        let aliases = HashMap::new();
        let materialized = transport_alias(&source, &aliases);
        let infer = only_infer_source(&materialized.transport);

        assert_eq!(
            std::mem::size_of::<Option<AliasBinderOriginId>>(),
            std::mem::size_of::<u32>()
        );
        assert_eq!(
            std::mem::size_of::<Option<AliasSourceOccurrenceId>>(),
            std::mem::size_of::<u32>()
        );
        assert_eq!(
            std::mem::size_of::<Option<AliasOutputOccurrenceId>>(),
            std::mem::size_of::<u32>()
        );
        assert!(std::mem::size_of::<AliasSourceOccurrence>() <= 5 * std::mem::size_of::<u32>());
        assert!(std::mem::size_of::<AliasOutputOccurrence>() <= 3 * std::mem::size_of::<u32>());
        assert_eq!(materialized.transport.sources.len(), 2);
        assert_eq!(materialized.transport.binders.len(), 1);
        assert_eq!(materialized.ty, source);
        assert_eq!(
            materialized
                .transport
                .first_forall_nested_infer()
                .map(|(_, _, binder)| binder.name.as_str()),
            Some("A")
        );
        assert!(matches!(
            materialized.transport.sources[infer.index()].disposition,
            AliasSourceDisposition::RetainedDirect
        ));
        assert!(materialized.transport.outputs.is_empty());
        assert!(materialized.transport.contributions.is_empty());
        assert!(materialized.transport.root.is_none());
        assert!(matches!(
            materialized.transport.emitted_infers.as_slice(),
            [(None, source)] if *source == infer
        ));
    }

    #[test]
    fn direct_occurrences_stay_untransported_around_one_alias_branch() {
        let params = vec![TypeParam {
            name: "T".to_owned(),
            span: Span::new(2, 3),
            kind: None,
        }];
        let body = forall("Provider", path("T"));
        let aliases = HashMap::from([(
            "Wrap",
            AliasDef {
                type_params: params.as_slice(),
                body: &body,
                owner_module: None,
            },
        )]);
        let source = forall("Caller", product(infer(), path_args("Wrap", vec![infer()])));
        let materialized = transport_alias(&source, &aliases);
        let infer_sources = materialized
            .transport
            .sources
            .iter()
            .enumerate()
            .filter(|(_, source)| source.is_infer)
            .map(|(index, _)| AliasSourceOccurrenceId::from_index(index))
            .collect::<Vec<_>>();
        let [direct, through_alias] = infer_sources.as_slice() else {
            panic!("expected one direct and one alias-routed source occurrence")
        };

        assert!(matches!(
            materialized.transport.sources[direct.index()].disposition,
            AliasSourceDisposition::RetainedDirect
        ));
        assert!(matches!(
            materialized.transport.sources[through_alias.index()].disposition,
            AliasSourceDisposition::RetainedTransported
        ));
        assert_eq!(
            materialized
                .transport
                .first_enclosing_forall(*direct)
                .map(|binder| binder.name.as_str()),
            Some("Caller")
        );
        assert_eq!(
            materialized
                .transport
                .first_enclosing_forall(*through_alias)
                .map(|binder| binder.name.as_str()),
            Some("Caller")
        );
        assert!(matches!(
            materialized.transport.emitted_infers.as_slice(),
            [(None, first), (Some(_), second)] if first == direct && second == through_alias
        ));
        assert_eq!(
            materialized
                .transport
                .binders
                .iter()
                .filter(|binder| binder.name == "Caller")
                .count(),
            2
        );
        assert_eq!(
            materialized
                .transport
                .binders
                .iter()
                .filter(|binder| binder.name == "Provider")
                .count(),
            1
        );
        assert!(materialized.transport.root.is_some());
    }

    #[test]
    fn dropped_occurrence_under_a_direct_forall_has_no_binder_route() {
        let params = vec![TypeParam {
            name: "T".to_owned(),
            span: Span::new(2, 3),
            kind: None,
        }];
        let body = Type::Unit {
            meta: Meta::new(span()),
        };
        let aliases = HashMap::from([(
            "Drop",
            AliasDef {
                type_params: params.as_slice(),
                body: &body,
                owner_module: None,
            },
        )]);
        let source = forall("Caller", path_args("Drop", vec![infer()]));
        let materialized = transport_alias(&source, &aliases);
        let infer = only_infer_source(&materialized.transport);

        assert!(matches!(
            materialized.transport.sources[infer.index()].disposition,
            AliasSourceDisposition::Dropped
        ));
        assert!(materialized.transport.first_forall_nested_infer().is_none());
        assert!(materialized.transport.emitted_infers.is_empty());
        assert_eq!(materialized.transport.binders.len(), 1);
        assert!(materialized.transport.outputs.is_empty());
        assert!(materialized.transport.contributions.is_empty());
        assert!(materialized.transport.root.is_none());
    }

    #[test]
    fn source_transport_records_alias_root_graft_and_mixed_duplication() {
        let params = vec![TypeParam {
            name: "T".to_owned(),
            span: Span::new(2, 3),
            kind: None,
        }];
        let body = product(path("T"), forall("Provider", path("T")));
        let aliases = HashMap::from([(
            "Dup",
            AliasDef {
                type_params: params.as_slice(),
                body: &body,
                owner_module: None,
            },
        )]);
        let source = path_args("Dup", vec![infer()]);
        let materialized = transport_alias(&source, &aliases);
        let infer_source = only_infer_source(&materialized.transport);

        assert_eq!(
            materialized.ty,
            product(infer(), forall("Provider", infer()))
        );
        assert_eq!(
            materialized
                .transport
                .first_forall_nested_infer()
                .map(|(_, _, binder)| binder.name.as_str()),
            Some("Provider")
        );
        assert!(matches!(
            materialized.transport.sources[infer_source.index()].disposition,
            AliasSourceDisposition::RetainedTransported
        ));
        let duplicated_outputs = materialized
            .transport
            .emitted_infers
            .iter()
            .filter_map(|(output, source)| (*source == infer_source).then_some(*output).flatten())
            .collect::<Vec<_>>();
        assert_eq!(duplicated_outputs.len(), 2);
        assert!(
            duplicated_outputs
                .windows(2)
                .all(|pair| pair[0].index() < pair[1].index())
        );
        let (root_source, root_output) = materialized
            .transport
            .contributions
            .iter()
            .find_map(|event| match event {
                AliasTransportContribution::Root {
                    alias_application: Some(source),
                    body_root,
                    ..
                } => Some((*source, *body_root)),
                _ => None,
            })
            .expect("the alias root retains its exact source occurrence");
        assert!(!materialized.transport.sources[root_source.index()].is_infer);
        assert!(matches!(
            materialized.transport.sources[root_source.index()].disposition,
            AliasSourceDisposition::Dropped
        ));
        assert_eq!(
            materialized
                .transport
                .contributions
                .iter()
                .filter(|event| matches!(event, AliasTransportContribution::Graft { formal, .. } if formal == "T"))
                .count(),
            2
        );
        let grafted_outputs = materialized
            .transport
            .contributions
            .iter()
            .filter_map(|event| match event {
                AliasTransportContribution::Graft { selected, .. } => Some(*selected),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(grafted_outputs, duplicated_outputs);
        let provider_output = materialized
            .transport
            .outputs
            .iter()
            .enumerate()
            .find_map(|(index, output)| {
                output
                    .binder
                    .is_some()
                    .then(|| AliasOutputOccurrenceId::from_index(index))
            })
            .expect("one transported provider binder output");
        let continue_edges = materialized
            .transport
            .contributions
            .iter()
            .filter_map(|event| match event {
                AliasTransportContribution::Continue { edge, .. } => Some(*edge),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(continue_edges.len(), 3);
        assert!(continue_edges.contains(&AliasTransportEdge::ProductLeft));
        assert!(continue_edges.contains(&AliasTransportEdge::ProductRight));
        assert!(continue_edges.contains(&AliasTransportEdge::ForallBody));
        assert!(materialized.transport.contributions.iter().any(|event| {
            matches!(
                event,
                AliasTransportContribution::Continue {
                    parent,
                    edge: AliasTransportEdge::ProductLeft,
                    child,
                } if *parent == root_output && *child == duplicated_outputs[0]
            )
        }));
        assert!(materialized.transport.contributions.iter().any(|event| {
            matches!(
                event,
                AliasTransportContribution::Continue {
                    parent,
                    edge: AliasTransportEdge::ProductRight,
                    child,
                } if *parent == root_output && *child == provider_output
            )
        }));
        assert!(materialized.transport.contributions.iter().any(|event| {
            matches!(
                event,
                AliasTransportContribution::Continue {
                    parent,
                    edge: AliasTransportEdge::ForallBody,
                    child,
                } if *parent == provider_output && *child == duplicated_outputs[1]
            )
        }));
    }

    #[test]
    fn source_transport_switches_to_the_exact_provider_file_and_grafts_to_the_caller() {
        let provider = parse_module("module provider; pub type Wrap[T] = [B] T -> B;");
        let caller = parse_module("module caller; import provider(Wrap);");
        let decoy = parse_module("module decoy; pub type Wrap[T] = T;");
        let provider_scope = crate::pass::resolve::TopLevelScope::build(&provider)
            .expect("provider declaration scope");
        let caller_scope =
            crate::pass::resolve::TopLevelScope::build(&caller).expect("caller declaration scope");
        let decoy_scope =
            crate::pass::resolve::TopLevelScope::build(&decoy).expect("decoy declaration scope");
        let package = crate::pass::resolve::Package::from_parts(
            std::collections::BTreeMap::from([
                (
                    "provider".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("provider.kio"),
                        module: provider,
                        scope: provider_scope,
                    },
                ),
                (
                    "caller".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("caller.kio"),
                        module: caller,
                        scope: caller_scope,
                    },
                ),
                (
                    "decoy".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("decoy.kio"),
                        module: decoy,
                        scope: decoy_scope,
                    },
                ),
            ]),
            None,
        );
        let caller = &package.module("caller").expect("caller entry").module;
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let source = path_args("Wrap", vec![infer()]);
        let materialized = materialize_aliases_with_source_transport(
            &source,
            &AliasCtx {
                local: &local,
                cross_module: &cross_module,
                type_interner: None,
                source_module: Some(caller),
                package: Some(&package),
                binder_locals: None,
            },
            false,
        );
        let binder = materialized
            .transport
            .first_forall_nested_infer()
            .map(|(_, _, binder)| binder)
            .expect("the provider forall encloses the caller hole");

        assert_eq!(binder.name, "B");
        assert_eq!(
            binder.provider_file.as_deref(),
            Some(std::path::Path::new("provider.kio"))
        );
        assert!(materialized.transport.contributions.iter().any(|event| {
            matches!(
                event,
                AliasTransportContribution::Root {
                    provider_file: Some(file),
                    ..
                } if file == std::path::Path::new("provider.kio")
            )
        }));
        assert!(materialized.transport.contributions.iter().any(|event| {
            matches!(
                event,
                AliasTransportContribution::Graft {
                    provider_file: Some(file),
                    ..
                } if file == std::path::Path::new("caller.kio")
            )
        }));
    }

    #[test]
    fn source_transport_reorders_one_occurrence_and_finalizes_a_drop() {
        let params = vec![
            TypeParam {
                name: "First".to_owned(),
                span: span(),
                kind: None,
            },
            TypeParam {
                name: "Second".to_owned(),
                span: span(),
                kind: None,
            },
        ];
        let reorder_body = product(path("Second"), path("First"));
        let keep_body = path("First");
        let unit = Type::Unit {
            meta: Meta::new(span()),
        };

        let reorder_aliases = HashMap::from([(
            "Reorder",
            AliasDef {
                type_params: params.as_slice(),
                body: &reorder_body,
                owner_module: None,
            },
        )]);
        let reorder_source = path_args("Reorder", vec![unit.clone(), infer()]);
        let reordered = transport_alias(&reorder_source, &reorder_aliases);
        let reorder_infer = only_infer_source(&reordered.transport);
        assert_eq!(reordered.ty, product(infer(), unit.clone()));
        assert!(matches!(
            reordered.transport.sources[reorder_infer.index()].disposition,
            AliasSourceDisposition::RetainedTransported
        ));
        assert!(matches!(
            reordered.transport.contributions.as_slice(),
            [
                AliasTransportContribution::Graft { .. },
                AliasTransportContribution::Continue {
                    edge: AliasTransportEdge::ProductLeft,
                    ..
                },
                AliasTransportContribution::Root { .. },
            ]
        ));

        let keep_aliases = HashMap::from([(
            "Keep",
            AliasDef {
                type_params: params.as_slice(),
                body: &keep_body,
                owner_module: None,
            },
        )]);
        let keep_source = path_args("Keep", vec![unit.clone(), infer()]);
        let kept = transport_alias(&keep_source, &keep_aliases);
        let dropped_infer = only_infer_source(&kept.transport);
        assert_eq!(kept.ty, unit);
        assert!(matches!(
            kept.transport.sources[dropped_infer.index()].disposition,
            AliasSourceDisposition::Dropped
        ));
        assert!(kept.transport.outputs.is_empty());
    }

    #[test]
    fn ordinary_materialization_policy_has_zero_transport_storage() {
        assert_eq!(std::mem::size_of::<NoAliasTransport>(), 0);
        assert_eq!(
            std::mem::size_of::<<NoAliasTransport as AliasTransportSink<Lowered>>::Node>(),
            0
        );
        assert_eq!(
            std::mem::size_of::<<NoAliasTransport as AliasTransportSink<Lowered>>::Children>(),
            0
        );
    }

    /// `[A] A -> A` and `[B] B -> B` are alpha-equivalent quantified
    /// types. With no interner in the context the fast identity path is
    /// skipped, so the only route to `true` is the `Forall` arm's exact
    /// lexical-level comparison; a genuinely different body and an outer
    /// reference hidden by a same-spelled inner binder must still be rejected.
    #[test]
    fn type_equiv_alpha_renames_forall_binders() {
        let local: HashMap<&str, AliasDef<Lowered>> = HashMap::new();
        let cross_module: HashMap<&str, AliasDef<Lowered>> = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };
        let lhs = forall("A", func(path("A"), path("A")));
        let rhs = forall("B", func(path("B"), path("B")));
        assert!(type_equiv(&lhs, &rhs, &ctx));

        let different = forall(
            "B",
            func(
                path("B"),
                Type::Unit {
                    meta: Meta::new(span()),
                },
            ),
        );
        assert!(!type_equiv(&lhs, &different, &ctx));

        let shadowed_lhs = forall("A", forall("A", path("A")));
        let shadowed_rhs = forall("X", forall("Y", path("Y")));
        let outer_rhs = forall("X", forall("Y", path("X")));
        assert!(type_equiv(&shadowed_lhs, &shadowed_rhs, &ctx));
        assert!(!type_equiv(&shadowed_lhs, &outer_rhs, &ctx));
    }

    #[test]
    fn type_equiv_alpha_renaming_requires_equal_effective_forall_kinds() {
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };
        let star = crate::ast::Kind::Star;
        let constructor = crate::ast::Kind::arrow_chain(1);
        let unit = || Type::Unit {
            meta: Meta::new(span()),
        };

        let used_left = forall_kinded("A", None, func(path("A"), path("A")));
        let used_right = forall_kinded("B", Some(star.clone()), func(path("B"), path("B")));
        assert!(type_equiv(&used_left, &used_right, &ctx));

        let unused_implicit = forall_kinded("A", None, func(unit(), unit()));
        let unused_explicit = forall_kinded("B", Some(star), func(unit(), unit()));
        assert!(type_equiv(&unused_implicit, &unused_explicit, &ctx));

        let unused_constructor =
            forall_kinded("F", Some(constructor.clone()), func(unit(), unit()));
        assert!(!type_equiv(&unused_implicit, &unused_constructor, &ctx));
        assert!(!type_equiv(&unused_constructor, &unused_implicit, &ctx));

        let nested_star = forall(
            "Outer",
            forall_kinded("A", None, func(path("Outer"), path("Outer"))),
        );
        let nested_constructor = forall(
            "Renamed",
            forall_kinded(
                "F",
                Some(constructor),
                func(path("Renamed"), path("Renamed")),
            ),
        );
        assert!(!type_equiv(&nested_star, &nested_constructor, &ctx));
    }

    #[test]
    fn mixed_canonical_flags_preserve_alpha_forall_binder_proofs() {
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };
        let left = forall("A", func(path("A"), path("A")));
        let right = forall("B", func(path("B"), path("B")));
        let left_product = product(
            left.clone(),
            Type::Unit {
                meta: Meta::new(span()),
            },
        );
        let right_product = product(
            right.clone(),
            Type::Unit {
                meta: Meta::new(span()),
            },
        );

        assert!(
            type_equiv_state(&left, &right, &ctx, true, false),
            "matching lexical binder levels must establish alpha-equivalence independently of nominal canonical provenance"
        );
        assert!(
            type_equiv_state(&left_product, &right_product, &ctx, true, false),
            "an ordinary structural parent must preserve the exact alpha-binder proof under mixed canonical provenance"
        );
    }

    #[test]
    fn equal_alias_spines_do_not_probe_or_materialize_declarations() {
        const DEPTH: usize = 512;
        let alias_body = Type::Unit {
            meta: Meta::new(span()),
        };
        let local = HashMap::from([(
            "A",
            AliasDef {
                type_params: &[],
                body: &alias_body,
                owner_module: None,
            },
        )]);
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };
        let spine = || {
            (0..DEPTH).fold(path("A"), |tail, _| {
                product(
                    Type::Unit {
                        meta: Meta::new(span()),
                    },
                    tail,
                )
            })
        };
        let left = spine();
        let right = spine();

        reset_alias_materializer_work();
        assert!(type_equiv(&left, &right, &ctx));
        let work = alias_materializer_work();
        assert_eq!(work.declaration_head_probes, 0);
        assert_eq!(work.materializer_roots, 0);
    }

    #[test]
    #[should_panic(expected = "`Type::Infer` reached the equality check")]
    fn nested_infer_is_an_invariant_failure_not_false_inequality() {
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };
        let left = product(
            Type::Unit {
                meta: Meta::new(span()),
            },
            Type::Infer {
                meta: Meta::new(span()),
                ext: (),
            },
        );
        let right = product(
            Type::Unit {
                meta: Meta::new(span()),
            },
            path("A"),
        );

        let _ = type_equiv(&left, &right, &ctx);
    }

    #[test]
    fn finite_repeated_transparent_alias_occurrences_reach_the_terminal_head() {
        let params = [TypeParam {
            name: "A".to_owned(),
            span: span(),
            kind: None,
        }];
        let body = path("A");
        let local = HashMap::from([(
            "Id",
            AliasDef {
                type_params: &params,
                body: &body,
                owner_module: None,
            },
        )]);
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };
        let terminal = path("N");
        let once = Type::synth_path(vec!["Id".to_owned()], vec![terminal.clone()], span());
        let twice = Type::synth_path(vec!["Id".to_owned()], vec![once], span());

        assert_eq!(unfold_top(&twice, &ctx), terminal);
        assert_eq!(unfold_and_qualify_state(&twice, &ctx, false).0, terminal);
        assert!(type_equiv(&twice, &terminal, &ctx));
    }

    #[test]
    fn malformed_recursive_alias_cutoff_is_not_reopened_by_type_equivalence() {
        let terminal = Type::Unit {
            meta: Meta::new(span()),
        };
        let body = product(path("A"), terminal.clone());
        let local = HashMap::from([(
            "A",
            AliasDef {
                type_params: &[],
                body: &body,
                owner_module: None,
            },
        )]);
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };
        let alias = path("A");

        assert_eq!(
            unfold_top(&alias, &ctx),
            body,
            "the one-shot public unfold retains its bounded partial expansion"
        );
        assert!(
            unfold_alias_frontier_for_comparison(&alias, &ctx, false).is_none(),
            "a structural consumer must not reopen a malformed cutoff in a fresh cursor"
        );
        assert!(
            !type_equiv(&alias, &terminal, &ctx),
            "malformed recursive aliases terminate as opaque heads rather than overflowing"
        );
    }

    #[test]
    fn repeated_formal_uses_of_one_higher_kinded_alias_view_all_expand() {
        let unary_params = [TypeParam {
            name: "E".to_owned(),
            span: span(),
            kind: None,
        }];
        let unary_body = Type::synth_path(vec!["Either".to_owned()], vec![path("E")], span());
        let twice_params = [
            TypeParam {
                name: "F".to_owned(),
                span: span(),
                kind: Some(crate::ast::Kind::arrow_chain(1)),
            },
            TypeParam {
                name: "A".to_owned(),
                span: span(),
                kind: None,
            },
        ];
        let twice_body = Type::synth_path(
            vec!["F".to_owned()],
            vec![Type::synth_path(
                vec!["F".to_owned()],
                vec![path("A")],
                span(),
            )],
            span(),
        );
        let local = HashMap::from([
            (
                "Unary",
                AliasDef {
                    type_params: &unary_params,
                    body: &unary_body,
                    owner_module: None,
                },
            ),
            (
                "Twice",
                AliasDef {
                    type_params: &twice_params,
                    body: &twice_body,
                    owner_module: None,
                },
            ),
        ]);
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };
        let string = path("String");
        let terminal = path("N");
        let unary_string = Type::synth_path(vec!["Unary".to_owned()], vec![string.clone()], span());
        let source = Type::synth_path(
            vec!["Twice".to_owned()],
            vec![unary_string, terminal.clone()],
            span(),
        );
        let inner = Type::synth_path(
            vec!["Either".to_owned()],
            vec![string.clone(), terminal],
            span(),
        );
        let expected = Type::synth_path(vec!["Either".to_owned()], vec![string, inner], span());

        assert_eq!(unfold_top(&source, &ctx), expected);
        assert_eq!(unfold_and_qualify_state(&source, &ctx, false).0, expected);
        assert!(type_equiv(&source, &expected, &ctx));
    }

    #[test]
    fn selected_formal_applications_complete_only_missing_alias_arguments() {
        let maker_params = [TypeParam {
            name: "A".to_owned(),
            span: span(),
            kind: None,
        }];
        let maker_body = path("A");
        let maker2_params = [
            TypeParam {
                name: "A".to_owned(),
                span: span(),
                kind: None,
            },
            TypeParam {
                name: "B".to_owned(),
                span: span(),
                kind: None,
            },
        ];
        let maker2_body = product(path("A"), path("B"));
        let apply_params = [
            TypeParam {
                name: "F".to_owned(),
                span: span(),
                kind: Some(crate::ast::Kind::arrow_chain(1)),
            },
            TypeParam {
                name: "A".to_owned(),
                span: span(),
                kind: None,
            },
        ];
        let apply_body = Type::synth_path(vec!["F".to_owned()], vec![path("A")], span());
        let local = HashMap::from([
            (
                "Maker",
                AliasDef {
                    type_params: &maker_params,
                    body: &maker_body,
                    owner_module: None,
                },
            ),
            (
                "Maker2",
                AliasDef {
                    type_params: &maker2_params,
                    body: &maker2_body,
                    owner_module: None,
                },
            ),
            (
                "Apply",
                AliasDef {
                    type_params: &apply_params,
                    body: &apply_body,
                    owner_module: None,
                },
            ),
        ]);
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };
        let terminal = path("N");
        let apply_missing = Type::synth_path(
            vec!["Apply".to_owned()],
            vec![path("Maker"), terminal.clone()],
            span(),
        );
        assert_eq!(unfold_top(&apply_missing, &ctx), terminal);

        let string = path("String");
        let partially_applied =
            Type::synth_path(vec!["Maker2".to_owned()], vec![string.clone()], span());
        let apply_prefix = Type::synth_path(
            vec!["Apply".to_owned()],
            vec![partially_applied, path("N")],
            span(),
        );
        assert_eq!(unfold_top(&apply_prefix, &ctx), product(string, path("N")));

        let direct_overapplication = Type::synth_path(
            vec!["Maker".to_owned()],
            vec![path("N"), path("Extra")],
            span(),
        );
        assert_eq!(
            unfold_top(&direct_overapplication, &ctx),
            direct_overapplication,
            "written excess alias arguments must not be reclassified as result application"
        );
    }

    #[test]
    fn forwarded_argument_rope_is_linear_in_chain_plus_arity() {
        const DEPTH: usize = 512;
        const ARITY: usize = 96;

        let pass_params = [TypeParam {
            name: "F".to_owned(),
            span: span(),
            kind: None,
        }];
        let pass_body = path("F");
        let mut apply_params = Vec::with_capacity(ARITY + 1);
        apply_params.push(TypeParam {
            name: "F".to_owned(),
            span: span(),
            kind: None,
        });
        apply_params.extend((0..ARITY).map(|index| TypeParam {
            name: format!("A{index}"),
            span: span(),
            kind: None,
        }));
        let apply_body = Type::synth_path(
            vec!["F".to_owned()],
            (0..ARITY).map(|index| path(&format!("A{index}"))).collect(),
            span(),
        );
        let local = HashMap::from([
            (
                "Pass",
                AliasDef {
                    type_params: &pass_params,
                    body: &pass_body,
                    owner_module: None,
                },
            ),
            (
                "Apply",
                AliasDef {
                    type_params: &apply_params,
                    body: &apply_body,
                    owner_module: None,
                },
            ),
        ]);
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };

        let head = (0..DEPTH).fold(path("Base"), |head, _| {
            Type::synth_path(vec!["Pass".to_owned()], vec![head], span())
        });
        let arguments = (0..ARITY)
            .map(|index| path(&format!("T{index}")))
            .collect::<Vec<_>>();
        let mut source_args = Vec::with_capacity(ARITY + 1);
        source_args.push(head);
        source_args.extend(arguments.iter().cloned());
        let source = Type::synth_path(vec!["Apply".to_owned()], source_args, span());
        let expected = Type::synth_path(vec!["Base".to_owned()], arguments, span());

        reset_alias_materializer_work();
        assert_eq!(unfold_top(&source, &ctx), expected);
        let work = alias_materializer_work();
        assert!(
            work.arg_rope_value_visits <= ARITY * 2,
            "forwarded arguments were flattened once per alias: {work:?}"
        );
        assert!(
            work.arg_rope_nodes <= (DEPTH + ARITY) * 8,
            "argument-rope storage exceeded the source-plus-output bound: {work:?}"
        );
        assert!(
            work.materialize_calls <= (DEPTH + ARITY) * 8,
            "alias materialization revisited the forwarded argument cross-product: {work:?}"
        );
        assert!(
            work.path_redirects >= DEPTH && work.path_redirects <= (DEPTH + ARITY) * 4,
            "the path-only alias chain did not use the bounded trampoline: {work:?}"
        );
    }

    #[test]
    fn deep_forwarded_argument_rope_measure_splits_without_rust_recursion() {
        const DEPTH: usize = 20_000;

        let pass_params = [TypeParam {
            name: "F".to_owned(),
            span: span(),
            kind: None,
        }];
        let pass_body = Type::synth_path(vec!["F".to_owned()], vec![path("Forwarded")], span());
        let apply_params = [
            TypeParam {
                name: "F".to_owned(),
                span: span(),
                kind: None,
            },
            TypeParam {
                name: "Last".to_owned(),
                span: span(),
                kind: None,
            },
        ];
        let apply_body = Type::synth_path(vec!["F".to_owned()], vec![path("Last")], span());
        let consume_params = (0..DEPTH)
            .map(|index| TypeParam {
                name: format!("A{index}"),
                span: span(),
                kind: None,
            })
            .collect::<Vec<_>>();
        let consume_body = path("Tail");
        let local = HashMap::from([
            (
                "Pass",
                AliasDef {
                    type_params: &pass_params,
                    body: &pass_body,
                    owner_module: None,
                },
            ),
            (
                "Apply",
                AliasDef {
                    type_params: &apply_params,
                    body: &apply_body,
                    owner_module: None,
                },
            ),
            (
                "Consume",
                AliasDef {
                    type_params: &consume_params,
                    body: &consume_body,
                    owner_module: None,
                },
            ),
        ]);
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };
        let head = (0..DEPTH).fold(path("Consume"), |head, _| {
            Type::synth_path(vec!["Pass".to_owned()], vec![head], span())
        });
        let source = Type::synth_path(
            vec!["Apply".to_owned()],
            vec![head, path("Terminal")],
            span(),
        );

        let provider = NominalProvider::new(None, None);
        let (mut materializer, root) = AliasMaterializer::for_caller(
            &source,
            ctx,
            provider.root(),
            &provider,
            AliasAmbientProtected::None,
            false,
        );
        assert_eq!(
            materializer.materialized_node_measure(root),
            Some(2),
            "Consume removes every forwarded argument except Tail(Terminal)"
        );
        drop(materializer);
        // The public contract concerns evaluator traversal, not Rust's
        // recursive drop glue for an adversarially hand-built AST.
        std::mem::forget(source);
    }

    #[test]
    fn ambient_binder_scope_is_borrowed_by_each_alias_frontier() {
        const BINDERS: usize = 512;

        let params = [TypeParam {
            name: "T".to_owned(),
            span: span(),
            kind: None,
        }];
        let identity_body = path("T");
        let decoy_body = Type::Unit {
            meta: Meta::new(span()),
        };
        let local = HashMap::from([
            (
                "Identity",
                AliasDef {
                    type_params: &params,
                    body: &identity_body,
                    owner_module: None,
                },
            ),
            (
                "B0",
                AliasDef {
                    type_params: &[],
                    body: &decoy_body,
                    owner_module: None,
                },
            ),
        ]);
        let cross_module = HashMap::new();
        let binders = (0..BINDERS)
            .map(|index| format!("B{index}"))
            .collect::<HashSet<_>>();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: Some(&binders),
        };
        let source = Type::synth_path(vec!["Identity".to_owned()], vec![path("B0")], span());

        reset_alias_materializer_work();
        assert_eq!(unfold_top(&source, &ctx), path("B0"));
        assert_eq!(
            alias_materializer_work().ambient_binder_copies,
            0,
            "an alias frontier must borrow the already-built ambient binder set instead of copying its full depth into a second persistent index"
        );
    }

    #[test]
    fn capture_avoiding_alias_materialization_is_linear_in_unique_output() {
        const DEPTH: usize = 128;

        let params = [TypeParam {
            name: "A".to_owned(),
            span: span(),
            kind: None,
        }];
        let body = (0..DEPTH).fold(path("A"), |body, _| forall("X", body));
        let local = HashMap::from([(
            "Wrap",
            AliasDef {
                type_params: &params,
                body: &body,
                owner_module: None,
            },
        )]);
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };
        let actual = (2..=DEPTH).fold(path("X"), |actual, suffix| {
            product(actual, path(&format!("X_n{suffix}")))
        });
        let source = Type::synth_path(vec!["Wrap".to_owned()], vec![actual], span());

        reset_alias_materializer_work();
        let materialized = unfold_top(&source, &ctx);
        let work = alias_materializer_work();

        let mut emitted = HashSet::new();
        let mut cursor = &materialized;
        for _ in 0..DEPTH {
            let Type::Forall { param, body, .. } = cursor else {
                panic!("capture avoidance lost a structural forall: {cursor:?}");
            };
            assert!(emitted.insert(param.name.clone()));
            assert!(param.name != "X");
            cursor = body;
        }
        assert!(matches!(cursor, Type::Product { .. }));
        assert_eq!(work.formal_summary_builds, 1);
        assert!(
            work.alpha_candidate_checks <= DEPTH * 2,
            "alpha suffix selection rescanned prior collisions: {work:?}"
        );
        assert!(
            work.materialize_calls <= DEPTH * 6,
            "unique alias output was rematerialized by depth: {work:?}"
        );
    }

    #[test]
    fn forwarded_formal_free_name_summary_is_shared_across_owner_chain() {
        const DEPTH: usize = 64;
        const SUMMARY_DEPTH: usize = 64;
        const WIDTH: usize = 64;

        let names = (0..DEPTH)
            .map(|index| format!("Wrap{index}"))
            .collect::<Vec<_>>();
        let params = (0..DEPTH)
            .map(|_| {
                [TypeParam {
                    name: "A".to_owned(),
                    span: span(),
                    kind: None,
                }]
            })
            .collect::<Vec<_>>();
        let bodies = (0..DEPTH)
            .map(|index| {
                let tail = if index + 1 == DEPTH {
                    path("A")
                } else {
                    Type::synth_path(vec![names[index + 1].clone()], vec![path("A")], span())
                };
                forall("X", tail)
            })
            .collect::<Vec<_>>();
        let local = (0..DEPTH)
            .map(|index| {
                (
                    names[index].as_str(),
                    AliasDef {
                        type_params: &params[index],
                        body: &bodies[index],
                        owner_module: None,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };
        let free_actual = (2..=WIDTH).fold(path("X"), |actual, suffix| {
            product(actual, path(&format!("X_n{suffix}")))
        });
        let actual = (0..SUMMARY_DEPTH).fold(free_actual, |body, index| {
            forall(&format!("Bound{index}"), body)
        });
        let source = Type::synth_path(vec![names[0].clone()], vec![actual], span());

        reset_alias_materializer_work();
        let materialized = unfold_top(&source, &ctx);
        let work = alias_materializer_work();

        let mut emitted = HashSet::new();
        let mut cursor = &materialized;
        for _ in 0..DEPTH {
            let Type::Forall { param, body, .. } = cursor else {
                panic!("owner-chain capture avoidance lost a forall: {cursor:?}");
            };
            assert_ne!(param.name, "X");
            assert!(emitted.insert(param.name.clone()));
            cursor = body;
        }
        for _ in 0..SUMMARY_DEPTH {
            let Type::Forall { body, .. } = cursor else {
                panic!("the selected caller view lost a nested forall: {cursor:?}");
            };
            cursor = body;
        }
        assert!(matches!(cursor, Type::Product { .. }));
        let linear_size = DEPTH + SUMMARY_DEPTH + WIDTH;
        assert!(
            work.materialize_calls <= linear_size * 20,
            "each owner rematerialized the same selected caller view: {work:?}"
        );
        assert!(
            work.free_name_summary_type_visits <= linear_size * 20,
            "the virtual free-name summary revisited the owner/actual cross-product: {work:?}"
        );
        assert!(
            work.free_name_summary_builds <= DEPTH + 1
                && work.free_name_summary_retained_names <= WIDTH * 2,
            "forwarded views retained duplicate free-name summaries: {work:?}"
        );
        assert!(
            work.alpha_candidate_checks <= linear_size * 2,
            "each owner restarted alpha-suffix selection from the first collision: {work:?}"
        );
    }

    #[test]
    fn canonical_non_alias_frontier_retains_the_borrowed_spine() {
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };
        let ty = (0..64).fold(path("Leaf"), |arg, _| {
            Type::synth_path(vec!["m".to_owned(), "Box".to_owned()], vec![arg], span())
        });

        assert!(
            unfold_alias_frontier_for_comparison(&ty, &ctx, true).is_none(),
            "a canonical nominal frontier must stay borrowed when no alias fires"
        );
    }

    #[test]
    fn canonical_nominal_leaf_skips_the_materializer_arena() {
        let ty: Type<Lowered> = Type::synth_path(
            vec!["owner".to_owned(), "Leaf".to_owned()],
            Vec::new(),
            span(),
        );

        reset_alias_materializer_work();
        assert_eq!(
            materialize_aliases_in_scope(&ty, None, None, None, true),
            ty
        );
        assert_eq!(alias_materializer_work().materializer_roots, 0);
    }

    #[test]
    fn canonical_non_alias_composite_skips_the_materializer_arena() {
        let module = parse_module("module owner; pub host type Leaf;");
        let scope =
            crate::pass::resolve::TopLevelScope::build(&module).expect("declaration-only scope");
        let package = crate::pass::resolve::Package::from_parts(
            std::collections::BTreeMap::from([(
                "owner".to_owned(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from("owner.kio"),
                    module,
                    scope,
                },
            )]),
            None,
        );
        let owner = &package.module("owner").expect("owner module").module;
        let leaf = || {
            Type::synth_path(
                vec!["owner".to_owned(), "Leaf".to_owned()],
                Vec::new(),
                span(),
            )
        };
        let ty = (1..64).fold(leaf(), |left, _| product(left, leaf()));

        reset_alias_materializer_work();
        assert_eq!(
            materialize_aliases_in_scope(&ty, Some(owner), Some(&package), None, true),
            ty
        );
        assert_eq!(
            (
                alias_materializer_work().materializer_roots,
                alias_materializer_work().arg_rope_walks,
            ),
            (0, 0),
            "a canonical alias-free composite must retain the no-materializer/no-rope-walk fast path"
        );
    }

    #[test]
    fn nominal_free_structural_equivalence_does_not_capture_a_nominal_provider() {
        let module = parse_module("module owner; pub host type Leaf; pub host type A;");
        let scope =
            crate::pass::resolve::TopLevelScope::build(&module).expect("declaration-only scope");
        let package = crate::pass::resolve::Package::from_parts(
            std::collections::BTreeMap::from([(
                "owner".to_owned(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from("owner.kio"),
                    module,
                    scope,
                },
            )]),
            None,
        );
        let owner = &package.module("owner").expect("owner module").module;
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(owner),
            package: Some(&package),
            binder_locals: None,
        };
        let structural = || {
            product(
                Type::Unit {
                    meta: Meta::new(span()),
                },
                Type::Bottom {
                    meta: Meta::new(span()),
                },
            )
        };
        let left = structural();
        let right = structural();

        crate::pass::resolve::reset_nominal_provider_work();
        assert!(type_equiv_state(&left, &right, &ctx, false, false));
        assert_eq!(
            crate::pass::resolve::nominal_provider_work(),
            crate::pass::resolve::NominalProviderWork::default(),
            "a nominal-free structural comparison must not capture or query a nominal scope"
        );

        let binders = HashSet::from(["A".to_owned()]);
        let binder_ctx = AliasCtx {
            binder_locals: Some(&binders),
            ..ctx
        };
        let left_binder = path("A");
        let right_binder = left_binder.clone();
        crate::pass::resolve::reset_nominal_provider_work();
        assert!(type_equiv_state(
            &left_binder,
            &right_binder,
            &binder_ctx,
            true,
            false,
        ));
        assert_eq!(
            crate::pass::resolve::nominal_provider_work(),
            crate::pass::resolve::NominalProviderWork::default(),
            "an exact ambient binder is path-shaped but requires no nominal-scope capture"
        );
    }

    #[test]
    fn canonical_parameterized_nominal_skips_the_materializer_arena() {
        let module = parse_module("module owner; pub host type Leaf; pub host type Box[A];");
        let scope =
            crate::pass::resolve::TopLevelScope::build(&module).expect("declaration-only scope");
        let package = crate::pass::resolve::Package::from_parts(
            std::collections::BTreeMap::from([(
                "owner".to_owned(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from("owner.kio"),
                    module,
                    scope,
                },
            )]),
            None,
        );
        let owner = &package.module("owner").expect("owner module").module;
        let leaf = Type::synth_path(
            vec!["owner".to_owned(), "Leaf".to_owned()],
            Vec::new(),
            span(),
        );
        let ty = Type::synth_path(
            vec!["owner".to_owned(), "Box".to_owned()],
            vec![leaf],
            span(),
        );

        reset_alias_materializer_work();
        assert_eq!(
            materialize_aliases_in_scope(&ty, Some(owner), Some(&package), None, true),
            ty
        );
        assert_eq!(
            alias_materializer_work().materializer_roots,
            0,
            "ordinary canonical arguments must remain stack-local when no alias fires"
        );
    }

    #[test]
    fn canonical_nominal_leaf_skips_the_source_alias_index() {
        const ALIASES: usize = 128;
        let mut source = String::from("module owner; pub host type Leaf;");
        for index in 0..ALIASES {
            source.push_str(&format!(" type A{index} = .;"));
        }
        let module = parse_module(&source);
        let scope =
            crate::pass::resolve::TopLevelScope::build(&module).expect("declaration-only scope");
        let package = crate::pass::resolve::Package::from_parts(
            std::collections::BTreeMap::from([(
                "owner".to_owned(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from("owner.kio"),
                    module,
                    scope,
                },
            )]),
            None,
        );
        let owner = &package.module("owner").expect("owner module").module;
        let ty: Type<Lowered> = Type::synth_path(
            vec!["owner".to_owned(), "Leaf".to_owned()],
            Vec::new(),
            span(),
        );

        reset_alias_materializer_work();
        assert_eq!(
            materialize_aliases_in_scope(&ty, Some(owner), Some(&package), None, true),
            ty
        );
        let work = alias_materializer_work();
        assert_eq!(work.materializer_roots, 0);
        assert_eq!(
            (
                work.source_alias_index_builds,
                work.source_alias_items_scanned
            ),
            (0, 0),
            "an exact canonical nominal must not scan or index unrelated source aliases"
        );
    }

    #[test]
    fn canonical_composite_uses_exact_package_alias_without_source_index() {
        const UNRELATED_ALIASES: usize = 128;
        let mut source = String::from("module owner; pub host type Leaf; pub type Alias = Leaf;");
        for index in 0..UNRELATED_ALIASES {
            source.push_str(&format!(" type A{index} = .;"));
        }
        let module = parse_module(&source);
        let scope =
            crate::pass::resolve::TopLevelScope::build(&module).expect("declaration-only scope");
        let package = crate::pass::resolve::Package::from_parts(
            std::collections::BTreeMap::from([(
                "owner".to_owned(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from("owner.kio"),
                    module,
                    scope,
                },
            )]),
            None,
        );
        let owner = &package.module("owner").expect("owner module").module;
        let nominal = |name: &str| {
            Type::synth_path(
                vec!["owner".to_owned(), name.to_owned()],
                Vec::new(),
                span(),
            )
        };
        let leaf = nominal("Leaf");
        let ty = product(nominal("Alias"), leaf.clone());
        let expected = product(leaf.clone(), leaf);

        reset_alias_materializer_work();
        assert_eq!(
            materialize_aliases_in_scope(&ty, Some(owner), Some(&package), None, true),
            expected
        );
        let work = alias_materializer_work();
        assert_eq!(work.materializer_roots, 1, "the alias must actually fire");
        assert_eq!(
            (
                work.source_alias_index_builds,
                work.source_alias_items_scanned
            ),
            (0, 0),
            "canonical package identities must resolve through the package scope, not a rebuilt source alias map"
        );
    }

    #[test]
    fn canonical_alias_output_uses_package_scope_without_rescanning_owner_declarations() {
        const UNRELATED_DECLARATIONS: usize = 64;
        const UNRELATED_IMPORTS: usize = 64;
        const OUTPUT_LEAVES: usize = 64;

        let mut source = String::from("module owner;");
        for index in 0..UNRELATED_IMPORTS {
            source.push_str(&format!(" import provider as p{index};"));
        }
        for index in 0..UNRELATED_DECLARATIONS {
            source.push_str(&format!(" host type Unrelated{index};"));
        }
        source.push_str(" pub host type Leaf; pub type Expanded = ");
        for _ in 1..OUTPUT_LEAVES {
            source.push_str("Leaf & ");
        }
        source.push_str("Leaf;");

        let module = parse_module(&source);
        let scope =
            crate::pass::resolve::TopLevelScope::build(&module).expect("declaration-only scope");
        let provider = parse_module("module provider; pub host type Token;");
        let provider_scope = crate::pass::resolve::TopLevelScope::build(&provider)
            .expect("provider declaration-only scope");
        let package = crate::pass::resolve::Package::from_parts(
            std::collections::BTreeMap::from([
                (
                    "owner".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("owner.kio"),
                        module,
                        scope,
                    },
                ),
                (
                    "provider".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("provider.kio"),
                        module: provider,
                        scope: provider_scope,
                    },
                ),
            ]),
            None,
        );
        let owner = &package.module("owner").expect("owner module").module;
        let source: Type<Lowered> = Type::synth_path(
            vec!["owner".to_owned(), "Expanded".to_owned()],
            Vec::new(),
            span(),
        );

        crate::pass::resolve::reset_type_reexport_work();
        let expanded =
            materialize_aliases_in_scope(&source, Some(owner), Some(&package), None, true);
        assert!(matches!(expanded, Type::Product { .. }));
        assert_eq!(
            crate::pass::resolve::type_reexport_declaration_items_scanned(),
            0,
            "one materialization must reuse the exact package scope instead of scanning the owner declaration list once per nominal output leaf"
        );
        assert_eq!(
            crate::pass::resolve::type_reexport_import_edges_scanned(),
            0,
            "one materialization must reuse the indexed written-import edges instead of scanning the owner import list once per nominal output leaf"
        );
    }

    #[test]
    fn one_alias_output_reuses_its_exact_owner_entry_handle() {
        const OUTPUT_LEAVES: usize = 64;

        let mut source = String::from("module owner; pub host type Leaf; pub type Expanded = ");
        for _ in 1..OUTPUT_LEAVES {
            source.push_str("Leaf & ");
        }
        source.push_str("Leaf;");

        let module = parse_module(&source);
        let scope =
            crate::pass::resolve::TopLevelScope::build(&module).expect("declaration-only scope");
        let package = crate::pass::resolve::Package::from_parts(
            std::collections::BTreeMap::from([(
                "owner".to_owned(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from("owner.kio"),
                    module,
                    scope,
                },
            )]),
            None,
        );
        let owner = &package.module("owner").expect("owner module").module;
        let source: Type<Lowered> = Type::synth_path(
            vec!["owner".to_owned(), "Expanded".to_owned()],
            Vec::new(),
            span(),
        );

        reset_alias_materializer_work();
        let expanded =
            materialize_aliases_in_scope(&source, Some(owner), Some(&package), None, true);
        fn leaf_count(ty: &Type<Lowered>) -> usize {
            match ty {
                Type::Product { left, right, .. } => leaf_count(left) + leaf_count(right),
                Type::Path { segments, args, .. }
                    if args.is_empty()
                        && segments.last().is_some_and(|name| name.as_str() == "Leaf") =>
                {
                    1
                }
                other => panic!("alias output contained an unexpected node: {other:?}"),
            }
        }
        assert_eq!(leaf_count(&expanded), OUTPUT_LEAVES);
        assert!(
            alias_materializer_work().exact_module_entry_lookups <= 2,
            "one materialization must retain its exact package-owner entry instead of reconstructing and looking it up once per nominal output leaf: {:?}",
            alias_materializer_work()
        );
    }

    #[test]
    fn checking_root_frontiers_reuse_one_exact_alias_scope_provider() {
        const UNRELATED_DECLARATIONS: usize = 96;
        const UNRELATED_IMPORTS: usize = 64;
        const OCCURRENCES: usize = 48;

        let mut source = String::from("module owner;");
        for index in 0..UNRELATED_IMPORTS {
            source.push_str(&format!(" import provider as p{index};"));
        }
        source.push_str(" pub host type Leaf; pub type Inner = Leaf; pub type Outer = Inner;");
        for index in 0..UNRELATED_DECLARATIONS {
            source.push_str(&format!(" type Unrelated{index} = .;"));
        }
        let module = parse_module(&source);
        let env = super::super::ModuleEnv::build(&module, None, None, None)
            .expect("checking-root environment");
        let ctx = env.alias_ctx();
        let repeated =
            |name: &str| (1..OCCURRENCES).fold(path(name), |left, _| product(left, path(name)));

        reset_alias_materializer_work();
        crate::pass::resolve::reset_type_reexport_work();
        assert!(type_equiv_state(
            &repeated("Outer"),
            &repeated("Leaf"),
            &ctx,
            false,
            false,
        ));
        let work = alias_materializer_work();
        assert!(
            work.owner_alias_index_builds <= 1,
            "one checking-root comparison must share one exact alias-scope provider across every structural frontier: {work:?}"
        );
        assert!(
            work.owner_alias_items_scanned <= module.items.len(),
            "checking-root alias lookup must be O(declarations + occurrences), not rescan all declarations per occurrence: {work:?}"
        );
        assert!(
            crate::pass::resolve::type_reexport_declaration_items_scanned() <= module.items.len(),
            "checking-root nominal qualification must reuse the same declaration provider instead of rescanning the module once per normalized leaf"
        );
        assert!(
            crate::pass::resolve::type_reexport_import_edges_scanned() <= module.imports.len(),
            "checking-root nominal qualification must reuse the same written-import provider instead of rescanning every import edge per normalized leaf"
        );
        assert_eq!(
            crate::pass::resolve::nominal_provider_work().cache_initializations,
            1,
            "one checking-root operation must allocate one shared cache bundle, not one map set per frontier"
        );
    }

    #[test]
    fn package_owner_frontiers_capture_one_exact_source_entry() {
        const MODULE_DEPTH: usize = 48;
        const OCCURRENCES: usize = 48;

        let module_segments = (0..MODULE_DEPTH)
            .map(|index| format!("m{index}"))
            .collect::<Vec<_>>();
        let module_path = module_segments.join("/");
        let module = parse_module(&format!(
            "module {module_path}; pub type Inner = .; pub type Outer = Inner;"
        ));
        let scope =
            crate::pass::resolve::TopLevelScope::build(&module).expect("declaration-only scope");
        let package = crate::pass::resolve::Package::from_parts(
            std::collections::BTreeMap::from([(
                module_path.clone(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from("owner.kio"),
                    module,
                    scope,
                },
            )]),
            None,
        );
        let owner = &package
            .module(&module_path)
            .expect("deep owner module")
            .module;
        let env = super::super::ModuleEnv::build(owner, None, None, Some(&package))
            .expect("package-owner environment");
        let unit = || Type::Unit {
            meta: Meta::new(span()),
        };
        let repeated_alias =
            (1..OCCURRENCES).fold(path("Outer"), |left, _| product(left, path("Outer")));
        let repeated_unit = (1..OCCURRENCES).fold(unit(), |left, _| product(left, unit()));

        reset_alias_materializer_work();
        assert!(type_equiv_state(
            &repeated_alias,
            &repeated_unit,
            &env.alias_ctx(),
            false,
            false,
        ));
        let work = alias_materializer_work();
        assert!(
            work.exact_module_entry_lookups <= 1,
            "one comparison must capture its exact package source once and thread that handle through every bare alias frontier: {work:?}"
        );
        assert!(
            work.exact_module_entry_path_segments <= MODULE_DEPTH,
            "one semantic operation may join/look up the source module path once, not once per bare alias frontier: {work:?}"
        );
        assert_eq!(
            crate::pass::resolve::nominal_provider_work().cache_initializations,
            0,
            "a direct package-owner alias must borrow its persistent scope without allocating operation-local maps"
        );
    }

    #[test]
    fn written_qualified_alias_fanout_captures_one_exact_edge_target() {
        const MODULE_DEPTH: usize = 32;
        const OCCURRENCES: usize = 32;

        let provider_segments = (0..MODULE_DEPTH)
            .map(|index| format!("provider{index}"))
            .collect::<Vec<_>>();
        let provider_path = provider_segments.join("/");
        let provider = parse_module(&format!("module {provider_path}; pub type Item = .;"));
        let consumer = parse_module(&format!("module consumer; import {provider_path} as p;"));
        let provider_scope =
            crate::pass::resolve::TopLevelScope::build(&provider).expect("provider scope");
        let consumer_scope =
            crate::pass::resolve::TopLevelScope::build(&consumer).expect("consumer scope");
        let package = crate::pass::resolve::Package::<Lowered>::from_parts(
            std::collections::BTreeMap::from([
                (
                    "consumer".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("consumer.kio"),
                        module: consumer,
                        scope: consumer_scope,
                    },
                ),
                (
                    provider_path.clone(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("provider.kio"),
                        module: provider,
                        scope: provider_scope,
                    },
                ),
            ]),
            None,
        );
        let consumer = &package.module("consumer").expect("consumer").module;
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(consumer),
            package: Some(&package),
            binder_locals: None,
        };
        let item = || qualified("p", "Item");
        let unit = || Type::Unit {
            meta: Meta::new(span()),
        };
        let aliases = (1..OCCURRENCES).fold(item(), |left, _| product(left, item()));
        let units = (1..OCCURRENCES).fold(unit(), |left, _| product(left, unit()));

        reset_alias_materializer_work();
        crate::pass::resolve::reset_type_reexport_work();
        assert!(type_equiv(&aliases, &units, &ctx));
        let work = alias_materializer_work();
        assert!(
            work.edge_target_lookups <= 1 && work.edge_target_path_segments <= MODULE_DEPTH,
            "one written qualified edge must capture and join its exact target entry once per comparison: {work:?}"
        );
        assert!(
            crate::pass::resolve::indexed_import_prefix_clones() <= 1
                && crate::pass::resolve::indexed_import_prefix_segments_cloned() <= MODULE_DEPTH,
            "one written qualified edge must borrow its indexed target prefix instead of cloning it at every alias frontier"
        );
    }

    #[test]
    fn selective_alias_fanout_shares_one_exact_deep_edge_target() {
        const MODULE_DEPTH: usize = 32;
        const ALIASES: usize = 32;

        let provider_segments = (0..MODULE_DEPTH)
            .map(|index| format!("provider{index}"))
            .collect::<Vec<_>>();
        let provider_path = provider_segments.join("/");
        let names = (0..ALIASES)
            .map(|index| format!("Alias{index}"))
            .collect::<Vec<_>>();
        let mut provider = format!("module {provider_path};");
        for name in &names {
            provider.push_str(&format!(" pub type {name} = .;"));
        }
        let provider = parse_module(&provider);
        let consumer = parse_module(&format!(
            "module consumer; import {provider_path}({});",
            names.join(", ")
        ));
        let provider_scope =
            crate::pass::resolve::TopLevelScope::build(&provider).expect("provider scope");
        let consumer_scope =
            crate::pass::resolve::TopLevelScope::build(&consumer).expect("consumer scope");
        let package = crate::pass::resolve::Package::<Lowered>::from_parts(
            std::collections::BTreeMap::from([
                (
                    "consumer".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("consumer.kio"),
                        module: consumer,
                        scope: consumer_scope,
                    },
                ),
                (
                    provider_path.clone(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("provider.kio"),
                        module: provider,
                        scope: provider_scope,
                    },
                ),
            ]),
            None,
        );
        let consumer = &package.module("consumer").expect("consumer").module;
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(consumer),
            package: Some(&package),
            binder_locals: None,
        };
        let aliases = names
            .iter()
            .skip(1)
            .fold(path(&names[0]), |left, name| product(left, path(name)));
        let unit = || Type::Unit {
            meta: Meta::new(span()),
        };
        let units = (1..ALIASES).fold(unit(), |left, _| product(left, unit()));

        reset_alias_materializer_work();
        crate::pass::resolve::reset_type_reexport_work();
        assert!(type_equiv(&aliases, &units, &ctx));
        let work = alias_materializer_work();
        assert!(
            work.edge_target_lookups <= 1 && work.edge_target_path_segments <= MODULE_DEPTH,
            "one same-prefix selective alias fanout must capture and join its exact target entry once per comparison: {work:?}"
        );
        assert!(
            crate::pass::resolve::indexed_import_prefix_clones() <= 1
                && crate::pass::resolve::indexed_import_prefix_segments_cloned() <= MODULE_DEPTH,
            "one same-prefix selective alias fanout must borrow its shared indexed target prefix instead of cloning it per name"
        );
    }

    #[test]
    fn in_scope_binder_shadows_same_named_local_alias() {
        let alias_body = Type::Unit {
            meta: Meta::new(span()),
        };
        let local = HashMap::from([(
            "A",
            AliasDef {
                type_params: &[],
                body: &alias_body,
                owner_module: None,
            },
        )]);
        let cross_module = HashMap::new();
        let binders = HashSet::from(["A".to_owned()]);
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: Some(&binders),
        };

        let (canonical, _) = canonicalize_for_comparison(&path("A"), &ctx, false);
        assert!(matches!(
            canonical,
            Type::Path { ref segments, ref args, .. }
                if segments.len() == 1 && segments[0].as_str() == "A" && args.is_empty()
        ));
    }

    #[test]
    fn alias_declaration_head_resolves_each_scope_and_owner_branch() {
        let source = parse_module(
            "module source; import provider(Host, Imported, Shared); import provider as dep; \
             type Host[A] = dep.Host(A); type Local = .; type Shared = dep.Shared; \
             host type Terminal;",
        );
        let provider = parse_module(
            "module provider; pub host type Host[A]; pub type Imported = .; \
             pub type Qualified = .; pub type Shared = .;",
        );
        let mut modules = std::collections::BTreeMap::new();
        for module in [provider, source] {
            let path = module
                .path
                .segments
                .iter()
                .map(crate::ast::PathSegment::as_str)
                .collect::<Vec<_>>()
                .join("/");
            let scope = crate::pass::resolve::TopLevelScope::build(&module).expect("scope");
            modules.insert(
                path.clone(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from(format!("{path}.kio")),
                    module,
                    scope,
                },
            );
        }
        let package = crate::pass::resolve::Package::<Lowered>::from_parts(modules, None);
        let source = &package.module("source").expect("source").module;
        let provider = &package.module("provider").expect("provider").module;
        let local = HashMap::from([
            (
                "Host",
                AliasDef {
                    owner_module: None,
                    ..owner_alias_def(source, "Host").expect("source Host")
                },
            ),
            (
                "Local",
                AliasDef {
                    owner_module: None,
                    ..owner_alias_def(source, "Local").expect("source Local")
                },
            ),
            (
                "Shared",
                AliasDef {
                    owner_module: None,
                    ..owner_alias_def(source, "Shared").expect("source Shared")
                },
            ),
        ]);
        let cross_module = HashMap::from([
            (
                "Imported",
                owner_alias_def(provider, "Imported").expect("provider Imported"),
            ),
            (
                "Shared",
                owner_alias_def(provider, "Shared").expect("provider Shared"),
            ),
        ]);
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(source),
            package: Some(&package),
            binder_locals: None,
        };
        let resolve = |names: &[&str], identity_canonical| {
            let segments = names
                .iter()
                .map(|name| crate::ast::PathSegment::new((*name).to_owned(), span()))
                .collect::<Vec<_>>();
            resolve_alias_declaration_head(&segments, &ctx, identity_canonical)
        };

        let local_hit = resolve(&["Local"], false).expect("bare local alias");
        assert!(std::ptr::eq(
            local_hit.owner_module.expect("source owner"),
            source
        ));
        assert!(std::ptr::eq(
            local_hit.def.body,
            owner_alias_def(source, "Local").expect("source Local").body
        ));

        let imported = resolve(&["Imported"], false).expect("bare selective alias");
        assert!(std::ptr::eq(
            imported.owner_module.expect("provider owner"),
            provider
        ));
        assert!(std::ptr::eq(
            imported.def.body,
            owner_alias_def(provider, "Imported")
                .expect("provider Imported")
                .body
        ));
        assert!(
            resolve(&["Host"], false).is_none(),
            "a written imported host is a selected non-alias, not a miss that may fall through to the same-spelled local alias"
        );
        let qualified = resolve(&["dep", "Qualified"], false).expect("qualified import alias");
        assert!(std::ptr::eq(
            qualified.owner_module.expect("provider owner"),
            provider
        ));
        let provider_qualified = owner_alias_def(provider, "Qualified")
            .expect("provider Qualified")
            .body;
        assert!(std::ptr::eq(qualified.def.body, provider_qualified));
        let canonical =
            resolve(&["provider", "Qualified"], true).expect("canonical provider alias");
        assert!(std::ptr::eq(
            canonical.owner_module.expect("provider owner"),
            provider
        ));
        assert!(std::ptr::eq(canonical.def.body, provider_qualified));

        let terminal_binding = resolve(&["Shared"], false).expect("terminal Shared");
        assert!(std::ptr::eq(
            terminal_binding.owner_module.expect("provider owner"),
            provider
        ));
        assert!(std::ptr::eq(
            terminal_binding.def.body,
            owner_alias_def(provider, "Shared")
                .expect("provider Shared")
                .body
        ));
        assert!(
            resolve(&["source", "Terminal"], true).is_none(),
            "an exact source non-alias is terminal in its validated package owner"
        );
    }

    #[test]
    fn qualified_alias_does_not_authorize_a_nested_module_tail() {
        let provider = parse_module("module provider;");
        let nested = parse_module("module provider/nested; pub type Item = .;");
        let consumer = parse_module("module consumer; import provider as p;");
        let provider_scope =
            crate::pass::resolve::TopLevelScope::build(&provider).expect("provider scope");
        let nested_scope =
            crate::pass::resolve::TopLevelScope::build(&nested).expect("nested scope");
        let consumer_scope =
            crate::pass::resolve::TopLevelScope::build(&consumer).expect("consumer scope");
        let package = crate::pass::resolve::Package::<Lowered>::from_parts(
            std::collections::BTreeMap::from([
                (
                    "consumer".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("consumer.kio"),
                        module: consumer,
                        scope: consumer_scope,
                    },
                ),
                (
                    "provider".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("provider.kio"),
                        module: provider,
                        scope: provider_scope,
                    },
                ),
                (
                    "provider/nested".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("nested.kio"),
                        module: nested,
                        scope: nested_scope,
                    },
                ),
            ]),
            None,
        );
        let consumer = &package.module("consumer").expect("consumer").module;
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(consumer),
            package: Some(&package),
            binder_locals: None,
        };
        let segments = |names: &[&str]| {
            names
                .iter()
                .map(|name| crate::ast::PathSegment::new((*name).to_owned(), span()))
                .collect::<Vec<_>>()
        };

        assert!(
            resolve_alias_declaration_head(&segments(&["provider", "nested", "Item"]), &ctx, true,)
                .is_some(),
            "an already-canonical multi-segment identity must retain the separate exact-package lookup"
        );
        assert!(
            resolve_alias_declaration_head(&segments(&["p", "nested", "Item"]), &ctx, false)
                .is_none(),
            "a written qualified alias names one exact module surface and must not authorize a nested module tail"
        );
    }

    #[test]
    fn canonical_tree_flag_keeps_a_bare_alias_in_scope_lookup() {
        let source = parse_module("module source; type Needs[A] = A;");
        let env = super::super::ModuleEnv::build(&source, None, None, None)
            .expect("standalone source environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = super::super::TypeCtx::new(&env, &mut elaborations);
        let segments = [crate::ast::PathSegment::new("Needs".to_owned(), span())];
        let error = super::super::types::resolve_nominal_head_kind(
            &segments,
            0,
            span(),
            &tcx,
            super::super::types::NominalHeadBinding::AmbientOrEmbedded(None),
            true,
        )
        .expect_err("a bare alias remains scope-resolved under a canonical tree flag");
        let diagnostic = match error {
            crate::error::Error::Type(diagnostic) => diagnostic,
            other => panic!("expected alias arity type error, found {other:?}"),
        };
        assert!(
            diagnostic.message.contains("requires 1 type argument"),
            "the alias lookup must run before the fallback kind: {}",
            diagnostic.message
        );
    }

    #[test]
    fn canonical_composite_unfolds_exact_source_alias_without_package_entry() {
        let source = parse_module("module aux; type B = .; type A = B;");
        let alias = owner_alias_def(&source, "A").expect("local alias A");
        let local = HashMap::from([(
            "A",
            AliasDef {
                owner_module: None,
                ..alias
            },
        )]);
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(&source),
            package: None,
            binder_locals: None,
        };
        let source_ty = product(
            path("A"),
            Type::Unit {
                meta: Meta::new(span()),
            },
        );
        let expected = product(
            Type::Unit {
                meta: Meta::new(span()),
            },
            Type::Unit {
                meta: Meta::new(span()),
            },
        );

        let (canonical, identity_canonical) = canonicalize_for_comparison(&source_ty, &ctx, false);
        assert!(
            identity_canonical,
            "the standalone source module must make the composite identity-exact"
        );
        assert!(
            type_equiv_state(&canonical, &expected, &ctx, identity_canonical, false),
            "the qualified `aux.A` frontier must recursively unfold `aux.B` through its exact source module"
        );
    }

    #[test]
    fn exact_source_non_alias_blocks_same_path_package_alias() {
        let source = parse_module("module aux; host type A;");
        let indexed = parse_module("module aux; type A = .;");
        let scope =
            crate::pass::resolve::TopLevelScope::build(&indexed).expect("indexed alias scope");
        let package = crate::pass::resolve::Package::<Lowered>::from_parts(
            std::collections::BTreeMap::from([(
                "aux".to_owned(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from("indexed-aux.kio"),
                    module: indexed,
                    scope,
                },
            )]),
            None,
        );
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(&source),
            package: Some(&package),
            binder_locals: None,
        };
        let source_a = Type::synth_path(vec!["aux".to_owned(), "A".to_owned()], Vec::new(), span());
        let unit = Type::Unit {
            meta: Meta::new(span()),
        };

        assert!(
            !type_equiv_state(&source_a, &unit, &ctx, true, false),
            "an exact source declaration must be terminal even when a same-path package module defines an alias"
        );
    }

    #[test]
    fn canonical_source_composite_is_terminal_over_same_path_package_import() {
        let source = parse_module("module aux; host type A;");
        let indexed = parse_module("module aux; import other(A);");
        let other = parse_module("module other; pub host type A;");
        let indexed_scope =
            crate::pass::resolve::TopLevelScope::build(&indexed).expect("indexed import scope");
        let other_scope =
            crate::pass::resolve::TopLevelScope::build(&other).expect("provider scope");
        let package = crate::pass::resolve::Package::<Lowered>::from_parts(
            std::collections::BTreeMap::from([
                (
                    "aux".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("indexed-aux.kio"),
                        module: indexed,
                        scope: indexed_scope,
                    },
                ),
                (
                    "other".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("other.kio"),
                        module: other,
                        scope: other_scope,
                    },
                ),
            ]),
            None,
        );
        let source_a = Type::synth_path(vec!["aux".to_owned(), "A".to_owned()], Vec::new(), span());
        let ty = product(
            source_a.clone(),
            Type::Unit {
                meta: Meta::new(span()),
            },
        );

        let materialized =
            materialize_aliases_in_scope(&ty, Some(&source), Some(&package), None, true);
        assert_eq!(
            materialized, ty,
            "an exact checking-root nominal inside a composite must not follow a same-path package entry's import edge"
        );
        let package_a =
            Type::synth_path(vec!["other".to_owned(), "A".to_owned()], Vec::new(), span());
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(&source),
            package: Some(&package),
            binder_locals: None,
        };
        assert!(
            !type_equiv_state(&source_a, &package_a, &ctx, true, false,),
            "the colliding package nominal must not acquire the checking-root nominal's identity"
        );
        assert_eq!(
            canonicalize_for_comparison(&ty, &ctx, true),
            (ty, true),
            "deep canonicalization must preserve the same exact checking-root terminal"
        );
    }

    #[test]
    fn checking_root_same_path_package_context_fails_closed_before_mixed_materialization() {
        let source = parse_module("module aux; host type A; type Pair[X] = A & X;");
        let package_aux = parse_module("module aux; pub host type A;");
        let provider = parse_module("module provider; pub type Get = aux.A;");
        let aux_scope =
            crate::pass::resolve::TopLevelScope::build(&package_aux).expect("package aux scope");
        let provider_scope =
            crate::pass::resolve::TopLevelScope::build(&provider).expect("provider scope");
        let package = crate::pass::resolve::Package::<Lowered>::from_parts(
            std::collections::BTreeMap::from([
                (
                    "aux".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("package-aux.kio"),
                        module: package_aux,
                        scope: aux_scope,
                    },
                ),
                (
                    "provider".to_owned(),
                    crate::pass::resolve::ModuleEntry {
                        file_path: PathBuf::from("provider.kio"),
                        module: provider,
                        scope: provider_scope,
                    },
                ),
            ]),
            None,
        );
        let env = super::super::ModuleEnv::build(&source, None, None, Some(&package))
            .expect("standalone checking-root environment");
        let aliases = env.alias_ctx();
        let package_get = Type::synth_path(
            vec!["provider".to_owned(), "Get".to_owned()],
            Vec::new(),
            span(),
        );
        let local_a = path("A");
        let mixed_pair =
            Type::synth_path(vec!["Pair".to_owned()], vec![package_get.clone()], span());
        let local_pair = Type::synth_path(vec!["Pair".to_owned()], vec![local_a.clone()], span());
        let nested_mixed = product(
            mixed_pair.clone(),
            Type::Unit {
                meta: Meta::new(span()),
            },
        );
        let nested_local = product(
            local_pair.clone(),
            Type::Unit {
                meta: Meta::new(span()),
            },
        );
        let expanded_local_pair = product(local_a.clone(), local_a.clone());
        let exact_local_a = qualified("aux", "A");
        let exact_local_pair = Type::synth_path(
            vec!["aux".to_owned(), "Pair".to_owned()],
            vec![exact_local_a.clone()],
            span(),
        );

        let nominal_provider = NominalProvider::new(Some(&source), Some(&package));
        let Type::Path {
            segments: exact_pair_segments,
            ..
        } = &exact_local_pair
        else {
            panic!("the exact local alias fixture must be a path")
        };
        assert!(matches!(
            nominal_provider.root(),
            AliasNominalScope::CheckingRoot {
                module,
                package_collision: true,
            } if std::ptr::eq(module, &source)
        ));
        let NominalSelection::Selected(exact_pair_declaration) =
            nominal_provider.select(nominal_provider.root(), exact_pair_segments, true)
        else {
            panic!("an exact checking-root path must retain its local declaration")
        };
        assert_eq!(exact_pair_declaration.route, NominalRoute::Exact);
        assert_eq!(
            exact_pair_declaration
                .declaration
                .type_alias()
                .map(|alias| alias.name.as_str()),
            Some("Pair")
        );
        assert_eq!(
            nominal_provider.qualify(nominal_provider.root(), exact_pair_segments, true,),
            (exact_pair_segments.to_vec(), true),
            "a collision blocks only cross-owner qualification, not the checking root's exact path"
        );

        let exact_materialized = materialize_aliases_in_scope(
            &exact_local_pair,
            Some(&source),
            Some(&package),
            None,
            true,
        );
        let Type::Product {
            left: exact_left,
            right: exact_right,
            ..
        } = exact_materialized
        else {
            panic!("the exact checking-root alias must materialize to its product body")
        };
        for exact_child in [&*exact_left, &*exact_right] {
            let Type::Path { segments, args, .. } = exact_child else {
                panic!("each materialized product child must remain an exact nominal path")
            };
            assert!(args.is_empty());
            assert!(
                segments
                    .iter()
                    .map(crate::ast::PathSegment::as_str)
                    .eq(["aux", "A"]),
                "each child must retain the checking root's exact `aux.A` identity"
            );
        }

        assert!(
            type_equiv(&local_a, &local_a, &aliases)
                && type_equiv(&local_pair, &expanded_local_pair, &aliases),
            "a malformed same-path package collision must preserve exact local reflexivity and local alias materialization"
        );

        assert!(
            !type_equiv(&package_get, &local_a, &aliases)
                && !type_equiv(&local_a, &package_get, &aliases)
                && !type_equiv(&mixed_pair, &local_pair, &aliases)
                && !type_equiv(&local_pair, &mixed_pair, &aliases)
                && !type_equiv(&nested_mixed, &nested_local, &aliases),
            "a pointer-distinct same-path package entry must make the checking-root context opaque before cross-owner alias materialization"
        );
    }

    #[test]
    fn equal_path_text_does_not_erase_mixed_canonical_identity() {
        let exact = parse_module("module base; pub host type Box;");
        let wrong = parse_module("module wrong; pub host type Box;");
        let consumer = parse_module("module consumer; import wrong as base;");
        let mut modules = std::collections::BTreeMap::new();
        for module in [exact, wrong, consumer] {
            let path = module
                .path
                .segments
                .iter()
                .map(|segment| segment.as_str())
                .collect::<Vec<_>>()
                .join("/");
            let scope = crate::pass::resolve::TopLevelScope::build(&module)
                .expect("declaration-only scope");
            modules.insert(
                path.clone(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from(format!("{path}.kio")),
                    module,
                    scope,
                },
            );
        }
        let package = crate::pass::resolve::Package::<Lowered>::from_parts(modules, None);
        let consumer = &package.module("consumer").expect("consumer module").module;
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let exact_box = Type::synth_path(
            vec!["base".to_owned(), "Box".to_owned()],
            Vec::new(),
            span(),
        );
        let contextual_box = exact_box.clone();
        let exact_product = product(
            exact_box.clone(),
            Type::Unit {
                meta: Meta::new(span()),
            },
        );
        let contextual_product = exact_product.clone();

        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(consumer),
            package: Some(&package),
            binder_locals: None,
        };
        let pointer_equal = type_equiv_state(&exact_box, &exact_box, &ctx, true, false);
        let raw_equal = type_equiv_state(&exact_box, &contextual_box, &ctx, true, false);
        let product_pointer_equal =
            type_equiv_state(&exact_product, &exact_product, &ctx, true, false);
        let product_raw_equal =
            type_equiv_state(&exact_product, &contextual_product, &ctx, true, false);

        let interner = super::super::TypeInterner::default();
        let interned_ctx = AliasCtx {
            type_interner: Some(&interner),
            ..ctx
        };
        let interned_equal =
            type_equiv_state(&exact_box, &contextual_box, &interned_ctx, true, false);
        let product_interned_equal = type_equiv_state(
            &exact_product,
            &contextual_product,
            &interned_ctx,
            true,
            false,
        );
        assert_eq!(
            (
                pointer_equal,
                raw_equal,
                interned_equal,
                product_pointer_equal,
                product_raw_equal,
                product_interned_equal,
            ),
            (false, false, false, false, false, false),
            "pointer identity, raw equality, and structural interning must preserve handle-local canonical provenance at both path and composite roots"
        );
        assert!(type_equiv_state(
            &exact_box,
            &contextual_box,
            &interned_ctx,
            true,
            true,
        ));
        assert!(type_equiv_state(
            &exact_box,
            &contextual_box,
            &interned_ctx,
            false,
            false,
        ));
    }

    #[test]
    fn mixed_canonical_flags_accept_the_same_exact_nominal_after_descent() {
        let base = parse_module("module base; pub host type Box;");
        let consumer = parse_module("module consumer;");
        let mut modules = std::collections::BTreeMap::new();
        for module in [base, consumer] {
            let path = module
                .path
                .segments
                .iter()
                .map(|segment| segment.as_str())
                .collect::<Vec<_>>()
                .join("/");
            let scope = crate::pass::resolve::TopLevelScope::build(&module)
                .expect("declaration-only scope");
            modules.insert(
                path.clone(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from(format!("{path}.kio")),
                    module,
                    scope,
                },
            );
        }
        let package = crate::pass::resolve::Package::<Lowered>::from_parts(modules, None);
        let consumer = &package.module("consumer").expect("consumer module").module;
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let exact_box = Type::synth_path(
            vec!["base".to_owned(), "Box".to_owned()],
            Vec::new(),
            span(),
        );
        let contextual_box = exact_box.clone();
        let exact_product = product(
            exact_box.clone(),
            Type::Unit {
                meta: Meta::new(span()),
            },
        );
        let contextual_product = exact_product.clone();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(consumer),
            package: Some(&package),
            binder_locals: None,
        };
        let interner = super::super::TypeInterner::default();
        let interned_ctx = AliasCtx {
            type_interner: Some(&interner),
            ..ctx
        };

        assert!(type_equiv_state(&exact_box, &exact_box, &ctx, true, false));
        assert!(type_equiv_state(
            &exact_box,
            &contextual_box,
            &ctx,
            true,
            false,
        ));
        assert!(type_equiv_state(
            &exact_box,
            &contextual_box,
            &interned_ctx,
            true,
            false,
        ));
        assert!(type_equiv_state(
            &exact_product,
            &exact_product,
            &ctx,
            true,
            false,
        ));
        assert!(type_equiv_state(
            &exact_product,
            &contextual_product,
            &ctx,
            true,
            false,
        ));
        assert!(type_equiv_state(
            &exact_product,
            &contextual_product,
            &interned_ctx,
            true,
            false,
        ));
    }

    #[test]
    fn mixed_canonical_flags_do_not_override_an_exact_ambient_binder() {
        let source = parse_module("module consumer; type A = .;");
        let alias = owner_alias_def(&source, "A").expect("local decoy alias");
        let local = HashMap::from([("A", alias)]);
        let cross_module = HashMap::new();
        let binders = HashSet::from(["A".to_owned()]);
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(&source),
            package: None,
            binder_locals: Some(&binders),
        };
        let left = path("A");
        let right = left.clone();
        let left_product = product(
            left.clone(),
            Type::Unit {
                meta: Meta::new(span()),
            },
        );
        let right_product = left_product.clone();

        assert!(
            type_equiv_state(&left, &right, &ctx, true, false),
            "the exact ambient binder proof outranks both canonical flags and a same-spelled nominal alias"
        );
        assert!(
            type_equiv_state(&left_product, &right_product, &ctx, true, false,),
            "mixed flags on an ordinary structural parent must descend to the exact ambient binder proof rather than force blanket inequality"
        );
    }

    #[test]
    fn mixed_canonical_flags_keep_an_ambient_binder_distinct_from_a_nominal() {
        let source = parse_module("module consumer; host type A;");
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let binders = HashSet::from(["A".to_owned()]);
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(&source),
            package: None,
            binder_locals: Some(&binders),
        };
        let binder = path("A");
        let nominal = qualified("consumer", "A");
        let binder_product = product(
            binder.clone(),
            Type::Unit {
                meta: Meta::new(span()),
            },
        );
        let nominal_product = product(
            nominal.clone(),
            Type::Unit {
                meta: Meta::new(span()),
            },
        );

        assert!(
            !type_equiv_state(&binder, &nominal, &ctx, false, true),
            "a contextual rigid binder must not acquire a same-spelled exact nominal identity"
        );
        assert!(
            !type_equiv_state(&nominal, &binder, &ctx, true, false),
            "binder-versus-nominal separation must be symmetric"
        );
        assert!(
            !type_equiv_state(&binder_product, &nominal_product, &ctx, false, true),
            "ordinary structural recursion must preserve binder-versus-nominal separation"
        );
    }

    /// A reflected type's payload, spelt through a qualified-import alias
    /// (`_elab_type_of__.Rep_unit`), is identity-exact-equal to the same
    /// payload reached by unfolding an imported alias to its bare owner
    /// spelling — once both sides are canonicalized to `(module, name)`
    /// against the checking module's imports. The bare
    /// last-segment leniency this used to lean on is gone: the match now
    /// holds only because the qualified-import alias and the selective
    /// imports both resolve `Rep_*` to module `type_of`.
    #[test]
    fn type_equiv_matches_qualified_expanded_payload_to_imported_alias_payload() {
        let owner = parse_module(
            "module type_of; \
             pub newtype Rep_unit : . { pub constructor mk_u; pub projector un_u; }; \
             pub newtype Rep_bottom : . { pub constructor mk_b; pub projector un_b; }; \
             pub newtype Rep_product : . { pub constructor mk_p; pub projector un_p; }; \
             pub newtype Rep_sum : . { pub constructor mk_s; pub projector un_s; }; \
             pub newtype Rep_function : . { pub constructor mk_f; pub projector un_f; }; \
             pub newtype Rep_opaque : . { pub constructor mk_o; pub projector un_o; }; \
             pub type Type_rep_tail4 = Rep_function | Rep_opaque; \
             pub type Type_rep_tail3 = Rep_sum | Type_rep_tail4; \
             pub type Type_rep_tail2 = Rep_product | Type_rep_tail3; \
             pub type Type_rep_tail1 = Rep_bottom | Type_rep_tail2;",
        );
        let consumer = parse_module(
            "module consumer; \
             import type_of as _elab_type_of__; \
             import type_of(Rep_unit, Rep_bottom, Rep_product, Rep_sum, Rep_function, Rep_opaque, \
                 Type_rep_tail1); \
             pub type Use = Rep_unit;",
        );
        let mut modules = std::collections::BTreeMap::new();
        for module in [owner, consumer] {
            let path = module
                .path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("/");
            let scope = crate::pass::resolve::TopLevelScope::build(&module).expect("scope");
            modules.insert(
                path.clone(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from(format!("{path}.kio")),
                    module,
                    scope,
                },
            );
        }
        let package = crate::pass::resolve::Package::<Lowered>::from_parts(modules, None);
        let consumer_entry = package.module("consumer").expect("consumer module");
        let tail1 = package
            .module("type_of")
            .expect("type_of module")
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::TypeAlias(alias) if alias.name == "Type_rep_tail1" => Some(alias),
                _ => None,
            })
            .expect("Type_rep_tail1 alias");
        let owner_module = &package.module("type_of").expect("type_of module").module;
        let mut cross_module = HashMap::new();
        cross_module.insert(
            "Type_rep_tail1",
            AliasDef {
                type_params: &tail1.type_params,
                body: tail1.type_body(),
                owner_module: Some(owner_module),
            },
        );
        let local = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(&consumer_entry.module),
            package: Some(&package),
            binder_locals: None,
        };
        let expected = sum(path("Rep_unit"), path("Type_rep_tail1"));
        let found = sum(
            qualified("_elab_type_of__", "Rep_unit"),
            sum(
                qualified("_elab_type_of__", "Rep_bottom"),
                sum(
                    qualified("_elab_type_of__", "Rep_product"),
                    sum(
                        qualified("_elab_type_of__", "Rep_sum"),
                        sum(
                            qualified("_elab_type_of__", "Rep_function"),
                            qualified("_elab_type_of__", "Rep_opaque"),
                        ),
                    ),
                ),
            ),
        );

        assert!(type_equiv(&found, &expected, &ctx));
    }

    /// A **qualified** cross-module alias the checking scope does *not*
    /// import by name (`dep/store.I32`, the transparent alias a `rehost`
    /// materializes — `pub type I32 = host.I32`) unfolds to its body
    /// through its declaring module's own definition, reached via the
    /// package. So `type_equiv(dep/store.I32, host.I32)` holds in a third
    /// module's scope that imports neither — exactly the cross-module-fn
    /// call shape the declaring-module qualification produces. Without the package
    /// the qualified alias stays opaque and the two are spuriously
    /// distinct (asserted by the no-package control below).
    #[test]
    fn type_equiv_unfolds_qualified_cross_module_alias_via_package() {
        let host = parse_module("module host; host type I32 role(i32);");
        let store = parse_module("module dep/store; import host as host; pub type I32 = host.I32;");
        let consumer = parse_module("module consumer; pub type Unused = .;");
        let mut modules = std::collections::BTreeMap::new();
        for module in [host, store, consumer] {
            let path = module
                .path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("/");
            let scope = crate::pass::resolve::TopLevelScope::build(&module).expect("scope");
            modules.insert(
                path.clone(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from(format!("{path}.kio")),
                    module,
                    scope,
                },
            );
        }
        let package = crate::pass::resolve::Package::<Lowered>::from_parts(modules, None);
        let consumer_entry = package.module("consumer").expect("consumer module");

        // `dep/store.I32` and `host.I32`, each as a fully-qualified head —
        // the shape the declaring-module qualification leaves in a cross-module fn's signature and at
        // a literal's resolved annotation. The consumer imports neither, so
        // `local` / `cross_module` are empty: only the package fallback can
        // relate them.
        let store_i32 = Type::synth_path(
            vec!["dep".into(), "store".into(), "I32".into()],
            Vec::new(),
            span(),
        );
        let host_i32 = Type::synth_path(vec!["host".into(), "I32".into()], Vec::new(), span());
        let empty_local = HashMap::new();
        let empty_cross = HashMap::new();

        let with_package = AliasCtx {
            local: &empty_local,
            cross_module: &empty_cross,
            type_interner: None,
            source_module: Some(&consumer_entry.module),
            package: Some(&package),
            binder_locals: None,
        };
        assert!(
            type_equiv(&store_i32, &host_i32, &with_package),
            "a qualified cross-module rehost alias must unfold via its declaring module in the package",
        );

        // Control: with no package the qualified alias cannot be unfolded,
        // so identity-exact qualification keeps the two distinct.
        let without_package = AliasCtx {
            local: &empty_local,
            cross_module: &empty_cross,
            type_interner: None,
            source_module: Some(&consumer_entry.module),
            package: None,
            binder_locals: None,
        };
        assert!(
            !type_equiv(&store_i32, &host_i32, &without_package),
            "without the package the qualified alias stays opaque and the two stay distinct",
        );
    }

    #[test]
    fn qualified_alias_ignores_caller_local_same_leaf_alias() {
        let types = parse_module(
            "module types; pub host type Actual role(bool); pub host type Other role(i32);",
        );
        let dep = parse_module("module dep; import types as types; pub type Flag = types.Actual;");
        let consumer =
            parse_module("module consumer; import types as types; type Actual = types.Other;");
        let mut modules = std::collections::BTreeMap::new();
        for module in [types, dep, consumer] {
            let path = module
                .path
                .segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join("/");
            let scope = crate::pass::resolve::TopLevelScope::build(&module).expect("scope");
            modules.insert(
                path.clone(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from(format!("{path}.kio")),
                    module,
                    scope,
                },
            );
        }
        let package = crate::pass::resolve::Package::<Lowered>::from_parts(modules, None);
        let consumer = &package.module("consumer").expect("consumer").module;
        let env = super::super::ModuleEnv::build(consumer, None, None, Some(&package))
            .expect("consumer env");
        let dep_flag = Type::synth_path(vec!["dep".into(), "Flag".into()], Vec::new(), span());
        let declared_actual =
            Type::synth_path(vec!["types".into(), "Actual".into()], Vec::new(), span());

        assert!(
            type_equiv(&dep_flag, &declared_actual, &env.alias_ctx()),
            "the qualified callee type must unfold in `dep`, not through caller alias `Actual`",
        );
        let caller_actual =
            Type::synth_path(vec!["types".into(), "Other".into()], Vec::new(), span());
        assert!(
            !type_equiv(&dep_flag, &caller_actual, &env.alias_ctx()),
            "the regression control must distinguish the caller's same-leaf alias target",
        );
    }

    #[test]
    fn generic_alias_argument_keeps_caller_identity_during_owner_unfolding() {
        let types = parse_module(
            "module types; pub host type Dep_actual role(bool); pub host type Caller_actual role(i32);",
        );
        let dep = parse_module(
            "module dep; import types as types; type Actual = types.Dep_actual; pub type Id[A] = A;",
        );
        let consumer = parse_module(
            "module consumer; import types as types; type Actual = types.Caller_actual;",
        );
        let mut modules = std::collections::BTreeMap::new();
        for module in [types, dep, consumer] {
            let path = module
                .path
                .segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join("/");
            let scope = crate::pass::resolve::TopLevelScope::build(&module).expect("scope");
            modules.insert(
                path.clone(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from(format!("{path}.kio")),
                    module,
                    scope,
                },
            );
        }
        let package = crate::pass::resolve::Package::<Lowered>::from_parts(modules, None);
        let consumer = &package.module("consumer").expect("consumer").module;
        let env = super::super::ModuleEnv::build(consumer, None, None, Some(&package))
            .expect("consumer env");
        let applied = Type::synth_path(
            vec!["dep".into(), "Id".into()],
            vec![path("Actual")],
            span(),
        );
        let caller_actual = Type::synth_path(
            vec!["types".into(), "Caller_actual".into()],
            Vec::new(),
            span(),
        );
        let dep_actual = Type::synth_path(
            vec!["types".into(), "Dep_actual".into()],
            Vec::new(),
            span(),
        );

        assert!(
            type_equiv(&applied, &caller_actual, &env.alias_ctx()),
            "the caller's alias argument must survive capture-avoiding substitution",
        );
        assert!(
            !type_equiv(&applied, &dep_actual, &env.alias_ctx()),
            "the alias owner's same-leaf declaration must not capture the caller argument",
        );
    }

    #[test]
    fn compound_alias_children_keep_owner_identity_during_recursive_comparison() {
        let base = parse_module("module base; pub host type Value role(bool);");
        let wrong = parse_module(
            "module wrong; pub host type Value role(i32); pub type Pair = Value & Value;",
        );
        let owner = parse_module(
            "module owner; import base as base; pub type Pair = base.Value & base.Value;",
        );
        let consumer =
            parse_module("module consumer; import wrong as base; import wrong as owner;");
        let mut modules = std::collections::BTreeMap::new();
        for module in [base, wrong, owner, consumer] {
            let path = module
                .path
                .segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join("/");
            let scope = crate::pass::resolve::TopLevelScope::build(&module).expect("scope");
            modules.insert(
                path.clone(),
                crate::pass::resolve::ModuleEntry {
                    file_path: PathBuf::from(format!("{path}.kio")),
                    module,
                    scope,
                },
            );
        }
        let package = crate::pass::resolve::Package::<Lowered>::from_parts(modules, None);
        let consumer = &package.module("consumer").expect("consumer").module;
        let env = super::super::ModuleEnv::build(consumer, None, None, Some(&package))
            .expect("consumer env");
        let owner_pair = Type::synth_path(vec!["owner".into(), "Pair".into()], Vec::new(), span());
        let wrong_value =
            Type::synth_path(vec!["wrong".into(), "Value".into()], Vec::new(), span());
        let wrong_pair = product(wrong_value.clone(), wrong_value);

        assert!(
            type_equiv(&owner_pair, &wrong_pair, &env.alias_ctx()),
            "a source path headed by the caller's `owner` alias denotes `wrong.Pair`",
        );
        assert!(
            !type_equiv_state(&owner_pair, &wrong_pair, &env.alias_ctx(), true, false,),
            "the caller's `base` alias must not reinterpret canonical `base.Value` children",
        );
    }

    #[test]
    fn owner_alias_expansion_respects_forall_binder_shadowing() {
        let owner = parse_module("module type_of; type A = .;");
        let poly_body = Type::Forall {
            param: TypeParam {
                name: "A".to_owned(),
                span: span(),
                kind: None,
            },
            body: Box::new(path("A")),
            meta: Meta::new(span()),
        };
        let mut cross_module = HashMap::new();
        cross_module.insert(
            "Poly",
            AliasDef {
                type_params: &[],
                body: &poly_body,
                owner_module: Some(&owner),
            },
        );
        let local = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };

        let unfolded = unfold_top(&path("Poly"), &ctx);

        let Type::Forall { body, .. } = unfolded else {
            panic!("expected forall body, got {unfolded:?}");
        };
        assert!(
            matches!(
                body.as_ref(),
                Type::Path { segments, args, .. }
                    if segments.len() == 1 && segments[0].as_str() == "A" && args.is_empty()
            ),
            "forall binder should not expand to the owner's private alias: {body:?}"
        );
    }

    /// A transparent alias whose body is a qualified, same-leaf nominal must
    /// preserve that exact owner path rather than reinterpreting the leaf as
    /// the caller's alias and reopening expansion.
    #[test]
    fn unfold_top_terminates_on_qualified_self_named_alias() {
        let owner = parse_module("module n; import m as m;");
        // Body `m.A` shares only the presentation leaf with the caller alias.
        let body = qualified("m", "A");
        let mut cross_module = HashMap::new();
        cross_module.insert(
            "A",
            AliasDef {
                type_params: &[],
                body: &body,
                owner_module: Some(&owner),
            },
        );
        let local = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };

        let unfolded = unfold_top(&path("A"), &ctx);

        assert!(
            matches!(
                &unfolded,
                Type::Path { segments, .. }
                    if segments.len() == 2
                        && segments[0].as_str() == "m"
                        && segments[1].as_str() == "A"
            ),
            "self-named alias should unfold once to M's `A` (`m.A`) and stop, \
             got {unfolded:?}"
        );
    }

    #[test]
    fn deep_canonicalization_terminates_on_qualified_self_named_alias() {
        let owner = parse_module("module m; type A = m.A;");
        let alias = owner_alias_def(&owner, "A").expect("owner alias A");
        let local = HashMap::from([("A", alias)]);
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(&owner),
            package: None,
            binder_locals: None,
        };

        let (canonical, identity_canonical) =
            canonicalize_deep_for_comparison(&path("A"), &ctx, false);

        assert!(
            identity_canonical,
            "the exact owner-qualified terminal must have canonical identity"
        );
        assert!(
            matches!(
                &canonical,
                Type::Path { segments, args, .. }
                    if segments.iter().map(|segment| segment.as_str()).eq(["m", "A"])
                        && args.is_empty()
            ),
            "self-named alias should canonicalize once to `m.A` and stop, got {canonical:?}"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn deep_canonicalization_marks_an_actual_alias_cycle_unproven() {
        let a = path("A");
        let b = path("B");
        let unit = Type::Unit {
            meta: Meta::new(span()),
        };
        let cross_module = HashMap::new();
        for (terminal, cyclic) in [(&a, true), (&unit, false)] {
            let local = HashMap::from([
                (
                    "A",
                    AliasDef {
                        type_params: &[],
                        body: &b,
                        owner_module: None,
                    },
                ),
                (
                    "B",
                    AliasDef {
                        type_params: &[],
                        body: terminal,
                        owner_module: None,
                    },
                ),
            ]);
            let ctx = AliasCtx {
                local: &local,
                cross_module: &cross_module,
                type_interner: None,
                source_module: None,
                package: None,
                binder_locals: None,
            };

            // Bypass source validation only to exercise the internal malformed-graph guard.
            let materialized = materialize_alias_root_state_with_ambient(
                &a,
                &ctx,
                AliasAmbientProtected::None,
                false,
            )
            .expect("A resolves to an alias in the supplied context");
            assert_eq!(materialized.defensive_cycle_cutoff, cyclic);

            let (canonical, proven) = canonicalize_deep_for_comparison(&a, &ctx, false);
            assert_eq!(proven, !cyclic);
            if !cyclic {
                assert!(matches!(canonical, Type::Unit { .. }));
            }
        }
    }

    #[cfg(feature = "surface")]
    #[test]
    fn deep_canonicalization_borrows_ambient_binders_and_visits_alias_frontiers_linearly() {
        const AMBIENT_BINDERS: usize = 256;
        const ALIAS_FRONTIERS: usize = 64;

        struct CountingBinders {
            names: HashSet<String>,
            enumerations: std::cell::Cell<usize>,
        }

        impl AliasBinderLookup for CountingBinders {
            fn contains_alias_binder(&self, name: &str) -> bool {
                self.names.contains(name)
            }

            fn for_each_alias_binder(&self, visit: &mut dyn FnMut(&str)) {
                self.enumerations
                    .set(self.enumerations.get().saturating_add(1));
                for name in &self.names {
                    visit(name);
                }
            }
        }

        let owner = parse_module("module owner; type Keep[A] = A;");
        let alias = owner_alias_def(&owner, "Keep").expect("owner alias Keep");
        let local = HashMap::from([("Keep", alias)]);
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(&owner),
            package: None,
            binder_locals: None,
        };
        let binders = CountingBinders {
            names: (0..AMBIENT_BINDERS)
                .map(|index| {
                    if index == 0 {
                        "Rigid".to_owned()
                    } else {
                        format!("Unused{index}")
                    }
                })
                .collect(),
            enumerations: std::cell::Cell::new(0),
        };
        let rigid = path("Rigid");
        let input = (0..ALIAS_FRONTIERS).fold(rigid.clone(), |tail, _| {
            product(path_args("Keep", vec![rigid.clone()]), tail)
        });

        reset_alias_materializer_work();
        let (canonical, identity_canonical) =
            canonicalize_deep_for_comparison_with_binder_lookup(&input, &ctx, &binders, false);
        let work = alias_materializer_work();

        assert!(identity_canonical);
        assert_eq!(binders.enumerations.get(), 0, "the rigid scope was copied");
        assert_eq!(
            work.materializer_roots, ALIAS_FRONTIERS,
            "each transparent frontier must materialize exactly once"
        );
        assert!(
            work.materialize_calls <= ALIAS_FRONTIERS.saturating_mul(2),
            "deep canonicalization revisited alias frontiers: {work:?}"
        );
        fn assert_rigid_leaves(ty: &Type<Lowered>) {
            match ty {
                Type::Product { left, right, .. } => {
                    assert_rigid_leaves(left);
                    assert_rigid_leaves(right);
                }
                Type::Path { segments, args, .. } => assert!(
                    segments
                        .iter()
                        .map(|segment| segment.as_str())
                        .eq(["Rigid"])
                        && args.is_empty(),
                    "an ambient rigid binder was reinterpreted as nominal: {ty:?}"
                ),
                _ => panic!("deep alias expansion produced an unexpected leaf: {ty:?}"),
            }
        }
        assert_rigid_leaves(&canonical);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn deep_canonicalization_layers_forall_binders_over_the_ambient_lookup() {
        let owner = parse_module("module owner; type A = .; type Real = .;");
        let local = HashMap::from([
            ("A", owner_alias_def(&owner, "A").expect("owner alias A")),
            (
                "Real",
                owner_alias_def(&owner, "Real").expect("owner alias Real"),
            ),
        ]);
        let cross_module = HashMap::new();
        let ctx = AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: Some(&owner),
            package: None,
            binder_locals: None,
        };
        let ambient = HashSet::from(["Outer".to_owned()]);
        let input = forall(
            "A",
            product(path("A"), product(path("Outer"), path("Real"))),
        );

        let (canonical, identity_canonical) =
            canonicalize_deep_for_comparison_with_binder_lookup(&input, &ctx, &ambient, false);

        assert!(identity_canonical);
        let Type::Forall { body, .. } = canonical else {
            panic!("deep canonicalization lost the forall shell")
        };
        let Type::Product { left, right, .. } = body.as_ref() else {
            panic!("deep canonicalization lost the forall body")
        };
        assert!(
            matches!(left.as_ref(), Type::Path { segments, args, .. }
                if segments.iter().map(|segment| segment.as_str()).eq(["A"])
                    && args.is_empty()),
            "a lexical forall binder was reinterpreted as an owner alias: {left:?}"
        );
        let Type::Product {
            left: outer,
            right: real,
            ..
        } = right.as_ref()
        else {
            panic!("deep canonicalization lost the ambient/alias siblings")
        };
        assert!(
            matches!(outer.as_ref(), Type::Path { segments, args, .. }
                if segments.iter().map(|segment| segment.as_str()).eq(["Outer"])
                    && args.is_empty()),
            "the ambient rigid binder was reinterpreted as nominal: {outer:?}"
        );
        assert!(
            matches!(real.as_ref(), Type::Unit { .. }),
            "a sibling transparent alias failed to unfold: {real:?}"
        );
    }
}
