# Case study: higher-kinded types (`hkt`)

This page is a faithful read-through of the higher-kinded-types
proof-of-concept package at
[`test-data/poc/hkt/`](../../test-data/poc/hkt/). That package is
executable, `kio test`-checked, and built and run against every declared
target in CI, so it is *ground truth*: every newtype, instance, and
brand-generic operation shown here exists in the package exactly as written,
and the program below is the one its `main` runs.

The `hkt` package is a **worked example**, not a drop-in library. Its job is
to exercise Kio's whole higher-kinded-type vocabulary end to end against two
arity-1 newtypes acting as *brands*, so you can see every piece — kind-`*→*`
binders, type-constructor application, first-class instance dictionaries,
`do!` pipelines, and `derive!` — running in one small program.

Two companion documents cover the same ground from different angles, and this
case study links to them rather than restating them:

- [`higher-kinded-types.md`](../guides/higher-kinded-types.md)
  is the **language reference** for the HKT machinery — kind annotations,
  type-constructor application, the polymorphic-newtype-payload contract,
  multi-arity type constructors, and the per-backend story. Read it for the
  *why* behind any mechanism this case study merely *uses*.
- [`docs/poc/elab.md`](elab.md) is the **elaborator-POC case study**, where
  `derive!`'s resolution algorithm — including the recursive and polymorphic
  derivations a monad-transformer stack needs — is exercised against `equiv`
  laws. This page uses only the simplest `derive!` shape (a single-target
  pick); the elab POC covers the rest.

What this page adds is the *single-program* view: how all of those pieces
sit together in one package whose `main` runs on every backend.

## What the package contains

The package name is `hkt`. Its `workdir/` is laid out like any worked-example
POC:

- [`hkt.pkg.kio`](../../test-data/poc/hkt/workdir/hkt.pkg.kio) — the package
  file: a `build { ... }` block targeting `js`, `ts`, `python`, `java`,
  `kio-prime`, `rust`, `go`, `swift`, and `haskell`, and a `bridge { ... }`
  block exposing the `testapi` host modules
  (`testapi; testapi/**; elab/testapi;`).
- [`testapi/main.kio`](../../test-data/poc/hkt/workdir/testapi/main.kio) —
  the program: every type constructor, instance dictionary, brand-generic
  operation, and the demonstrative `main`.
- `testapi.kio` and `testapi/{io,text,fmt}.kio` — host-boundary scaffolding
  (`host type` / `host fn` declarations) standing in for the capabilities a
  real host supplies. The program uses three of them: `String`, `print`, and
  `string_concat`. A user replaces this scaffolding with their own host.
- `elab/derive.kio` and `elab/elaborator_util.kio` — the imported `derive!`
  elaborator and its shared compile-time utilities, brought in through the
  `elab` dependency (declared by `elab.dep.kio`, materialized under the
  `elab/` prefix) and imported with `import elab/derive(derive);`. These
  are the subject of the separate [`elab` POC](../guides/using-libraries.md);
  here they are imported and used, not explained.
- `elab/sequence.kio` supplies `do!`, which runs a sequence using an explicit
  bind function. The program imports it with `import elab/sequence(do);`.

Running the package chains `kio check`, `kio test`, the per-backend build, and
the runner; `main` prints five lines:

```text
higher-kinded box ok
higher-kinded identity ok
brand-generic pipeline over box ok
brand-generic pipeline over identity ok
derived functor over identity ok
```

Each line corresponds to one section below.

The snippets in this case study thread together — each picks up where the
previous one left off — and a hidden harness validates them as one growing
program, the same way the POC's `testapi/main` module is one growing module.
The harness supplies the host shape the POC's `testapi` modules declare
(`String`, `print`, `string_concat`) and imports the `derive` and `do` elaborators the
package bridges:

<!--kio {harness=hkt placeholder="__INSERT_CODE_HERE__" accumulate}
bridge {
  kiodoc;
}

module kiodoc;

import derive(derive);
import sequence(do);

