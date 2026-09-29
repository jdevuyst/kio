# Case study: the optics library

This page is a faithful read-through of the optics proof-of-concept package
at [`test-data/poc/optics/`](../../test-data/poc/optics/). That package is
executable, `kio test`-checked, and run end to end against every declared
target in CI, so it is *ground truth*: every type, function, operator, and
law shown here exists in the package exactly as written, and the laws are
discharged by the `equiv` blocks at the bottom of the library file.

This case study is the reference-grade walkthrough: it covers the *whole*
public surface of the library, explains the structural glue each optic is
built from, and works through every `equiv` law that pins the library's
behavior. For the structural building blocks it leans on, the topic guides
[Structural products and row types](../guides/products.md),
[Structural sums and pattern matching](../guides/sums.md), and
[Operators](../guides/operators.md) are the gentler
companions — read those first if products, sums, or `op` bindings are new to
you.

An **optic** is a first-class, composable accessor into a data structure.
The whole library rests on one idea: an optic is **a pair of functions**.
A lens is a getter paired with a setter; a prism is a partial getter paired
with a constructor; an iso is a forward map paired with its inverse. Every
combinator just pulls those two functions out of the pair and applies them.
Nothing in the library is magic — each optic is two functions in a tuple,
plus the structural glue that builds and takes apart tuples and sums. The
library is **host-capability-free**: it depends on nothing but the spine
elaborators and `match!`, so the whole vocabulary is one file you can copy
into your own package and use as-is.

## How the package is laid out

The package name is `optics`. Its `workdir/` follows the split POC layout:

- [`optics.pkg.kio`](../../test-data/poc/optics/workdir/optics.pkg.kio) — the package
  file: a `build { ... }` block, and a `bridge { ... }` block exposing the
  `optics` module and the dependency's `elab/testapi` host module.
- [`elab.dep.kio`](../../test-data/poc/optics/workdir/elab.dep.kio) — the
  dependency declaration that brings in the `elab` POC, whose
  `spine_elaborators` and `match` modules the library is built on. The
  dependency form is documented in
  [`pkg.md` § Dependency files](../guides/pkg.md#dependency-files).
- [`optics.kio`](../../test-data/poc/optics/workdir/optics.kio) — the
  library itself: a single **root module** holding the entire optic
  vocabulary (lens, prism, iso, their combinators and concrete instances,
  the two coercions) followed by the `equiv` laws.
- [`demo/optics_demo.pkg.kio`](../../test-data/poc/optics/workdir/demo/optics_demo.pkg.kio)
  and [`demo/library.dep.kio`](../../test-data/poc/optics/workdir/demo/library.dep.kio)
  — the nested runnable package. It imports the root package as `library`
  and supplies the host-facing tour.
- [`demo/testapi/main.kio`](../../test-data/poc/optics/workdir/demo/testapi/main.kio)
  — the program: host display helpers, the operator DSL, the row-update
  operator, and a demonstrative `main` that exercises the whole surface.
- `demo/testapi.kio` and `demo/testapi/{arith,fmt,io}.kio` —
  host-boundary scaffolding (`host type` / `host fn` declarations) standing in
  for the capabilities a real host supplies (`I32`, `String`, `add`,
  `int_to_string`, `print`). A user replaces these with their own host.
- `elab/spine_elaborators` and `elab/match` — the imported compile-time
  helpers the library is built on, brought in through the `elab` dependency
  (the modules re-root under the `elab/` local name). They are the subject of
  the separate [`elab` POC](../guides/using-libraries.md); here they
  are imported and used, not explained.

The split between `optics.kio` and `demo/testapi/main.kio` is deliberate: the
optic vocabulary needs no host capability at all, so it sits alone in the
root package; the tour needs arithmetic and printing, so those live in the
nested demo package where the host items are declared. You can depend on the
package or copy `optics.kio` and declare the `elab` dependency under the local
name `elab`. If you want the worked tour too, copy `demo/testapi/main.kio` and
point its host imports at your own host.

All snippets in this case study compile inside a hidden harness that supplies
the same host shape the POC's `testapi` modules declare — `Bool`, `I32`,
`Int`, `String`, and the `add` / `int_to_string` / `print` host functions the
tour uses — plus the spine elaborators and `match!`. The snippets thread
together: each picks up where the previous one left off, and the harness
validates them as one growing program.

<!--kio {harness=optics placeholder="__INSERT_CODE_HERE__" accumulate}
bridge {
  kiodoc;
}

module kiodoc;

import match(match);
import spine_elaborators(narrow_prod, one_prod, widen_sum);

host type Bool role(bool);
host type I32 role(i32);
pub type Int = I32;
host type String role(str);
host fn print(p0: String) -> .;
host fn int_to_string(p0: I32) -> String;
host fn add(p0: I32, p1: I32) -> I32;

__INSERT_CODE_HERE__
-->

## The structural glue

Before the optics themselves, a word on the toolkit they are built from. The
optic functions read and write `&`-products and `|`-sums, and the library
does that entirely with **spine elaborators** — checked, imported
compile-time helpers that generate the value-level glue to build and take
apart structural types. The library's import block is its whole dependency
list:

```kio {ignore}
import elab/spine_elaborators(flatten_sum, narrow_sum, one_prod, one_sum, widen_sum);
import elab/match(match);
```

Two of these carry almost the entire library:

- **`one_prod!(p, T)`** extracts the single component of type `T` out of a
  product `p`. It is how every combinator pulls a getter or setter back out
  of the pair an optic *is*. It is well-defined precisely when exactly one
  spine slot of `p` has type `T`.
- **`widen_sum!(v, T)`** injects a value `v` into a wider sum type `T` — the
  injection that builds an `(A | .)` "hit", drops to the `()` "miss" arm, or
  lands a value at a chosen arm of a sum.

The library's import line names five spine forms (`flatten_sum`,
`narrow_sum`, `one_prod`, `one_sum`, `widen_sum`). Only `one_prod!` and
`widen_sum!` are used in `optics.kio` itself; the others ride along in the
import because they are part of the spine palette an adopter typically keeps
in scope. The row-update operator in `demo/testapi/main` adds `narrow_prod!`. No
algebraic elaborator (`iso!`, `into!`, `onto!`, `align!`, `ease!`, `atom!`)
and no raw `__intrinsics__` appear anywhere in the library — a deliberate
constraint the library header states: the optic vocabulary is built from the
everyday structural palette an adopter already has.

