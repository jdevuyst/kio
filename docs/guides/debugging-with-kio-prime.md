# Debugging with Kio'

Kio' is the small core that surface Kio lowers to and then validates before
backend emission. When a surface construct behaves differently than you
expected, emitted Kio' shows the completed, independently checked program that
the backend will see.

Use this guide for advanced debugging of elaborator calls, `match!`, tuple
and sum elaborations, `do` blocks, placeholder lambdas, and backend
emission surprises. For interactive navigation through names and types,
start with [`kio repl`](repl.md).

## Build the Kio' target

Kio' is a build target, not a diagnostic flag. Add a `kio-prime`
target to the package's package file:

```kio {variant=package}
package app;

build {
  cache "out/.kio-cache/";

  target kio-prime {
    out "out/kio-prime/"
  }
}
```

Then build just that target:

```sh
kio build kio-prime
```

The output directory contains one Kio' text file per completed module.
Those files are source, not an implementation AST dump, so you can read
them directly. The `kio build` invocation that emitted them first checked the
Lowered surface program, substituted every recorded elaboration and inferred
annotation, and validated the assembled package again as standalone Kio'.

## Read the lowered form

Kio' has no surface conveniences. The most useful thing to know is what
disappears:

| Surface Kio | What to look for in Kio' |
| --- | --- |
| Elaborator calls such as `fit!`, `widen_sum!`, `narrow_prod!`, `one_prod!` | Explicit intrinsic trees; the elaborator call itself is gone |
| Tuple literals and product projection | `__pair__`, `__fst__`, `__snd__` |
| Sum construction and branching | `__left__`, `__right__`, `__either__` |
| `if` / `else` | `__if_then_else__` |
| `match!` | A decision tree built from intrinsics and let-bound handlers |
| `do` blocks | Nested calls to the receiver's `bind` |
| Placeholder lambdas | Explicit lambdas with generated parameters |
| `labels`, `literal` aliases, operators, `equiv` | Lowered or removed before the Kio' boundary |

If the Kio' is surprising, the issue is earlier than the backend:
resolution, typechecking, elaboration, or lowering. If the Kio'
matches your expectation but a backend's output does not, the issue is in
backend emission or host integration.

## A practical loop

1. Reproduce the behavior with `kio check` or a small `equiv` claim.
2. Build `kio-prime`.
3. Inspect the module that contains the surprising expression.
4. Search for the lowered helper, intrinsic tree, or generated lambda.
5. Compare that shape with the relevant surface construct in
   [`specs/language.md`](../../specs/language.md) and the Kio' core in
   [`specs/prime.md`](../../specs/prime.md).

For reusable compile-time claims, move the reduced expression pair into
[Testing with `equiv`](equiv.md). For name lookup and type
inspection, use [`kio repl`](repl.md) before dropping to Kio'.
