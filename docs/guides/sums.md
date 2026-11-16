# Structural sums and pattern matching

A sum type `A | B` says a value is one of several shapes. Kio has no separate
`enum` declaration: `|` composes structural alternatives, while nominal entries in a `type` declaration
create distinct names for alternatives that carry domain meaning.

Those two forms belong to the language. The convenient operations used below
are library macros: `widen_sum` constructs a chosen arm and `match`
builds exhaustive dispatch. They are not keywords or ambient builtins. A real
module must depend on the library and select their macro declarations explicitly:

```kio {ignore}
import macros/match {macro match}
import macros/spine_macros {macro widen_sum}
import macros/tuple_macros {macro concat}
```

Here `macros` is the configured dependency name. Follow
[Using libraries](using-libraries.md#the-macro-library) to adopt it under
that name or adjust the imports to the name you choose.

Assumed setup: a module whose host supplies `Int`, `String`, `Bool`, `int_to_string`, `string_concat`, and `print`. Snippets are module bodies.
Annotate a literal source when the macro needs its carrier before expansion;
ordinary concrete contexts can infer literals. See
[literal inference](typechecking.md#literal-types-and-local-inference).

<!--kio {harness=module placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

mod kiodoc;

import match {macro match}
import tuple_macros {macro concat}
import spine_macros {macro widen_sum}

host type Int role(i32);
host type String role(str);
host type Bool role(bool);
host fn int_to_string(p0: Int) -> String;
host fn string_concat(p0: String, p1: String) -> String;
host fn print(p0: String);

__INSERT_CODE_HERE__
-->

## Sums are structural

`String | Int` is the type of a value that is either a `String` or an `Int`. It does not need to be declared:

```kio {@module}
fn build_string_sum() -> String | Int { "hello\n"[String].>widen_sum }

fn build_int_sum() -> String | Int { 42[Int].>widen_sum }
```

`widen_sum` injects the source value into a target sum that contains its type.
It is an imported user macro, despite how closely its operation follows
the `|` structure. When the expected type is not obvious, write the target
explicitly:

```kio {@module}
fn explicit_target() -> String | Int { widen_sum("hello\n"[String])[String | Int] }
```

For a sum-valued source, grouping matters: `widen_sum` matches the alternatives
along the source and target's right-hand chains. With distinct atomic arm types,
`A | B` can widen to `C | A | B`, but not directly to `(A | B) | C`, whose left
branch groups the original sum into one slot. A transparent type alias preserves
that grouping. The [spine palette](../poc/macros.md#two-source-to-target-palettes)
explains how nested sums and products are treated.

## When branches need names

Sum branches are positional, and repeated payload types are allowed. If two branches mean different domain cases, do not use `String | String` and rely on position. Use nominal types to give the alternatives distinct identities, then widen those values into the named sum:

```kio {@module}
type Channel = { Primary: String } | { Fallback: String };

fn primary_channel(s: String) -> Channel { (Primary.mk(s).>widen_sum)[Channel] }

fn fallback_channel(s: String) -> Channel { (Fallback.mk(s).>widen_sum)[Channel] }

fn describe_channel(c: Channel) -> String {
  match(c) {
    .(p: Primary) { string_concat("primary:", Primary.get(p)) };
    .(f: Fallback) { string_concat("fallback:", Fallback.get(f)) }
  }
}
```

Inside each clause the parameter has one statically known nominal type:
`p: Primary` or `f: Fallback`. The corresponding `get` member projects its
payload after `match` selects the branch.

The same pattern scales to several alternatives:

```kio {@module}
type Event = { Text: String } | { Code: Int } | { Done: . };

fn describe_event(e: Event) -> String {
  match(e) {
    .(t: Text) { string_concat("text:", Text.get(t)) };
    .(c: Code) { string_concat("code:", int_to_string(Code.get(c))) };
    .(_d: Done) { "done" }
  }
}
```

## Enum-like sums with nominal types

Use named nominal entries when arms need distinct identities:

```kio {@module}
type Color = { Red: . } | { Green: . } | { Blue: . };

fn red_color() -> Color { (Red.mk(()).>widen_sum)[Color] }
```

A case with data uses a non-unit payload:

```kio {@module}
type Message = { Text: String } | { Code: Int };
```

An arm with several fields stays record-shaped. This type's error arm carries both `Err` and `Code` fields:

```kio {@module}
type Result = { Ok: String } | { Err: String; Code: Int };
```

When alternatives share a field, use the existing nominal type in each arm:

```kio {@module}
type { Correlation_id: Int }

type Reply = (Correlation_id & { Reply_text: String }) | (Correlation_id & { Reply_code: Int });
```

Both arms contain the same `Correlation_id` nominal identity.

## Matching a sum

`match` is also an imported user macro. Put the scrutinee before its
block and the clause lambdas inside it. The clause parameter type is the
branch shape it handles:

```kio {@module}
fn describe(v: String | Int) -> String {
  match(v) {
    .(s: String) { s };
    .(n: Int) { int_to_string(n) }
  }
}
```

Every clause body must produce the same result type. The surrounding expected
type or a clause whose result is already known can supply that type; see
[inferring a match result](typechecking.md#inferring-a-match-result) for a
worked example and where to add an annotation.

Clauses must cover the scrutinee. Leaving out the `Int` branch is a macro error:

```kio {@module check_exit_code=15}
fn incomplete(v: String | Int) -> String {
  match(v) {
    .(s: String) { s }
  }
}
```

When there is only one clause, put it directly in the block. One block entry
passes that function value through unchanged; it does not make a one-element
tuple.

Clauses are tried in source order. `.() { … }` and `.(_all: .) { … }` are
catch-alls, not tests for an empty arm: any branch can supply Unit by discarding
its payload. Put them after more specific clauses. Every clause must handle
at least one branch that earlier clauses leave uncovered; an unreachable clause
is rejected. The scrutinee is still evaluated once. This differs from `Done`
above: a nominal type with a Unit payload
remains a distinct nominal type, so `.(done: Done) { … }` matches that tag only.

## Destructuring product branches

A clause can name multiple value parameters. That matches a product branch positionally:

```kio {@module}
fn render(v: (String & Int) | String) -> String {
  match(v) {
    .(label: String, n: Int) { string_concat(label, int_to_string(n)) };
    .(s: String) { s }
  }
}
```

This is not record-style matching. The first product slot goes to `label`; the second goes to `n`.

## Recursive sums

Anonymous sum types cannot mention themselves. Recursion is grounded at a nominal type, which wraps and unwraps the recursive body:

```kio {@module}
rec type { List[A]: . | (A & List[A]) }

fn nil[A] -> List[A] { List.mk(widen_sum(())[. | (A & List[A])]) }

fn head_or[A](xs: List[A], default: A) -> A {
  match(List.get(xs)) {
    .(x: A, _rest: List[A]) { x };
    .(_empty: .) { default }
  }
}
```

`nil[Int]` constructs an empty `List[Int]`; the type-only signature has no
Unit argument. For a constructor that waits for an explicit Unit call, keep
`()` in its signature as explained in
[Type-only values](../tutorials/language.md#type-only-values).

`rec type` gives one compact declaration an explicit atomic recursive scope.
Its nominal heads are visible throughout the declaration, and a
named form also sees its transparent alias head. This makes a recursive
disjunction concise while keeping its cycles nominally grounded:

```kio {@module}
rec type Label_list[A] = { Nil[A]: . } | { Cons[A]: A & Label_list[A] };

fn empty_label_list[A]() -> Label_list[A] { (Nil.mk[A](()).>widen_sum)[Label_list[A]] }
```

`Label_list` remains a transparent alias; the `Cons` nominal is the
nominal recursion knot. An unmarked type declaration is source ordered and
cannot see its own alias or later generated heads. `rec` is required for a
genuine cycle and rejected when the declaration is acyclic.

## Polymorphic clauses

A clause can bind type parameters when the same body works for several branches:

```kio {@module}
type { Box[A]: A }

fn box_label[A](b: Box[A]) -> String { "boxed\n" }

fn label(p: Box[Int] | Box[String]) -> String {
  match(p) {
    .[A](b: Box[A]) { box_label(b) }
  }
}
```

The clause `.[A](b: Box[A]) { ... }` matches *both* `Box[Int]` and
`Box[String]`, binding `A` per branch. Here the surrounding function's `String`
result determines the clause's return type before its body is checked, so the
clause does not need to repeat that annotation. This omitted result is
determined before the body is checked and cannot mention the clause's local
`A`. A result that uses that binder needs a concrete return annotation, such
as `-> A`, or a complete expected function type. `match` specializes an
annotated result for each compatible source branch; those specialized results
must still agree on one common result type. See
[marked macro inputs](macros.md#marked-fills-implementations) for
the annotation and staging rules.

## Composable clause sets

The clause set is an ordinary product value. A caller can build a common set,
extend it locally, and pass the composed product to `match` as one block entry:

The imported `concat` helper combines the nested products into the flat
product of three clause functions shown in its target type. Without that
step, `(text_clauses(), extra_clause)` would retain the first pair as a nested
product.

```kio {@module}
fn text_clauses() -> (String -> String) & (Int -> String) {
  (.(s: String) { s }, .(n: Int) { int_to_string(n) })
}

fn describe_extended(v: String | Int | .) -> String {
  let clauses =
    concat((text_clauses(), .(_unit: .) -> String { "unit" }))[& (String -> String)
    & (Int -> String)
    & (. -> String)];
  match(v) { clauses }
}
```

Composition changes the product value you pass at that call site. There is no
hidden handler registry.

## Direct block call

Write `match` directly with its scrutinee and clause block:

```kio {@module}
fn direct(v: String | Int) -> String {
  match(v) {
    .(s: String) { s };
    .(n: Int) { int_to_string(n) }
  }
}
```

The block supplies the clauses in source order. You can also put the scrutinee
first as `v.>match { clauses }`, or after the complete block as
`match { clauses }.<v`. Both insert `v` before the projected clause argument;
see [trailing-block UFCS](ufcs.md#calls-with-trailing-blocks).

## Where this leads

- [Error handling](error-handling.md) uses sums and bottom for
  partial-function-style failure modeling.
- [Row-typed records with ordinary products](products.md) covers the product side of
  the same structural type system.
- [UFCS calls](ufcs.md) covers `.>`, `.>>`, `.<`, and `.<<` in detail.
- [Using libraries](using-libraries.md) covers the dependency workflow and
  points to the advanced macro library's conditional cross-sum helpers.
