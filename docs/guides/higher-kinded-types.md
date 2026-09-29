# Higher-kinded types

Kio supports **higher-kinded types** through kind-annotated binders and direct type application. A binder annotated kind-`*→*` stands for a **type constructor**, and `F(A)` applies that type constructor to a carrier type `A`, all inside System F's rank-N discipline.

## Type constructors (kind `*→*`)

A **type constructor** in Kio is a type that takes a type argument to produce a type — kind `*→*`. It's the same shape as Haskell's `Box a` or OCaml's `'a Box`: an arity-1 newtype (`Box[A] : A`) is a kind-`*→*` type constructor, and `Box(A)` (or `F(A)` for an abstract type constructor `F`) is the type-level application "the type constructor applied to the carrier `A`." (The Rust backend erases these into its `Rc<dyn Any>` body — see [`specs/backends/rust.md`](../../specs/backends/rust.md) § Higher-kinded types — but that is a backend-encoding detail; at the language level there is just the kind-annotated binder and direct application.)

Properties of type constructors in Kio:

- **Kinds are explicit.** A binder's kind is written, never inferred. `[A]` is the default kind `*` (an ordinary type); `[*F]` is kind `*→*` (a one-argument type constructor); `[**G]` is kind `*→*→*`. The visual star count equals the number of arrows in the kind. Applying a type constructor, `F(A)`, is admitted only when `F`'s kind is `*→*` (or higher) — applying a kind-`*` binder is a kind error.
- **Complete polymorphic function schemes are structural values.** One or more type-binder layers ending in a function type form a complete scheme of kind `*`, even when a binder is higher-kinded. Such a scheme may be named or anonymous and may appear as a parameter or return, inside a product or sum, in a type argument or annotation, or inside a recursive or existential newtype payload. Transparent aliases preserve that classification. A higher-kinded binder whose body does not ultimately reach a function remains incomplete and is rejected; newtype existential binders themselves remain kind `*`.
- **Values use completed types.** A value parameter, return type, newtype payload, annotation, product member, or sum member must have kind `*`. For `[*F]` and `[*G]`, write `F(A) & G(A)` or `F(A) | G(A)` for value types; bare `F`, `F & G`, and `F | G` are still type constructors and are rejected there. A bare constructor is useful only when a declaration explicitly asks for one, as `Monad(F)` does.
- **Alias application is exact-arity.** A reference to a parametric transparent alias supplies every parameter declared by that alias. `type Pair[A][B] = A & B;` admits `Pair(String, String)`, but bare `Pair` and undersaturated `Pair(String)` are type errors; neither spelling may retain the alias declaration's unfilled binders. Exact application can still produce a constructor when the expanded body does: with `type Unary[E] = Either(E);`, `Unary(String)` has kind `*→*`.
- **Type-level application.** `Box(A)` names an application of `Box` to `A`; it does not itself construct a value. Backends may erase the wrapper inside compiled Kio code, while public host interfaces can use distinct wrappers and conversions. See the [host integration guides](../README.md#host-integrations) for the representation a host uses.
- **Lifted and projected by the newtype's own members.** Lifting a value into a type constructor and projecting it back out go through the newtype's declared constructor / projector — `Box.mk_box` / `Box.un_box`. A canonical constructor's payload is the carrier value itself; a non-canonical constructor has a different payload shape.

The kind-`*→*` binder `[*F]` in a function signature can stay abstract — code parametric over `[*F]` works against any concrete type constructor. This is how generic combinators (`sequence`, `traverse`, …) abstract over the choice of type constructor without committing to a specific one.

This guide is the **language reference** for the HKT machinery — kind annotations, how type-constructor application works, the polymorphic newtype payload contract, the multi-arity story. For a worked, executable walkthrough that builds first-class `Functor` / `Monad` instances over arity-1 brands, consumes them with imported `do!` sequencing, and derives one with `derive!`, see the case study [`docs/poc/hkt.md`](../poc/hkt.md).

Assumed setup: a module whose host supplies `String`, `print`, and `string_concat`.

<!--kio {harness=module placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import sequence(do);

host type String role(str);
host fn print(p0: String) -> .;
host fn string_concat(p0: String, p1: String) -> String;

__INSERT_CODE_HERE__
-->

## Kind annotations

A binder's kind is the count of leading stars on its name:

| Binder | Kind | Meaning |
| --- | --- | --- |
| `[A]` | `*` | an ordinary type |
| `[*F]` | `*→*` | a one-argument type constructor |
| `[**G]` | `*→*→*` | a two-argument type constructor |

Kinds are part of a type's public signature. A `Functor` dictionary that abstracts over an arity-1 type constructor quantifies over a kind-`*→*` binder:

```kio {@module}
pub newtype Functor[*F] : [A][B] ((A -> B) & F(A)) -> F(B) {
  pub constructor mk_functor;
  pub projector fmap
  }
