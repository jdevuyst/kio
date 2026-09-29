# Recursion

Kio separates recursive computation from recursive data. Functions are not
recursive by default. Use a `rec(loop)` group when a function needs to call
itself or another function in the same recursive group. The name inside
`rec(...)` is an ordinary function value, usually a `host fn` the host
supplies, imported with a plain `import`:

```text
import app(loop, leq_i32, sub_i32);
```

The loop function has the shape:

```text
loop[S][R](step: S -> S | R, state: S) -> R
```

<!--kio {harness=module placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import control(if);

host type Bool role(bool);
host type I32 role(i32);
host type String role(str);
host fn eq_i32(p0: I32, p1: I32) -> Bool;
host fn leq_i32(p0: I32, p1: I32) -> Bool;
host fn sub_i32(p0: I32, p1: I32) -> I32;
host fn mul_i32(p0: I32, p1: I32) -> I32;
host fn loop[S][R](step: S -> S | R, state: S) -> R;

__INSERT_CODE_HERE__
-->

For a single function, put `rec(loop)` directly before the `fn` keyword:

```kio {@module}
pub rec(loop) fn countdown(n: I32) -> I32 {
  if! leq_i32(n, 0) {
    n
  } else {
    rec countdown(sub_i32(n, 1))
  }
}
```

This is exactly the one-member braced form:

```text
rec(loop) fn f(...) { ... }            == rec(loop) { fn f(...) { ... } }
pub(app) rec(loop) fn f(...) { ... }   == rec(loop) { pub(app) fn f(...) { ... } }
pub rec(loop) fn f(...) { ... }        == rec(loop) { pub fn f(...) { ... } }
```

The visibility may appear after `rec(loop)` in source. `kio fmt` puts it first
and collapses any one-member braced group to this shorthand.

Every recursive call must use `rec`. A plain `countdown(...)` inside the group is not recursive syntax and is rejected.

The marker precedes the recursive function's name, as in `rec countdown(n)`.
The word remains available as an ordinary name: `fn id(rec: .) -> . { rec }`
uses a parameter named `rec`, and `rec(x)` calls an ordinary function named
`rec`. Neither expression is a recursive call.

## Mutual recursion

Use braces for a group with more than one member. The braces group recursive
bodies; they do not create a visibility scope. Visibility is written on each
function:

```kio {@module}
rec(loop) {
  pub fn even(n: I32) -> Bool {
    if! eq_i32(n, 0) {
      .t
    } else {
      rec odd(sub_i32(n, 1))
    }
  };
  fn odd(n: I32) -> Bool {
    if! eq_i32(n, 0) {
      .f
    } else {
      rec even(sub_i32(n, 1))
    }
  }
}
```

Every member becomes an ordinary module function. With no modifier it is
module-private, `pub(path)` admits imports from that subtree, and `pub` makes it
public—the same visibility choices that newtype constructors and projectors
have. Later declarations in the same module call these wrappers normally. In a
group member's own body, calls to members of that group still use
`rec name(...)`.

Members must have the same type-parameter list and return type shape. Their
value parameters can differ, but every member returns through the group's one
result type. This also applies when calls use `rec(cont)`.

## Call annotations

The default recursive call is monomorphic and tail-position:

```text
rec loop_member(next)
```

Use `rec(poly)` when the recursive call supplies a different type argument:

```kio {@module}
pub rec(loop) fn steps[A](n: I32, marker: A) -> I32 {
  if! leq_i32(n, 0) {
    n
  } else {
    rec(poly) steps(String, sub_i32(n, 1), "next")
  }
}
```

Use `rec(cont)` when the recursive call is not in tail position:

```kio {@module}
pub rec(loop) fn fact(n: I32) -> I32 {
  if! leq_i32(n, 1) {
    1
  } else {
    mul_i32(n, rec(cont) fact(sub_i32(n, 1)))
  }
}
```

The annotations compose. `kio fmt` prints them in the canonical order `poly`, then `cont`, then `escape`:

```text
rec(poly, cont) steps(String, sub_i32(n, 1), "next")
```

`rec(poly)` is rejected when the call uses the current type instantiation. `rec(cont)` is rejected in tail position. Calls captured by nested functions or `do!` continuations are rejected; `rec(escape)` is reserved for that case.

A recursive call may appear in the first binding's right-hand side of a monadic `do!` block: `do! chain { let x <- rec(cont) step(v); x }`. That call supplies an ordinary argument to `chain`, so it needs `cont`; it is not inside the callback for the rest of the block. Once a bind or sequenced expression introduces that callback, recursive calls in the remaining block are captured and rejected.

Tail position is broader than the final expression alone: it includes both branches of `if!` **and each `match!` clause body**. A plain `rec` call directly in a `match!` arm — for example reading the next input line and recursing in the non-EOF arm — is a tail call and needs no `rec(cont)`. A `rec` call in the `match!` *scrutinee* (or an `if!` condition, or any other argument position) is not tail and does need `rec(cont)`.

## Recursive data has no loop capability

Recursive types use the same contextual word but different, capability-free
forms. A singleton nominal marks its own recursive scope:

```kio {@module}
rec newtype Chain[A] : . | (A & Chain(A)) { constructor chain; projector un_chain }
```

Genuinely mutual declarations use bare braces, with no loop name:

```kio {@module}
rec {
  type Tree[A] = Branch(A);
  newtype Branch[A] : . | (A & Tree(A) & Tree(A)) { constructor branch; projector un_branch }
}
```

The first form must contain a real self-cycle; the second must be exactly one
mutual cycle whose every cycle crosses a `newtype` boundary. Neither form sees
unrelated later declarations. `rec labels` is the surface counterpart for a
single recursive label declaration. These forms control type-name scope and
do not make a function recursive, call a host loop, or admit `rec name(...)`
calls. See [Aliases, newtypes, visibility, and purity](declarations.md#recursive-newtypes)
and [Structural sums and pattern matching](sums.md#recursive-sums).

## Lowering

`rec(loop)` is surface language only. The frontend lowers the group into private state-packet `newtype`s, one visibility-preserving wrapper function per member, and a call to the ordinary loop function. `rec(cont)` groups carry continuations in the state packet, so non-tail recursion still lowers to iteration rather than a Kio' fixpoint. If a generated continuation packet refers to itself, the emitted Kio' declaration uses `rec newtype`; if several packets refer to one another, they use one minimal bare `rec { ... }` type group. Those markers describe only the generated type-name scope—no recursive-function group or recursive-call marker survives.

The call annotations make the lowering choice explicit in the source: a plain `rec` call is the cheap monomorphic tail-call shape, while `poly` and `cont` name the cases that need heavier machinery.

The loop boundary is deliberate. It keeps Kio' free of term recursion, makes recursion depth depend on the supplied `loop` function rather than on each host language's native call stack, and gives `rec(poly)` one portable meaning even on host languages whose native functions cannot express the same polymorphic recursion pattern. Kio therefore does not lower recursive groups to ordinary native recursive calls.
