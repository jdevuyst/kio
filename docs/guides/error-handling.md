# Error handling

Kio has no exceptions and no built-in `Result` type. Failure is modeled the same way every other "one of several shapes" is: with a structural sum. This guide covers the `!` bottom type, the `(T | !)` idiom, and how fallible steps compose through the elaborators.

Assumed setup: a module whose host supplies `Int`, `String`, `Bool`, `print`, `int_to_string`, and arithmetic. Snippets are module bodies.

<!--kio {harness=module placeholder="__INSERT_CODE_HERE__" accumulate}
bridge {
  kiodoc;
}

module kiodoc;

import match(match);
import control(if);
import spine_elaborators(widen_sum, narrow_sum, fit);

host type Int role(i32);
host type String role(str);
host type Bool role(bool);
host fn add(p0: Int, p1: Int) -> Int;
host fn int_to_string(p0: Int) -> String;
host fn print(p0: String) -> .;
host fn eprint(p0: String) -> .;
host fn exit(p0: Int) -> !;

__INSERT_CODE_HERE__
-->

## The bottom type `!`

`!` is the **uninhabited type** — no value ever has type `!`. On its own it's not useful; its role is as a sum arm. `(String | !)` is "a `String`, or a value of a type that can't exist" — which is just a `String`, wearing the shape of a sum.

That sounds pointless until you see what it buys: a *uniform shape* for "might have failed." A function that always succeeds can still hand back a `(String | !)`, and a caller that handles both arms works whether or not failure is actually reachable.

`widen_sum!` adds a `!` arm as an unreachable extension; `narrow_sum!` drops it back via its `!`-elimination rule — `A | !` is `A`:

```kio {@module}
fn never_fails(s: String) -> String | ! { s.>widen_sum! }

fn unwrap_total(s: String | !) -> String { s.>narrow_sum! }
```

Both moves fall out of the spine palette's general rules. `widen_sum!`'s capacity-widening rule covers "add an arm of any type, even `!`," and `narrow_sum!`'s explicit `!`-arm carve-out covers "drop source `!` arms freely" — no inhabitant exists to route, so the value-set is unchanged.

## Modeling real failure: `(T | E)`

For failure that *is* reachable, the error arm carries a real type — a message, a code, a structured error value:

```kio {@module}
labels Diverror = { div_by_zero: . };

fn safe_div(a: Int, b: Int, is_zero: Bool) -> Int | Div_by_zero {
  if! is_zero {
    widen_sum!({div_by_zero=})
  } else {
    widen_sum!(add(a, b))
    // stand-in for the real division
  }
}
```

The result type `(Int | Div_by_zero)` is "an `Int`, or a division error." The `if` arms both produce that same sum type; `widen_sum!` uses the function's return type to pick the target.

## Consuming a fallible value

A caller dispatches with `match!`:

```kio {@module}
fn show_result(r: Int | Div_by_zero) -> . {
  match! r {
    .(n: Int) { print(int_to_string(n)) };
    .(_e: Div_by_zero) { print("division by zero\n") }
  }
}
```

Exhaustiveness is enforced — you cannot forget the error arm.

## Composing fallible steps

When several fallible steps chain, each one's error arm has to flow through. Elaborators do the plumbing.

`widen_sum!` extends a narrower error into a wider error sum, so a step that can only fail one way slots into a pipeline that can fail several:

```kio {@module}
labels Overflowtag = { overflow: . };

// step error: Div_by_zero
// pipeline error: Div_by_zero | Overflow
fn widen_error(e: Div_by_zero) -> Div_by_zero | Overflow { e.>widen_sum!(Div_by_zero | Overflow) }
```

A `match!` on the first step's result either continues with the success value or short-circuits by widening its error into the pipeline's error type. Every arm produces the same `(Result | WideError)` shape, so the dispatch result can flow directly to the next step.

This is the manual version of what monadic `bind` automates in other languages. Imported `do!` blocks smooth over exactly this friction for monadic pipelines — the [higher-kinded-types case study](../poc/hkt.md) threads a `do!` pipeline over a `Monad(F)` instance. `match!` + `widen_sum!` remains the explicit idiom when you want the control flow visible.

## Crashing on purpose

`(T | E)` is for failures the caller should *handle*. For failures that mean "this program is broken" — a violated invariant — a package can build a `panic` by sequencing the host's `eprint` and `exit`:

```kio {@module}
fn panic[A](msg: String) -> A { eprint(msg); exit(70).>fit!(A) }
```

`exit` has return type `!`; `fit!` handles a `!` source by emitting the bottom eliminator for the requested target type. No value is constructed at runtime because `exit` never returns. `panic` slots in anywhere. Reserve this for genuine bugs; recoverable failure belongs in a `(T | E)` sum.

## Where this leads

- [Structural sums + `match!`](sums.md) — the sum machinery this guide builds on.
- [Using libraries](using-libraries.md) — the checked structural helpers used by the examples.
