//! Abstract syntax tree for Kio.
//!
//! Mirrors the surface forms specified in `specs/prime.md` and
//! `specs/language.md`. Every node carries a [`Span`] for diagnostics.
//! Disambiguating type-parameter references vs alias references
//! (both spelled as identifiers in type position) and qualified-import
//! vs type-member access (both spelled as dotted paths) happens at name
//! resolution, not at parse time.
//!
//! ## Phase-typed AST (Trees That Grow)
//!
//! AST nodes are parametrized by a [`Phase`] marker that gates which
//! surface variants are constructible. Each phase-gated variant carries
//! an `ext: P::ExtName` extension field whose type is `()` when the
//! phase admits the variant and [`Never`] when it
//! doesn't — so the compiler statically refuses to build trees that
//! mix phases incorrectly.
//!
//! The seven exported phase markers are:
//!
//! - [`Surface`] — parser output. All surface variants present:
//!   [`Expr::Tuple`], [`Expr::LabelValue`],
//!   [`Expr::Elaborator`] (internal elaboration forms produced by
//!   parser/desugar recovery paths), [`Expr::UserElaborator`],
//!   [`Expr::Ufcs`], [`Type::LabelSugar`],
//!   [`Item::Labels`] — plus every common variant.
//! - [`Desugared`] — post-`desugar`. Early-lowered expression, item, import,
//!   and parameter-pattern extensions are uninhabited; label- and
//!   typer-directed surface forms remain. `desugar` is the single
//!   `Surface → Desugared` transformer.
//! - [`Lowered`] — post-`label_elab`. Additionally strips
//!   [`Expr::LabelValue`], [`Type::LabelSugar`], and [`Item::Labels`].
//! - [`UncheckedPrime`] — Kio'-shaped but not final-artifact checked.
//!   Compile-time evaluator inputs use this phase while the surface typer is
//!   still filling deferred elaborator sites. It admits exactly the same AST
//!   variants as [`Prime`], but it is an internal phase-boundary artifact, not
//!   a validated backend input.
//! - [`Prime`] — strict Kio' **shape**: the AST variants reachable here are
//!   exactly the ones in [`specs/prime.md`](../../specs/prime.md). The marker is
//!   not itself evidence that checking ran: `prime::lower` constructs this
//!   shape before standalone validation, and full-Kio substitution constructs
//!   it before revalidation. On the full-Kio path, `substitute` replaces each
//!   elaboration site (`Expr::Elaborator`,
//!   `Expr::UserElaborator`, `Expr::Ufcs`) with its explicit Kio' tree, so every
//!   surface-only variant carries [`Never`]:
//!   - Internal elaboration forms → the chosen rearrangement of
//!     `__pair__` / `__fst__` / `__snd__` / `__left__` /
//!     `__right__` / `__either__` calls,
//!   - user-defined elaborator calls → the checked Kio' tree returned
//!     by the elaborator implementation.
//!
//!   The backend-facing pipeline validates the assembled package before
//!   exposing Prime to backends. The Kio' emitter therefore sees only validated
//!   Kio'-shaped trees; every surface-only variant arm in that emitter is a
//!   `match *ext {}` proof over [`Never`]. The
//!   contract "Prime contains exactly the AST nodes Kio' specifies"
//!   is the load-bearing invariant of this stage. The standalone checker may
//!   record Kio'-admitted call/lambda completions in a boundary-local table,
//!   but bakes them into the AST before returning `Prime`.
//! - [`Enriched`] — structural-recovery IR. Intrinsic chains are recovered into
//!   n-ary structural nodes, and the backend-neutral optimizer consumes and
//!   returns this phase.
//! - [`Routed`] — post-`recover_to_low`. Calls and value paths carry their
//!   resolved backend-routing classification; capability passes annotate this
//!   phase before host emission.
//!
//! Dot-splice calls (`r.>f(args)`, `r.>>f(args)`, `f(args).<r`,
//! `f(args).<<r`) survive through `Lowered`. The typer resolves the
//! named callee, inserts the receiver into the resolved value-argument
//! stream, records the equivalent prefix call or user-elaborator
//! expansion, and `substitute` erases the [`Expr::Ufcs`] before Prime.
//!
//! The `Surface` and `Lowered` phases bracket the typer; the
//! `Desugared` phase between them tightens the `Tuple` / `FnPlaceholder`
//! / `Op` / `LiteralAlias` extension types to [`Never`]
//! (so the desugar pass becomes phase-changing); `Lowered` further
//! tightens the `Labels` / `LabelValue` / `TypeLabelSugar` extensions;
//! `UncheckedPrime` / `Prime` then tighten the `ExprElab` extension
//! (covering `Elaborator` / `UserElaborator` / field access
//! and update) to [`Never`] after the typer's elaboration substitutes
//! them out, closing the door on every surface-only variant.

use crate::span::Span;

// ---- Doc-comment attachment -----------------------------------------------

/// A contiguous run of `///` doc-comment lines attached to a top-level
/// definition or to the module itself. The payload is the *joined text*
/// of the lines (each line's `///` prefix and one optional leading space
/// already stripped by the lexer). Lines are separated by `\n` in the
/// stored value; a blank doc-comment line is stored as an empty segment.
///
/// Markdown / Kiodoc directive parsing is **not** done at this layer —
/// this is raw text. Later Kiodoc sessions will consume and validate the
/// payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocComment {
    /// The joined text of the doc-comment lines, one per `\n`. Each
    /// element is the payload of one `/// …` line (leading `///` and
    /// one optional space already stripped).
    pub lines: Vec<String>,
    /// Source span covering the entire doc-comment block, from the
    /// leading `///` of the first line to the end of the last line.
    pub span: Span,
}

// ---- Uninhabited witness with serde derive --------------------------------

/// Uninhabited witness for surface-only variant extensions at every
/// phase that rejects them. Replaces `std::convert::Infallible` in
/// the phase associated-type definitions so the AST's
/// `serde::Serialize` / `serde::Deserialize` derives flow through
/// generically — `Infallible` is defined in `std` and does not
/// implement `serde::Serialize`, and the orphan rule keeps kio-rs
/// from adding the impl externally, so a hand-rolled local
/// uninhabited type is needed instead.
///
/// The enum has **zero variants**, exactly like `Infallible`, so any
/// value of type `Never` is statically impossible to construct;
/// pattern-matching on one discharges via `match v {}` (the empty
/// match). Cross-phase walkers that exit a surface-only arm via
/// `match *ext {}` continue to typecheck — `Never` and `Infallible`
/// have identical never-type semantics. The serde derives are
/// derivable: a zero-variant enum has no data to serialize, and
/// deserialization always errors at runtime (no valid input shape
/// produces a `Never`), which is exactly the behaviour we want for
/// a witness that signals "this variant is uninhabited at this
/// phase."
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Never {}

impl From<std::convert::Infallible> for Never {
    fn from(v: std::convert::Infallible) -> Self {
        match v {}
    }
}

impl From<Never> for std::convert::Infallible {
    fn from(v: Never) -> Self {
        match v {}
    }
}

// ---- Node identity --------------------------------------------------------

/// Node IDs synthesized after parsing occupy the upper half of the ID
/// space, disjoint from the parser's monotonically increasing IDs.
#[cfg(feature = "surface")]
pub(crate) const SYNTHESIZED_NODE_ID_START: u64 = 1 << 63;

/// Stable identity for AST nodes keyed by later phases, including
/// elaboration-bearing expressions and `Expr::RecOrder`. Parsed nodes use a
/// per-parse-invocation counter; desugaring issues synthesized IDs from the
/// disjoint range above. IDs are preserved unchanged across `Clone` and phase
/// rebuilds. The value has no source-position meaning — it's a key, not a span.
///
/// Using an explicit `NodeId` here, rather than the previous
/// pointer-as-`usize` keying, lets surface elaboration clone source subtrees
/// into recorded replacements without breaking lookup: clones share `NodeId`
/// with their origin, so when substitution walks the cloned subtree the
/// `Elaborations` lookup finds the recorded entry.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct NodeId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExpressionOccurrenceId(std::num::NonZeroU64);

/// Transient correspondence between a checked expression and its completed facts.
/// Structural AST comparisons and artifacts do not observe this correspondence.
#[derive(Clone, Copy, Default)]
pub struct ExpressionOccurrence(Option<ExpressionOccurrenceId>);

impl ExpressionOccurrence {
    pub fn assigned_key(self) -> Option<ExpressionOccurrenceId> {
        self.0
    }
    pub fn fresh() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .expect("expression occurrence identity exhausted");
        Self(Some(ExpressionOccurrenceId(
            std::num::NonZeroU64::new(id).expect("expression occurrence identities are nonzero"),
        )))
    }

    pub fn key(self) -> ExpressionOccurrenceId {
        self.0
            .expect("checker publication requires an assigned expression occurrence")
    }

    pub fn is_assigned(self) -> bool {
        self.0.is_some()
    }
}

impl std::fmt::Debug for ExpressionOccurrence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExpressionOccurrence")
    }
}

impl PartialEq for ExpressionOccurrence {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for ExpressionOccurrence {}

pub trait ExpressionOccurrenceCarrier: Clone + Copy + Default + std::fmt::Debug + Eq {
    fn fresh() -> Self;
}

impl ExpressionOccurrenceCarrier for ExpressionOccurrence {
    fn fresh() -> Self {
        Self::fresh()
    }
}

impl ExpressionOccurrenceCarrier for () {
    fn fresh() -> Self {}
}

#[derive(Debug, Clone, Copy)]
pub struct ExpressionSite {
    pub id: ExpressionOccurrenceId,
    pub span: Span,
}

// ---- Type-inference goal identity ----------------------------------------

/// Capability namespace of one transient inference store.
///
/// Ordinary compiler stores carry a process-unique nonce. Isolated evaluator
/// scratch uses a structurally distinct namespace whose goal handles are
/// confined behind an API that can return only goal-free values. Test scopes
/// start unstamped and are attached to the store under test at their first
/// boundary.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TypeGoalStoreIdentity(u64);

impl TypeGoalStoreIdentity {
    #[cfg(any(feature = "surface", test))]
    pub(crate) const UNSTAMPED_TEST: Self = Self(0);
    #[cfg(feature = "surface")]
    pub(crate) const ISOLATED: Self = Self(u64::MAX);

    #[cfg(feature = "surface")]
    pub(crate) const fn process(nonce: u64) -> Self {
        assert!(nonce != Self::UNSTAMPED_TEST.0 && nonce != Self::ISOLATED.0);
        Self(nonce)
    }

    #[cfg(feature = "surface")]
    pub(crate) const fn is_process(self) -> bool {
        self.0 != Self::UNSTAMPED_TEST.0 && self.0 != Self::ISOLATED.0
    }
}

const _: () = assert!(std::mem::size_of::<TypeGoalStoreIdentity>() == std::mem::size_of::<u64>());

impl std::fmt::Debug for TypeGoalStoreIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TypeGoalStoreIdentity(..)")
    }
}

/// Identity of one finite inference domain.
///
/// Kept opaque outside the crate so an open type cannot acquire identity from
/// a source spelling, pointer, or untyped integer. The typechecker's goal store
/// is the only code that allocates domains. `store_nonce` is a process-local
/// capability discriminator: it prevents equal local indices from different
/// stores from aliasing, is never serialized or displayed, and must be erased
/// before Prime. It is not compilation data and cannot affect emitted output.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TypeGoalDomain {
    store: TypeGoalStoreIdentity,
    index: u32,
}

const _: () = assert!(std::mem::size_of::<TypeGoalDomain>() == std::mem::size_of::<(u64, u32)>());

#[cfg(feature = "surface")]
impl TypeGoalDomain {
    pub(crate) const fn new(store: TypeGoalStoreIdentity, index: u32) -> Self {
        Self { store, index }
    }

    pub(crate) const fn index(self) -> u32 {
        self.index
    }

    pub(crate) fn belongs_to(self, store: TypeGoalStoreIdentity) -> bool {
        self.store == store
    }
}

impl std::fmt::Debug for TypeGoalDomain {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TypeGoalDomain(..)")
    }
}

/// One owner within a [`TypeGoalDomain`].
///
/// Carrying the domain token structurally prevents two independently planned
/// calls from aliasing merely because they allocated the same owner-local
/// index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TypeGoalOwner {
    domain: TypeGoalDomain,
    index: u32,
}

#[cfg(feature = "surface")]
impl TypeGoalOwner {
    pub(crate) const fn new(domain: TypeGoalDomain, index: u32) -> Self {
        Self { domain, index }
    }

    pub(crate) const fn domain(self) -> TypeGoalDomain {
        self.domain
    }

    pub(crate) const fn index(self) -> u32 {
        self.index
    }
}

/// One slot within a [`TypeGoalOwner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TypeGoalSlot(u32);

#[cfg(feature = "surface")]
impl TypeGoalSlot {
    pub(crate) const fn from_index(index: u32) -> Self {
        Self(index)
    }

    pub(crate) const fn index(self) -> u32 {
        self.0
    }
}

/// Opaque identity of an internal type-inference goal.
///
/// The pair keeps ownership and slot identity structurally distinct. It is
/// carried only by the phase-gated [`Type::Goal`] form and has no surface
/// spelling or persistent-artifact meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeGoalRef {
    owner: TypeGoalOwner,
    slot: TypeGoalSlot,
}

impl TypeGoalRef {
    #[cfg(feature = "surface")]
    pub(crate) const fn new(owner: TypeGoalOwner, slot: TypeGoalSlot) -> Self {
        Self { owner, slot }
    }

    #[cfg(feature = "surface")]
    pub(crate) const fn owner(self) -> TypeGoalOwner {
        self.owner
    }

    #[cfg(feature = "surface")]
    pub(crate) const fn slot(self) -> TypeGoalSlot {
        self.slot
    }

    #[cfg(feature = "surface")]
    pub(crate) const fn domain(self) -> TypeGoalDomain {
        self.owner.domain()
    }

    #[cfg(test)]
    pub(crate) const fn for_test(domain: u32, owner: u32, slot: u32) -> Self {
        Self {
            owner: TypeGoalOwner {
                domain: TypeGoalDomain {
                    store: TypeGoalStoreIdentity::UNSTAMPED_TEST,
                    index: domain,
                },
                index: owner,
            },
            slot: TypeGoalSlot(slot),
        }
    }
}

impl serde::Serialize for TypeGoalRef {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        Err(serde::ser::Error::custom(
            "transient type-inference goals cannot be serialized",
        ))
    }
}

impl<'de> serde::Deserialize<'de> for TypeGoalRef {
    fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Err(serde::de::Error::custom(
            "transient type-inference goals cannot be deserialized",
        ))
    }
}

// ---- Path segments --------------------------------------------------------

/// One segment of a dotted path (`a` in `a.b.c`). Carries the
/// segment's text and its source span so diagnostics can pin to the
/// specific segment that errored — e.g. an unresolved `path` in
/// `bad.path` should point at `path`, not at the full `bad.path`
/// range.
///
/// Used by every path-like AST position: [`Expr::Path::segments`],
/// [`Type::Path::segments`], [`ModulePath::segments`],
/// [`LexicalCallablePath`], and [`Expr::Ufcs::callee_segments`]. Equality compares only the
/// `name`; the `span` is informational metadata for diagnostics.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PathSegment {
    pub name: String,
    pub span: Span,
}

impl PartialEq for PathSegment {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

/// Surface spelling of a UFCS-style syntactic call splice.
///
/// The parser stores the value being inserted in [`Expr::Ufcs::receiver`]
/// for all four forms. The flavor records where the callable was written
/// and where that inserted value lands in the eventual call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum UfcsFlavor {
    /// `r.>f(args)` -> `f(r, args)`.
    ReceiverFirst,
    /// `r.>>f(args)` -> `f(args, r)`.
    ReceiverLast,
    /// `f(args).<r` -> `f(args, r)`.
    ArgumentLast,
    /// `f(args).<<r` -> `f(r, args)`.
    ArgumentFirst,
}

impl UfcsFlavor {
    pub fn token(self) -> &'static str {
        match self {
            Self::ReceiverFirst => ".>",
            Self::ReceiverLast => ".>>",
            Self::ArgumentLast => ".<",
            Self::ArgumentFirst => ".<<",
        }
    }

    pub fn inserts_first(self) -> bool {
        matches!(self, Self::ReceiverFirst | Self::ArgumentFirst)
    }

    pub fn callee_on_right(self) -> bool {
        matches!(self, Self::ReceiverFirst | Self::ReceiverLast)
    }
}

impl Eq for PathSegment {}

impl std::hash::Hash for PathSegment {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl PartialEq<str> for PathSegment {
    fn eq(&self, other: &str) -> bool {
        self.name == other
    }
}

impl PartialEq<&str> for PathSegment {
    fn eq(&self, other: &&str) -> bool {
        self.name == *other
    }
}

impl PartialEq<String> for PathSegment {
    fn eq(&self, other: &String) -> bool {
        &self.name == other
    }
}

impl PartialEq<PathSegment> for str {
    fn eq(&self, other: &PathSegment) -> bool {
        self == other.name.as_str()
    }
}

impl PartialEq<PathSegment> for &str {
    fn eq(&self, other: &PathSegment) -> bool {
        *self == other.name.as_str()
    }
}

impl PartialEq<PathSegment> for String {
    fn eq(&self, other: &PathSegment) -> bool {
        self == &other.name
    }
}

impl std::ops::Deref for PathSegment {
    type Target = str;
    fn deref(&self) -> &str {
        &self.name
    }
}

impl std::borrow::Borrow<str> for PathSegment {
    fn borrow(&self) -> &str {
        &self.name
    }
}

impl AsRef<str> for PathSegment {
    fn as_ref(&self) -> &str {
        &self.name
    }
}

impl std::fmt::Display for PathSegment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

impl PathSegment {
    pub fn new(name: impl Into<String>, span: Span) -> Self {
        Self {
            name: name.into(),
            span,
        }
    }

    /// Build a `PathSegment` for a synthesized position (no source
    /// span available). The span defaults to the supplied fallback.
    /// Used by synthesizers that mint paths from non-source positions
    /// (the typer's elaboration trees, `elaborator`'s intrinsic factories,
    /// the test module).
    pub fn synth(name: impl Into<String>, span: Span) -> Self {
        Self::new(name, span)
    }

    /// Borrow the segment's name as a `&str`. Equivalent to `&self.name`
    /// but available as a method for ergonomic use with iterator chains.
    pub fn as_str(&self) -> &str {
        &self.name
    }
}

// ---- Phase markers -------------------------------------------------------

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
pub enum Purity {
    #[default]
    Impure,
    Pure,
}

impl Purity {
    pub fn is_pure(self) -> bool {
        matches!(self, Purity::Pure)
    }

    pub fn as_modifier(self) -> Option<&'static str> {
        match self {
            Purity::Impure => None,
            Purity::Pure => Some("pure"),
        }
    }
}

pub trait FnPurityExt {
    fn is_pure_fn(&self) -> bool;
}

impl FnPurityExt for Purity {
    fn is_pure_fn(&self) -> bool {
        self.is_pure()
    }
}

impl FnPurityExt for () {
    fn is_pure_fn(&self) -> bool {
        false
    }
}

/// The container shape for a literal's `(Type)` annotation, which
/// narrows at the Lowered → Kio' boundary. `Option<Type<P>>` through
/// the surface phases ([`Surface`] / [`Desugared`] / [`Lowered`]) —
/// where the annotation is optional syntax the user may omit — and
/// through [`UncheckedPrime`], the evaluator-facing Kio'-shaped phase where a
/// literal whose host type the typer has not yet pinned is a still-explicit
/// compile-time hole (`None`). A bare (mandatory) `Type<P>` in the
/// boundary-local [`crate::pass::substitute::PrePrime`] staging phase and the
/// fixed-annotation Kio'-shaped phases ([`Prime`] / [`Enriched`] / [`Routed`]).
/// The phase marker describes representation shape; validation is established
/// by the pipeline that produced the artifact. Final module
/// substitution pins every literal before constructing `PrePrime`.
/// Eval-facing staging may instead carry the private
/// [`RESIDUAL_LITERAL_TYPE_HOLE`] sentinel, which the consuming
/// PrePrime → UncheckedPrime conversion restores to `None`.
/// `specs/grammar.md` § Kio' grammar mandates a `(Type)` on every
/// serialized literal (`LiteralCall ::= LITERAL '(' Type ')'`), so a
/// bare serialized Kio' literal is statically unrepresentable.
pub trait LitAnnotationExt<P: Phase> {
    /// Optional view of the resolved annotation type. The
    /// `Option<Type<P>>` impl returns the stored option; the bare
    /// `Type<P>` impl always returns `Some`.
    fn as_type(&self) -> Option<&Type<P>>;
    /// Mutable view of the resolved annotation type.
    fn as_type_mut(&mut self) -> Option<&mut Type<P>>;
    /// Build the container from an optional resolved type. The
    /// `Option<Type<P>>` impl stores it verbatim; the bare `Type<P>`
    /// impl requires `Some` — a `None` means an upstream pass tried to
    /// construct a mandatory-annotation phase without either a resolved
    /// host type or the eval-staging sentinel, a compiler-contract
    /// violation that panics with `span` context.
    fn build(ty: Option<Type<P>>, span: Span) -> Self;
}

impl<P: Phase> LitAnnotationExt<P> for Option<Type<P>> {
    fn as_type(&self) -> Option<&Type<P>> {
        self.as_ref()
    }
    fn as_type_mut(&mut self) -> Option<&mut Type<P>> {
        self.as_mut()
    }
    fn build(ty: Option<Type<P>>, _span: Span) -> Self {
        ty
    }
}

impl<P: Phase> LitAnnotationExt<P> for Type<P> {
    fn as_type(&self) -> Option<&Type<P>> {
        Some(self)
    }
    fn as_type_mut(&mut self) -> Option<&mut Type<P>> {
        Some(self)
    }
    fn build(ty: Option<Type<P>>, span: Span) -> Self {
        ty.unwrap_or_else(|| {
            unreachable!(
                "literal at {span:?} reached a mandatory-annotation phase without a resolved \
                 `(Type)` annotation or the eval-staging sentinel; substitution must pin or \
                 explicitly residualize every literal before crossing that boundary"
            )
        })
    }
}

/// Reserved type-name for a literal whose host type eval-facing substitution
/// could not pin (a genuinely ambiguous or context-free literal on a
/// compile-time-eval path). The angle brackets make it an invalid Kio'
/// identifier, so it never collides with a user type. It exists only in the
/// substitution pass's private staging phase. Conversion to `UncheckedPrime`
/// strips it back to the still-explicit compile-time hole (`None`), and it can
/// never inhabit a validated `Prime` module, whose literal annotation is
/// mandatory.
pub(crate) const RESIDUAL_LITERAL_TYPE_HOLE: &str = "<literal-hole>";

/// Build the residual literal-type hole (see [`RESIDUAL_LITERAL_TYPE_HOLE`]).
/// Only the `surface`-gated substitution pass produces one;
/// `is_residual_literal_type_hole` (which strips it) is always compiled.
#[cfg(feature = "surface")]
pub(crate) fn residual_literal_type_hole<P: Phase>(span: Span) -> Type<P> {
    Type::synth_path(
        vec![RESIDUAL_LITERAL_TYPE_HOLE.to_owned()],
        Vec::new(),
        span,
    )
}

/// Whether `ty` is the residual literal-type hole.
pub(crate) fn is_residual_literal_type_hole<P: Phase>(ty: &Type<P>) -> bool {
    matches!(
        ty,
        Type::Path { segments, args, .. }
            if args.is_empty()
                && segments.len() == 1
                && segments[0].name == RESIDUAL_LITERAL_TYPE_HOLE
    )
}

/// Convert a literal's `(Type)` annotation across phases, dropping the
/// residual literal-type hole ([`RESIDUAL_LITERAL_TYPE_HOLE`]) to the
/// bare (`None`) compile-time hole. The hole is only ever produced in the
/// eval path's private substitution staging phase; every other conversion
/// carries a real type (or a genuine `None`) verbatim.
fn convert_lit_annotation<P, Q>(annotation: &P::LitAnnotation, span: Span) -> Q::LitAnnotation
where
    P: Phase<TypeLabelSugar = Never>,
    Q: Phase,
{
    let resolved = annotation
        .as_type()
        .filter(|t| !is_residual_literal_type_hole(t))
        .map(convert_type::<P, Q>);
    <Q::LitAnnotation as LitAnnotationExt<Q>>::build(resolved, span)
}

/// Marker trait for AST phases. Each associated type names the
/// per-variant extension whose Rust type controls whether the variant
/// is constructible at this phase: `()` admits the variant,
/// [`Never`] forbids it (the compiler refuses to
/// construct a value, and `match` arms over a forbidden variant
/// discharge via `match ext {}`).
///
/// The associated-type bounds carry the trait bounds that AST nodes
/// derive (`Clone`, `Debug`, `PartialEq`, `Eq`, `Serialize`,
/// `DeserializeOwned`) so the derives flow through generically. The
/// serde bounds let the enriched-IR cache round-trip a
/// `Module<Enriched>` through `postcard`; every front-end phase
/// instantiates the witnesses with serde-derivable concrete types
/// (`()`, `bool`, `usize`, `NodeId`, `Vec<Trivia>`, `Never`)
/// so the bound is satisfied without per-phase impls.
pub trait Phase: Sized {
    /// Neutral block calls project to ordinary Surface expressions before
    /// desugaring; later phases cannot retain their syntax or item lists.
    type ExprBlockSyntax: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    type ExpressionOccurrence: ExpressionOccurrenceCarrier;
    type ExprTuple: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// `Expr::FnPlaceholder` (`.x. { x1 }`) is a surface-only form. Before
    /// operator folding, source-local classification replaces its references
    /// with private sentinels; desugaring allocates ordinary hygienic parameters
    /// and lowers the form to `Expr::FnExpr`. `Surface`
    /// admits the carrier (witness `()`); later phases forbid it (witness
    /// [`Never`]).
    type ExprFnPlaceholder: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// `Expr::OpChain` is a Surface-only placeholder produced by
    /// the parser at every operator-usage site (both normal-op
    /// applications and variadic-op bracket literals). The
    /// operator-fold pass — running before desugar and ordinary name
    /// resolution — looks each chain up against the precomputed package
    /// operator scope (module-local `op`s plus cross-module operator-pattern
    /// imports) and substitutes the placeholder with the resolved `Expr::Call`.
    /// `Surface` admits the variant
    /// (witness `()`); `Desugared` / `Lowered` / `Prime` forbid
    /// it (witness [`Never`]).
    type ExprOpChain: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    type ExprLabelValue: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// `Expr::RowLet` (`.{f, g as x} = row; rest`) is surface-only
    /// statement syntax. Desugar lowers it to ordinary let-bindings
    /// plus field-access elaborators so later phases never observe
    /// row-let binders directly.
    type ExprRowLet: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// Witness for the elaboration-bearing `Expr` variants whose
    /// type-checking semantics depend on a side table the typer
    /// records during synthesis: `Expr::Elaborator`,
    /// and `Expr::UserElaborator`.
    ///
    /// All these variants share the same witness shape per phase:
    /// `NodeId` at `Surface` / `Desugared` / `Lowered` (the side-
    /// table key that the typer's `Elaborations` table consumes),
    /// [`Never`] at `Prime` (every elaboration-bearing variant is
    /// uninhabited there because the substitution pass swaps the
    /// recorded `Call` shape in at the `Lowered → Prime` boundary).
    /// Unifying the associated types into one removed the per-variant
    /// bookkeeping noise while keeping the typer's "variant is
    /// inhabited iff the side table needs a key" rule.
    type ExprElab: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// `rec <callee>(...)` is the mandatory marker for recursive calls
    /// inside a surface `rec(loop)` group. The desugar pass eliminates
    /// it while lowering the enclosing group to an ordinary `loop(...)`
    /// call. `Surface` admits it; every later phase rejects it.
    type ExprRecCall: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// Compiler-private recursive-CPS ordering/type-flow carrier introduced
    /// by `desugar` and consumed at the `Lowered → Prime` boundary. It delays
    /// one generated binding until the ordinary checker resolves its exact
    /// disposition in the carrier's declared direction. Surface source and
    /// every Kio' phase forbid the form.
    type ExprRecOrder: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// `Expr::Ufcs` (`r.>f(args)`, `r.>>f(args)`, `f(args).<r`,
    /// `f(args).<<r`, `r.>iso!(T)`) is a dot-splice dispatch form.
    /// Surface, Desugared, and Lowered carry its parser-issued
    /// [`NodeId`] so the typer can record the equivalent prefix call
    /// or user-elaborator expansion at the Lowered -> Prime boundary.
    /// Prime forbids the variant (witness [`Never`]).
    type ExprUfcs: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// Witness for the seven **enriched** `Expr` variants the
    /// structural-recovery pass introduces — `Expr::EnrichedTuple`,
    /// `Expr::EnrichedProject`, `Expr::EnrichedInject`,
    /// `Expr::EnrichedMatch`, `Expr::EnrichedConditional`,
    /// `Expr::EnrichedRecord`, and `Expr::EnrichedFieldGet`. These are the
    /// structural forms recovered from right-leaning intrinsic chains after
    /// typechecking. They are uninhabited at every front-end phase (`Surface` /
    /// `Desugared` / `Lowered` / `Prime`, witness [`Never`]) and inhabited at
    /// [`Enriched`] and [`Routed`] (witness `()`). The seven variants share one
    /// witness for the same
    /// reason `ExprElab` is shared by the elaboration-bearing
    /// variants: it collapses the per-variant bookkeeping while
    /// keeping the "variant is inhabited iff this phase recovered
    /// it" rule.
    type ExprEnriched: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// Witness for the synthesized-type fields on the enriched
    /// variants (`EnrichedTuple::synth_ty`,
    /// `EnrichedInject::synth_ty`, `EnrichedRecord::synth_ty`,
    /// `EnrichedConditional::result_ty`,
    /// `EnrichedMatch::scrutinee_ty` / `result_ty`,
    /// `EnrichedProject::target_ty`,
    /// `EnrichedFieldGet::target_ty`). The structural-recovery pass
    /// carries these over from the recovered intrinsic call's type
    /// args so typed backends can pick the right shape struct /
    /// enum without re-typechecking. The witness is `Type<Enriched>` at
    /// [`Enriched`], `Type<Routed>` at [`Routed`], and [`Never`] at every
    /// front-end phase (mirroring `ExprEnriched`'s gating role).
    type ExprEnrichedSynthTy: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// Witness for the `(Type)` annotation on the literal `Expr`
    /// variants (`StrLit` / `IntLit` / `FloatLit` / `BoolLit`).
    /// `Option<Type<P>>` through the surface phases ([`Surface`] /
    /// [`Desugared`] / [`Lowered`]) — where the annotation is optional
    /// syntax — and through [`UncheckedPrime`], where an unpinned
    /// literal is a still-explicit compile-time hole; a bare (mandatory)
    /// `Type<P>` in substitution's private `PrePrime` staging phase and the
    /// fixed-annotation Kio'-shaped phases ([`Prime`] / [`Enriched`] /
    /// [`Routed`]).
    /// `PrePrime` carries either the resolved type or the private eval-hole
    /// sentinel; the fixed-annotation phases always carry the type required by
    /// the Kio' grammar. The phase marker fixes this representation but does not
    /// itself prove that the standalone checker ran. See [`LitAnnotationExt`].
    type LitAnnotation: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + LitAnnotationExt<Self>
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// Witness for the **Low-IR** `Expr` variants the
    /// [`crate::pass::recover_to_low::lower`] pass introduces:
    /// `Expr::LowHostCall`, `Expr::LowModuleCall`,
    /// `Expr::LowQualifiedModuleCall`,
    /// `Expr::LowQualifiedNewtypeMember`, `Expr::LowNewtypeCtor`,
    /// `Expr::LowNewtypeProj`, `Expr::LowClosureCall`,
    /// `Expr::LowIndirectCall`, `Expr::LowTypeApplication`,
    /// `Expr::LowAbsurdCall`,
    /// `Expr::LowCpsProjectorApply`, `Expr::LowBoundRef`,
    /// `Expr::LowHostFnValueRef`, `Expr::LowModuleFnValueRef`.
    ///
    /// These pre-classified call / value-position nodes carry the
    /// resolution context — host vs module, mangled name, callee
    /// signature — directly on the variant, so per-backend
    /// lowerings drop to syntactic templating over the variant's
    /// shape instead of re-walking the package's `scope` /
    /// `selective_imports` / `qualified_imports` tables at every call site.
    /// Inhabited only at [`Routed`] (witness `()`); every other phase
    /// (surface, typed, enriched) sets it to [`Never`].
    type ExprLow: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// Witness for the surface-residual `Expr::Call` / `Expr::Path` variants —
    /// the two forms the
    /// [`crate::pass::recover_to_low::lower`] pass classifies away. At every
    /// pre-`Routed` phase the witness is `()` (the variants are
    /// constructible exactly as before). At [`Routed`] the witness is
    /// [`Never`]: the lower pass has rewritten every call and path into one of
    /// the `Expr::Low*` variants, and the type
    /// system pushes "did we forget a case" to a compile-time
    /// exhaustiveness error inside the Routed-consuming code.
    type ExprResolved: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    type TypeLabelSugar: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// `Type::Infer` (`_` placeholder) is surface-only. Lowered checking records
    /// each solution and substitution materializes it at the Prime boundary;
    /// `Prime` therefore rejects the variant. `Surface` / `Desugared` /
    /// `Lowered` admit it (witness `()`); `Prime` forbids it
    /// (witness [`Never`]).
    type TypeInfer: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// Internal type-inference goals. [`Lowered`] admits this TTG extension
    /// while the typer is planning one finite inference domain; every source,
    /// persistent, evaluator, and backend-facing phase forbids it.
    type TypeGoal: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    type ItemLabels: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// `equiv` is a surface-only declaration form: it survives
    /// `desugar` / `label_elab` so the typer can validate the body, but
    /// the substitution pass that drives `Lowered → Prime` filters
    /// `Equiv` items out (they have no execution semantics in the
    /// build artifact). So `Surface` / `Desugared` / `Lowered` admit
    /// the variant (witness `()`) and `Prime` forbids it (witness
    /// [`Never`]).
    type ItemEquiv: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// `pub elab name : CallType { impl implementation; };` is
    /// surface-only compile-time metadata. It survives through
    /// `Lowered` so the typer can validate and use it, then is
    /// filtered before `Prime`.
    type ItemElaborator: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// `op` is a surface-only declaration form: the desugar
    /// pass consumes it (registers operator bindings, then drops
    /// the item) so every later phase rejects it via
    /// [`Never`]. Only `Surface` admits it.
    type ItemOp: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// `rec(loop) fn ...` / `rec(loop) { ... }` is surface-only.
    /// It lowers to ordinary `fn` items before name resolution.
    type ItemRecGroup: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// `literal name = 1;` is a surface-only declaration form:
    /// the desugar pass substitutes unshadowed references and drops
    /// the item, so every later phase makes the variant statically
    /// uninhabited via [`Never`].
    type ItemLiteralAlias: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// The destructuring-pattern extension on a value [`Param`].
    /// `Option<ParamPattern>` at `Surface` (the parser populates it
    /// for `(a: A, b: B)` / `name: (a: A, b: B)` parameters; `None`
    /// for ordinary `name: T` params and non-parameter `let` sites);
    /// `()` at every later
    /// phase, where the desugar pass has already rewritten the
    /// pattern into an outer `name: T` slot plus generated product
    /// projections around the body. The `Default` bound lets cross-phase rebrands mint
    /// the no-pattern witness without explicit values at every
    /// construction site.
    type ParamPatternExt: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + Default
        + serde::Serialize
        + serde::de::DeserializeOwned;

