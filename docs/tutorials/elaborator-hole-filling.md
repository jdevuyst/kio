# Filling a shared `match!` result

Use this tutorial when the clauses in a `match!` call leave some type arguments open. We will start with an explicit result type in the surrounding function, make the clauses disagree on purpose, and then let two differently ordered clause lists infer the same result.

This follows the [language tutorial](language.md), which introduces sums, `match!`, and the `_` type-argument placeholder. Here the focus is the feedback loop around one call: what information can fill the result, what a failure means, and how to correct it without annotating every clause.

`match!` is imported elaborator code, not a keyword. A package that materializes the library as `elab` imports it with `import elab/match(match);`. The checked examples below use a file-backed Kiodoc package and the same real module through the documentation support tree, where it is re-rooted as `match`.

<!-- The examples share a package and a complete root module. The harness is
file-backed so it can import the real elaborator support configured by
docs/docs.pkg.kio. Each visible snippet is checked as a fresh consumer file. -->
<!--kio {file}
package hole_filling;

bridge {
  hole_filling/**;
}
-->

<!--kio {file}
module hole_filling;
-->

<!--kio {harness=match_example file placeholder="__SNIPPET__"}
module hole_filling/example;

import testapi(I32, String);
import match(match);
import spine_elaborators(widen_sum);

type I32_fn = I32 -> I32;

fn identity[A](value: A) -> A { value }

fn concrete_string_clause(_text: String) -> I32_fn { identity(I32) }

__SNIPPET__
-->

## Start with an explicit result

The function's declared return type makes every clause check against one known result:

```kio {@match_example}
fn explicit_result(source: I32 | String) -> I32_fn {
  match! source {
    .(number: I32) { identity(_) };
    .(text: String) { identity(_) }
  }
}
```

`identity` is polymorphic. In `identity(_)`, the underscore asks Kio to choose its type argument while leaving the resulting function unapplied. The explicit `I32_fn` return type therefore makes both clause bodies produce `I32 -> I32`.

An explicit return type is a useful baseline: it separates a clause-body problem from a missing-result problem. Once this version checks, you can see whether a local use already supplies enough context to omit it.

## Make the clauses disagree

Now omit the result and give the clauses concrete, incompatible results. The first clause returns `I32`; the second returns `String`:

```kio {@match_example check_exit_code=14}
fn incompatible_results(source: I32 | String) -> . {
  let _result =
    match! source {
      .(number: I32) { number };
      .(text: String) { text }
    };
  ()
}
```

`kio check` exits with the type-error code, `14`. Source order does not make the first result win. The call succeeds only if all clauses agree on one result, so reversing these clauses would not turn the program into a `String`-returning match.

The failed call also publishes no partial choice. Although the first clause offers `I32`, that choice is kept only if the whole call succeeds. After you correct the source and run `kio check` again, inference starts from the corrected call's local information; it does not remember `I32` from the failed attempt.

## Let a concrete clause fill the common result

A target need not be written when one clause determines the result and every other clause can agree with it. Here `concrete_string_clause` has result `I32_fn`. The other clause contains `identity(_)`, whose type argument can therefore be filled as `I32`.

The two functions deliberately reverse their clause order:

```kio {@match_example}
fn apply_generic_first(source: I32 | String, value: I32) -> I32 {
  let selected =
    match! source {
      .(number: I32) { identity(_) };
      concrete_string_clause
    };
  selected(value)
}

fn apply_concrete_first(source: I32 | String, value: I32) -> I32 {
  let selected =
    match! source {
      concrete_string_clause;
      .(number: I32) { identity(_) }
    };
  selected(value)
}

equiv clause_order_does_not_choose_result(number: I32, text: String, value: I32) {
  apply_generic_first(widen_sum!(number, I32 | String), value);
  apply_concrete_first(widen_sum!(text, I32 | String), value);
  identity(I32)(value)
}
```

Both calls infer `I32_fn` as their clause result. The determining clause may appear before or after the open one; clause order controls neither the chosen type nor whether inference succeeds. What matters is the complete, finite set of results contributed by that one `match!` call.

The `equiv` block checks the result of the elaborator-generated dispatch as well as the inferred type. One path selects the generic clause, the other selects the concrete clause, and both functions act as the identity on `value`.

## Run the loop

For an edit like this, use the ordinary commands in increasing order:

```sh
kio check
kio test
kio build
```

`kio check` is the fastest answer to “do all clause results agree?” `kio test` then partially evaluates and compares the fully checked `equiv` arms, so the elaborator-generated dispatch must reduce to the behavior you claimed. It does not run emitted backend code. Finally, `kio build` emits the targets already named by your package's `build` block. The bang call itself is gone by that point: typechecking has replaced it with ordinary checked Kio code.

During a correction cycle, return to `kio check` first. A failed check produces no build artifact, and fixing the error does not require clearing a cache or changing the elaborator library.

## If inference still fails

Check these points in order:

- Import `match` and spell the block call `match! source { clauses }`; the bang is what runs the elaborator while checking.
- Give each clause parameter enough type information to identify the source arm.
- Check every clause body, including helper functions, for one common result type.
- Look for a local anchor: a surrounding checked position or a clause whose result is already concrete.
- If every result is still open and the match is the function's returned expression, write the intended function return type. For an earlier local match, use a concrete typed binding such as `let .(selected: I32_fn) = match! source { clauses };`; a later use of an untyped `selected` does not send its type back across the `let`.
- Do not reorder clauses to steer inference. A clause's parameter types say which source branch it handles; its tuple position does not give it priority as the result-type anchor.
- After changing a failed call, rerun `kio check`; no partial choice from the earlier failure survives.

For clause coverage and structural sum shapes, see [Structural sums and pattern matching](../guides/sums.md). For the author-facing details behind a fills-aware elaborator, see [Defining elaborators](../guides/elaborators.md).

## Keep the problem bounded

Most calls need no extra annotation. Start with natural source and let concrete clauses or a directly checked position supply the result. When several nested generic clauses leave the result unclear, return the match directly from a helper with a concrete return type or give its local binding a concrete type instead of annotating every internal expression.

This keeps the inference problem local and makes a disagreement point at the clause that caused it. It also gives the checker less unresolved structure to carry through a large match. Split deeply nested matches into named helpers when that improves the source on its own; do not reshape clear code merely to chase an unspecified compile-time improvement.
