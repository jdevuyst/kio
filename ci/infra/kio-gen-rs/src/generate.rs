//! Type-directed term generation.
//!
//! Each program is `fn gen(p0: T0, ..., pN: Tn) -> R { <body> }`
//! where every parameter type and the return type are picked
//! independently from the full Kio' type universe (bases, function
//! types, product types `A & B`, sum types `A | B`, and `Unit`).
//! Inside the body, `gen_at(target, env, depth)` produces a term of
//! the given target type by combining always-applicable productions
//! (variable lookup, `let`-bindings, the `id` polymorphic helper,
//! `__if_then_else__`) with type-specific introductions
//! (`__pair__` for products, `__left__`/`__right__` for
//! sums, lambdas for function types) and eliminator-shaped
//! productions (`__fst__`, `__snd__`, `__either__`) gated on enough
//! depth to recurse safely.

use rand::Rng;
use rand::SeedableRng;
use rand::seq::{IteratorRandom, SliceRandom};
use rand_chacha::ChaCha20Rng;

use kio_gen::ast::{
    Base, Program, Term, Type, UfcsKind, UserElaboratorCapture, UserElaboratorImplementation,
    UserElaboratorSpec, UserElaboratorTemplate,
};
use kio_gen::render::term_uses_surface_forms;

const SAMPLE_STRINGS: &[&str] = &["", "hello", "kio", "world", "foo", "bar", "baz", "x"];

/// Reserved / keyword identifiers from popular languages that the
/// generator deliberately picks for binding names every once in a
/// while. The point is to exercise kio-rs's parser, typechecker, and
/// (eventually) JS-codegen mangling on identifiers that collide with
/// other languages' reserved words. JS is the live backend today —
/// the kio-rs JS-emit path already mangles colliding identifiers
/// (per `bootstrap.md` Open questions), so we want to keep it
/// honest. Names are filtered to fit kio-rs's identifier rule
/// (`[a-z_][a-z0-9_]*`) and to avoid Kio contextual words the
/// parser routes specially at binding positions.
const RESERVED_NAMES: &[&str] = &[
    // JavaScript / ECMAScript
    "class",
    "default",
    "return",
    "async",
    "await",
    "function",
    "new",
    "delete",
    "typeof",
    "var",
    "const",
    "extends",
    "super",
    "this",
    "void",
    "instanceof",
    "import",
    "export",
    "arguments",
    "eval",
    "yield",
    // Python
    "def",
    "lambda",
    "pass",
    "print",
    "with",
    "assert",
    "raise",
    "global",
    "nonlocal",
    // Java / C# / Kotlin
    "interface",
    "implements",
    "static",
    "final",
    "volatile",
    "synchronized",
    "abstract",
    "package",
    "enum",
    "record",
    "sealed",
    "permits",
    // C / C++
    "auto",
    "register",
    "extern",
    "inline",
    "signed",
    "unsigned",
    "union",
    "goto",
    "sizeof",
    "template",
    "typename",
    "namespace",
    "operator",
    "friend",
    "virtual",
    "nullptr",
    // Rust (excluding kio-rs contextual keywords)
    "move",
    "mut",
    "dyn",
    "impl",
    "unsafe",
    "where",
    "box",
    "loop",
    "trait",
    // Go
    "chan",
    "defer",
    "go",
    "select",
    "range",
    "map",
    "func",
    "struct",
    // Swift / Objective-C
    "guard",
    "inout",
    "init",
    "deinit",
    "subscript",
    "convenience",
    "weak",
    "unowned",
];

/// True iff `a` and `b` work as the two scrutinee-arm types of a
/// generated `match!` whose clauses are `.(p: a) { … }` and
/// `.(q: b) { … }` *without one clause covering both DNF
/// branches*. Per spec § Pattern matching, dispatch is per-branch
/// via `onto!`, so we need the first clause's parameter type to
/// not cover the second branch (and vice versa). For the simple
/// types this generator emits (Base / Unit / Nominal / Label / single-
/// param Box), `onto!`'s only ways to match a non-equal target
/// are: (1) two equal types (so `a == b` is bad), and (2) `()`
/// absorbing every product (so `a` being `Unit` makes the first
/// clause a catch-all). Outside that, two simple non-equal
/// non-Unit types are mutually `onto!`-distinct, so each clause
/// covers exactly its own DNF branch.
///
/// `b` is allowed to be `Unit` — a catch-all *second* clause would
/// still cover the second DNF branch (the first already took the
/// first branch).
fn match_disjoint(a: &Type, b: &Type) -> bool {
    a != b && !matches!(a, Type::Unit)
}

/// Sample two scrutinee-arm types `(a, b)` for the `match!`
/// production that satisfy `match_disjoint(a, b)`. Returns `None`
/// after a few unsuccessful tries; the caller falls back to the
/// homogeneous-arm `__either__` production (which has no DNF
/// dispatch and so isn't constrained by `match_disjoint`).
fn pick_match_disjoint_pair<R: Rng>(rng: &mut R, surface_mode: bool) -> Option<(Type, Type)> {
    for _ in 0..10 {
        let a = arbitrary_simple_type(rng, surface_mode);
        let b = arbitrary_simple_type(rng, surface_mode);
        if match_disjoint(&a, &b) {
            return Some((a, b));
        }
    }
    None
}

/// Pick a binding name that's *occasionally* drawn from
/// `RESERVED_NAMES` instead of the generator's usual numbered
/// scheme — so the cross-implementation oracle catches identifier
/// hazards (notably JS-codegen mangling) on every batch. Probability
/// is deliberately low so the corpus stays mostly readable.
pub fn arbitrary_value_name<R: Rng>(rng: &mut R, default: &str) -> String {
    if rng.gen_bool(0.04) {
        return RESERVED_NAMES.choose(rng).unwrap().to_string();
    }
    default.to_string()
}
const TYPE_DEPTH: u32 = 2;
const BODY_DEPTH: u32 = 5;

/// Knobs that tune what the generator emits. Independent of the seed
/// (the seed governs *which* program comes out; opts govern *what
/// surface* the program is allowed to use). Default is "no
/// restrictions" — full Kio.
#[derive(Debug, Clone, Default)]
pub struct GenOpts {
    /// When true, restrict emission to Kio'. Surface forms (tuple
    /// literal sugar, `if!` blocks, `labels`, label-value sugar,
    /// `match!`, the algebraic elaborators `iso!` / `into!` / `onto!` /
    /// `align!` / `atom!`, and the spine palette `reorder_sum!` /
    /// `reorder_prod!` / `narrow_sum!` / `narrow_prod!` /
    /// `widen_sum!` / `widen_prod!` / `one_sum!` / `one_prod!` /
    /// `fit!`) are skipped.
    pub prime_only: bool,
}

pub fn program_from_seed(seed: u64) -> Program {
    program_from_seed_opts(seed, &GenOpts::default())
}

