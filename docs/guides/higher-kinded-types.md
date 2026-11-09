# Higher-kinded types

Higher-kinded types let one function work with several kinds of container,
such as a box or an optional value. Instead of taking just an element type
`A`, it takes a **type constructor** `F`: a type that accepts another type.
`F[A]` then means the container type for elements of type `A`.

## Type constructors (kind `*→*`)

A **kind** describes which type arguments a type accepts. An ordinary type
has kind `*`; a constructor that accepts one ordinary type has kind `*→*`.
For example, a nominal entry `type { Box[A]: A; }` takes one type argument, so
`Box` has kind `*→*` and `Box[A]` is an ordinary type. The same application
syntax works for an abstract constructor `F`, written `F[A]`.

Properties of type constructors in Kio:

- **Kinds are explicit.** A binder's kind is written, never inferred. `[A]` is the default kind `*` (an ordinary type); `[*F]` is kind `*→*` (a one-argument type constructor); `[**G]` is kind `*→*→*`. The visual star count equals the number of arrows in the kind. Applying a type constructor, `F[A]`, is admitted only when `F`'s kind is `*→*` (or higher) — applying a kind-`*` binder is a kind error.
- **Complete polymorphic function schemes are structural values.** One or more type-binder layers ending in a function type form a complete scheme of kind `*`, even when a binder is higher-kinded. Such a scheme may be named or anonymous and may appear as a parameter or return, inside a product or sum, in a type argument or annotation, or inside a recursive or existential nominal payload. Transparent aliases preserve that classification. A higher-kinded binder whose body does not ultimately reach a function remains incomplete and is rejected; nominal existential binders themselves remain kind `*`.
- **Values use completed types.** A value parameter, return type, nominal payload, annotation, product member, or sum member must have kind `*`. For `[*F]` and `[*G]`, write `F[A] & G[A]` or `F[A] | G[A]` for value types; bare `F`, `F & G`, and `F | G` are still type constructors and are rejected there. A bare constructor is useful only when a declaration explicitly asks for one, as `Monad[F]` does.
- **Alias application is exact-arity.** A reference to a parametric transparent alias supplies every parameter declared by that alias. `type Pair[A][B] = A & B;` admits `Pair[String][ String]`, but bare `Pair` and undersaturated `Pair[String]` are type errors; neither spelling may retain the alias declaration's unfilled binders. Exact application can still produce a constructor when the expanded body does: with `type Unary[E] = Either[E];`, `Unary[String]` has kind `*→*`.
- **Type-level application.** `Box[A]` names an application of `Box` to `A`; it does not itself construct a value. Backends may erase the wrapper inside compiled Kio code, while public host interfaces can use distinct wrappers and conversions. See the [host integration guides](../README.md#host-integrations) for the representation a host uses.
- **Lifted and projected by the nominal type's own members.** Lifting a value into a type constructor and projecting it back out go through the nominal type's declared constructor / projector — `Box.mk` / `Box.get`. A canonical constructor's payload is the carrier value itself; a non-canonical constructor has a different payload shape.

The kind-`*→*` binder `[*F]` in a function signature can stay abstract — code parametric over `[*F]` works against any concrete type constructor. This is how generic combinators (`sequence`, `traverse`, …) abstract over the choice of type constructor without committing to a specific one.

This guide explains the HKT machinery — kind annotations, type-constructor application, polymorphic nominal payloads, and multiple type arguments. The [language reference](../../specs/language.md#higher-kinded-types) defines the precise rules. For a worked, executable walkthrough that builds first-class `Functor` / `Monad` instances over arity-1 brands, consumes them with imported `do` sequencing, and derives one with `derive`, see the case study [`docs/poc/hkt.md`](../poc/hkt.md).

Assumed setup: a module whose host supplies `String` with `role(str)`,
`print: String -> .`, and `string_concat: (String & String) -> String`.
Concrete local context determines ordinary string literals; annotate a
standalone initializer such as `let suffix = "!"[String];` before its
binding boundary. See [literal inference](typechecking.md#literal-types-and-local-inference).

<!--kio {harness=module placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import sequence(macro do);

host type String role(str);
host fn print(p0: String);
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
pub type { Functor[*F]: [A][B] ((A -> B) & F[A]) -> F[B] }
```

The `[*F]` binder is applied as `F[A]` and `F[B]` inside the payload. A nominal type's own arity comes from its declared parameters: `Box[A]` is kind `*→*`, so writing `Box` where a `[*F]` type constructor is expected typechecks, and `Box[String]` is an ordinary kind-`*` type.

## Canonical and non-canonical type constructors

Any arity-1 nominal type is a kind-`*→*` type constructor. Its payload determines what the constructor accepts and the projector returns.

**Canonical type constructors** have a payload that is exactly the type-parameter:

```kio {@module}
pub type { Box[A]: A }
```

`type { Box[A]: A; }` declares a distinct type whose payload is exactly `A`. Constructing a `Box[A]` and projecting it returns the original value; this semantic identity does not promise identical host representations. The Identity wrapper `type { Identity[A]: A; }` has the same shape.

**Non-canonical type constructors** have an arbitrary arity-1 payload:

```kio {@module}
pub type { Maybe[A]: . | A }

pub rec type { List[A]: . | (A & List[A]) }
```

`Maybe` and `List` are both kind-`*→*` type constructors, so a `Functor[*F]` dictionary can be instantiated for either. Their payloads are not the bare type-parameter, so lifting and projecting wrap / unwrap the payload shape — but that is still spelled with the nominal type's own `mk` / `get` members.

## Lifting and projecting values

For a canonical type constructor, the constructor lifts a value into the wrapper and the projector projects it back out; the composition is the identity on the carrier. Round-trip a value through a `Box`:

```kio {@module}
pub type { Box[A]: A }

fn roundtrip[A](x: A) -> A { Box.get[A](Box.mk[A](x)) }
```

`Box.mk[A](x)` lifts `x : A` to `Box[A]`; `Box.get[A](…)` projects it back to `A`. Both members take the carrier type as a leading type-argument. The round-trip returns `x`; its execution cost depends on the backend and whether the value crosses a host interface.

```kio {@module}
pub type { Box[A]: A }

fn lift[A](x: A) -> Box[A] { Box.mk[A](x) }

fn peel[A](x: Box[A]) -> A { Box.get[A](x) }
```

## Abstract type constructors and generic code

The kind-`*→*` binder `[*F]` can stay abstract — code that takes a `[*F]` and a value of `F[A]` works against any concrete type constructor. There is no lift/project primitive over an opaque `F`, though: a type-constructor-generic body cannot call a specific nominal type's `mk` / `get`. To lift into or project out of an abstract type constructor, thread an instance dictionary that carries the operation and dispatch through its member. A `Pure[F]` dictionary supplies the lift:

```kio {@module}
pub type { Pure[*F]: [A] A -> F[A] }

// `wrap` is generic over the type constructor: it lifts `x` into
// whatever `F` the threaded `Pure[F]` instance describes.
fn wrap[*F][A](pur: Pure[F], x: A) -> F[A] { Pure.get[F](pur)[A](x) }
```

A caller builds the instance from a concrete type constructor's constructor and picks the type constructor at the call site:

```kio {@module}
pub type { Box[A]: A }

pub type { Pure[*F]: [A] A -> F[A] }

fn wrap[*F][A](pur: Pure[F], x: A) -> F[A] { Pure.get[F](pur)[A](x) }

fn box_pure() -> Pure[Box] {
  Pure.mk[Box](.[A](x) { Box.mk[A](x) })
}

pub fn main() {
  let v = wrap[Box][String](box_pure(), "hello\n"[String]);
  print(Box.get[String](v))
}
```

## Polymorphic nominal payloads — the dictionary pattern

A nominal payload may be a polymorphic function, such as
`[A][B] (F[A] & (A -> F[B])) -> F[B]`. This lets a `Functor[F]` or `Monad[F]`
value store one operation that works for every element type. A caller
projects that operation, then supplies the types and values for a particular
call.

```kio {@module}
pub type { Box[A]: A }

pub type { Monad[*F]: [A][B] (F[A] & (A -> F[B])) -> F[B] }
```

`Monad[*F]`'s payload is a polymorphic function: `[A][B]` are bound at construction time inside the lambda literal that goes into `Monad.mk`; the projector preserves the polymorphism. `Monad.get[F](monad)` yields a value whose remaining `[A][B]` polymorphism is intact, so the same instance handles every payload pair.

The polymorphic-fn payload contract (the constructor accepts any value of the
declared polymorphic function type, and the projector preserves its binders) is
shared by all backends. The [host integration guides](../README.md#host-integrations)
explain how a host constructs and uses such values.

Once a value of `Monad[F]` (or `Functor[F]`, `Applicative[F]`, …) is in hand,
callers project it with `Monad.get(monad)` and use the result as a receiver —
including as a `do` receiver. The full walkthrough — defining concrete
instances, `do` sequencing, and combinators generic over the type constructor
— is the case study [`docs/poc/hkt.md`](../poc/hkt.md).

## Monadic `do`

`do` comes from the [macro library](using-libraries.md#the-macro-library),
not a keyword. After adopting that dependency under the name `macros`, import
it with `import macros/sequence(macro do);`.

A monadic `do` block supplies the bind operation it will sequence with. The
receiver must have the shape `[A][B] (F[A] & (A -> F[B])) -> F[B]`. It is an
ordinary function value, so the projected operation from a first-class
`Monad[F]` dictionary works directly:

```kio {@module}
pub type { Monad[*F]: [A][B] (F[A] & (A -> F[B])) -> F[B] }

pub type { Pure[*F]: [A] A -> F[A] }

fn pipeline[*F](monad: Monad[F], pur: Pure[F], seed: F[String]) -> F[String] {
  do(Monad.get(monad)) {
    let prefix <- seed;
    let suffix = "!"[String];
    let combined <- Pure.get(pur)(string_concat(prefix, suffix));
    Pure.get(pur)(string_concat(combined, "\n"))
  }
}
```

`let prefix <- seed; rest` becomes
`Monad.get(monad)(seed, .(prefix) { rest })`. The later bind is folded the
same way. By contrast, `let suffix = "!"[String];` is a normal lexical `let`; it does
not invoke bind. A unit-discarding expression statement becomes a bind whose
continuation ignores the unit payload, and the final expression must already
produce `F[_]`.

The block carries no ambient `pure` operation. Lifting is explicit through
`Pure.get(pur)`, which is why the generic body threads both dictionaries. The
receiver's declared shape connects each `F[A]` expression to its continuation
parameter type; the sequence becomes ordinary nested bind calls, and the `do`
call is gone before Kio'. The supplied receiver expression is evaluated exactly
once, even when the block contains no bind steps; each step uses that same
bind value. Its carrier must be determined by ordinary call and header
information, not inferred from the block body. For an ordinary scoped
computation, use the distinct
imported `scope { ... }` form, which has no monadic receiver or `<-` statements.

## Multi-arity type constructors

A nominal type of arity ≥ 2 is a higher-kinded type constructor of the matching kind. `type { Either[E][A]: E | A; }` is kind `*→*→*`; a `Bifunctor` dictionary quantifies over a kind-`*→*→*` binder `[**F]` and applies it with two arguments, `F[A][ C]`:

```kio {@module}
pub type { Either[E][A]: E | A }

pub type { Bifunctor[**F]: [A][B][C][D] ((A -> B) & (C -> D) & F[A][C]) -> F[B][D] }
```

**Partial application.** Filling some of a multi-arity nominal type's arguments yields a type constructor of lower kind by *kind arithmetic*: `Either : *→*→*`, `String : *`, so `Either[String] : *→*`. A kind-`*→*` slot — like `Monad`'s `[*F]` — accepts `Either[String]` directly. This does not permit a transparent alias reference to omit any parameter declared by that alias.

<!--kio {harness=partial_application placeholder="__SNIPPET__"}
bridge {
  kiodoc;
}

module kiodoc;

host type String role(str);

pub type { Either[E][A]: E | A; }
pub type { Monad[*F]: [A][B] (F[A] & (A -> F[B])) -> F[B]; }

__SNIPPET__
-->

```kio {@partial_application}
// Either[String] is a kind-*→* type constructor: "Either with the
// error type fixed to String". It slots into Monad's [*F] parameter.
fn keep_either_string_monad(m: Monad[Either[String]]) -> Monad[Either[String]] { m }
```

Partial application drops *trailing* arguments only — `Either[String]` fixes the first slot and leaves the second to fill. Fixing a non-trailing slot would need an anonymous type-level function, which Kio does not have; declare a nominal wrapper that swaps the argument order if you need it.

## Language boundaries

- **No anonymous type-level functions.** Partial application is trailing-slot-drop only; there is no `λα. T`. Fixing a non-trailing slot uses a nominal wrapper.
- **No kind polymorphism or higher-order kinds.** Kinds are concrete right-associative chains over `*`; a binder cannot itself be kind-polymorphic, and `(*→*)→*` is not expressible.
- **Conversions are explicit.** A constructor or projector crosses one nominal boundary. A function of type `Box[A] -> Box[B]` is still distinct from `A -> B`: an adapter must wrap its input, call the original function, and unwrap its result. Products and sums likewise need explicit conversion of their relevant components. For an abstract `F`, supply the required conversion operations as dictionary values.