```

The `[*F]` binder is applied as `F(A)` and `F(B)` inside the payload. A newtype's own arity comes from its declared parameters: `Box[A]` is kind `*→*`, so writing `Box` where a `[*F]` type constructor is expected typechecks, and `Box(String)` is an ordinary kind-`*` type.

## Canonical and non-canonical type constructors

Any arity-1 newtype is a kind-`*→*` type constructor. Its payload determines what the constructor accepts and the projector returns.

**Canonical type constructors** have a payload that is exactly the type-parameter:

```kio {@module}
pub newtype Box[A] : A { pub constructor mk_box; pub projector un_box }
```

`Box[A] : A` declares a distinct type whose payload is exactly `A`. Constructing a `Box(A)` and projecting it returns the original value; this semantic identity does not promise identical host representations. The Identity wrapper `Identity[A] : A` has the same shape.

**Non-canonical type constructors** have an arbitrary arity-1 payload:

```kio {@module}
pub newtype Maybe[A] : . | A { pub constructor mk_maybe; pub projector un_maybe }

pub rec newtype List[A] : . | (A & List(A)) { pub constructor cons; pub projector un_list }
```

`Maybe` and `List` are both kind-`*→*` type constructors, so a `Functor[*F]` dictionary can be instantiated for either. Their payloads are not the bare type-parameter, so lifting and projecting wrap / unwrap the payload shape — but that is still spelled with the newtype's own `mk_*` / `un_*` members.

## Lifting and projecting values

For a canonical type constructor, the constructor lifts a value into the wrapper and the projector projects it back out; the composition is the identity on the carrier. Round-trip a value through a `Box`:

```kio {@module}
pub newtype Box[A] : A { pub constructor mk_box; pub projector un_box }

fn roundtrip[A](x: A) -> A { Box.un_box(A, Box.mk_box(A, x)) }
```

`Box.mk_box(A, x)` lifts `x : A` to `Box(A)`; `Box.un_box(A, …)` projects it back to `A`. Both members take the carrier type as a leading type-argument. The round-trip returns `x`; its execution cost depends on the backend and whether the value crosses a host interface.

```kio {@module}
pub newtype Box[A] : A { pub constructor mk_box; pub projector un_box }

fn lift[A](x: A) -> Box(A) { Box.mk_box(A, x) }

fn peel[A](x: Box(A)) -> A { Box.un_box(A, x) }
```

## Abstract type constructors and generic code

The kind-`*→*` binder `[*F]` can stay abstract — code that takes a `[*F]` and a value of `F(A)` works against any concrete type constructor. There is no lift/project primitive over an opaque `F`, though: a type-constructor-generic body cannot call a specific newtype's `mk_*` / `un_*`. To lift into or project out of an abstract type constructor, thread an instance dictionary that carries the operation and dispatch through its member. A `Pure(F)` dictionary supplies the lift:

```kio {@module}
pub newtype Pure[*F] : [A] A -> F(A) { pub constructor mk_pure; pub projector lift }