    /// Witness for `FnDef.ret_elided` — the surface-only flag
    /// recording whether the user wrote `-> .` explicitly or
    /// elided the return-type annotation. `bool` at [`Surface`] /
    /// [`Desugared`] / [`Lowered`] where the formatter still
    /// needs to round-trip the user's spelling; `()` at [`Prime`]
    /// and [`Enriched`] where the back-end consumes Kio'-shape
    /// alone (every surviving fn type carries an explicit return
    /// type). [`PhaseBridge`] handles the value at conversion
    /// time — sideways transitions preserve the bool, the
    /// [`Lowered`] → [`Prime`] transition discards it, and the
    /// [`Prime`] → [`Surface`] embed reconstructs the canonical
    /// `false` (always write out `-> .`).
    type FnDefRetElided: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + PhaseBridge<bool>
        + PhaseBridge<()>
        + serde::Serialize
        + serde::de::DeserializeOwned;

    /// Ordinary-function purity. `Surface` through [`Prime`] retain the
    /// modifier so source typing and final Kio' validation enforce the same
    /// body contract, including after elaborator substitution. Post-Prime
    /// runtime phases narrow it to `()` after that validation has completed.
    type FnPurity: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + FnPurityExt
        + PhaseBridge<Purity>
        + PhaseBridge<()>
        + serde::Serialize
        + serde::de::DeserializeOwned;

    /// Source trivia attached to a single AST position (e.g., the
    /// run of comments immediately before a `let`, a `match!` clause,
    /// or a `term` keyword). `Vec<crate::pass::lexer::Trivia>` at [`Surface`]
    /// where `kio fmt` reads it; `()` at every later phase, where
    /// no pass needs it. The `Default` bound lets the
    /// phase-changing walkers construct it without manufacturing
    /// data.
    type LeadingTrivia: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + Default
        + serde::Serialize
        + serde::de::DeserializeOwned;

    /// Capability annotations attached to an [`Expr::FnExpr`]. Pre-
    /// [`Routed`] phases have nothing to annotate (`()`); at
    /// [`Routed`] the per-module [`crate::pass::capabilities`] pass fills
    /// in a [`Capabilities`] bundle (`captured_from`).
    /// The `Default` bound lets the resolution-lowering pass mint
    /// the safe default (empty captures) before the annotation pass
    /// refines it.
    type FnExprCapabilities: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + Default
        + serde::Serialize
        + serde::de::DeserializeOwned;

    /// Capability annotations attached to a [`Type::Function`].
    /// Pre-[`Routed`] phases have nothing to annotate (`()`); at
    /// [`Routed`] the per-module
    /// [`crate::pass::capabilities::annotate_lifetime`] pass fills in a
    /// [`FnTypeCapabilities`] bundle (`lifetime`). The `Default`
    /// bound lets the resolution-lowering pass mint the safe
    /// default (`Lifetime::Heap`) before the annotation pass
    /// refines it. Sibling annotation to
    /// [`Self::FnExprCapabilities`].
    type FnTypeCapabilities: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + Default
        + serde::Serialize
        + serde::de::DeserializeOwned;
}

/// Cross-phase conversion for a phase-specific field. Each
/// implementation translates a value from a source phase's
/// representation (`F`) to the target phase's. The semantic is
/// "carry over the datum, or reconstruct the canonical default if the source
/// phase didn't store it." Used
/// by [`convert_type`] / [`convert_fn_def`] to route fields
/// whose type narrows at the [`Lowered`] → [`Prime`] boundary.
pub trait PhaseBridge<F> {
    fn bridge(from: F) -> Self;
}

// Identity bridges — sideways conversions between phases that
// share the same field type.
impl PhaseBridge<bool> for bool {
    fn bridge(from: bool) -> Self {
        from
    }
}
impl PhaseBridge<usize> for usize {
    fn bridge(from: usize) -> Self {
        from
    }
}
impl PhaseBridge<()> for () {
    fn bridge(_: ()) -> Self {}
}
impl PhaseBridge<Purity> for Purity {
    fn bridge(from: Purity) -> Self {
        from
    }
}

// Narrowing bridges. `bool` / `usize` are discarded at the [`Prime`]
// boundary; `Purity` is discarded only after validated Prime enters a
// runtime-codegen phase.
impl PhaseBridge<bool> for () {
    fn bridge(_: bool) -> Self {}
}
impl PhaseBridge<usize> for () {
    fn bridge(_: usize) -> Self {}
}
impl PhaseBridge<Purity> for () {
    fn bridge(_: Purity) -> Self {}
}

// Defaulting bridges used when a later phase no longer stores a field. The
// canonical default is the conservative spelling-independent value.
impl PhaseBridge<()> for bool {
    /// `false` = "the user wrote the return type explicitly."
    /// Kio' has no `-> .` elision, so the embed always renders
    /// the return.
    fn bridge(_: ()) -> Self {
        false
    }
}
impl PhaseBridge<()> for usize {
    /// `1` = "one canonical param." Pretty-printing pairs the
    /// arity with the function-type's param structure: a unit
    /// param renders as `()` regardless (see `pretty::doc_type`),
    /// a product param renders as `(A & B) -> R` (the canonical
    /// Kio' spelling), a non-product param renders as `A -> R`.
    fn bridge(_: ()) -> Self {
        1
    }
}
impl PhaseBridge<()> for Purity {
    fn bridge(_: ()) -> Self {
        Purity::Impure
    }
}

/// Per-node metadata bundle. Carries the source span (always present
/// at every phase) alongside leading trivia ([`Phase::LeadingTrivia`])
/// captured before the node's first token at [`Surface`]. At every
/// later phase `LeadingTrivia` collapses to `()` so `Meta<P>` is
/// effectively a wrapper around `Span`.
///
/// **Auxiliary spans** (sub-token positions like `name_span`,
/// `label_span`) stay as separate sibling fields — they don't fit
/// the per-node single-position shape that `Meta` captures.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Meta<P: Phase> {
    pub span: Span,
    pub leading_trivia: P::LeadingTrivia,
    /// Source trivia stranded at a *closing* boundary — the run of
    /// comments between the last meaningful token inside a container
    /// and its closing delimiter (`}` / `)`) or end of file. At
    /// [`Surface`] this is the closing token's leading trivia that the
    /// parser stashes here instead of discarding it (the lexer's
    /// trivia model is leading-only, so a comment in a closer's
    /// leading run has no other home); at every later phase it
    /// collapses to `()` like [`Self::leading_trivia`]. Only the
    /// bounded set of container positions the formatter knows about
    /// — block / `do` bodies, comma-list closers, label braces, and
    /// the module/file end — populate it; every other node leaves it
    /// at its default. The pretty-printer reads it to keep a trailing
    /// or dangling comment from being dropped on a `kio fmt`
    /// round-trip.
    #[serde(default)]
    pub trailing_trivia: P::LeadingTrivia,
}

impl<P: Phase> Meta<P> {
    /// Build a `Meta` from a span alone, with default (empty / unit)
    /// leading and trailing trivia. Used by every synthesizer that
    /// creates AST nodes from non-source positions (the typer's
    /// elaboration trees, `elaborator`'s intrinsic factories, the test
    /// module).
    pub fn new(span: Span) -> Self {
        Meta {
            span,
            leading_trivia: P::LeadingTrivia::default(),
            trailing_trivia: P::LeadingTrivia::default(),
        }
    }
}

/// Phase-rebrand a [`Meta`] across a phase boundary by preserving the
/// span and dropping the trivia (default at the target). Used by
/// every cross-phase walker (`desugar`, `label_elab`, `substitute`,
/// `prime::lower`, `backends::kio_prime::embed_*`).
pub fn convert_meta<P: Phase, Q: Phase>(m: &Meta<P>) -> Meta<Q> {
    Meta::new(m.span)
}

/// Capability annotations attached to a fn value at the [`Routed`]
/// phase. Holds the data the per-backend lowerings need to make
/// representational decisions about a closure without re-walking
/// the AST.
///
/// The `captured_from` field records which outer-scope binders the body
/// references. Sibling annotation on the adjacent AST position
/// ([`Lifetime`] on [`Type::Function`]) rides its own
/// phase-associated witness ([`FnTypeCapabilities`]); the
/// annotations form a coordinated framework, each writing through
/// the per-pass walker in [`crate::pass::capabilities`]. `Default`
/// produces the safe default — empty captures — so a `Routed` AST
/// produced before the annotation pass runs still behaves
/// correctly.
///
/// `captured_from` is an internal cross-pass annotation, not an
/// emitter input: the escape pass
/// ([`crate::pass::capabilities::annotate_escapes`]) writes it, and
/// the lifetime pass ([`crate::pass::capabilities::annotate_lifetime`])
/// reads it to derive the [`Lifetime`] each backend actually consumes.
/// No per-backend lowering reads `captured_from`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Capabilities {
    /// Outer-scope binder names this closure's body references —
    /// excluding the closure's own value-params (which it owns) and
    /// any locals introduced inside the body. Order is first-
    /// occurrence in a left-to-right traversal so emitted prologues
    /// are stable across runs.
    pub captured_from: Vec<String>,
}

/// Whether a [`Type::Function`]-typed slot's value may outlive the
/// stack frame that constructs it. `Heap` slots store the value
/// behind a heap-managed representation chosen by the backend
/// (refcounted-with-dyn-dispatch, GC'd reference, etc.); `Stack`
/// slots can be lowered to an unboxed in-place form when the host
/// language admits one. Type-erased / dynamically-typed backends
/// ignore the distinction because their runtime handles
/// representation uniformly; static-typed backends consume it.
///
/// The conservative default is [`Lifetime::Heap`]: a fn-typed
/// slot with no further information must be assumed to outlive
/// its constructing frame. The annotation pass refines to
/// [`Lifetime::Stack`] when the analysis proves the slot's value
/// is constructed and consumed within a single frame.
///
/// Sibling annotation to [`Capabilities`] on [`Expr::FnExpr`]; the
/// framework is shaped to admit further annotations later without
/// restructuring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum Lifetime {
    /// Conservative default: assume the fn-typed slot's value may
    /// escape; lower to whatever heap-managed form the backend
    /// chooses (Rust's `Rc<dyn Fn>`, Java's `Function<>`, etc.).
    #[default]
    Heap,
    /// The fn-typed slot's value is constructed and consumed within
    /// a single stack frame. Backends that admit an unboxed form
    /// (Rust's `impl Fn`, C++ `auto`-typed lambda, …) can lower to
    /// it; others fall back to their `Heap` form.
    Stack,
}

/// Capability annotations attached to a [`Type::Function`] slot at
/// the [`Routed`] phase. The single `lifetime` field carries the heap / stack
/// representation choice; retaining a bundle lets the annotation framework
/// add fields without changing its phase witness. `Default` produces the safe
/// default — `Heap` — so a
/// `Routed` AST produced before the annotation pass runs still
/// lowers correctly.
///
/// Each per-backend lowering reads only the fields it needs.
/// Static-typed backends typically consume `lifetime` to pick
/// between heap-managed and unboxed forms; type-erased /
/// dynamically-typed backends ignore it because their runtime
/// handles representation uniformly.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FnTypeCapabilities {
    /// Whether the slot's value may outlive its constructing stack
    /// frame. See [`Lifetime`].
    pub lifetime: Lifetime,
}

/// The parser's output phase. Every surface variant is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Surface;

impl Phase for Surface {
    type ExprBlockSyntax = ();
    type ExpressionOccurrence = ();
    type LitAnnotation = Option<Type<Surface>>;
    type ExprTuple = ();
    type ExprFnPlaceholder = ();
    type ExprOpChain = ();
    type ExprLabelValue = NodeId;
    type ExprRowLet = ();
    // The elaboration-bearing variants carry a `NodeId` so the
    // typer's `Elaborations` table has a clone-stable key (see
    // [`NodeId`] for why pointer-keying didn't work).
    type ExprElab = NodeId;
    type ExprRecCall = ();
    type ExprRecOrder = Never;
    type ExprUfcs = NodeId;
    // Enriched variants are introduced by the post-`Prime`
    // structural-recovery pass; uninhabited at every front-end phase.
    type ExprEnriched = Never;
    type ExprEnrichedSynthTy = Never;
    // Low-IR variants are introduced by the post-`Enriched`
    // resolution-lowering pass (`crate::pass::recover_to_low`); uninhabited
    // at every pre-Routed phase.
    type ExprLow = Never;
    // Pre-Routed phases admit raw `Call` / `Path` / `FnExpr`.
    type ExprResolved = ();
    type TypeLabelSugar = ();
    type TypeInfer = ();
    type TypeGoal = Never;
    type ItemLabels = ();
    type ItemEquiv = ();
    type ItemElaborator = ();
    type ItemOp = ();
    type ItemRecGroup = ();
    type ItemLiteralAlias = ();
    type ParamPatternExt = Option<ParamPattern>;
    type FnDefRetElided = bool;
    type FnPurity = Purity;
    type LeadingTrivia = Vec<crate::pass::lexer::Trivia>;
    // No capability-annotation pass runs at the surface phase.
    type FnExprCapabilities = ();
    type FnTypeCapabilities = ();
}

/// Post-`desugar` phase. Extensions handled by early lowering are no longer
/// constructible; label- and typer-directed surface forms remain for their
/// later passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Desugared;

impl Phase for Desugared {
    type ExprBlockSyntax = Never;
    type ExpressionOccurrence = ();
    type LitAnnotation = Option<Type<Desugared>>;
    type ExprTuple = Never;
    type ExprFnPlaceholder = Never;
    type ExprOpChain = Never;
    type ExprLabelValue = NodeId;
    type ExprRowLet = Never;
    type ExprElab = NodeId;
    type ExprRecCall = Never;
    type ExprRecOrder = NodeId;
    type ExprUfcs = NodeId;
    type ExprEnriched = Never;
    type ExprEnrichedSynthTy = Never;
    type ExprLow = Never;
    type ExprResolved = ();
    type TypeLabelSugar = ();
    type TypeInfer = ();
    type TypeGoal = Never;
    type ItemLabels = ();
    type ItemEquiv = ();
    type ItemElaborator = ();
    type ItemOp = Never;
    type ItemRecGroup = Never;
    type ItemLiteralAlias = Never;
    type ParamPatternExt = ();
    type FnDefRetElided = bool;
    type FnPurity = Purity;
    type LeadingTrivia = ();
    type FnExprCapabilities = ();
    type FnTypeCapabilities = ();
}

/// Post-`label_elab` phase. `Expr::Tuple`, `Expr::LabelValue`, `Type::LabelSugar`,
/// and `Item::Labels` are all stripped; what reaches `typecheck` is
/// the surface AST minus the early-desugar surface forms. Dot-splice
/// calls survive to the typer. The
/// elaboration-bearing variants (`Expr::Elaborator`,
/// `Expr::UserElaborator`) survive into the typer, which substitutes their
/// elaborations during the [`Lowered`] → [`Prime`] transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Lowered;

impl Phase for Lowered {
    type ExprBlockSyntax = Never;
    type ExpressionOccurrence = ExpressionOccurrence;
    type LitAnnotation = Option<Type<Lowered>>;
    type ExprTuple = Never;
    type ExprFnPlaceholder = Never;
    type ExprOpChain = Never;
    type ExprLabelValue = Never;
    type ExprRowLet = Never;
    type ExprElab = NodeId;
    type ExprRecCall = Never;
    type ExprRecOrder = NodeId;
    type ExprUfcs = NodeId;
    type ExprEnriched = Never;
    type ExprEnrichedSynthTy = Never;
    type ExprLow = Never;
    type ExprResolved = ();
    type TypeLabelSugar = Never;
    type TypeInfer = ();
    type TypeGoal = ();
    type ItemLabels = Never;
    type ItemEquiv = ();
    type ItemElaborator = ();
    type ItemOp = Never;
    type ItemRecGroup = Never;
    type ItemLiteralAlias = Never;
    type ParamPatternExt = ();
    type FnDefRetElided = bool;
    type FnPurity = Purity;
    type LeadingTrivia = ();
    type FnExprCapabilities = ();
    type FnTypeCapabilities = ();
}

/// Kio'-shaped but not final-artifact validated. Compile-time
/// evaluator inputs use this phase after substituting every required
/// expression elaboration and inferred type argument. An unresolved
/// literal annotation may remain as the phase's explicit compile-time
/// hole; a missing expression elaboration or `Type::Infer` resolution
/// is an invariant violation. It admits exactly the same AST variants
/// as [`Prime`] so surface-only forms are uninhabited, but it is not
/// itself a validated backend input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UncheckedPrime;

impl Phase for UncheckedPrime {
    type ExprBlockSyntax = Never;
    type ExpressionOccurrence = ();
    type LitAnnotation = Option<Type<UncheckedPrime>>;
    type ExprTuple = Never;
    type ExprFnPlaceholder = Never;
    type ExprOpChain = Never;
    type ExprLabelValue = Never;
    type ExprRowLet = Never;
    type ExprElab = Never;
    type ExprRecCall = Never;
    type ExprRecOrder = Never;
    type ExprUfcs = Never;
    type ExprEnriched = Never;
    type ExprEnrichedSynthTy = Never;
    type ExprLow = Never;
    type ExprResolved = ();
    type TypeLabelSugar = Never;
    type TypeInfer = Never;
    type TypeGoal = Never;
    type ItemLabels = Never;
    type ItemEquiv = Never;
    type ItemElaborator = Never;
    type ItemOp = Never;
    type ItemRecGroup = Never;
    type ItemLiteralAlias = Never;
    type ParamPatternExt = ();
    type FnDefRetElided = ();
    type FnPurity = Purity;
    type LeadingTrivia = ();
    type FnExprCapabilities = ();
    type FnTypeCapabilities = ();
}

/// Strict Kio' shape. The expression extensions for `Tuple`, `FnPlaceholder`,
/// `OpChain`, `LabelValue`, `RowLet`, `Elaborator` /
/// `UserElaborator`, `RecCall`, `RecOrder`, and `Ufcs`
/// are uninhabited here, as are `Type::LabelSugar` / `Infer` / internal `Goal`
/// and `Item::Labels` / `Equiv` / `Elaborator` / `Op` / `VariadicOperator` / `RecGroup` /
/// `LiteralAlias`. Enriched and Low-IR expression extensions are also
/// uninhabited; parameter-pattern and return-elision metadata has been erased.
/// The full route removes these forms across operator folding, desugaring,
/// label elaboration, resolution, type-directed completion, and substitution.
/// The Kio' parser instead rejects them while lowering Surface directly to
/// Prime. The Kio' emitter consumes `Prime`; host-emitter input is later derived
/// through `Enriched` and `Routed`. The marker proves shape, not that standalone
/// validation ran.
///
/// The seven `Expr::Enriched*` variants are also uninhabited at
/// `Prime`: they are introduced by the post-`Prime`
/// structural-recovery pass at the [`Enriched`] phase, not by the
/// typer. `backends::kio_prime` therefore still sees a pure Kio'-shaped
/// `Module<Prime>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Prime;

impl Phase for Prime {
    type ExprBlockSyntax = Never;
    type ExpressionOccurrence = ExpressionOccurrence;
    type LitAnnotation = Type<Prime>;
    type ExprTuple = Never;
    type ExprFnPlaceholder = Never;
    type ExprOpChain = Never;
    type ExprLabelValue = Never;
    type ExprRowLet = Never;
    type ExprElab = Never;
    type ExprRecCall = Never;
    type ExprRecOrder = Never;
    type ExprUfcs = Never;
    type ExprEnriched = Never;
    type ExprEnrichedSynthTy = Never;
    type ExprLow = Never;
    type ExprResolved = ();
    type TypeLabelSugar = Never;
    type TypeInfer = Never;
    type TypeGoal = Never;
    type ItemLabels = Never;
    type ItemEquiv = Never;
    type ItemElaborator = Never;
    type ItemOp = Never;
    type ItemRecGroup = Never;
    type ItemLiteralAlias = Never;
    type ParamPatternExt = ();
    type FnDefRetElided = ();
    type FnPurity = Purity;
    type LeadingTrivia = ();
    type FnExprCapabilities = ();
    type FnTypeCapabilities = ();
}

/// Post-`Prime` structural IR produced by the structural-recovery pass
/// (`crate::pass::structural_recovery`). Surface-only and elaboration-bearing
/// variants stay uninhabited, while the seven `Expr::Enriched*` variants are
/// admitted (`ExprEnriched = ()`). The recovery
/// pass walks a `Module<Prime>` and collapses right-leaning
/// intrinsic chains (`__pair__` / `__fst__` / `__snd__` /
/// `__left__` / `__right__` / `__either__` /
/// `__if_then_else__`) into the n-ary structural nodes, leaving
/// every non-chain Kio' expression intact. Host pipelines optimize this phase
/// and route it to [`Routed`]; `backends::kio_prime` bypasses recovery and
/// consumes validated `Module<Prime>`.
///
/// `Enriched` is **not** a [`crate::pass::resolve::ResolvePhase`]: name
/// resolution runs before type-checking, long before recovery, so
/// the resolver never sees this phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Enriched;

impl Phase for Enriched {
    type ExprBlockSyntax = Never;
    type ExpressionOccurrence = ();
    type LitAnnotation = Type<Enriched>;
    type ExprTuple = Never;
    type ExprFnPlaceholder = Never;
    type ExprOpChain = Never;
    type ExprLabelValue = Never;
    type ExprRowLet = Never;
    type ExprElab = Never;
    type ExprRecCall = Never;
    type ExprRecOrder = Never;
    type ExprUfcs = Never;
    // This phase introduces the enriched structural variants; Routed retains
    // them for host emission.
    type ExprEnriched = ();
    type ExprEnrichedSynthTy = Type<Enriched>;
    type ExprLow = Never;
    type ExprResolved = ();
    type TypeLabelSugar = Never;
    type TypeInfer = Never;
    type TypeGoal = Never;
    type ItemLabels = Never;
    type ItemEquiv = Never;
    type ItemElaborator = Never;
    type ItemOp = Never;
    type ItemRecGroup = Never;
    type ItemLiteralAlias = Never;
    type ParamPatternExt = ();
    type FnDefRetElided = ();
    type FnPurity = ();
    type LeadingTrivia = ();
    // Capability annotations land at the Routed phase, not here.
    type FnExprCapabilities = ();
    type FnTypeCapabilities = ();
}

/// Post-`Enriched` host-emission phase produced by the
/// resolution-lowering pass (`crate::pass::recover_to_low`). Admits
/// **exactly** what [`Enriched`] admits — every enriched structural
/// variant carries through — *plus* the `Expr::Low*` family
/// (`ExprLow = ()`), and *minus* `Expr::Call` / `Expr::Path`
/// (`ExprResolved = Never`). `Expr::FnExpr` survives as a value-construction
/// site and carries Routed capability annotations.
///
/// The variants encode *routing decisions*: is this call to a host
/// fn, a module fn, a qualified module fn, an inline
/// lambda, a newtype constructor, …? Per-backend lowerings consume `Routed`
/// and drop to syntactic templating over the variant's shape — they
/// never re-derive routing from the package's `scope` / `selective_imports`
/// tables at every call site.
///
/// The name `Routed` is deliberately different from [`Lowered`] —
/// the existing surface-pipeline `Lowered` is the typer-input phase
/// that emerges from `desugar` + `label_elab`. `Routed` is the
/// post-typer phase that emerges from `recover_to_low::lower`.
///
/// `Routed` is **not** a [`crate::pass::resolve::ResolvePhase`]: name
/// resolution runs before type-checking, long before lowering, so
/// the resolver never sees this phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Routed;

impl Phase for Routed {
    type ExprBlockSyntax = Never;
    type ExpressionOccurrence = ();
    type LitAnnotation = Type<Routed>;
    type ExprTuple = Never;
    type ExprFnPlaceholder = Never;
    type ExprOpChain = Never;
    type ExprLabelValue = Never;
    type ExprRowLet = Never;
    type ExprElab = Never;
    type ExprRecCall = Never;
    type ExprRecOrder = Never;
    type ExprUfcs = Never;
    // Carry-through from Enriched: the structural variants stay
    // inhabited (the pass is additive over them).
    type ExprEnriched = ();
    type ExprEnrichedSynthTy = Type<Routed>;
    // The Low-IR variants this phase introduces.
    type ExprLow = ();
    // Surface-residual `Call` / `Path` variants are statically unreachable:
    // every such node is classified into a `Low*` variant. `Expr::FnExpr`
    // carries through on its separate [`FnExprCapabilities`] witness because
    // it is a value-construction site rather than a routing decision.
    type ExprResolved = Never;
    type TypeLabelSugar = Never;
    type TypeInfer = Never;
    type TypeGoal = Never;
    type ItemLabels = Never;
    type ItemEquiv = Never;
    type ItemElaborator = Never;
    type ItemOp = Never;
    type ItemRecGroup = Never;
    type ItemLiteralAlias = Never;
    type ParamPatternExt = ();
    type FnDefRetElided = ();
    type FnPurity = ();
    type LeadingTrivia = ();
    // The Routed phase is the one phase that holds annotation data
    // on an `Expr::FnExpr`. The per-module
    // [`crate::pass::capabilities::annotate_escapes`] pass fills the
    // [`Capabilities`] bundle; pre-pass nodes carry the default
    // (empty captures).
    type FnExprCapabilities = Capabilities;
    // Sibling annotation: per-module
    // [`crate::pass::capabilities::annotate_lifetime`] fills the
    // [`FnTypeCapabilities`] bundle on each `Type::Function`.
    // Pre-pass nodes carry the conservative default
    // (`Lifetime::Heap`).
    type FnTypeCapabilities = FnTypeCapabilities;
}

// ---- Top-level (regular module) -----------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum KioFileKind {
    Module,
    Package,
    Signature,
    Dependency,
    Lock,
}

impl KioFileKind {
    pub fn variant_name(self) -> &'static str {
        match self {
            KioFileKind::Module => "module",
            KioFileKind::Package => "package",
            KioFileKind::Signature => "signature",
            KioFileKind::Dependency => "dependency",
            KioFileKind::Lock => "lock",
        }
    }

    pub fn from_variant_name(value: &str) -> Option<Self> {
        match value {
            "module" => Some(KioFileKind::Module),
            "package" => Some(KioFileKind::Package),
            "signature" => Some(KioFileKind::Signature),
            "dependency" => Some(KioFileKind::Dependency),
            "lock" => Some(KioFileKind::Lock),
            _ => None,
        }
    }

    pub fn accepts_module_path(self) -> bool {
        matches!(self, KioFileKind::Module)
    }

    pub fn inferred_path(self, segments: &[String]) -> Option<String> {
        let first = segments.first()?;
        Some(match self {
            KioFileKind::Module => format!("{}.kio", segments.join("/")),
            KioFileKind::Package => format!("{first}.pkg.kio"),
            KioFileKind::Signature => format!("{first}.sig.kio"),
            KioFileKind::Dependency => format!("{first}.dep.kio"),
            KioFileKind::Lock => format!("{first}.lock.kio"),
        })
    }

    pub fn package_name(self, segments: &[String]) -> Option<&str> {
        match self {
            KioFileKind::Module => {
                if segments.len() > 1 {
                    segments.first().map(String::as_str)
                } else {
                    None
                }
            }
            KioFileKind::Package | KioFileKind::Signature => segments.first().map(String::as_str),
            KioFileKind::Dependency | KioFileKind::Lock => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Module<P: Phase = Surface> {
    pub path: ModulePath,
    pub imports: Vec<Import>,
    pub items: Vec<Item<P>>,
    pub meta: Meta<P>,
    /// Doc-comment attached to the module itself. A `///` block at the
    /// very top of the file — before the `module` line, with no `import`
    /// or decl between it and the `module` keyword — attaches here.
    /// `None` when no module-level doc-comment is present.
    pub doc: Option<DocComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecGroup<P: Phase = Surface> {
    pub loop_path: Vec<PathSegment>,
    pub members: Vec<FnDef<P>>,
    pub meta: Meta<P>,
}

/// A capability-free recursive scope for type declarations. Unlike
/// [`RecGroup`], this is part of Kio' and therefore remains explicit through
/// every phase artifact.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TypeRecGroup<P: Phase = Surface> {
    pub members: Vec<TypeRecMember<P>>,
    /// Doc-comment attached to the group container, immediately before its
    /// `rec` token.
    #[serde(default)]
    pub doc: Option<DocComment>,
    /// Exact source ownership for structural editor repairs. Written groups
    /// retain one complete, prefix-inclusive span per member plus any comment
    /// run before the closing brace. Lowering-generated groups leave this
    /// unset: they have no source delimiters whose layout an editor may
    /// safely rewrite.
    #[serde(default)]
    pub source_layout: Option<TypeRecSourceLayout>,
    /// Exact source-token spans for a written group. Lowering-generated
    /// groups leave these unset because they do not own editable source
    /// delimiters.
    #[serde(default)]
    pub rec_span: Option<Span>,
    #[serde(default)]
    pub open_brace_span: Option<Span>,
    #[serde(default)]
    pub close_brace_span: Option<Span>,
    /// A surface `labels` declaration whose required singleton `rec` marker
    /// was omitted. Label lowering temporarily keeps the declaration's whole
    /// generated type scope in this group so the shared signature checker can
    /// validate it before publishing the tailored marker diagnostic. Valid
    /// source and persistent Kio' artifacts leave this unset.
    #[serde(default)]
    pub deferred_rec_labels_diagnostic: Option<DeferredRecLabelsDiagnostic>,
    pub meta: Meta<P>,
}

/// Source positions retained only while validating an unmarked recursive
/// `labels` declaration. The shared checker emits the missing-marker error
/// after every generated member passes ordinary visibility, kind, arity,
/// nominal-grounding, and positivity checks.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeferredRecLabelsDiagnostic {
    pub reference_span: Span,
    pub declaration_span: Span,
    pub head_span: Span,
    pub has_named_alias: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TypeRecSourceLayout {
    pub member_spans: Vec<Span>,
    /// Exact semicolon tokens owned by the group and its outer entry.
    pub separator_spans: Vec<Span>,
    /// Grammar-defined insertion point for a singleton `rec` marker: after
    /// any member visibility and before the declaration keyword.
    pub member_marker_offsets: Vec<u32>,
    pub trailing_comment_span: Option<Span>,
}

/// The declaration kinds admitted by a [`TypeRecGroup`]. `labels` is surface
/// sugar and becomes statically uninhabited after label elaboration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TypeRecMember<P: Phase = Surface> {
    TypeAlias(TypeAlias<P>),
    Newtype(Newtype<P>),
    Labels(Labels<P>, P::ItemLabels),
}

impl<P: Phase> TypeRecMember<P> {
    pub fn meta(&self) -> &Meta<P> {
        match self {
            Self::TypeAlias(alias) => &alias.meta,
            Self::Newtype(newtype) => &newtype.meta,
            Self::Labels(labels, _) => &labels.meta,
        }
    }

    pub fn meta_mut(&mut self) -> &mut Meta<P> {
        match self {
            Self::TypeAlias(alias) => &mut alias.meta,
            Self::Newtype(newtype) => &mut newtype.meta,
            Self::Labels(labels, _) => &mut labels.meta,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RecCallMode {
    Poly,
    Cont,
    Escape,
}

impl RecCallMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RecCallMode::Poly => "poly",
            RecCallMode::Cont => "cont",
            RecCallMode::Escape => "escape",
        }
    }

    pub fn fmt_rank(self) -> u8 {
        match self {
            RecCallMode::Poly => 0,
            RecCallMode::Cont => 1,
            RecCallMode::Escape => 2,
        }
    }

    pub fn from_annotation(s: &str) -> Option<Self> {
        match s {
            "poly" => Some(Self::Poly),
            "cont" => Some(Self::Cont),
            "escape" => Some(Self::Escape),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Item<P: Phase = Surface> {
    FnDef(FnDef<P>),
    RecGroup(RecGroup<P>, P::ItemRecGroup),
    TypeRecGroup(TypeRecGroup<P>),
    /// `type NAME = Type;` — a transparent type synonym.
    TypeAlias(TypeAlias<P>),
    /// `literal name = 1;` — a surface-only literal token alias.
    LiteralAlias(LiteralAlias<P>, P::ItemLiteralAlias),
    Newtype(Newtype<P>),
    /// `labels { f: X, g: Y };` (anonymous) or `labels T = { f: X };`
    /// (named). **Lowered during label-elab** to N `Item::Newtype`
    /// chains plus an optional `Item::TypeAlias` for the named form.
    Labels(Labels<P>, P::ItemLabels),
    /// `type {local} = {provider.label};` — a nonminting label binding,
    /// consumed with the label namespace before Lowered.
    LabelForward(LabelForward<P>, P::ItemLabels),
    /// `equiv name[A](x: A) { <expr>; <expr>; ... }` — a claim
    /// that all the arm expressions reduce to the same normal form.
    /// **Surface-only:** survives `desugar` and
    /// `label_elab` so `typecheck_full` can validate the bodies, then
    /// the `Lowered → Prime` substitution pass filters these items
    /// out. Has no execution semantics in the build artifact; the
    /// `kio test` runner reads them out of the typed AST and
    /// discharges them via partial evaluation.
    Equiv(Equiv<P>, P::ItemEquiv),
    /// `pub elab name : CallType { impl implementation; };` — a
    /// compile-time elaborator declaration. Elaborator items live in
    /// their own import/use namespace and are filtered before Kio'.
    Elaborator(UserElaboratorDef<P>, P::ItemElaborator),
    /// `op _ + _ { impl add; };` — a user-defined operator binding.
    /// **Surface-only:** the desugar pass folds operator usages into
    /// plain `Expr::Call` and drops the `Op` item itself before
    /// the `Desugared` phase, so no later pass sees this variant.
    Op(Box<Op<P>>, P::ItemOp),
    /// `varop [* *] { foldl append empty; };` — a
    /// user-defined variadic delimited fold binding. Surface-only:
    /// the operator-fold pass uses it to fold bracketed uses into
    /// calls, and desugar drops the item before `Desugared`.
    VariadicOperator(Box<VariadicOperator<P>>, P::ItemOp),
    /// `host type Foo;` / `host type Box[A];` / `host type Count role(i32);`
    /// — a host-supplied
    /// opaque type. Declaration only, no body; the host fills in the
    /// concrete type at invocation time. **Inhabited at every phase**
    /// (not surface-only); opaque in Kio' as `h : T`. Always public.
    HostType(HostType<P>),
    /// `host fn foo(a: Bar) -> Baz;` — a host-supplied function the host
    /// must provide. Declaration only, no body. **Inhabited at every
    /// phase** (not surface-only); opaque in Kio'. Always public.
    HostFn(HostFn<P>),
}

/// The visibility of a top-level item (and of a newtype's or recursive
/// group's members). `Private` (the default) is visible only within the
/// declaring module; `Public` exports the item to every importer including
/// consuming packages; `PublicIn(path)` restricts it to the module subtree
/// rooted at `path` — importable from `path` and any module beneath it,
/// sealed from everywhere else. `path` must be a prefix of the declaring
/// module's own path (visibility can only be relaxed up to an ancestor
/// scope), which the resolver validates.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Visibility {
    Private,
    Public,
    PublicIn(ModulePath),
}

impl Visibility {
    /// True for any `pub` form — `Public` or `PublicIn` — i.e. visible
    /// beyond the declaring module. Use for intra-package reasoning
    /// (resolver suggestions, dev-tool display). NOT the host-export test
    /// — `PublicIn` is package-internal; see `is_exported`.
    pub fn is_pub(&self) -> bool {
        !matches!(self, Visibility::Private)
    }

    /// True only for `Public` — the fully-exported form. `PublicIn`
    /// (scoped) is package-internal: importable within its own subtree
    /// but kept OUT of the package's host interface and the `.sig`
    /// contract. The contract-surface, sig, and backend-export gates use
    /// this.
    pub fn is_exported(&self) -> bool {
        matches!(self, Visibility::Public)
    }
}

/// User-defined fixed-arity operator binding declaration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Op<P: Phase = Surface> {
    /// `pub op` exports the operator binding to consumers of
    /// the declaring module. Without `pub`, the binding stays
    /// module-local. A consumer brings the binding into its own
    /// operator scope with `import m(op <pattern>);`, and the
    /// operator-fold pass resolves use-site chains against it.
    pub vis: Visibility,
    /// The fixed-arity pattern and implementation function.
    pub body: OpBody,
    /// Canonical trivia owner for comments inside the compact body, keeping
    /// them distinct from declaration documentation during formatting.
    pub body_trivia: P::LeadingTrivia,
    pub meta: Meta<P>,
    /// Doc-comment attached to this `op` declaration, if any.
    pub doc: Option<DocComment>,
}

/// User-defined variadic operator declaration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VariadicOperator<P: Phase = Surface> {
    /// Visibility of the operator's grammar and its callable binding.
    pub vis: Visibility,
    /// The single maximal OPEN symbol run that starts the delimited syntax.
    pub open: Vec<String>,
    /// Mirrored CLOSE token, fold mode, and callable targets.
    pub spec: Box<VariadicSpec>,
    /// Canonical trivia owner for comments inside the compact body, keeping
    /// them distinct from declaration documentation during formatting.
    pub body_trivia: P::LeadingTrivia,
    pub meta: Meta<P>,
    /// Doc-comment attached to this variadic operator declaration, if any.
    pub doc: Option<DocComment>,
}

/// User-defined elaborator declaration.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ElaboratorSchedule {
    #[default]
    Late,
    Fills,
}

