# Structural sums and pattern matching

A sum type `A | B` says a value is one of several shapes. Kio has no separate
`enum` declaration: `|` composes structural alternatives, while `labels`
creates nominally distinct names for alternatives that carry domain meaning.

Those two forms belong to the language. The convenient operations used below
are library elaborators: `widen_sum!` constructs a chosen arm and `match!`
builds exhaustive dispatch. They are not keywords or ambient builtins. A real
module must depend on the library and import their names without the bang:

```kio {ignore}
import elab/match(match);
import elab/spine_elaborators(widen_sum);
```

The examples use the repository's documentation support modules without the
`elab/` dependency prefix. [Using libraries](using-libraries.md) explains why
dependency modules acquire that re-rooted prefix.

Assumed setup: a module whose host supplies `Int`, `String`, `Bool`, `int_to_string`, `string_concat`, and `print`. Snippets are module bodies.

<!--kio {harness=module placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import match(match);
import tuple_elaborators(concat);
import spine_elaborators(widen_sum);

host type Int role(i32);
host type String role(str);
host type Bool role(bool);
host fn int_to_string(p0: Int) -> String;
host fn string_concat(p0: String, p1: String) -> String;
host fn print(p0: String) -> .;

__INSERT_CODE_HERE__
-->

## Sums are structural

`String | Int` is the type of a value that is either a `String` or an `Int`. It does not need to be declared:

```kio {@module}
fn build_string_sum() -> String | Int { "hello\n".>widen_sum! }

fn build_int_sum() -> String | Int { 42.>widen_sum! }
```

`widen_sum!` injects the source value into a target sum that contains its type.
It is an imported user elaborator, despite how closely its operation follows
the `|` structure. When the expected type is not obvious, write the target
explicitly:

```kio {@module}
fn explicit_target() -> String | Int { widen_sum!("hello\n", String | Int) }
```

For a sum-valued source, grouping matters: `widen_sum!` matches the alternatives
along the source and target's right-hand chains. With distinct atomic arm types,
`A | B` can widen to `C | A | B`, but not directly to `(A | B) | C`, whose left
branch groups the original sum into one slot. A transparent type alias preserves
that grouping. The [spine palette](../poc/elab.md#two-source-to-target-palettes)
explains how nested sums and products are treated.

## When branches need names

Sum branches are positional, and repeated payload types are allowed. If two branches mean different domain cases, do not use `String | String` and rely on position. Use labels to mint distinct generated types, then widen those labels into the named sum:

```kio {@module}
labels Channel = { primary: String } | { fallback: String };

fn primary_channel(s: String) -> Channel { {primary = s}.>widen_sum!(Channel) }

fn fallback_channel(s: String) -> Channel { {fallback = s}.>widen_sum!(Channel) }

fn describe_channel(c: Channel) -> String {
  match! c {
    .(p: Primary) { string_concat("primary:", p.?{primary}) };
    .(f: Fallback) { string_concat("fallback:", f.?{fallback}) }
  }
}
```

Inside each clause the parameter has one statically known label type:
`p: Primary` or `f: Fallback`. Field access then uses the ordinary product
operation `p.?{primary}` or `f.?{fallback}`. The access is not a conditional
search across the whole sum; `match!` first selects a branch, and `.?{field}`
projects a field that the selected clause type guarantees is present.

The same pattern scales to several alternatives:

```kio {@module}
labels Event = { text: String } | { code: Int } | { done: . };

fn describe_event(e: Event) -> String {
  match! e {
    .(t: Text) { string_concat("text:", t.?{text}) };
    .(c: Code) { string_concat("code:", int_to_string(c.?{code})) };
    .(_d: Done) { "done" }
  }
}
```

## Enum-like sums with labels

Use a named `labels` sum when arms need stable names:

```kio {@module}
labels Color = { red: . } | { green: . } | { blue: . };

fn red_color() -> Color { {red=}.>widen_sum!(Color) }
```

A case with data uses a non-unit payload:

```kio {@module}
labels Message = { text: String } | { code: Int };
```

An arm with several fields stays record-shaped. This type's error arm carries both `Err` and `Code` labels:

```kio {@module}
labels Result = { ok: String } | { err: String, code: Int };
```

When several alternatives carry the same previously declared field, mark each
reuse explicitly:

```kio {@module}
labels { correlation_id: Int };

labels Reply = { correlation_id: _, reply_text: String } | { correlation_id: _, reply_code: Int };
```

Both arms contain the same `Correlation_id` nominal. Writing the payload again
would be a second declaration, not a reuse.

## Matching a sum

`match!` is also an imported user elaborator. Put the scrutinee before its
block and the clause lambdas inside it. The clause parameter type is the
branch shape it handles:

```kio {@module}
fn describe(v: String | Int) -> String {
  match! v {
    .(s: String) { s };
    .(n: Int) { int_to_string(n) }
  }
}
```

Every clause body must produce the same result type. Clauses must cover the scrutinee. Leaving out the `Int` branch is an elaborator error:

```kio {@module check_exit_code=15}
fn incomplete(v: String | Int) -> String {
  match! v {
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
above: a label with a Unit payload
remains a distinct nominal type, so `.(done: Done) { … }` matches that tag only.

## Destructuring product branches

A clause can name multiple value parameters. That matches a product branch positionally:

```kio {@module}
fn render(v: (String & Int) | String) -> String {
  match! v {
    .(label: String, n: Int) { string_concat(label, int_to_string(n)) };
    .(s: String) { s }
  }
}
```

This is not record-style matching. The first product slot goes to `label`; the second goes to `n`.

## Recursive sums

Anonymous sum types cannot mention themselves. Recursion is grounded at a `newtype`, which wraps and unwraps the recursive body:

```kio {@module}
rec newtype List[A] : . | (A & List(A)) { pub constructor cons; pub projector un_list }

fn nil[A]() -> List(A) { List.cons(widen_sum!((), . | (A & List(A)))) }

fn head_or[A](xs: List(A), default: A) -> A {
  match! xs.>List.un_list {
    .(x: A, _rest: List(A)) { x };
    .(_empty: .) { default }
  }
}
```

`rec labels` gives one label declaration an explicit atomic recursive scope.
Its generated nominal heads are visible throughout the declaration, and a
named form also sees its transparent alias head. This makes a recursive
disjunction concise while keeping its cycles nominally grounded:

```kio {@module}
rec labels Label_list[A] = { nil[A]: . } | { cons[A]: A & Label_list(A) };

fn empty_label_list[A]() -> Label_list(A) { Nil.mk(A, ()).>widen_sum!(Label_list(A)) }
```

`Label_list` remains a transparent alias; the generated `Cons` newtype is the
nominal recursion knot. An unmarked `labels` declaration is source ordered and
cannot see its own alias or later generated heads. `rec` is required for a
genuine cycle and rejected when the declaration is acyclic.

## Polymorphic clauses

A clause can bind type parameters when the same body works for several branches:

```kio {@module}
newtype Box[A] : A { pub constructor mk_box; pub projector un_box }

fn box_label[A](b: Box(A)) -> String { "boxed\n" }

fn label(p: Box(Int) | Box(String)) -> String {
  match! p {
    .[A](b: Box(A)) { box_label(b) }
  }
}
```

The clause `.[A](b: Box(A)) { ... }` matches *both* `Box(Int)` and
`Box(String)`, binding `A` per branch. Here the surrounding function's `String`
result determines the clause's return type before its body is checked, so the
clause does not need to repeat that annotation. The clause body must produce a
result type that doesn't mention its local `A`, because one `match!` expression
has one result type.

## Composable clause sets

The clause set is an ordinary product value. A caller can build a common set,
extend it locally, and pass the composed product to `match!` as one block entry:

```kio {@module}
fn text_clauses() -> (String -> String) & (Int -> String) {
  (.(s: String) { s }, .(n: Int) { int_to_string(n) })
}

fn describe_extended(v: String | Int | .) -> String {
  let clauses =
    concat!(
      , (text_clauses(), .(_unit: .) { "unit" })
      , (String -> String) & (Int -> String) & (. -> String)
      );
  match! v {
    clauses
  }
}
```

Composition changes the product value you pass at that call site. There is no
hidden handler registry.

## Direct block call

Write `match!` directly with its scrutinee and clause block:

```kio {@module}
fn direct(v: String | Int) -> String {
  match! v {
    .(s: String) { s };
    .(n: Int) { int_to_string(n) }
  }
}
```

The block supplies the clauses in source order. `match!` with a trailing block
uses this direct call shape rather than UFCS rearrangement.

## Where this leads

- [Error handling](error-handling.md) uses sums and bottom for
  partial-function-style failure modeling.
- [Structural products and row types](products.md) covers the product side of
  the same structural type system.
- [UFCS calls](ufcs.md) covers `.>`, `.>>`, `.<`, and `.<<` in detail.
- [Using libraries](using-libraries.md) covers the dependency workflow and
  points to the advanced elaborator library's conditional cross-sum helpers.