// `wrap` is generic over the type constructor: it lifts `x` into
// whatever `F` the threaded `Pure(F)` instance describes.
fn wrap[*F][A](pur: Pure(F), x: A) -> F(A) { Pure.lift(F, pur)(A, x) }
```

A caller builds the instance from a concrete type constructor's constructor and picks the type constructor at the call site:

```kio {@module}
pub newtype Box[A] : A { pub constructor mk_box; pub projector un_box }

pub newtype Pure[*F] : [A] A -> F(A) { pub constructor mk_pure; pub projector lift }

fn wrap[*F][A](pur: Pure(F), x: A) -> F(A) { Pure.lift(F, pur)(A, x) }

fn box_pure() -> Pure(Box) { Pure.mk_pure(Box, .[A](x) { Box.mk_box(A, x) }) }

pub fn main() -> . {
  let v = wrap(Box, String, box_pure(), "hello\n"(String));
  print(Box.un_box(String, v))
}
```

## Polymorphic newtype payloads — the instance-newtype pattern

A newtype payload may be a polymorphic function — for example,
`Forall(A, Forall(B, Function(args, ret)))`. This is the language feature that
underwrites first-class `Functor(F)` / `Monad(F)` instances: the instance
newtype carries the operation as a polymorphic-fn field that callers project
to invoke at a specific binding.

```kio {@module}
pub newtype Box[A] : A { pub constructor mk_box; pub projector un_box }

pub newtype Monad[*F] : [A][B] (F(A) & (A -> F(B))) -> F(B) {
  pub constructor mk_monad;
  pub projector bind
  }
```

`Monad[*F]`'s payload is a polymorphic function: `[A][B]` are bound at construction time inside the lambda literal that goes into `mk_monad`; the projector preserves the polymorphism. `Monad.bind(F, monad)` yields a value whose remaining `[A][B]` polymorphism is intact, so the same instance handles every payload pair.

The polymorphic-fn payload contract (the constructor accepts any value of the
declared polymorphic function type, and the projector preserves its binders) is
specified at [`specs/backends/README.md` § Polymorphic newtype payloads](../../specs/backends/README.md).
Each backend uses its ordinary newtype representation at that boundary; Rust's
body representation is already erased, so projection needs no HKT-specific
up/down-cast adapter (see [`specs/backends/rust.md`](../../specs/backends/rust.md)
§ Polymorphic newtype payloads).

Once a value of `Monad(F)` (or `Functor(F)`, `Applicative(F)`, …) is in hand,
callers project it with `Monad.bind(monad)` and use the result as a receiver —
including as a `do!` receiver. The full walkthrough — defining concrete
instances, `do!` sequencing, and combinators generic over the type constructor
— is the case study [`docs/poc/hkt.md`](../poc/hkt.md).

## Monadic `do!`

A monadic `do!` block supplies the bind operation it will sequence with. The
receiver must have the shape `[A][B] (F(A) & (A -> F(B))) -> F(B)`. It is an
ordinary function value, so the projected operation from a first-class
`Monad(F)` dictionary works directly:

```kio {@module}
pub newtype Monad[*F] : [A][B] (F(A) & (A -> F(B))) -> F(B) {
  pub constructor mk_monad;
  pub projector bind
  }

pub newtype Pure[*F] : [A] A -> F(A) { pub constructor mk_pure; pub projector lift }