Both `one_prod!` and `widen_sum!` are **positional**. The spine engine walks
the right spine of the source and target types slot-by-slot, with no DNF
normalization and no distributivity rewrite — a slot matches a slot by type
and position only. That positional discipline is exactly why the optic
functions stay correct even when their type parameters coincide: a getter and
setter of *distinct* types are still distinguishable by `one_prod!`, and a
left-vs-right arm of a sum is still distinguishable by position even when both
arms have the same type. The full per-form contract for the spine palette —
the right-spine slot multiset each form matches, and why each form is sound,
total, and information-preserving — is documented in the elaborator-POC case
study [`docs/poc/elab.md`](elab.md), which exercises every spine form against
`equiv` laws; this case study uses the forms in the optics setting rather
than re-deriving their per-form semantics.

`match!` is the dispatch elaborator. Its product block supplies clause lambdas,
which it turns into structural dispatch over a product or sum: a product
scrutinee is destructured positionally by a single clause, and a sum
scrutinee is dispatched arm-by-arm by clauses whose parameter types name the
arms. The library uses it for both: to eliminate the `(A | .)` a prism
preview returns, and to destructure an iso's pair when `one_prod!` cannot
(see § Iso). The `match!` clause-dispatch mechanism is specified in
[`specs/language.md` § Pattern matching](../../specs/language.md#pattern-matching);
the per-branch dispatch and per-leaf reconstruction algorithm is exercised
end to end in the elaborator-POC case study [`docs/poc/elab.md`](elab.md).

## Lens — a getter paired with a setter

A **lens** focuses on a part `A` that is *always present* inside a container
`S`. Concretely, that is two functions — read the part out, and write a new
part back:

```kio {@optics}
type Lens[S][A] = (S -> A) & ((A & S) -> S);
```

A `Lens(S, A)` is the product of a getter `S -> A` and a setter
`(A & S) -> S`. Nothing more. The getter is unary and the setter binary, so
the two components have distinct types — and that distinction is exactly what
lets `one_prod!` pick each one out of the pair unambiguously.

### view, set, over

The three core lens combinators pull the two functions out and apply them:

```kio {@optics}
fn view[S][A](l: Lens(S, A), x: S) -> A { one_prod!(l, S -> A)(x) }

fn set[S][A](l: Lens(S, A), new_a: A, x: S) -> S { one_prod!(l, (A & S) -> S)(new_a, x) }

fn over[S][A](l: Lens(S, A), f: A -> A, x: S) -> S { set(l, f(view(l, x)), x) }
```

`one_prod!(l, S -> A)` picks the getter and applies it to `x`;
`one_prod!(l, (A & S) -> S)` picks the setter and applies it to the new value
and the container. `one_prod!` can disambiguate because the getter type
`S -> A` and the setter type `(A & S) -> S` are different — there is
exactly one product slot of each. `over` is *defined* in terms of the other
two — view, apply `f`, set — so its meaning is the equation
`over(l, f, x) ≡ set(l, f(view(l, x)), x)`. That equation is later checked by
an `equiv` law.

### Concrete lenses

The library ships three concrete lenses. The first two focus a positional
component of a pair:

```kio {@optics}
fn fst_lens[A][B]() -> Lens(A & B, A) {
  let get = .((x: A, _y: B)) -> A { x };
  let put = .(new_a: A, (_x: A, y: B)) -> A & B { (new_a, y) };
  (get, put)
}

fn snd_lens[A][B]() -> Lens(A & B, B) {
  let get = .((_x: A, y: B)) -> B { y };
  let put = .(new_b: B, (x: A, _y: B)) -> A & B { (x, new_b) };
  (get, put)
}
```

The getter and setter destructure the pair *positionally* — the lambda binds
`(x: A, _y: B)` by position, so `fst_lens` stays correct (it really does
focus the *first* component) even when `A` and `B` are the same type. Each
lambda is return-annotated, and the two go into a tuple literal whose type is
`Lens(A & B, A)`. `snd_lens` is the mirror image, focusing the second
component. Both have a Unit value layer: in `fst_lens()` the empty source call
lets the surrounding context infer the type parameters and supplies Unit.

The third concrete lens is the identity lens, which focuses the whole
container — it is the unit of lens composition:

```kio {@optics}
fn id_lens[A]() -> Lens(A, A) {
  let get = .(x: A) -> A { x };
  let put = .(new_a: A, _old: A) -> A { new_a };
  (get, put)
}
```

`view(id_lens(), x)` returns `x` unchanged; `set(id_lens(), new, x)` replaces
it with `new`, discarding the old container. Both facts are pinned by laws
below.

### Composing lenses

The point of optics is that they *compose*. A lens `S → A` and a lens
`A → B` make a lens `S → B`:

```kio {@optics}
fn compose_lens[S][A][B](outer: Lens(S, A), inner: Lens(A, B)) -> Lens(S, B) {
  let get = .(x: S) -> B { view(inner, view(outer, x)) };
  let put =
    .(new_b: B, x: S) -> S { let focus = view(outer, x); set(outer, set(inner, new_b, focus), x) };
  (get, put)
}
```

`get` reads through both lenses: view through the outer to reach the middle
`A`, then view through the inner to reach `B`. `put` is the careful half — it
reads the middle structure, sets the inner part *inside* that middle, then
writes the updated middle back through the outer lens. With `compose_lens`
you reach arbitrarily deep: `compose_lens(fst_lens(...), snd_lens(...))`
focuses the second component of the first component.

## Prism — focusing one arm of a sum

Where a lens focuses a part that is *always there*, a **prism** focuses a
part that *might not be* — one arm of a sum. Its getter can miss, so it
returns `(A | .)`, with the `()` arm meaning "this is not the focused case".
Its constructor injects an `A` back into `S`:

```kio {@optics}
type Prism[S][A] = (S -> A | .) & (A -> S);
```

### preview, review, over_prism

`preview` runs the partial getter; `review` runs the constructor. Both are
the same `one_prod!` projection the lens combinators use, picking the right
half of the pair by type:

```kio {@optics}
fn preview[S][A](p: Prism(S, A), x: S) -> A | . { one_prod!(p, S -> A | .)(x) }

fn review[S][A](p: Prism(S, A), v: A) -> S { one_prod!(p, A -> S)(v) }
```

`over_prism` applies a function through the prism when the source is in the
matching arm, and leaves it unchanged otherwise. This is where `match!`
eliminates the `(A | .)` that `preview` produces:

```kio {@optics}
fn over_prism[S][A](p: Prism(S, A), f: A -> A, x: S) -> S {
  match! preview(p, x) {
    (.(av: A) { review(p, f(av)) }, .(_u: .) { x })
  }
}
```

The clause whose parameter is typed `A` matches the focused arm and rebuilds
through `review(p, f(av))`; the clause typed `()` is the miss path and returns
`x` untouched. Both arms produce `S`, so the call returns `S`.

### Concrete prisms

`left_prism` focuses the left arm of a sum `(A | B)`. Its preview maps the
source's left arm to the target's `A` arm and the right arm to `()`; each
`match!` clause explicitly constructs the common `A | .` result with
`widen_sum!`. Its review is a single left-injection:

```kio {@optics}
fn left_prism[A][B]() -> Prism(A | B, A) {
  let pre =
    .(x: A | B) -> A | . {
      match! x {
        (.(av: A) { widen_sum!(av, A | .) }, .(_bv: B) { widen_sum!((), A | .) })
      }
    };
  let rev = .(av: A) -> A | B { widen_sum!(av, A | B) };
  (pre, rev)
}

fn right_prism[A][B]() -> Prism(A | B, B) {
  let pre =
    .(x: A | B) -> B | . {
      match! x {
        (.(_av: A) { widen_sum!((), B | .) }, .(bv: B) { widen_sum!(bv, B | .) })
      }
    };
  let rev = .(bv: B) -> A | B { widen_sum!(bv, A | B) };
  (pre, rev)
}
```

`right_prism` is the mirror of `left_prism`: its preview keeps the right arm
and drops the left, and its review injects into the right arm of `(A | B)`.
In each preview, the two `match!` clauses are dispatched by their parameter
type — `.(av: A)` takes the left arm, `.(_bv: B)` takes the right — and each
body writes `widen_sum!` against that preview's common result — `A | .` for
`left_prism`, `B | .` for `right_prism`. This explicit result construction is
separate from dispatch: the clause parameter selects the source arm, while the
common result makes the clause bodies agree. The positional `match!` keeps
`left_prism` meaning "left" and `right_prism` meaning "right" even when `A`
and `B` coincide.

There is a subtle and important point about `rev`. `widen_sum!(av, A | B)` is
a single left-injection, and the spine engine generates that glue *once*
against the abstract type `A` — treating `A` as one opaque spine slot.
Because the slot is opaque, the generated injection stays a valid
*left*-injection even when `A` is later instantiated to a *sum* type. That is
the adopter-natural way to build an outer-left injection that a monomorphic
spine `widen_sum!` could not reach directly — you route the whole value
through a polymorphic prism's own `review`. The demonstrative tour exercises
exactly this when it composes prisms (see § The deep-prism tour).

### Composing prisms

Two prisms compose into a prism for a nested focus:

```kio {@optics}
fn compose_prism[S][A][B](psa: Prism(S, A), pab: Prism(A, B)) -> Prism(S, B) {
  let pre =
    .(x: S) -> B | . {
      match! preview(psa, x) {
        (.(av: A) { preview(pab, av) }, .(_u: .) { widen_sum!((), B | .) })
      }
    };
  let rev = .(bv: B) -> S { review(psa, review(pab, bv)) };
  (pre, rev)
}
```

The preview runs the outer prism `psa`; on a hit it runs the inner prism
`pab` on the focused value (the result is already a `B | .`), and on a miss
it widens `()` into `B | .`. The review injects inside-out:
`review(psa, review(pab, bv))` builds the inner arm first, then injects that
into the outer arm.

## Iso — a lossless, reversible pair

An **iso** is the strongest optic — a bijection. Both directions are total;
neither can miss:

```kio {@optics}
type Iso[S][A] = (S -> A) & (A -> S);
```

`view_iso` and `review_iso` are mutual inverses. That contract is a usage
discipline, not something the type system enforces — but the concrete isos in
the library honor it, and the `equiv` laws check it.

### view_iso, review_iso, over_iso

The elimination strategy here differs from the lens and prism combinators,
and the reason is worth dwelling on. An iso's two components can have the
*same* type: when `S` and `A` coincide, both the forward `S -> A` and the
backward `A -> S` are `S -> S`. So `one_prod!` — which needs a *distinct*
slot type to pick a component — cannot tell them apart. The iso combinators
therefore use `match!` *positional* destructuring instead:

```kio {@optics}
fn view_iso[S][A](i: Iso(S, A), x: S) -> A {
  let fwd =
    match! i {
      .(f: S -> A, _b: A -> S) { f }
    };
  fwd(x)
}

fn review_iso[S][A](i: Iso(S, A), v: A) -> S {
  let bwd =
    match! i {
      .(_f: S -> A, b: A -> S) { b }
    };
  bwd(v)
}

fn over_iso[S][A](i: Iso(S, A), f: A -> A, x: S) -> S { review_iso(i, f(view_iso(i, x))) }
```

`match! i { .(f: S -> A, _b: A -> S) { f } }` binds the pair positionally — `f` is the first
component, `_b` the second — and returns the first; `review_iso` returns the
second. `over_iso` converts forward, applies `f` to the converted value, and
converts back — its meaning is `over_iso(i, f, x) ≡ review_iso(i, f(view_iso(i, x)))`,
again pinned by a law.

> This is the cleanest illustration of the positional-vs-typed split in the
> spine palette: when the two slots have distinct types, pick by type with
> `one_prod!` (lens, prism); when they may coincide, destructure by position
> with `match!` (iso).

### Concrete isos

The library ships two concrete isos. `swap_iso` swaps the components of a
pair; `assoc_iso` re-associates a left-nested triple to the right:

```kio {@optics}
fn swap_iso[A][B]() -> Iso(A & B, B & A) {
  let to = .((x: A, y: B)) -> B & A { (y, x) };
  let from = .((y: B, x: A)) -> A & B { (x, y) };
  (to, from)
}

fn assoc_iso[A][B][C]() -> Iso((A & B) & C, A & B & C) {
  let to = .(((av: A, bv: B), cv: C)) -> A & B & C { (av, (bv, cv)) };
  let from = .((av: A, (bv: B, cv: C))) -> (A & B) & C { ((av, bv), cv) };
  (to, from)
}
```

`swap_iso`'s forward and backward maps are both pair-shuffles; the `to`
binder destructures `(A & B)` and rebuilds it swapped. `assoc_iso` moves the
nesting parenthesis: `to` takes `((A & B) & C)` and produces the
right-nested `A & B & C` (which parses as `A & (B & C)`), and `from` is the
inverse. Both rely on `match!`-style positional binding in the lambda
parameter patterns to take pairs apart.

### Composing isos

Two isos compose into one round-trip conversion. The forward map runs both
forward; the backward map runs both backward, inside-out:

```kio {@optics}
fn compose_iso[S][A][B](isa: Iso(S, A), iab: Iso(A, B)) -> Iso(S, B) {
  let to = .(x: S) -> B { view_iso(iab, view_iso(isa, x)) };
  let from = .(v: B) -> S { review_iso(isa, review_iso(iab, v)) };
  (to, from)
}
```

## Coercions — every iso is a lens and a prism

Because an iso is both lossless *and* total, every iso *is* a lens and *is* a
prism. The library provides the two forward coercions:

```kio {@optics}
fn iso_to_lens[S][A](i: Iso(S, A)) -> Lens(S, A) {
  let get = .(x: S) -> A { view_iso(i, x) };
  let put = .(new_a: A, _old: S) -> S { review_iso(i, new_a) };
  (get, put)
}

fn iso_to_prism[S][A](i: Iso(S, A)) -> Prism(S, A) {
  let pre = .(x: S) -> A | . { widen_sum!(view_iso(i, x), A | .) };
  let rev = .(v: A) -> S { review_iso(i, v) };
  (pre, rev)
}
```

The derived lens's getter *is* the iso's forward map, and its setter ignores
the old container entirely — an iso loses no information, so there is nothing
to preserve from the old value; `put` just runs `review_iso` on the new
value. The derived prism always hits: its preview widens `view_iso(i, x)`
into the `A | .` "hit" arm, never the `()` miss arm, and its review is the
iso's backward map.

The reverse coercions — lens-to-iso, prism-to-iso — do **not** exist, and
cannot: a lens that ignores half its input, or a prism with no total inverse,
has no way to become an iso. The hierarchy is one-directional.

## An operator DSL

The named combinators are correct but get noisy when chained. The
demonstrative tour binds a handful of operators with `op` declarations, each
delegating to a combinator. Because every combinator takes its receiver
first, the operator desugar threads the operands through in the right order:

```kio {@optics}
op _ ^ _ { impl view }

op __ ** _ { impl compose_lens }

op _ ^? _ { impl preview }

op _ ## _ { impl review }
```

Now:

- `l ^ x` is `view(l, x)` — lens (or iso-derived lens) forward read.
- `l1 ** l2` is `compose_lens(l1, l2)` — lens chaining; the `__` left slot
  makes the chain *left-associative*, so `a ** b ** c` is
  `compose_lens(compose_lens(a, b), c)`.
- `p ^? x` is `preview(p, x)` — prism preview.
- `p ## a` is `review(p, a)` — prism review (and, through an iso-derived
  prism, the iso's backward map).

A composed lens chain reads naturally:

```kio {ignore}
let l_chain = fst_lens(I32 & String, I32, ()) ** snd_lens(I32, String, ());
say_str(l_chain ^ outer);
```

`l_chain` focuses the inner `String` of `((I32 & String) & I32)` by composing
`fst_lens` then `snd_lens`, and `l_chain ^ outer` views it — `outer` is the
tour's `((I32 & String) & I32)` sample value.

> The `op` mechanics — the `_` / `__` slot patterns, associativity, and the
> receiver-first desugar — are specified in
> [Operators](../guides/operators.md).

### The row-update operator

The tour also binds a per-label row-update setter to a `%%` operator. This is
the concrete bridge between optics and **row types**: a row-typed field setter
is exactly the "set" half of a lens, specialized to a named field. It builds
on an anonymous `labels` block and the `narrow_prod!` spine form:

```kio {@optics}
labels { hed: I32, mid: String, tal: I32 };

fn set_hed[R](x: Hed & R, n: I32) -> Hed & R { ({hed = n}, narrow_prod!(x, R)) }

op __ %% _ { impl set_hed }
```

The `labels` block generates three nominal types — `Hed`, `Mid`, `Tal`
(label spelling, first letter capitalized) — each a thin newtype over its
payload (`Hed` over `I32`, `Mid` over `String`, `Tal` over `I32`). A value
crosses *into* the type with the brace constructor `{hed = n}` and crosses
*out* with the generated `.get` projector (used in `show_hmt` below).

`set_hed` takes a row whose head is a `Hed` and whose tail is anything (`R`),
and rebuilds it with a fresh `Hed`: it constructs `{hed = n}` for the new
head and uses `narrow_prod!(x, R)` to drop the old `Hed` head off `x`,
keeping the rest of the row. `r %% n` replaces the `hed` field; the `[R]`
binder is inferred from the receiver's value type, so the call site writes no
type argument. The `__` left slot makes `%%` left-associative, so
`r %% a %% b` chains as `set_hed(set_hed(r, a), b)` — last write wins.

> The `labels` machinery, brace construction, `.get` projection, and the
> `narrow_prod!` row-tail drop are covered in
> [Structural products and row types](../guides/products.md).

## The demonstrative tour

The [`demo/testapi/main`](../../test-data/poc/optics/workdir/demo/testapi/main.kio)
module runs the whole surface against a real host. Three things in it are
worth calling out as patterns an adopter can copy: how it keeps host display
code in the runner-facing tour, how it reads a row back out, and how it
builds a deeply-nested sum value.

### Host use stays in the tour

The pure optic vocabulary needs no host item. The tour does: arithmetic drives
`over`, and display helpers stringify and print the observed values. Because
those helpers are demo-only rather than reusable optic API, `demo/testapi/main.kio`
imports the host functions directly instead of wrapping them in a capability
bundle:

```kio {@optics}
fn say_i32(n: I32) -> . { print(int_to_string(n)); print("\n") }

fn say_str(s: String) -> . { print(s); print("\n") }

fn say_opt_i32(x: I32 | .) -> . {
  match! x {
    (.(n: I32) { print("Some("); print(int_to_string(n)); print(")") }, .(_u: .) { print("None") })
  };
  print("\n")
}
```

That split is the useful pattern: `optics.kio` stays host-capability-free,
while the runner-facing module is free to name the host it actually runs
against. POCs whose public operations genuinely need host capabilities declare
those host requirements in the root package and let demos `rehost` them to a
local adapter; this tour does not need that extra boundary.

### Reading a row back out

The `%%` operator writes the `hed` field; `show_hmt` reads all three fields
back to render the row. It `match!`-destructures the three-label product and
projects each field with the generated `.get` (written UFCS-style as
`h.>Hed.get`, i.e. `Hed.get(h)`):

```kio {@optics}
fn show_hmt(x: Hed & Mid & Tal) -> . {
  match! x {
    .(h: Hed, m: Mid, t: Tal) {
      print(int_to_string(h.>Hed.get));
      print(", ");
      print(m.>Mid.get);
      print(", ");
      print(int_to_string(t.>Tal.get));
      print("\n")
    }
  }
}
```

The single `match!` clause binds the three labels positionally; each `.get`
crosses the label boundary to recover the `I32` or `String` payload. This is
the read half of the optics-and-row-types bridge: `set_hed` is the lens "set"
specialized to `hed`, and `Hed.get` is the lens "view" for the same field.

### The deep-prism tour

Composing prisms needs a deeply-nested sum value to preview against. The
outer sum is `(I32 | String) | I32`, and the tour reaches the inner `I32`
through `compose_prism(left_prism((I32 | String), I32, ()), left_prism())`.

The interesting case is building a value that lands in the *outer-left* arm —
a whole `(I32 | String)` injected into the left of `(I32 | String) | I32`. A
monomorphic spine `widen_sum!` cannot do this directly: it walks the source's
right spine arm-by-arm against the target's right-spine slots
`[(I32 | String), I32]`, and the source's `String` arm has no like-typed
target slot to land in. The adopter-natural construction is the *outer
prism's own* `review`:

```kio {ignore}
let p_outer = left_prism(I32 | String, I32, ());
let deep_hit = review(p_outer, widen_sum!(5, I32 | String));
```

`left_prism(I32 | String, I32, ())`'s `rev` is `widen_sum!(av, A | B)` generated
against the abstract `A` (a single opaque spine slot), so its left-injection
glue stays valid when `A` is instantiated to the sum `I32 | String`. No
intrinsic, no algebraic elaborator — the polymorphic prism *is* the tool that
reaches the nested arm. The outer-*right* arm, by contrast, is reachable with
`widen_sum!` directly, because `100 : I32` finds a like-typed target arm at
the outer-right slot:

```kio {ignore}
let deep_outer_miss = widen_sum!(100, (I32 | String) | I32);
```

Previewing the composed prism against these three values — outer-left hit
through to the inner `I32`, outer-left holding a `String` (inner miss), and
the outer-right `I32` (outer miss) — yields `Some(5)`, `None`, `None`. The
deep-prism construction is the one place the whole library leans on a prism's
own polymorphic `review` rather than a bare spine form, and it is the worked
answer to "how do I inject a built sum into the left arm of a left-nested
sum?"

### What the tour prints

The full tour exercises lens basics and composition, prism basics and
composition (including deliberate misses), iso basics and composition,
`over_iso`, `id_lens`, both coercions, the four-operator lens/prism DSL, and
the row-update operator. Running the package chains `kio check`, `kio test`,
the per-backend build, and the runner; `main` prints:

```text
42
hello
99, hello
43, hello
hello
world
Some(42)
None
Some(kio)
Some(11)
Some(43)
Some(kio)
Some(5)
None
None
Some(99)
hello
7
42, hello
1, 2, 3
43, hello
7, 100
hello
11, hi
42, hello
hello
Some(42)
None
Some(77)
7, kio, 11
100, kio, 11
300, kio, 11
```

## The laws

The library's behavioral contract is not just prose — it is discharged by
`equiv` blocks at the bottom of `optics.kio`. `kio test` runs each block: it
partial-evaluates both sides with **opaque atoms** standing in for the value
parameters and checks that the two sides reduce to the same residual form. A
law over a sum value residualizes into a *symbolic case-split*, so the prism
and iso round-trips discharge without the evaluator ever deciding a branch.
The `equiv` mechanism — opaque-atom substitution, residual normal forms, and
symbolic case-split equivalence — is specified in
[`specs/formal/equiv.md`](../../specs/formal/equiv.md); the laws below are the
library's, verbatim. There are twenty-two of them, grouped by optic kind.

### Lens laws

The three classic lens laws — get-set, set-get, set-set — hold for both
positional lenses. For `fst_lens`:

```text
// get-set: setting back what you got changes nothing.
equiv fst_get_set[A][B](x: A & B) {
  set(fst_lens(A, B, ()), view(fst_lens(A, B, ()), x), x);
  x
}

// set-get: getting after a set returns what you set.
equiv fst_set_get[A][B](x: A & B, new_a: A) {
  view(fst_lens(A, B, ()), set(fst_lens(A, B, ()), new_a, x));
  new_a
}

// set-set: the last set wins.
equiv fst_set_set[A][B](x: A & B, a1: A, a2: A) {
  set(fst_lens(A, B, ()), a2, set(fst_lens(A, B, ()), a1, x));
  set(fst_lens(A, B, ()), a2, x)
}
```

`snd_lens` carries the same three laws (`snd_get_set`, `snd_set_get`,
`snd_set_set`), identical in shape with `snd_lens` in place of `fst_lens`.
`id_lens` is a unit for `view` and `set` — `view(id_lens(A, ()), x) ≡ x`
(`id_view`) and `set(id_lens(A, ()), new, x) ≡ new` (`id_set`). And `over`'s
defining equation is checked on `fst_lens`:

```text
equiv fst_over_def[A][B](x: A & B, f: A -> A) {
  over(fst_lens(A, B, ()), f, x);
  set(fst_lens(A, B, ()), f(view(fst_lens(A, B, ()), x)), x)
}
```

### Prism laws

The prism round-trip law is build-then-match: `preview(p, review(p, v))` hits
with `v`, expressed as `widen_sum!(v, A | .)` — the `Some(v)` arm. For the
left prism:

```text
equiv left_review_preview[A][B](v: A) {
  preview(left_prism(A, B, ()), review(left_prism(A, B, ()), v));
  widen_sum!(v, A | .)
}
```

`right_prism` carries the mirror round-trip (`right_review_preview`, with the
result widened to `B | .`). And `over_prism` on a freshly-reviewed value
applies `f` under the focus — the focused value really is reached and
transformed:

```text
equiv left_over_review[A][B](v: A, f: A -> A) {
  preview(
    , left_prism(A, B, ())
    , over_prism(left_prism(A, B, ()), f, review(left_prism(A, B, ()), v))
    );
  widen_sum!(f(v), A | .)
}
```

### Iso laws

The two halves of `swap_iso` are mutual inverses in both directions
(`swap_view_review` and `swap_review_view`), and `swap` composed with itself
is the identity iso (`swap_swap_view`):

```text
equiv swap_view_review[A][B](y: B & A) {
  view_iso(swap_iso(A, B, ()), review_iso(swap_iso(A, B, ()), y));
  y
}

equiv swap_swap_view[A][B](x: A & B) {
  view_iso(compose_iso(swap_iso(A, B, ()), swap_iso(B, A, ())), x);
  x
}
```

The `assoc_iso` round-trips (`assoc_view_review`, `assoc_review_view`) are
restated over a concrete tuple shape — opaque leaf atoms `av`, `bv`, `cv` in
a *built* `(av, (bv, cv))` rather than a single fully-opaque product parameter
— because product-eta does not fire on a fully-opaque product; the round-trip
then discharges by reconstruction. And `over_iso`'s defining
equation is checked on `swap_iso`:

```text
equiv swap_over_def[A][B](x: A & B, f: (B & A) -> B & A) {
  over_iso(swap_iso(A, B, ()), f, x);
  review_iso(swap_iso(A, B, ()), f(view_iso(swap_iso(A, B, ()), x)))
}
```

### Coercion laws

The derived optics agree with the iso they came from. The iso-to-lens view
*is* the iso's forward map; the iso-derived lens's set ignores the old
container (so set-then-view returns what you set); the iso-to-prism review
*is* the iso's backward map; and the iso-derived prism always hits on
preview:

```text
equiv iso_lens_view[A][B](x: A & B) {
  view(iso_to_lens(swap_iso(A, B, ())), x);
  view_iso(swap_iso(A, B, ()), x)
}

equiv iso_lens_set_view[A][B](x: A & B, new: B & A) {
  view(iso_to_lens(swap_iso(A, B, ())), set(iso_to_lens(swap_iso(A, B, ())), new, x));
  new
}

equiv iso_prism_review[A][B](y: B & A) {
  review(iso_to_prism(swap_iso(A, B, ())), y);
  review_iso(swap_iso(A, B, ()), y)
}

equiv iso_prism_preview[A][B](x: A & B) {
  preview(iso_to_prism(swap_iso(A, B, ())), x);
  widen_sum!(view_iso(swap_iso(A, B, ()), x), (B & A) | .)
}
```

These four close the loop on the claim that *every iso is a lens and a
prism*: the coercions do not invent behavior, they re-present the iso's own
forward and backward maps through the lens and prism interfaces.

## Adopting the library

To reuse the package, create `optics.dep.kio` at your consumer package root:

```kio {variant=dependency}
dependency optics;

source {
  git "https://github.com/jdevuyst/kio/";
  ref "main";
  path "test-data/poc/optics/workdir/optics.pkg.kio"
}
```

Run `kio dep fetch` from that package root. This writes `optics.lock.kio`
and materializes the selected package, including its `elab` dependency,
under the `optics/` local root. Import the public module with, for example,
`import optics/optics(Lens, view, set);`. The optic functions themselves are
host-capability-free. The POC package bridge also exposes `elab/testapi` host
requirements, so provide or rehost them at the consumer boundary where they
enter its public contract; [Package files and bridges](../guides/pkg.md)
explains that step.

If you need to adapt the library source, copy
[`optics.kio`](../../test-data/poc/optics/workdir/optics.kio) into your package
and declare the [`elab` dependency](elab.md#adopting-the-elaborators) under the
local name `elab`, which is the root its imports use. The worked tour and
operator DSL live separately in
[`demo/testapi/main.kio`](../../test-data/poc/optics/workdir/demo/testapi/main.kio);
its host imports (`I32`, `String`, `add`, `int_to_string`, `print`) refer to
the POC scaffolding and must point at your host when copied.

The whole optic vocabulary — every type, combinator, concrete instance, and
coercion shown above — works as-is once the spine elaborators and `match!`
are in scope, and the `equiv` laws travel with it, so `kio test` re-checks
the library's contract in your package too.

## See also

- [The elaborator POC](elab.md) — the case study that documents the spine
  palette (`one_prod!`, `widen_sum!`, `narrow_prod!`, …), `match!`, and the
  rest of the elaborator vocabulary per form, with their `equiv` laws.
- [Operators](../guides/operators.md) — the `op` mechanics
  behind the `^` / `**` / `^?` / `##` / `%%` DSL.
- [Structural products and row types](../guides/products.md) — the
  `labels` machinery and the per-label setter that becomes the `%%`
  row-update operator.
- [Structural sums and pattern matching](../guides/sums.md) —
  the sum machinery prisms are built on, and `match!` in depth.
- [Using libraries](../guides/using-libraries.md) — the
  map of the `elab` POC modules the spine elaborators and `match!` live in.
- [Testing with `equiv`](../guides/equiv.md) — how the laws
  above are written and run.
- [`test-data/poc/optics/`](../../test-data/poc/optics/) — the full worked
  package this case study reads through.