pub fn program_from_seed_opts(seed: u64, opts: &GenOpts) -> Program {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    // Surface-mode is decided up front so it gates type-pool
    // membership: Label types are only available when surface_mode is
    // on (in --prime-only the pool stays Kio'-only). Body and
    // signature generation use the gated pool, so a Kio'-only
    // program never references Greet.mk / Greet.get / Greet — and
    // emit.rs can omit generated labels and label-type helpers for those
    // cases, keeping the body-level Kio' render signal honest corpus-wide.
    let surface_mode = !opts.prime_only && rng.gen_bool(0.5);

    let ret = arbitrary_type(&mut rng, TYPE_DEPTH, surface_mode);
    let arity = rng.gen_range(0u32..=3);
    let params: Vec<(String, Type)> = (0..arity)
        .map(|i| {
            (
                format!("p{i}"),
                arbitrary_type(&mut rng, TYPE_DEPTH, surface_mode),
            )
        })
        .collect();
    // Generated newtype wrappers sit in the typing environment from
    // the start; the body's existing env_fn / target_vars productions
    // pick them up like any other in-scope function.
    let mut env = wrapper_env(surface_mode);
    env.extend(params.iter().cloned());
    // The fn body is a check position (return-type pinned), so it's
    // safe to occasionally wrap in any of the surface elaborator forms
    // (algebraic or spine; see `maybe_wrap_elaborator`).
    let user_elaborators = if surface_mode {
        let mut user_elaborator_rng = ChaCha20Rng::seed_from_u64(seed ^ 0xE1AB_0A71_0DEC_0DED);
        arbitrary_user_elaborators(&mut user_elaborator_rng)
    } else {
        Vec::new()
    };
    let mut user_elaborator_call_rng = ChaCha20Rng::seed_from_u64(seed ^ 0xCA11_E1AB_0A71_51DE);
    let body_inner = gen_at(
        &mut rng,
        &mut user_elaborator_call_rng,
        &ret,
        &env,
        BODY_DEPTH,
        surface_mode,
        &user_elaborators,
    );
    let body = maybe_wrap_check_position(
        &mut rng,
        &mut user_elaborator_call_rng,
        body_inner,
        &ret,
        surface_mode,
        &user_elaborators,
    );

    // `uses_surface` is true iff the rendered output actually
    // exercises a surface form. Surface forms today live exclusively
    // in the body — generated label types render nominally in type
    // position (see `render::render_type_opts`), so signature-position
    // `Type::Label` doesn't bump the flag. A surface_mode program
    // whose body happens to have no surface terms still renders to
    // pure Kio' source at the body level.
    let uses_surface = surface_mode && term_uses_surface_forms(&body);

    Program {
        fn_def_name: "gen".to_string(),
        params,
        ret,
        body,
        user_elaborators,
        uses_surface,
        surface_mode,
    }
}

/// Initial typing environment populated by generated newtype
/// wrappers. Public so `tests/selfcheck.rs` can pre-load the same
/// environment when it re-walks a generated program. Generated label
/// members (`Greet.mk`/`Greet.get`/`Count.mk`/`Count.get`) are gated on
/// `surface_mode` — Kio'-only programs don't see them, matching
/// emit.rs's conditional generated labels.
pub fn wrapper_env(surface_mode: bool) -> Vec<(String, Type)> {
    let mut env = Vec::new();
    for nominal in nominal_catalog() {
        let Type::Nominal {
            payload,
            constructor,
            projector,
            ..
        } = &nominal
        else {
            continue;
        };
        env.push((
            constructor.clone(),
            Type::Fun(vec![(**payload).clone()], Box::new(nominal.clone())),
        ));
        env.push((
            projector.clone(),
            Type::Fun(vec![nominal.clone()], Box::new((**payload).clone())),
        ));
    }
    if surface_mode {
        for label_ty in label_catalog() {
            let Type::Label {
                payload,
                constructor,
                projector,
                ..
            } = &label_ty
            else {
                continue;
            };
            // `Greet.mk(payload) -> Greet` and `Greet.get(Greet) ->
            // payload`, both emitted as generated newtype members by
            // label_elab.
            env.push((
                constructor.clone(),
                Type::Fun(vec![(**payload).clone()], Box::new(label_ty.clone())),
            ));
            env.push((
                projector.clone(),
                Type::Fun(vec![label_ty.clone()], Box::new((**payload).clone())),
            ));
        }
        // `pub fn kio_gen_op_pick(x: I32, y: I32) -> I32` — the
        // monomorphic 2-arg binding target for the `<+>` user-op
        // declared in the generated root module.
        env.push((
            "kio_gen_op_pick".to_string(),
            Type::Fun(vec![Type::i32(), Type::i32()], Box::new(Type::i32())),
        ));
    }
    env
}

/// Pick a simple Kio' type — Base, Unit, a nullary nominal from
/// the catalog, or a `Box(simple)` instantiation of the parametric
/// newtype in the generated root module. These are the types that can appear as type
/// arguments at a polymorphic call site: kio-rs's parser routes exact
/// type-name idents to a type expression, and a parametric type written
/// `Box(I32)` parses cleanly because its leading token is also type-shaped.
fn arbitrary_simple_type<R: Rng>(rng: &mut R, surface_mode: bool) -> Type {
    arbitrary_simple_type_at_depth(rng, 1, surface_mode)
}

/// Inner picker with a depth budget so parametric types don't nest
/// without bound (`Box(Box(Box(…)))`). Label types are only available
/// when `surface_mode` is true — Kio'-only programs never see a
/// `Type::Label` because emit.rs omits generated labels.
fn arbitrary_simple_type_at_depth<R: Rng>(rng: &mut R, depth: u32, surface_mode: bool) -> Type {
    let upper = if surface_mode { 14 } else { 12 };
    let r = rng.gen_range(0..upper);
    match r {
        0..=6 => Type::Base(*Base::all().choose(rng).unwrap()),
        7 => Type::Unit,
        8 | 9 => {
            let nominals = nominal_catalog();
            nominals.choose(rng).unwrap().clone()
        }
        10 | 11 if surface_mode => {
            // Label-generated nominal types from the surface-mode
            // catalog. They render as generated type names.
            let labels = label_catalog();
            labels.choose(rng).unwrap().clone()
        }
        _ => {
            // Parametric `Box(X)` where X is also simple. Bound the
            // recursion so we don't wander into Box(Box(Box(…))).
            let inner = if depth == 0 {
                Type::Base(*Base::all().choose(rng).unwrap())
            } else {
                arbitrary_simple_type_at_depth(rng, depth - 1, surface_mode)
            };
            box_param(inner)
        }
    }
}

/// The fixed nullary newtypes the generator can use, paired with the
/// flat wrapper-function names declared in the generated root module. The
/// wrappers exist because kio-rs doesn't yet support cross-module
/// dotted-path access; the wrapper's name fills the `constructor` /
/// `projector` slot of `Type::Nominal` and is used as the callee in
/// generated `Term::App`s.
pub fn nominal_catalog() -> Vec<Type> {
    vec![
        Type::Nominal {
            name: "I32_box".to_string(),
            payload: Box::new(Type::i32()),
            constructor: "box_i32".to_string(),
            projector: "unbox_i32".to_string(),
        },
        Type::Nominal {
            name: "Strbox".to_string(),
            payload: Box::new(Type::str()),
            constructor: "box_str".to_string(),
            projector: "unbox_str".to_string(),
        },
        Type::Nominal {
            name: "Boolbox".to_string(),
            payload: Box::new(Type::bool()),
            constructor: "box_bool".to_string(),
            projector: "unbox_bool".to_string(),
        },
    ]
}

