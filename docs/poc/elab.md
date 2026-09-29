# Case study: the elaborator library (`elab`)

This page is a faithful read-through of the elaborator proof-of-concept
package at [`test-data/poc/elab/`](../../test-data/poc/elab/). That package is
executable, `kio test`-checked, and built and run against every declared
target in CI, so it is *ground truth*: every elaborator, rule, and law shown
here exists in the package exactly as written, and the laws below are the
`equiv` blocks the package's demo entry discharges.

The `elab` package is a **worked example of the elaborator mechanism itself**.
Its modules are reusable library code; the demo is a worked application. The
package defines, in plain Kio, the control,
coercion, and dispatch elaborators used throughout the documentation —
`if!`, `scope!`, `do!`, the algebraic and spine source-to-target palettes,
`match!`, `derive!`, and the
reflected-type vocabulary they share. The demo exercises the coercion and
dispatch forms end to end.
Because the elaborators are ordinary imported `elab` declarations resolved
through scoped imports (see
[`specs/language.md` § Elaborators are imported, not ambient](../../specs/language.md#elaborators-are-imported-not-ambient)),
nothing here is a language primitive: the whole vocabulary is a library you
can read, copy, and replace.

The companion guides go deeper on the surrounding pieces and link here for the
per-form rules:

- [`elaborators.md`](../guides/elaborators.md) is the
  authoring guide — the compile-time `import __comptime__;` API (`__Type__`,
  `__Checked_term__`, the `__Comptime__` proof argument, and the marked-only
  `__Fill_ctx__`) an elaborator implementation programs against.
- [`using-libraries.md`](../guides/using-libraries.md) is
  the module map of this same POC package.
- [`docs/poc/optics.md`](optics.md) reads through a *library built on* the
  spine palette and `match!`; this page reads through the palettes themselves.

## How the package is laid out

The package name is `elab`. Its
[`workdir/`](../../test-data/poc/elab/workdir/) follows the standard POC
layout:

- [`elab.pkg.kio`](../../test-data/poc/elab/workdir/elab.pkg.kio) — the
  package file: a `build { ... }` block targeting `js`, `ts`, `python`,
  `java`, `rust`, `go`, `swift`, and `haskell`, and a `bridge { ... }` block
  exposing the root `testapi` host boundary plus every elaborator module.
- [`demo/elab_demo.pkg.kio`](../../test-data/poc/elab/workdir/demo/elab_demo.pkg.kio)
  and [`demo/library.dep.kio`](../../test-data/poc/elab/workdir/demo/library.dep.kio)
  — the nested runnable package. It imports the root elaborator package as
  `library` and supplies the runner-facing host modules.
- [`demo/elab/main.kio`](../../test-data/poc/elab/workdir/demo/elab/main.kio)
  — the demonstrative entry module. It imports every elaborator, runs one
  smoke-test `_case` function per form, and carries the `equiv` laws that pin
  each form's rule. This page is organized around that module.
- The elaborator modules, each a reusable library:
  [`control.kio`](../../test-data/poc/elab/workdir/control.kio)
  (`if!` and `scope!`),
  [`sequence.kio`](../../test-data/poc/elab/workdir/sequence.kio)
  (`Bind`, `Sequence`, and `do!`),
  [`spine_elaborators.kio`](../../test-data/poc/elab/workdir/spine_elaborators.kio)
  (the twelve spine forms),
  [`algebraic_elaborators.kio`](../../test-data/poc/elab/workdir/algebraic_elaborators.kio)
  (the six algebraic forms),
  [`match.kio`](../../test-data/poc/elab/workdir/match.kio) (`match!`),
  [`derive.kio`](../../test-data/poc/elab/workdir/derive.kio) (`derive!`),
  [`lookup.kio`](../../test-data/poc/elab/workdir/lookup.kio) (`lookup!` /
  `contains!`),
  [`tuple_elaborators.kio`](../../test-data/poc/elab/workdir/tuple_elaborators.kio),
  [`row_elaborators.kio`](../../test-data/poc/elab/workdir/row_elaborators.kio),
  [`type_of.kio`](../../test-data/poc/elab/workdir/type_of.kio),
  [`show.kio`](../../test-data/poc/elab/workdir/show.kio), and the shared
  [`elaborator_util.kio`](../../test-data/poc/elab/workdir/elaborator_util.kio).
- `testapi.kio` in the root package and `demo/testapi{.kio,/**}` in the demo
  package — host-boundary scaffolding (`host type` / `host fn` declarations)
  standing in for `String`, `I32`, `Bool`, `print`, `string_concat`, and the
  integer/bool stringifiers. A user replaces these with their own host.

The source-to-target coercion elaborators are all declared with the same call
type — `[Source](Source)[Target] -> Target`: a single value parameter (the
source) and a target type-argument that follows the value, exactly
the source-to-target shape the language mechanism specifies. Not every
elaborator shares that shape — `match!`, `zip!` / `map!`, and `set!` / `get!`
take a second value parameter, and `type_of!` takes none (only a type
argument). A block call supplies the final value slots through the declared
blocks: `match! value { clauses }` projects its product block into the clause
argument. The bang at the call site (`iso!`, `fit!`, `match!`, …)
runs the declared `impl` over the reflected source value and target type;
what the implementation returns is the Kio' glue the substitute pass swaps in
at the Lowered → Prime boundary. None of these forms survives into Kio'.

The laws in this case study are the package's `equiv` blocks shown verbatim
(as non-validated `text` fences, since they are discharged by `kio test` over
the package, not by `kio doc`). `kio test` partial-evaluates each law's two
arms with **opaque atoms** standing in for the value parameters and checks
that they reduce to the same residual form; a law over a sum residualizes
into a symbolic case-split (see
[`specs/formal/equiv.md`](../../specs/formal/equiv.md)). Read each law as "the
elaborator call on the first line elaborates to exactly the explicit
`__intrinsics__` term on the second." Those explicit core terms explain what
the elaborator expands to, and some law inputs use core-shaped fixtures to pin
a structural branch. They are not authoring examples for ordinary programs.
When modeling domain data, use labels/newtypes, tuple syntax, `widen_sum!`,
and `match!` at the source level.

## Library-defined control and sequencing

The [`control`](../../test-data/poc/elab/workdir/control.kio) and
[`sequence`](../../test-data/poc/elab/workdir/sequence.kio) modules use the same
elaborator mechanism as the coercion palettes. Their block descriptors say
how the written body becomes an ordinary value argument:

| Export | Block and behavior |
| --- | --- |
| `control.if` | `if! condition { yes } else { no }` has two `thunk` blocks. Generated code evaluates the condition once and invokes only the selected branch. |
| `control.scope` | `scope! { body }` has one `thunk` block. Generated code invokes that body once and returns its result. |
| `sequence.do` | `do! bind { body }` has one `sequence` block. Its implementation applies that sequence to the supplied bind value. |

Both control declarations use `impl(fills)`. `if_checked` constructs a
branching term with `__intrinsic_if_then_else__`; its operands come from the
checked condition and projected thunks. `scope_checked` builds an ordinary
call to its thunk. Each returns its generated term paired with a fill context.
The public types connect their body results to the overall result.
They use ordinary reflected constructors, so neither declaration needs a
compiler hook keyed to its name.

`sequence` exports the transparent aliases
`Bind[*F] = [A][B] (F(A) & (A -> F(B))) -> F(B)` and
`Sequence[*F][Result] = Bind(F) -> F(Result)`. `do` has public type
`[Receiver][Result] Receiver -> (Receiver -> Result) -> Result`.
Its `do_checked` implementation constructs an ordinary application of the
projected sequence to the supplied bind function and returns an explicit fill
relating that application's type to the declared result. The receiver is
evaluated exactly once, including for a block with no bind steps. Ordinary
public-call and header constraints determine the receiver's carrier; the
block does not infer a missing carrier argument of a receiver factory. The
sequence contains ordinary local lets and bind continuations, and ends with an explicit
`F(R)` expression for the same carrier. It never inserts an implicit `pure` call.

Import these names from the module you need, as with any other elaborator.
The [elaborator guide](../guides/elaborators.md) explains all three descriptor
kinds, including the `product` block used by `match!`; the
[higher-kinded-types case study](hkt.md#abstract-brand-crossing-in-a-do-block-pipeline)
shows `do!` over two concrete type constructors through one generic function.

## Two source-to-target palettes

Both palettes answer the same question — *given a value of one structural
shape and a target of another, generate the value-level glue that bridges
them* — but with different engines and different mental models. They are
independent: learning one does not require the other.

- The **spine palette** walks the right spine of source and target types
  **positionally**, slot by slot, with no normalization. A slot matches a
  slot by type and position; `(A & B) & C` and `A & B & C` are *different*
  shapes (2 slots vs 3), and a spine form that expects one rejects the other.
  This is the everyday palette: a reader can mechanically compute a spine
  form's result by walking the two spines.
- The **algebraic palette** first normalizes source and target to **DNF** (a
  sum of products), then matches branch-to-branch using associativity,
  distributivity, and commutativity as rewrites. It accepts shape differences
  the spine palette rejects (distributivity is the headline) at the cost of a
  less mechanical mental model and worst-case exponential DNF blow-up.

The right-spine walk both engines lean on is defined in
[`specs/backends/README.md` § The right-spine walk](../../specs/backends/README.md#the-right-spine-walk).
Three slot rules hold for every form in both palettes: a `newtype` (including
a `labels`-generated newtype) is a single opaque slot — the walk never enters
its payload; a `Forall` is a single opaque slot compared by structural
equality; and a `host type` is a single slot keyed on its declaration site.
Crossing into or out of a label or host type is never an elaborator's job —
use the generated `mk` / `get` members or a host conversion.

## The spine palette

Twelve forms: ten **axis-specific** primitives (each operating on one axis —
sum or product — treating slot types as opaque), the identity form
`spine_identity!`, and the recursive composer `fit!`. The 2×5 grid of the
axis-specific forms, by axis and operation:

| | Sum | Product |
|---|---|---|
| **Reorder** (permutation) | `reorder_sum!` | `reorder_prod!` |
| **Capacity** (extend / truncate) | `widen_sum!` | `narrow_prod!` |
| **Multiplicity** (codiagonal / diagonal) | `narrow_sum!` | `widen_prod!` |
| **Flatten** (associativity) | `flatten_sum!` | `flatten_prod!` |
| **Pick** | `one_sum!` | `one_prod!` |

A shared property: **same-type-order-preserving commutativity** (R-Comm).
Within a like-typed group, the *k*th source slot of type `T` maps to the *k*th
target slot of type `T`; across distinct types, slots may reorder freely. This
is what keeps a spine form deterministic even when several slots share a type.
R-Identity (`A ↔ A & .`, `A ↔ A | !`) lives only in the widen and narrow
forms, never in reorder.

### `reorder_prod!` / `reorder_sum!` — pure permutation

Same spine length required; identical type multiset; no `()` / `!` insertion
or removal. `reorder_prod!` rebuilds the product with `__pair__` placing each
factor at its new slot; `reorder_sum!` rebuilds the sum with
`__left__` / `__right__` threaded through `__either__`.

```text
equiv reorder_prod_law[A][B](a: A, b: B) {
  reorder_prod!((a, b), B & A);
  (b, a)
}

equiv reorder_sum_left_law[A][B](a: A) {
  reorder_sum!(__left__(A, B, a), B | A);
  __right__(B, A, a)
}
```

The smoke-test cases (`reorder_prod_case`, `reorder_sum_case` in `main`)
exercise the same forms against the host `String` type.

### `narrow_prod!` — truncation

Drops product factors whose type is not in the target's multiset; the target
multiset must be a sub-multiset of the source's. Glue is a `__fst__` /
`__snd__` chain picking the kept slots. R-Identity-Prod-elim (dropping a `()`)
falls out as a sub-multiset projection.

```text
equiv narrow_prod_law[A][B](a: A, b: B) {
  narrow_prod!((a, b), A);
  a
}
```

**Forward type rule (default).** With no explicit target and no surrounding
expected type, `narrow_prod!` keeps every slot (the maximal sub-multiset is
the source itself), so `narrow_prod!((a, b))` is `(a, b)` — overridden when a
context requests a proper sub-product:

```text
equiv narrow_prod_default_law[A][B](a: A, b: B) {
  narrow_prod!((a, b));
  (a, b)
}
```

### `narrow_sum!` — codiagonal collapse

Consolidates same-typed source arms onto fewer target arms, with **type-set
equality on non-`!` types** (every non-`!` source type must appear in the
target). A source `!` arm drops freely — the explicit `!`-arm carve-out —
because no value inhabits it. Glue is `__either__` routing each source arm to
the target arm sharing its type. Dropping a *distinct* non-`!` arm (`A | B → A`)
is **rejected**: that is partial sum narrowing, which is `match!`'s territory.

```text
equiv narrow_sum_left_law[A](a: A) {
  narrow_sum!(__left__(A, A, a), A);
  a
}

equiv narrow_sum_drops_bottom_law[A](a: A) {
  narrow_sum!(__left__(A, !, a), A);
  a
}
```

The forward type rule (default) infers the maximal narrowing — the source's
non-`!` arms deduplicated by type:

```text
equiv narrow_sum_default_law[A][B](a: A) {
  narrow_sum!(__left__(A, B, a));
  __left__(A, B, a)
}
```

### `widen_sum!` — capacity extension

Injects the source into a wider sum target whose extra arms are unreachable;
the source multiset must be a sub-multiset of the target's. Glue is a
`__left__` / `__right__` chain placing the source value at its target slot.
Adding a `!` arm (R-Identity-Sum-intro) is a degenerate widening.

```text
equiv widen_sum_law[A][B](a: A) {
  widen_sum!(a, B | A);
  __right__(B, A, a)
}

equiv widen_sum_default_law[A](a: A) {
  widen_sum!(a);
  a
}
```

`widen_sum!` is the single most-used spine form in adopter code: it is how a
value enters a sum at all (the optics library builds every `(A | .)` "hit"
and "miss" arm with it).

### `widen_prod!` — diagonal

Extends the source to a wider product target by **duplicating source slots**
(the diagonal) and synthesizing `__unit__` at `.`-typed target slots; the
source multiset must be a sub-multiset of the target's. The source-order
duplication rule: target slot *k* of type `T` reads source slot *k* of type
`T` if it exists, else source slot 0 of that type. This is the one spine form
that uses a source value more than once.

```text
equiv widen_prod_law[A](a: A) {
  widen_prod!(a, A & . & A);
  (a, ((), a))
}
```

### `flatten_prod!` / `flatten_sum!` — iterative associativity

Un-nest left-leaning structure at the outer axis, applying
`Prod(Prod(L1, L2), R) → Prod(L1, Prod(L2, R))` (and its sum dual) repeatedly
until the source's shape equals the target's. The arm/factor multiset is
preserved at every step — flatten never adds, drops, or reorders across types.
The target is **required** (un-nesting depth is genuinely target-directed);
the form identity-short-circuits when the source already matches.

```text
equiv flatten_prod_law[A][B][C](a: A, b: B, c: C) {
  flatten_prod!(((a, b), c), A & B & C);
  (a, (b, c))
}

equiv flatten_prod_multistep_law[A][B][C][D](a: A, b: B, c: C, d: D) {
  flatten_prod!((((a, b), c), d), A & B & C & D);
  (a, (b, (c, d)))
}

equiv flatten_sum_law[A][B][C](b: B) {
  flatten_sum!(__left__(A | B, C, __right__(A, B, b)), A | B | C);
  __right__(A, B | C, __left__(B, C, b))
}
```

### `one_prod!` / `one_sum!` — focused pickers

`one_prod!` picks the **first** source product slot whose type equals the
target (first-wins); `one_sum!` forwards a sum whose arms all produce the
same target type (the target has sum-spine length 1). An atomic source
degenerates to identity. Both carry a **strict** forward type rule: when the
source pins a unique target (a single slot type, or a uniform arm type), a
call supplying a different `T` is an error.

```text
equiv one_prod_law[A][B](a: A, b: B) {
  one_prod!((b, a), A);
  a
}

equiv one_prod_default_law[A][B](a: A, b: B) {
  one_prod!((a, b));
  a
}

equiv one_sum_left_law[A](a: A) {
  one_sum!(__left__(A, A, a), A);
  a
}

equiv one_sum_ignores_bottom_law[A](a: A) {
  one_sum!(__left__(A, !, a), A);
  a
}
```

`one_prod!` is the spine palette's workhorse for projecting a component out of
a product by its type — the operation the optics library uses to pull a getter
or setter out of the pair an optic *is*.

### `fit!` — the recursive composer

`fit!` is the form a user reaches for when they do not want to name the axis.
It composes `narrow_sum!`, `narrow_prod!`, and `widen_sum!` per a
**narrow-before-widen** discipline, recursing on **spine slots** (the
structural axis at each level) and on **function arrows** (the variance axis
through `->`). It deliberately **excludes `widen_prod!`** — the no-duplication
guarantee is `fit!`'s: every source factor's value flows to at most one target
slot. Users who want duplication reach for `widen_prod!` by name.

```text
equiv fit_widen_sum_law[A][B](a: A) {
  fit!(a, A | B);
  __left__(A, B, a)
}

equiv fit_recursive_product_law[A][B][C](a: A, b: B, c: C) {
  fit!(((a, b), c), A & C);
  (a, c)
}

equiv fit_recursive_sum_law[A][B][C](a: A, b: B) {
  fit!(__left__(A & B, C, (a, b)), A | C);
  __left__(A, C, a)
}
```

**Function-arrow variance.** When source and target are both function types,
`fit!` recurses through `->` with variance — **contravariant** on the
parameter (target's param flows into the source's), **covariant** on the
return. The synthesized adapter is a wrapping lambda whose call shape matches
the target. The two function laws pin both polarities:

```text
equiv fit_function_param_law[A][B](f: (A | B) -> ., a: A) {
  fit!(f, A -> .)(a);
  f(__left__(A, B, a))
}

equiv fit_function_return_law[A][B](f: A -> A & B, a: A) {
  fit!(f, A -> A)(a);
  __fst__(A, B, f(a))
}
```

**Bottom source — R-Strict-Initial.** When the source type is `!`, `fit!`
emits `__absurd__` at any walk depth, encoding the initial-object law (`!` has
no inhabitants, so the unique morphism `! → T` is trivial). The load-bearing
case is the covariant function-arrow return position when a source function
returns `!`: a wrapper around `panic_fn : String -> !` coerced to
`String -> Int` composes cleanly because the never-reached return
absurd-coerces.

```text
equiv fit_bottom_law[A](impossible: !) {
  fit!(impossible, A);
  __absurd__(A, impossible)
}
```

`fit!` rejects polymorphic source / target at the top-level function-arrow
walk (coercing under a `[A]`-binder needs instantiation machinery the engine
does not have), and short-circuits to the receiver when source and target are
already `equiv`-equal. The framing is not "use `fit!` for everything": it is
the safe starting point, and the axis-specifics read better when the operation
is simple enough to name.

## The algebraic palette

Six forms over the DNF engine. All six share a common core —
associativity / distributivity / commutativity-where-forced, plus identity
absorption (`A & . ↔ A`, `A | ! ↔ A`) — and the name chosen at the call site
gates which length-changing rules the engine may additionally apply:

| Rule | `iso!` | `into!` | `onto!` | `align!` | `ease!` | `atom!` |
|---|:-:|:-:|:-:|:-:|:-:|:-:|
| Associativity, distributivity, identity absorption | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Commutativity (source-order pinned) | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Sum widening (`A → A \| B`) | ✗ | ✓ | ✗ | ✓ | ✓ | ✗ |
| Product duplication (`A → A & A`) | ✗ | ✓ | ✗ | ✗ | ✗ | ✗ |
| Product projection (`A & B → A`) | ✗ | ✗ | ✓ | ✗ | ✓ | ✓ |
| Sum collapse (`A \| A → A`) | ✗ | ✗ | ✓ | ✓ | ✓ | ✓ |

### `iso!` — bijection

Rearrangement (associativity, distributivity, commutativity-where-forced)
plus identity absorption — source and target have the same value-set,
reshuffled. The distributivity round-trip `A & (B | C) ↔ (A & B) | (A & C)` is
the headline thing the spine palette cannot do.

```text
equiv iso_reorder_law[A][B](a: A, b: B) {
  iso!((a, b), B & A);
  (b, a)
}

equiv iso_distribute_law[A][B][C](a: A, b: B) {
  iso!((a, __left__(B, C, b)), (A & B) | (A & C));
  __left__(A & B, A & C, (a, b))
}

equiv iso_undistribute_law[A][B][C](a: A, b: B) {
  iso!(__left__(A & B, A & C, (a, b)), A & (B | C));
  (a, __left__(B, C, b))
}
```

### `into!` — injection

Strict superset of `iso!`: adds sum widening (`A → A | B`) and product
duplication (`A → A & A`, the diagonal). Type widens; no information lost. Use
`into!` when you want the compiler to reject data loss. Like-typed widening
pins source order — the *k*th source branch of type `T` maps to the *k*th
target slot of type `T`.

```text
equiv into_duplicate_law[A](a: A) {
  into!(a, A & A);
  (a, a)
}

equiv into_like_typed_source_order_law[A][B](first: A, b: B, second: A) {
  into!((first, (b, second)), A & A & B);
  (first, (second, b))
}
```

`into!(e : !)` is realizable through the existing rules (widen to `! | B`,
absorb the identity) and reduces operationally to `__absurd__(e)` — so `into!`
and `__absurd__` are the same operation with two surface entry points, the
"Kio' primitive + surface convenience" pattern.

### `onto!` — surjection

Strict superset of `iso!`: adds product projection (`A & B → A`) and sum
collapse (`A | A → A`). Type narrows; values may identify. Sum *narrowing*
(`A | B → A`) is **not** included — it is partial, so it belongs to `match!`.

```text
equiv onto_project_law[A][B](a: A, b: B) {
  onto!((a, b), A);
  a
}
```

### `align!` — payload-preserving

Strict superset of `iso!`: adds sum widening and sum collapse, but **not**
product projection (it would lose payload data) and **not** product
duplication. The call-site guarantee: no factor data is dropped.

```text
equiv align_sum_law[A][B](a: A) {
  align!(a, A | B);
  __left__(A, B, a)
}
```

### `ease!` — no-duplication coercion

Strict superset of `iso!`: adds sum widening, sum collapse, **and** product
projection in a single form, but **excludes** product duplication — every
source factor flows to at most one target slot. `ease!` fills the gap where a
coercion mixes sum widening with product projection, which neither `align!`
(no projection) nor `onto!` (no widening) covers alone.

```text
equiv ease_project_law[A][B](a: A, b: B) {
  ease!((a, b), A);
  a
}
```

`ease!` also recognizes function-typed source and target with the same
variance walk over `->` that `fit!` uses (contravariant parameter, covariant
return), running its own DNF rule set at each leaf — the algebraic-palette
entry for higher-order function adaptation. It rejects polymorphic source /
target and short-circuits to the receiver when the two function types are
already `equiv`-equal.

### `atom!` — focused single-value picker

Same rule set as `onto!` (projection plus collapse), restricted so the target
`T` must DNF-normalize to a **single arm** — atom, nominal, or product. A
multi-arm sum target is rejected. Within an arm, like-typed components resolve
by source order (first-wins). When the target is elided with no surrounding
expected type, the typer infers it from the source's DNF as the unique
atomic/nominal type appearing in every arm; an atomic source degenerates to
identity.

```text
equiv atom_project_law[A][B](a: A, b: B) {
  atom!((a, b), A);
  a
}
```

## `match!` — dispatch

`match!` is the dispatch elaborator: its direct call `match! value { clauses }`
projects a `product` block into the clause value, then builds explicit
structural dispatch over the scrutinee. A single product-valued entry may
supply a preassembled clause tuple. Each clause is an expression
of function type whose parameter type is the **dispatch pattern**; `match!`
deliberately supplies no expected parameter shape from the scrutinee or common
result: the written parameter type determines which branches it accepts.
The [sums guide](../guides/sums.md#matching-a-sum) explains catch-all clauses and
the requirement that each clause add coverage.
Dispatch is **per DNF branch**, first clause that fits wins, and the chosen
clause is *called* with its argument reconstructed from the matched scrutinee
components.

`match` and `derive` declare marked implementations. `match_checked` receives
`__Comptime__`, then a `__Fill_ctx__`, followed by the same reflected `Source`,
scrutinee, `Clauses`, clause tuple, and `Target` slots. It returns the generated
dispatch term paired with the final context. `derive_checked` takes the same
leading pair and returns the supplied context unchanged: its marked reflected
target can retain the identity of a generic binder from the enclosing
function while `__term_specialize__` instantiates a candidate rule. The
coercion palettes and other elaborators keep their ordinary `impl`
declarations.

The fill traversal is deliberately separate from runtime dispatch. It visits
clauses in written order. For a polymorphic clause, it calls
`__term_specialize__` against compatible source branches in source-branch
order and appends each specialized result with
`__fill__(ct, fills, target, result)`. A binder-independent raw result is used
when no specialization contributes; a clause-local type that cannot close is
diagnosed. A monomorphic clause appends its raw result.

That authored traversal order does not give one result inference priority. All
candidates for `Target` are adopted as one finite common-result group,
including candidates from a shadowed clause. An enclosing target
checks every candidate. Otherwise any independently determined candidate may
determine `Target`, and every other result must agree. A conflict or a group
with no determining candidate fails as a non-directional common-result
problem. Runtime dispatch remains separate and first-fitting in written clause
order.

An inline polymorphic clause may leave its final result open while the marked
schedule prepares its written binder and parameter header. The common match
target and the ordinary fill relations must determine that result before the
generic body is checked. The worked package's `match_polymorphic_clause_case`
uses `.[A](_value: A) { ... }` with an independently known `String` target.
The provisional result cannot depend on the clause's new `A`; a binder-dependent
result uses a concrete annotation or a complete expected function type.
Named or already-typed polymorphic clause values are unaffected.

The private forall traversal carries the actual binder/body pair as its
structural-recursion fuel. Each step removes one universal binder even when
the final result remains open; binder-arity metadata is not part of that
descent measure.

Each node of the clause tuple's ordinary product construction can expose its
two operands during this marked staging because the resolved product intrinsic
has the exact ordered `[A][B](A, B) -> A & B` scheme. An equivalently typed
ordinary function remains opaque—it may reorder, duplicate, or discard its
inputs—and each original product call still completes once. The rule follows
the resolved intrinsic and scheme, not the spelling `match` or a tuple argument
index.

```text
equiv match_left_law[A][B](a: A) {
  match! __left__(A, B, a) {
    (.(x: A) { x }, .(_y: B) { a })
  };
  a
}

equiv match_right_law[A][B](b: B) {
  match! __right__(A, B, b) {
    (.(_x: A) { b }, .(y: B) { y })
  };
  b
}
```

A clause slot may be a compound product or a reordered packet, not only an
atom — the same structural fit that handles any slot reconstructs it:

```text
equiv match_product_packet_law[A][B](a: A, b: B) {
  match!((a, b)) {
    .(x: A, _y: B) { x }
  };
  a
}

equiv match_reordered_packet_law[A][B](a: A, b: B) {
  match!((a, b)) {
    .(_y: B, x: A) { x }
  };
  a
}

equiv match_recursive_packet_law[A][B][C](a: A, b: B, c: C) {
  match!(((a, b), c)) {
    .(_x: A, z: C) { z }
  };
  c
}
```

`match!` desugars to a `let`-bound lambda per clause plus nested `__either__`
calls for sum discrimination and the projection intrinsics for product
destructure — no `match!` form survives into Kio'. The surface contract is in
[`specs/language.md` § Pattern matching](../../specs/language.md#pattern-matching);
the optics case study [`docs/poc/optics.md`](optics.md) shows `match!` used in
anger to eliminate option arms and destructure pairs.

## `derive!` — instance deriving

`derive!(<candidates>, <TargetType>)` constructs a value of a requested target
type by composing caller-supplied candidate `fn`s, threading each candidate's
logical value slots as sub-goals to discharge from the same set, until the
target is produced. The candidate argument is `()` for no candidates, one
function expression for a singleton, or a product tuple for multiple
functions; every candidate must be function-typed. `derive!` finds the
**unique** composition — two distinct derivations is an error, none is an
error — with kind-aware unification and a static termination guarantee from
the candidates' shapes alone. The target type is the trailing slot; it may be
omitted, or written `_`, when a check-mode position pins it.

The simplest cases pick one target from a candidate argument:

```text
equiv derive_explicit_law() {
  derive!((derive_unit_rule, derive_string_rule), String);
  derive_string_rule()
}

equiv derive_expected_law() {
  derive!((derive_unit_rule, derive_expected_rule), String);
  derive_expected_rule()
}

equiv derive_explicit_unit_law() {
  derive!(derive_unit_rule, .);
  derive_unit_rule()
}

equiv derive_unit_parameter_law() {
  derive!(derive_unit_parameter_rule, String);
  derive_unit_parameter_rule(())
}
```

Only candidate rules produce values. Consequently `derive!((), T)` reports
that no candidate rule derives `T` for every target `T`, including `.`,
structurally inhabited product or sum types, and opaque `newtype`s. A candidate
with no source parameters and one whose sole source parameter is `.` both have
the logical zero-slot shape `. -> R`, so each is an ordinary base rule. Such a
rule can explicitly produce any `R`, including `.`, a host type, a product, a
sum, or a unit-payload `newtype`. A Unit parameter within a multi-slot value
group remains an ordinary precondition.

A candidate with a precondition makes resolution **recursive** — the
precondition is a sub-goal discharged from the same candidate set:

```text
equiv derive_recursive_law() {
  Derived_wrap.un_derived_wrap(
    , Derived_base
    , derive!((derive_wrap_rule, derive_base_rule), Derived_wrap(Derived_base))
    );
  ()
}
```

with the candidates (verbatim from `main`):

```text
fn derive_base_rule() -> Derived_base { Derived_base.mk_derived_base(()) }

fn derive_wrap_rule(_base: Derived_base) -> Derived_wrap(Derived_base) {
  Derived_wrap.mk_derived_wrap(Derived_base, ())
}
```

`derive_wrap_rule`'s value parameter `Derived_base` is a precondition: the
resolver discharges it via the explicit zero-slot `derive_base_rule`, then
applies `derive_wrap_rule` to the result. This is the same recursion that,
scaled up, composes a monad-transformer stack — each transformer rule's
precondition is the inner instance, one type-constructor-wrap smaller, so
resolution terminates. The package also exercises **polymorphic** derivation
(a candidate with a `[A]` binder bound from the goal) and the **derived
`Functor(Identity)`** pick the
[`hkt` case study](hkt.md) uses:

```text
equiv derive_polymorphic_law() {
  Witness.un_witness(String, derive!((derive_unit_rule, derive_witness_rule), Witness(String)));
  ()
}

equiv derive_functor_law() {
  Identity.un_id(
    , String
    , Functor.fmap(derive!((box_functor, identity_functor), Functor(Identity)))(
        , String
        , String
        , .(value) { value }
        , Identity.mk_id("derive functor: ok\n")
        )
    );
  "derive functor: ok\n"
}
```

**Open-world.** The candidate set is exactly the function expressions the
candidate argument literally supplies — `derive!` reaches no implicit pool,
so adding a `fn` anywhere in the package cannot change resolution at an
existing site. A "module handle"
candidate is rejected for the same reason. The synthesized derivation is a
nested call tree of ordinary `fn` applications — no `derive!` form survives,
and no runtime machinery is added. The surface contract is in
[`specs/language.md` § The `derive!` elaborator](../../specs/language.md#the-derive-elaborator).

## The rest of the vocabulary

The package ships more imported elaborators, each `equiv`-pinned in `main` and
mapped in [`using-libraries.md`](../guides/using-libraries.md):

- **`lookup!` / `contains!`** ([`lookup.kio`](../../test-data/poc/elab/workdir/lookup.kio))
  — look a label up in a sum by its generated nominal type, returning the
  payload (`lookup!`) or a boolean (`contains!`):

  ```text
  equiv lookup_present_label_law(a: I32) {
    lookup!(widen_sum!(Foo.mk(a), Foo | (Bar & Flag)), Foo);
    __left__(Foo, ., Foo.mk(a))
  }
  ```

- **Tuple elaborators** ([`tuple_elaborators.kio`](../../test-data/poc/elab/workdir/tuple_elaborators.kio))
  — `head!`, `tail!`, `last!`, `concat!`, `flatten!`, `group!`, `split!`,
  `zip!`, `map!`, `distinct!` over product spines:

  ```text
  equiv tuple_zip_law[A][B][C][D](a: A, b: B, c: C, d: D) {
    zip!((a, b), (c, d), (A & C) & B & D);
    ((a, c), (b, d))
  }
  ```

- **Row elaborators** ([`row_elaborators.kio`](../../test-data/poc/elab/workdir/row_elaborators.kio))
  — `mk!` / `set!` / `get!` for constructing, upserting, and projecting label
  products (the machinery behind the optics row-update operator):

  ```text
  equiv row_set_single_update_law(old: I32, new: I32, b: String) {
    (Foo.mk(old), Bar.mk(b)).>set!((Foo.mk, new));
    (Foo.mk(new), Bar.mk(b))
  }
  ```

  A single `set!` / `get!` can address several fields at once by joining
  selectors with the `*` (row_pair) combinator:

  ```text
  equiv row_get_multi_law(a: I32, b: String) {
    (Foo.mk(a), Bar.mk(b)).>get!(Foo.get * Bar.get);
    (a, b)
  }
  ```

- **`type_of!`** ([`type_of.kio`](../../test-data/poc/elab/workdir/type_of.kio))
  — reflects a type argument into a user-defined `Type_rep` value:

  ```text
  equiv type_of_product_law() {
    type_of!(. & ., ());
    rep_product(rep_unit(), rep_unit())
  }
  ```

- **`show!`** ([`show.kio`](../../test-data/poc/elab/workdir/show.kio)) —
  builds a string-rendering function for supported value shapes from a tuple
  of host converters.

These five show the breadth of what an imported elaborator can express
once it has the reflected-type API: structural search (`lookup!`), spine
arithmetic (`tuple_*`), type-directed row manipulation (`row_*`), type
reflection (`type_of!`), and code generation (`show!`) — all ordinary library
code, all removed before Kio'.

## What `main` prints

`main` chains every `_case` function and prints one line per family. Running
the package chains `kio check`, `kio test`, the per-backend build, and the
runner; the output is:

```text
elaborators: ok
match right: ok
match infer left: ok
match right: ok
match polymorphic: ok
contains elaborator: ok
tuple elaborators: ok
row elaborators: ok
derive explicit: ok
derive expected: ok
derive polymorphic: ok
derive recursive: ok
derive polymorphic recursive: ok
derive functor: ok
type reflection: ok
(show, 42, true)
```

## Adopting the elaborators

To use these elaborators in your own package, **depend on `elab`** rather than
copying its modules — declare it as a dependency and import the elaborator
modules under the dependency's local name. The dependency mechanism (the
`<local>.dep.kio` form, local-path vs. git sources, and the lock file) is
documented once in
[`pkg.md` § Dependency files](../guides/pkg.md#dependency-files);
this page only notes what is elab-specific.

Create `elab.dep.kio` at the root of the consuming package:

```kio {variant=dependency}
dependency elab;

source {
  git "https://github.com/jdevuyst/kio/";
  ref "main";
  path "test-data/poc/elab/workdir/elab.pkg.kio"
}
```

Run `kio dep fetch` from that package root. It writes `elab.lock.kio` and
materializes the selected modules under `elab/`; the lock records the selected
path and commit. The POC package also bridges `testapi` host declarations; if
they enter your public contract, provide or rehost those requirements through
your package's host boundary as explained in [Package files and bridges](../guides/pkg.md).

Pick the elaborator modules your code calls — `control` for `if!` and `scope!`,
`sequence` for `do!`, `spine_elaborators` for the spine palette,
`algebraic_elaborators` for the algebraic one, `match` for `match!`,
`derive` for `derive!`, and so on. The coercion and dispatch modules pull in
the shared `elaborator_util` module transitively; callers do not import it
directly. With `elab` declared under
the local name `elab`, the modules re-root beneath it:

```kio {ignore}
import elab/spine_elaborators(fit);
import elab/match(match);
```

Import each elaborator **without** the `!` — the call site supplies the bang.
The [`elaborators.md`](../guides/elaborators.md) guide covers
the compile-time API if you want to *write* one rather than reuse these.

## See also

- [Defining elaborators](../guides/elaborators.md) — authoring
  elaborators with the `import __comptime__;` reflected-type API.
- [Using libraries](../guides/using-libraries.md) — the
  module map of this same package.
- [The optics library](optics.md) — a library *built on* the spine palette and
  `match!`, with its own `equiv` laws.
- [Higher-kinded types](hkt.md) — uses `derive!` for a single-target instance
  pick.
- [`specs/language.md` § Elaborators are imported, not ambient](../../specs/language.md#elaborators-are-imported-not-ambient)
  — the call mechanism every form here shares.
- [`specs/formal/equiv.md`](../../specs/formal/equiv.md) — how the `equiv`
  laws above are discharged.
- [`test-data/poc/elab/`](../../test-data/poc/elab/) — the full worked package
  this case study reads through.