/// An ordinary lexical value path selected as a declaration's callable
/// target. The parser admits a bare local/selectively imported name or a
/// dotted path through an explicit module alias or newtype owner.
///
/// This is a nominal, non-empty wrapper rather than an arbitrary expression.
/// Its source range is derived from the already-spanned first and last
/// segments, so declaration paths do not carry a redundant whole-path span.
#[repr(transparent)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub struct LexicalCallablePath(Vec<PathSegment>);

impl LexicalCallablePath {
    pub fn new(segments: Vec<PathSegment>) -> Self {
        assert!(
            !segments.is_empty(),
            "a lexical callable path must contain at least one segment"
        );
        Self(segments)
    }

    pub fn synth(names: impl IntoIterator<Item = String>, span: Span) -> Self {
        Self::new(
            names
                .into_iter()
                .map(|name| PathSegment::synth(name, span))
                .collect(),
        )
    }

    pub fn segments(&self) -> &[PathSegment] {
        &self.0
    }

    pub fn span(&self) -> Span {
        let first = self
            .0
            .first()
            .expect("a lexical callable path is non-empty");
        let last = self.0.last().expect("a lexical callable path is non-empty");
        Span::new(first.span.start, last.span.end)
    }
}

impl<'de> serde::Deserialize<'de> for LexicalCallablePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let segments = <Vec<PathSegment> as serde::Deserialize>::deserialize(deserializer)?;
        if segments.is_empty() {
            return Err(serde::de::Error::custom(
                "a lexical callable path must contain at least one segment",
            ));
        }
        Ok(Self(segments))
    }
}

impl std::ops::Deref for LexicalCallablePath {
    type Target = [PathSegment];

    fn deref(&self) -> &Self::Target {
        self.segments()
    }
}

impl AsRef<[PathSegment]> for LexicalCallablePath {
    fn as_ref(&self) -> &[PathSegment] {
        self.segments()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UserElaboratorDef<P: Phase = Surface> {
    pub vis: Visibility,
    pub name: String,
    pub name_span: Span,
    pub trailing_blocks: Vec<TrailingBlockDecl<P>>,
    /// Optional compile-time captures prepended to the implementation
    /// ABI. Each capture is a name/path, resolved in the elaborator's
    /// definition environment.
    pub captures: Vec<UserElaboratorCapture>,
    /// Surface call signature. Type slots are reflected as declared
    /// ABI positions; value slots are reflected as checked-term ABI
    /// positions. The implementation parameter type determines whether
    /// a type slot requires `__Type__` or accepts `__Type__ | .`.
    pub call_ty: Type<P>,
    pub schedule: ElaboratorSchedule,
    /// Lexical path naming the implementation function that uses the
    /// `__comptime__` helper surface.
    pub implementation: LexicalCallablePath,
    /// Canonical trivia owner for comments inside the compact body, keeping
    /// them distinct from declaration documentation during formatting.
    pub body_trivia: P::LeadingTrivia,
    pub meta: Meta<P>,
    /// Doc-comment attached to this elaborator declaration, if any.
    pub doc: Option<DocComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UserElaboratorCapture {
    pub segments: Vec<PathSegment>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BlockExposure {
    Product,
    Thunk,
    Sequence,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrailingBlockDecl<P: Phase = Surface> {
    pub exposure: BlockExposure,
    pub label: Option<PathSegment>,
    pub meta: Meta<P>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum UserElaboratorCallForm {
    Ordinary,
    TrailingBlocks,
}

/// The body of a fixed-arity `op` declaration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OpBody {
    /// Fixed-arity operator: `<pattern> { impl <function>; }`. `pattern`
    /// is the full alternating slot/token sequence — the slot
    /// pattern conveys associativity (`[Plain, Op, Plain]` is
    /// non-assoc, `[Plain, Op, Recursive]` is right-assoc,
    /// `[Recursive, Op, Plain]` is left-assoc). `function` is the
    /// bound function's lexical path (single name `add`, qualified `m.add`,
    /// or type-member `T.add`).
    Normal {
        pattern: Vec<OpPart>,
        function: LexicalCallablePath,
    },
}

/// Callable behavior of a comma-separated variadic operator.
/// The OPEN tokens live on [`VariadicOperator::open`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VariadicSpec {
    /// The single maximal CLOSE run, mirroring OPEN.
    pub close: Vec<String>,
    pub mode: VariadicMode,
    /// The mode determines whether this callable receives no arguments or
    /// one element value.
    pub initializer: CallableSpec,
    /// Step callable — invoked per element with its value and the accumulator.
    pub step: CallableSpec,
    /// Finalize callable — `None` is the implicit identity (the
    /// fold result is the final expression). When `Some`, called
    /// with the fold result as its sole positional arg.
    pub finalize: Option<CallableSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VariadicMode {
    FoldLeft,
    FoldRight,
    FoldLeftOne,
    FoldRightOne,
}

impl VariadicMode {
    pub fn keyword(self) -> &'static str {
        match self {
            Self::FoldLeft => "foldl",
            Self::FoldRight => "foldr",
            Self::FoldLeftOne => "foldl1",
            Self::FoldRightOne => "foldr1",
        }
    }

    pub fn is_right(self) -> bool {
        matches!(self, Self::FoldRight | Self::FoldRightOne)
    }

    pub fn requires_element(self) -> bool {
        matches!(self, Self::FoldLeftOne | Self::FoldRightOne)
    }
}

/// A lexical callable path inside a variadic operator declaration. The variadic
/// mechanism supplies positional arguments according to the callable's
/// position in the spec.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CallableSpec {
    pub path: LexicalCallablePath,
}

impl CallableSpec {
    pub fn span(&self) -> Span {
        self.path.span()
    }
}

/// One element of a `op` pattern. Slot kinds determine
/// associativity (per [§ Operators](specs/language.md#operators));
/// op tokens are the literal operator strings between slots.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OpPart {
    /// `_` — an ordinary slot, admits a single atom or paren-
    /// expression. `lenient = true` marks the slot as wrapped in
    /// the declaration-only `( … )` grouping marker, which widens
    /// the use-site operand parser from atom-only to a full
    /// expression chain (cross-operator mixing inside the slot
    /// still requires explicit parens — same as `___`).
    SlotPlain { span: Span, lenient: bool },
    /// `__` — a self-recursive slot. Admits the surrounding op's
    /// same-op chain. At most one of `__` or [`SlotGreedy`] per
    /// pattern; the position relative to the operator(s) sets the
    /// associativity. `lenient` mirrors [`SlotPlain::lenient`].
    SlotRecursive { span: Span, lenient: bool },
    /// `___` — a greedy slot. Admits a chain of *any* single
    /// operator (possibly different from the surrounding op), with
    /// cross-operator mixing inside the slot still rejected. Shares
    /// the at-most-one-recursive-slot count with `__`; dual-greedy
    /// patterns (`___ OP ___`) are a parse error since there is no
    /// position info to pin associativity. Always full-chain at
    /// use sites — `( ___ )` is rejected as redundant.
    SlotGreedy { span: Span },
    /// An operator-character literal. The string is the textual
    /// operator content; consecutive operator-character runs lex
    /// as one [`crate::pass::lexer::TokenKind::SymbolRun`]. `lenient`
    /// records group-membership for round-trip preservation of
    /// the `( … )` declaration-only slot-grouping marker —
    /// pretty-printing wraps maximal runs of `lenient = true`
    /// parts in `( … )`. `quoted` records that the token was
    /// written in operator-token-quotation form `(token)` — the
    /// declaration-only `(...)` wrapper that distinguishes an
    /// operator token from its structural role in the surrounding
    /// grammar. Quotation does not bypass spelling reservations such
    /// as dot-led one-dot runs.
    Token {
        content: String,
        span: Span,
        lenient: bool,
        quoted: bool,
    },
}

impl OpPart {
    /// True if this part was declared inside the `( … )` lenient-
    /// grouping marker. Slots use the flag to widen use-site
    /// operand parsing; tokens use it for round-trip preservation.
    pub fn is_lenient(&self) -> bool {
        match self {
            OpPart::SlotPlain { lenient, .. }
            | OpPart::SlotRecursive { lenient, .. }
            | OpPart::Token { lenient, .. } => *lenient,
            OpPart::SlotGreedy { .. } => false,
        }
    }

    /// True if this token was declared in operator-token-quotation
    /// form `(token)`. Only `Token` variants can be quoted.
    pub fn is_quoted(&self) -> bool {
        matches!(self, OpPart::Token { quoted: true, .. })
    }
}

/// `pub? type NAME TypeParams? = Type;` — a transparent type
/// synonym. The typer unfolds aliases structurally.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TypeAlias<P: Phase = Surface> {
    pub vis: Visibility,
    pub name: String,
    /// Exact span of the declared type name. Kept separately from `meta`
    /// so diagnostics can point at the head rather than the whole item.
    pub name_span: Span,
    pub type_params: Vec<TypeParam>,
    pub body: Type<P>,
    pub meta: Meta<P>,
    /// Exact complete source range that an editor may move or wrap as this
    /// declaration. Parsed Surface declarations include visibility and an
    /// attached `///` block; synthesized declarations and persistent Prime
    /// artifacts leave it unset.
    #[serde(default)]
    pub editable_span: Option<Span>,
    /// Doc-comment attached to this `type` definition, if any.
    pub doc: Option<DocComment>,
}

impl<P: Phase> TypeAlias<P> {
    pub fn type_body(&self) -> &Type<P> {
        &self.body
    }
}

/// `pub? literal name = <literal>;` — a surface-only alias for a
/// single unannotated literal token.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LiteralAlias<P: Phase = Surface> {
    pub vis: Visibility,
    pub name: String,
    pub value: LiteralAliasValue,
    pub meta: Meta<P>,
    /// Doc-comment attached to this `literal` definition, if any.
    pub doc: Option<DocComment>,
}

/// The literal token stored by a [`LiteralAlias`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LiteralAliasValue {
    Str { value: String, span: Span },
    Int { digits: String, span: Span },
    Float { digits: String, span: Span },
    Bool { value: bool, span: Span },
}

impl LiteralAliasValue {
    pub fn span(&self) -> Span {
        match self {
            LiteralAliasValue::Str { span, .. }
            | LiteralAliasValue::Int { span, .. }
            | LiteralAliasValue::Float { span, .. }
            | LiteralAliasValue::Bool { span, .. } => *span,
        }
    }

    /// The literal-alias declaration form (`literal name = 1;`) is
    /// surface-only, so `to_expr` only ever mints a literal at a phase
    /// whose annotation container is the optional surface shape; the
    /// bound both documents that and lets the bare `None` construct.
    pub fn to_expr<P: Phase<LitAnnotation = Option<Type<P>>>>(&self) -> Expr<P> {
        match self {
            LiteralAliasValue::Str { value, span } => Expr::StrLit {
                occurrence: Default::default(),
                value: value.clone(),
                annotation: None,
                meta: Meta::new(*span),
            },
            LiteralAliasValue::Int { digits, span } => Expr::IntLit {
                occurrence: Default::default(),
                digits: digits.clone(),
                annotation: None,
                meta: Meta::new(*span),
            },
            LiteralAliasValue::Float { digits, span } => Expr::FloatLit {
                occurrence: Default::default(),
                digits: digits.clone(),
                annotation: None,
                meta: Meta::new(*span),
            },
            LiteralAliasValue::Bool { value, span } => Expr::BoolLit {
                occurrence: Default::default(),
                value: *value,
                annotation: None,
                meta: Meta::new(*span),
            },
        }
    }
}

// ---- imports ------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Import {
    pub kind: ImportKind,
    pub span: Span,
    /// Leading trivia (line comments / blank-line markers) captured
    /// before the `import` declaration and among its header tokens. Populated by the parser; the
    /// pretty-printer reads it to preserve docstring-style comments
    /// belonging to imports through a `kio fmt` round-trip. Empty
    /// at non-Surface phases (no walker re-emits source there).
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
    /// Comments before a selective list's closing parenthesis.
    /// Empty for generated imports and non-selective forms.
    pub trailing_trivia: Vec<crate::pass::lexer::Trivia>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ImportKind {
    /// `import path/to/module(a, B, {field}, op _ + _);` — selective
    /// import. Bare identifiers select the ordinary value/type namespace,
    /// braced identifiers select the label namespace, and operator patterns
    /// select the operator namespace. The three spellings are deliberately
    /// distinct: adding an export in one namespace cannot change what an
    /// existing import denotes.
    Selective {
        items: Vec<ImportItem>,
        from: ModulePath,
    },
    /// `import path/to/module as m;`
    Qualified { path: ModulePath, alias: String },
    /// `import __intrinsics__;` — brings the eight value intrinsics into scope.
    Intrinsics,
    /// `import __comptime__;` — brings the compile-time helper surface into scope.
    Comptime,
}

/// One entry in an [`ImportKind::Selective`] item list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ImportItem {
    /// `foo`, `Bar` — regular identifier import (function or type).
    Name {
        name: String,
        span: Span,
        leading_trivia: Vec<crate::pass::lexer::Trivia>,
    },
    /// `{foo}` — a label-syntax import. The label elaborator consumes this
    /// item and rewrites label references through a collision-free qualified
    /// module binding before the tree reaches Kio'.
    Label {
        name: String,
        span: Span,
        leading_trivia: Vec<crate::pass::lexer::Trivia>,
    },
    /// A complete consumer-declared fixed or variadic parse grammar.
    /// Semantic import resolution checks this projection against the
    /// explicitly selected provider before attaching ordinary call targets.
    OperatorPattern {
        grammar: OperatorGrammar,
        span: Span,
        leading_trivia: Vec<crate::pass::lexer::Trivia>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OperatorGrammar {
    Fixed(Vec<OpPart>),
    Variadic {
        open: Vec<String>,
        close: Vec<String>,
    },
}

impl OperatorGrammar {
    pub fn fixed(pattern: &[OpPart]) -> Self {
        let zero = Span::new(0, 0);
        Self::Fixed(
            pattern
                .iter()
                .map(|part| match part {
                    OpPart::Token { content, .. } => OpPart::Token {
                        content: content.clone(),
                        span: zero,
                        lenient: false,
                        quoted: content == "=",
                    },
                    OpPart::SlotPlain { lenient, .. } => OpPart::SlotPlain {
                        span: zero,
                        lenient: *lenient,
                    },
                    OpPart::SlotRecursive { lenient, .. } => OpPart::SlotRecursive {
                        span: zero,
                        lenient: *lenient,
                    },
                    OpPart::SlotGreedy { .. } => OpPart::SlotGreedy { span: zero },
                })
                .collect(),
        )
    }

    pub fn variadic(open: &[String], spec: &VariadicSpec) -> Self {
        Self::Variadic {
            open: open.to_vec(),
            close: spec.close.clone(),
        }
    }

    pub fn dispatch_key(&self) -> OperatorDispatchKey {
        match self {
            Self::Fixed(pattern) => OperatorDispatchKey::from_pattern(pattern),
            Self::Variadic { open, .. } => OperatorDispatchKey {
                expr_start: true,
                leading_run: open.clone(),
            },
        }
    }

    pub fn render(&self) -> String {
        match self {
            Self::Fixed(pattern) => {
                let mut words = vec!["op".to_owned()];
                words.extend(pattern.iter().map(|part| match part {
                    OpPart::Token { content, .. } if content == "=" => "(=)".to_owned(),
                    OpPart::Token { content, .. } => content.clone(),
                    OpPart::SlotPlain { lenient: false, .. } => "_".to_owned(),
                    OpPart::SlotPlain { lenient: true, .. } => "(_)".to_owned(),
                    OpPart::SlotRecursive { lenient: false, .. } => "__".to_owned(),
                    OpPart::SlotRecursive { lenient: true, .. } => "(__)".to_owned(),
                    OpPart::SlotGreedy { .. } => "___".to_owned(),
                }));
                words.join(" ")
            }
            Self::Variadic { open, close } => {
                let mut words = vec!["varop".to_owned()];
                words.extend(open.iter().cloned());
                words.extend(close.iter().cloned());
                words.join(" ")
            }
        }
    }
}

/// Internal dispatch and collision key: expression position plus leading
/// operator-token runs. Distinct full grammars can share this key, so imports,
/// documentation identities and public queries use [`OperatorGrammar`] instead.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct OperatorDispatchKey {
    /// Expression-start syntax shares one keyspace across fixed and variadic
    /// operators. A leading operand instead selects the continuation keyspace.
    pub expr_start: bool,
    /// Token-run boundaries are significant: `&& ++` differs from `&&++`.
    pub leading_run: Vec<String>,
}

impl OperatorDispatchKey {
    /// Derive the collision key from a fixed operator declaration.
    pub fn from_body(body: &OpBody) -> OperatorDispatchKey {
        match body {
            OpBody::Normal { pattern, .. } => OperatorDispatchKey {
                expr_start: matches!(pattern.first(), Some(OpPart::Token { .. })),
                leading_run: op_leading_run(pattern),
            },
        }
    }

    /// Variadic operators occupy the expression-start keyspace.
    pub fn from_variadic(fold: &VariadicOperator) -> OperatorDispatchKey {
        OperatorDispatchKey {
            expr_start: true,
            leading_run: fold.open.clone(),
        }
    }

    /// The fixed operator's collision key, derived from its grammar.
    pub fn from_pattern(pattern: &[OpPart]) -> OperatorDispatchKey {
        OperatorDispatchKey {
            expr_start: matches!(pattern.first(), Some(OpPart::Token { .. })),
            leading_run: op_leading_run(pattern),
        }
    }
}