/// The fixed label-generated nominal types the generator can use.
/// Each label is declared in `prog.kio` via `pub labels { … };`.
/// The minted newtype name is the surface label spelling with the
/// first letter capitalized (`greet` -> `Greet`), per the kio-rs
/// `label_elab` pass. Construction and projection go through the
/// generated `mk` / `get` members.
pub fn label_catalog() -> Vec<Type> {
    vec![
        Type::Label {
            label: "greet".to_string(),
            newtype_name: mint_label_newtype_name("greet"),
            payload: Box::new(Type::str()),
            constructor: "Greet.mk".to_string(),
            projector: "Greet.get".to_string(),
        },
        Type::Label {
            label: "count".to_string(),
            newtype_name: mint_label_newtype_name("count"),
            payload: Box::new(Type::i32()),
            constructor: "Count.mk".to_string(),
            projector: "Count.get".to_string(),
        },
    ]
}

fn arbitrary_user_elaborators<R: Rng>(rng: &mut R) -> Vec<UserElaboratorSpec> {
    let count = rng.gen_range(1usize..=3);
    let mut specs: Vec<_> = (0..count)
        .map(|i| {
            let captures = match rng.gen_range(0..4) {
                0 => Vec::new(),
                1 => vec![UserElaboratorCapture::TypeAlias],
                2 => vec![UserElaboratorCapture::ValueFn],
                _ => vec![
                    UserElaboratorCapture::TypeAlias,
                    UserElaboratorCapture::ValueFn,
                ],
            };
            UserElaboratorSpec {
                name: format!("kio_gen_elab{i}"),
                impl_name: format!("kio_gen_elab{i}_impl"),
                source_param: format!("Kio_gen_source{i}"),
                target_param: format!("Kio_gen_target{i}"),
                implementation: UserElaboratorImplementation::Late(
                    *UserElaboratorTemplate::all().choose(rng).unwrap(),
                ),
                captures,
            }
        })
        .collect();
    specs.push(UserElaboratorSpec {
        name: "kio_gen_fills".to_owned(),
        impl_name: "kio_gen_fills_impl".to_owned(),
        source_param: "Kio_gen_fills_source".to_owned(),
        target_param: "Kio_gen_fills_target".to_owned(),
        implementation: UserElaboratorImplementation::FillsIdentity,
        captures: Vec::new(),
    });
    specs
}

/// Mirror of kio-rs's `label_elab::mint_label_newtype_name`: capitalize
/// the first letter of the surface label spelling. Kept in sync with
/// the implementation by spec contract; tested against the live
/// kio-rs binary by the generator self-tests.
fn mint_label_newtype_name(label: &str) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Pick an arbitrary Kio type at outer position. Composite types
/// (Fun / Product / Sum) wrap simple components only, which keeps
/// call-site type args simple by construction.
fn arbitrary_type<R: Rng>(rng: &mut R, _depth: u32, surface_mode: bool) -> Type {
    if rng.gen_bool(0.55) {
        arbitrary_simple_type(rng, surface_mode)
    } else {
        match rng.gen_range(0u32..3) {
            0 => {
                let arity = rng.gen_range(1u32..=2);
                let args = (0..arity)
                    .map(|_| arbitrary_simple_type(rng, surface_mode))
                    .collect();
                let ret = Box::new(arbitrary_simple_type(rng, surface_mode));
                Type::Fun(args, ret)
            }
            1 => Type::Product(
                Box::new(arbitrary_simple_type(rng, surface_mode)),
                Box::new(arbitrary_simple_type(rng, surface_mode)),
            ),
            _ => Type::Sum(
                Box::new(arbitrary_simple_type(rng, surface_mode)),
                Box::new(arbitrary_simple_type(rng, surface_mode)),
            ),
        }
    }
}

fn is_simple_type(t: &Type) -> bool {
    matches!(
        t,
        Type::Base(_) | Type::Unit | Type::Nominal { .. } | Type::Param { .. } | Type::Label { .. }
    )
}

/// The parametric Box newtype, instantiated at `payload`.
fn box_param(payload: Type) -> Type {
    Type::Param {
        name: "Box".to_string(),
        payload: Box::new(payload),
        constructor: "box_any".to_string(),
        projector: "unbox_any".to_string(),
    }
}

/// Generate a literal of the given base. Integer ranges stay well
/// inside each base's representable range so the typechecker doesn't
/// reject the literal as out-of-range. Floats render as
/// `<digits>.<digits>` so they lex as float literals without
/// exponent-form complexity; the rendered form pins the host
/// type with an explicit `(Type)` annotation.
///
/// When `surface_mode` is on and the base has a matching literal
/// alias declared in the generated root module, the generator occasionally substitutes
/// an annotated alias use instead of the raw literal. The Kio' fallback
/// expands to the bound literal shape.
fn base_literal_at<R: Rng>(rng: &mut R, b: Base, surface_mode: bool) -> Term {
    let raw = base_literal(rng, b);
    if !surface_mode {
        return raw;
    }
    let (name, literal) = match b {
        Base::I32 if rng.gen_bool(0.15) => (
            "zero_i32",
            Term::NumLit {
                base: Base::I32,
                repr: "0".to_string(),
            },
        ),
        Base::Str if rng.gen_bool(0.15) => ("hello_str", Term::StrLit("hello".to_string())),
        _ => return raw,
    };
    Term::LiteralAliasRef {
        name: name.to_string(),
        literal: Box::new(literal),
    }
}

fn base_literal<R: Rng>(rng: &mut R, b: Base) -> Term {
    use Base::*;
    match b {
        Str => Term::StrLit(SAMPLE_STRINGS.choose(rng).unwrap().to_string()),
        Bool => Term::BoolLit(rng.gen_bool(0.5)),
        I8 | U8 => Term::NumLit {
            base: b,
            repr: rng.gen_range(0u32..=100).to_string(),
        },
        I16 | U16 => Term::NumLit {
            base: b,
            repr: rng.gen_range(0u32..=10_000).to_string(),
        },
        I32 | U32 => Term::NumLit {
            base: b,
            repr: rng.gen_range(0u64..=1_000_000).to_string(),
        },
        I64 | U64 => Term::NumLit {
            base: b,
            repr: rng.gen_range(0u64..=1_000_000_000_000).to_string(),
        },
        I128 | U128 => {
            // Stay inside both i128 and u128 for safety; large enough
            // to drive BigInt-backed codegen on the runtime side.
            Term::NumLit {
                base: b,
                repr: rng.gen_range(0u128..=10u128.pow(18)).to_string(),
            }
        }
        F32 | F64 => Term::NumLit {
            base: b,
            repr: format!(
                "{}.{}",
                rng.gen_range(0u32..=1_000),
                rng.gen_range(0u32..=999)
            ),
        },
    }
}

