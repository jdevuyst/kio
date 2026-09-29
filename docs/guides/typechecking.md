# Understanding typechecking

Kio checks how pieces of a program fit together before they run. A type such
as `Int -> String` describes a function's input and output; `[A] A -> A`
describes one function that works for any type `A`. You can pass these
functions as values, put them in data structures, and compose them without
giving up static checking.

This guide explains where Kio can fill in types and where it needs your help.
It follows the [language tutorial](../tutorials/language.md); no type-theory
background is required. The examples assume host-provided `Int`, `String`,
and `Bool` types. The host supplies their operations too.

<!--kio {harness=types placeholder="__EXAMPLE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type Int role(i32);
host type String role(str);
host type Bool role(bool);

__EXAMPLE__
-->

<!--kio {harness=branches placeholder="__EXAMPLE__"}
bridge {
  kiodoc;
}

module kiodoc;

import control(if);
import sequence(do);
import spine_elaborators(widen_sum);

host type Int role(i32);
host type String role(str);
host type Bool role(bool);

__EXAMPLE__
-->

## What the rules are trying to achieve

Kio balances reusable abstractions with predictable, local reasoning:

- **Catch incompatible combinations.** A function expecting one type cannot
  silently receive another. Host implementations must still fulfill their
  declared contracts; checking cannot prove that a host function is correct.
- **Keep checking decidable.** Type inference is a bounded process, not a
  search for arbitrary conversions or a proof about the whole program.
- **Make interfaces readable.** A named function declares its contract.
  Callers use that contract, not guesses drawn from the function's body.
- **Avoid unnecessary repetition.** An argument, result position, or nearby
  call can already provide the type a lambda or generic call needs.

These goals explain why Kio has useful inference without trying to infer every
annotation. They also support [open-world compilation](open-world.md): adding
unrelated declarations to a dependency's module body does not change an
existing consumer's meaning.

## Types can come from either direction

Sometimes the expression tells Kio its type. In `identity(number)`, the
argument fixes `A` as `Int`:

```kio {@types}
fn identity[A](value: A) -> A { value }

fn keep_number(number: Int) -> Int { identity(number) }
```

You could spell the call `identity(Int, number)`, but there is no need here.
An explicit `_` type argument likewise asks Kio to fill that slot; it is not
a request to search for a value.

In the other direction, a position can tell an expression what type it needs.
The return type below supplies `Int -> Int`, so the lambda need not repeat
its parameter type:

```kio {@types}
fn identity_function() -> Int -> Int { .(value) { value } }
```

These directions are often called *synthesis* (finding an expression's type)
and *checking* (checking it against an expected type). You need not memorize
those terms; ask which part of the source supplies the missing information.

### Arguments of one call can help each other

Information is not limited to the argument immediately to the left:

```kio {@types}
fn apply[A][B](f: A -> B, value: A) -> B { f(value) }

fn keep_number(number: Int) -> Int { apply(.(value) { value }, number) }
```

`number` fixes `A`, and the function's return position fixes `B`. Together
they give the lambda its expected `Int -> Int` type.

Calls are **connected** here when a call is nested within another call's
argument and its unfinished result type participates in checking that argument.
This can extend through several nested calls. It does not allow later uses to
infer an earlier local binding or sequence action's missing type. For example:

```kio {@types}
fn make_identity[A]() -> A -> A { .(value) { value } }

fn apply[A][B](f: A -> B, value: A) -> B { f(value) }

fn nested(number: Int) -> Int { apply(make_identity(), number) }
```

Here the outer call's argument and result information can determine the inner
factory's `A`. By contrast, two calls appearing in the same function are not
automatically connected. An already completed inner call contributes only its
finished type; Kio does not reconsider its body. Lambda bodies still follow
their own rules, and elaborator inputs must also satisfy the
[pre-expansion boundary](#elaborators-need-input-descriptions-before-they-expand).

This is local cooperation between expressions you wrote, not a search through
other functions or a whole-program inference pass.

A lambda called immediately can also take missing parameter types from that
same call's arguments:

```kio {@types}
fn immediate(number: Int) -> Int { .(value) { value }(number) }
```

Here `number` supplies the parameter type directly. This does not look through
a separate binding or use the lambda body to guess it.

## Type inference between block clauses

A block clause is a local binding or expression separated from the next by
`;`. The useful rule is source-first checking, not a special privilege of
`let`: later clauses cannot supply missing types for an earlier clause's
source. An earlier clause can use its own annotation or required type.
Sequence blocks also have shared bind-call constraints, as explained below;
semicolons do not ban all sharing of type information.

### Local bindings

Kio finishes a `let` right-hand side before examining the following code.
This example is rejected even though the later call seems to explain what
`value` should be:

```kio {@types check_exit_code=14}
fn too_late(number: Int) -> Int { let f = .(value) { value }; f(number) }
```

The unannotated lambda has neither an expected function type nor annotated
parameters. Its body does not invent a type for `value`, and the later use of
`f` cannot send information back across the binding. Give the binding its
contract instead:

```kio {@types}
fn explicit_boundary(number: Int) -> Int { let .(f: Int -> Int) = .(value) { value }; f(number) }
```

Alternatively, write `.(value: Int) { value }` on the right-hand side. Once
the parameter is known, an ordinary lambda can infer its result from its body.
Unlike a named `fn`, an omitted lambda result does **not** default to unit.

A complete binding annotation can also solve a generic factory's result:

```kio {@types}
fn make_identity[A]() -> A -> A { .(value) { value } }

fn use_factory(number: Int) -> Int { let .(f: Int -> Int) = make_identity(); f(number) }
```

Without that annotation, `make_identity()` has no information that selects
`A`. A later `f(number)` cannot supply it. An explicit type argument at the
factory call is another way to select `A`.

Partial patterns have a deliberate limit. In `let .(first: Int, second) = e;`,
the annotation does not describe the whole product, so `e` must determine
its own type first. The annotated leaves are checked afterward. Annotate the
whole pattern if its right-hand side needs that expected type.

### Expression blocks: separators and the final value

An **expression block**, such as a function body, runs local bindings and
expressions and yields its final expression's value.
Only the final expression of an expression block receives the block's expected
type. Earlier expression clauses must produce unit (`.`); use `let _ = e;`
when you deliberately discard a non-unit result. A trailing semicolon does
not discard the final value or change its type: `;` is a separator.
Earlier clauses, whether bindings or expressions, finish typechecking without
help from later clauses. In this sense, type inference does not travel backward
over semicolons in an expression block.
For example, `{ let value = number; value; }` has the same final value as
`{ let value = number; value }`. It is a clause's position and form, not the
presence of a semicolon after it, that determines whether it is a binding,
an earlier expression, or the final result.

This makes a completed binding a useful checkpoint. Moving an expression
into a separate unannotated binding can remove context it previously received
from an enclosing call. When that matters, write the contract at the new
boundary rather than relying on later uses to reconstruct it.

### Sequence blocks: each binding has its own input boundary

In a **sequence block**, such as the block passed to `do!`, semicolons also
separate clauses. The supplied bind function connects an action of type
`F(A)` to a continuation receiving its payload `A`:

```kio {@branches}
fn sequence_example[*F](
  , bind: [A][B] (F(A) & (A -> F(B))) -> F(B)
  , source: F(Int)
  , finish: Int -> F(String)
  ) -> F(String) {
  do! bind {
    let value <- source;
    finish(value)
  }
}
```

`source` supplies the payload type `Int` before later clauses use `value`.
The later `finish(value)` does not infer that earlier payload backward. A
complete pattern annotation, such as `let .(value: Int) <- source;`, can guide
the source at its own binding. Ordinary `let ... = ...` clauses keep their
ordinary binding rules and do not invoke bind.

Sequence blocks become nested bind calls, so those calls still share carrier
and result-type constraints. For example, the expected block result can guide
the bind's result type. This does not permit a later use of `value` to choose
the earlier action's payload type: that source-first boundary remains in place.

An earlier unbound expression must produce `F(.)`; write `let _ <- action;`
to discard an action's non-unit payload deliberately. The final expression
instead supplies the block's `F(R)` result. Its trailing `;` does not add
another bind, discard that result, or insert a lift. Within each source
expression, nested calls still cooperate under the ordinary call rules.
See [monadic `do!`](higher-kinded-types.md#monadic-do) for the supplied bind
function and carrier type.

Semicolons are not a universal inference barrier for every kind of block.
A **product block**, such as the collection of clauses supplied to `match!`,
provides separate values to the elaborator; their result types can cooperate
according to its public contract. It is not an expression or sequence block's
chain of local bindings and earlier expressions.

## Polymorphism is written, not invented

An unannotated binding preserves an existing polymorphic type:

```kio {@types}
fn identity[A](value: A) -> A { value }

fn keep_both(number: Int, text: String) -> Int & String { let f = identity; (f(number), f(text)) }
```

Here `f` remains usable at different types because `identity` already declares
`[A]`. Kio does not turn the earlier untyped lambda into a generic function by
inventing a type parameter. An explicitly typed polymorphic binding is another
option:

```kio {@types}
fn keep_both(number: Int, text: String) -> Int & String {
  let .(f: [A] A -> A) = .[A](value) { value };
  (f(number), f(text))
}
```

Generic functions can themselves be arguments and results. Kio keeps the
written placement of type parameters: a function accepting a generic function
is not interchangeable with one whose caller chooses a single type for that
argument. See [higher-kinded types](higher-kinded-types.md) for abstractions
that also take type constructors such as a collection type.

## Alternatives agree on one result

The library's `if!` relates the results of both branches. Neither branch wins
just because it appears first. The surrounding expected result can guide
both, or an independently determined branch can supply the common result.
If they disagree, Kio does not silently invent a sum type:

```kio {@branches check_exit_code=14}
fn disagree(flag: Bool, number: Int, text: String) -> . {
  let result =
    if! flag {
      number
    } else {
      text
    };
  ()
}
```

If a sum is what you intend, declare it and construct the alternatives:

```kio {@branches}
fn either_value(flag: Bool, number: Int, text: String) -> Int | String {
  if! flag {
    number.>widen_sum!
  } else {
    text.>widen_sum!
  }
}
```

The condition is checked before the branch results; those results do not infer
the condition's type backward. At runtime only the selected branch runs.
`match!` also requires a common result, while its clause parameters must first
identify which inputs they handle. The
[shared-result tutorial](../tutorials/elaborator-hole-filling.md) develops this
with checked examples in both clause orders.

These are library-defined elaborators, not special meanings attached to the
names `if` or `match`. Their public types and declared elaboration rules
determine how their arguments cooperate. The same applies to imported `do!`:
each action determines its payload before the following binding uses it;
later uses do not infer an earlier action backward. See
[higher-kinded types](higher-kinded-types.md) for sequencing.

## Elaborators need input descriptions before they expand

An elaborator constructs code at compile time. Some elaborators, including
`if!` and `match!`, exchange type information with callback bodies: their
declarations use `impl(fills)`. They can use the types and structure already
available at the call, but cannot ask the compiler to enter an unfinished
argument body to find information they need to expand. Ordinary `impl`
elaborators instead receive arguments whose checking is already complete.
The distinction can matter to callers, not just library authors.

Computed arguments must have determined types before the expansion starts.
The call's outside expected result and already known sibling arguments can
help determine them through the elaborator's public type. The implementation's
later choices cannot. A declared generic parameter still counts as known:
`Box(A)` inside a function declaring `A` is different from a factory call whose
type argument has not been chosen. Supply an explicit type argument or a
complete binding annotation when the call otherwise lacks that information.
Adding an unannotated binding alone does not supply it.

For example, suppose an ordinary function has the signature
`drive[S][R](step: S -> S | R, seed: S) -> R`. A callback written as
`.(state: State) -> State | Done { advance(state) }` identifies the call's
result as `Done` without inspecting the callback body. An `impl(fills)`
elaborator receiving that call can use the result type. If the callback omits
its return annotation and only its body establishes `Done`, the same input needs to finish ordinary
checking first. Bind the complete `drive(...)` call to a local value before
passing it to the elaborator, or supply the callback's result annotation.

This does not require every argument type to be fully determined in advance.
For instance, `match!` can relate the results of directly supplied clauses
after their parameter types identify the alternatives they handle. An explicit
tuple exposes its components for that purpose; an ordinary function returning
a tuple supplies one computed value instead. Its signature alone does not say
whether it reorders, duplicates, or discards the arguments used to build it.
A computed tuple with a determined type remains a valid input; it just does
not expose its unfinished components. Ordinary match arms can still omit
their result annotations.

These rules keep compile-time expansion from depending on exploratory checks
of unfinished bodies. They do not excuse checking those bodies: an invalid
argument is still rejected even when the elaborator ignores its value. The
[elaborator guide](elaborators.md#marked-fills-implementations) explains the
precise input requirements and how library authors declare result relations.

## Type equality is not automatic conversion

Knowing that a value could be rearranged is different from having the required
type. `A & B` and `B & A` have different layouts; a newtype remains distinct
from its payload type. Transparent type aliases name an existing type without
creating that distinction.

Kio makes structural rearrangement explicit with elaborators such as
`reorder_prod!` and `widen_sum!`, rather than trying conversions during every
type comparison. This keeps ordinary checking predictable and makes the
adaptation visible. Read [products](products.md), [sums](sums.md), and
[declarations](declarations.md) for the relevant constructions.

## When Kio asks for more information

Start at the smallest boundary that lacks a type:

1. For a lambda, check its expected function type, the arguments supplied when
   it is called immediately, or its parameter annotations. Kio does not inspect
   parameter uses to guess missing parameter types.
2. For a generic call, check whether its arguments and result position select
   every type argument. Supply an explicit type argument when they do not.
3. For a local binding, put a complete annotation on that binding when its
   right-hand side needs context. There is no expression-level `e: T` syntax.
4. For disagreeing results, decide on a shared type and construct it explicitly.
5. For a named `fn`, check the declared result: omitting `-> T` means unit,
   not an inferred return type.

The [language specification](../../specs/language.md#type-system) gives the
precise rules. Surface inference finishes before the explicit
[Kio' core](debugging-with-kio-prime.md): local unknowns are not left for a host
backend to guess.
