//! Internal typed AST for the generator's Kio' subset.

/// The base (non-function) types the generator emits today.
///
/// Each one is bound in the emitted package's host to a
/// `type … role(<role>);` declaration; literals type-check against the
/// matching host type in scope via the typer's
/// three-tier resolution. Naming follows the size-suffix
/// convention used by the goldens (`I8`/`I32`/`F64`/…); every
/// numeric base has its own variant so we can dispatch literal
/// generation off the base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Base {
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
    Str,
    Bool,
}

impl Base {
    /// Every base, in a fixed order. Stable across runs — the
    /// distribution self-tests rely on every base eventually
    /// appearing as a return type.
    pub fn all() -> &'static [Base] {
        use Base::*;
        &[
            I8, I16, I32, I64, I128, U8, U16, U32, U64, U128, F32, F64, Str, Bool,
        ]
    }

    /// Type-name as it appears in Kio' source: `I8`, …, `String`,
    /// `Bool`. Type-shaped — Kio's parser routes exact type-name
    /// identifiers to the type position at call sites, so this also
    /// happens to be the "safe to use as a type argument" form.
    pub fn type_name(self) -> &'static str {
        use Base::*;
        match self {
            I8 => "I8",
            I16 => "I16",
            I32 => "I32",
            I64 => "I64",
            I128 => "I128",
            U8 => "U8",
            U16 => "U16",
            U32 => "U32",
            U64 => "U64",
            U128 => "U128",
            F32 => "F32",
            F64 => "F64",
            Str => "String",
            Bool => "Bool",
        }
    }

    pub fn is_integer(self) -> bool {
        use Base::*;
        matches!(
            self,
            I8 | I16 | I32 | I64 | I128 | U8 | U16 | U32 | U64 | U128
        )
    }

    pub fn is_float(self) -> bool {
        matches!(self, Base::F32 | Base::F64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Base(Base),
    /// `.`
    Unit,
    /// `(T1 & T2) -> T`
    Fun(Vec<Type>, Box<Type>),
    /// `(A & B)` — always parenthesized in Kio'.
    Product(Box<Type>, Box<Type>),
    /// `(A | B)` — always parenthesized in Kio'.
    Sum(Box<Type>, Box<Type>),
    /// A nominal type minted by a top-level `newtype` declaration.
    /// `payload` is the wrapped type (what the constructor accepts
    /// and the projector returns); `constructor` / `projector` are
    /// the declared member names. The whole struct is auto-equated,
    /// so two `Nominal`s match only when every field agrees — fine
    /// today because the generator's catalog is fixed and consistent.
    Nominal {
        name: String,
        payload: Box<Type>,
        constructor: String,
        projector: String,
    },
    /// A single-parameter parametric `newtype` instantiated at
    /// `payload`: the surface form `Name(payload)`. Same shape as
    /// `Nominal` but with a polymorphic constructor / projector
    /// reached through wrapper helpers in the generated root module (see
    /// `pub fn box_any[A](x: A) -> Box(A)` and friends). Equality
    /// is structural across all fields.
    Param {
        name: String,
        payload: Box<Type>,
        /// Polymorphic wrapper that constructs `Name(payload)` from a
        /// payload value: signature `[A](x: A) -> Name(A)`.
        constructor: String,
        /// Polymorphic wrapper that projects `Name(payload)` back to
        /// payload: signature `[A](x: Name(A)) -> A`.
        projector: String,
    },
    /// A label declared via `labels` — sugar over a generated
    /// `newtype`. The `label` field is the surface lowercase spelling
    /// (`greet`); `newtype_name` is the compiler-minted type
    /// (`Greet`). At type-position render, this prints as the generated
    /// type name. At value level, construction uses
    /// `Term::LabelConstruct` for `{label = e}` surface sugar, while
    /// Kio' rendering calls the generated `Greet.mk` member.
    Label {
        label: String,
        newtype_name: String,
        payload: Box<Type>,
        constructor: String,
        projector: String,
    },
}

impl Type {
    pub fn i32() -> Self {
        Type::Base(Base::I32)
    }
    pub fn str() -> Self {
        Type::Base(Base::Str)
    }
    pub fn bool() -> Self {
        Type::Base(Base::Bool)
    }
}

#[derive(Debug, Clone)]
pub enum Term {
    /// A numeric literal at one of the integer or float bases. The
    /// `repr` is the rendered numeric portion (digits, optionally a
    /// `.<frac>` for floats); the suffix is determined by `base`.
    NumLit {
        base: Base,
        repr: String,
    },
    StrLit(String),
    BoolLit(bool),
    /// `()` — the unique inhabitant of `Type::Unit`.
    UnitLit,
    Var(String),
    Lambda {
        params: Vec<(String, Type)>,
        ret: Type,
        body: Box<Term>,
    },
    App(Box<Term>, Vec<Term>),
    Let {
        name: String,
        ty: Type,
        rhs: Box<Term>,
        body: Box<Term>,
    },
    /// Call a built-in polymorphic helper or intrinsic (`id`,
    /// `__if_then_else__`, `__pair__`, `__fst__`, `__snd__`,
    /// `__left__`, `__right__`, `__either__`, `const1`,
    /// `const2`). Renders as `name(T1, ..., Tk, a1, ..., aN)` — type
    /// arguments first, then value arguments. Module-level
    /// definitions for `id`/`const1`/`const2` are emitted by
    /// `emit::write_case`; the `__name__` intrinsics come into scope
    /// through `import __intrinsics__;`.
    PolyCall {
        name: String,
        type_args: Vec<Type>,
        args: Vec<Term>,
    },
    /// Call a `newtype`'s `constructor` or `projector` member by
    /// dotted path: `I32_box.mk_i32_box(42i32)` or
    /// `I32_box.un_i32_box(box)`. Nullary newtypes only today —
    /// parametric / `rec` newtypes need their own follow-up. Not
    /// produced by the current generator (kio-rs doesn't yet
    /// support cross-module dotted-path access, so the generator
    /// goes through generated wrapper functions); kept in the AST as
    /// the right shape for a future commit.
    TypeMember {
        type_name: String,
        member: String,
        args: Vec<Term>,
    },
    /// `if! cond { then } else { else_ }`. Both arms have the same
    /// result type; the whole expression has that type.
    /// Renders two ways depending on `RenderOpts.surface`:
    /// - surface: an imported `if! <cond> { <then> } else { <else_> }` call.
    /// - Kio': `__if_then_else__(R, cond, .() { then }, .() { else_ })`,
    ///   matching the ordinary conditional elaborator's result.
    IfElse {
        cond: Box<Term>,
        then: Box<Term>,
        else_: Box<Term>,
        then_ty: Type,
        else_ty: Type,
    },
    /// `{<label> = <payload>}` — surface sugar for the generated
    /// constructor-member call `<constructor>(<payload>)`. Renders two ways:
    /// - surface: `{<label> = <payload>}`.
    /// - Kio': `<constructor>(<payload>)`.
    LabelConstruct {
        label: String,
        constructor: String,
        payload: Box<Term>,
    },
    /// `match!(<scrutinee>, (.(p1: A) { <left_body> }, .(p2: B) { <right_body> }))`
    /// — surface pattern matching with two clauses against a sum-
    /// typed scrutinee `(A | B) -> ret_ty`. Both clause bodies have
    /// the same `ret_ty`. Renders two ways:
    /// - surface: `match!(<scrutinee>, (.(p1: A) { <left> }, .(p2: B) { <right> }))`.
    /// - Kio': `__either__(A, B, ret_ty, <scrutinee>, .(p1) { <left> }, .(p2) { <right> })`,
    ///   the same shape the typer's match elaboration produces (for
    ///   the simple two-clause case).
    MatchBang {
        scrutinee: Box<Term>,
        left_param: String,
        left_ty: Type,
        left_body: Box<Term>,
        right_param: String,
        right_ty: Type,
        right_body: Box<Term>,
        ret_ty: Type,
    },
    /// `into!(<inner>)` — surface elaborator form, total and
    /// information-preserving. The generator only emits identity
    /// coercions, so the Kio' fallback is sound: surface mode
    /// renders `into!(<inner>)`, Kio' mode renders just
    /// `<inner>`. The elaborator accepts the identity coercion at any
    /// annotated-target position where source and target types
    /// coincide.
    Into {
        inner: Box<Term>,
    },
    /// `onto!(<inner>)` — surface elaborator form, total but lossy.
    /// Same identity-only constraint as `Into`, so Kio' falls back
    /// to `<inner>`.
    Onto {
        inner: Box<Term>,
    },
    /// `iso!(<inner>)` — bijection-only surface elaborator form. The
    /// identity coercion is trivially a bijection, so the generator
    /// emits it the same way it emits `Into` / `Onto` (Kio' fallback
    /// renders just the inner).
    Iso {
        inner: Box<Term>,
    },
    /// `align!(<inner>)` — payload-preserving surface elaborator form.
    /// Identity is a payload-preserving coercion at any type.
    Align {
        inner: Box<Term>,
    },
    /// `ease!(<inner>)` — no-duplication surface elaborator form, a
    /// strict superset of `iso!` (adds sum widening, sum collapse, and
    /// product projection; excludes product duplication). Identity maps
    /// every source factor to its own slot, so it is admitted at any
    /// type — including function types, where `ease!` short-circuits
    /// to the receiver when source and target are already equal. The
    /// generator emits only monomorphic types, so `ease!`'s
    /// polymorphic-source/target rejection never applies.
    Ease {
        inner: Box<Term>,
    },
    /// `atom!(<inner>)` — single-arm-pick surface elaborator form. For
    /// atomic targets `atom!` collapses to a degenerate identity (the
    /// "single arm" is the source itself). The generator only emits
    /// it at atomic targets so the inner-as-result identity holds.
    Atom {
        inner: Box<Term>,
    },
    /// `reorder_sum!(<inner>)` — sum-axis permutation surface elaborator.
    /// At identity coercion (source == target), the permutation is
    /// the identity permutation, so the Kio' fallback is a
    /// passthrough. The generator emits this only when the source
    /// type's outermost shape is a sum (`Type::Sum`); other shapes
    /// have no sum spine to reorder.
    ReorderSum {
        inner: Box<Term>,
    },
    /// `reorder_prod!(<inner>)` — product-axis permutation surface
    /// elaborator. At identity coercion, the permutation is the identity
    /// permutation; Kio' fallback is a passthrough. The generator
    /// emits this only when the source's outermost shape is a
    /// product (`Type::Product`).
    ReorderProd {
        inner: Box<Term>,
    },
    /// `narrow_sum!(<inner>)` — sum-axis codiagonal / capacity-shrink
    /// surface elaborator. At identity coercion the multiset equality
    /// trivially holds, so the Kio' fallback is a passthrough. The
    /// generator emits this only when the source's outermost shape
    /// is a sum.
    NarrowSum {
        inner: Box<Term>,
    },
    /// `narrow_prod!(<inner>)` — product-axis truncation surface
    /// elaborator. At identity coercion target == source so no slots
    /// are dropped; Kio' fallback passes the inner through. Emitted
    /// only when source's outermost shape is a product.
    NarrowProd {
        inner: Box<Term>,
    },
    /// `widen_sum!(<inner>)` — sum-axis capacity-extend surface
    /// elaborator. At identity coercion no new arms are added; Kio'
    /// fallback passes the inner through. Emitted only when source's
    /// outermost shape is a sum.
    WidenSum {
        inner: Box<Term>,
    },
    /// `widen_prod!(<inner>)` — product-axis diagonal / capacity-
    /// extend surface elaborator. At identity coercion no slots are
    /// duplicated; Kio' fallback passes the inner through. Emitted
    /// only when source's outermost shape is a product.
    WidenProd {
        inner: Box<Term>,
    },
    /// `flatten_sum!(<inner>)` — sum-axis iterative-associativity
    /// surface elaborator. The form identity-short-circuits whenever
    /// the source type already equals the target, whatever its
    /// nesting, so at identity coercion the Kio' fallback is a
    /// passthrough. Emitted only when source's outermost shape is a
    /// sum, matching the other sum-axis forms' gate.
    FlattenSum {
        inner: Box<Term>,
    },
    /// `flatten_prod!(<inner>)` — product-axis iterative-associativity
    /// surface elaborator. Same identity short-circuit as
    /// `flatten_sum!`; Kio' fallback passes the inner through.
    /// Emitted only when source's outermost shape is a product.
    FlattenProd {
        inner: Box<Term>,
    },
    /// `one_sum!(<inner>)` — sum-axis uniform-arm pick surface
    /// elaborator. At identity-coercion-with-atomic-source the source is
    /// the single "arm" and the form is a degenerate identity. The
    /// generator emits it only at atomic sources — the degenerate
    /// identity case, where no arm selection can go wrong.
    OneSum {
        inner: Box<Term>,
    },
    /// `one_prod!(<inner>)` — product-axis first-matching-slot pick
    /// surface elaborator. At identity-coercion-with-atomic-source the
    /// source itself fills the "one slot" and the form is a degenerate
    /// identity. The generator emits it only at atomic sources —
    /// the degenerate identity case, where no slot selection can go
    /// wrong. Renders `one_prod!(<inner>)` in surface mode; Kio'
    /// fallback is the inner.
    OneProd {
        inner: Box<Term>,
    },
    /// `fit!(<inner>)` — recursive composer surface elaborator. At
    /// identity coercion fit! reduces to identity at every leaf (the
    /// variance walk on `->` and the spine walk on structural types
    /// both bottom out to identity), so the Kio' fallback is a
    /// passthrough. The generator emits this at any target type —
    /// fit! is the most general of the spine palette.
    Fit {
        inner: Box<Term>,
    },
    /// UFCS wrapper around a call. Surface mode renders as
    /// `receiver.>callee_name` (when `rest_args` is empty) or
    /// `receiver.>callee_name(rest_args...)` (when there are extra
    /// value-args). Kio' mode falls back to the underlying call
    /// shape: `callee_name(<type_args>, receiver, rest_args...)`
    /// for a `PolyCall`-style callee, or `callee_name(receiver,
    /// rest_args...)` for an `App`-style callee. `callee_kind`
    /// records which shape to emit.
    Ufcs {
        receiver: Box<Term>,
        callee_name: String,
        rest_args: Vec<Term>,
        callee_kind: UfcsKind,
    },
    /// Placeholder-lambda `.arg. { body }`. The body references each
    /// param via `Term::Placeholder { index }`. Surface mode renders
    /// as `.arg. { body }`; Kio' fallback renders as a regular
    /// `.(p1, ..., pN) { body }` lambda (the same shape the surface
    /// desugar pass produces, per `specs/language.md` § Placeholder
    /// lambdas).
    FnPlaceholder {
        params: Vec<(String, Type)>,
        ret: Type,
        body: Box<Term>,
    },
    /// Placeholder reference inside a `FnPlaceholder` body. `index`
    /// is the 1-based position of the param the placeholder stands
    /// for; surface mode renders as `arg<index>`, Kio' mode renders
    /// the enclosing `FnPlaceholder`'s `<index>`th param name.
    /// `fallback_name` is the param name stored at construction
    /// time so the renderer doesn't have to thread the enclosing
    /// param list down through nested expressions.
    Placeholder {
        index: u32,
        fallback_name: String,
        ty: Type,
    },
    /// Reference to a literal alias declared in the generated root module.
    /// Surface mode renders as an annotated alias use (`name(Type)`);
    /// Kio' mode renders as the raw annotated literal.
    LiteralAliasRef {
        name: String,
        literal: Box<Term>,
    },
    /// User-operator call: a binary op `lhs <op-tokens> rhs`
    /// declared via `op _ <op-tokens> _ { impl callee; };`. Surface mode
    /// renders as `lhs <op-tokens> rhs`; Kio' mode renders the
    /// underlying call `callee(lhs, rhs)`.
    OpCall {
        op_tokens: String,
        callee: String,
        lhs: Box<Term>,
        rhs: Box<Term>,
    },
    /// User-defined elaborator call generated alongside a matching
    /// `pub elab` declaration. Surface mode renders as
    /// `<name>!(<inner>, _)`; Kio' mode falls back to `<inner>`.
    /// Generated definitions are identity-style elaborators, so the
    /// result type is the inner type at every emitted call site.
    UserElaborator {
        name: String,
        inner: Box<Term>,
    },
}

/// Kind of callee for a UFCS receiver-first call. Decides the Kio'
/// fallback rendering — a polymorphic intrinsic with `type_args`
/// vs. a plain `Var`-headed application — without re-deriving the
/// shape from the underlying term.
#[derive(Debug, Clone)]
pub enum UfcsKind {
    /// `callee_name(type_args, receiver, rest_args...)` — the
    /// fallback shape for a polymorphic helper or intrinsic.
    PolyCall { type_args: Vec<Type> },
    /// `callee_name(receiver, rest_args...)` — the fallback shape
    /// for an env-bound free function reached via `Var`.
    App,
}

/// Safe reflected-ABI implementation template for a generated
/// user-defined elaborator. Each template preserves the checked
/// value's type at the call sites the generator emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UserElaboratorTemplate {
    Direct,
    TargetBranch,
    SourceTargetGuard,
    TermTypeGuard,
    LetIdentity,
    PairRoundTrip,
    SumRoundTrip,
}