/// Maybe rewrap a freshly-generated `PolyCall` or `App` as a UFCS
/// receiver-first call (per `specs/language.md` § UFCS). The
/// receiver is the first value-arg of the underlying call; type
/// arguments stay on the side channel because the surface form
/// doesn't accept user-supplied type-args (the typer backsolves
/// them from the receiver and the remaining value-args). Only
/// fires in surface mode and only on calls with at least one
/// value-arg.
///
/// kio-rs's UFCS resolution requires the receiver's monotype to
/// equal the callee's first value-parameter type. The generator
/// only emits well-typed calls, so a UFCS rewrap is safe: the
/// receiver already has the right type by construction.
fn maybe_wrap_ufcs<R: Rng>(rng: &mut R, term: Term, surface_mode: bool) -> Term {
    if !surface_mode || !rng.gen_bool(0.2) {
        return term;
    }
    match term {
        Term::PolyCall {
            name,
            type_args,
            args,
        } if !args.is_empty() => {
            let mut iter = args.into_iter();
            let receiver = iter.next().unwrap();
            let rest_args: Vec<Term> = iter.collect();
            Term::Ufcs {
                receiver: Box::new(receiver),
                callee_name: name,
                rest_args,
                callee_kind: UfcsKind::PolyCall { type_args },
            }
        }
        Term::App(f, args) if !args.is_empty() => {
            let name = match *f {
                Term::Var(n) => n,
                other => {
                    // Non-`Var` callee can't be UFCS'd at the surface
                    // (the spec requires a path-shaped callee). Put
                    // the term back together and return unchanged.
                    return Term::App(Box::new(other), args);
                }
            };
            let mut iter = args.into_iter();
            let receiver = iter.next().unwrap();
            let rest_args: Vec<Term> = iter.collect();
            Term::Ufcs {
                receiver: Box::new(receiver),
                callee_name: name,
                rest_args,
                callee_kind: UfcsKind::App,
            }
        }
        other => other,
    }
}

/// Maybe wrap `inner` in one of the 17 surface elaborator forms — the
/// 9-form spine palette (`reorder_sum!`, `reorder_prod!`,
/// `narrow_sum!`, `narrow_prod!`, `widen_sum!`, `widen_prod!`,
/// `one_sum!`, `one_prod!`, `fit!`) or the 5-form algebraic palette
/// (`iso!`, `into!`, `onto!`, `align!`, `atom!`).
///
/// Spine forms are the everyday palette (per `specs/language.md`
/// § Spine-palette elaborators) and dominate the wrap choice; algebraic
/// forms keep a low non-zero share to exercise the algebraic
/// codepath without skewing coverage. Today's split: spine ~80%,
/// algebraic ~20% (within the 10% of expressions that get wrapped
/// at all). When neither subset has any per-form applicability at
/// the inner type (`inner_ty`'s shape rules out every form on that
/// side), the routine falls through to no wrap; this can happen on
/// composite non-simple non-sum non-product types when the algebraic
/// subset is rolled.
///
/// Only fires in surface mode and only as identity coercions, so
/// the Kio' fallback (passthrough) is sound for every form — the
/// inner expression's type *is* the result type, and each elaborator
/// admits identity at every applicable position. Callers must
/// invoke this at *check positions* — kio-rs's typer routes the
/// elaborator call through `check_value_against` and rejects them in
/// synth-only positions ("appears in synth-only position here")
/// with exit 14. In practice that means the value-arg slots of
/// intrinsic `PolyCall`s (where the type-arg pins the expected
/// type) and the fn body (return-type position) — the let body /
/// let RHS / `__either__` scrutinee / `__fst__` / `__snd__`
/// scrutinee are synth positions and must NOT be wrapped here.
///
/// **Per-form applicability** (filtered by `inner_ty`'s outermost
/// shape):
///
/// Spine subset:
/// - `reorder_sum!` / `narrow_sum!` / `widen_sum!` / `flatten_sum!`:
///   source must be a top-level `Type::Sum`. At identity, the spine
///   length is the same on both sides and the rule reduces to
///   identity (`flatten_sum!` identity-short-circuits whenever the
///   source type already equals the target).
/// - `reorder_prod!` / `narrow_prod!` / `widen_prod!` /
///   `flatten_prod!`: source must be a top-level `Type::Product`.
///   Same identity reduction.
/// - `one_sum!`: emitted only at atomic sources — the degenerate
///   identity case, where source itself is the "single arm".
/// - `one_prod!`: emitted only at atomic sources — the degenerate
///   identity case, where source itself fills the "one slot".
/// - `fit!`: source may be any type — `fit!`'s spine walk reduces
///   to identity at every leaf when source == target.
///
/// Algebraic subset:
/// - `atom!`: source must be atomic (simple) — its target must
///   DNF-normalize to a single arm; only atomic sources qualify as
///   degenerate identity.
/// - `iso!`, `into!`, `onto!`, `align!`, `ease!`: identity is
///   admitted at every type (generated types are monomorphic, so
///   `ease!`'s polymorphic-source/target rejection never applies).
fn maybe_wrap_elaborator<R: Rng>(
    rng: &mut R,
    inner: Term,
    inner_ty: &Type,
    surface_mode: bool,
) -> Term {
    if !surface_mode || !rng.gen_bool(0.5) {
        return inner;
    }
    // 80/20 split: prefer spine; keep algebraic at a low rate to
    // exercise the codepath. The wrap rate (50%) compensates for
    // the larger 17-form palette — without it, the per-form share
    // drops below the distribution-test floor for forms with tight
    // applicability gates (`atom!` requires simple source;
    // `narrow_sum!` / `widen_prod!` require the matching axis).
    //
    // When the rolled subset has no applicable forms at this type,
    // fall through to no wrap (the inner is returned unchanged) —
    // this is rare in practice because `fit!` (spine) and
    // `iso!`/`into!`/`onto!`/`align!` (algebraic) all admit
    // identity at every type.
    if rng.gen_bool(0.8) {
        wrap_spine(rng, inner, inner_ty)
    } else {
        wrap_algebraic(rng, inner, inner_ty)
    }
}

fn maybe_wrap_user_elaborator<R: Rng>(
    rng: &mut R,
    inner: Term,
    surface_mode: bool,
    user_elaborators: &[UserElaboratorSpec],
) -> Term {
    if !surface_mode || user_elaborators.is_empty() || !rng.gen_bool(0.25) {
        return inner;
    }
    let Some(spec) = user_elaborators
        .iter()
        .filter(|spec| matches!(spec.implementation, UserElaboratorImplementation::Late(_)))
        .choose(rng)
    else {
        return inner;
    };
    Term::UserElaborator {
        name: spec.name.clone(),
        inner: Box::new(inner),
    }
}

