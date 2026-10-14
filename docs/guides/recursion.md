# Recursion

Kio separates recursive computation from recursive data. Functions are not
recursive by default. Use `rec` for recursive declarations and `recur` for
recursive calls. Use a `rec(loop)` group when a function needs to call
itself or another function in the same recursive group. The name inside
`rec(...)` is an ordinary function value, usually a `host fn` the host
supplies, imported with a plain `import`:

```text
import app(loop, leq_i32, sub_i32);
```

The loop function has the shape:

```text
loop[S][R](step: S -> (S | R), state: S) -> R
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
host fn loop[S][R](step: S -> (S | R), state: S) -> R;
host fn loop_reentry[S][R](step: ((S -> R) & S) -> (S | R), state: S) -> R;

__INSERT_CODE_HERE__
-->

The examples use host types `I32`, `Bool`, and `String` with roles `i32`,
`bool`, and `str`. The host comparisons `eq_i32` and `leq_i32` take two `I32`
values and return `Bool`; `sub_i32` and `mul_i32` take two `I32` values and
return `I32`. Import the conditional elaborator with
`import elab/control(if);` after following the
[elaborator-library setup](using-libraries.md#the-elaborator-library) under
the dependency name `elab`.

For a single function, put `rec(loop)` directly before the `fn` keyword:

```kio {@module}
pub rec(loop) fn countdown(n: I32) -> I32 {
  if! leq_i32(n, 0) { n } else { recur countdown(sub_i32(n, 1)) }
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

Every recursive call must use `recur`. A plain `countdown(...)` inside the group is not recursive syntax and is rejected.

In an editor connected to `kio lsp`, a missing call marker can offer a quick fix.
The compiler checks the proposed edit before offering it, including any required
`poly` or `cont` annotation. The declaration's loop capability remains your
choice: `rec(path)` names an ordinary value at `path`, not a compiler-selected
function. If an edit would leave another error at the call or expose a different
error, follow the diagnostic's help and correct the source directly.

Parentheses may group the selected capability path repeatedly: `rec((loop))`
selects the same function as `rec(loop)`. Formatting removes the extra grouping.

The marker precedes the recursive function's name, as in `recur countdown(n)`.
Both words remain available as ordinary names outside their grammatical
positions: `fn id(rec: .) { rec }` uses a parameter named `rec`, and
`recur(x)` calls an ordinary function named `recur`. Neither is a recursive call.

## Mutual recursion

Use braces for a group with more than one member. The braces group recursive
bodies; they do not create a visibility scope. Visibility is written on each
function:

```kio {@module}
rec(loop) {
  pub fn even(n: I32) -> Bool {
    if! eq_i32(n, 0) { .t } else { recur odd(sub_i32(n, 1)) }
  };
  fn odd(n: I32) -> Bool {
    if! eq_i32(n, 0) { .f } else { recur even(sub_i32(n, 1)) }
  }
}
```

Every member becomes an ordinary module function. With no modifier it is
module-private, `pub(path)` admits imports from that subtree, and `pub` makes it
public—the same visibility choices that newtype constructors and projectors
have. Later declarations in the same module call these wrappers normally. In a
group member's own body, calls to members of that group still use
`recur name(...)`.

Members must have the same type-parameter list and return type shape. Their
value parameters can differ, but every member returns through the group's one
result type. This also applies when calls use `recur(cont)`.

## Call annotations

The default recursive call is monomorphic and tail-position:

```text
recur loop_member(next)
```

Use `recur(poly)` when the recursive call supplies a different type argument:

```kio {@module}
pub rec(loop) fn steps[A](n: I32, marker: A) -> I32 {
  if! leq_i32(n, 0) { n } else { recur(poly) steps(String, sub_i32(n, 1), "next") }
}
```

Use `recur(cont)` when the recursive call is not in tail position:

```kio {@module}
pub rec(loop) fn fact(n: I32) -> I32 {
  if! leq_i32(n, 1) { 1 } else { mul_i32(n, recur(cont) fact(sub_i32(n, 1))) }
}
```

The annotations compose. `poly` comes first in canonical formatting:

```text
recur(poly, cont) steps(String, sub_i32(n, 1), "next")
```

`recur(poly)` is rejected when the call uses the current type instantiation.
`recur(cont)` is rejected in tail position. A call captured by an ordinary
closure instead needs `recur(escape)`, even when that closure is called
immediately. `escape` includes `cont`: writing both is accepted, and the
formatter removes the redundant `cont`. A type-changing escaping call uses
`recur(poly, escape)`. `escape` without an enclosing closure is rejected.

A recursive call may appear in the first binding's right-hand side of a monadic `do!` block: `do! chain { let x <- recur(cont) step(v); x }`. That call supplies an ordinary argument to `chain`, so it needs `cont`; it is not inside the callback for the rest of the block. Once a bind or sequenced expression introduces that callback, recursive calls in the remaining block are captured and need `escape`.

Tail position is broader than the final expression alone: it includes both branches of `if!` **and each `match!` clause body**. A plain `recur` call directly in a `match!` arm — for example reading the next input line and recursing in the non-EOF arm — is a tail call and needs no `recur(cont)`. A `recur` call in the `match!` *scrutinee* (or an `if!` condition, or any other argument position) is not tail and does need `recur(cont)`.

## Calls that outlive their loop step

An escaping call can live inside an explicit lambda, a placeholder lambda, or
the callback introduced by a monadic `do!` step. The resulting closure can be
returned, stored, or invoked repeatedly after its creating function returns.
Use a re-entry-capable loop for that group:

```text
loop_reentry[S][R](step: ((S -> R) & S) -> (S | R), state: S) -> R
```

The host still interprets the left sum arm as the next state and the right arm
as the final result. It calls `step(reenter, state)`, supplying a callback
`reenter: S -> R` that can run the same loop again from a new state.

For example, `later(3)` returns a closure without invoking its recursive call.
The call happens when that closure is invoked:

```kio {@module}
pub rec(loop_reentry) fn later(n: I32) -> . -> I32 {
  .() -> I32 {
    if! leq_i32(n, 0) { n } else { recur(escape) later(sub_i32(n, 1))() }
  }
}

fn use_later() -> I32 { let saved = later(3); saved() }
```

The same saved closure may be called more than once. Each escaped call
evaluates its arguments once, in order, and re-enters at the called member's
selected type instantiation. It does not change which recursion group the
closure belongs to.

Ordinary groups keep the simpler loop interface. A host that provides only
the general interface can supply an ordinary adapter for such groups:

```kio {@module}
fn ordinary_loop[S][R](step: S -> (S | R), state: S) -> R {
  loop_reentry(S, R, .(_reenter: S -> R, current: S) -> S | R { step(current) }, state)
}
```

Use `rec(ordinary_loop)` for an ordinary group and `rec(loop_reentry)` for an
escaping group. These are normal function values, not special names.

Loop-managed transitions remain stack-safe when the supplied loop implements
them iteratively. This includes a deep run started after earlier calls have
returned. Synchronous re-entry is different: an earlier host call may wait
while its callback starts another run, leaving waiting host frames on the
stack. Arbitrarily deep nested synchronous callbacks can therefore exhaust
the host stack. The loop does not capture or suspend an opaque host continuation.

Ordinary function composition also avoids unnecessary waiting frames. When a
Kio function finishes by calling another Kio function, generated code continues
in the called function without keeping a frame just to return its result. This
includes calling a function parameter, such as the final `next(value)` in a
binding helper. When the caller still has work to do, Kio retains that work
in managed storage instead; a deeper chain can therefore need more memory.
Both behaviors apply through the documented callable types, including unchanged
transport through host storage and compatible independently generated packages.
They do not capture waiting host calls or recover Kio's calling behavior after
an explicit conversion to a native host function.
See the [backend contract](../../specs/backends/README.md#tail-calls-within-generated-code)
for the precise boundary.

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
do not make a function recursive, call a host loop, or admit `recur name(...)`
calls. See [Aliases, newtypes, visibility, and purity](declarations.md#recursive-newtypes)
and [Structural sums and pattern matching](sums.md#recursive-sums).

## Lowering

The compiler turns a `rec(loop)` group into ordinary functions that send
state transitions to the supplied loop. A `recur(cont)` transition also carries
the remaining computation, so the loop can finish the called step and resume
its caller. This turns both tail and non-tail recursion into iteration through
the capability. The group's functions keep their written visibility, and no
recursive-function group or recursive-call marker remains in Kio'.

The call annotations make the lowering choice explicit in the source: a plain `recur` call is the monomorphic tail-call shape, `poly` changes its type instantiation, `cont` retains the rest of the current step, and `escape` captures an ordinary re-entry function for use across a closure boundary.

The loop boundary is deliberate. It keeps Kio' free of term recursion, makes loop-managed depth depend on the supplied capability, and gives `recur(poly)` one portable meaning even on hosts whose native functions cannot express the same polymorphic recursion pattern. An escaping group's re-entry function is an ordinary captured function; it does not make a generated wrapper or step call itself. Kio therefore does not lower recursive groups to ordinary native recursive calls.