/// The leading op-token run-sequence of a fixed-arity `op` pattern —
/// the contiguous run of op-tokens from the first token up to the
/// next slot. This is the `SYMBOLS` part of the operator's short name
/// and, paired with the expression-start flag, its registry
/// uniqueness key. For an expression-start pattern the run starts at
/// index 0; for a non-expression-start pattern it starts at the first
/// token after the leading slot.
pub(crate) fn op_leading_run(pattern: &[OpPart]) -> Vec<String> {
    let start = pattern
        .iter()
        .position(|p| matches!(p, OpPart::Token { .. }))
        .unwrap_or(0);
    pattern[start..]
        .iter()
        .map_while(|p| match p {
            OpPart::Token { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

impl ImportItem {
    /// Comments attached to this selection, including hoisted inter-token trivia.
    pub fn leading_trivia(&self) -> &[crate::pass::lexer::Trivia] {
        match self {
            Self::Name { leading_trivia, .. }
            | Self::Label { leading_trivia, .. }
            | Self::OperatorPattern { leading_trivia, .. } => leading_trivia,
        }
    }

    pub fn leading_trivia_mut(&mut self) -> &mut Vec<crate::pass::lexer::Trivia> {
        match self {
            Self::Name { leading_trivia, .. }
            | Self::Label { leading_trivia, .. }
            | Self::OperatorPattern { leading_trivia, .. } => leading_trivia,
        }
    }

    /// The name string when this item selects the ordinary value/type
    /// namespace. Label and operator imports deliberately return `None`.
    pub fn as_name(&self) -> Option<&str> {
        match self {
            ImportItem::Name { name, .. } => Some(name.as_str()),
            ImportItem::Label { .. } | ImportItem::OperatorPattern { .. } => None,
        }
    }

    /// The ordinary value/type spelling and its exact source span. Label and
    /// operator imports deliberately return `None`.
    pub fn as_name_with_span(&self) -> Option<(&str, Span)> {
        match self {
            ImportItem::Name { name, span, .. } => Some((name.as_str(), *span)),
            ImportItem::Label { .. } | ImportItem::OperatorPattern { .. } => None,
        }
    }

    /// The label spelling and source span when this item selects the label
    /// namespace. Ordinary and operator imports deliberately return `None`.
    pub fn as_label(&self) -> Option<(&str, Span)> {
        match self {
            ImportItem::Label { name, span, .. } => Some((name.as_str(), *span)),
            ImportItem::Name { .. } | ImportItem::OperatorPattern { .. } => None,
        }
    }
}

/// Mint the generated nominal type spelling for a label. This helper is
/// phase-independent because projected registries and label lowering must
/// agree on the same declaration identity even in builds without the surface
/// lowering feature.
pub(crate) fn mint_label_newtype_name(label: &str) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModulePath {
    pub segments: Vec<PathSegment>,
    pub span: Span,
}

// ---- Declarations -------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FnDef<P: Phase = Surface> {
    pub vis: Visibility,
    pub purity: P::FnPurity,
    pub name: String,
    /// Ordered signature groups. Type-binder groups (`[A]`) and
    /// value-parameter groups (`(x: T)`) stay in source order; each
    /// value group is one callable layer.
    pub sig: Signature<P>,
    pub ret: Type<P>,
    /// Whether the source elided the `-> .` return-type
    /// annotation (the one optional type position in Kio, per
    /// language.md § Function definitions). When elided, `ret`
    /// carries a synthetic [`Type::Unit`] so downstream phases
    /// can treat the two spellings uniformly; the [`Surface`]
    /// formatter consults the `bool` to round-trip the original
    /// source shape. Narrows to `()` at [`Prime`] / [`Enriched`] —
    /// see [`Phase::FnDefRetElided`].
    pub ret_elided: P::FnDefRetElided,
    pub body: Expr<P>,
    pub meta: Meta<P>,
    /// Doc-comment attached to this `fn` definition, if any.
    pub doc: Option<DocComment>,
}

/// One entry in a signature group. [`Signature`] keeps the group
/// boundaries separately so adjacent value groups remain distinct
/// callable layers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SignatureParam<P: Phase = Surface> {
    Type(TypeParam),
    Value(Param<P>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SignatureGroupKind {
    Type { len: usize },
    Value { len: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureGroup<P: Phase = Surface> {
    Type(Vec<TypeParam>),
    Value(Vec<Param<P>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureGroupRef<'a, P: Phase = Surface> {
    Type(&'a [SignatureParam<P>]),
    Value(&'a [SignatureParam<P>]),
}

#[cfg(test)]
thread_local! {
    static SIGNATURE_GROUP_CURSOR_SIGNATURE_ROUTES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static SIGNATURE_GROUP_CURSOR_PARAMS_ROUTES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static SIGNATURE_GROUP_CURSOR_STEPS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_signature_group_cursor_test_counts() {
    SIGNATURE_GROUP_CURSOR_SIGNATURE_ROUTES.with(|count| count.set(0));
    SIGNATURE_GROUP_CURSOR_PARAMS_ROUTES.with(|count| count.set(0));
    SIGNATURE_GROUP_CURSOR_STEPS.with(|count| count.set(0));
}

#[cfg(test)]
fn signature_group_cursor_test_counts() -> (usize, usize, usize) {
    (
        SIGNATURE_GROUP_CURSOR_SIGNATURE_ROUTES.with(std::cell::Cell::get),
        SIGNATURE_GROUP_CURSOR_PARAMS_ROUTES.with(std::cell::Cell::get),
        SIGNATURE_GROUP_CURSOR_STEPS.with(std::cell::Cell::get),
    )
}

fn record_signature_group_cursor_step() {
    #[cfg(test)]
    SIGNATURE_GROUP_CURSOR_STEPS.with(|count| count.set(count.get() + 1));
}

fn record_signature_group_cursor_signature_route() {
    #[cfg(test)]
    SIGNATURE_GROUP_CURSOR_SIGNATURE_ROUTES.with(|count| count.set(count.get() + 1));
}

fn record_signature_group_cursor_params_route() {
    #[cfg(test)]
    SIGNATURE_GROUP_CURSOR_PARAMS_ROUTES.with(|count| count.set(count.get() + 1));
}

/// Borrowed, allocation-free traversal of a signature's canonical groups.
///
/// Stored group boundaries are authoritative. A legacy/constructed signature
/// with no stored groups infers the same singleton type groups and consecutive
/// value runs as [`Signature::new`], without first allocating a group vector.
/// Front and back offsets are independent so callers that build callable types
/// right-to-left do not need to collect before reversing the traversal.
pub(crate) struct SignatureGroupRefs<'a, P: Phase> {
    params: &'a [SignatureParam<P>],
    groups: &'a [SignatureGroupKind],
    stored: bool,
    front_param: usize,
    back_param: usize,
    front_group: usize,
    back_group: usize,
}

impl<'a, P: Phase> SignatureGroupRefs<'a, P> {
    fn from_signature(signature: &'a Signature<P>) -> Self {
        Self::new(&signature.params, &signature.groups)
    }

    fn inferred(params: &'a [SignatureParam<P>]) -> Self {
        Self::new(params, &[])
    }

    fn new(params: &'a [SignatureParam<P>], groups: &'a [SignatureGroupKind]) -> Self {
        Self {
            params,
            groups,
            stored: !groups.is_empty(),
            front_param: 0,
            back_param: params.len(),
            front_group: 0,
            back_group: groups.len(),
        }
    }

    fn stored_group(
        &self,
        group: SignatureGroupKind,
        start: usize,
        end: usize,
    ) -> SignatureGroupRef<'a, P> {
        debug_assert!(start <= end && end <= self.params.len());
        match group {
            SignatureGroupKind::Type { .. } => SignatureGroupRef::Type(&self.params[start..end]),
            SignatureGroupKind::Value { .. } => SignatureGroupRef::Value(&self.params[start..end]),
        }
    }
}

impl<'a, P: Phase> Iterator for SignatureGroupRefs<'a, P> {
    type Item = SignatureGroupRef<'a, P>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.stored {
            if self.front_group == self.back_group {
                debug_assert_eq!(
                    self.front_param, self.back_param,
                    "stored signature groups cover every parameter"
                );
                return None;
            }
            let group = self.groups[self.front_group];
            self.front_group += 1;
            let len = match group {
                SignatureGroupKind::Type { len } | SignatureGroupKind::Value { len } => len,
            };
            let start = self.front_param;
            let end = start
                .checked_add(len)
                .expect("signature group offset fits in usize");
            assert!(
                end <= self.back_param,
                "stored signature groups exceed their parameter slice"
            );
            self.front_param = end;
            record_signature_group_cursor_step();
            return Some(self.stored_group(group, start, end));
        }

        if self.front_param == self.back_param {
            return None;
        }
        let start = self.front_param;
        let first = self.params.get(start)?;
        let end = match first {
            SignatureParam::Type(_) => start + 1,
            SignatureParam::Value(_) => {
                let mut end = start + 1;
                while end < self.back_param && matches!(&self.params[end], SignatureParam::Value(_))
                {
                    end += 1;
                }
                end
            }
        };
        self.front_param = end;
        record_signature_group_cursor_step();
        Some(match first {
            SignatureParam::Type(_) => SignatureGroupRef::Type(&self.params[start..end]),
            SignatureParam::Value(_) => SignatureGroupRef::Value(&self.params[start..end]),
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if self.stored {
            let remaining = self.back_group - self.front_group;
            (remaining, Some(remaining))
        } else {
            let remaining_params = self.back_param - self.front_param;
            (usize::from(remaining_params != 0), Some(remaining_params))
        }
    }
}

impl<P: Phase> DoubleEndedIterator for SignatureGroupRefs<'_, P> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.stored {
            if self.front_group == self.back_group {
                debug_assert_eq!(
                    self.front_param, self.back_param,
                    "stored signature groups cover every parameter"
                );
                return None;
            }
            self.back_group -= 1;
            let group = self.groups[self.back_group];
            let len = match group {
                SignatureGroupKind::Type { len } | SignatureGroupKind::Value { len } => len,
            };
            let end = self.back_param;
            let start = end
                .checked_sub(len)
                .expect("stored signature groups fit their parameter slice");
            assert!(
                start >= self.front_param,
                "stored signature groups overlap their parameter slice"
            );
            self.back_param = start;
            record_signature_group_cursor_step();
            return Some(self.stored_group(group, start, end));
        }

        if self.front_param == self.back_param {
            return None;
        }
        let end = self.back_param;
        let last = self.params.get(end.checked_sub(1)?)?;
        let start = match last {
            SignatureParam::Type(_) => end - 1,
            SignatureParam::Value(_) => {
                let mut start = end - 1;
                while start > self.front_param
                    && matches!(&self.params[start - 1], SignatureParam::Value(_))
                {
                    start -= 1;
                }
                start
            }
        };
        self.back_param = start;
        record_signature_group_cursor_step();
        Some(match last {
            SignatureParam::Type(_) => SignatureGroupRef::Type(&self.params[start..end]),
            SignatureParam::Value(_) => SignatureGroupRef::Value(&self.params[start..end]),
        })
    }
}

impl<P: Phase> std::iter::FusedIterator for SignatureGroupRefs<'_, P> {}

/// Newtype wrapping a grouped signature — the unified
/// representation embedded by every signature-bearing AST node
/// ([`FnDef`], [`Equiv`], [`Expr::FnExpr`]). Exists to attach the derived
/// queries (System-F type assembly, value-param name enumeration,
/// first-layer arity, …) as discoverable methods rather than free
/// functions scattered across `ast.rs` and `pass/typecheck_core`.
///
/// Value-group boundaries are semantic: each one introduces a distinct
/// callable layer. Type binders are stored as singleton groups because
/// their source grouping is not observable — signature synthesis introduces
/// one `Forall` per binder, and Kio has no bracket type-application syntax
/// ([`specs/language.md`](../../specs/language.md#type-system)). The flattened
/// `params` vector preserves source order for phase walkers; `groups` records
/// the callable layers and the canonical type-binder sequence.
///
/// `ret` lives on the embedding struct, not here, because not every
/// embedder has one: `FnDef` carries a
/// `ret: Type<P>` and a `ret_elided: bool`, `Expr::FnExpr` carries
/// `ret_ty: Option<Type<P>>`, and `Equiv` has no
/// return slot at all. Keeping `ret` per-embedder avoids stuffing a
/// synthetic `Type::Unit` into `Signature` for the positions that
/// don't have one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Signature<P: Phase = Surface> {
    pub params: Vec<SignatureParam<P>>,
    pub groups: Vec<SignatureGroupKind>,
}

impl<P: Phase> Signature<P> {
    pub fn new(params: Vec<SignatureParam<P>>) -> Self {
        let groups = infer_signature_groups(&params);
        Self { params, groups }
    }

    pub fn from_parts(params: Vec<SignatureParam<P>>, groups: Vec<SignatureGroupKind>) -> Self {
        let groups = if groups.is_empty() {
            infer_signature_groups(&params)
        } else {
            canonical_type_groups(groups)
        };
        debug_assert!(signature_groups_match(&params, &groups));
        Self { params, groups }
    }

    pub fn from_groups(groups: Vec<SignatureGroup<P>>) -> Self {
        let mut params = Vec::new();
        let mut group_kinds = Vec::new();
        for group in groups {
            match group {
                SignatureGroup::Type(items) => {
                    group_kinds.extend(type_binder_groups(items.len()));
                    params.extend(items.into_iter().map(SignatureParam::Type));
                }
                SignatureGroup::Value(items) => {
                    group_kinds.push(SignatureGroupKind::Value { len: items.len() });
                    params.extend(items.into_iter().map(SignatureParam::Value));
                }
            }
        }
        Self {
            params,
            groups: group_kinds,
        }
    }

    pub(crate) fn canonical_group_refs(&self) -> SignatureGroupRefs<'_, P> {
        SignatureGroupRefs::from_signature(self)
    }

    pub fn canonical_groups(&self) -> Vec<SignatureGroupRef<'_, P>> {
        self.canonical_group_refs().collect()
    }

    /// Build the full System-F type for this signature against a
    /// supplied return type and span: applies the same right-fold
    /// rule as [`Type::synth_scheme_from_signature_params`].
    ///
    /// The `ret` parameter is passed in (not stored on `Signature`)
    /// so the same method works for embedders whose return-slot
    /// shape differs (`Fn` has a `Type<P>`, `Expr::FnExpr` has an
    /// `Option<Type<P>>`, etc.).
    pub fn signature_ty(&self, ret: Type<P>, span: Span) -> Type<P>
    where
        Type<P>: Clone,
    {
        Type::synth_scheme_from_signature(self, ret, span)
    }

    /// True iff the signature declares at least one type-parameter
    /// binder (`[A]`). Used by the typer to decide between
    /// `Synth::value` and `Synth::scheme` at synthesis sites.
    pub fn has_type_params(&self) -> bool {
        self.params
            .iter()
            .any(|p| matches!(p, SignatureParam::Type(_)))
    }

    pub(crate) fn has_value_group(&self) -> bool {
        if self.groups.is_empty() {
            self.params
                .iter()
                .any(|param| matches!(param, SignatureParam::Value(_)))
        } else {
            self.groups
                .iter()
                .any(|group| matches!(group, SignatureGroupKind::Value { .. }))
        }
    }

    /// Iterate the value-parameters in source order.
    pub fn value_params(&self) -> impl Iterator<Item = &Param<P>> {
        self.params.iter().filter_map(|p| match p {
            SignatureParam::Value(vp) => Some(vp),
            SignatureParam::Type(_) => None,
        })
    }

    /// Iterate the value-parameter names in source order.
    pub fn value_param_names(&self) -> impl Iterator<Item = &str> {
        self.value_params().map(|vp| vp.name.as_str())
    }

    /// Total count of value-parameters in the signature, summed
    /// across every curry layer.
    pub fn value_param_count(&self) -> usize {
        self.value_params().count()
    }

    pub fn value_group_count(&self) -> usize {
        self.canonical_group_refs()
            .filter(|group| matches!(group, SignatureGroupRef::Value(_)))
            .count()
    }
}

fn type_binder_groups(len: usize) -> impl Iterator<Item = SignatureGroupKind> {
    std::iter::repeat_n(SignatureGroupKind::Type { len: 1 }, len)
}

fn canonical_type_groups(groups: Vec<SignatureGroupKind>) -> Vec<SignatureGroupKind> {
    let mut canonical = Vec::with_capacity(groups.len());
    for group in groups {
        match group {
            SignatureGroupKind::Type { len } => canonical.extend(type_binder_groups(len)),
            value @ SignatureGroupKind::Value { .. } => canonical.push(value),
        }
    }
    canonical
}

fn infer_signature_groups<P: Phase>(params: &[SignatureParam<P>]) -> Vec<SignatureGroupKind> {
    let mut groups = Vec::new();
    for param in params {
        match param {
            SignatureParam::Type(_) => groups.push(SignatureGroupKind::Type { len: 1 }),
            SignatureParam::Value(_) => match groups.last_mut() {
                Some(SignatureGroupKind::Value { len }) => *len += 1,
                _ => groups.push(SignatureGroupKind::Value { len: 1 }),
            },
        }
    }
    groups
}

fn signature_groups_match<P: Phase>(
    params: &[SignatureParam<P>],
    groups: &[SignatureGroupKind],
) -> bool {
    let mut offset = 0usize;
    for group in groups {
        let len = match *group {
            SignatureGroupKind::Type { len } | SignatureGroupKind::Value { len } => len,
        };
        let Some(end) = offset.checked_add(len) else {
            return false;
        };
        if end > params.len() {
            return false;
        }
        let matches_group = params[offset..end].iter().all(|param| {
            matches!(
                (group, param),
                (SignatureGroupKind::Type { .. }, SignatureParam::Type(_))
                    | (SignatureGroupKind::Value { .. }, SignatureParam::Value(_))
            )
        });
        if !matches_group {
            return false;
        }
        offset = end;
    }
    offset == params.len()
}

/// The kind of a type binder or type constructor — its type-level
/// arity. The kind language is a right-associative chain over the
/// single base kind `*`; there are no kind variables and no
/// higher-order kinds (the domain of every arrow is `*`). See
/// [`specs/grammar.md` § Kind grammar].
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    /// `*` — the kind of an ordinary (saturated) type.
    Star,
    /// `* → κ` — a type-level function that consumes one kind-`*`
    /// argument and yields a type of kind `κ`. Right-associative:
    /// `*→*→*` is `Arrow(Star, Arrow(Star, Star))`.
    Arrow(Box<Kind>, Box<Kind>),
}

impl Kind {
    /// The right-associative arrow chain with `n` arrows over `*`.
    /// `arrow_chain(0) = *`, `arrow_chain(1) = *→*`,
    /// `arrow_chain(2) = *→*→*`, …
    pub fn arrow_chain(n: usize) -> Self {
        let mut k = Kind::Star;
        for _ in 0..n {
            k = Kind::Arrow(Box::new(Kind::Star), Box::new(k));
        }
        k
    }

    /// The number of leading arrows — the type-level arity. `*` ⇒ 0,
    /// `*→*` ⇒ 1, `*→*→*` ⇒ 2, …
    pub fn arity(&self) -> usize {
        match self {
            Kind::Star => 0,
            Kind::Arrow(_, codomain) => 1 + codomain.arity(),
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Kind::Star => write!(f, "*"),
            Kind::Arrow(_, _) => {
                // Every domain is `*`, so the chain is fully described
                // by its arity: render as `*→*→…→*`.
                for _ in 0..self.arity() {
                    write!(f, "*→")?;
                }
                write!(f, "*")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TypeParam {
    pub name: String,
    pub span: Span,
    /// The binder's kind annotation. `None` means the default kind
    /// `*` (the user wrote `[Name]` with no leading stars);
    /// `Some(kind)` is the explicit higher kind from a starred binder
    /// (`[*F]` ⇒ `Some(*→*)`, `[**G]` ⇒ `Some(*→*→*)`). Kinds are
    /// never inferred — this field reflects exactly what the user
    /// wrote. See [`specs/grammar.md` § Kind grammar].
    pub kind: Option<Kind>,
}

impl TypeParam {
    /// The binder's effective kind — its annotation, defaulting to
    /// `*` when unannotated.
    pub fn effective_kind(&self) -> Kind {
        self.kind.clone().unwrap_or(Kind::Star)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Param<P: Phase = Surface> {
    pub name: String,
    /// `Some(t)` when the value parameter carries a type annotation;
    /// `None` for an anonymous-lambda parameter whose whole type slot
    /// must be supplied by an expected function type or its direct
    /// application.
    pub ty: Option<Type<P>>,
    /// Surface-only destructuring pattern. `Some(...)` when the
    /// user wrote a tuple-pattern parameter `(a: A, b: B)` (bare
    /// form, `name` is a synthesized outer name) or `name: (a: A,
    /// b: B)` (as-pattern, `name` is the user-given outer name);
    /// `None` for ordinary `name: T` params. The desugar pass
    /// rewrites every pattern-bearing param into a plain `name:
    /// <synth-product-type>` slot plus projection lets and drops the
    /// field — every non-Surface phase witnesses `()`.
    /// See [`specs/language.md` § Parameter patterns].
    pub pattern: P::ParamPatternExt,
    pub meta: Meta<P>,
}

/// Surface-only destructuring pattern attached to a [`Param`] —
/// the comma-separated element list inside the pattern's outer
/// `(...)`. See [`specs/language.md` § Parameter patterns] for the
/// surface form and [`ParamPatternElem`] for the per-slot shape.
///
/// `match_id` is the [`NodeId`] used by pattern lowering artifacts.
/// Parameter patterns stamp it onto the generated `match!`; statement
/// patterns use it when minting the outer product name. The parser
/// owns the ID so downstream lowering never mixes parser- and
/// desugar-minted IDs for the same source pattern.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ParamPattern {
    pub elems: Vec<ParamPatternElem>,
    pub span: Span,
    pub match_id: NodeId,
}

/// One slot in a [`ParamPattern`]'s element list.
///
/// Wildcards (`_: T`) are represented as [`ParamPatternElem::Bind`]
/// with `name == "_"` — the lexer admits the pure-underscore slot
/// the same way it does in any other binder position, and the
/// desugar pass keeps the `_` name when it flattens the pattern
/// into the inner clause's `fn` params (where `_` is also a
/// wildcard).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ParamPatternElem {
    /// `name: T` or `_: T` — names (or skips) a slot with its type.
    Bind {
        name: String,
        name_span: Span,
        ty: Type<Surface>,
    },
    /// `(<elems>)` — a nested bare tuple. The desugar pass mints
    /// a synthetic outer name for this level when it flattens.
    Tuple(ParamPattern),
    /// `name: (<elems>)` — a nested as-pattern. Binds `name` to
    /// the nested product *and* descends into it.
    BindTuple {
        name: String,
        name_span: Span,
        inner: ParamPattern,
    },
}

impl ParamPattern {
    /// Build the right-folded product type induced by this pattern's slots.
    pub fn outer_type(&self) -> Type<Surface> {
        let leaves = self
            .elems
            .iter()
            .map(ParamPatternElem::outer_type)
            .collect();
        build_product_right_fold(leaves, self.span)
    }
}

impl ParamPatternElem {
    fn outer_type(&self) -> Type<Surface> {
        match self {
            Self::Bind { ty, .. } => ty.clone(),
            Self::Tuple(pattern) | Self::BindTuple { inner: pattern, .. } => pattern.outer_type(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Newtype<P: Phase = Surface> {
    pub vis: Visibility,
    /// Exact `rec` keyword span for a recursive singleton declaration.
    /// Group members leave this unset because the enclosing group owns scope.
    #[serde(default)]
    pub rec_span: Option<Span>,
    pub name: String,
    /// Exact span of the declared type name. Kept separately from `meta`
    /// so diagnostics can point at the head rather than the whole item.
    pub name_span: Span,
    pub type_params: Vec<TypeParam>,
    /// Existential type binders, written as whitespace-separated
    /// `<X>` atoms trailing the universal-parameter list on the newtype
    /// header (e.g., `newtype Box[A] <U> <V> : A & U & V`). Empty
    /// for the common no-existential case. The names are in scope
    /// inside the payload only; each projector call freshly skolemizes
    /// them inside the CPS continuation.
    pub existential_params: Vec<TypeParam>,
    pub payload: Type<P>,
    pub constructor: TypeMember<P>,
    pub projector: TypeMember<P>,
    pub meta: Meta<P>,
    /// Exact complete source range that an editor may move or wrap as this
    /// declaration. Parsed Surface declarations include visibility and an
    /// attached `///` block; synthesized declarations and persistent Prime
    /// artifacts leave it unset.
    #[serde(default)]
    pub editable_span: Option<Span>,
    /// Doc-comment attached to this `newtype` declaration, if any.
    pub doc: Option<DocComment>,
}

/// The exact part of a public newtype that crosses the host boundary.
///
/// An opaque newtype carries neither its hidden members nor its payload;
/// every payload-bearing state carries exactly the member or members that
/// expose it. The view borrows the phase-specific AST, so the same projection
/// is used before and after lowering without copying or erasing type facts.
pub(crate) enum NewtypeHostSurface<'a, P: Phase> {
    Opaque,
    Constructor {
        constructor: &'a TypeMember<P>,
        payload: &'a Type<P>,
    },
    Projector {
        projector: &'a TypeMember<P>,
        payload: &'a Type<P>,
    },
    Both {
        constructor: &'a TypeMember<P>,
        projector: &'a TypeMember<P>,
        payload: &'a Type<P>,
    },
}

impl<'a, P: Phase> NewtypeHostSurface<'a, P> {
    pub(crate) fn payload(&self) -> Option<&'a Type<P>> {
        match self {
            Self::Opaque => None,
            Self::Constructor { payload, .. }
            | Self::Projector { payload, .. }
            | Self::Both { payload, .. } => Some(payload),
        }
    }
}

impl<P: Phase> Newtype<P> {
    /// The backend-independent FFI key identity for this newtype: the
    /// newtype's own type name. Each backend decides how that identity
    /// is rendered at its host boundary.
    pub fn ffi_key(&self) -> &str {
        &self.name
    }

    /// Whether the nominal type itself belongs to the host contract.
    pub(crate) fn is_host_exported(&self) -> bool {
        self.vis.is_exported()
    }

    /// Project this declaration onto its public host surface. A private or
    /// scoped outer declaration has no host surface at all.
    pub(crate) fn host_surface(&self) -> Option<NewtypeHostSurface<'_, P>> {
        if !self.is_host_exported() {
            return None;
        }

        match (
            self.constructor.vis.is_exported(),
            self.projector.vis.is_exported(),
        ) {
            (false, false) => Some(NewtypeHostSurface::Opaque),
            (true, false) => Some(NewtypeHostSurface::Constructor {
                constructor: &self.constructor,
                payload: &self.payload,
            }),
            (false, true) => Some(NewtypeHostSurface::Projector {
                projector: &self.projector,
                payload: &self.payload,
            }),
            (true, true) => Some(NewtypeHostSurface::Both {
                constructor: &self.constructor,
                projector: &self.projector,
                payload: &self.payload,
            }),
        }
    }
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TypeMember<P: Phase = Surface> {
    pub vis: Visibility,
    pub name: String,
    pub span: Span,
    pub leading_trivia: P::LeadingTrivia,
}

impl<P: Phase> Clone for TypeMember<P> {
    fn clone(&self) -> Self {
        Self {
            vis: self.vis.clone(),
            name: self.name.clone(),
            span: self.span,
            leading_trivia: self.leading_trivia.clone(),
        }
    }
}

pub fn convert_type_member<P: Phase, Q: Phase>(m: &TypeMember<P>) -> TypeMember<Q> {
    TypeMember {
        vis: m.vis.clone(),
        name: m.name.clone(),
        span: m.span,
        leading_trivia: Q::LeadingTrivia::default(),
    }
}

/// `labels { f: X, g[A]: Y };` and `labels T = { ... } | { ... };` —
/// the surface label-declaration form, sugar over `newtype`. The named
/// form additionally introduces a structural `alias` over the written
/// product/sum of generated label types. Lowered during label-elab.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Labels<P: Phase = Surface> {
    pub vis: Visibility,
    /// Exact `rec` keyword span for a recursive singleton declaration.
    #[serde(default)]
    pub rec_span: Option<Span>,
    /// `labels T = { … };` carries `Some("T")`; the anonymous form
    /// `labels { … };` carries `None`.
    pub type_alias_name: Option<String>,
    pub type_alias_span: Option<Span>,
    /// Type parameters of the named form (`labels T[A] = { … };`).
    /// Empty for the anonymous form and for nullary named forms.
    pub type_alias_params: Vec<TypeParam>,
    /// Product arms written by the named form. `None` for anonymous
    /// `labels { ... };`; `Some([arm])` for `labels T = { ... };`; more
    /// than one arm for named sums.
    pub type_alias_arms: Option<Vec<LabelsArm<P>>>,
    pub entries: Vec<LabelEntry<P>>,
    pub meta: Meta<P>,
    /// Exact complete source range that an editor may move or wrap as this
    /// declaration. It excludes unattached plain comments.
    #[serde(default)]
    pub editable_span: Option<Span>,
    /// Doc-comment attached to this `labels` declaration, if any.
    pub doc: Option<DocComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabelForward<P: Phase = Surface> {
    pub vis: Visibility,
    pub name: String,
    pub name_span: Span,
    pub target: String,
    pub target_span: Span,
    pub body_trivia: P::LeadingTrivia,
    pub meta: Meta<P>,
    #[serde(default)]
    pub editable_span: Option<Span>,
    pub doc: Option<DocComment>,
}

#[cfg(feature = "surface")]
impl<P: Phase> Labels<P> {
    pub(crate) fn arms_in_source_order(&self) -> impl Iterator<Item = &[LabelEntry<P>]> {
        self.type_alias_arms
            .iter()
            .flat_map(|arms| arms.iter().map(|arm| arm.entries.as_slice()))
            .chain(
                self.type_alias_arms
                    .is_none()
                    .then_some(self.entries.as_slice()),
            )
    }

    pub(crate) fn unbound_entry_universal<'a>(
        &self,
        entry: &'a LabelEntry<P>,
    ) -> Option<&'a TypeParam> {
        entry.type_params.iter().find(|entry_param| {
            !self.type_alias_params.iter().any(|alias_param| {
                alias_param.name == entry_param.name
                    && alias_param.effective_kind() == entry_param.effective_kind()
            })
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabelsArm<P: Phase = Surface> {
    pub entries: Vec<LabelEntry<P>>,
    pub meta: Meta<P>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabelEntry<P: Phase = Surface> {
    pub name: String,
    pub name_span: Span,
    /// The label's own type parameters: `f[A][B]: X`. Independent
    /// from the `type_alias_params` on the named form.
    pub type_params: Vec<TypeParam>,
    /// Per-entry existential type binders, written `<L> <R>` as
    /// whitespace-separated atoms after the universal `[A]` binder.
    /// Threaded through label-elab onto the generated
    /// `Newtype.existential_params`.
    pub existential_params: Vec<TypeParam>,
    pub payload: Type<P>,
    pub meta: Meta<P>,
}

impl<P: Phase> LabelEntry<P> {
    /// Whether this entry is the exact surface spelling `name[...]: _`.
    /// Label elaboration consumes this marker; it never reaches Lowered.
    pub fn is_reuse_marker(&self) -> bool {
        matches!(self.payload, Type::Infer { .. })
    }

    #[cfg(feature = "surface")]
    pub(crate) fn universal_header_matches(&self, other: &Self) -> bool {
        self.type_params.len() == other.type_params.len()
            && self
                .type_params
                .iter()
                .zip(&other.type_params)
                .all(|(first, second)| first.effective_kind() == second.effective_kind())
    }
}

/// Return the sole generated nominal behind a named `labels` declaration
/// exactly when label elaboration emits a fully saturated positional identity
/// alias for it. The entry's written parameter names matter: `T[A, B] =
/// Foo(B, A)` is not an identity alias even when both binders have the same
/// kinds.
#[cfg(feature = "surface")]
pub(crate) fn named_labels_identity_alias_entry<P: Phase>(
    labels: &Labels<P>,
) -> Option<&LabelEntry<P>> {
    labels.type_alias_name.as_ref()?;
    let [arm] = labels.type_alias_arms.as_deref()? else {
        return None;
    };
    let [entry] = arm.entries.as_slice() else {
        return None;
    };
    (entry.type_params.len() == labels.type_alias_params.len()
        && entry
            .type_params
            .iter()
            .zip(&labels.type_alias_params)
            .all(|(entry, alias)| entry.name == alias.name))
    .then_some(entry)
}

/// `equiv name[A](x: A) { <expr>; <expr>; ... }` — an equivalence
/// claim across N expressions. The `kio test` runner discharges each
/// `equiv` by partial-evaluating every arm and checking they share a
/// residual normal form (alpha+eta).
///
/// Parameter list shape mirrors `FnDef` exactly — type and value
/// parameters mixed positionally. Names are required (no anonymous
/// form). At least two arms are required; `N≤1` is a parse error.
/// Surface-only — filtered before codegen.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Equiv<P: Phase = Surface> {
    pub name: String,
    pub name_span: Span,
    pub sig: Signature<P>,
    pub terms: Vec<EquivTerm<P>>,
    pub meta: Meta<P>,
}

/// One arm of an `equiv` block: `<expr>`. `leading_trivia` carries
/// any line comments captured before the arm body.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EquivTerm<P: Phase = Surface> {
    pub body: Expr<P>,
    pub meta: Meta<P>,
}

// ---- Package files (`<name>.pkg.kio`) ---------------------------------------

/// Parsed `<name>.pkg.kio` file. The package file declares the package
/// name, the optional `build` block, and an optional `bridge` block
/// listing the module-path globs whose `pub` items form the host
/// contract surface. The env / export surface is **derived** by
/// scanning the bridged modules; there is no per-item ledger.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackageFile<P: Phase = Surface> {
    pub name: String,
    pub build: Option<BuildBlock>,
    pub bridge: Option<BridgeBlock>,
    pub meta: Meta<P>,
}

// ---- Dependency files (`<local>.dep.kio`) -----------------------------------

/// Parsed `<local>.dep.kio` file. Declares one direct cross-package
/// dependency and where to fetch it. The filename stem is the
/// dependency's **local name** — the alias the consumer chooses for it,
/// not necessarily the dependency's own package name — and becomes a
/// synthetic module root under which the dependency's tree re-roots.
///
/// Opens with a `dependency <local>;` header (its `<local>` matches the
/// filename stem, the same coherence check `package <name>;` runs),
/// followed by exactly one [`SourceBlock`] naming the dependency's
/// `*.pkg.kio` file. A file-shape node sibling to [`PackageFile`] /
/// [`SignatureFile`]; carries a `Meta<P>` so it round-trips like them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DependencyFile<P: Phase = Surface> {
    /// The dependency's local name from the `dependency <local>;`
    /// header. Must match the filename stem (`<local>.dep.kio`).
    pub name: String,
    /// The `source { … }` block: where the dependency is fetched from.
    pub source: SourceBlock,
    /// Zero or more `rehost <dep>/<mod> to <local>/<mod>;` statements,
    /// each rebinding a dependency module's `host` items onto a consumer
    /// module. Empty when the file declares none. `kio fmt` emits them
    /// after the `source` block, sorted lexicographically by `from`.
    pub rehost: Vec<RehostDecl>,
    /// Zero or more `retype <dep>/<mod> to <local>/<mod>;` statements,
    /// each remapping a dependency module's `newtype`s onto the consumer's
    /// same-named counterparts. Empty when the file declares none. `kio
    /// fmt` emits them after the `rehost` statements, sorted
    /// lexicographically by `from`.
    pub retype: Vec<RetypeDecl>,
    pub meta: Meta<P>,
}

/// A `source { … }` block: where a dependency is rooted. Reused from the
/// removed single-module source mechanism, re-aimed from naming a single
/// *module file* to naming a whole *package* (the `path` points at the
/// dependency's `*.pkg.kio`). **Phase-independent** — it carries only raw
/// origin syntax, so it is the same node at every phase and never walks
/// across a phase boundary (like [`BridgeBlock`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceBlock {
    pub trailing_trivia: Vec<crate::pass::lexer::Trivia>,
    pub origin: SourceOrigin,
    pub span: Span,
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
}

/// The origin inside a [`SourceBlock`]: the local-`path` form or the
/// remote `git` form.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SourceOrigin {
    /// `source { path "<rel>/<name>.pkg.kio"; }` — a local dependency:
    /// `path` is a filesystem path, relative to the consumer's package
    /// root, naming the dependency's package file (always spelled with
    /// its `.pkg.kio` extension). May climb `../`. Spelled with `/`
    /// separators; an absolute path or a `\\` separator is a parse error.
    Path {
        path: String,
        path_span: Span,
        path_leading_trivia: Vec<crate::pass::lexer::Trivia>,
    },
    /// `source { git "<url>"; ref "<rev|tag|branch>"; path "<manifest>"; }` — a remote
    /// dependency fetched from a git repository. `url` is the clone URL
    /// (`file://`, `https://`, `git@…`, …); `git_ref` is a single
    /// revision designator git resolves uniformly — a branch name, a
    /// tag, or a commit SHA. The optional `path` selects a manifest inside
    /// the checkout. Fields are unordered; the formatter emits `git`,
    /// `ref`, then the optional `path`.
    ///
    /// The exact commit a floating ref resolved to is pinned in the
    /// dependency's `<local>.lock.kio` file ([`LockFile`]); when that
    /// lockfile is present, materialization checks out the locked commit
    /// and never re-resolves the ref.
    Git(GitSource),
}

/// A Git origin and its optional repository-relative package manifest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitSource {
    pub url: String,
    pub url_span: Span,
    pub url_leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub git_ref: String,
    pub ref_span: Span,
    pub ref_leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub manifest_path: Option<GitManifestPath>,
}

/// One present `path` field, retaining its diagnostic and formatter metadata.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitManifestPath {
    pub path: String,
    pub span: Span,
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
}

/// A `rehost <dep>/<mod> to <local>/<mod>;` statement in a `.dep.kio`
/// file. Rebinds the `host` items declared in the dependency module
/// `from` (written with the dependency's local name as its first
/// segment) onto the consumer's local module `to`, which must provide a
/// type-compatible item for each. Applied during materialization: the
/// re-rooted `from` module's host items are replaced by imports of `to`'s
/// items, so the dependency's code binds to the consumer's providers
/// instead of expecting the host to supply them under the re-rooted path.
/// **Phase-independent** — raw path syntax, like [`SourceBlock`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RehostDecl {
    /// The dependency module whose `host` items are rebound, written with
    /// the dependency's local name as its first segment (`<dep>/<mod>`).
    pub from: ModulePath,
    /// The consumer's local module that provides the rebound items.
    pub to: ModulePath,
    pub span: Span,
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
}

/// A `retype <dep>/<mod> to <local>/<mod>;` statement in a `.dep.kio`
/// file. Remaps the `newtype`s declared in the dependency module `from`
/// (written with the dependency's local name as its first segment) onto
/// the consumer's same-named `newtype`s in the local module `to`.
///
/// Two forms. The **module** form (`type_name = None`) remaps *every*
/// newtype declared under `from` to the same-named newtype under `to`.
/// The **per-type** form (`type_name = Some(T)`, spelled `<from>.T to
/// <to>.T`) remaps the single newtype `T`. Either way the matched target
/// has the **same last-segment name** and must be **structurally
/// congruent** (payloads compared modulo module paths, unwrapping nominal
/// boundaries, bottoming at host-type leaves); `from`'s newtype set must
/// be a subset of `to`'s same-named newtypes. After module-form expansion,
/// each exact source `(module, name)` may be selected by only one statement.
///
/// Applied during materialization, like [`RehostDecl`]: the re-rooted
/// `from` module's matched newtype declarations are replaced by imports
/// of `to`'s newtypes, so the dependency's code (and the consumer) bind
/// to one shared nominal type instead of two distinct same-named copies.
/// Collapsing the two copies of a diamond dependency is the motivating
/// case. **Phase-independent** — raw path syntax, like [`SourceBlock`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RetypeDecl {
    /// The dependency module whose `newtype`s are remapped, written with
    /// the dependency's local name as its first segment (`<dep>/<mod>`).
    pub from: ModulePath,
    /// The consumer's local module that provides the same-named
    /// counterparts.
    pub to: ModulePath,
    /// The single newtype name when this is the per-type form (`<from>.T
    /// to <to>.T`); `None` for the module form (remap every newtype under
    /// `from`). When present, the same name is the last segment on both
    /// the `from` and `to` sides at parse time.
    pub type_name: Option<String>,
    pub span: Span,
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
}

// ---- Dependency lock files (`<local>.lock.kio`) -----------------------------

/// Parsed `<local>.lock.kio` file. Records what a remote
/// [`SourceOrigin::Git`] dependency's floating `ref` resolved to — the
/// pin that makes a git dependency reproducible. It sits beside the
/// `<local>.dep.kio` it locks (same stem) and **is committed** by users:
/// the lockfile is the pin, recording the resolved commit, so it belongs
/// in version control alongside the committed materialized module tree.
///
/// Opens with a `lock <local>;` header (its `<local>` matches the
/// filename stem, the same coherence check `dependency <local>;` runs),
/// followed by one `resolved { … }` block recording the resolved
/// `(git, ref, optional path, commit, sig)`. A file-shape node sibling to
/// [`DependencyFile`]; carries a `Meta<P>` so it round-trips like the
/// dependency file.
///
/// The `resolved { … }` block records the dependency's
/// **contract-surface digest** (`sig "<…>";`) alongside the commit: the
/// pinned commit's sealed contract surface (when the dependency ships a
/// `<pkg>.sig.kio`) or its bridge-reachable live surface (when it does
/// not), hashed canonically. `kio dep update` recomputes the digest at
/// the new commit and runs the contract-compatibility honesty gate (see
/// [`crate::git_dep::update_git_lock`] and
/// [`specs/versioning.md`](../../specs/versioning.md)).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LockFile<P: Phase = Surface> {
    pub field_leading_trivia: std::collections::BTreeMap<String, Vec<crate::pass::lexer::Trivia>>,
    pub resolved_leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub trailing_trivia: Vec<crate::pass::lexer::Trivia>,
    /// The dependency's local name from the `lock <local>;` header. Must
    /// match the filename stem (`<local>.lock.kio`) and the stem of the
    /// `<local>.dep.kio` it pins.
    pub name: String,
    /// The clone URL the lock pins (echoes the dependency's `git` key).
    pub url: String,
    /// The revision designator the dependency requested (echoes the
    /// dependency's `ref` key) — recorded so a changed `ref` in the
    /// `.dep.kio` is detectable against the lock.
    pub git_ref: String,
    /// The repository-relative manifest selector, absent for discovery.
    pub manifest_path: Option<String>,
    /// The exact commit SHA the `ref` resolved to. This is the pin
    /// materialization checks out.
    pub commit: String,
    /// The dependency's contract-surface digest at the pinned commit — a
    /// canonical hash of its sealed (or, sig-less, live) contract surface.
    /// `kio dep update` compares the old commit's contract against the
    /// new commit's before re-pinning; the recorded digest is the new
    /// commit's.
    pub sig: String,
    pub meta: Meta<P>,
}

/// `host type Foo;` / `host type Box[A];` / `host type Count role(i32);`
/// — a host-supplied
/// opaque type declared at module level. Declaration only, no body. The
/// host fills in the concrete type at invocation time. Host type parameters
/// have the ordinary kind `*`; higher-kinded binders are not admitted on an
/// opaque host declaration. The optional
/// `role(...)` annotation makes the type a receiver for source-level
/// literals whose shape its role admits (and, for `role(bool)`, the
/// type that `if`/`else` desugars against). Type parameters and a role are
/// mutually exclusive: role-bearing host types are always nullary.
///
/// The optional `{ owned }` block after the role annotation remains accepted
/// and round-tripped for source compatibility. It selects no alternate
/// representation: Rust renders a `role(str)` facade by value at every
/// occurrence whether the block is present or not (see
/// [`specs/backends/rust.md`](../../specs/backends/rust.md) § FFI surface >
/// Atomic types). The flag has no runtime meaning on other backends.
///
/// **Inhabited at every phase**; carries a `Meta<P>` so it round-trips
/// like [`FnDef`] / [`Newtype`] and lives in `module.items`. Always
/// public — host items have no private form.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HostType<P: Phase = Surface> {
    pub name: String,
    pub type_params: Vec<TypeParam>,
    pub role: Option<RoleAnnotation>,
    /// `true` when the declaration carries the source-compatible `{ owned }`
    /// annotation. See [`HostType`] doc; the flag selects no alternate facade.
    pub owned: bool,
    pub meta: Meta<P>,
    /// Doc-comment attached to this `host type` declaration, if any.
    pub doc: Option<DocComment>,
}

/// The package's `bridge { … }` block — a `;`-separated list of
/// module-path globs. Each glob selects **modules** (not items); from
/// every matched module, all `pub` items form the host contract surface
/// (`pub host` → env, other `pub` → exports). **Phase-independent**:
/// the block carries only raw glob syntax, so it is the same node at
/// every phase and never walks across a phase boundary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BridgeBlock {
    pub globs: Vec<BridgeGlob>,
    pub span: Span,
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
    /// Comments dangling before the block's closing `}` — emitted
    /// before the closer so a trailing comment inside `bridge { … }`
    /// survives a `kio fmt` round-trip.
    #[serde(default)]
    pub trailing_trivia: Vec<crate::pass::lexer::Trivia>,
}

/// One module-path glob in a [`BridgeBlock`]. A `/`-separated run of
/// segments; each segment is a literal module name, `*` (one segment),
/// or `**` (zero-or-more segments), following shell glob semantics.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BridgeGlob {
    pub segments: Vec<BridgeGlobSegment>,
    pub span: Span,
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
}

/// One segment of a [`BridgeGlob`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BridgeGlobSegment {
    /// A literal module-name segment.
    Literal(String),
    /// `*` — matches exactly one path segment.
    Star,
    /// `**` — matches zero or more path segments.
    DoubleStar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Role {
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    U128,
    F32,
    F64,
    Bool,
    Str,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::I8 => "i8",
            Role::I16 => "i16",
            Role::I32 => "i32",
            Role::I64 => "i64",
            Role::I128 => "i128",
            Role::U8 => "u8",
            Role::U16 => "u16",
            Role::U32 => "u32",
            Role::U64 => "u64",
            Role::U128 => "u128",
            Role::F32 => "f32",
            Role::F64 => "f64",
            Role::Bool => "bool",
            Role::Str => "str",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Role> {
        Some(match s {
            "i8" => Role::I8,
            "i16" => Role::I16,
            "i32" => Role::I32,
            "i64" => Role::I64,
            "i128" => Role::I128,
            "u8" => Role::U8,
            "u16" => Role::U16,
            "u32" => Role::U32,
            "u64" => Role::U64,
            "u128" => Role::U128,
            "f32" => Role::F32,
            "f64" => Role::F64,
            "bool" => Role::Bool,
            "str" => Role::Str,
            _ => return None,
        })
    }

    /// Whether this role's values exceed JavaScript's exact-integer
    /// range (53-bit mantissa) — the `i64` / `i128` / `u64` / `u128`
    /// roles. The JS backend emits literals of these roles as BigInt.
    pub fn is_wide_int(self) -> bool {
        matches!(self, Role::I64 | Role::I128 | Role::U64 | Role::U128)
    }

    /// The literal-shape category this role belongs to. Drives the
    /// literal admission relation (see [`RoleShape::admits`]).
    pub fn shape(self) -> RoleShape {
        match self {
            Role::I8
            | Role::I16
            | Role::I32
            | Role::I64
            | Role::I128
            | Role::U8
            | Role::U16
            | Role::U32
            | Role::U64
            | Role::U128 => RoleShape::Int,
            Role::F32 | Role::F64 => RoleShape::Float,
            Role::Str => RoleShape::Str,
            Role::Bool => RoleShape::Bool,
        }
    }
}

/// The four literal-shape categories. A source literal token has a
/// shape (an integer literal is [`RoleShape::Int`], a float literal
/// [`RoleShape::Float`], …), and so does every role (see
/// [`Role::shape`]). The literal admission relation pairs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RoleShape {
    Int,
    Float,
    Str,
    Bool,
}