fn maybe_wrap_check_position<R: Rng>(
    rng: &mut R,
    user_rng: &mut R,
    inner: Term,
    inner_ty: &Type,
    surface_mode: bool,
    user_elaborators: &[UserElaboratorSpec],
) -> Term {
    let inner = maybe_wrap_elaborator(rng, inner, inner_ty, surface_mode);
    if is_builtin_elaborator_call(&inner) {
        inner
    } else {
        maybe_wrap_user_elaborator(user_rng, inner, surface_mode, user_elaborators)
    }
}

fn is_builtin_elaborator_call(term: &Term) -> bool {
    matches!(
        term,
        Term::Into { .. }
            | Term::Onto { .. }
            | Term::Iso { .. }
            | Term::Align { .. }
            | Term::Ease { .. }
            | Term::Atom { .. }
            | Term::ReorderSum { .. }
            | Term::ReorderProd { .. }
            | Term::NarrowSum { .. }
            | Term::NarrowProd { .. }
            | Term::WidenSum { .. }
            | Term::WidenProd { .. }
            | Term::FlattenSum { .. }
            | Term::FlattenProd { .. }
            | Term::OneSum { .. }
            | Term::OneProd { .. }
            | Term::Fit { .. }
    )
}

/// Pick a spine elaborator form whose per-source applicability gate
/// fires on `inner_ty`. `fit!` always applies (identity reduces to
/// identity at every leaf via the spine walk). `reorder_*` /
/// `narrow_*` / `widen_*` / `flatten_*` are gated on the outermost
/// shape being the matching axis (sum / product); `flatten_*`
/// identity-short-circuits whenever source already equals target, so
/// any nesting is admissible at identity. `one_sum!` / `one_prod!`
/// only fire at atomic sources (the safe degenerate-identity case).
fn wrap_spine<R: Rng>(rng: &mut R, inner: Term, inner_ty: &Type) -> Term {
    let is_sum = matches!(inner_ty, Type::Sum(_, _));
    let is_prod = matches!(inner_ty, Type::Product(_, _));
    let is_atomic = is_simple_type(inner_ty);
    // Per-form weights — `fit!` carries a higher weight as the
    // most general spine form.
    let weights: [u32; 11] = [
        if is_sum { 2 } else { 0 },    // 0: reorder_sum!
        if is_prod { 2 } else { 0 },   // 1: reorder_prod!
        if is_sum { 2 } else { 0 },    // 2: narrow_sum!
        if is_prod { 2 } else { 0 },   // 3: narrow_prod!
        if is_sum { 2 } else { 0 },    // 4: widen_sum!
        if is_prod { 2 } else { 0 },   // 5: widen_prod!
        if is_sum { 2 } else { 0 },    // 6: flatten_sum!
        if is_prod { 2 } else { 0 },   // 7: flatten_prod!
        if is_atomic { 1 } else { 0 }, // 8: one_sum!
        if is_atomic { 1 } else { 0 }, // 9: one_prod!
        4,                             // 10: fit! (every type)
    ];
    let total: u32 = weights.iter().sum();
    if total == 0 {
        return inner;
    }
    let mut pick = rng.gen_range(0..total);
    let mut choice = 0usize;
    for (i, w) in weights.iter().enumerate() {
        if pick < *w {
            choice = i;
            break;
        }
        pick -= *w;
    }
    let boxed = Box::new(inner);
    match choice {
        0 => Term::ReorderSum { inner: boxed },
        1 => Term::ReorderProd { inner: boxed },
        2 => Term::NarrowSum { inner: boxed },
        3 => Term::NarrowProd { inner: boxed },
        4 => Term::WidenSum { inner: boxed },
        5 => Term::WidenProd { inner: boxed },
        6 => Term::FlattenSum { inner: boxed },
        7 => Term::FlattenProd { inner: boxed },
        8 => Term::OneSum { inner: boxed },
        9 => Term::OneProd { inner: boxed },
        _ => Term::Fit { inner: boxed },
    }
}

/// Pick an algebraic elaborator form whose per-source applicability gate
/// fires on `inner_ty`. `iso!` / `into!` / `onto!` / `align!` / `ease!`
/// admit identity at every type (the generator's types are monomorphic,
/// so `ease!`'s polymorphic-source/target rejection never applies);
/// `atom!` is restricted to atomic sources.
fn wrap_algebraic<R: Rng>(rng: &mut R, inner: Term, inner_ty: &Type) -> Term {
    let allow_atom = is_simple_type(inner_ty);
    let upper = if allow_atom { 6 } else { 5 };
    let boxed = Box::new(inner);
    match rng.gen_range(0..upper) {
        0 => Term::Into { inner: boxed },
        1 => Term::Onto { inner: boxed },
        2 => Term::Iso { inner: boxed },
        3 => Term::Align { inner: boxed },
        4 => Term::Ease { inner: boxed },
        _ => Term::Atom { inner: boxed },
    }
}