impl UserElaboratorTemplate {
    pub fn all() -> &'static [UserElaboratorTemplate] {
        use UserElaboratorTemplate::*;
        &[
            Direct,
            TargetBranch,
            SourceTargetGuard,
            TermTypeGuard,
            LetIdentity,
            PairRoundTrip,
            SumRoundTrip,
        ]
    }
}

/// The schedule and body family of a generated user elaborator.
/// Keeping the marked schedule in the same enum as its fixed body prevents
/// the generator from pairing `impl(fills)` with a late-only reflected ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UserElaboratorImplementation {
    Late(UserElaboratorTemplate),
    FillsIdentity,
}

/// Optional capture prefix for a generated user-defined elaborator.
/// Captures exercise the declaration ABI without changing the
/// generated term's runtime meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UserElaboratorCapture {
    TypeAlias,
    ValueFn,
}

/// A generated user-defined elaborator declaration and the
/// implementation body shape it should render with.
#[derive(Debug, Clone)]
pub struct UserElaboratorSpec {
    pub name: String,
    pub impl_name: String,
    pub source_param: String,
    pub target_param: String,
    pub implementation: UserElaboratorImplementation,
    pub captures: Vec<UserElaboratorCapture>,
}

/// The fn the generator wraps each program's body in.
#[derive(Debug, Clone)]
pub struct Program {
    pub fn_def_name: String,
    pub params: Vec<(String, Type)>,
    pub ret: Type,
    pub body: Term,
    pub user_elaborators: Vec<UserElaboratorSpec>,
    /// True when the rendered program uses any surface form beyond
    /// Kio' (tuple-literal sugar, `if!` blocks, `labels`,
    /// label-value sugar, `match!`, `into!` / `onto!`). This tracks
    /// the generated function body's surface usage; whole generated
    /// cases are not Kio' while prog.kio carries the host
    /// mirror needed by current kio-rs.
    pub uses_surface: bool,
    /// True iff the program was generated with surface mode on (so
    /// it may reference generated label types from `prog.util` and may emit
    /// `Term::LabelConstruct` / `Type::Label`). `emit.rs` uses this to
    /// include the package-level declarations and imports that support
    /// surface programs, including generated user elaborators.
    /// `surface_mode` may be true while `uses_surface` is false:
    /// surface_mode is the *generator-time* decision, while
    /// `uses_surface` is the *render-time* signal that a surface
    /// form will actually fire in the generated function body.
    pub surface_mode: bool,
}