host type String role(str);
host fn print(p0: String) -> .;
host fn string_concat(p0: String, p1: String) -> String;

__INSERT_CODE_HERE__
-->

## Brands: two arity-1 newtypes

The package's two type constructors are both **canonical** — each an arity-1
newtype whose payload is exactly its type parameter:

```kio {@hkt}
pub newtype Box[A] : A { pub constructor mk_box; pub projector un_box }

pub newtype Identity[A] : A { pub constructor mk_id; pub projector un_id }
```

`Box[A] : A` and `Identity[A] : A` are the same value-level shape — a thin
wrapper around `A` — but they are **nominally distinct** type constructors.
The POC treats them as *brands*: their job is to carry a type-system
identity, not to change the carrier's bytes. Lifting (`Box.mk_box`) and
projecting (`Box.un_box`) a canonical type constructor are runtime identities
on every backend; the brand lives only in the type system.

Because `Box` and `Identity` differ only by name, they are the cleanest
possible way to show that brand-generic code distinguishes type constructors
structurally — the same operation runs against either without source changes,
and the type checker keeps the two apart.

> The carrier/brand distinction, the kinds of `[A]` and `[*F]`, and what
> "canonical" means are all in
> [`higher-kinded-types.md` § Canonical and non-canonical type
> constructors](../guides/higher-kinded-types.md).

## Instance dictionaries with polymorphic payloads

Four instance newtypes capture the operations the program dispatches over.
Each carries a **polymorphic function** as its payload — the language feature
that makes first-class `Functor(F)` / `Monad(F)` instances possible:

```kio {@hkt}
pub newtype Monad[*F] : [A][B] (F(A) & (A -> F(B))) -> F(B) {
  pub constructor mk_monad;
  pub projector bind
  }

pub newtype Functor[*F] : [A][B] ((A -> B) & F(A)) -> F(B) {
  pub constructor mk_functor;
  pub projector fmap
  }

pub newtype Pure[*F] : [A] A -> F(A) { pub constructor mk_pure; pub projector lift }

pub newtype Extract[*F] : [A] F(A) -> A { pub constructor mk_extract; pub projector peel }
```

The `[*F]` binder is kind-`*→*`: it stands for a one-argument type
constructor, applied as `F(A)` and `F(B)` inside each payload. The payload is
itself polymorphic — `Monad`'s `[A][B]` are bound at construction time inside
the lambda literal that goes into `mk_monad`, and the projector preserves
that polymorphism. So `Monad.bind(monad)` yields a value whose remaining
`[A][B]` polymorphism is intact, and one instance handles every carrier pair.

`Pure(F)` lifts a bare `A` into `F(A)`; `Extract(F)` peels an `F(A)` back to
`A`. These two exist because a brand-*generic* body has no brand-crossing
primitive over an opaque `F` — there is no way to call `Box.mk_box` when all
you know is "some `F`." Generic code threads `Pure(F)` / `Extract(F)`
instances and dispatches through their members instead.

> Why a newtype payload may be a polymorphic function, and how each backend
> stores and projects it, is
> [`higher-kinded-types.md` § Polymorphic newtype payloads](../guides/higher-kinded-types.md)
> and [`specs/backends/README.md` § Polymorphic newtype payloads](../../specs/backends/README.md).

## Concrete instances per brand

Each brand gets its instances built from its own `mk_*` / `un_*` members. The
instance bodies are where the concrete brand crossing happens — `Box` lifts
and projects through `Box.mk_box` / `Box.un_box`, `Identity` through its own:

```kio {@hkt}
fn box_monad() -> Monad(Box) { Monad.mk_monad(.[A][B](m, k) { k(Box.un_box(m)) }) }

fn box_functor() -> Functor(Box) {
  Functor.mk_functor(.[A][B](step, value) { Box.mk_box(step(Box.un_box(value))) })
}

fn box_pure() -> Pure(Box) { Pure.mk_pure(.[A](x) { Box.mk_box(x) }) }

fn box_extract() -> Extract(Box) { Extract.mk_extract(.[A](m) { Box.un_box(m) }) }

fn identity_monad() -> Monad(Identity) { Monad.mk_monad(.[A][B](m, k) { k(Identity.un_id(m)) }) }

fn identity_functor() -> Functor(Identity) {
  Functor.mk_functor(.[A][B](step, value) { Identity.mk_id(step(Identity.un_id(value))) })
}

fn identity_pure() -> Pure(Identity) { Pure.mk_pure(.[A](x) { Identity.mk_id(x) }) }

fn identity_extract() -> Extract(Identity) { Extract.mk_extract(.[A](m) { Identity.un_id(m) }) }
```

The `Monad` and `Functor` payload literals are polymorphic in `[A][B]`,
matching each dictionary's payload shape; `Pure` and `Extract` are
polymorphic in `[A]`. The bodies peel via `un_*`, apply the operation, and
re-wrap via `mk_*` where needed. The `Identity` instances are byte-for-byte
the same shape as the `Box` ones with `mk_id` / `un_id` swapped in — the only
thing that differs between the two brands' instances is which newtype's
members they name.

Note that none of these construction sites spell explicit type arguments —
`Box.mk_box(x)`, `Box.un_box(m)`, `k(...)` — even though the members are
type-parameterized. The carrier types are inferred from the surrounding
context (the instance's declared return type and the lambda's binders). The
POC leans on inference everywhere it can, which is what a user would
naturally write.

## Concrete brand crossing: `rebrand`

The first two output lines come from round-tripping a value through a brand:
build the branded value with the newtype's own constructor, peel and re-lift
through threaded instances, then project back out. The brand-generic part is
`rebrand`, which is parametric over `[*F]` — it works against any brand whose
`Pure` and `Extract` instances you hand it:

```kio {@hkt}
// `[*F]`-generic: there is no brand-crossing primitive over an opaque
// `F`, so the body threads the two instances and dispatches through their
// members. Peel `x : F(A)` down to `A`, then re-lift back up to `F(A)`.
fn rebrand[*F][A](pur: Pure(F), ext: Extract(F), x: F(A)) -> F(A) {
  Pure.lift(pur)(Extract.peel(ext)(x))
}
```

`Extract.peel(ext)` projects the `Extract(F)` dictionary to its underlying
`F(A) -> A` function; `Pure.lift(pur)` projects `Pure(F)` to `A -> F(A)`.
Composing them is the type-constructor-generic identity on `F(A)`. The body
never names `Box` or `Identity` — it cannot, since `F` is abstract — so the
same compiled body serves both brands.

The two drivers pick a brand at the call site, build the branded value with
the *concrete* newtype's constructor, and supply that brand's instances:

```kio {@hkt}
fn run_box(s: String) -> String {
  let branded = Box.mk_box(s);
  let rebranded = rebrand(box_pure(), box_extract(), branded);
  Box.un_box(rebranded)
}

fn run_identity(s: String) -> String {
  let branded = Identity.mk_id(s);
  let rebranded = rebrand(identity_pure(), identity_extract(), branded);
  Identity.un_id(rebranded)
}
```

`run_box` and `run_identity` differ only in which brand and instances they
name — the `rebrand` call in the middle is identical. This is the headline
point of the concrete-crossing half of the POC: a `[*F]`-generic operation
applied at two distinct brands.

## Abstract brand crossing in a `do!` block: `pipeline`

The next two lines exercise the same generic abstraction in a richer body.
`pipeline` is `[*F]`-generic over a `Monad(F)` dictionary, a `Pure(F)`
dictionary, and a branded seed; it threads two bind steps interleaved with a
pure `let`, all via imported `do!` with the *projected* dictionary's `bind`:

```kio {@hkt}
fn pipeline[*F](monad: Monad(F), pur: Pure(F), seed: F(String)) -> F(String) {
  do! Monad.bind(monad) {
    let prefix <- seed;
    let middle = " ok"(String);
    let trailed <- Pure.lift(pur)(string_concat(prefix, middle));
    Pure.lift(pur)(string_concat(trailed, "\n"(String)))
  }
}
```