fn gen_at<R: Rng>(
    rng: &mut R,
    user_rng: &mut R,
    target: &Type,
    env: &[(String, Type)],
    depth: u32,
    surface_mode: bool,
    user_elaborators: &[UserElaboratorSpec],
) -> Term {
    let target_vars: Vec<String> = env
        .iter()
        .filter(|(_, t)| t == target)
        .map(|(n, _)| n.clone())
        .collect();
    let env_fns: Vec<(String, Vec<Type>)> = env
        .iter()
        .filter_map(|(n, t)| match t {
            Type::Fun(args, ret) if &**ret == target => Some((n.clone(), args.clone())),
            _ => None,
        })
        .collect();

    if depth == 0 {
        if !target_vars.is_empty() && rng.gen_bool(0.6) {
            return Term::Var(target_vars.choose(rng).unwrap().clone());
        }
        return canonical(rng, target);
    }

    // `id`, `__if_then_else__`, and the eliminator productions all
    // pass `target` as a type argument, so they're only available
    // when target is simple (Base / Unit / Nominal).
    let target_simple = is_simple_type(target);
    let nominal_projectors: Vec<(Type, String)> = nominal_catalog()
        .into_iter()
        .filter_map(|t| match &t {
            Type::Nominal {
                payload, projector, ..
            } if &**payload == target => Some((t.clone(), projector.clone())),
            _ => None,
        })
        .collect();
    let weights: [u32; 13] = [
        if target_vars.is_empty() { 0 } else { 2 }, // 0: bound var
        2,                                          // 1: let
        if target_simple { 2 } else { 0 },          // 2: id
        if target_simple && depth >= 2 { 2 } else { 0 }, // 3: __if_then_else__
        3,                                          // 4: type-directed introduction
        if env_fns.is_empty() { 0 } else { 3 },     // 5: call host fn
        if target_simple && depth >= 2 { 1 } else { 0 }, // 6: __fst__
        if target_simple && depth >= 2 { 1 } else { 0 }, // 7: __snd__
        // Choice 8: __either__ (target_simple) or MatchBang (target
        // is a `Type::Sum`, exercised by the `match!` rendering).
        // Slice 12.8 relaxed kio-rs's call-arg parsing so a sum-typed
        // type-arg parses fine, so MatchBang's Kio' rendering
        // (`__either__(_, _, (X | Y), …)`) is admissible.
        if (target_simple || matches!(target, Type::Sum(_, _))) && depth >= 2 {
            1
        } else {
            0
        }, // 8: __either__ / match!
        if target_simple { 1 } else { 0 }, // 9: const1
        if target_simple { 1 } else { 0 }, // 10: const2
        if !nominal_projectors.is_empty() && depth >= 2 {
            1
        } else {
            0
        }, // 11: nominal projector
        // 12: surface-only `<+> ` user-op call — only available when
        // the target is `I32` and surface_mode is on (the op binding
        // and the underlying `kio_gen_op_pick` fn live in the generated root module
        // gated on surface mode; `OpCall` is a surface form).
        if surface_mode && *target == Type::Base(Base::I32) && depth >= 1 {
            3
        } else {
            0
        },
    ];
    let total: u32 = weights.iter().sum();
    let mut pick = rng.gen_range(0..total);
    let mut choice = 0usize;
    for (i, w) in weights.iter().enumerate() {
        if pick < *w {
            choice = i;
            break;
        }
        pick -= *w;
    }

    let result = match choice {
        0 => Term::Var(target_vars.choose(rng).unwrap().clone()),
        1 => {
            let n = arbitrary_value_name(rng, &format!("v{}", env.len()));
            let bind_ty = arbitrary_type(rng, 1, surface_mode);
            let rhs = gen_at(
                rng,
                user_rng,
                &bind_ty,
                env,
                depth - 1,
                surface_mode,
                user_elaborators,
            );
            let mut e2 = env.to_vec();
            e2.push((n.clone(), bind_ty.clone()));
            Term::Let {
                name: n,
                ty: bind_ty,
                rhs: Box::new(rhs),
                body: Box::new(gen_at(
                    rng,
                    user_rng,
                    target,
                    &e2,
                    depth - 1,
                    surface_mode,
                    user_elaborators,
                )),
            }
        }
        2 => Term::PolyCall {
            name: "id".to_string(),
            type_args: vec![target.clone()],
            args: vec![gen_at(
                rng,
                user_rng,
                target,
                env,
                depth - 1,
                surface_mode,
                user_elaborators,
            )],
        },
        3 => {
            let cond = Term::BoolLit(rng.gen_bool(0.5));
            let then_lam = Term::Lambda {
                params: vec![],
                ret: target.clone(),
                body: Box::new(gen_at(
                    rng,
                    user_rng,
                    target,
                    env,
                    depth - 1,
                    surface_mode,
                    user_elaborators,
                )),
            };
            let else_lam = Term::Lambda {
                params: vec![],
                ret: target.clone(),
                body: Box::new(gen_at(
                    rng,
                    user_rng,
                    target,
                    env,
                    depth - 1,
                    surface_mode,
                    user_elaborators,
                )),
            };
            Term::PolyCall {
                name: "__if_then_else__".to_string(),
                type_args: vec![target.clone()],
                args: vec![cond, then_lam, else_lam],
            }
        }
        4 => intro(
            rng,
            user_rng,
            target,
            env,
            depth,
            surface_mode,
            user_elaborators,
        ),
        5 => {
            let (name, args_ty) = env_fns.choose(rng).unwrap().clone();
            let args: Vec<Term> = args_ty
                .iter()
                .map(|t| {
                    gen_at(
                        rng,
                        user_rng,
                        t,
                        env,
                        depth - 1,
                        surface_mode,
                        user_elaborators,
                    )
                })
                .collect();
            Term::App(Box::new(Term::Var(name)), args)
        }
        6 => {
            // __fst__: target = a; pick b (simple); build (a & b).
            let other = arbitrary_simple_type(rng, surface_mode);
            let pair_ty = Type::Product(Box::new(target.clone()), Box::new(other.clone()));
            Term::PolyCall {
                name: "__fst__".to_string(),
                type_args: vec![target.clone(), other],
                args: vec![gen_at(
                    rng,
                    user_rng,
                    &pair_ty,
                    env,
                    depth - 1,
                    surface_mode,
                    user_elaborators,
                )],
            }
        }
        7 => {
            // __snd__: target = b; pick a (simple); build (a & b).
            let other = arbitrary_simple_type(rng, surface_mode);
            let pair_ty = Type::Product(Box::new(other.clone()), Box::new(target.clone()));
            Term::PolyCall {
                name: "__snd__".to_string(),
                type_args: vec![other, target.clone()],
                args: vec![gen_at(
                    rng,
                    user_rng,
                    &pair_ty,
                    env,
                    depth - 1,
                    surface_mode,
                    user_elaborators,
                )],
            }
        }
        9 => {
            // const1[A][B](x: A, y: B) -> A — target is A; pick B (simple).
            let other = arbitrary_simple_type(rng, surface_mode);
            Term::PolyCall {
                name: "const1".to_string(),
                type_args: vec![target.clone(), other.clone()],
                args: vec![
                    gen_at(
                        rng,
                        user_rng,
                        target,
                        env,
                        depth - 1,
                        surface_mode,
                        user_elaborators,
                    ),
                    gen_at(
                        rng,
                        user_rng,
                        &other,
                        env,
                        depth - 1,
                        surface_mode,
                        user_elaborators,
                    ),
                ],
            }
        }
        10 => {
            // const2[A][B](x: A, y: B) -> B — target is B; pick A (simple).
            let other = arbitrary_simple_type(rng, surface_mode);
            Term::PolyCall {
                name: "const2".to_string(),
                type_args: vec![other.clone(), target.clone()],
                args: vec![
                    gen_at(
                        rng,
                        user_rng,
                        &other,
                        env,
                        depth - 1,
                        surface_mode,
                        user_elaborators,
                    ),
                    gen_at(
                        rng,
                        user_rng,
                        target,
                        env,
                        depth - 1,
                        surface_mode,
                        user_elaborators,
                    ),
                ],
            }
        }
        11 => {
            // Project a nullary nominal whose payload matches target,
            // through the generated wrapper function.
            let (nominal, projector) = nominal_projectors.choose(rng).unwrap().clone();
            Term::App(
                Box::new(Term::Var(projector)),
                vec![gen_at(
                    rng,
                    user_rng,
                    &nominal,
                    env,
                    depth - 1,
                    surface_mode,
                    user_elaborators,
                )],
            )
        }
        12 => {
            // Surface `lhs <+> rhs` user-op call. Target is I32
            // (the underlying `kio_gen_op_pick`'s return type).
            let i32_ty = Type::i32();
            let lhs = gen_at(
                rng,
                user_rng,
                &i32_ty,
                env,
                depth - 1,
                surface_mode,
                user_elaborators,
            );
            let rhs = gen_at(
                rng,
                user_rng,
                &i32_ty,
                env,
                depth - 1,
                surface_mode,
                user_elaborators,
            );
            Term::OpCall {
                op_tokens: "<+>".to_string(),
                callee: "kio_gen_op_pick".to_string(),
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            }
        }
        8 => {
            // Sum eliminator with two-clause structure. When target
            // happens to be a `Type::Sum`, the eliminator can render
            // as the `match!` elaborator form; both clause bodies still
            // produce the same common target type. Otherwise we fall
            // back to the homogeneous-arms `__either__` shape.
            //
            // For the MatchBang path we restrict the scrutinee's two
            // arm types to be **`match!`-disjoint** (see
            // `match_disjoint`) — otherwise the typer would reject
            // the second clause as unreachable: `match!` dispatch
            // is per-DNF-branch, and if the first clause's parameter
            // type covers both DNF branches via `onto!` (e.g.,
            // arms are `(I32 | I32)`, or one is `()` which absorbs
            // anything), the second clause never fires. The fallback
            // `__either__` shape isn't subject to that constraint
            // because its arm dispatch is positional, not
            // `onto!`-driven.
            let l_param = arbitrary_value_name(rng, &format!("l{}", env.len()));
            let r_param = arbitrary_value_name(rng, &format!("r{}", env.len()));

            let want_match_bang = matches!(target, Type::Sum(_, _)) && rng.gen_bool(0.5);
            let pair = if want_match_bang {
                pick_match_disjoint_pair(rng, surface_mode)
            } else {
                None
            };

            if let (Type::Sum(_, _), Some((a, b))) = (target, pair) {
                // MatchBang path: the scrutinee's two arms are
                // match-disjoint so neither clause ends up
                // unreachable. Both clause bodies produce the common
                // target type.
                let sum_ty = Type::Sum(Box::new(a.clone()), Box::new(b.clone()));
                let sum_term = gen_at(
                    rng,
                    user_rng,
                    &sum_ty,
                    env,
                    depth - 1,
                    surface_mode,
                    user_elaborators,
                );
                let mut l_env = env.to_vec();
                l_env.push((l_param.clone(), a.clone()));
                let l_body = gen_at(
                    rng,
                    user_rng,
                    target,
                    &l_env,
                    depth - 1,
                    surface_mode,
                    user_elaborators,
                );
                let mut r_env = env.to_vec();
                r_env.push((r_param.clone(), b.clone()));
                let r_body = gen_at(
                    rng,
                    user_rng,
                    target,
                    &r_env,
                    depth - 1,
                    surface_mode,
                    user_elaborators,
                );
                Term::MatchBang {
                    scrutinee: Box::new(sum_term),
                    left_param: l_param,
                    left_ty: a,
                    left_body: Box::new(l_body),
                    right_param: r_param,
                    right_ty: b,
                    right_body: Box::new(r_body),
                    ret_ty: target.clone(),
                }
            } else {
                let a = arbitrary_simple_type(rng, surface_mode);
                let b = arbitrary_simple_type(rng, surface_mode);
                let sum_ty = Type::Sum(Box::new(a.clone()), Box::new(b.clone()));
                let sum_term = gen_at(
                    rng,
                    user_rng,
                    &sum_ty,
                    env,
                    depth - 1,
                    surface_mode,
                    user_elaborators,
                );
                let mut l_env = env.to_vec();
                l_env.push((l_param.clone(), a.clone()));
                let l_body = gen_at(
                    rng,
                    user_rng,
                    target,
                    &l_env,
                    depth - 1,
                    surface_mode,
                    user_elaborators,
                );
                let mut r_env = env.to_vec();
                r_env.push((r_param.clone(), b.clone()));
                let r_body = gen_at(
                    rng,
                    user_rng,
                    target,
                    &r_env,
                    depth - 1,
                    surface_mode,
                    user_elaborators,
                );
                let l_arm = Term::Lambda {
                    params: vec![(l_param, a.clone())],
                    ret: target.clone(),
                    body: Box::new(l_body),
                };
                let r_arm = Term::Lambda {
                    params: vec![(r_param, b.clone())],
                    ret: target.clone(),
                    body: Box::new(r_body),
                };
                Term::PolyCall {
                    name: "__either__".to_string(),
                    type_args: vec![a, b, target.clone()],
                    args: vec![sum_term, l_arm, r_arm],
                }
            }
        }
        _ => unreachable!(),
    };
    // Surface UFCS rewrap (per `specs/language.md` § UFCS). Gated
    // on `surface_mode`: each call-shaped production occasionally
    // becomes `receiver.>callee(rest)`. `__either__` /
    // `__if_then_else__` calls aren't rewrapped because their first
    // value-arg is a non-receiver-shaped expression (the
    // scrutinee/condition), and the kio-rs UFCS dispatch rule still
    // requires the receiver type to equal the callee's first
    // value-parameter type — which it does for every call this
    // function generates.
    match choice {
        2 | 5 | 6 | 7 | 9 | 10 | 11 => maybe_wrap_ufcs(rng, result, surface_mode),
        _ => result,
    }
}