fn pipeline[*F](monad: Monad(F), pur: Pure(F), seed: F(String)) -> F(String) {
  do! Monad.bind(monad) {
    let prefix <- seed;
    let suffix = "!";
    let combined <- Pure.lift(pur)(string_concat(prefix, suffix));
    Pure.lift(pur)(string_concat(combined, "\n"))
  }
}
```

`let prefix <- seed; rest` becomes
`Monad.bind(monad)(seed, .(prefix) { rest })`. The later bind is folded the
same way. By contrast, `let suffix = "!";` is a normal lexical `let`; it does
not invoke bind. A unit-discarding expression statement becomes a bind whose
continuation ignores the unit payload, and the final expression must already
produce `F(_)`.

The block carries no ambient `pure` operation. Lifting is explicit through
`Pure.lift(pur)`, which is why the generic body threads both dictionaries. The
receiver's declared shape connects each `F(A)` expression to its continuation
parameter type; the sequence becomes ordinary nested bind calls, and the `do!`
call is gone before Kio'. The supplied receiver expression is evaluated exactly
once, even when the block contains no bind steps; each step uses that same
bind value. Its carrier must be determined by ordinary call and header
information, not inferred from the block body. For an ordinary scoped
computation, use the distinct
imported `scope! { ... }` form, which has no monadic receiver or `<-` statements.

## Multi-arity type constructors

A newtype of arity ≥ 2 is a higher-kinded type constructor of the matching kind. `Either[E][A] : E | A` is kind `*→*→*`; a `Bifunctor` dictionary quantifies over a kind-`*→*→*` binder `[**F]` and applies it with two arguments, `F(A, C)`:

```kio {@module}
pub newtype Either[E][A] : E | A { pub constructor mk_either; pub projector un_either }

pub newtype Bifunctor[**F] : [A][B][C][D] ((A -> B) & (C -> D) & F(A, C)) -> F(B, D) {
  pub constructor mk_bifunctor;
  pub projector bimap
  }
```

**Partial application.** Filling some of a multi-arity newtype's arguments yields a type constructor of lower kind by *kind arithmetic*: `Either : *→*→*`, `String : *`, so `Either(String) : *→*`. A kind-`*→*` slot — like `Monad`'s `[*F]` — accepts `Either(String)` directly. This does not permit a transparent alias reference to omit any parameter declared by that alias.

<!--kio {harness=partial_application placeholder="__SNIPPET__"}
bridge {
  kiodoc;
}

module kiodoc;

host type String role(str);

pub newtype Either[E][A] : E | A { pub constructor mk_either; pub projector un_either }
pub newtype Monad[*F] : [A][B](F(A) & (A -> F(B))) -> F(B) { pub constructor mk_monad; pub projector bind }

__SNIPPET__
-->

```kio {@partial_application}
// Either(String) is a kind-*→* type constructor: "Either with the
// error type fixed to String". It slots into Monad's [*F] parameter.
fn keep_either_string_monad(m: Monad(Either(String))) -> Monad(Either(String)) { m }
```

Partial application drops *trailing* arguments only — `Either(String)` fixes the first slot and leaves the second to fill. Fixing a non-trailing slot would need an anonymous type-level function, which Kio does not have; declare a wrapper newtype that swaps the argument order if you need it.

## Language boundaries

- **No anonymous type-level functions.** Partial application is trailing-slot-drop only; there is no `λα. T`. Fixing a non-trailing slot uses a wrapper newtype.
- **No kind polymorphism or higher-order kinds.** Kinds are concrete right-associative chains over `*`; a binder cannot itself be kind-polymorphic, and `(*→*)→*` is not expressible.
- **Manual congruence walks.** A newtype's `mk_*` / `un_*` lift and project at a single position. Structurally lifting / projecting the type constructor through `->` / `&` / `|` constructors — equating, say, `F(A) -> F(B)` with `A -> B` — requires writing the walk by hand at each leaf, with variance handled the standard way (covariant for `->`'s codomain and both sides of `&` / `|`; contravariant for `->`'s domain).
- **Per-backend instance support.** The instance-newtype pattern (polymorphic-fn value at a newtype payload position) requires each backend to support polymorphic-function values across the storage / projection boundary. JS handles this through its ordinary dynamic representation; Rust uses its ordinary erased body representation and needs no HKT-specific adapter at projection — see [`specs/backends/README.md` § Polymorphic newtype payloads](../../specs/backends/README.md#polymorphic-newtype-payloads) and [`specs/backends/rust.md` § Polymorphic newtype payloads](../../specs/backends/rust.md#polymorphic-newtype-payloads).