`Monad.bind(monad)` projects the `Monad(F)` instance to a value whose
`[A][B]` polymorphism survives — that is what makes it a valid bind function
for `do!` against an abstract `F`. Inside the block:

- `let prefix <- seed;` is a bind step: it unwraps `seed : F(String)` and
  names the carrier `prefix : String`.
- `let middle = " ok"(String);` is a pure let — no bind, just a local
  binding interleaved between steps.
- `let trailed <- Pure.lift(pur)(...);` lifts the concatenated string back
  into `F(String)` (via the threaded `Pure` instance, since `F` is abstract)
  and binds the next step.
- the final expression is the block's overall `F(String)` result.

The `do!` library applies its projected sequence to `Monad.bind(monad)`.
The sequence contains nested bind calls; the supplied bind expression is
evaluated exactly once, including when no bind step uses it. Because the whole
thing is `[*F]`-generic and lifts through
`Pure.lift(pur)` rather than any concrete constructor, the same source runs
against either brand:

```kio {@hkt}
fn run_box_pipeline(s: String) -> String {
  let monad = box_monad();
  let seed = Box.mk_box(s);
  Box.un_box(pipeline(monad, box_pure(), seed))
}

fn run_identity_pipeline(s: String) -> String {
  let monad = identity_monad();
  let seed = Identity.mk_id(s);
  Identity.un_id(pipeline(monad, identity_pure(), seed))
}
```

Each driver builds the brand's monad and `Pure` instances, seeds the value
with the concrete constructor, runs the shared `pipeline`, and projects the
carrier back out. The seed string differs only so the output lines read
distinctly; the `pipeline` call is identical between the two.