/// Type-directed introduction: build a term of `target` using the
/// canonical introduction form for that type's shape.
fn intro<R: Rng>(
    rng: &mut R,
    user_rng: &mut R,
    target: &Type,
    env: &[(String, Type)],
    depth: u32,
    surface_mode: bool,
    user_elaborators: &[UserElaboratorSpec],
) -> Term {
    match target {
        Type::Base(b) => base_literal_at(rng, *b, surface_mode),
        Type::Unit => Term::UnitLit,
        Type::Nominal {
            payload,
            constructor,
            ..
        } => Term::App(
            Box::new(Term::Var(constructor.clone())),
            vec![gen_at(
                rng,
                user_rng,
                payload,
                env,
                depth - 1,
                surface_mode,
                user_elaborators,
            )],
        ),
        Type::Param {
            payload,
            constructor,
            ..
        } => Term::PolyCall {
            name: constructor.clone(),
            type_args: vec![(**payload).clone()],
            args: vec![gen_at(
                rng,
                user_rng,
                payload,
                env,
                depth - 1,
                surface_mode,
                user_elaborators,
            )],
        },
        Type::Label {
            label,
            constructor,
            payload,
            ..
        } => Term::LabelConstruct {
            label: label.clone(),
            constructor: constructor.clone(),
            payload: Box::new(gen_at(
                rng,
                user_rng,
                payload,
                env,
                depth - 1,
                surface_mode,
                user_elaborators,
            )),
        },
        Type::Fun(args, ret) => {
            let lam_params: Vec<(String, Type)> = args
                .iter()
                .enumerate()
                .map(|(i, t)| (format!("a{}", env.len() + i), t.clone()))
                .collect();
            // Surface `.arg.` placeholder lambda (per
            // `specs/language.md` § Placeholder lambdas). The
            // arity of `.arg. { ... }` is set by the highest indexed
            // placeholder in the body — `.arg. { argN }` always
            // parses as an N-arg lambda. So we only emit the
            // identity-on-Nth-slot body `.arg. { argN }` when the
            // target function's *last* parameter has the return
            // type — that pins arity = N = arg-count, matching the
            // target arity. Other shapes would need a richer body
            // (placeholders for the discarded params); kept out of
            // scope for the simple production. Kio' mode renders
            // the canonical `.(p1, ..., pN) { pN }` lambda, which
            // is what the surface desugar pass produces.
            let last_param_matches_ret =
                lam_params.last().map(|(_, t)| *t == **ret).unwrap_or(false);
            if surface_mode && !lam_params.is_empty() && last_param_matches_ret {
                let n = lam_params.len();
                let (name, ty) = lam_params[n - 1].clone();
                let placeholder = Term::Placeholder {
                    index: n as u32,
                    fallback_name: name,
                    ty,
                };
                return wrap_in_id(
                    target,
                    Term::FnPlaceholder {
                        params: lam_params,
                        ret: (**ret).clone(),
                        body: Box::new(placeholder),
                    },
                );
            }
            let mut lam_env = env.to_vec();
            lam_env.extend(lam_params.iter().cloned());
            let body = gen_at(
                rng,
                user_rng,
                ret,
                &lam_env,
                depth - 1,
                surface_mode,
                user_elaborators,
            );
            wrap_in_id(
                target,
                Term::Lambda {
                    params: lam_params,
                    ret: (**ret).clone(),
                    body: Box::new(body),
                },
            )
        }
        Type::Product(a, b) => Term::PolyCall {
            // Don't wrap the value-args in `into!` / `onto!`
            // here: when surface mode is on, the renderer collapses
            // `__pair__(T1, T2, a, b)` into the surface tuple
            // literal `(a, b)`, where the leading type-args are
            // *inferred* from the value-args' synthesised types. That makes
            // the value-arg slots synth positions, and `into!` /
            // `onto!` requires a check position. The fn body
            // and `__left__` / `__right__` value-args (which
            // never collapse to surface sugar) remain valid wrap
            // sites.
            name: "__pair__".to_string(),
            type_args: vec![(**a).clone(), (**b).clone()],
            args: vec![
                gen_at(
                    rng,
                    user_rng,
                    a,
                    env,
                    depth - 1,
                    surface_mode,
                    user_elaborators,
                ),
                gen_at(
                    rng,
                    user_rng,
                    b,
                    env,
                    depth - 1,
                    surface_mode,
                    user_elaborators,
                ),
            ],
        },
        Type::Sum(a, b) => {
            // Three productions, each 1/3:
            //   - __left__(A, B, x: A)
            //   - __right__(a, b, y:b)
            //   - IfElse(cond, x:target, y:target) — a surface-form
            //     node that renders as `if c { x } else { y }` when
            //     surface mode is on, and as
            //     `__if_then_else__(target, c, .(){x}, .(){y})`
            //     otherwise. Including it as a Kio'-mode-renderable
            //     production keeps the body-level Kio' render signal
            //     honest: a program with IfElse renders the function
            //     body to Kio' source iff its `uses_surface` flag is false.
            let pick = rng.gen_range(0..3);
            match pick {
                0 => {
                    let inner = gen_at(
                        rng,
                        user_rng,
                        a,
                        env,
                        depth - 1,
                        surface_mode,
                        user_elaborators,
                    );
                    let inner = maybe_wrap_check_position(
                        rng,
                        user_rng,
                        inner,
                        a,
                        surface_mode,
                        user_elaborators,
                    );
                    Term::PolyCall {
                        name: "__left__".to_string(),
                        type_args: vec![(**a).clone(), (**b).clone()],
                        args: vec![inner],
                    }
                }
                1 => {
                    let inner = gen_at(
                        rng,
                        user_rng,
                        b,
                        env,
                        depth - 1,
                        surface_mode,
                        user_elaborators,
                    );
                    let inner = maybe_wrap_check_position(
                        rng,
                        user_rng,
                        inner,
                        b,
                        surface_mode,
                        user_elaborators,
                    );
                    Term::PolyCall {
                        name: "__right__".to_string(),
                        type_args: vec![(**a).clone(), (**b).clone()],
                        args: vec![inner],
                    }
                }
                _ => Term::IfElse {
                    cond: Box::new(Term::BoolLit(rng.gen_bool(0.5))),
                    then: Box::new(gen_at(
                        rng,
                        user_rng,
                        target,
                        env,
                        depth - 1,
                        surface_mode,
                        user_elaborators,
                    )),
                    else_: Box::new(gen_at(
                        rng,
                        user_rng,
                        target,
                        env,
                        depth - 1,
                        surface_mode,
                        user_elaborators,
                    )),
                    then_ty: target.clone(),
                    else_ty: target.clone(),
                },
            }
        }
    }
}