impl RoleShape {
    /// Whether a literal of `self`'s shape may be typed against a host
    /// type carrying a role of shape `role_shape`. An integer literal
    /// admits int-shaped *and* float-shaped roles; every other literal
    /// shape admits only its own.
    pub fn admits(self, role_shape: RoleShape) -> bool {
        match self {
            RoleShape::Int => matches!(role_shape, RoleShape::Int | RoleShape::Float),
            other => other == role_shape,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RoleAnnotation {
    pub role: Role,
    pub span: Span,
}

/// `host fn name[A](x: A) -> R;` — a function the host must supply,
/// declared at module level. Host function value parameters use the
/// same grouped `name: Type` shape as ordinary function signatures.
/// **Inhabited at every phase**; carries a `Meta<P>` so it lives in
/// `module.items`. Always public.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HostFn<P: Phase = Surface> {
    pub name: String,
    pub params: Vec<HostFnParam<P>>,
    pub param_groups: Vec<SignatureGroupKind>,
    pub ret: Type<P>,
    pub meta: Meta<P>,
    /// Doc-comment attached to this host function declaration, if any.
    pub doc: Option<DocComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HostFnParam<P: Phase = Surface> {
    Type(TypeParam),
    Value(HostFnValueParam<P>),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HostFnValueParam<P: Phase = Surface> {
    /// Parsed host function parameters always carry a name. The field
    /// stays optional for compiler-synthesized ABI slots and serialized
    /// cache bridges that share this AST node.
    pub name: Option<String>,
    pub ty: Type<P>,
    pub meta: Meta<P>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostFnParamGroup<P: Phase = Surface> {
    Type(Vec<TypeParam>),
    Value(Vec<HostFnValueParam<P>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostFnGroupRef<'a, P: Phase = Surface> {
    Type(&'a [HostFnParam<P>]),
    Value(&'a [HostFnParam<P>]),
}

pub fn infer_host_fn_groups<P: Phase>(params: &[HostFnParam<P>]) -> Vec<SignatureGroupKind> {
    let mut groups = Vec::new();
    for param in params {
        match param {
            HostFnParam::Type(_) => groups.push(SignatureGroupKind::Type { len: 1 }),
            HostFnParam::Value(_) => match groups.last_mut() {
                Some(SignatureGroupKind::Value { len }) => *len += 1,
                _ => groups.push(SignatureGroupKind::Value { len: 1 }),
            },
        }
    }
    groups
}

pub fn host_fn_from_groups<P: Phase>(
    name: String,
    groups: Vec<HostFnParamGroup<P>>,
    ret: Type<P>,
    meta: Meta<P>,
    doc: Option<DocComment>,
) -> HostFn<P> {
    let mut params = Vec::new();
    let mut param_groups = Vec::new();
    for group in groups {
        match group {
            HostFnParamGroup::Type(items) => {
                param_groups.extend(type_binder_groups(items.len()));
                params.extend(items.into_iter().map(HostFnParam::Type));
            }
            HostFnParamGroup::Value(items) => {
                param_groups.push(SignatureGroupKind::Value { len: items.len() });
                params.extend(items.into_iter().map(HostFnParam::Value));
            }
        }
    }
    HostFn {
        name,
        params,
        param_groups,
        ret,
        meta,
        doc,
    }
}

pub fn host_fn_group_refs<'a, P: Phase>(
    params: &'a [HostFnParam<P>],
    groups: &[SignatureGroupKind],
) -> Vec<HostFnGroupRef<'a, P>> {
    let groups = if groups.is_empty() {
        infer_host_fn_groups(params)
    } else {
        groups.to_vec()
    };
    let mut out = Vec::new();
    let mut offset = 0usize;
    for group in groups {
        match group {
            SignatureGroupKind::Type { len } => {
                let end = offset + len;
                out.push(HostFnGroupRef::Type(&params[offset..end]));
                offset = end;
            }
            SignatureGroupKind::Value { len } => {
                let end = offset + len;
                out.push(HostFnGroupRef::Value(&params[offset..end]));
                offset = end;
            }
        }
    }
    out
}

// ---- Block field values ----------------------------------------------------

/// The value in a `build` block's directive-style field. A field value
/// is the unit literal `()` (the no-value default), a string literal, a
/// number literal, a boolean literal, or a bare word (a value-level
/// identifier). Each field then validates which of these kinds it
/// accepts; an omitted optional field is canonicalised to
/// [`BlockFieldValue::Unit`] by `kio fmt`. See
/// [`grammar.md` § Package files](../../specs/grammar.md#package-files)
/// and [`style.md`](../../specs/style.md) for the contract.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BlockFieldValue {
    /// `()` — the no-value default.
    Unit,
    /// A double-quoted string literal (post-escape value).
    Str(String),
    /// A number literal, carried verbatim (digit separators stripped by
    /// the lexer). Covers both integer and float spellings.
    Num(String),
    /// A boolean literal.
    Bool(bool),
    /// A bare word — a value-level identifier (`[a-z_][a-z0-9_]*`).
    Word(String),
}

// ---- Type expressions ---------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Type<P: Phase = Surface> {
    /// `Foo`, `Foo(A, B)`, `m.Foo`, `m.Foo(A, B)`. Length-1 lowercase
    /// segments are also valid here as type-parameter references; the
    /// parser doesn't know which, so disambiguation happens at name
    /// resolution.
    ///
    /// Comments captured immediately before each `args[i]`'s first
    /// token flow onto that arg's `meta.leading_trivia`; the
    /// pretty-printer breaks the args list to multi-line A1 layout
    /// when any arg carries a `LineComment`.
    Path {
        segments: Vec<PathSegment>,
        args: Vec<Type<P>>,
        meta: Meta<P>,
    },
    /// `.` — the unit type.
    Unit { meta: Meta<P> },
    /// `!` — the bottom type.
    Bottom { meta: Meta<P> },
    /// `(P) -> R`. Strict System F arrow: a single `param` and a
    /// single `ret`. Multi-value signature groups lower by
    /// right-folding their value types into `param`.
    ///
    /// `abi_arity` is the lowering representation arity for this
    /// function value. It is usually the arity of the value group that
    /// formed the layer, with generic domains kept opaque across
    /// substitution. It is not part of type identity; semantic call
    /// checking reads the product shape from `param`.
    ///
    /// `caps: P::FnTypeCapabilities` is the capability annotation
    /// the per-module [`crate::pass::capabilities::annotate_lifetime`]
    /// pass writes at the [`Routed`] phase (`()` at each pre-`Routed`
    /// phase, including the private `PrePrime` staging phase, and
    /// [`FnTypeCapabilities`] at `Routed`).
    /// The conservative default is `Heap`; the pass refines to
    /// `Stack` where the analysis admits it. Static-typed backends
    /// consume the field to pick between heap-managed and unboxed
    /// representations; type-erased backends ignore it.
    Function {
        param: Box<Type<P>>,
        ret: Box<Type<P>>,
        meta: Meta<P>,
        abi_arity: usize,
        caps: P::FnTypeCapabilities,
    },
    /// `(A & B)` — anonymous product. Always explicitly parenthesized
    /// in Kio'; right-associative `A & B & C` sugar is not in scope.
    Product {
        left: Box<Type<P>>,
        right: Box<Type<P>>,
        meta: Meta<P>,
    },
    /// `(A | B)` — anonymous sum. Always explicitly parenthesized in
    /// Kio'; right-associative `A | B | C` sugar is not in scope.
    Sum {
        left: Box<Type<P>>,
        right: Box<Type<P>>,
        meta: Meta<P>,
    },
    /// `{f: X}`, `{f}`, `{f: X, g: Y}` — label-derived type sugar.
    /// **Stripped during label-elab** by looking each label up in the
    /// per-module label table, producing `F(args)` (or a chain
    /// of `&` for multi-label forms). Bare `f` and `f(T, ...)` in
    /// type position are emitted by the parser as `Type::Path` and
    /// rewritten in the same pass.
    ///
    /// Per-label leading trivia flows onto each `labels[i]`'s
    /// `meta.leading_trivia`. Dropped at the `label_elab` boundary
    /// along with the variant itself.
    LabelSugar {
        labels: Vec<LabelSugarLabel<P>>,
        meta: Meta<P>,
        ext: P::TypeLabelSugar,
    },
    /// `_` — a placeholder asking the typer to infer this position.
    /// Resolved during type-checking via bidirectional flow plus
    /// unification with sibling positions. Statically uninhabited at
    /// [`Prime`] — the typer's substitution pass replaces every
    /// `Infer` with the solved concrete type before the AST reaches
    /// codegen.
    Infer { meta: Meta<P>, ext: P::TypeInfer },
    /// A transient, call-owned type-inference goal.
    ///
    /// `goal` identifies one slot in one explicit inference domain. `args`
    /// retains applications to an unresolved higher-kinded head, so `F(A)`
    /// remains one head goal plus its ordinary arguments rather than being
    /// encoded as a generated path. The form is inhabited only at
    /// [`Lowered`] while the typer is planning; it has no source spelling and
    /// every persistent, evaluator, and backend-facing phase forbids it.
    Goal {
        goal: TypeGoalRef,
        args: Vec<Type<P>>,
        meta: Meta<P>,
        ext: P::TypeGoal,
    },
    /// `[A](P0, …) -> R` — a polymorphic type. One universal binder
    /// scopes over `body`; source-level binder runs lower to nested
    /// `Forall` nodes. Surface syntax for the binder is the same
    /// `[Name]` notation as in declaration parameter lists. Per
    /// `language.md` § Type parameters.
    ///
    /// **`body` may be any type.** Mid-position binders in a paren-
    /// list desugar to nested `Forall` / `Function` shapes; the outer
    /// `body` can therefore be a `Type::Forall`, a `Type::Function`,
    /// or — at the limit — a non-arrow type (a binder over a value
    /// type). The Kio' parser keeps the stricter "no interleaving"
    /// rule for the surface; this AST variant accepts whatever the
    /// lowering constructs.
    Forall {
        param: TypeParam,
        body: Box<Type<P>>,
        meta: Meta<P>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabelSugarLabel<P: Phase = Surface> {
    pub label: String,
    pub label_span: Span,
    /// `None` for payload-elided forms (`{f}`, `{f, g: Y}`); the
    /// resolver fills in from the recorded signature.
    pub payload: Option<Type<P>>,
    pub meta: Meta<P>,
}

// ---- Value expressions --------------------------------------------------

/// Discriminator for the compiler-private field-syntax forms carried by
/// [`Expr::Elaborator`]: field access (`x.?{foo}`) and field update
/// (`x.!{foo = y}`). These are produced by the parser from postfix field
/// syntax; they are not user-callable elaborator names. A single
/// `Expr::Elaborator` variant dispatches on `kind` rather than carrying
/// one variant per form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ElaboratorKind {
    /// Compiler-private field access elaboration (`__access__` in
    /// design notes). Parser-produced postfix field syntax lowers to
    /// this; it is not a user-callable elaborator name.
    Access,
    /// Compiler-private field update/filter elaboration (`__filtered__`
    /// in design notes). Parser-produced postfix update syntax lowers to
    /// this; it is not a user-callable elaborator name.
    Filtered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RecOrderTypeFlow {
    /// Check the continuation body first and recover the pending value's exact
    /// type from the generated binder's classified use.
    ExpectedFromBody,
    /// Synthesize the pending value first and expose that exact type while
    /// checking the continuation body.
    SynthesizedValue,
    /// Check the pending value at its ordinary source type, but expose the
    /// separately validated elaborated runtime type while checking and
    /// lowering the continuation body.
    ElaboratedValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RecOrderDisposition {
    /// Apply one of the ordinary recursive-order typing directions.
    Ordered(RecOrderTypeFlow),
    /// Defer whether this candidate keeps its ordinary value or applies its
    /// compact continuation wrapper until a checked fills recipe proves the
    /// candidate's structural tail role.
    DeferredTail {
        requirement: RecOrderTailRequirement,
        lifted_flow: RecOrderTypeFlow,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RecOrderTailRequirement {
    Required,
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecOrderPlan<P: Phase = Surface> {
    pub name: String,
    pub disposition: RecOrderDisposition,
    /// The exact outer continuation used to lift a deferred callback. Its
    /// input is the callback's public result, while its output is runtime_ty.
    pub tail_continuation: Option<Box<Expr<P>>>,
    /// `SynthesizedValue` retains a source binding annotation so the pending
    /// value can synthesize before that annotation is checked. A
    /// compiler-generated `ExpectedFromBody` continuation carrier instead
    /// stores an exact `(_input) -> runtime_ty` shell here; its root hole is
    /// reserved and exposed while the body is checked. The body must solve the
    /// hole before the pending value is checked against that same shell.
    /// Fully concrete source annotations remain ordinary
    /// checking-mode continuation parameters, and every private `RecOrder` is
    /// eliminated at the Lowered-to-Prime boundary.
    pub annotation: Option<Type<P>>,
    pub value: Box<Expr<P>>,
    pub body: Box<Expr<P>>,
    pub runtime_ty: Type<P>,
}

/// A recursive computation whose continuation is chosen by the elaborated
/// recipe rather than by the written order of quoted arguments.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RecQuotePlan<P: Phase = Surface> {
    Operand {
        /// A recursive member supplies its declared result; a nested
        /// elaborator instead recovers this type from its closed local CPS
        /// function without making its unfinished source header-ready.
        public_ty: Option<Type<P>>,
        runtime_ty: Type<P>,
        computation: Box<Expr<P>>,
    },
    Expansion {
        runtime_ty: Type<P>,
        continuation: Box<Expr<P>>,
        value: Box<Expr<P>>,
    },
}

impl<P: Phase> RecQuotePlan<P> {
    pub fn expressions(&self) -> impl Iterator<Item = &Expr<P>> {
        match self {
            Self::Operand { computation, .. } => [Some(computation.as_ref()), None],
            Self::Expansion {
                continuation,
                value,
                ..
            } => [Some(continuation.as_ref()), Some(value.as_ref())],
        }
        .into_iter()
        .flatten()
    }

    pub fn annotations(&self) -> impl Iterator<Item = &Type<P>> {
        match self {
            Self::Operand {
                public_ty,
                runtime_ty,
                ..
            } => [public_ty.as_ref(), Some(runtime_ty)],
            Self::Expansion { runtime_ty, .. } => [Some(runtime_ty), None],
        }
        .into_iter()
        .flatten()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ElaboratorCall<P: Phase = Surface> {
    /// Compiler-private field access shape produced from `x.?{foo}`.
    FieldAccess {
        receiver: Box<Expr<P>>,
        labels: Vec<FieldAccessLabel<P>>,
    },
    /// Compiler-private field update shape produced from
    /// `x.!{foo = y}`.
    FieldUpdate {
        receiver: Box<Expr<P>>,
        updates: Vec<FieldUpdateLabel<P>>,
    },
}

impl ElaboratorKind {
    /// Compiler-private field-syntax names without the trailing `!`.
    pub fn name(self) -> &'static str {
        match self {
            ElaboratorKind::Access => "__access__",
            ElaboratorKind::Filtered => "__filtered__",
        }
    }

    /// Compiler-private field-syntax names with the trailing `!` — the
    /// spelling used in diagnostics.
    pub fn bang_name(self) -> &'static str {
        match self {
            ElaboratorKind::Access => "__access__!",
            ElaboratorKind::Filtered => "__filtered__!",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Expr<P: Phase = Surface> {
    /// A source-local block call. Descriptor selection and projection erase
    /// this variant before ordinary desugaring; parsing never reads providers.
    BlockCall {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        id: NodeId,
        head: PathSegment,
        prefix: Vec<Expr<P>>,
        prefix_explicit: bool,
        blocks: Vec<NeutralBlock<P>>,
        meta: Meta<P>,
        ext: P::ExprBlockSyntax,
    },
    /// `bar`, `Foo.mk_foo`, `m.bar`. Length-1 segments are bare
    /// identifiers; longer paths are dotted member access (type
    /// members) or qualified imports — disambiguated at name
    /// resolution.
    ///
    /// The `ext: P::ExprResolved` field gates this variant out at the
    /// [`Routed`] phase, where `recover_to_low::lower` has classified
    /// every path into one of the `Expr::Low*` variants. Earlier markers with
    /// the `()` witness admit it, including the exported [`Surface`],
    /// [`Desugared`], [`Lowered`], [`UncheckedPrime`], [`Prime`], and
    /// [`Enriched`] phases plus substitution's private `PrePrime` staging phase;
    /// the associated-type witness, rather than this catalogue, is authoritative.
    Path {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        segments: Vec<PathSegment>,
        meta: Meta<P>,
        ext: P::ExprResolved,
    },
    /// Function application. Type and value arguments share the
    /// positional list, distinguished syntactically at parse time:
    /// type-shaped `Path`s and type-only forms (`(A & B)`,
    /// `(A | B)`, `!`, `P -> R`) become type args; everything else
    /// becomes a value arg. `()` is always the Unit value; `.` is the
    /// Unit type.
    ///
    /// Per-arg leading trivia flows onto each wrapped element's
    /// `meta.leading_trivia` (via [`CallArg::meta_mut`]) — preserving
    /// any line comments authors place between commas in a multi-arg
    /// call. The pretty-printer breaks the call to multi-line layout
    /// when any arg carries a `LineComment`.
    ///
    /// The `ext: P::ExprResolved` field gates this variant out at the
    /// [`Routed`] phase, where every call has been routed into one of
    /// the `Expr::Low*` variants.
    Call {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        callee: Box<Expr<P>>,
        args: Vec<CallArg<P>>,
        meta: Meta<P>,
        ext: P::ExprResolved,
    },
    /// `rec f(args)` / `rec(poly, cont) f(args)` — mandatory marked
    /// recursive call inside a surface `rec(loop)` group. The marker
    /// is not a value name and has no runtime meaning; desugar checks
    /// the call-site modes and lowers the call to the group's loop
    /// state machinery.
    RecCall {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        modes: Vec<RecCallMode>,
        callee: PathSegment,
        args: Vec<CallArg<P>>,
        meta: Meta<P>,
        ext: P::ExprRecCall,
    },
    /// `.[A](x: T) -> R { body }`. Value-parameter annotations
    /// (`x: T`) and the return-type annotation (`-> R`) are
    /// optional. `Param.ty` is `Some(T)` when the user wrote one,
    /// `None` when the slot is bare. `ret_ty` is `Some(R)` when
    /// the user wrote `-> R`, `None` when the slot is bare. Either
    /// position may be `Type::Infer` (the `_` placeholder). A value
    /// parameter placeholder requires an expected function slot;
    /// a return placeholder leaves the body to synthesize its type.
    /// Concrete annotations remain authoritative.
    ///
    /// **Survives into the [`Routed`] phase.** Lambda literals are
    /// value-construction sites, not call sites; the design's
    /// `Low*` variant set classifies *calls* (and their value-
    /// position counterparts), not lambda-construction. A let-
    /// bound lambda literal flows through `Routed` unchanged so the
    /// surrounding `Expr::Let { value: Expr::FnExpr{...}, .. }`
    /// shape is preserved.
    ///
    /// The `caps: P::FnExprCapabilities` field carries the
    /// `captured_from` capability annotation the
    /// per-module [`crate::pass::capabilities::annotate_escapes`] pass
    /// writes after `recover_to_low::lower` runs. At every pre-
    /// [`Routed`] phase the field is `()`; at [`Routed`] it is a
    /// [`Capabilities`] bundle. The framework is shaped to admit
    /// additional annotations without restructuring. Its sibling
    /// phase-associated annotation is [`Lifetime`] on [`Type::Function`].
    FnExpr {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        sig: Signature<P>,
        ret_ty: Option<Type<P>>,
        body: Box<Expr<P>>,
        meta: Meta<P>,
        caps: P::FnExprCapabilities,
    },
    /// `let x = e;` or `let .(x: T) = e;` followed by the rest of the
    /// surrounding block. `ty` is the optional unary binding
    /// annotation. A fully concrete annotation makes the RHS a checked
    /// position; an absent or partial annotation leaves the RHS in
    /// synth-only mode and checks any concrete annotation leaves after
    /// synthesis. `pattern` is the surface-only tuple/as-pattern let
    /// extension; desugar rewrites it to direct `__fst__` / `__snd__`
    /// projections before typechecking, so every later phase carries
    /// the no-pattern witness.
    ///
    /// The body is the continuation of the enclosing block — everything
    /// that follows the `;` up to the closing `}`.
    ///
    /// `leading_trivia` carries any line comments / blank-line
    /// markers captured before the `let` keyword by the parser.
    /// Comments captured between the `=` and the value expression
    /// flow onto `value.meta.leading_trivia`; comments captured
    /// between the `;` and the body expression flow onto
    /// `body.meta.leading_trivia`. The pretty-printer reads them
    /// from those inner nodes.
    Let {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        name: String,
        name_span: Span,
        ty: Option<Type<P>>,
        pattern: P::ParamPatternExt,
        value: Box<Expr<P>>,
        body: Box<Expr<P>>,
        meta: Meta<P>,
    },
    /// `let .({f, g as payload}) = row; rest` — row destructuring
    /// statement syntax. Each entry names a row label and a local
    /// binder for that label's payload; omitted `as` binds the last
    /// path segment (`let .({m.f})` binds `f`). Desugar lowers this to a
    /// single temporary for `row` followed by ordinary `let`
    /// bindings whose RHS expressions are field-access elaborators.
    RowLet {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        entries: Vec<RowLetEntry<P>>,
        value: Box<Expr<P>>,
        body: Box<Expr<P>>,
        temp_name: String,
        meta: Meta<P>,
        ext: P::ExprRowLet,
    },
    /// `e;` followed by the rest of the surrounding block — an
    /// **expression statement** whose value is discarded. Per spec
    /// the typer requires `value : .`; discarding a non-unit value
    /// is a compile-time error (use `let _ = e;` to discard a
    /// non-unit value deliberately).
    ///
    /// Distinct from `Expr::Let { name: "_", … }`: the wildcard-let
    /// form admits any value type, while `Seq` enforces the unit
    /// constraint. Codegen is identical to a let with a discarded
    /// binding.
    ///
    /// Trivia flows onto `value.meta.leading_trivia` (before the
    /// value expression) and `body.meta.leading_trivia` (after the
    /// `;`), same as [`Expr::Let`].
    Seq {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        value: Box<Expr<P>>,
        body: Box<Expr<P>>,
        meta: Meta<P>,
    },
    /// `()` — the unit value.
    Unit {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        meta: Meta<P>,
    },
    /// String literal, post-escape. `annotation` is the trailing
    /// `(Type)` call form (`"hi"(String)`). Its container narrows
    /// across the Kio' boundary ([`Phase::LitAnnotation`]):
    /// `Option<Type<P>>` at the surface phases, where `None` is a bare
    /// literal whose type the typer resolves from context; a bare
    /// (mandatory) `Type<P>` at the Kio' phases, where every literal
    /// carries its resolved host type.
    StrLit {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        value: String,
        annotation: P::LitAnnotation,
        meta: Meta<P>,
    },
    /// Integer literal — no decimal point. `annotation` is the trailing
    /// `(Type)` call form (`100(I32)`). See [`Expr::StrLit`] on the
    /// `annotation` field's phase-narrowing container.
    IntLit {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        digits: String,
        annotation: P::LitAnnotation,
        meta: Meta<P>,
    },
    /// Float literal — has a decimal point and/or exponent. See
    /// [`Expr::StrLit`] on the `annotation` field.
    FloatLit {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        digits: String,
        annotation: P::LitAnnotation,
        meta: Meta<P>,
    },
    BoolLit {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        value: bool,
        annotation: P::LitAnnotation,
        meta: Meta<P>,
    },
    /// `(e1, e2)`, `(e1, e2, e3)`, ... — tuple literal sugar.
    /// **Stripped by `pass/desugar`** (pre-resolve): right-folds into
    /// nested `__pair__` calls. The desugaring auto-injects
    /// `import __intrinsics__;` into the module if it's not already
    /// present.
    ///
    /// Comments captured immediately before each `items[i]`'s first
    /// token flow onto that item's `meta.leading_trivia`, so authors
    /// can place line comments above any element of a multi-line
    /// tuple literal. The pretty-printer breaks to multi-line A1
    /// layout when any item carries a `LineComment`.
    Tuple {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        items: Vec<Expr<P>>,
        meta: Meta<P>,
        ext: P::ExprTuple,
    },
    /// `.x. { body }` — numbered references in the `xN` family, shadowed by
    /// authored value binders inside the body. A nested placeholder starts a
    /// fresh owner. Classification precedes operator expansion; hygienic name
    /// allocation follows body desugaring.
    ///
    /// **Stripped during desugar.** The variant is `Surface`-only;
    /// `Desugared`+ phases have it as [`Never`].
    FnPlaceholder {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        stem: PathSegment,
        body: Box<Expr<P>>,
        state: PlaceholderState,
        meta: Meta<P>,
        ext: P::ExprFnPlaceholder,
    },
    /// `{f = e}` and multi-label `{f = e1, g = e2}` — label-value
    /// construction sugar.
    /// **Stripped during label-elab**: a local `f = e` lowers to the
    /// newtype member call `F.mk(e)` (type-shaped newtype head);
    /// multi-label right-folds into nested `__pair__` calls.
    ///
    /// Per-label leading trivia flows onto each `labels[i]`'s
    /// `meta.leading_trivia`.
    LabelValue {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        labels: Vec<LabelValueLabel<P>>,
        meta: Meta<P>,
        ext: P::ExprLabelValue,
    },
    /// Compiler-private field-syntax carrier: postfix field access
    /// `x.?{foo}` and field update `x.!{foo = y}`, distinguished by `kind`
    /// ([`ElaboratorKind::Access`] / [`ElaboratorKind::Filtered`]) and
    /// `call` ([`ElaboratorCall::FieldAccess`] /
    /// [`ElaboratorCall::FieldUpdate`]). These are not user-callable
    /// elaborator names — the parser builds them from postfix field
    /// syntax. (User-written `name!(...)` elaborator calls are carried
    /// by [`Expr::UserElaborator`] instead.)
    ///
    /// The variant survives into `Lowered`; the typer records its elaboration
    /// in a side table keyed by `NodeId`, and substitution consumes that record
    /// to replace the carrier at the `Lowered → Prime` boundary.
    Elaborator {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        kind: ElaboratorKind,
        call: ElaboratorCall<P>,
        meta: Meta<P>,
        ext: P::ExprElab,
    },
    /// Boundary-local carrier for one recursive-CPS ordering/type-flow
    /// binding. Desugar uses it both to preserve an earlier value before
    /// later recursive work and to apply an untyped generated continuation
    /// with an explicit type-flow direction. Typechecking records the exact
    /// binding disposition, and substitution removes the carrier before
    /// Prime.
    RecOrder {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        plan: Box<RecOrderPlan<P>>,
        meta: Meta<P>,
        ext: P::ExprRecOrder,
    },
    /// Retained recursive quotation. Only Desugared and Lowered inhabit this
    /// carrier; checked recipe adoption consumes it before Prime.
    RecQuote {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        plan: Box<RecQuotePlan<P>>,
        meta: Meta<P>,
        ext: P::ExprRecOrder,
    },
    /// `name!(args...)` where `name` is imported from a user elaborator
    /// declaration. The call arguments are interpreted against the
    /// declaration's `call_ty`; `name` is import-scoped rather than part
    /// of the compiler's closed [`ElaboratorKind`] enum.
    UserElaborator {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        name: String,
        args: Vec<CallArg<P>>,
        form: UserElaboratorCallForm,
        meta: Meta<P>,
        ext: P::ExprElab,
    },
    /// `r.>f`, `r.>>f(args)`, `f(args).<r`, `f(args).<<r` — UFCS-style
    /// syntactic call splices. Also carries bang-call splice forms such as
    /// `r.>show!` / `show!.<r`: when `bang` is `Some`, the callee path
    /// is a single segment naming the user-defined elaborator and `args`
    /// carries the unspliced post-`!` argument list.
    ///
    /// **Pre-Prime only**: the typer resolves the callee's actual call
    /// type, inserts `receiver` into the value-argument stream according
    /// to [`UfcsFlavor`], records the equivalent prefix call or user
    /// elaborator expansion, and `substitute` erases the node before
    /// Prime. Kio' has no UFCS.
    ///
    /// `callee_segments` is a non-empty path: a single segment
    /// (`r.>f`) names a free value via the value namespace;
    /// multiple segments are a qualified path (`r.>m.f` for a
    /// module alias `m`, `r.>T.member` for a type member). Elaborator-
    /// UFCS (`bang = Some(_)`) always has exactly one segment, the
    /// elaborator name.
    ///
    /// `flavor` records which surface spelling was written and which
    /// side receives the inserted value. Elaborator bang-call splices
    /// use the same flavor rules as ordinary call splices; the
    /// elaborator's argument contract is checked after the splice builds
    /// the final argument list.
    ///
    /// `bang` is `Some(span)` for elaborator-UFCS (the span of the `!`
    /// suffix, used by diagnostics) and `None` for regular UFCS.
    /// The pretty-printer reads this to round-trip the `!` in the
    /// emitted source.
    ///
    /// Per-arg leading trivia flows onto each wrapped element's
    /// `meta.leading_trivia` per the [`Call`] variant's invariant.
    Ufcs {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        receiver: Box<Expr<P>>,
        callee_segments: Vec<PathSegment>,
        callee_span: Span,
        args: Vec<CallArg<P>>,
        flavor: UfcsFlavor,
        /// `Some(bang_span)` for the elaborator-UFCS form
        /// (`r.>iso!(T)` etc.); `None` for regular UFCS
        /// (`r.>f(args)`).
        bang: Option<Span>,
        meta: Meta<P>,
        ext: P::ExprUfcs,
    },
    /// Operator-usage placeholder. Emitted by the parser at every
    /// operator-application site (and every variadic-op bracket
    /// literal). Before desugar and ordinary name resolution, the operator-fold
    /// pass looks each placeholder up against the precomputed package operator
    /// scope (module-local `op`s plus cross-module operator-pattern imports)
    /// and substitutes a concrete `Expr::Call` in its place. Surface-only.
    ///
    /// `kind` distinguishes a fixed operator's matched pattern and slot
    /// operands from a varop region's written delimiters and comma-separated
    /// expressions. The region is self-describing: parsing and formatting
    /// its elements do not require the later-resolved fold declaration.
    OpChain {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        kind: OpChainKind<P>,
        meta: Meta<P>,
        ext: P::ExprOpChain,
    },
    /// Flat n-ary product, recovered from a right-leaning
    /// `__pair__` chain. `items.len() >= 2`. Semantically a value
    /// of the n-ary product type whose components are `items` in
    /// order; the binary `__pair__` spine is gone. Introduced by
    /// the structural-recovery pass; inhabited at [`Enriched`] and retained at
    /// [`Routed`] (`ExprEnriched = ()`), uninhabited at every front-end phase.
    EnrichedTuple {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        items: Vec<Expr<P>>,
        /// The product type the tuple inhabits, carried over from
        /// the recovered `__pair__(A, B, …)` call's type args.
        /// Typed backends (Rust) consult this to look up the shape
        /// struct; type-erased backends (JS) ignore it.
        synth_ty: P::ExprEnrichedSynthTy,
        meta: Meta<P>,
        ext: P::ExprEnriched,
    },
    /// Direct positional access into a product, recovered from a
    /// right-leaning `__fst__` / `__snd__` chain. `target` is the
    /// product value; `index` is the component picked (`0 <= index
    /// < arity`); `arity` is the product's component count
    /// (`>= 2`). The binary projection spine is gone. Introduced by
    /// the structural-recovery pass; inhabited at [`Enriched`] and [`Routed`].
    EnrichedProject {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        target: Box<Expr<P>>,
        index: usize,
        arity: usize,
        /// The product type the projection reads from. Recovered
        /// from the `__fst__(A, B, …)` / `__snd__(A, B, …)` chain's type
        /// args. Typed backends need it to spell out the shape
        /// struct's field name on the access; type-erased backends
        /// ignore it.
        target_ty: P::ExprEnrichedSynthTy,
        meta: Meta<P>,
        ext: P::ExprEnriched,
    },
    /// Positional variant injection into an n-ary sum, recovered
    /// from a right-leaning `__left__` / `__right__` chain.
    /// `payload` is the injected value; `variant` is the variant
    /// index (`0 <= variant < variants`); `variants` is the sum's
    /// total variant count (`>= 2`). The binary injection spine is
    /// gone. Introduced by the structural-recovery pass; inhabited at
    /// [`Enriched`] and [`Routed`].
    EnrichedInject {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        payload: Box<Expr<P>>,
        variant: usize,
        variants: usize,
        /// The sum type the injection produces, carried over from
        /// the recovered `__left__(A, B, …)` / `__right__(A, B, …)`
        /// chain's type args. Typed backends use it to pick the
        /// shape enum and its variant; type-erased backends ignore.
        synth_ty: P::ExprEnrichedSynthTy,
        meta: Meta<P>,
        ext: P::ExprEnriched,
    },
    /// N-arm dispatch on a sum, recovered from a right-leaning
    /// `__either__` chain. `scrutinee` is the sum value; each
    /// [`EnrichedArm`] carries the variant's bound payload name and
    /// handler body. `arms.len() >= 2` and matches the scrutinee's
    /// variant count. The per-`__either__` IIFE wrappers are gone.
    /// Structural recovery stages computed handler expressions in a
    /// surrounding `Let` spine before this node; an arm invokes only
    /// its already-constructed handler value.
    /// Introduced by the structural-recovery pass; inhabited at [`Enriched`]
    /// and [`Routed`].
    EnrichedMatch {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        scrutinee: Box<Expr<P>>,
        arms: Vec<EnrichedArm<P>>,
        /// The sum type the scrutinee inhabits — variant indices
        /// pair against this. Carried over from the recovered
        /// `__either__(a, b, r, …)` chain's type args.
        scrutinee_ty: P::ExprEnrichedSynthTy,
        /// The result type the match produces (`r` in the recovered
        /// `__either__(a, b, r, …)`).
        result_ty: P::ExprEnrichedSynthTy,
        meta: Meta<P>,
        ext: P::ExprEnriched,
    },
    /// First-class conditional recovered from
    /// `__if_then_else__(c, then_thunk, else_thunk)`. Literal thunk
    /// wrappers are stripped and their bodies sit directly in the
    /// branches. Computed thunk expressions are staged in a surrounding
    /// `Let` spine before this node, and the selected branch invokes its
    /// already-constructed thunk value.
    /// Introduced by the structural-recovery pass; inhabited at [`Enriched`]
    /// and [`Routed`].
    EnrichedConditional {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        cond: Box<Expr<P>>,
        then_branch: Box<Expr<P>>,
        else_branch: Box<Expr<P>>,
        /// The result type both branches produce (`r` in the
        /// recovered `__if_then_else__(R, …)` call).
        result_ty: P::ExprEnrichedSynthTy,
        meta: Meta<P>,
        ext: P::ExprEnriched,
    },
    /// Flat n-field record literal, recovered from a right-leaning
    /// `__pair__` chain whose **every** component is a newtype
    /// constructor call (`Foo.mk_foo(payload)` for some
    /// module-level `newtype Foo : T`). Each [`RecordField`]
    /// carries the newtype's FFI key (its projector's name; see
    /// [`Newtype::ffi_key`]) as `name` and the *unwrapped* payload
    /// (the constructor wrap is
    /// type-erased / runtime-identity, so the IR strips it). Field
    /// order is the right-spine order; backends that emit native
    /// records consume `fields` directly, while backends that keep a
    /// positional internal rep treat them as the n-ary product the
    /// surface tuple desugars into. Introduced by the structural-
    /// recovery pass; inhabited at [`Enriched`] and [`Routed`].
    EnrichedRecord {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        fields: Vec<RecordField<P>>,
        /// The product type the record inhabits (with each slot a
        /// label-derived newtype). Carried over from the recovered
        /// `__pair__(A, B, …)` chain's type args. Typed backends use
        /// it to look up the shape struct.
        synth_ty: P::ExprEnrichedSynthTy,
        meta: Meta<P>,
        ext: P::ExprEnriched,
    },
    /// Named-slot access into a record, recovered from a
    /// `Label.get(<positional access>)` call where `Label` is the
    /// label-derived newtype whose projector strips the wrap. Carries
    /// **both** the field name (`field_name`, the lowercase label
    /// spelling) and the positional layout (`index` / `arity` —
    /// where this field sits in the underlying product's right-
    /// spine) so a backend that keeps a positional internal rep can
    /// lower this as direct positional access while a backend with
    /// native records can lower it as `<target>.<field_name>`.
    /// Introduced by the structural-recovery pass; inhabited at [`Enriched`]
    /// and [`Routed`].
    EnrichedFieldGet {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        target: Box<Expr<P>>,
        field_name: String,
        index: usize,
        arity: usize,
        /// The product type the field-get reads from. Same role as
        /// `EnrichedProject::target_ty` — typed backends spell out
        /// the field name on the shape struct from it.
        target_ty: P::ExprEnrichedSynthTy,
        meta: Meta<P>,
        ext: P::ExprEnriched,
    },
    /// **Low-IR**: a call to a host function declaration. `name` is
    /// the resolved host-fn name
    /// (surface segment, no mangling); `type_args` is the
    /// substituted-type list at this call site (one per universal
    /// type param of the host-fn signature); `args` carries the
    /// value-args in order; `sig` is the host-fn's signature
    /// (carried so the per-backend lowering can read param/return
    /// types directly without re-looking-up the host-fn declaration).
    ///
    /// Covers both the bare single-segment shape (`io_print("x")`
    /// when `io_print` is a 1-seg host fn) and the 2-segment
    /// qualified-host shape (`io.print("x")` when `io` is a
    /// qualified-host alias). The lower pass merges the two
    /// classifications because the per-backend rendering is identical
    /// — both spell `self.__host.<name>(...)` in Rust and
    /// `__host__.<name>(...)` in JS.
    ///
    /// Introduced by [`crate::pass::recover_to_low::lower`]; inhabited only
    /// at [`Routed`].
    LowHostCall {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        name: String,
        /// The slash-path of the module that declares this `host fn`.
        /// Drives the namespaced host boundary (rung 1): the Rust backend
        /// reaches the call through the per-module host sub-trait, the
        /// JS backend through `__host__[module_path][name]`. Stamped from
        /// the resolved binder's defining module, not the syntactic call
        /// prefix.
        module_path: String,
        type_args: Vec<Type<P>>,
        args: Vec<Expr<P>>,
        sig: Signature<P>,
        /// The declared return of this terminal host invocation. A signature
        /// with groups after its first value group is instead lowered through
        /// [`Expr::LowHostFnValueRef`] plus an ordinary indirect call, so each
        /// residual type/value stage remains explicit before the compact host
        /// boundary is invoked.
        ret_ty: Type<P>,
        meta: Meta<P>,
        ext: P::ExprLow,
    },
    /// **Low-IR**: a call to a module fn declared in the current
    /// package (selectively imported or local). `mangled` is the
    /// cross-module-stable name (the surface name flat-mangled,
    /// possibly prefixed for cross-module disambiguation — see
    /// `backends/rust/emit.rs::build_selective_imports`); `type_args` is the
    /// type-arg list; `args` is the value-arg list; `sig` is the
    /// callee's signature.
    ///
    /// Introduced by [`crate::pass::recover_to_low::lower`]; inhabited only
    /// at [`Routed`].
    LowModuleCall {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        mangled: String,
        type_args: Vec<Type<P>>,
        args: Vec<Expr<P>>,
        sig: Signature<P>,
        /// The callee's declared return type, when the lower pass
        /// resolved the callee's `FnDef` (same-module / cross-module).
        /// `None` for the unresolved-fallback classification where no
        /// signature was found. `Signature` carries no `ret` (it lives
        /// per-embedder — see [`Signature`]), so the lower pass stamps
        /// it here, mirroring [`Expr::LowHostCall::ret_ty`]. Read by the
        /// Rust backend to decide whether a fn-value argument flowing
        /// into a generic value-slot must be lifted to `Rc<dyn Fn>`
        /// storage (the callee *stores* the binder in its return) or
        /// stay `impl Fn` (a *passthrough* whose return is the bare
        /// binder).
        ret_ty: Option<Type<P>>,
        meta: Meta<P>,
        ext: P::ExprLow,
    },
    /// **Low-IR**: a call to a module fn through a qualified import
    /// alias — `<alias>.<fn>` from `import <pkg>/<mod> as <alias>;`.
    /// `alias` is the surface alias name; `mangled` is the resolved
    /// mangled fn name as it would appear in
    /// [`LowExpr::LowModuleCall`]. The two variants stay distinct
    /// because the per-backend lowering may want to print the alias
    /// path verbatim in diagnostics or in source-aware emit modes.
    ///
    /// Introduced by [`crate::pass::recover_to_low::lower`]; inhabited only
    /// at [`Routed`].
    LowQualifiedModuleCall {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        alias: String,
        mangled: String,
        type_args: Vec<Type<P>>,
        args: Vec<Expr<P>>,
        sig: Signature<P>,
        /// The callee's declared return type when the lower pass
        /// resolved the qualified callee to a module `FnDef`. `None`
        /// for unrecognized-fallback classifications. See
        /// [`Expr::LowModuleCall::ret_ty`].
        ret_ty: Option<Type<P>>,
        meta: Meta<P>,
        ext: P::ExprLow,
    },
    /// **Low-IR**: a cross-module newtype-member call. `module_path`
    /// is the exact slash-separated identity of the module that declares
    /// `newtype`; any source alias has already been resolved by the lower
    /// pass. `member` is that newtype's constructor or projector,
    /// `type_args` is the type-arg list, and `payload` is the single
    /// value-arg.
    ///
    /// Distinct from [`LowExpr::LowNewtypeCtor`] /
    /// [`LowExpr::LowNewtypeProj`] (which cover the same forms when
    /// the newtype lives in the importer's own module) because the
    /// per-backend rendering for the qualified shape may need a
    /// qualifier prefix on the path. The Rust backend uses
    /// `crate::shapes::nominal::<module_path>::<newtype>`; the JS
    /// backend likewise uses `module_path` in its package namespace.
    ///
    /// Introduced by [`crate::pass::recover_to_low::lower`]; inhabited only
    /// at [`Routed`].
    LowQualifiedNewtypeMember {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        module_path: String,
        newtype: String,
        member: String,
        type_args: Vec<Type<P>>,
        payload: Box<Expr<P>>,
        meta: Meta<P>,
        ext: P::ExprLow,
    },
    /// **Low-IR**: a 2-segment newtype constructor call —
    /// `<NT>.<member>(<payload>)` where `<member>` is the newtype's
    /// constructor. `newtype` is the newtype name; `member` is the
    /// constructor's actual surface name (typically `mk_<name>` but
    /// the AST carries whatever the declaration specified); `type_args`
    /// covers **both** the newtype's universal and existential type-
    /// params at the constructor's call surface (the per-backend
    /// lowering reads `type_args[0..u]` for universals and
    /// `type_args[u..u+e]` for existentials); `payload` is the
    /// constructor argument.
    ///
    /// Introduced by [`crate::pass::recover_to_low::lower`]; inhabited only
    /// at [`Routed`].
    LowNewtypeCtor {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        newtype: String,
        member: String,
        type_args: Vec<Type<P>>,
        payload: Box<Expr<P>>,
        meta: Meta<P>,
        ext: P::ExprLow,
    },
    /// **Low-IR**: a 2-segment newtype projector call —
    /// `<NT>.<member>(<target>)` where `<member>` is the newtype's
    /// projector. `newtype` is the newtype name; `member` is the
    /// projector's actual surface name; `type_args` is the type-arg
    /// list (universals + existentials, same convention as
    /// [`LowExpr::LowNewtypeCtor`]); `target` is the projected
    /// receiver.
    ///
    /// Introduced by [`crate::pass::recover_to_low::lower`]; inhabited only
    /// at [`Routed`].
    LowNewtypeProj {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        newtype: String,
        member: String,
        type_args: Vec<Type<P>>,
        target: Box<Expr<P>>,
        meta: Meta<P>,
        ext: P::ExprLow,
    },
    /// **Low-IR**: a call whose callee is a 1-segment bound local
    /// (a fn-param, a let-bound name) — the value is a fn-typed
    /// closure being applied. `name` is the bound local's name;
    /// `args` is the value-arg list. `type_args` carries the call's
    /// per-call type-args when the bound local's elaborated type is
    /// rank-N (a `Type::Forall` over a function) — the call
    /// instantiates those binders, and the Rust backend needs the
    /// resolved per-call types to size the downcast at the rank-N
    /// invocation boundary (see `specs/backends/rust.md` § Higher-
    /// rank value parameters). Empty when the callee is monomorphic.
    ///
    /// Introduced by [`crate::pass::recover_to_low::lower`]; inhabited only
    /// at [`Routed`].
    LowClosureCall {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        name: String,
        type_args: Vec<Type<P>>,
        args: Vec<Expr<P>>,
        meta: Meta<P>,
        ext: P::ExprLow,
    },
    /// **Low-IR**: a call whose callee is **not** a path — e.g., an
    /// immediately-applied lambda `(.(x) { ... })(arg)`, or the
    /// result of an inner call `(f())(arg)`, or any expression of fn
    /// type. `callee` is the callee expression (already lowered);
    /// `args` is the value-arg list. `type_args` carries the call's
    /// per-call type-args (the typer-resolved instantiation for the
    /// callee's outer `Forall` binders, when the callee value is a
    /// polymorphic function — typically a polymorphic newtype
    /// projector). Empty when the callee is monomorphic.
    ///
    /// Distinct from [`LowExpr::LowClosureCall`] because the per-
    /// backend rendering may inline the callee differently (Rust
    /// borrows it as `(&callee)(args)`; JS calls it directly as
    /// `callee(args)`).
    ///
    /// Introduced by [`crate::pass::recover_to_low::lower`]; inhabited only
    /// at [`Routed`].
    LowIndirectCall {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        callee: Box<Expr<P>>,
        type_args: Vec<Type<P>>,
        args: Vec<Expr<P>>,
        meta: Meta<P>,
        ext: P::ExprLow,
    },
    /// **Low-IR**: one type-application boundary on a computed value.
    /// `type_arg` instantiates exactly one outer `Forall`; consecutive
    /// binders are represented by nested nodes rather than one aggregate.
    /// This keeps the System-F elimination order explicit after runtime type
    /// representations erase: an erased backend forces one hidden stage,
    /// while a backend with visible type application specializes the value.
    ///
    /// Introduced by [`crate::pass::recover_to_low::lower`]; inhabited only
    /// at [`Routed`].
    LowTypeApplication {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        callee: Box<Expr<P>>,
        type_arg: Type<P>,
        meta: Meta<P>,
        ext: P::ExprLow,
    },
    /// **Low-IR**: the `__absurd__` intrinsic — a value of type `!`
    /// projected into any other type. `type_arg` is the target type
    /// `T` (the type the absurd call inhabits); `value_arg` is the
    /// `!`-typed expression being projected.
    ///
    /// Carries its own variant so the lowering pass is total over
    /// the `Expr::Call` input — every `Expr::Call` shape becomes one
    /// of the `Low*` variants, with `__absurd__` being the one
    /// pseudo-intrinsic that's neither a host nor module fn nor a
    /// closure.
    ///
    /// Introduced by [`crate::pass::recover_to_low::lower`]; inhabited only
    /// at [`Routed`].
    LowAbsurdCall {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        type_arg: Type<P>,
        value_arg: Box<Expr<P>>,
        meta: Meta<P>,
        ext: P::ExprLow,
    },
    /// **Low-IR**: the **CPS-projector-apply** call shape — an inner
    /// call to a newtype projector with existentials, with an outer
    /// call passing the continuation. The fold from `LowIndirectCall`
    /// to this variant happens in
    /// [`crate::pass::recover_to_low::lower`] so the per-backend
    /// lowering doesn't re-detect the shape. Static-typed families
    /// consume `type_args` for the projector's generic header
    /// (Rust's turbofish-aware emit; an OCaml or Haskell port would
    /// use it for phantom-marker–scoped polymorphic projection);
    /// dynamic / type-erased families ignore `type_args`.
    ///
    /// `newtype` is the projected newtype and `module_path` is its
    /// identity-exact declaring module. Keeping the owner on the routed
    /// node prevents an unrelated same-leaf declaration in the caller's
    /// module from redirecting runtime type lookup. `receiver` is the value
    /// being projected; `type_args` carries the inner projector's
    /// per-call type-args (the typer-resolved instantiation for the
    /// newtype's universal-param header — empty when the newtype is
    /// monomorphic). `continuation_ty` is the exact routed type expected
    /// by the already-resolved CPS application: one leading `forall` per
    /// existential followed by a function from the instantiated payload
    /// to the selected result type. Backends use that self-contained type
    /// for erased type stages and callable ABI shape; they must not reopen
    /// the newtype declaration to derive either fact or inspect the
    /// continuation's source shape.
    /// `continuation` is the corresponding value invoked with the unwrapped
    /// payload.
    ///
    /// Introduced by [`crate::pass::recover_to_low::lower`]; inhabited only
    /// at [`Routed`].
    LowCpsProjectorApply {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        newtype: String,
        module_path: String,
        type_args: Vec<Type<P>>,
        receiver: Box<Expr<P>>,
        continuation: Box<Expr<P>>,
        continuation_ty: Type<P>,
        meta: Meta<P>,
        ext: P::ExprLow,
    },
    /// **Low-IR**: a 1-segment path naming a bound local in **value
    /// position** (i.e., not a call callee). `name` is the bound
    /// local's name. Per-backend lowering renders this as a plain
    /// identifier reference (Rust: `<name>.clone()` if needed; JS:
    /// `<name>`).
    ///
    /// Introduced by [`crate::pass::recover_to_low::lower`]; inhabited only
    /// at [`Routed`].
    LowBoundRef {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        name: String,
        meta: Meta<P>,
        ext: P::ExprLow,
    },
    /// **Low-IR**: a 1-segment path naming a **host fn** in value
    /// position (the fn-value form, not a call). `name` is the
    /// host-fn name; `sig` is its signature; `ret_ty` is the host
    /// fn's declared return type.
    ///
    /// Per-backend lowering wraps this into a fn-value of the host-
    /// trait method (Rust: an `Rc<dyn Fn(...) -> ret_ty>` capturing
    /// `self.__host`; JS: a JS function that delegates to
    /// `__host__.<name>`). `ret_ty` is carried on the variant (rather
    /// than looked up at render time) so the Rust backend's value-
    /// position closure-wrap can spell the dyn-Fn's return type
    /// without consulting a per-host-fn table.
    ///
    /// Introduced by [`crate::pass::recover_to_low::lower`]; inhabited only
    /// at [`Routed`].
    LowHostFnValueRef {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        name: String,
        /// The slash-path of the module declaring this `host fn` — drives
        /// the namespaced host boundary (see [`Expr::LowHostCall`]).
        module_path: String,
        sig: Signature<P>,
        ret_ty: Type<P>,
        meta: Meta<P>,
        ext: P::ExprLow,
    },
    /// **Low-IR**: a 1-segment path naming a **module fn** in value
    /// position. `mangled` is the cross-module-stable mangled name;
    /// `sig` is the fn's signature.
    ///
    /// Per-backend lowering wraps this into a fn-value of the module
    /// .(Rust: an `Rc<dyn Fn(...) -> ...>` capturing `self`; JS: a
    /// JS function that delegates to the module-namespace method).
    ///
    /// Introduced by [`crate::pass::recover_to_low::lower`]; inhabited only
    /// at [`Routed`].
    LowModuleFnValueRef {
        #[serde(skip)]
        occurrence: P::ExpressionOccurrence,
        mangled: String,
        sig: Signature<P>,
        meta: Meta<P>,
        ext: P::ExprLow,
    },
}

/// One arm of an [`Expr::EnrichedMatch`]. `param` is the name the
/// arm binds its variant's payload to; `body` is the handler
/// expression evaluated with `param` in scope. Recovered from one
/// `__either__`'s handler `.(param) -> ... { body }`; when a
/// handler is not a literal `fn`, the recovery binds a fresh
/// `param` and `body` becomes the handler applied to it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnrichedArm<P: Phase = Surface> {
    pub param: String,
    pub body: Expr<P>,
    pub meta: Meta<P>,
}

/// One field of an [`Expr::EnrichedRecord`]. `name` is the
/// newtype's FFI key — the originating newtype's projector name
/// (see [`Newtype::ffi_key`]); `value` is the unwrapped payload
/// — the runtime-identity newtype constructor wrap is gone.
/// Field order in the parent record matches the right-spine
/// order of the underlying product type, so positional consumers
/// see the same layout an `EnrichedTuple` would have.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecordField<P: Phase = Surface> {
    pub name: String,
    pub value: Expr<P>,
    pub meta: Meta<P>,
}

/// Discriminates a normal-op usage from a variadic-op
/// bracket-literal usage. Carries the kind-specific data the
/// operator-fold pass needs to substitute the placeholder with
/// a concrete `Expr::Call` shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OpChainKind<P: Phase = Surface> {
    /// Normal-op application (`a + b`, `a ? b : c`,
    /// `a ? b : c`, etc.). `pattern` is the full matched op pattern
    /// in source order; `slots` carries one parsed operand
    /// per slot part in `pattern`, in source order. Keeping the
    /// pattern with the chain makes the placeholder self-describing
    /// — the operator-fold pass uses it as the lookup key (via the
    /// leading-token run helper) and the pretty-printer uses it to
    /// reconstruct the source-level interleaving of tokens and
    /// slot operands without consulting any out-of-band operator
    /// scope.
    Normal {
        pattern: Vec<OpPart>,
        slots: Vec<Expr<P>>,
    },
    /// Variadic bracket literal (`[[a, b, c]]`). Each element is one ordinary
    /// expression. The written delimiters identify the operator independently
    /// of the declaration scope and preserve its source spelling for formatting.
    Variadic {
        open_tokens: Vec<String>,
        close_tokens: Vec<String>,
        elements: Vec<Expr<P>>,
        empty_trivia: Vec<crate::pass::lexer::Trivia>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Uninterpreted source items with ordinary lexical binder scope. Exposure
/// validation belongs to projection, not this representation or the formatter.
pub struct NeutralBlock<P: Phase = Surface> {
    pub label: Option<PathSegment>,
    pub elided: bool,
    pub items: Vec<NeutralItem<P>>,
    pub open: Span,
    pub close: Span,
    pub separators: Vec<NeutralSeparator>,
    pub leading: Vec<crate::pass::lexer::Trivia>,
    pub trailing: Vec<crate::pass::lexer::Trivia>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NeutralItem<P: Phase = Surface> {
    Expression {
        value: Expr<P>,
        semicolon: Option<Span>,
    },
    Binding {
        name: String,
        name_span: Span,
        ty: Option<Type<P>>,
        pattern: P::ParamPatternExt,
        value: Expr<P>,
        bind: bool,
        meta: Meta<P>,
    },
    RowBinding {
        entries: Vec<RowLetEntry<P>>,
        value: Expr<P>,
        meta: Meta<P>,
    },
    ExistentialBinding {
        type_params: Vec<TypeParam>,
        name: String,
        name_span: Span,
        pattern: P::ParamPatternExt,
        value: Expr<P>,
        meta: Meta<P>,
    },
}

impl<P: Phase> NeutralItem<P> {
    pub fn value(&self) -> &Expr<P> {
        match self {
            Self::Expression { value, .. }
            | Self::Binding { value, .. }
            | Self::RowBinding { value, .. }
            | Self::ExistentialBinding { value, .. } => value,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            Self::Expression { value, .. } => value.span(),
            Self::Binding { meta, .. }
            | Self::RowBinding { meta, .. }
            | Self::ExistentialBinding { meta, .. } => meta.span,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NeutralSeparator {
    pub span: Span,
    pub leading: Vec<crate::pass::lexer::Trivia>,
    pub after_item: Option<usize>,
}

impl<P: Phase> OpChainKind<P> {
    /// True iff this is a `Normal` chain whose pattern starts
    /// with a `Token` part (i.e. a prefix-shaped operator like
    /// `! _` or `++ _`). Variadic chains are not prefix-shaped.
    pub fn is_prefix(&self) -> bool {
        match self {
            OpChainKind::Normal { pattern, .. } => {
                matches!(pattern.first(), Some(OpPart::Token { .. }))
            }
            OpChainKind::Variadic { .. } => false,
        }
    }

    /// The op-token run that the parser matched as the chain's
    /// lookup key — the leading tokens of `pattern` for a
    /// `Normal` chain (skipping a leading slot if non-prefix);
    /// the `open_tokens` for a `Variadic` chain. Used by the
    /// operator-fold pass and by diagnostics; matches the
    /// `leading_op_run` semantics in the parser and op-fold.
    pub fn leading_op_run(&self) -> Vec<String> {
        match self {
            OpChainKind::Normal { pattern, .. } => {
                let start = if matches!(
                    pattern.first(),
                    Some(OpPart::SlotPlain { .. })
                        | Some(OpPart::SlotRecursive { .. })
                        | Some(OpPart::SlotGreedy { .. })
                ) {
                    1
                } else {
                    0
                };
                pattern[start..]
                    .iter()
                    .take_while(|p| matches!(p, OpPart::Token { .. }))
                    .filter_map(|p| match p {
                        OpPart::Token { content, .. } => Some(content.clone()),
                        _ => None,
                    })
                    .collect()
            }
            OpChainKind::Variadic { open_tokens, .. } => open_tokens.clone(),
        }
    }
}

/// One entry in a label-value construction expression `{f = e, g = e'}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabelValueLabel<P: Phase = Surface> {
    pub label: String,
    pub label_span: Span,
    pub value: Expr<P>,
    pub meta: Meta<P>,
}

/// One binder in row-let statement syntax `let .({label as local}) = row;`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RowLetEntry<P: Phase = Surface> {
    pub label: String,
    pub label_span: Span,
    pub local: String,
    pub local_span: Span,
    pub alias_explicit: bool,
    /// Parser-issued key for the field-access elaborator desugar
    /// creates for this entry.
    pub access_ext: P::ExprElab,
    pub meta: Meta<P>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FieldAccessLabel<P: Phase = Surface> {
    pub label: String,
    pub label_span: Span,
    /// The generated label newtype's **identity-exact path**, populated by
    /// label elaboration. A locally-declared label is a bare `[Foo]` (the
    /// single same-module newtype `mint_label_newtype_name` produced); a
    /// cross-module label imported by `import m({foo});` or the qualified
    /// `{m.foo}` sugar is the **fully-qualified** `[m…, Foo]` so the typer
    /// selects the slot, looks the newtype up, and emits the `mk`/`get`
    /// member call by `(module, name)` identity rather than by bare leaf —
    /// two modules whose labels mint the same leaf `Foo` stay distinct.
    /// `None` only before label elaboration.
    pub label_type: Option<Vec<PathSegment>>,
    pub meta: Meta<P>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FieldUpdateLabel<P: Phase = Surface> {
    pub label: String,
    pub label_span: Span,
    /// The generated label newtype's identity-exact path — bare `[Foo]`
    /// for a local label, fully-qualified `[m…, Foo]` for a cross-module
    /// one. See [`FieldAccessLabel::label_type`]. `None` only before label
    /// elaboration.
    pub label_type: Option<Vec<PathSegment>>,
    pub value: Expr<P>,
    pub meta: Meta<P>,
}

/// A classified owner cannot reinterpret operator-generated paths as authored
/// numbered references when folding is repeated. Both states disappear during
/// Surface → Desugared conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlaceholderState {
    /// Parsed arity for source tooling. Lowering reclassifies the written body.
    Source {
        slot_count: usize,
    },
    Classified {
        slot_count: usize,
    },
}

/// One positional argument at a call site — either a type argument or a
/// value argument. Distinguished by the parser using syntactic shape
/// (and, for bare `Path` arguments, the exact type-name spelling
/// convention enforced by the language).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CallArg<P: Phase = Surface> {
    Type(Type<P>),
    Value(Expr<P>),
}

impl<P: Phase> CallArg<P> {
    pub fn meta(&self) -> &Meta<P> {
        match self {
            CallArg::Type(t) => t.meta(),
            CallArg::Value(v) => v.meta(),
        }
    }

    pub fn meta_mut(&mut self) -> &mut Meta<P> {
        match self {
            CallArg::Type(t) => t.meta_mut(),
            CallArg::Value(v) => v.meta_mut(),
        }
    }
}

// ---- Build block (inside `<name>.pkg.kio`) -----------------------

/// The parsed `build { ... }` block inside a `<name>.pkg.kio`
/// package file: the mandatory `cache` declaration plus a list of
/// `target <id> { ... }` blocks. Per
/// [`specs/package.md`](../../specs/package.md): inside the build
/// block, `build`, `target`, `cache`, and `docs` are contextual
/// keywords; otherwise the block uses ordinary Kio grammar tokens.
/// There are no types or values — the block isn't a language surface.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BuildBlock {
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub trailing_trivia: Vec<crate::pass::lexer::Trivia>,
    /// The optional `cache <string-or-unit>;` declaration. Either a
    /// filesystem-path string (caching enabled at that location) or the
    /// unit literal `()` (caching explicitly disabled). When the build
    /// block omits the field it canonicalises to `cache ();` (caching
    /// disabled), which `kio fmt` inserts; the parser fills the absent
    /// field with [`BuildBlockCache::Disabled`].
    pub cache: BuildBlockCache,
    /// The optional `docs { … };` block — points `kio doc` at the
    /// package's documentation tree. `None` when the build block omits
    /// it; `kio doc check` / `kio doc build` then fail with a clear
    /// "no `docs` field" diagnostic.
    pub docs: Option<BuildBlockDocs>,
    pub targets: Vec<TargetBlock>,
    pub span: Span,
}

/// The resolved `docs { md …; support …; md_out …; html …; };` block.
///
/// `md` is the markdown source tree (required when `docs` is
/// present). `support` names additional root module support-file
/// directories for snippet validation. `md_out` / `html` name the
/// rendered-output directories; each is optional with a default
/// (`out/docs-md/`, `out/docs/`). All filesystem paths are relative to
/// the package root.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BuildBlockDocs {
    pub trailing_trivia: Vec<crate::pass::lexer::Trivia>,
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
    /// `md "<path>";` — the markdown source tree. Required.
    pub md: String,
    pub md_leading_trivia: Vec<crate::pass::lexer::Trivia>,
    /// `support "<path>";` — root module support-file directory.
    pub support: Vec<String>,
    pub support_leading_trivia: Vec<Vec<crate::pass::lexer::Trivia>>,
    /// `md_out "<path>";` — rendered-Markdown output directory.
    /// `None` when the key is omitted; the default is `out/docs-md/`.
    pub md_out: Option<String>,
    pub md_out_leading_trivia: Vec<crate::pass::lexer::Trivia>,
    /// `html "<path>";` — rendered-HTML output directory. `None`
    /// when the key is omitted; the default is `out/docs/`.
    pub html: Option<String>,
    pub html_leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub span: Span,
}

/// The `cache` field's resolved value. Either a string path or the
/// unit literal opting caching out.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BuildBlockCache {
    /// `cache "<path>";` — caching enabled at this filesystem path.
    /// Path semantics: relative paths are relative to the package
    /// root; absolute paths are accepted as-is.
    Path {
        path: String,
        span: Span,
        leading_trivia: Vec<crate::pass::lexer::Trivia>,
    },
    /// `cache ();` — caching explicitly disabled.
    Disabled {
        span: Span,
        leading_trivia: Vec<crate::pass::lexer::Trivia>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TargetBlock {
    pub trailing_trivia: Vec<crate::pass::lexer::Trivia>,
    /// The target id — a bare identifier after `target` naming the
    /// backend (`rust`, `js`, `kio-prime`, …). What `kio build <id>`
    /// selects.
    pub id: String,
    /// The ordered list of `key "value";` entries inside the block.
    /// Keep insertion order so repeat-key diagnostics name the second
    /// occurrence at its actual source position.
    pub entries: Vec<TargetEntry>,
    pub span: Span,
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TargetEntry {
    pub key: String,
    pub value: String,
    pub span: Span,
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
}

// ---- Signature file (`<name>.sig.kio`) ---------------------------------

/// A parsed `<name>.sig.kio` package-signature changelog.
///
/// The file is its own file shape, sibling to `module` / `package`:
/// a `signature <pkg> v(<N>);` header followed by a
/// body of oldest-first per-version blocks. It records *what changed at
/// each contract generation*, in backend-agnostic Kio′ declarations; the
/// current interface is recovered by **replaying** the version blocks in
/// version order (replay is a later phase — Phase 1 only parses the
/// changelog into this AST).
///
/// The declaration grammar inside each module section is verbatim Kio′
/// at the section (declaration) level — `host type` / `host fn`, `type`,
/// `newtype`, and a sig-only body-less `pub fn f(p) -> R;` export
/// production — reusing the regular item parser. The changelog framing
/// (header, `v(N) { breaking { … } nonbreaking { … } }`, braced
/// `module <path> { … }` sections, `add` / `modify` / `remove`) is the
/// sig format itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SignatureFile<P: Phase = Surface> {
    /// The package name from the `signature <pkg> v(<N>);` header. Must
    /// match the filename stem (`<pkg>.sig.kio`), checked the same way
    /// `package <name>;` matches its `.pkg.kio` stem.
    pub pkg: String,
    /// The current contract generation `N` from the header `v(<N>)`.
    /// Author/CI/changelog bookkeeping only — neither the loader nor the
    /// compat check ever consults it; both are purely structural.
    pub version: u32,
    /// Per-version blocks in **source order**, which is oldest-first
    /// (`v(1)` directly under the header, newest at the bottom). Replay
    /// reorders by `SigVersion::version`, so the source order is
    /// presentational.
    pub versions: Vec<SigVersion<P>>,
    pub meta: Meta<P>,
}

/// One `v(<N>) { breaking { … } nonbreaking { … } }` block. A version
/// partitions its changes into a `breaking` section and a `nonbreaking`
/// section, **breaking first**, each **omitted when empty**. The
/// *presence* of the `breaking` section **is** the version's
/// breaking-ness — there is no separate marker.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SigVersion<P: Phase = Surface> {
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub trailing_trivia: Vec<crate::pass::lexer::Trivia>,
    pub with_leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub with_trailing_trivia: Vec<crate::pass::lexer::Trivia>,
    /// An optional leading `///` doc-comment on the `v(<N>) { … }` block —
    /// the version's changelog message (what `kio sig commit -m "…"`
    /// records). A multi-line message is multiple `///` lines. `None` when
    /// the version carries no message.
    pub doc: Option<DocComment>,
    /// The version number `N` from `v(<N>) { … }`.
    pub version: u32,
    /// Complete version-local declarations needed to validate and replay
    /// operation targets that belong to one explicit recursive type group.
    /// The corresponding `add` / `modify` blocks name individual members by
    /// exact [`SigItemRef`] rather than duplicating the declarations.
    pub with: Vec<SigModuleSection<P>>,
    /// The `breaking { … }` section, present iff the version records a
    /// break. Its presence is the breaking marker.
    pub breaking: Option<SigChangeSet<P>>,
    /// The `nonbreaking { … }` section, present iff the version records a
    /// compatible change.
    pub nonbreaking: Option<SigChangeSet<P>>,
    pub span: Span,
}

/// The contents of one `breaking { … }` or `nonbreaking { … }` section:
/// the `add` / `modify` / `remove` blocks, each optional and each
/// holding a list of module sections (or, for `remove`, name lists).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SigChangeSet<P: Phase = Surface> {
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub trailing_trivia: Vec<crate::pass::lexer::Trivia>,
    pub add_leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub add_trailing_trivia: Vec<crate::pass::lexer::Trivia>,
    pub modify_leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub modify_trailing_trivia: Vec<crate::pass::lexer::Trivia>,
    pub remove_leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub remove_trailing_trivia: Vec<crate::pass::lexer::Trivia>,
    /// `add { module <path> { … } … }` — module sections introducing new
    /// declarations. Empty when the `add` block is absent.
    pub add: Vec<SigModuleSection<P>>,
    /// Exact declaration references whose complete declarations live in the
    /// enclosing version's `with` block.
    #[serde(default)]
    pub add_refs: Vec<SigItemRef>,
    /// `modify { module <path> { … } … }` — module sections whose
    /// declarations reshape an existing item. Empty when absent.
    pub modify: Vec<SigModuleSection<P>>,
    #[serde(default)]
    pub modify_refs: Vec<SigItemRef>,
    /// `remove { module <path> { name; … } … }` — name-only lists of
    /// removed items. Empty when absent.
    pub remove: Vec<SigRemoveModule>,
    /// Canonical exact-name spelling for removals. The nested legacy module
    /// representation remains accepted for existing changelogs.
    #[serde(default)]
    pub remove_refs: Vec<SigItemRef>,
    pub span: Span,
}

/// An exact declaration name in a signature operation block. This is the
/// non-expression `ItemRef` category: slash-separated module path followed by
/// a dot and one declaration name (`foo/bar.Item`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SigItemRef {
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub path: ModulePath,
    pub name: String,
    pub span: Span,
}

/// One `module <path> { <import clauses> <signature-only items> }` section
/// inside an `add` / `modify` block. The braces make each module's
/// boundary explicit in the nested structure; the section carries its
/// own `import` clauses so a reference resolves at *its* version even when a
/// live type later changes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SigModuleSection<P: Phase = Surface> {
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub trailing_trivia: Vec<crate::pass::lexer::Trivia>,
    /// The module path (`a/b/c`) this section's declarations belong to.
    pub path: ModulePath,
    /// The section's imports (the ordinary Kio′ import grammar).
    pub imports: Vec<Import>,
    /// The signature-only declarations, in source order.
    pub items: Vec<SigItem<P>>,
    pub span: Span,
}

/// A body-less exported function signature in a [`SignatureFile`].
///
/// The signature reuses [`HostFn`]'s body-less parameter/return shape,
/// while the separate purity field records the compile-time-use
/// capability that only ordinary functions can provide.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SigExportFn<P: Phase = Surface> {
    pub purity: P::FnPurity,
    pub function: HostFn<P>,
}

/// One signature-only declaration inside a [`SigModuleSection`]. Wraps
/// the shared Kio′ declaration variants the sig grammar admits plus the
/// sig-only body-less export-fn production, so a module section's
/// declarations stay in one source-ordered list without adding a variant
/// to the shared [`Item`] enum (which only the sig file consumes).
///
/// The admitted shapes (per the settled sig grammar): `host type` /
/// `host fn` (already signature-only, the env / contravariant side),
/// `type`, `newtype`, and a body-less `pub fn f(p0: T) -> R;` export
/// signature (the export / covariant side). No surface-only forms
/// (`elab` / `op` / `labels` / `equiv` / `literal`) — they are
/// not Kio′.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SigItem<P: Phase = Surface> {
    /// `host type Foo;` — host requirement (env side, contravariant).
    HostType(HostType<P>),
    /// `host fn foo(a: Bar) -> Baz;` — host requirement (env side).
    HostFn(HostFn<P>),
    /// `type Name = …;` — a transparent type synonym in the closure.
    TypeAlias(TypeAlias<P>),
    /// `newtype Name = …;` — a nominal type in the closure.
    Newtype(Newtype<P>),
    /// A complete Kio' recursive declaration group. Signature operations
    /// target its members individually through [`SigItemRef`].
    TypeRecGroup(TypeRecGroup<P>),
    /// `pub fn f(p0: T) -> R;` — a body-less export-fn signature
    /// (export side, covariant). A *new sig production* reusing the
    /// host-fn signature shape; carried in a [`HostFn`] node because that
    /// is exactly the body-less fn-signature shape, but it is an export
    /// provision, not a host requirement. The parser rejects a stray
    /// `{ … }` body with a sig-context diagnostic.
    ExportFn(SigExportFn<P>),
}

/// One `module <path> { <name>; … }` entry inside a `remove` block —
/// names only. A bare name is side-ambiguous (a value-shaped name could be
/// a host fn or an export fn; a type-shaped name a `host type` or an
/// exported `newtype` / `type`); the verifier (a later phase) replays
/// history to the item's `add` / `modify` origin to recover its side and
/// frozen signature. Module removal is expressed as removing all of a
/// module's items (an empty module after replay); there is no dedicated
/// module-removal op.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SigRemoveModule {
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub trailing_trivia: Vec<crate::pass::lexer::Trivia>,
    /// The module path the removed names belong to.
    pub path: ModulePath,
    /// The removed item names, in source order.
    pub names: Vec<SigRemoveName>,
    pub span: Span,
}

/// One removed item name inside a [`SigRemoveModule`]. Side (host vs
/// export) and signature are recovered by replay, not stored here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SigRemoveName {
    pub leading_trivia: Vec<crate::pass::lexer::Trivia>,
    pub name: String,
    pub span: Span,
}

/// Right-fold a list of types into a nested `Product`. The
/// 0-element case yields `Unit`.
/// 1 element passes through; 2+ elements build `Product(t0,
/// Product(t1, … Product(tn-1, tn)))`. Used by
/// [`Type::synth_function`] to convert a surface multi-arg list to
/// the canonical System F single-product param.
pub(crate) fn build_product_right_fold<P: Phase>(mut params: Vec<Type<P>>, span: Span) -> Type<P> {
    if params.is_empty() {
        return Type::Unit {
            meta: Meta::new(span),
        };
    }
    if params.len() == 1 {
        return params.pop().expect("len > 0");
    }
    let last = params.pop().expect("len > 1");
    let mut acc = last;
    while let Some(prev) = params.pop() {
        let acc_span = match &acc {
            Type::Path { meta, .. }
            | Type::Unit { meta }
            | Type::Bottom { meta }
            | Type::Function { meta, .. }
            | Type::Product { meta, .. }
            | Type::Sum { meta, .. }
            | Type::LabelSugar { meta, .. }
            | Type::Forall { meta, .. }
            | Type::Infer { meta, .. }
            | Type::Goal { meta, .. } => meta.span,
        };
        let prev_span = match &prev {
            Type::Path { meta, .. }
            | Type::Unit { meta }
            | Type::Bottom { meta }
            | Type::Function { meta, .. }
            | Type::Product { meta, .. }
            | Type::Sum { meta, .. }
            | Type::LabelSugar { meta, .. }
            | Type::Forall { meta, .. }
            | Type::Infer { meta, .. }
            | Type::Goal { meta, .. } => meta.span,
        };
        let combined_span = Span::new(prev_span.start, acc_span.end);
        acc = Type::Product {
            left: Box::new(prev),
            right: Box::new(acc),
            meta: Meta::new(combined_span),
        };
    }
    acc
}

impl<P: Phase> Type<P> {
    /// Build a `Type::Path` for a synthesized site — i.e., one not
    /// derived from user source. `args_trivia` is set to
    /// [`Default::default()`], which is the empty unit `()` at every
    /// post-parse phase (where the typer / codegen run) and an empty
    /// `Vec<Vec<Trivia>>` at [`Surface`] (the only phase where the
    /// parser would have captured real trivia anyway).
    pub fn synth_path(segments: Vec<String>, args: Vec<Type<P>>, span: Span) -> Type<P> {
        Self::synth_path_segments(
            segments
                .into_iter()
                .map(|n| PathSegment::synth(n, span))
                .collect(),
            args,
            span,
        )
    }

    /// Build a `Type::Path` from an explicit segment list. The
    /// segment-based form is used by walkers (e.g. [`convert_type`])
    /// that already have the source path's per-segment spans in hand
    /// and want to preserve them across phase boundaries; the
    /// string-based [`synth_path`] is the right entry point for
    /// synthesis sites that have no source spans for the segments.
    pub fn synth_path_segments(
        segments: Vec<PathSegment>,
        args: Vec<Type<P>>,
        span: Span,
    ) -> Type<P> {
        Type::Path {
            segments,
            args,
            meta: Meta::new(span),
        }
    }

    /// Count the fully structural slots of a function domain type.
    /// This is useful for callers that intentionally want the product
    /// spine of an already-formed domain. New function layers should
    /// usually use the arity of the value group that formed them.
    pub fn function_param_abi_slot_count(param: &Type<P>) -> usize {
        match param {
            Type::Unit { .. } => 0,
            Type::Product { .. } => Self::flatten_param_list(param).len(),
            _ => 1,
        }
    }

    /// Build a `Type::Function` from a list of value-domain types and
    /// a return type. The domain is canonicalized as a right-folded
    /// product: zero items produce unit, one item is kept verbatim,
    /// and two or more items produce `Product(A, Product(B, …))`.
    ///
    /// The function layer's host ABI arity follows the value group that
    /// formed it. An empty group and a single unit domain both erase to zero
    /// host parameters; otherwise each supplied domain entry is one ABI slot.
    /// This field never selects source-call arguments. Product types remain
    /// product values; they do not create extra function parameters merely by
    /// appearing as a slot's type.
    pub fn synth_function(params: Vec<Type<P>>, ret: Type<P>, span: Span) -> Type<P> {
        let abi_arity = match params.as_slice() {
            [] => 0,
            [Type::Unit { .. }] => 0,
            _ => params.len(),
        };
        let param = build_product_right_fold(params, span);
        Type::Function {
            param: Box::new(param),
            ret: Box::new(ret),
            meta: Meta::new(span),
            abi_arity,
            caps: P::FnTypeCapabilities::default(),
        }
    }

    /// Walk the full right-spine of a `Type::Product`, returning each
    /// spine slot in source order. A `Product` node whose `right` is
    /// itself a `Product` extends the spine with `left` and continues
    /// into `right`; a non-`Product` terminal ends the spine as the
    /// last slot. A left-nested chain `(A & B) & C` therefore reports
    /// two slots `[A & B, C]` — the positional non-collapsing
    /// semantics every backend's FFI surface shares.
    ///
    /// This is the `&` half of the cross-cutting **right-spine walk**
    /// (`specs/backends/README.md` § The right-spine walk). The FFI
    /// skin keys its per-backend shapes on it; [`Type::right_spine_sum`]
    /// is the `|` half. A non-product terminal (atomic, `Unit`, path)
    /// reports a single-element list `[self]`.
    pub fn right_spine_product(t: &Type<P>) -> Vec<&Type<P>> {
        let mut out = Vec::new();
        let mut cur = t;
        loop {
            match cur {
                Type::Product { left, right, .. } => {
                    out.push(left.as_ref());
                    cur = right.as_ref();
                }
                other => {
                    out.push(other);
                    return out;
                }
            }
        }
    }

    /// Walk the full right-spine of a `Type::Sum`, returning each spine
    /// slot in source order. The `|` mirror of
    /// [`Type::right_spine_product`]: a `Sum` whose `right` is itself a
    /// `Sum` extends the spine, a non-`Sum` terminal ends it. The other
    /// half of the cross-cutting **right-spine walk**
    /// (`specs/backends/README.md` § The right-spine walk).
    pub fn right_spine_sum(t: &Type<P>) -> Vec<&Type<P>> {
        let mut out = Vec::new();
        let mut cur = t;
        loop {
            match cur {
                Type::Sum { left, right, .. } => {
                    out.push(left.as_ref());
                    cur = right.as_ref();
                }
                other => {
                    out.push(other);
                    return out;
                }
            }
        }
    }

    /// Return one payload type from a recovered match's actual arity.
    /// Recovery may stop before the end of a sum's right spine; the final arm
    /// then consumes the whole remaining sub-sum rather than one flattened
    /// leaf.
    pub fn right_spine_sum_slot_for_arity(
        t: &Type<P>,
        index: usize,
        arity: usize,
    ) -> Option<&Type<P>> {
        if arity == 0 || index >= arity {
            return None;
        }
        let mut cur = t;
        for _ in 0..index {
            let Type::Sum { right, .. } = cur else {
                return None;
            };
            cur = right;
        }
        if index + 1 == arity {
            Some(cur)
        } else {
            match cur {
                Type::Sum { left, .. } => Some(left),
                _ => None,
            }
        }
    }

    /// Walk the right-spine of a `Type::Function::param`, returning
    /// the value-param types in source order.
    ///
    /// **Inverse of `synth_function`'s right-fold.** Callers that
    /// want "the list of value params" use this; the AST itself
    /// carries the canonical nested form. A thin alias for
    /// [`Type::right_spine_product`] applied to a function's domain:
    /// `Unit` is treated as a terminal, so a `Function` whose `param`
    /// is `Unit` reports a single-element list `[Unit]`. Callers that
    /// need the original signature/value-group slots use
    /// [`Type::right_spine_take`].
    pub fn flatten_param_list(param: &Type<P>) -> Vec<&Type<P>> {
        Type::right_spine_product(param)
    }

    /// Walk a function-type `param` for exactly `abi_arity` levels of
    /// the right-spine. This is the lowering ABI view of a function
    /// domain, not the semantic product width used by call checking.
    pub fn right_spine_take(param: &Type<P>, abi_arity: usize) -> Vec<&Type<P>> {
        if abi_arity == 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(abi_arity);
        let mut cur = param;
        for i in 0..abi_arity {
            if i + 1 == abi_arity {
                out.push(cur);
                return out;
            }
            match cur {
                Type::Product { left, right, .. } => {
                    out.push(left.as_ref());
                    cur = right.as_ref();
                }
                other => {
                    // The spine ran out before `abi_arity` was
                    // reached — the declaration's value-arity and
                    // the type's right-spine disagree. Pad with the
                    // terminal so the caller sees something sensible
                    // rather than silently losing positions.
                    out.push(other);
                    return out;
                }
            }
        }
        out
    }

    /// Peel the ordered leading universal-binder run from a type. The
    /// returned count is the number of erased type-application stages a
    /// backend must preserve before consuming the returned body.
    pub fn peel_leading_foralls(&self) -> (usize, &Type<P>) {
        let mut count = 0usize;
        let mut current = self;
        while let Type::Forall { body, .. } = current {
            count += 1;
            current = body;
        }
        (count, current)
    }

    /// Right-fold a slice of types into a single arg-product type:
    /// the type that the function call's positional args build at
    /// the call site. 0 elements yield `Unit` (the call-site dual
    /// of `. -> R`); 1 element passes through; 2+ elements
    /// right-fold into `Product(t0, Product(t1, …))` — matching
    /// the right-fold rule for declaration-side param lists.
    ///
    /// Used by [`crate::pass::typecheck_core::synth_call`] to compute
    /// the arg-product type and unify it with the function's
    /// `param`.
    pub fn right_fold_args(types: Vec<Type<P>>, span: Span) -> Type<P> {
        build_product_right_fold(types, span)
    }

    /// Build the System-F type of a signature against `R`.
    /// Walks source groups right-to-left:
    ///
    /// - A value-parameter group wraps the accumulator as
    ///   `Function(product(run), acc)`.
    /// - A type-binder group wraps as `Forall(run, acc)`.
    ///
    /// Position is semantically meaningful: every value group
    /// introduces a distinct callable layer. Examples:
    ///
    /// - `[A](x: P0, y: P1) -> R` → `Forall([A], Function(Product(P0, P1), R))`
    /// - value group `P0`, then binder `[A]`, then value group `P1`
    ///   → `Function(P0, Forall([A], Function(P1, R)))`
    /// - two value groups `P0`, then `P1`
    ///   → `Function(P0, Function(P1, R))`
    ///
    /// Empty params yields `Function(Unit, ret)` with zero ABI slots.
    ///
    /// Used by `synth_top_fn_def` / `synth_env_fn_def` / the
    /// `newtype` member scheme synthesisers to produce a single
    /// `Type<P>` representation of any signature.
    pub fn synth_scheme_from_signature_params(
        params: &[SignatureParam<P>],
        ret: Type<P>,
        span: Span,
    ) -> Type<P>
    where
        Type<P>: Clone,
    {
        record_signature_group_cursor_params_route();
        Type::synth_scheme_from_signature_group_refs(
            SignatureGroupRefs::inferred(params),
            params
                .iter()
                .any(|param| matches!(param, SignatureParam::Value(_))),
            ret,
            span,
        )
    }

    pub fn synth_scheme_from_signature(sig: &Signature<P>, ret: Type<P>, span: Span) -> Type<P>
    where
        Type<P>: Clone,
    {
        record_signature_group_cursor_signature_route();
        Type::synth_scheme_from_signature_group_refs(
            sig.canonical_group_refs(),
            sig.has_value_group(),
            ret,
            span,
        )
    }

    fn synth_scheme_from_signature_group_refs<'a>(
        groups: impl DoubleEndedIterator<Item = SignatureGroupRef<'a, P>>,
        has_value_group: bool,
        ret: Type<P>,
        span: Span,
    ) -> Type<P>
    where
        P: 'a,
        Type<P>: Clone,
    {
        let mut acc = ret;
        if !has_value_group {
            acc = Type::synth_function(Vec::new(), acc, span);
        }
        for group in groups.rev() {
            match group {
                SignatureGroupRef::Type(params) => {
                    for param in params.iter().rev() {
                        let SignatureParam::Type(param) = param else {
                            unreachable!("SignatureGroupRef::Type contains only type params")
                        };
                        acc = Type::Forall {
                            param: param.clone(),
                            body: Box::new(acc),
                            meta: Meta::new(span),
                        };
                    }
                }
                SignatureGroupRef::Value(params) => {
                    let value_tys: Vec<Type<P>> = params
                        .iter()
                        .map(|param| {
                            let SignatureParam::Value(vp) = param else {
                                unreachable!("SignatureGroupRef::Value contains only value params")
                            };
                            vp.ty
                                .as_ref()
                                .expect(
                                    "synth_scheme_from_signature: value param missing type \
                                     annotation (fn / equiv / host fn value params are \
                                     mandatorily annotated)",
                                )
                                .clone()
                        })
                        .collect();
                    acc = Type::synth_function(value_tys, acc, span);
                }
            }
        }
        acc
    }

    pub fn span(&self) -> Span {
        self.meta().span
    }

    pub fn meta(&self) -> &Meta<P> {
        match self {
            Type::Path { meta, .. }
            | Type::Unit { meta }
            | Type::Bottom { meta }
            | Type::Function { meta, .. }
            | Type::Product { meta, .. }
            | Type::Sum { meta, .. }
            | Type::LabelSugar { meta, .. }
            | Type::Forall { meta, .. }
            | Type::Infer { meta, .. }
            | Type::Goal { meta, .. } => meta,
        }
    }

    pub fn meta_mut(&mut self) -> &mut Meta<P> {
        match self {
            Type::Path { meta, .. }
            | Type::Unit { meta }
            | Type::Bottom { meta }
            | Type::Function { meta, .. }
            | Type::Product { meta, .. }
            | Type::Sum { meta, .. }
            | Type::LabelSugar { meta, .. }
            | Type::Forall { meta, .. }
            | Type::Infer { meta, .. }
            | Type::Goal { meta, .. } => meta,
        }
    }
}

/// Mechanically rebuild a [`Type`] across a phase boundary.
/// Identical structure, just with the type parameter changed.
///
/// The `LabelSugar` arm requires `P::TypeLabelSugar = Never`. Markers that
/// satisfy that bound include [`Lowered`], [`UncheckedPrime`], [`Prime`],
/// [`Enriched`], [`Routed`], and substitution's private `PrePrime` staging
/// phase; the generic bound, rather than this catalogue, determines admission.
/// The variant is therefore uninhabited and the rebuild covers every reachable
/// shape via the `match *ext {}` discharge. `Q` is unconstrained: the
/// rebuild never produces a `LabelSugar` so the target's
/// `TypeLabelSugar` shape doesn't matter.
///
/// Used by `pass/substitute` (Lowered → Prime) and
/// `backends/kio_prime.rs` (Prime → Surface) to share their `Type`
/// walks. Adding a new pair of phase walkers wires up the same
/// helper without copy-pasting the recursion.
pub fn convert_type<P, Q>(ty: &Type<P>) -> Type<Q>
where
    P: Phase<TypeLabelSugar = Never>,
    Q: Phase,
{
    // Trivia fields use `P::ParallelTrivia` / `P::LeadingTrivia`,
    // which differ between phases (`Vec<…>` at Surface, `()` at every
    // later phase). The phase walkers that go through `convert_type`
    // are all post-Surface (e.g., Lowered → Prime in `pass/substitute`,
    // Prime → Surface in `backends/kio_prime.rs`), so the
    // source either has no trivia to preserve (`()`) or the target
    // has nowhere to put it. Either way `Default::default()` is the
    // right fill.
    match ty {
        Type::Path {
            segments,
            args,
            meta,
            ..
        } => Type::synth_path_segments(
            segments.clone(),
            args.iter().map(convert_type::<P, Q>).collect(),
            meta.span,
        ),
        Type::Unit { meta } => Type::Unit {
            meta: convert_meta(meta),
        },
        Type::Bottom { meta } => Type::Bottom {
            meta: convert_meta(meta),
        },
        Type::Function {
            param,
            ret,
            meta,
            abi_arity,
            ..
        } => {
            let param = convert_type::<P, Q>(param);
            let ret = convert_type::<P, Q>(ret);
            Type::Function {
                param: Box::new(param),
                ret: Box::new(ret),
                meta: convert_meta(meta),
                abi_arity: *abi_arity,
                // Capability annotations are phase-local and are not copied
                // by a mechanical cross-phase conversion. The target
                // witness's `Default` supplies its initial value; a pass that
                // owns a nontrivial target annotation populates it separately.
                caps: Q::FnTypeCapabilities::default(),
            }
        }
        Type::Product { left, right, meta } => Type::Product {
            left: Box::new(convert_type::<P, Q>(left)),
            right: Box::new(convert_type::<P, Q>(right)),
            meta: convert_meta(meta),
        },
        Type::Sum { left, right, meta } => Type::Sum {
            left: Box::new(convert_type::<P, Q>(left)),
            right: Box::new(convert_type::<P, Q>(right)),
            meta: convert_meta(meta),
        },
        Type::Forall { param, body, meta } => Type::Forall {
            param: param.clone(),
            body: Box::new(convert_type::<P, Q>(body)),
            meta: convert_meta(meta),
        },
        Type::LabelSugar { ext, .. } => match *ext {},
        // `Type::Infer` should be resolved by the typer's
        // substitution pass before reaching `convert_type`. At Prime
        // (`P::TypeInfer = Never`) it's statically unreachable;
        // at Lowered (`P::TypeInfer = ()`), `pass/substitute` is
        // responsible for replacing every `Infer` with its solved
        // concrete type before invoking `convert_type`. A reachable
        // arm here means an upstream contract was broken.
        Type::Infer { .. } => {
            unreachable!("Type::Infer should have been resolved by the typer's substitution pass")
        }
        Type::Goal { .. } => {
            unreachable!(
                "Type::Goal reached a phase conversion; the owning inference domain must close \
                 and zonk every goal before crossing the Lowered boundary"
            )
        }
    }
}

/// Phase-rebrand an [`Expr`] across a phase boundary where every
/// surface-only variant is statically uninhabited at the source —
/// every elaboration-bearing variant (`Elaborator` /
/// `UserElaborator` via `ExprElab`), the pre-typer surface forms (`Tuple` /
/// `FnPlaceholder` / `LabelValue` / `Ufcs`), and any nested
/// `Type::LabelSugar` discharge via `match *ext {}`.
///
/// [`crate::backends::kio_prime`] uses this walker for Prime → Surface
/// pretty-printing, and the substitute pass uses it for its strict-core
/// PrePrime → UncheckedPrime exit. Source phases with inhabited elaboration
/// variants need a rewrite-aware walker (see [`crate::pass::substitute`],
/// [`crate::pass::desugar`], [`crate::pass::label_elab`]).
pub fn convert_expr<P, Q>(e: &Expr<P>) -> Expr<Q>
where
    P: Phase<
            ExprTuple = Never,
            ExprFnPlaceholder = Never,
            ExprOpChain = Never,
            ExprLabelValue = Never,
            ExprRowLet = Never,
            ExprElab = Never,
            ExprBlockSyntax = Never,
            ExprRecCall = Never,
            ExprRecOrder = Never,
            ExprUfcs = Never,
            ExprEnriched = Never,
            ExprLow = Never,
            TypeLabelSugar = Never,
        >,
    Q: Phase<ExprResolved = ()>,
{
    match e {
        Expr::Path { segments, meta, .. } => Expr::Path {
            occurrence: Default::default(),
            segments: segments.clone(),
            meta: convert_meta(meta),
            ext: (),
        },
        Expr::Call {
            callee, args, meta, ..
        } => Expr::synth_call(
            convert_expr::<P, Q>(callee),
            args.iter()
                .map(|a| match a {
                    CallArg::Type(t) => CallArg::Type(convert_type::<P, Q>(t)),
                    CallArg::Value(v) => CallArg::Value(convert_expr::<P, Q>(v)),
                })
                .collect(),
            meta.span,
        ),
        Expr::RecCall { ext, .. } => match *ext {},
        Expr::FnExpr {
            sig,
            ret_ty,
            body,
            meta,
            ..
        } => Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(
                sig.params
                    .iter()
                    .map(convert_signature_param::<P, Q>)
                    .collect(),
                sig.groups.clone(),
            ),
            ret_ty: ret_ty.as_ref().map(convert_type::<P, Q>),
            body: Box::new(convert_expr::<P, Q>(body)),
            meta: convert_meta(meta),
            // Capability annotations are phase-local and are not copied by a
            // mechanical cross-phase conversion. The target witness's
            // `Default` supplies its initial value; a pass that owns a
            // nontrivial target annotation populates it separately.
            caps: Q::FnExprCapabilities::default(),
        },
        Expr::Let {
            name,
            name_span,
            ty,
            value,
            body,
            ..
        } => Expr::Let {
            occurrence: Default::default(),
            name: name.clone(),
            name_span: *name_span,
            ty: ty.as_ref().map(convert_type::<P, Q>),
            pattern: <Q::ParamPatternExt as Default>::default(),
            value: Box::new(convert_expr::<P, Q>(value)),
            body: Box::new(convert_expr::<P, Q>(body)),
            meta: Meta::new(value.span()),
        },
        Expr::RowLet { ext, .. } => match *ext {},
        Expr::Seq { value, body, .. } => Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(convert_expr::<P, Q>(value)),
            body: Box::new(convert_expr::<P, Q>(body)),
            meta: Meta::new(value.span()),
        },
        Expr::Unit {
            occurrence: _,
            meta,
        } => Expr::Unit {
            occurrence: Default::default(),
            meta: convert_meta(meta),
        },
        Expr::StrLit {
            occurrence: _,
            value,
            annotation,
            meta,
        } => Expr::StrLit {
            occurrence: Default::default(),
            value: value.clone(),
            annotation: convert_lit_annotation::<P, Q>(annotation, meta.span),
            meta: convert_meta(meta),
        },
        Expr::IntLit {
            occurrence: _,
            digits,
            annotation,
            meta,
        } => Expr::IntLit {
            occurrence: Default::default(),
            digits: digits.clone(),
            annotation: convert_lit_annotation::<P, Q>(annotation, meta.span),
            meta: convert_meta(meta),
        },
        Expr::FloatLit {
            occurrence: _,
            digits,
            annotation,
            meta,
        } => Expr::FloatLit {
            occurrence: Default::default(),
            digits: digits.clone(),
            annotation: convert_lit_annotation::<P, Q>(annotation, meta.span),
            meta: convert_meta(meta),
        },
        Expr::BoolLit {
            occurrence: _,
            value,
            annotation,
            meta,
        } => Expr::BoolLit {
            occurrence: Default::default(),
            value: *value,
            annotation: convert_lit_annotation::<P, Q>(annotation, meta.span),
            meta: convert_meta(meta),
        },
        // Surface-only variants are statically uninhabited at the
        // source phase per the trait bounds; each arm discharges via
        // `match *ext {}` over `Never`.
        Expr::Tuple { ext, .. } => match *ext {},
        Expr::FnPlaceholder { ext, .. } => match *ext {},
        Expr::LabelValue { ext, .. } => match *ext {},
        Expr::Elaborator { ext, .. } => match *ext {},
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::UserElaborator { ext, .. } => match *ext {},
        Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Ufcs { ext, .. } => match *ext {},
        Expr::OpChain { ext, .. } => match *ext {},
        // The enriched structural variants are uninhabited at every
        // phase `convert_expr` accepts as a source (the bound pins
        // `ExprEnriched = Never`); consumers of the post-`Prime` phases walk
        // `Module<Enriched>` or `Module<Routed>` directly.
        Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
        | Expr::EnrichedRecord { ext, .. }
        | Expr::EnrichedFieldGet { ext, .. } => match *ext {},
        // The Low-IR variants are statically uninhabited at every
        // phase `convert_expr` accepts as a source (the bound pins
        // `ExprLow = Never`); a Routed-consuming pass walks
        // `Module<Routed>` directly.
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

/// Phase-rebrand a [`SignatureParam`] across a phase boundary —
/// type-params pass through unchanged; value-params recurse via
/// [`convert_type`].
pub fn convert_signature_param<P, Q>(p: &SignatureParam<P>) -> SignatureParam<Q>
where
    P: Phase<TypeLabelSugar = Never>,
    Q: Phase,
{
    match p {
        SignatureParam::Type(tp) => SignatureParam::Type(tp.clone()),
        SignatureParam::Value(v) => SignatureParam::Value(Param {
            name: v.name.clone(),
            ty: v.ty.as_ref().map(convert_type::<P, Q>),
            pattern: <Q::ParamPatternExt as Default>::default(),
            meta: convert_meta(&v.meta),
        }),
    }
}

/// Phase-rebrand a [`Signature`] across a phase boundary by
/// walking each [`SignatureParam`] via [`convert_signature_param`].
pub fn convert_signature<P, Q>(sig: &Signature<P>) -> Signature<Q>
where
    P: Phase<TypeLabelSugar = Never>,
    Q: Phase,
{
    Signature::from_parts(
        sig.params
            .iter()
            .map(convert_signature_param::<P, Q>)
            .collect(),
        sig.groups.clone(),
    )
}

/// Phase-rebrand a [`FnDef`] across a phase boundary where every
/// surface-only `Expr` variant is statically uninhabited at the
/// source. Bound set matches [`convert_expr`]. Used by
/// [`crate::backends::kio_prime`]'s Prime → Surface walk and by
/// compile-time evaluator setup for Kio'-shaped internal artifacts.
pub fn convert_fn_def<P, Q>(d: &FnDef<P>) -> FnDef<Q>
where
    P: Phase<
            ExprTuple = Never,
            ExprFnPlaceholder = Never,
            ExprOpChain = Never,
            ExprLabelValue = Never,
            ExprRowLet = Never,
            ExprElab = Never,
            ExprBlockSyntax = Never,
            ExprRecCall = Never,
            ExprRecOrder = Never,
            ExprUfcs = Never,
            ExprEnriched = Never,
            ExprLow = Never,
            TypeLabelSugar = Never,
        >,
    Q: Phase<ExprResolved = ()>,
    Q::FnDefRetElided: PhaseBridge<P::FnDefRetElided>,
    Q::FnPurity: PhaseBridge<P::FnPurity>,
{
    FnDef {
        vis: d.vis.clone(),
        purity: <Q::FnPurity as PhaseBridge<P::FnPurity>>::bridge(d.purity.clone()),
        name: d.name.clone(),
        sig: convert_signature::<P, Q>(&d.sig),
        ret: convert_type::<P, Q>(&d.ret),
        ret_elided: <Q::FnDefRetElided as PhaseBridge<P::FnDefRetElided>>::bridge(
            d.ret_elided.clone(),
        ),
        body: convert_expr::<P, Q>(&d.body),
        meta: convert_meta(&d.meta),
        doc: d.doc.clone(),
    }
}

/// Phase-rebrand an [`TypeAlias`] across a phase boundary.
pub fn convert_type_alias<P, Q>(a: &TypeAlias<P>) -> TypeAlias<Q>
where
    P: Phase<TypeLabelSugar = Never>,
    Q: Phase,
{
    TypeAlias {
        vis: a.vis.clone(),
        name: a.name.clone(),
        name_span: a.name_span,
        type_params: a.type_params.clone(),
        body: convert_type::<P, Q>(&a.body),
        meta: convert_meta(&a.meta),
        editable_span: None,
        doc: a.doc.clone(),
    }
}

/// Phase-rebrand a [`Newtype`] across a phase boundary. Payload
/// recurses via [`convert_type`]; constructor / projector members
/// keep their semantic fields and drop source trivia.
pub fn convert_newtype<P, Q>(d: &Newtype<P>) -> Newtype<Q>
where
    P: Phase<TypeLabelSugar = Never>,
    Q: Phase,
{
    Newtype {
        vis: d.vis.clone(),
        rec_span: d.rec_span,
        name: d.name.clone(),
        name_span: d.name_span,
        type_params: d.type_params.clone(),
        existential_params: d.existential_params.clone(),
        payload: convert_type::<P, Q>(&d.payload),
        constructor: convert_type_member(&d.constructor),
        projector: convert_type_member(&d.projector),
        meta: convert_meta(&d.meta),
        editable_span: None,
        doc: d.doc.clone(),
    }
}

/// Phase-rebrand a recursive type group after surface `labels` members have
/// been eliminated.
pub fn convert_type_rec_group<P, Q>(group: &TypeRecGroup<P>) -> TypeRecGroup<Q>
where
    P: Phase<TypeLabelSugar = Never, ItemLabels = Never>,
    Q: Phase,
{
    TypeRecGroup {
        members: group
            .members
            .iter()
            .map(|member| match member {
                TypeRecMember::TypeAlias(alias) => {
                    TypeRecMember::TypeAlias(convert_type_alias::<P, Q>(alias))
                }
                TypeRecMember::Newtype(newtype) => {
                    TypeRecMember::Newtype(convert_newtype::<P, Q>(newtype))
                }
                TypeRecMember::Labels(_, ext) => match *ext {},
            })
            .collect(),
        doc: group.doc.clone(),
        source_layout: group.source_layout.clone(),
        rec_span: group.rec_span,
        open_brace_span: group.open_brace_span,
        close_brace_span: group.close_brace_span,
        deferred_rec_labels_diagnostic: group.deferred_rec_labels_diagnostic.clone(),
        meta: convert_meta(&group.meta),
    }
}

/// Phase-rebrand a [`HostType`] across a phase boundary. Signature-only
/// (no body); the role / owned flags and type params carry verbatim.
pub fn convert_host_type<P, Q>(h: &HostType<P>) -> HostType<Q>
where
    P: Phase,
    Q: Phase,
{
    HostType {
        name: h.name.clone(),
        type_params: h.type_params.clone(),
        role: h.role,
        owned: h.owned,
        meta: convert_meta(&h.meta),
        doc: h.doc.clone(),
    }
}

/// Phase-rebrand a [`HostFn`] across a phase boundary. Signature-only
/// (no body); each value param's type recurses via [`convert_type`].
pub fn convert_host_fn<P, Q>(h: &HostFn<P>) -> HostFn<Q>
where
    P: Phase<TypeLabelSugar = Never>,
    Q: Phase,
{
    HostFn {
        name: h.name.clone(),
        params: h
            .params
            .iter()
            .map(|p| match p {
                HostFnParam::Type(t) => HostFnParam::Type(t.clone()),
                HostFnParam::Value(v) => HostFnParam::Value(HostFnValueParam {
                    name: v.name.clone(),
                    ty: convert_type::<P, Q>(&v.ty),
                    meta: convert_meta(&v.meta),
                }),
            })
            .collect(),
        param_groups: h.param_groups.clone(),
        ret: convert_type::<P, Q>(&h.ret),
        meta: convert_meta(&h.meta),
        doc: h.doc.clone(),
    }
}

/// Phase-rebrand an [`Item`] across a phase boundary where every
/// surface-only extension type is statically uninhabited at the
/// source (so the `Labels` / `Equiv` / `Op` / `VariadicOperator` /
/// `LiteralAlias` arms discharge via `match *ext {}`). Bound set
/// matches `Prime` exactly. Used by
/// [`crate::backends::kio_prime`].
pub fn convert_item<P, Q>(item: &Item<P>) -> Item<Q>
where
    P: Phase<
            ExprTuple = Never,
            ExprFnPlaceholder = Never,
            ExprOpChain = Never,
            ExprLabelValue = Never,
            ExprRowLet = Never,
            ExprElab = Never,
            ExprBlockSyntax = Never,
            ExprRecCall = Never,
            ExprRecOrder = Never,
            ExprUfcs = Never,
            ExprEnriched = Never,
            ExprLow = Never,
            TypeLabelSugar = Never,
            ItemLabels = Never,
            ItemEquiv = Never,
            ItemOp = Never,
            ItemRecGroup = Never,
            ItemElaborator = Never,
            ItemLiteralAlias = Never,
        >,
    Q: Phase<ExprResolved = ()>,
    Q::FnDefRetElided: PhaseBridge<P::FnDefRetElided>,
    Q::FnPurity: PhaseBridge<P::FnPurity>,
{
    match item {
        Item::FnDef(d) => Item::FnDef(convert_fn_def::<P, Q>(d)),
        Item::RecGroup(_, ext) => match *ext {},
        Item::TypeRecGroup(group) => Item::TypeRecGroup(convert_type_rec_group::<P, Q>(group)),
        Item::TypeAlias(a) => Item::TypeAlias(convert_type_alias::<P, Q>(a)),
        Item::LiteralAlias(_, ext) => match *ext {},
        Item::Newtype(d) => Item::Newtype(convert_newtype::<P, Q>(d)),
        Item::Labels(_, ext) => match *ext {},
        Item::LabelForward(_, ext) => match *ext {},
        Item::Equiv(_, ext) => match *ext {},
        Item::Elaborator(_, ext) => match *ext {},
        Item::Op(_, ext) => match *ext {},
        Item::VariadicOperator(_, ext) => match *ext {},
        Item::HostType(h) => Item::HostType(convert_host_type::<P, Q>(h)),
        Item::HostFn(h) => Item::HostFn(convert_host_fn::<P, Q>(h)),
    }
}

impl<P: Phase<ExpressionOccurrence = ExpressionOccurrence>> Expr<P> {
    pub fn site(&self) -> ExpressionSite {
        ExpressionSite {
            id: self.occurrence().key(),
            span: self.span(),
        }
    }

    pub(crate) fn with_fresh_occurrence(mut self) -> Self {
        *self.occurrence_mut() = ExpressionOccurrence::fresh();
        self
    }
}

impl<P: Phase> Expr<P> {
    pub fn occurrence(&self) -> &P::ExpressionOccurrence {
        match self {
            Self::Path { occurrence, .. }
            | Self::Call { occurrence, .. }
            | Self::RecCall { occurrence, .. }
            | Self::FnExpr { occurrence, .. }
            | Self::Let { occurrence, .. }
            | Self::RowLet { occurrence, .. }
            | Self::Seq { occurrence, .. }
            | Self::Unit { occurrence, .. }
            | Self::StrLit { occurrence, .. }
            | Self::IntLit { occurrence, .. }
            | Self::FloatLit { occurrence, .. }
            | Self::BoolLit { occurrence, .. }
            | Self::Tuple { occurrence, .. }
            | Self::FnPlaceholder { occurrence, .. }
            | Self::LabelValue { occurrence, .. }
            | Self::Elaborator { occurrence, .. }
            | Self::RecOrder { occurrence, .. }
            | Self::RecQuote { occurrence, .. }
            | Self::UserElaborator { occurrence, .. }
            | Self::BlockCall { occurrence, .. }
            | Self::Ufcs { occurrence, .. }
            | Self::OpChain { occurrence, .. }
            | Self::EnrichedTuple { occurrence, .. }
            | Self::EnrichedProject { occurrence, .. }
            | Self::EnrichedInject { occurrence, .. }
            | Self::EnrichedMatch { occurrence, .. }
            | Self::EnrichedConditional { occurrence, .. }
            | Self::EnrichedRecord { occurrence, .. }
            | Self::EnrichedFieldGet { occurrence, .. }
            | Self::LowHostCall { occurrence, .. }
            | Self::LowModuleCall { occurrence, .. }
            | Self::LowQualifiedModuleCall { occurrence, .. }
            | Self::LowQualifiedNewtypeMember { occurrence, .. }
            | Self::LowNewtypeCtor { occurrence, .. }
            | Self::LowNewtypeProj { occurrence, .. }
            | Self::LowClosureCall { occurrence, .. }
            | Self::LowIndirectCall { occurrence, .. }
            | Self::LowTypeApplication { occurrence, .. }
            | Self::LowAbsurdCall { occurrence, .. }
            | Self::LowCpsProjectorApply { occurrence, .. }
            | Self::LowBoundRef { occurrence, .. }
            | Self::LowHostFnValueRef { occurrence, .. }
            | Self::LowModuleFnValueRef { occurrence, .. } => occurrence,
        }
    }

    pub fn occurrence_mut(&mut self) -> &mut P::ExpressionOccurrence {
        match self {
            Self::Path { occurrence, .. }
            | Self::Call { occurrence, .. }
            | Self::RecCall { occurrence, .. }
            | Self::FnExpr { occurrence, .. }
            | Self::Let { occurrence, .. }
            | Self::RowLet { occurrence, .. }
            | Self::Seq { occurrence, .. }
            | Self::Unit { occurrence, .. }
            | Self::StrLit { occurrence, .. }
            | Self::IntLit { occurrence, .. }
            | Self::FloatLit { occurrence, .. }
            | Self::BoolLit { occurrence, .. }
            | Self::Tuple { occurrence, .. }
            | Self::FnPlaceholder { occurrence, .. }
            | Self::LabelValue { occurrence, .. }
            | Self::Elaborator { occurrence, .. }
            | Self::RecOrder { occurrence, .. }
            | Self::RecQuote { occurrence, .. }
            | Self::UserElaborator { occurrence, .. }
            | Self::BlockCall { occurrence, .. }
            | Self::Ufcs { occurrence, .. }
            | Self::OpChain { occurrence, .. }
            | Self::EnrichedTuple { occurrence, .. }
            | Self::EnrichedProject { occurrence, .. }
            | Self::EnrichedInject { occurrence, .. }
            | Self::EnrichedMatch { occurrence, .. }
            | Self::EnrichedConditional { occurrence, .. }
            | Self::EnrichedRecord { occurrence, .. }
            | Self::EnrichedFieldGet { occurrence, .. }
            | Self::LowHostCall { occurrence, .. }
            | Self::LowModuleCall { occurrence, .. }
            | Self::LowQualifiedModuleCall { occurrence, .. }
            | Self::LowQualifiedNewtypeMember { occurrence, .. }
            | Self::LowNewtypeCtor { occurrence, .. }
            | Self::LowNewtypeProj { occurrence, .. }
            | Self::LowClosureCall { occurrence, .. }
            | Self::LowIndirectCall { occurrence, .. }
            | Self::LowTypeApplication { occurrence, .. }
            | Self::LowAbsurdCall { occurrence, .. }
            | Self::LowCpsProjectorApply { occurrence, .. }
            | Self::LowBoundRef { occurrence, .. }
            | Self::LowHostFnValueRef { occurrence, .. }
            | Self::LowModuleFnValueRef { occurrence, .. } => occurrence,
        }
    }

    pub fn span(&self) -> Span {
        self.meta().span
    }

    pub fn meta(&self) -> &Meta<P> {
        match self {
            Expr::Path { meta, .. }
            | Expr::Call { meta, .. }
            | Expr::RecCall { meta, .. }
            | Expr::FnExpr { meta, .. }
            | Expr::Let { meta, .. }
            | Expr::RowLet { meta, .. }
            | Expr::Seq { meta, .. }
            | Expr::Unit {
                occurrence: _,
                meta,
            }
            | Expr::StrLit { meta, .. }
            | Expr::IntLit { meta, .. }
            | Expr::FloatLit { meta, .. }
            | Expr::BoolLit { meta, .. }
            | Expr::Tuple { meta, .. }
            | Expr::FnPlaceholder { meta, .. }
            | Expr::LabelValue { meta, .. }
            | Expr::Elaborator { meta, .. }
            | Expr::RecOrder { meta, .. }
            | Expr::RecQuote { meta, .. }
            | Expr::UserElaborator { meta, .. }
            | Expr::BlockCall { meta, .. }
            | Expr::Ufcs { meta, .. }
            | Expr::OpChain { meta, .. }
            | Expr::EnrichedTuple { meta, .. }
            | Expr::EnrichedProject { meta, .. }
            | Expr::EnrichedInject { meta, .. }
            | Expr::EnrichedMatch { meta, .. }
            | Expr::EnrichedConditional { meta, .. }
            | Expr::EnrichedRecord { meta, .. }
            | Expr::EnrichedFieldGet { meta, .. }
            | Expr::LowHostCall { meta, .. }
            | Expr::LowModuleCall { meta, .. }
            | Expr::LowQualifiedModuleCall { meta, .. }
            | Expr::LowQualifiedNewtypeMember { meta, .. }
            | Expr::LowNewtypeCtor { meta, .. }
            | Expr::LowNewtypeProj { meta, .. }
            | Expr::LowClosureCall { meta, .. }
            | Expr::LowIndirectCall { meta, .. }
            | Expr::LowTypeApplication { meta, .. }
            | Expr::LowAbsurdCall { meta, .. }
            | Expr::LowCpsProjectorApply { meta, .. }
            | Expr::LowBoundRef { meta, .. }
            | Expr::LowHostFnValueRef { meta, .. }
            | Expr::LowModuleFnValueRef { meta, .. } => meta,
        }
    }

    pub fn meta_mut(&mut self) -> &mut Meta<P> {
        match self {
            Expr::Path { meta, .. }
            | Expr::Call { meta, .. }
            | Expr::RecCall { meta, .. }
            | Expr::FnExpr { meta, .. }
            | Expr::Let { meta, .. }
            | Expr::RowLet { meta, .. }
            | Expr::Seq { meta, .. }
            | Expr::Unit {
                occurrence: _,
                meta,
            }
            | Expr::StrLit { meta, .. }
            | Expr::IntLit { meta, .. }
            | Expr::FloatLit { meta, .. }
            | Expr::BoolLit { meta, .. }
            | Expr::Tuple { meta, .. }
            | Expr::FnPlaceholder { meta, .. }
            | Expr::LabelValue { meta, .. }
            | Expr::Elaborator { meta, .. }
            | Expr::RecOrder { meta, .. }
            | Expr::RecQuote { meta, .. }
            | Expr::UserElaborator { meta, .. }
            | Expr::BlockCall { meta, .. }
            | Expr::Ufcs { meta, .. }
            | Expr::OpChain { meta, .. }
            | Expr::EnrichedTuple { meta, .. }
            | Expr::EnrichedProject { meta, .. }
            | Expr::EnrichedInject { meta, .. }
            | Expr::EnrichedMatch { meta, .. }
            | Expr::EnrichedConditional { meta, .. }
            | Expr::EnrichedRecord { meta, .. }
            | Expr::EnrichedFieldGet { meta, .. }
            | Expr::LowHostCall { meta, .. }
            | Expr::LowModuleCall { meta, .. }
            | Expr::LowQualifiedModuleCall { meta, .. }
            | Expr::LowQualifiedNewtypeMember { meta, .. }
            | Expr::LowNewtypeCtor { meta, .. }
            | Expr::LowNewtypeProj { meta, .. }
            | Expr::LowClosureCall { meta, .. }
            | Expr::LowIndirectCall { meta, .. }
            | Expr::LowTypeApplication { meta, .. }
            | Expr::LowAbsurdCall { meta, .. }
            | Expr::LowCpsProjectorApply { meta, .. }
            | Expr::LowBoundRef { meta, .. }
            | Expr::LowHostFnValueRef { meta, .. }
            | Expr::LowModuleFnValueRef { meta, .. } => meta,
        }
    }

    /// Build an `Expr::Call` for a synthesized call site — i.e.,
    /// one not derived from user source, so there is no per-arg
    /// leading trivia to preserve. `arg_trivia` is set to
    /// [`Default::default()`], which is the empty unit `()` at
    /// every post-parse phase (where the typer / codegen run) and
    /// an empty `Vec<Vec<Trivia>>` at [`Surface`].
    ///
    /// Use this anywhere a pass synthesizes a call (intrinsics,
    /// generated wrappers, elaboration injection); user-source
    /// calls flow through the parser, which captures real trivia
    /// onto each arg's `meta.leading_trivia` directly.
    pub fn synth_call(callee: Expr<P>, args: Vec<CallArg<P>>, span: Span) -> Expr<P>
    where
        P: Phase<ExprResolved = ()>,
    {
        Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(callee),
            args,
            meta: Meta::new(span),
            ext: (),
        }
    }
}

impl<P: Phase> Item<P> {
    pub fn span(&self) -> Span {
        self.meta().span
    }

    pub fn meta(&self) -> &Meta<P> {
        match self {
            Item::FnDef(d) => &d.meta,
            Item::RecGroup(d, _) => &d.meta,
            Item::TypeRecGroup(d) => &d.meta,
            Item::TypeAlias(a) => &a.meta,
            Item::LiteralAlias(l, _) => &l.meta,
            Item::Newtype(d) => &d.meta,
            Item::Labels(d, _) => &d.meta,
            Item::LabelForward(d, _) => &d.meta,
            Item::Equiv(e, _) => &e.meta,
            Item::Elaborator(s, _) => &s.meta,
            Item::Op(d, _) => &d.meta,
            Item::VariadicOperator(d, _) => &d.meta,
            Item::HostType(h) => &h.meta,
            Item::HostFn(h) => &h.meta,
        }
    }

    pub fn meta_mut(&mut self) -> &mut Meta<P> {
        match self {
            Item::FnDef(d) => &mut d.meta,
            Item::RecGroup(d, _) => &mut d.meta,
            Item::TypeRecGroup(d) => &mut d.meta,
            Item::TypeAlias(a) => &mut a.meta,
            Item::LiteralAlias(l, _) => &mut l.meta,
            Item::Newtype(d) => &mut d.meta,
            Item::Labels(d, _) => &mut d.meta,
            Item::LabelForward(d, _) => &mut d.meta,
            Item::Equiv(e, _) => &mut e.meta,
            Item::Elaborator(s, _) => &mut s.meta,
            Item::Op(d, _) => &mut d.meta,
            Item::VariadicOperator(d, _) => &mut d.meta,
            Item::HostType(h) => &mut h.meta,
            Item::HostFn(h) => &mut h.meta,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn expression_occurrences_are_transient_and_structurally_neutral() {
        use super::{Expr, ExpressionOccurrence, Lowered, Meta};
        let unassigned = Expr::<Lowered>::Unit {
            occurrence: ExpressionOccurrence::default(),
            meta: Meta::new(crate::span::Span::new(3, 5)),
        };
        assert!(!unassigned.occurrence().is_assigned());
        let first = unassigned.clone().with_fresh_occurrence();
        let same = first.clone();
        let independent = first.clone().with_fresh_occurrence();
        assert_eq!(first.site().id, same.site().id);
        assert_ne!(first.site().id, independent.site().id);
        assert_eq!(first, independent);
        assert_eq!(first, unassigned);
        assert_eq!(format!("{first:?}"), format!("{unassigned:?}"));
        let serialized = serde_json::to_string(&first).expect("serialize expression");
        assert_eq!(serialized, serde_json::to_string(&unassigned).unwrap());
        let reparsed: Expr<Lowered> =
            serde_json::from_str(&serialized).expect("deserialize expression");
        assert!(!reparsed.occurrence().is_assigned());
    }

    use super::*;
    use crate::span::Span;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    fn type_param(name: &str) -> TypeParam {
        TypeParam {
            name: name.to_owned(),
            span: sp(),
            kind: None,
        }
    }

    fn unit_value_param(name: &str) -> Param<Surface> {
        Param {
            name: name.to_owned(),
            ty: Some(Type::Unit {
                meta: Meta::new(sp()),
            }),
            pattern: None,
            meta: Meta::new(sp()),
        }
    }

    fn host_unit_value_param(name: &str) -> HostFnValueParam<Surface> {
        HostFnValueParam {
            name: Some(name.to_owned()),
            ty: Type::Unit {
                meta: Meta::new(sp()),
            },
            meta: Meta::new(sp()),
        }
    }

    fn newtype_with_visibility(
        vis: Visibility,
        constructor_vis: Visibility,
        projector_vis: Visibility,
    ) -> Newtype<Surface> {
        let member = |name: &str, vis| TypeMember {
            vis,
            name: name.to_owned(),
            span: sp(),
            leading_trivia: Default::default(),
        };
        Newtype {
            vis,
            rec_span: None,
            name: "Opaque".to_owned(),
            name_span: sp(),
            type_params: Vec::new(),
            existential_params: Vec::new(),
            payload: Type::Unit {
                meta: Meta::new(sp()),
            },
            constructor: member("mk", constructor_vis),
            projector: member("un", projector_vis),
            meta: Meta::new(sp()),
            editable_span: None,
            doc: None,
        }
    }

    fn scoped_visibility() -> Visibility {
        Visibility::PublicIn(ModulePath {
            segments: vec![PathSegment::new("api", sp())],
            span: sp(),
        })
    }

    #[test]
    fn lexical_callable_path_derives_its_span_from_segments() {
        let path = LexicalCallablePath::new(vec![
            PathSegment::new("helpers", Span::new(3, 10)),
            PathSegment::new("run", Span::new(11, 14)),
        ]);

        assert_eq!(path.span(), Span::new(3, 14));
    }

    #[test]
    fn lexical_callable_path_rejects_an_empty_serialized_path() {
        let error = serde_json::from_str::<LexicalCallablePath>("[]")
            .expect_err("serialized declaration callable paths must remain non-empty");
        assert!(
            error
                .to_string()
                .contains("a lexical callable path must contain at least one segment")
        );
    }

    #[test]
    fn newtype_host_visibility_intersects_outer_and_member_visibility() {
        use Visibility::{Private, Public};

        let cases = [
            (Private, Public, Public, None),
            (scoped_visibility(), Public, Public, None),
            (Public, Private, Private, Some("opaque")),
            (Public, scoped_visibility(), Private, Some("opaque")),
            (Public, Public, Private, Some("constructor")),
            (Public, Private, Public, Some("projector")),
            (Public, Public, Public, Some("both")),
        ];

        for (outer, constructor, projector, expected) in cases {
            let newtype = newtype_with_visibility(outer, constructor, projector);
            let actual = newtype.host_surface().map(|surface| match surface {
                NewtypeHostSurface::Opaque => "opaque",
                NewtypeHostSurface::Constructor { .. } => "constructor",
                NewtypeHostSurface::Projector { .. } => "projector",
                NewtypeHostSurface::Both { .. } => "both",
            });
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn signature_constructors_canonicalize_type_binders() {
        let params = vec![
            SignatureParam::Type(type_param("A")),
            SignatureParam::Type(type_param("B")),
            SignatureParam::Value(unit_value_param("x")),
            SignatureParam::Value(unit_value_param("y")),
        ];
        let expected = vec![
            SignatureGroupKind::Type { len: 1 },
            SignatureGroupKind::Type { len: 1 },
            SignatureGroupKind::Value { len: 2 },
        ];

        let inferred: Signature<Surface> = Signature::new(params.clone());
        assert_eq!(inferred.groups, expected);

        let supplied: Signature<Surface> = Signature::from_parts(
            params,
            vec![
                SignatureGroupKind::Type { len: 2 },
                SignatureGroupKind::Value { len: 2 },
            ],
        );
        assert_eq!(supplied.groups, expected);

        let grouped: Signature<Surface> = Signature::from_groups(vec![
            SignatureGroup::Type(vec![type_param("A"), type_param("B")]),
            SignatureGroup::Value(vec![unit_value_param("x"), unit_value_param("y")]),
        ]);
        assert_eq!(grouped.groups, expected);
    }

    #[test]
    fn signature_constructors_preserve_value_group_boundaries() {
        let signature: Signature<Surface> = Signature::from_parts(
            vec![
                SignatureParam::Type(type_param("A")),
                SignatureParam::Type(type_param("B")),
                SignatureParam::Value(unit_value_param("x")),
                SignatureParam::Value(unit_value_param("y")),
                SignatureParam::Value(unit_value_param("z")),
            ],
            vec![
                SignatureGroupKind::Type { len: 2 },
                SignatureGroupKind::Value { len: 1 },
                SignatureGroupKind::Value { len: 2 },
            ],
        );
        assert_eq!(
            signature.groups,
            vec![
                SignatureGroupKind::Type { len: 1 },
                SignatureGroupKind::Type { len: 1 },
                SignatureGroupKind::Value { len: 1 },
                SignatureGroupKind::Value { len: 2 },
            ]
        );
        assert_eq!(
            signature.value_param_names().collect::<Vec<_>>(),
            vec!["x", "y", "z"]
        );
    }

    #[test]
    fn host_fn_constructors_canonicalize_type_binders() {
        let params = vec![
            HostFnParam::Type(type_param("A")),
            HostFnParam::Type(type_param("B")),
            HostFnParam::Value(host_unit_value_param("x")),
            HostFnParam::Value(host_unit_value_param("y")),
        ];
        let expected = vec![
            SignatureGroupKind::Type { len: 1 },
            SignatureGroupKind::Type { len: 1 },
            SignatureGroupKind::Value { len: 2 },
        ];
        assert_eq!(infer_host_fn_groups(&params), expected);

        let host_fn: HostFn<Surface> = host_fn_from_groups(
            "f".to_owned(),
            vec![
                HostFnParamGroup::Type(vec![type_param("A"), type_param("B")]),
                HostFnParamGroup::Value(vec![
                    host_unit_value_param("x"),
                    host_unit_value_param("y"),
                ]),
            ],
            Type::Unit {
                meta: Meta::new(sp()),
            },
            Meta::new(sp()),
            None,
        );
        assert_eq!(host_fn.param_groups, expected);
    }

    /// `Type::synth_path` builds a `Type::Path` with the given
    /// segments and args — no parallel-trivia bookkeeping needed.
    #[test]
    fn synth_path_returns_path_with_args() {
        let t: Type<Surface> = Type::synth_path(
            vec!["Foo".into()],
            vec![
                Type::Unit {
                    meta: Meta::new(sp())
                };
                3
            ],
            sp(),
        );
        match t {
            Type::Path { args, .. } => {
                assert_eq!(args.len(), 3);
            }
            _ => panic!("expected Type::Path"),
        }
    }

    /// `Expr::synth_call` builds the Call variant correctly across
    /// phases without per-phase branching at the call site.
    #[test]
    fn synth_call_works_at_both_surface_and_lowered() {
        let span = sp();
        let surface_call: Expr<Surface> = Expr::synth_call(
            Expr::Path {
                occurrence: Default::default(),
                segments: vec![PathSegment::new("__pair__", span)],
                meta: Meta::new(span),
                ext: (),
            },
            vec![
                CallArg::Type(Type::Unit {
                    meta: Meta::new(span),
                }),
                CallArg::Value(Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(span),
                }),
            ],
            span,
        );
        match surface_call {
            Expr::Call { args, .. } => {
                assert_eq!(args.len(), 2);
            }
            _ => panic!("expected Expr::Call"),
        }
        let lowered_call: Expr<Lowered> = Expr::synth_call(
            Expr::Path {
                occurrence: Default::default(),
                segments: vec![PathSegment::new("__pair__", span)],
                meta: Meta::new(span),
                ext: (),
            },
            vec![CallArg::Value(Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(span),
            })],
            span,
        );
        match lowered_call {
            Expr::Call { args, .. } => {
                assert_eq!(args.len(), 1);
            }
            _ => panic!("expected Expr::Call"),
        }
    }

    /// Every `Expr` and `Type` variant returns its declared span via
    /// `span()`. A regression that forgets a new variant in either
    /// match would surface here as a "no match arm" compile error,
    /// but this test catches the runtime invariant for variants
    /// already present.
    #[test]
    fn expr_span_returns_declared_span() {
        let span = Span::new(7, 19);
        let e: Expr<Surface> = Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(span),
        };
        assert_eq!(e.span(), span);
        let p: Expr<Surface> = Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::new("x", span)],
            meta: Meta::new(span),
            ext: (),
        };
        assert_eq!(p.span(), span);
    }

    #[test]
    fn type_span_returns_declared_span() {
        let span = Span::new(3, 11);
        let u: Type<Surface> = Type::Unit {
            meta: Meta::new(span),
        };
        assert_eq!(u.span(), span);
        let b: Type<Surface> = Type::Bottom {
            meta: Meta::new(span),
        };
        assert_eq!(b.span(), span);
        let p: Type<Surface> = Type::synth_path(vec!["A".into()], Vec::new(), span);
        assert_eq!(p.span(), span);
    }

    /// `convert_type` round-trips a Lowered `Type` to Prime, walking
    /// segments / args / spans across the phase boundary. Trivia
    /// fields are `()` at both Lowered and Prime, so there's nothing
    /// to preserve there — the test pins the structural rebrand.
    #[test]
    fn convert_type_rebrands_phase_without_loss() {
        let span = sp();
        let inner: Type<Lowered> = Type::synth_path(vec!["I32".into()], Vec::new(), span);
        let outer: Type<Lowered> = Type::synth_path(vec!["Box".into()], vec![inner], span);
        let converted: Type<Prime> = convert_type(&outer);
        match converted {
            Type::Path { segments, args, .. } => {
                assert_eq!(segments, vec!["Box".to_owned()]);
                assert_eq!(args.len(), 1);
                if let Type::Path {
                    segments: inner_segs,
                    ..
                } = &args[0]
                {
                    assert_eq!(inner_segs, &vec!["I32".to_owned()]);
                } else {
                    panic!("inner not a Path");
                }
            }
            _ => panic!("outer not a Path"),
        }
    }

    // ---- Type::synth_scheme_from_signature_params ----

    /// Helpers for the scheme-builder tests. `vp(name, ty)` builds
    /// a value SignatureParam; `tp(name)` builds a type binder.
    fn vp_sp(name: &str, ty: Type<Surface>) -> SignatureParam<Surface> {
        SignatureParam::Value(Param {
            name: name.into(),
            ty: Some(ty),
            pattern: None,
            meta: Meta::new(sp()),
        })
    }

    fn tp_sp(name: &str) -> SignatureParam<Surface> {
        SignatureParam::Type(TypeParam {
            name: name.into(),
            span: sp(),
            kind: None,
        })
    }

    fn path(name: &str) -> Type<Surface> {
        Type::synth_path(vec![name.into()], Vec::new(), sp())
    }

    fn unit_ty() -> Type<Surface> {
        Type::Unit {
            meta: Meta::new(sp()),
        }
    }

    /// Empty params produce `Function(Unit, R)`.
    #[test]
    fn synth_scheme_no_params_wraps_function_unit() {
        let t = <Type<Surface>>::synth_scheme_from_signature_params(&[], path("R"), sp());
        match t {
            Type::Function { param, ret, .. } => {
                assert!(matches!(*param, Type::Unit { .. }));
                assert!(matches!(*ret, Type::Path { .. }));
            }
            _ => panic!("expected Function(Unit, R)"),
        }
    }

    /// Leading binders with no values produce
    /// `Forall(A, Function(Unit, R))` because every declaration
    /// is callable.
    #[test]
    fn synth_scheme_only_binders_still_wraps_function() {
        let t = <Type<Surface>>::synth_scheme_from_signature_params(&[tp_sp("A")], path("R"), sp());
        match t {
            Type::Forall { param, body, .. } => {
                assert_eq!(param.name, "A");
                assert!(matches!(*body, Type::Function { .. }));
            }
            _ => panic!("expected Forall(A, Function(Unit, R))"),
        }
    }

    /// Leading binders + values: `[A][B](x: A, y: B) -> R`
    /// produces `Forall(A, Forall(B, Function(Product(A, B), R)))`.
    #[test]
    fn synth_scheme_leading_binders_values_run() {
        let t = <Type<Surface>>::synth_scheme_from_signature_params(
            &[
                tp_sp("A"),
                tp_sp("B"),
                vp_sp("x", path("A")),
                vp_sp("y", path("B")),
            ],
            path("R"),
            sp(),
        );
        match t {
            Type::Forall { param, body, .. } => {
                assert_eq!(param.name, "A");
                match *body {
                    Type::Forall {
                        param: inner_param,
                        body: inner_body,
                        ..
                    } => {
                        assert_eq!(inner_param.name, "B");
                        match *inner_body {
                            Type::Function { param, .. } => match *param {
                                Type::Product { left, right, .. } => {
                                    assert!(matches!(*left, Type::Path { .. }));
                                    assert!(matches!(*right, Type::Path { .. }));
                                }
                                _ => panic!("expected Product(A, B) param"),
                            },
                            _ => panic!("expected inner Forall body to be Function"),
                        }
                    }
                    _ => panic!("expected outer Forall body to be Forall"),
                }
            }
            _ => panic!("expected outer Forall"),
        }
    }

    /// Mid-position binder: `(P0)[A] P1 -> R` produces
    /// `Function(P0, Forall([A], Function(P1, R)))`.
    #[test]
    fn synth_scheme_mid_position_binder() {
        let t = <Type<Surface>>::synth_scheme_from_signature_params(
            &[vp_sp("x", unit_ty()), tp_sp("A"), vp_sp("y", path("A"))],
            path("R"),
            sp(),
        );
        match t {
            Type::Function { param, ret, .. } => {
                assert!(matches!(*param, Type::Unit { .. }));
                match *ret {
                    Type::Forall { param, body, .. } => {
                        assert_eq!(param.name, "A");
                        match *body {
                            Type::Function {
                                param: inner_p,
                                ret: inner_r,
                                ..
                            } => {
                                assert!(matches!(*inner_p, Type::Path { .. }));
                                assert!(matches!(*inner_r, Type::Path { .. }));
                            }
                            _ => panic!("inner ret not Function"),
                        }
                    }
                    _ => panic!("outer ret not Forall"),
                }
            }
            _ => panic!("outer not Function"),
        }
    }

    /// Trailing binder: `(x: P0, y: P1)[A] -> R` produces
    /// `Function(Product(P0, P1), Forall([A], R))`.
    #[test]
    fn synth_scheme_trailing_binder() {
        let t = <Type<Surface>>::synth_scheme_from_signature_params(
            &[vp_sp("x", unit_ty()), vp_sp("y", unit_ty()), tp_sp("A")],
            path("R"),
            sp(),
        );
        match t {
            Type::Function { param, ret, .. } => {
                assert!(matches!(*param, Type::Product { .. }));
                match *ret {
                    Type::Forall { param, body, .. } => {
                        assert_eq!(param.name, "A");
                        assert!(matches!(*body, Type::Path { .. }));
                    }
                    _ => panic!("outer ret not Forall"),
                }
            }
            _ => panic!("outer not Function"),
        }
    }

    /// Consecutive binders become nested Forall nodes:
    /// `[A][B] . -> R` → `Forall(A, Forall(B, Function(Unit, R)))`.
    #[test]
    fn synth_scheme_consecutive_binders_cluster() {
        let t = <Type<Surface>>::synth_scheme_from_signature_params(
            &[tp_sp("A"), tp_sp("B")],
            path("R"),
            sp(),
        );
        match t {
            Type::Forall { param, body, .. } => {
                assert_eq!(param.name, "A");
                match *body {
                    Type::Forall {
                        param: inner_param,
                        body: inner_body,
                        ..
                    } => {
                        assert_eq!(inner_param.name, "B");
                        assert!(matches!(*inner_body, Type::Function { .. }));
                    }
                    _ => panic!("expected nested Forall"),
                }
            }
            _ => panic!("expected outer Forall"),
        }
    }

    fn signature_group_shape(group: SignatureGroupRef<'_, Surface>) -> (&'static str, Vec<String>) {
        match group {
            SignatureGroupRef::Type(params) => (
                "type",
                params
                    .iter()
                    .map(|param| match param {
                        SignatureParam::Type(param) => param.name.clone(),
                        SignatureParam::Value(_) => {
                            panic!("a type group contained a value parameter")
                        }
                    })
                    .collect(),
            ),
            SignatureGroupRef::Value(params) => (
                "value",
                params
                    .iter()
                    .map(|param| match param {
                        SignatureParam::Value(param) => param.name.clone(),
                        SignatureParam::Type(_) => {
                            panic!("a value group contained a type parameter")
                        }
                    })
                    .collect(),
            ),
        }
    }

    #[test]
    fn signature_group_cursor_preserves_stored_empty_layers_from_both_ends() {
        let signature: Signature<Surface> = Signature::from_groups(vec![
            SignatureGroup::Type(vec![type_param("A")]),
            SignatureGroup::Value(vec![]),
            SignatureGroup::Type(vec![type_param("B")]),
            SignatureGroup::Value(vec![unit_value_param("x"), unit_value_param("y")]),
            SignatureGroup::Type(vec![type_param("C")]),
        ]);

        let mut groups = signature.canonical_group_refs();
        assert_eq!(groups.size_hint(), (5, Some(5)));
        assert_eq!(
            signature_group_shape(groups.next().expect("leading type group")),
            ("type", vec!["A".to_owned()])
        );
        assert_eq!(
            signature_group_shape(groups.next_back().expect("trailing type group")),
            ("type", vec!["C".to_owned()])
        );
        assert_eq!(
            signature_group_shape(groups.next_back().expect("trailing value group")),
            ("value", vec!["x".to_owned(), "y".to_owned()])
        );
        assert_eq!(
            signature_group_shape(groups.next().expect("explicit empty value group")),
            ("value", vec![])
        );
        assert_eq!(
            signature_group_shape(groups.next().expect("middle type group")),
            ("type", vec!["B".to_owned()])
        );
        assert_eq!(groups.size_hint(), (0, Some(0)));
        assert!(groups.next().is_none());
        assert!(groups.next_back().is_none());
    }

    #[test]
    fn signature_group_cursor_infers_value_runs_without_collecting_groups() {
        let signature: Signature<Surface> = Signature {
            params: vec![
                SignatureParam::Type(type_param("A")),
                SignatureParam::Value(unit_value_param("x")),
                SignatureParam::Value(unit_value_param("y")),
                SignatureParam::Type(type_param("B")),
                SignatureParam::Value(unit_value_param("z")),
            ],
            groups: vec![],
        };

        let forward = signature
            .canonical_group_refs()
            .map(signature_group_shape)
            .collect::<Vec<_>>();
        assert_eq!(
            forward,
            vec![
                ("type", vec!["A".to_owned()]),
                ("value", vec!["x".to_owned(), "y".to_owned()]),
                ("type", vec!["B".to_owned()]),
                ("value", vec!["z".to_owned()]),
            ]
        );

        let reverse = signature
            .canonical_group_refs()
            .rev()
            .map(signature_group_shape)
            .collect::<Vec<_>>();
        assert_eq!(reverse, forward.into_iter().rev().collect::<Vec<_>>());
    }

    #[test]
    fn stored_and_inferred_signature_groups_have_identical_cursor_and_scheme_shapes() {
        let params = vec![
            SignatureParam::Type(type_param("A")),
            SignatureParam::Value(unit_value_param("x")),
            SignatureParam::Value(unit_value_param("y")),
            SignatureParam::Type(type_param("B")),
            SignatureParam::Value(unit_value_param("z")),
        ];
        let stored = Signature::new(params.clone());
        let inferred = Signature {
            params,
            groups: vec![],
        };

        let mut stored_groups = stored.canonical_group_refs();
        let mut inferred_groups = inferred.canonical_group_refs();
        for take_front in [true, false, false, true] {
            let stored_group = if take_front {
                stored_groups.next()
            } else {
                stored_groups.next_back()
            };
            let inferred_group = if take_front {
                inferred_groups.next()
            } else {
                inferred_groups.next_back()
            };
            assert_eq!(
                stored_group.map(signature_group_shape),
                inferred_group.map(signature_group_shape)
            );
        }
        assert!(stored_groups.next().is_none());
        assert!(inferred_groups.next_back().is_none());

        assert_eq!(
            stored.signature_ty(path("R"), sp()),
            inferred.signature_ty(path("R"), sp())
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "stored signature groups cover every parameter")]
    fn signature_group_cursor_rejects_under_covering_public_group_storage() {
        let signature: Signature<Surface> = Signature {
            params: vec![
                SignatureParam::Type(type_param("A")),
                SignatureParam::Value(unit_value_param("x")),
            ],
            groups: vec![SignatureGroupKind::Type { len: 1 }],
        };

        let _ = signature.canonical_group_refs().count();
    }

    #[test]
    #[should_panic(expected = "stored signature groups exceed their parameter slice")]
    fn signature_group_cursor_rejects_over_covering_public_group_storage() {
        let signature: Signature<Surface> = Signature {
            params: vec![SignatureParam::Type(type_param("A"))],
            groups: vec![SignatureGroupKind::Type { len: 2 }],
        };

        let _ = signature.canonical_group_refs().count();
    }

    #[test]
    fn signature_scheme_route_uses_the_borrowed_group_cursor() {
        reset_signature_group_cursor_test_counts();
        let signature: Signature<Surface> = Signature::from_groups(vec![
            SignatureGroup::Type(vec![type_param("A")]),
            SignatureGroup::Value(vec![unit_value_param("x")]),
        ]);

        let _ = signature.signature_ty(path("R"), sp());
        let (signature_routes, params_routes, cursor_steps) = signature_group_cursor_test_counts();
        assert_eq!(signature_routes, 1);
        assert_eq!(params_routes, 0);
        assert!(cursor_steps > 0);
    }

    #[test]
    fn signature_params_route_uses_the_borrowed_group_cursor() {
        reset_signature_group_cursor_test_counts();
        let params = vec![
            SignatureParam::Type(type_param("A")),
            SignatureParam::Value(unit_value_param("x")),
        ];

        let _ = Type::synth_scheme_from_signature_params(&params, path("R"), sp());
        let (signature_routes, params_routes, cursor_steps) = signature_group_cursor_test_counts();
        assert_eq!(signature_routes, 0);
        assert_eq!(params_routes, 1);
        assert!(cursor_steps > 0);
    }

    #[test]
    fn type_goal_phase_witness_is_lowered_only() {
        fn assert_allowed<P: Phase<TypeGoal = ()>>() {}
        fn assert_forbidden<P: Phase<TypeGoal = Never>>() {}

        assert_forbidden::<Surface>();
        assert_forbidden::<Desugared>();
        assert_allowed::<Lowered>();
        assert_forbidden::<UncheckedPrime>();
        assert_forbidden::<Prime>();
        assert_forbidden::<Enriched>();
        assert_forbidden::<Routed>();
    }

    #[test]
    fn type_goal_identity_includes_domain_owner_and_slot() {
        assert_eq!(
            TypeGoalRef::for_test(5, 7, 11),
            TypeGoalRef::for_test(5, 7, 11)
        );
        assert_ne!(
            TypeGoalRef::for_test(5, 7, 11),
            TypeGoalRef::for_test(6, 7, 11)
        );
        assert_ne!(
            TypeGoalRef::for_test(5, 7, 11),
            TypeGoalRef::for_test(5, 8, 11)
        );
        assert_ne!(
            TypeGoalRef::for_test(5, 7, 11),
            TypeGoalRef::for_test(5, 7, 12)
        );
    }

    #[test]
    fn type_goal_identity_rejects_serialization() {
        let error = serde_json::to_string(&TypeGoalRef::for_test(5, 7, 11))
            .expect_err("a transient goal identity must not be serialized");
        assert!(
            error
                .to_string()
                .contains("type-inference goals cannot be serialized")
        );
    }

    #[test]
    fn type_goal_identity_rejects_deserialization() {
        let error = serde_json::from_str::<TypeGoalRef>("null")
            .expect_err("a transient goal identity must not be deserialized");
        assert!(
            error
                .to_string()
                .contains("type-inference goals cannot be deserialized")
        );
    }

    #[test]
    fn open_type_goal_cannot_cross_a_phase_boundary() {
        let goal = Type::<Lowered>::Goal {
            goal: TypeGoalRef::for_test(5, 7, 11),
            args: vec![Type::synth_path(vec!["A".to_owned()], Vec::new(), sp())],
            meta: Meta::new(sp()),
            ext: (),
        };

        assert!(
            std::panic::catch_unwind(|| convert_type::<Lowered, Prime>(&goal)).is_err(),
            "an owning inference domain must close and zonk its goals before conversion"
        );
    }
}