> Sequence projection and the bind-function contract are specified in
> [`specs/language.md` § Monadic `do` blocks](../../specs/language.md#monadic-do-blocks);
> the abstract-vs-concrete lift/project distinction is
> [`higher-kinded-types.md` § Abstract type constructors and generic
> code](../guides/higher-kinded-types.md#abstract-type-constructors-and-generic-code).

## Deriving an instance with `derive!`

The last line shows that an instance need not be hand-written at all. The
[`derive!`](../guides/using-libraries.md) elaborator — imported
from the package's `derive` module — constructs a value of a requested target
type from a tuple of candidate rules, finding the one way they fit. The
harness imports it the way the POC does (in the POC the module path is
`elab/derive`):

```text
import derive(derive);
```

The POC's use is the simplest possible derivation: pick the right base
instance out of a candidate tuple. The target is elided as `_` because the
function's return type pins it, and the candidate tuple lists both brands'
functor builders:

```kio {@hkt}
// Target elided as `_`; candidate tuple lists both functor builders.
// `derive!` unifies each candidate's result type against the goal
// `Functor(Identity)` and finds exactly one that fits — `identity_functor`.
fn derived_identity_functor() -> Functor(Identity) { derive!(_, (box_functor, identity_functor)) }
```

The candidate tuple is the value argument; the target type is the trailing
slot and may be elided to `_` when the enclosing function's return type pins
it. Because `_` marks the type slot rather than a value, it may sit ahead of
the candidate tuple, as it does here. `derive!` reads `Functor(Identity)`
from the return type, then unifies each candidate's result type against that
goal: `box_functor` produces `Functor(Box)` (no match), `identity_functor`
produces `Functor(Identity)` (match). Exactly one candidate fits, so resolution is unambiguous and the
form elaborates to a plain call to `identity_functor()`. `derive!` is pure
surface convenience — it writes the call you would otherwise write by hand
and leaves no trace in Kio'.

This single-target pick is the smallest case of `derive!`. Its motivating use
— composing a monad-transformer stack from base instances plus per-transformer
rules — is exercised in the elaborator-POC case study
[`docs/poc/elab.md`](elab.md) (which drives `derive!`'s recursive and
polymorphic resolution against `equiv` laws) and specified in
[`specs/language.md` § The `derive!` elaborator](../../specs/language.md#the-derive-elaborator).

The derived instance is used exactly like a hand-written one — project it
with `Functor.fmap`, apply the mapping function over a branded value, and
project the carrier back out:

```kio {@hkt}
fn run_derived_functor(s: String) -> String {
  let functor = derived_identity_functor();
  let branded = Identity.mk_id(s);
  let mapped =
    Functor.fmap(functor)(
      , String
      , String
      , .(value) { string_concat(value, " ok\n"(String)) }
      , branded
      );
  Identity.un_id(mapped)
}
```

`Functor.fmap(functor)` projects the dictionary to its `fmap` value; the
following call supplies the two carrier types (`String`, `String`), the
mapping function, and the branded value. The leading-comma positional layout
is Kio's multi-argument-call style.

## The demonstrative `main`

`main` is the package's only `pub fn`. It runs the five drivers in order and
prints each result, matching the five output lines verbatim:

```kio {@hkt}
pub fn main() -> . {
  print(run_box("higher-kinded box ok\n"(String)));
  print(run_identity("higher-kinded identity ok\n"(String)));
  print(run_box_pipeline("brand-generic pipeline over box"(String)));
  print(run_identity_pipeline("brand-generic pipeline over identity"(String)));
  print(run_derived_functor("derived functor over identity"(String)))
}
```

The `rebrand` drivers carry their own `" ok\n"` suffix in the seed string,
while the `pipeline` and derived-functor drivers append `" ok\n"` inside the
program, which is why the `main`-level string literals for those three carry
no trailing newline. The print order is the source order, and it is exactly
the five-line output the package snapshots.

## On laws

The optics POC ships `equiv` blocks that discharge its lens / prism / iso
laws, and [a faithful read-through of that package](optics.md) devotes a whole
section to them. The `hkt` package carries **no `equiv` blocks**: it is a
surface-and-execution demonstration, not an algebraic library, so there is no
behavioral law for `kio test` to discharge here. The functor and monad laws
that *would* apply to these instances (identity, composition, left/right
identity, associativity) are the standard ones; this POC demonstrates the
*surface* that would carry them, not the law discharge. For how `equiv` laws
are written and run over an HKT-flavored library, see
[Testing with `equiv`](../guides/equiv.md).

## Adopting the package

To reuse the HKT pieces in your own package:

1. Copy the instance newtypes you need (`Monad`, `Functor`, `Pure`,
   `Extract`) and any brands (`Box`, `Identity`, or your own arity-1
   newtypes) into a module of your package.
2. Supply a host that provides `String`, `print`, and `string_concat` (or
   adjust the program to your host's capabilities), replacing the `testapi`
   scaffolding.
3. To use `derive!`, depend on the
   [`elab` POC](../guides/using-libraries.md), which carries the
   `derive` elaborator, and import it with `import elab/derive(derive);`.
   Declaring a dependency is documented once in
   [`pkg.md` § Dependency files](../guides/pkg.md#dependency-files).

The brand-generic operations (`rebrand`, `pipeline`) and the `derive!` call
work against any arity-1 newtype without source changes, so adding a third
brand is a matter of writing its four instance builders and pointing the
drivers at them.

## See also

- [`higher-kinded-types.md`](../guides/higher-kinded-types.md)
  — the HKT language reference (kinds, application, polymorphic payloads,
  multi-arity, per-backend codegen).
- [The elaborator POC](elab.md) — the case study where `derive!`'s resolution
  (including recursive and polymorphic derivations) is exercised per shape
  against `equiv` laws.
- [`using-libraries.md`](../guides/using-libraries.md)
  — the map of the `elab` POC modules, including the `derive` elaborator.
- [The optics library case study](optics.md) — the companion POC built from
  function pairs and spine elaborators.
- [`specs/language.md` § The `derive!` elaborator](../../specs/language.md#the-derive-elaborator)
  — the `derive!` surface contract.
- [`test-data/poc/hkt/`](../../test-data/poc/hkt/) — the full worked package
  this case study reads through.