/// Wrap a freshly-introduced `fn` in `id(<fun_ty>, <fn>)` so the
/// lambda always has a Case-C call-site context. Slice 11.0 stripped
/// `fn` parameter and return-type annotations from the rendered
/// surface, so an un-wrapped `.(a, b) { … }` whose body doesn't
/// reference `a`/`b` lands in the typer with unset parameter cells
/// and is rejected as ambiguous. Wrapping in `id` pre-fills the
/// cells from the type-arg position and is transparent at runtime.
fn wrap_in_id(fun_ty: &Type, lam: Term) -> Term {
    Term::PolyCall {
        name: "id".to_string(),
        type_args: vec![fun_ty.clone()],
        args: vec![lam],
    }
}

/// A small canonical inhabitant of `target`. Used at depth 0 and
/// from the shrinker.
pub fn canonical<R: Rng>(rng: &mut R, target: &Type) -> Term {
    match target {
        Type::Base(b) => base_literal(rng, *b),
        Type::Unit => Term::UnitLit,
        Type::Label {
            label,
            constructor,
            payload,
            ..
        } => Term::LabelConstruct {
            label: label.clone(),
            constructor: constructor.clone(),
            payload: Box::new(canonical(rng, payload)),
        },
        Type::Nominal {
            payload,
            constructor,
            ..
        } => Term::App(
            Box::new(Term::Var(constructor.clone())),
            vec![canonical(rng, payload)],
        ),
        Type::Param {
            payload,
            constructor,
            ..
        } => Term::PolyCall {
            name: constructor.clone(),
            type_args: vec![(**payload).clone()],
            args: vec![canonical(rng, payload)],
        },
        Type::Fun(args, ret) => {
            let lam_params: Vec<(String, Type)> = args
                .iter()
                .enumerate()
                .map(|(i, t)| (format!("u{i}"), t.clone()))
                .collect();
            wrap_in_id(
                target,
                Term::Lambda {
                    params: lam_params,
                    ret: (**ret).clone(),
                    body: Box::new(canonical(rng, ret)),
                },
            )
        }
        Type::Product(a, b) => Term::PolyCall {
            name: "__pair__".to_string(),
            type_args: vec![(**a).clone(), (**b).clone()],
            args: vec![canonical(rng, a), canonical(rng, b)],
        },
        Type::Sum(a, b) => {
            if rng.gen_bool(0.5) {
                Term::PolyCall {
                    name: "__left__".to_string(),
                    type_args: vec![(**a).clone(), (**b).clone()],
                    args: vec![canonical(rng, a)],
                }
            } else {
                Term::PolyCall {
                    name: "__right__".to_string(),
                    type_args: vec![(**a).clone(), (**b).clone()],
                    args: vec![canonical(rng, b)],
                }
            }
        }
    }
}
