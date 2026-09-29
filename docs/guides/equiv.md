# Testing with `equiv`

An `equiv` declaration is a compile-time claim that two or more Kio
expressions have equivalent residual normal forms. `kio test` typechecks the
package, partially evaluates every arm without running a host, and compares
the results. The declaration contributes nothing to emitted package code.

Use `equiv` for language-level laws and focused regression claims. Use an
executable golden or host test when the behavior depends on actual I/O,
mutation, a backend's emitted representation, or other host-runtime details.

<!--kio {harness=equiv placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import __intrinsics__;
import control(scope);

host type String role(str);
host fn print(value: String) -> .;

__INSERT_CODE_HERE__
-->

## Declaration and typing rules

An `equiv` has a diagnostic name, optional type and value parameters, and at
least two semicolon-separated expression arms:

```kio {@equiv}
fn id[A](value: A) -> A { value }

equiv id_unfolds[A](value: A) {
  id(A, value);
  value
}
```

Every value parameter needs a type annotation. Every arm sees the same
parameters and must have the same type. A disagreement there is an ordinary
type error before testing starts, not a failed equivalence claim. An arm that
needs local statements wraps them in a `scope! { ... }` expression:

```kio {@equiv}
equiv local_work[A](value: A) {
  scope! {
    let copy = value;
    copy
  };
  value
}
```

`equiv` has no `pub` form and cannot be called as a value. Its name exists only
so test output can identify the claim.

## The discharge model

For each claim, `kio test` works on the fully checked and elaborated Kio' terms:

1. Each value parameter becomes a fresh opaque symbolic atom shared by every
   arm; type parameters are erased.
2. Each arm is partially evaluated with Kio's β- and ι-reductions.
3. Evaluation stops at a residual normal form wherever an opaque parameter or
   host operation prevents further reduction.
4. The residuals are compared up to α-equivalence, η-equivalence, and
   structural equality of opaque applications.

Kio' is strongly normalizing and confluent, so discharge terminates and does
not depend on which sensible reduction order an implementation chooses.

### Fresh symbolic parameters

The same parameter denotes the same opaque atom in every arm. This makes a
parameterized declaration a universal claim:

```kio {@equiv}
equiv shared_atom[A](value: A) {
  value;
  .(x: A) { x }(value)
}
```

Both arms normalize to the atom for `value`. Distinct parameters receive
distinct atoms, so a claim comparing `left` with `right` would fail: it claims
equality for every pair of possible values, not only pairs that happen to be
equal.

## Partial evaluation and residual forms

Ordinary function application, type application, `let`, known sum branches,
product projections, newtype wrap/unwrap pairs, and literal `if` conditions
reduce. What cannot reduce remains visible in the result. Residuals include
literals, functions, pairs, sum injections, newtype constructors, opaque
atoms, stuck applications and conditionals, expression-statement sequences,
and symbolic sum case splits.

This is deliberately not a concrete evaluator. The residual records exactly
the structure Kio can justify without inventing facts about symbolic inputs or
the host.

## α- and η-equivalence

Binder spelling does not matter: functions compare up to consistent renaming
of bound value and type parameters. Functions also compare extensionally by
η, so a wrapper that only applies a function to its argument equals the
function itself:

```kio {@equiv}
equiv function_eta[A][B](f: A -> B) {
  .(value: A) { f(value) };
  f
}
```

The corresponding η laws apply to products and sums. Rebuilding both
projections of an opaque product equals the product; an exhaustive sum
elimination that re-injects each payload unchanged equals the original sum.

## Host calls stay opaque

A `host fn` participates in typing but never executes during `kio test`. A call
is a stuck residual identified by the host item's path and the residual shape
of its arguments. Identical calls compare equal:

```kio {@equiv}
equiv same_host_call() {
  print("hello");
  print("hello")
}
```

Different arguments, different host functions, or an additional call produce
different residuals. In particular, discarding a call's unit result does not
erase the action:

```kio {@equiv}
equiv extra_action_fails() {
  scope! {
    print("hello");
    print("hello")
  };
  print("hello")
}
```

This declaration typechecks, but `kio test` reports it as failed. Likewise, an
`if` whose condition is an opaque host value stays residual; only the literal
tokens `.t` and `.f` select a branch during partial evaluation.

## Symbolic sums

An opaque sum parameter still has a closed set of possible constructors.
Rather than getting stuck at `__either__`, the evaluator creates a symbolic
case split, evaluates both handlers with fresh payload atoms, and compares the
branches structurally. Nested eliminations distribute through that split.

The double-swap law therefore discharges for every `A | B` value:

```kio {@equiv}
fn swap[A][B](value: A | B) -> B | A {
  __either__(
    , A
    , B
    , B | A
    , value
    , .(left) { __right__(B, A, left) }
    , .(right) { __left__(B, A, right) }
    )
}

equiv swap_twice[A][B](value: A | B) {
  swap(B, A, swap(A, B, value));
  value
}
```

The inner split flips each injection, the outer elimination distributes and
flips it back, and sum η collapses the identity case split to `value`. This
symbolic rule belongs to `equiv` discharge; it does not change ordinary Kio'
runtime reduction.

## Reading multi-arm failures

Claims may have more than two arms. The runner normalizes them all, partitions
equivalent arms into groups, and prints each group's arm indices and residual
normal form. A three-arm failure can therefore show that arms 1 and 3 agree
while arm 2 differs, instead of reducing the result to a single boolean.

A failed claim exits with code `50`. Parse, name-resolution, type, and
elaborator failures retain their own earlier compile-time exit categories. A
successful run prints one `pass` line per claim and a final passed/total
summary.

## Selecting modules and dependencies

With no selector, `kio test` considers every module in the current package:

```sh
kio test
```

Pass module names or `.kio` paths to narrow the run. Equivs in dependency
modules are skipped by default, even though the dependency source is
materialized in the package tree. Include them explicitly when auditing the
whole closure:

```sh
kio test --include-deps
```

The default keeps a consumer responsible for its own laws while leaving each
dependency's internal suite to that dependency. The command reports when it
skips dependency claims.

## What a passing claim proves

A passing parameterized `equiv` proves equality, under Kio's documented
partial-evaluation and α/η relation, for every instantiation of its symbolic
parameters. It can pin helper unfolding, newtype round trips, structural laws,
or the result of elaborator-generated glue.

It does not execute host code, establish a host function's mathematical law,
exercise an emitter, compare performance, or prove arbitrary semantic
properties outside that reduction model. The claim is only as broad as its
parameters and arms.

## Debugging a failed claim

Start with the normal-form groups in the failure output:

- A different opaque head or argument points to a different host call or
  symbolic function application.
- An extra residual sequence points to an additional unit-returning action.
- Different injections or constructor heads expose a structural mismatch.
- A symbolic case split shows which sum branch behaves differently.

Then make the claim smaller: unfold one helper at a time, keep the same
symbolic parameters on both sides, and separate unrelated laws into distinct
`equiv` declarations. Use [the REPL](repl.md) to normalize concrete terms and
[Debugging with Kio'](debugging-with-kio-prime.md) to inspect the elaborated
core shape when surface glue is the suspect.

The exact relation is specified in
[`specs/formal/equiv.md`](../../specs/formal/equiv.md); command selection,
output, and exit behavior are specified in
[`specs/cli.md`](../../specs/cli.md#kio-test-module).
