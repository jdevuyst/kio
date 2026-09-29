# Kio language reference

This document is the source of truth for Kio's **syntax and semantics**. It describes only what is settled. The canonical grammar productions live in [`grammar.md`](grammar.md) — this document narrates *what each form means and how it desugars*; that one pins *what parses*.

Kio is layered. The small subset **Kio'** — described in [`prime.md`](prime.md) — carries the formal semantics and is the target for self-hosting; this document describes the full surface, which adds sugar and elaborator calls on top of Kio'. Elaborator calls are resolved through explicit imports. Compile-time reflection/helpers are imported only through the compiler-provided `import __comptime__;` block. Every feature here is either already in Kio' or expands into Kio' as noted inline.

## Overview

Kio is an ultra-portable language. Kio' deliberately contains nothing host-specific, so a program can be transpiled to any backend for which a host is provided.

- Built on **System F** (polymorphic lambda calculus with explicit universal quantification): functions, universal types (`forall`), type application. Kinds are first-order (`κ ::= * | κ → κ`), so a binder may range over a type constructor, not only a saturated type; there is no type-level lambda and there are no kind variables — a Church-style fragment of F-ω rather than full F-ω (rank-N permitted; quantifiers nest freely under arrows because every binder is explicitly written by the user; see [Higher-kinded types](#higher-kinded-types)). Anonymous type expressions cannot be self-referential. Recursive types exist, but the recursion is always grounded inside a `newtype` body (see [Type declarations](#type-declarations)), giving an iso-recursive wrap/unwrap at the nominal boundary.
- Supports **anonymous sum**, **anonymous product**, and **label-generated nominal** types on top of Kio' (the label forms are surface sugar; the underlying primitive is `newtype`).
- **No built-in types.** There are no numeric types, no booleans, no I/O, no ambient runtime, and no special `String`. Every type Kio code uses is either built from the language's structural constructors (`->`, `forall`, `&`, `|`), introduced by a module-level declaration (`type`, `newtype`, or `labels`), or declared as a `host type` in a module. Literal tokens in the source — integers (`42`), floats (`3.14`), booleans, and strings — are typed against the `role(...)`-marked host types in scope (see [Host declarations and the bridge block](#host-declarations-and-the-bridge-block)).
- **No implicit recursive definitions.** Ordinary `fn` bindings may not refer to themselves, nor may two ordinary bindings mutually refer to each other. Surface Kio has one explicit term-level escape hatch: `rec(loop)` groups, which lower annotated recursive calls to an ordinary host-supplied `loop` function and do not introduce a Kio' fixpoint. Recursive data scope is separately explicit and capability-free: `rec newtype` / `rec labels` for a singleton and bare `rec { ... }` for one genuinely mutual type component. Every cyclic type path crosses a nominal newtype boundary; there is no alias-only fixed point. This keeps Kio' strongly normalizing while still letting hosted packages opt into general iteration through an explicit capability.
- Surface syntax aims to feel close to **modern curly-brace languages**.
- **Trait-like machinery is just label-generated types.** A "trait" is a label-generated type, or a product of label-generated types, whose labels are the method names. Callers reach the methods by explicit projection. Value-level glue between a source value and an expected type is synthesized by **imported elaborator bang-calls** — `name!(e)` forms resolved through ordinary scoped imports — not by any built-in coercion. Resolution is opt-in and name-driven: an elaborator only runs where its name is in scope and written with `!` at the call site. The reference source-to-target palettes (an algebraic palette and a spine palette of structural-coercion elaborators) are user-defined Kio libraries that live in the case studies, not language primitives; see [§ Elaborators are imported, not ambient](#elaborators-are-imported-not-ambient). *(Every elaborator bang-call form is surface-only — none exist in [Kio'](prime.md).)*
- **Open-world design.** Adding definitions to a module body never causes another module to fail to compile. (Scoped to module bodies — a package file's contract surface is governed by versioning, not open-world; see [Open-world design](#open-world-design).)

## Type system

Kio's type discipline is **bidirectional**: every type a program needs is anchored somewhere in the source — at a binder, in a top-level signature, as a positional type argument at a call site, or as the expected type of the position the expression sits in. There is no Hindley-Milner-style global inference. Type arguments to polymorphic items are passed positionally alongside value arguments; the language has no `[T]`-style instantiation syntax. A call site may write each type argument out explicitly, mark a slot with the `_` placeholder, or fully elide the type-argument list. Elided and `_`-marked slots are inferred by a finite application-local relation over value arguments, connected nested applications, and (when the call sits in a checked position) the expected type at that position — see [Bidirectional elaboration](#bidirectional-elaboration) below for the formal account.

What this rules out: no inference variable escapes its finite connected
application or lexical owner, no let-generalization, and no global constraint
propagation. What it allows: an application can relate its resolved callee type,
surrounding expected result, immediate value arguments, and connected nested
application results using first-order equality constraints. An independently
completed nested application contributes only its closed result. When a child
still needs information from that finite relation, its unfinished typing edge
is retained until the relevant parameter slot becomes usable. Source typing
derivations and elaborator actions are never replayed; no backtracking or
global worklist is involved. Every local unknown and retained edge is gone
before the program reaches Kio'.

This most visibly lets an enclosing call provide a function type to a lambda,
or a result type to a nested polymorphic call, after another sibling has pinned
a shared type argument. A direct lambda callee can also fill whole missing
parameters from that same node's value arguments. The source-to-target
elaborator calls are the only direct user-written positions whose primary job
is source-to-target glue; `match!` may synthesize glue internally as part of
dispatch.

### Bidirectional elaboration

Kio's type-checker is an **elaborator**: it takes a Kio surface program and produces a [Kio'](prime.md) tree, with every surface-only form removed and every inferable type-argument filled in. The elaborator follows Peyton Jones, Vytiniotis, Weirich, and Shields (2007), *Practical type inference for arbitrary-rank types* (PJ-V-W-S), the discipline of choice for systems that want rank-N polymorphism without giving up decidability. The formal rules are pinned in [`formal/elaboration.md`](formal/elaboration.md); this section gives the user-facing summary.

**Two modes.** Every typing rule operates in one of two modes: **synthesize** (⇑, "the typer reads an expression and computes its type") or **check** (⇓, "the typer reads an expression alongside an expected type and verifies the expression fits"). Both modes also produce the elaborated Kio' term. The rule a sub-expression follows is determined by its parent: a call's value-argument slot is a checking position (the expected type is the parameter type); an untyped `let`'s right-hand side is a synthesizing position; a `let` whose binding pattern is fully concrete creates a checked right-hand side position; a top-level `fn`'s body is checked against the declared return type. The two modes never run simultaneously on the same sub-expression — each position is one or the other.

**Finite retained application inference.** Each polymorphic application owns
a finite set of equality goals derived from its written arguments, declared
call layers, and expected result. A connected nested application retains its
own owner and lexical scope while contributing to that relation; a completed
child contributes only its closed type. If a parameter slot is not usable yet,
the corresponding source action is retained and consumed exactly once after
other local facts refine the slot. Each application owner may likewise retain
at most one pending expected-result equation; a connected tree may contain one
for each owner. After an initial blocked attempt, another attempt requires a
retained child to close or one of the equation's zonked sides to change
structurally; another deferral replaces that progress snapshot. Advancing to a
different schedule mode is not progress and never retries the equation. These
bounded deferrals do not add a third typing mode: source premises are entered
once, completed subexpressions are not rechecked, and elaborator actions are
not rerun.

The schedule preserves source order and semicolon boundaries. A lambda body
may independently determine its result type, but does not determine a missing
parameter type by inspecting uses of that parameter. Within a block, every
`let` or expression statement is finished before the following clause is
typed. Only the final expression receives the block's surrounding expected
type; information never flows backwards across `;`.

**Exact inferred type arguments.** An `_`-marked or elided type-argument slot
may be solved by either a monotype or a complete polytype such as
`[A] A -> A`. A polytype solution is admitted only when finite evidence in the
connected written application already exposes that entire, well-kinded type
tree — for example, `consume[T](value: T)` called as `consume(identity)` when
`identity : [A] A -> A`. The typer assigns the whole inference goal that exact
tree and compares later evidence structurally, modulo alpha-renaming. It never
invents or generalizes a `forall`, guesses binder placement, leaves an
inference goal beneath a `forall`, searches ambient declarations, or
backtracks. A polymorphic value therefore still cannot fill a proper subterm
of a declared monomorphic shape such as `T -> T`; only a whole inferred
type-argument goal may receive the already-complete polytype. Explicit
polytype arguments retain the same structural checking rule. This bounded
exact-copy rule keeps inference decidable without requiring the user to repeat
a polytype already fixed by local evidence.

**Shallow skolemization.** When a lambda literal is checked against a polytype `[A][B] P -> R`, only the leading binders `[A][B]` are converted to fresh skolems; binders nested inside the parameter type or return type are left intact and surface as expected polytypes for those positions. This is why `([A] A -> A) -> Int` (rank-2: parameter is polytype) is distinct from `[A] (A -> A) -> Int` (rank-1: parameter is monotype, type-arg lives at the outer call) — the typer cannot rearrange binders silently.

**No expression-level `:` annotations.** Kio's grammar admits no expression-level type annotation: there is no `e : T` form. Annotations live exclusively at:

- Top-level `fn` / `host fn` parameter and return-type slots.
- Local binding patterns, including the unary spelling `let .(x: T) = e;`.
- Type-argument slots at call sites (positional, optionally `_`-marked or fully elided).
- `host type` / `newtype` / parametric `type` / parametric `labels` declaration headers.
- The body of a `type` declaration.

The user has to seed the expected type from one of these positions; everywhere else, the elaborator either synthesizes bottom-up or pulls the expected type from the immediately-enclosing context.

**Untyped `let` takes the RHS's type as-is.** A `let x = e;` types `e` in synthesizing mode with no expected type and binds `x` to whatever type `e` synthesized — polytype or monotype, taken unchanged. No narrowing, no generalization, no defaulting. The reason is structural: every type-binder in Kio is user-written (universal `[A]` at signatures and `fn` headers, existential `<U>` at `newtype` payloads), so any polytype the elaborator synthesizes traces back to a binder the user wrote in the source. There is no ambiguity for the rule to resolve. The Hindley-Milner-era defaulting rules (ML's value restriction, Haskell's `default`) exist to disambiguate un-annotated inference — Kio has nothing to disambiguate. The principal example: `let f = id;` where `id` is an in-scope top-level polymorphic `fn`, e.g. `fn id[T](x: T) -> T { x }`. `id` synthesizes its declared polytype `[T] T -> T`; `f` inherits it; the local `f` is usable at every instantiation `id` accepts.

**Typed `let` checks a local RHS.** A fully concrete binding pattern supplies an expected type to the RHS: `let .(f: Int -> Int) = .x. { x1 };` checks the placeholder lambda against `Int -> Int`, and `let .(f: Int -> Int) = make_id();` can solve a polymorphic factory from the checked return position. This is still a local bidirectional boundary, not body-driven inference. The checked position comes only from the binder pattern itself. A partial pattern or partial annotation, such as `let .(f: _) = e;` or `let .(x: Int, y) = e;`, does not push a partial expected type into `e`; the RHS must synthesize on its own, and the written leaves are checked against the synthesized product afterward. This deliberately skips partial product unification.

Before a partial local annotation is compared, transparent aliases are
materialized exactly. A written `_` is rejected when any resulting occurrence
lies strictly beneath a structural `forall` introduced inside that annotation.
Identity and reordering aliases preserve the source occurrence; duplication
reports once at the one written `_` when any copy violates the rule; a dropped
occurrence has no resulting annotation hole and remains admitted. This check
happens before an inference goal or annotation publication is created. The RHS
still synthesizes first, and the ordinary concrete-leaf comparison is
unchanged.

The Hindley-Milner idiom `let id = .(x) { x };` is still rejected — but the rejection is at the **lambda-literal rule**, not at the let rule. The lambda literal sits in synthesizing position (the let RHS is synthesize-mode), and a synthesizing lambda literal requires every value-parameter slot to carry a concrete annotation; `x` has none. The error names the lambda literal, not the let. To bind a polymorphic local that the user writes inline, the lambda literal must sit at a position that supplies the expected polytype (commonly the value-arg slot of an enclosing polymorphic call), which puts it in checking mode where shallow skolemization pins the binders. This forces the reader to see the polymorphism at the binder site.

**No backward flow across a prefix.** The let-rule chooses the RHS mode before
the body is examined — either synthesize for an untyped or partial binder, or
check against the fully concrete binder type. The body's uses of `x`, the
tail's expected type, and facts from a later sibling cannot propagate an
expected type back into `e`. An expression statement similarly checks its
expression against `.` before typing the tail. This is separate from the
"as-is" rule above: the "as-is" rule says nothing modifies an untyped RHS's
type once synthesized; "no backward flow" says nothing after the semicolon
informs the prefix in the first place. The pair keeps rank-N inference
decidable under PJ-V-W-S.

**Positional type-arguments at calls.** A call site `f(T₁, T₂, a₁, a₂)` writes
type arguments and value arguments in one positional list. Type arguments
precede values within the current universal/function layer; applying its value
layer may expose another binder run. The typer reads explicit type arguments
left-to-right, then solves remaining binders from the finite local relation
described above: value-argument types, connected nested results, and the call's
expected return type. An unannotated literal first uses a refined parameter
slot when one is available; only then does lexical role fallback apply. Any
binder still unknown after the bounded schedule is a type error. UFCS calls use
the same positional list and application rules: `r.>f(T, x)` supplies `T`,
inserts `r` into a value slot, and checks the resulting ordinary call plan —
see § UFCS.

**Named callables share a uniform user-facing contract.** Top-level `fn`s, intrinsics (`__left__`, `__right__`, …, `__absurd__`, `__if_then_else__`), and elaborator names (such as `iso!`, `fit!`, `match!`, `derive!`, or a package-defined `checked!` after the corresponding import) are all looked up by name and expose a declared polytype. Elaborator names are called with bang syntax and run compile-time implementation code over reflected arguments; from the caller's perspective, explicit type arguments, `_` placeholders, value arguments, and surrounding expected types are solved against the elaborator's declared call type by the ordinary call-type rules. `__if_then_else__`'s scheme depends on the unique selected `role(bool)` host identity in the module's unqualified lexical scope; with zero or multiple identities, the name has no scheme and its use is a type error.

> *Implementation note.* The kio-rs implementation routes intrinsics and `__if_then_else__` through the `intrinsic_scheme` registry plus standard application typing. User-defined elaborators route through `Expr::UserElaborator`; their implementation returns reflected checked terms and the substitute pass swaps those terms in at the Lowered → Prime boundary. The split is deliberate: elaborator implementations produce Kio' terms through the compile-time ABI rather than through the runtime function-call path.

**Surface forms reduce to standard call shapes.** UFCS, tuple literals, and elaborator calls reduce to ordinary Kio' trees. `r.>f(T, x)` records the same call plan as `f(T, r, x)`; `(a, b)` becomes `__pair__(a, b)`. A block elaborator call projects its declared blocks into ordinary value arguments and invokes its resolved implementation. The reference library supplies `if!`, `match!`, `scope!`, and `do!`; these names have no compiler-owned control semantics. Elaborator bangs and dot-splices retain boundary-local elaborations until their Kio' terms or normalized calls are substituted at the Lowered → Prime boundary.

The decidability argument combines PJ-V-W-S with Kio's finite application
boundary: source-bounded exact instantiation, shallow skolemization, explicit
binders, no let-generalization, forward-only block prefixes, structural
equality over a source-bounded goal set, and bounded one-shot retained actions.
The formal account in [`formal/elaboration.md`](formal/elaboration.md) pins the
schedule and proof obligation.

### Anonymous sum and product types

In addition to System F, Kio has **anonymous sum** (`|`) and **anonymous product** (`&`) type constructors — ordinary logical disjunction and conjunction on types:

- `A | B` — a value of this type is a value of `A` or a value of `B`.
- `A & B` — a value of this type is a value of `A` and a value of `B` simultaneously.

So Kio's type system is effectively **System F with disjunction, conjunction, and nominal labels**, with recursion grounded at `newtype` and label-generated boundaries.

**Not Haskell-style ADTs.** Unlike a Haskell `data` declaration, these types are anonymous: there is no user-supplied type name, no named constructors, and no named fields. The *type itself* acts as the discriminator. In practice each branch of a `|` will often have a distinct constructor, but that is a convention, not a requirement: the sum is tagged by position, not by a user-provided name.

**Product types are finite cartesian products, represented as right-associated binary products.** Tuple values are elements of those products, not members of a separate nominal family indexed by arity; tuple notation is syntax for product introduction. Mathematics normally treats different parenthesizations of a finite product as canonically isomorphic rather than as one literal binary tree. Kio chooses a right-associated tree as its definitional surface and core representation. Parentheses group expressions and types unless comma/product structure gives them product meaning. The nullary product is `()`; a one-item parenthesized form is just grouping (`(x)` and `(x,)` are `x`, not a one-tuple); two or more comma items fold to the right. Thus `(a, b, c)` is by definition `(a, (b, c))`, inhabiting `A & (B & C)`. The left-nested product `((a, b), c)` inhabits `(A & B) & C`, which is a different structural type under ordinary equality and is bridged only by an explicit elaborator such as `flatten_prod!` / `fit!`.

**Positional and type-indexed.** The operators are ordered and do not collapse duplicates. `A & A` is a two-component product, not one; `A | A` is a two-branch sum, not one. Repeated types are distinguished **positionally** (first-match wins at elimination). This is *not* Boolean algebra on types: `A & B` ≠ `B & A` and `A & A` ≠ `A` up to type equality. At runtime a `|` value carries a small positional integer tag, not a reflective type tag.

**Construction and parenthesization.** Kio accepts same-operator `&` / `|` chains **without** outer parentheses in both surface and Kio' syntax: `A & B`, `A | B`, `A & B & C`, `A | B | C` are all valid `Type` expressions wherever a type is expected. Chains right-associate: `A & B & C` denotes `A & (B & C)`. Outer parens become **load-bearing** only where omitting them would change parsing: when the chain is a child of the *other* operator (`(A & B) | C`, `A & (B | C)`), and as the parameter list of a function-type form (`(A & B) -> C` — the parens belong to the arrow's parameter list, not to chain grouping). Values of `A & B` are written as the surface tuple literal `(a, b)`, which is sugar for the Prime intrinsic call `__pair__(a, b)`; correspondingly, `(a, b, c)` is sugar for `__pair__(a, __pair__(b, c))` and inhabits `A & B & C`, whereas `__pair__(__pair__(a, b), c)` inhabits the distinct type `(A & B) & C`. Values of `A | B` are normally produced by the spine elaborator via `widen_sum!(e)`. For explicit construction (the only path in [Kio'](prime.md)), the intrinsic injections `__left__ : [A][B] A -> A | B` and `__right__ : [A][B] B -> A | B` are available but **not in scope by default**: pull them in (along with the rest of the value intrinsics) with a top-of-file `import __intrinsics__;`. The friction of that declaration reinforces that `widen_sum!` is the usual surface path. Since `A | B | C` is sugar for `A | (B | C)`, the third branch is constructed as `__right__(__right__(c))`. The same applies to the product intrinsics (`__pair__`, `__fst__`, `__snd__`) and the sum eliminator (`__either__`) — those six, together with the branching intrinsic `__if_then_else__` (see [Conditionals](#conditionals)), are all gated behind the same `import __intrinsics__;`.

**The zero-component product.** The anonymous product with no components has type `.` and the sole value `()`. It is typically used as the return type of a function whose effect happens at the host boundary (`String -> .`). The positional non-collapsing rule still applies: `A & .` is a 2-component product distinct from `A`.

**The bottom type.** The type `!` has no values — no surface syntax constructs one, and no newtype or intrinsic ever produces one. `!` enters a program only as the return type of a host function that does not return (e.g., `fn panic(msg: String) -> !;`) or through the `__absurd__` intrinsic described below. `!` is a lexer token like `.`, not a user identifier, and cannot appear at the value level.

`!` is the **sum identity at the elaborator layer**: an elaborator may absorb `A | ! ↔ A` in either direction. The mirror at the product layer is `()` (the zero-component product): `A & . ↔ A` is likewise an elaborator-layer absorption. **Neither identity holds at type equality** — `A | !` and `A` are distinct types under structural comparison, just as `A & .` and `A` are; each pair is bridged only at an elaborator-call site, never by the type checker. Which coercions a particular elaborator admits is that elaborator library's contract; see [§ Elaborators are imported, not ambient](#elaborators-are-imported-not-ambient).

The asymmetry with type-equality identities is **deliberate**: Kio has no algebra of type equality. Associativity, commutativity, distribution, duplication/collapse, and identity absorption all live at the elaborator layer, not at type comparison. Where other languages teach the equality procedure to rewrite identities away, Kio's equality is purely structural and identity bridging is opt-in. One consequence: a direct sum value `s : ! | T` used where `T` is expected requires an explicit elaborator call — identity absorption (`! | T ↔ T`) is an elaborator-layer rewrite, so an imported coercion elaborator can bridge it.

**Elimination.**

- For `x : A & B`, project components with the `__fst__` / `__snd__` intrinsics (after `import __intrinsics__;`): `__fst__(x) : A`, `__snd__(x) : B`. For longer products, chain them (`__fst__(__snd__(x))` for the middle component of a three-tuple).
- For `x : A | B`, discriminate with the `__either__` intrinsic: `__either__(x, handle_a, handle_b)` picks the arm matching the injected branch. Surface pattern matching over `|` is the `match!` dispatch elaborator — see [Pattern matching](#pattern-matching).
- For `x : !`, the `__absurd__` intrinsic — `__absurd__ : [A](x: !) -> A` — produces a value of any type. The usual shape is `__absurd__(panic("boom"))` at a call site that expects some `T`. There is no introduction form to mirror this — `!` has no values, so there is nothing to construct.

**Two modes, scoped narrowly.** Per the bidirectional discipline (see [Bidirectional elaboration](#bidirectional-elaboration)), every typing position runs in either synthesize (⇑) or check (⇓) mode. Synthesize is the default — most positions read an expression and compute its type from its children, with no Hindley-Milner-style global unification and no inference variable that escapes its finite connected application or lexical owner. Check mode fires at specific positions where the surrounding context supplies an expected type: a binding annotation, a parent call's parameter slot, a return-type position, or a `fn` value-parameter or return-type slot. A polymorphic application may retain a blocked immediate argument while other arguments or connected nested results refine that finite local relation, but it does not create a third typing mode or a program-wide constraint problem. Check-mode flow does not cross binders or semicolon-separated block prefixes.

An **elaborator call** is an explicit surface bang call that asks a scoped elaborator implementation to synthesize Kio' glue at that call site rather than relying on ambient inference. The umbrella includes source-to-target coercion elaborators, the `match!` dispatch elaborator, and the `derive!` instance-deriving elaborator. This document uses "elaborator" as the shorthand once the family is clear, and specifies the call **mechanism** here; the rule set a particular elaborator admits is that library's contract (see [§ Elaborators are imported, not ambient](#elaborators-are-imported-not-ambient)).

At a source-to-target elaborator position the source type is synthesized from
the source expression, and the target is normally fixed by a surrounding
expected type or an explicit type argument before the implementation runs.
When an unresolved type variable occurs in the elaborator declaration's result,
the type of the single checked term returned by the implementation may instead
solve that already-existing result variable. This remains part of the same
finite application relation: the returned term cannot introduce an inference
variable or trigger replay of the implementation.

A source-to-target elaborator takes its target as a type argument that follows
the value argument — `name!(e, T)` — subject to the general `_`-or-elide
inference rule. It is a common reference-library use of Kio's general
successive-layer call shape: a later type-binder layer may follow a completed
value-parameter layer. The position keeps `name!(e)` reading as "coerce `e` to
the surrounding expected type" without giving elaborators a special calling
rule.

Related local elaboration positions use the same "known shape, generate glue" discipline but are not source-to-target coercions:

- `match!` clause dispatch — the dispatch elaborator selects a clause per DNF branch of the scrutinee's type and synthesizes the projection/injection glue that reconstructs each clause's argument. See [Pattern matching](#pattern-matching).
- **Surface forms that desugar to polymorphic intrinsic calls with elided type arguments.** The tuple literal `(a, b, …)` desugars to a `__pair__` chain whose leading `[A][B]` type arguments are not written; each operand constrains its corresponding value-parameter slot. Imported block elaborators likewise relate their projected operands through their resolved public types and, when declared, authenticated fills. Neither route searches declarations.

Ordinary function calls supply type arguments — fully explicit, partially
explicit with `_` placeholders, or fully elided — under the same finite
application-local rules. Written arguments, expected results, and connected
nested results constrain the call's owner-scoped goals. A value argument is
synthesized when it can complete independently, or checked after its parameter
slot becomes usable; an unannotated lambda receives its parameter type only
from that checking position. No goal escapes the connected call tree or crosses
a semicolon boundary.

The reference conditional requires a single result type: `if! cond { a } else { b }` checks
both arms against the surrounding expected type when one exists; otherwise its
two arm results form one finite symmetric common-result relation. Either arm
whose result is independently determined may establish or refine the common
result, and source order gives neither arm inference priority. If neither the
context nor either arm determines the result, inference is underdetermined.
Incompatible arm results are rejected rather than combined into an implicit
sum; write the sum construction explicitly in each contributing branch,
normally with `widen_sum!`.

Alternative-result inference is open-world. The reference conditional relates
exactly its two written arms through its public type; a fills-marked elaborator
may also supply its finite authenticated fill transcript. Neither route
consults ambient declarations, so adding an unrelated declaration to a module
body cannot add an alternative, nominate a result producer, or change the
common result.

**Annotations at bindings.** Every top-level binding (`fn`, `host fn`, `host type`, `type`, `newtype`, `labels`) carries an explicit type — these are public-contract positions where the type is part of the declared interface. Top-level positions accept no inferred `_` placeholder; the type must be written out. The label-specific exact payload `name: _` is instead an explicit reference to an earlier label declaration, as specified in [Labels](#labels). The **one exception** for inference is a `fn`'s return-type annotation, which may be elided when it would be `-> .` (see [Function definitions](#function-definitions)). Local `let` bindings may annotate the binding pattern: the unary form is `let .(x: T) = e;`, and tuple destructuring and as-patterns write the type leaves inside the pattern. A fully concrete local binding pattern checks the RHS against the pattern-derived type. An untyped or partially typed binding pattern leaves the RHS in synthesis mode; written concrete leaves are verified after the RHS synthesizes. For every partial local form, a source `_` is first classified under the post-materialization annotation rule below; a hole beneath an annotation-local structural `forall` is rejected before the RHS relation is opened. There is no let-generalization: inference never synthesizes a `forall` on its own; if you want polymorphism, you write it (as a `fn` or as a polymorphic `fn` with explicit `<…>` type-parameter slots).

**Type-argument inference and the `_` placeholder.** A call site's type-argument list may contain `_` placeholders for type arguments to infer. When the application-local equality relation over value arguments, connected nested results, and any expected result determines the inferred type uniquely, the call is well-typed. The solution may be a monotype or a complete polytype already present as one closed type tree in that finite evidence. Inference never constructs a new `forall` tree or guesses where binders belong, and a polymorphic value does not unify with a proper subterm of a declared monomorphic parameter shape. Kio has no subtypes or implicit coercions in this relation. Unsolved binders and conflicting equalities are compile-time errors. `_` placeholders may be dropped from the call's syntactic surface; full elision is accepted for any binder count, with the same rule.

This rule is open-world: its evidence is fixed by the resolved callee type, the
written connected application tree, and the immediately enclosing expected
type. It never consults an ambient declaration pool, so adding an unrelated
declaration to a module body cannot introduce another solution or change an
existing one.

The `_` placeholder occupies a whole type-argument slot — it cannot appear nested inside a larger type expression at a call site (e.g., `f(List(_), x)` is rejected). The user must either write the type-argument out fully (`f(List(I32), x)`) or drop the slot entirely and let inference fill it from value-arg unification (`f(_, x)`, or just `f(x)`).

**`fn` signatures and the `_` placeholder.** `fn` value parameters and the return type may carry annotations or `_` placeholders. `: _` on a value param may be dropped to write `(a)` for "value param `a` with type inferred from context"; `-> _` may be dropped to write `.(…)` for "return type inferred from context." Each parameter's annotation is independent — there is no all-or-nothing rule. Concrete annotations (`: T`, `-> R`) are verified against the bidirectional flow's expected type from the call site; a contradicting annotation is a type error. Beyond verification, a concrete `-> R` return annotation also *seeds* the body's expected type: when a `fn` sits in a synthesis-only position — no call-site expected type to flow in — its body is still checked against `-> R`, so an elaborator or other expected-type-consuming form in the body's tail position finds its target from the annotation the author already wrote, with no redundant restatement. This is bidirectional *checking*, not return-type inference — `-> _` and a dropped return annotation stay synthesized bottom-up. A `fn` with `_`-marked or dropped types must be applied or bound at a position that determines them; standalone `fn`-expressions with unconstrained metavariables are a compile-time error.

The `_` placeholder may appear nested inside a value-parameter or return
annotation on an anonymous lambda literal when the literal sits in a checked
position, subject to one local boundary. Transparent aliases are first
materialized through their already-resolved exact declarations. A written `_`
is rejected when any materialized occurrence lies strictly beneath a
structural `forall` introduced inside that same parameter or return annotation.
The diagnostic is
`` type placeholder `_` cannot appear beneath a `forall` inside an annotation ``;
the first enclosing binder is labeled
`this annotation introduces the enclosing binder`, with help
`` write a concrete type beneath this binder, or make the whole annotation slot `_` ``.

Header binders are outside their separate parameter and return annotation
trees, so whole slots remain useful:

```kio
.[A](x: _) -> _ { x }       // admitted: header binder, whole slots
```

By contrast, `.(f: [A] A -> _) { ... }` is rejected because the placeholder
is inside `f`'s own `forall` annotation. An identity or reordering alias keeps
the source occurrence, an alias that duplicates it rejects once if any copy
lands beneath such a binder, and a dropped occurrence creates no annotation
hole. Otherwise the bidirectional flow supplies the resolution: the annotation
is matched structurally against the expected type, every admissible `_` is
filled from the expected subtree, and concrete leaves must agree.
`.(pair: (_ & B)) { … }` passed where `((A & B)) -> R` is expected therefore
resolves the placeholder to `A`. This composes with parameter destructuring
patterns: a bare-name slot (`.((x, b: B))`) desugars to an outer annotation
`(_ & B)`, which the same rule handles. Synthesis-only positions (a
`let`-bound lambda, or another lambda whose callee is being synthesized)
supply no expected type, so an unresolved nested `_` there is a type error at
the placeholder.

Planning uses only the written annotation, exact resolved alias/import edges,
exact lexical binder proofs, and the local expected relation. It performs no
ambient declaration or spelling-selected candidate scan, so adding an
unrelated declaration cannot change admissibility or the resulting equation.
Literal-role selection remains the separately documented lexical selection
that occurs before an exact literal annotation enters this rule.

Top-level annotations otherwise remain unaffected — `fn` / `host fn` / `host type` / `type` / `newtype` / explicit `labels` payloads are public-contract positions and admit no inferred `_` anywhere in their signatures. The exact whole-payload `name: _` label-reuse marker names an already-declared nominal rather than inferring a new payload.

### Labels

A `labels` declaration introduces lowercase value-surface labels and generated nominal types. The generated type name is the label spelling with the first letter capitalized (`foo` → `Foo`):

```kio
pub labels { box[A] : A };
```

elaborates as if the module contained:

```kio
newtype Box[A] : A {
  pub constructor mk;
  pub projector get;
};
```

The generated type name is the only type-surface spelling. Use `Box`, `Box(Int)`, and products or sums such as `Box & Name`; lowercase labels and braced label forms are not type expressions. The label `box` remains value-surface syntax only, so it does not introduce a same-module value alias and does not reserve the ordinary value name `box`.

**Visibility and binding origin.** `pub labels` exports each explicit lowercase
label spelling for braced selective or qualified use as well as its generated nominal;
`pub(path)` restricts both spellings to that module subtree. A qualified module
alias remains usable outside the subtree for the module's public members, but
does not expose a restricted label through `{alias.label}`. Within a consumer's
label-syntax registry, `import source({item});` selects only label `item`, while
`import source(Item);` selects only its generated nominal type. Import both as
`import source(Item, {item});` when a module names the type and uses construction,
access, or update syntax. Each explicit braced label import may be written once;
repeating it is a Name error even when both imports name the same declaration.
Importing one label spelling from distinct declarations, or combining an
imported label with a same-named local label declaration, is likewise a Name
error after every written import and value-import cycle has been validated.
Label syntax and ordinary values remain separate namespaces, so a label and a
function may share a lowercase spelling; neither declaration order nor that
overlap changes which visible label origin a braced form uses.

**Forwarding a label family.** A standalone declaration introduces another
spelling for an existing label without generating a nominal type:

```kio
import original as source;
pub type {renamed} = {source.foo};
```

`renamed` denotes the same field identity, constructor and projector as the
explicit label at the end of that written route. The declaration introduces
only the braced label spelling, not `Renamed`, an ordinary value, or members.
Its target may be a local label, a braced selective import, or a label selected
through an explicitly imported module alias; chains must end at an explicit
label declaration. Missing targets and cycles without such an origin are
errors. Generic and existential parameters remain those of the original label
family: a forwarding declaration has no parameter list or type application.
It is a standalone Surface item, never a recursive type-group member.

The new spelling has its own documentation and visibility. Each written edge
must be ordinarily accessible where declared, and a forward cannot expose its
target beyond that target's visibility. Ordinary terminal nominal/member
access checks still apply. A forwarding module need not export the terminal
uppercase type; code that names that type imports it separately. Repeated
label introductions are errors even when their routes share one terminal.
Forwarding does not reserve a same-spelled ordinary value name.

Label elaboration consumes these declarations and uses ordinary explicit
imports and nominal member calls to the exact terminal. No forwarding record
or producer authority survives into Kio'. Resolution follows only written
label and import edges, so unrelated declarations in another module cannot
change a route or make an unused forwarding label affect an existing use.

**Construction.** A value is constructed with label syntax:

```kio
{box = value}
{ready=}
{name = n, age = a}
```

`{box = value}` lowers to `Box.mk(value)`. `{box}` is value-only sugar for `{box = box}`: the payload expression is the local value whose name is the label's last path segment, so `{m.box}` means `{m.box = box}`. `{ready=}` is the unit-payload shorthand for `{ready = ()}`. `{}` is the unit value `()`. Multi-label construction builds a right-spine product in written order, so `{name = n, age = a}` has type `Name & Age` and lowers to a `__pair__` chain of generated constructor-member calls. Duplicate labels in one construction expression are rejected.

**Named products and sums.** Anonymous `labels` declarations declare one product arm only:

```kio
labels { name : String, age : I32 };
```

Named declarations may define products and sums:

```kio
labels Person = { name : String, age : I32 };
labels Message = { text : String } | { code : I32, fatal : . };
```

The first declaration emits `Name`, `Age`, and `type Person = Name & Age`. The second emits `Text`, `Code`, `Fatal`, and `type Message = Text | (Code & Fatal)`. Duplicate labels inside one product arm are rejected. Anonymous sum declarations are rejected because sum syntax has only the named alias to attach to.

**Reusing an earlier label.** An entry with an explicit payload, `name: Payload`, declares a module-local label and mints its generated newtype. At a later source position inside a named `labels` declaration—whether a later arm of the same form or another declaration—the exact `name: _` spelling reuses that nominal:

```kio
labels Reply = { request_id: I32, ok: . } | { request_id: _, error: String };
```

The `request_id: _` entry contributes the `Request_id` declared in the first arm at that position in `Reply`; it emits no newtype and supplies no second payload to compare. Repeating `request_id: I32` is a duplicate declaration error even though the payload text matches. The diagnostic points to the first declaration and suggests replacing the later payload with `_` when that edit is sufficient.

Reuse is deliberately explicit and source-ordered. The original explicit declaration must occur earlier in the same module. A later declaration, a nonminting `type {name} = {target};` forward, an imported label, or a qualified label cannot satisfy a marker, and anonymous `labels { name: _ };` has no alias position in which reuse is meaningful. Visibility on the reusing declaration does not revise the original nominal; it applies only to the named alias being declared.

For a generic label, the marker repeats the universal binder arity and kinds, using parameters introduced by the enclosing named alias. Binder names need not match the original:

```kio
labels { value[*F][A]: F(A) };
labels Choice[*G][B] = { value[*G][B]: _, left: . } | { value[*G][B]: _, right: . };
```

A nullary marker cannot reuse a generic label. Existential binders belong only to the original explicit declaration and are rejected on a marker. Exact bare `_` is the marker only at the whole label-payload position; an `_` nested inside a larger payload remains an ordinary inference placeholder and is checked by the rules for that surrounding type position.

The marker is fully consumed by label elaboration. Kio' receives the original generated newtype and ordinary alias references to it, with no `_`, reuse record, or declaration history. Resolution consults only earlier explicit declarations written in the same module, so adding declarations to another module or later in this module cannot change an existing marker's meaning (see [Open-world design](#open-world-design)). The Kio'-level duplicate-newtype rule remains strict (see [`prime.md`](prime.md)).

**Recursive labels.** A label declaration is source ordered unless it carries the explicit `rec` marker. One `rec labels` declaration is an atomic generated-type scope: every nominal head generated by that declaration is visible across its payloads and a named declaration also sees its alias head. It does not see unrelated declarations later in the module. Recursive references use generated type names or the named alias:

```kio
rec labels { list[A] : . | (A & List(A)) };

rec labels Tree =
  { leaf: I32 }
  | { branch: Tree & Tree };
```

Every cyclic component produced by label elaboration must cross a generated nominal boundary and satisfy strict positivity. The generated newtypes remain iso-recursive boundaries. Values cross one with `{list = e}` or `List.mk(e)`, and leave it with `List.get(v)`, never by implicit unfolding. Omitting `rec` from a recursive declaration is an error with a fix that adds it; writing `rec` when no generated component is recursive is a redundant-marker error. See [Type declarations](#type-declarations).

**Field access.** `x.?{foo}` returns the unwrapped payload from the visible `Foo` slot of `x`; `x.?{foo, bar}` returns payloads in written order. `x.?{}` evaluates `x` once and returns `()`. Duplicate access labels are allowed and reuse the same visible slot. Visibility is mechanical: walk the receiver product's right spine into slots and choose the lowest-index slot whose type is the requested generated label type. There is no payload-shape search and no declaration-pool search.

**Field update.** `x.!{foo = y, bar = z}` evaluates `x` once, evaluates RHS expressions left-to-right, removes visible matching slots if present, and prepends replacements in written order. `x.!{foo}` is value-only sugar for `x.!{foo = foo}` by the same last-segment rule as construction; `x.!{foo=}` is the unit-payload shorthand. `x.!{}` evaluates `x` once and returns the receiver row. Updates are upserts: an absent field is added. Duplicate update labels are rejected.

**Expected-order checking.** Construction and update synthesize their default result in written order. When either form is checked against an expected product type that is only a permutation of the default row result, the checker accepts the form and emits the ordinary product spine in the expected order. This is a product-axis reorder only: no widening, narrowing, insertion, deletion, label search beyond the labels written at the site, or `fit!` composition. Access does not participate in this rule: `x.?{foo, bar}` unwraps the selected labels first and produces payloads in written order, so checking it against a different product order is a type error.

**Row-let binding.** `.{foo, bar} = x;` is a statement form that evaluates `x` once and binds payload locals from visible label slots for the continuation. Each entry may give an alias: `.{foo as payload} = x;` binds `payload` to `x.?{foo}`. The shorthand `.{foo}` means `.{foo as foo}`; qualified labels use the last segment, so `.{m.foo}` binds `foo`. Row-let lists are non-empty, and duplicate local names in one row-let are rejected.

**Enum-like sums.** A closed set of named alternatives is a structural sum of generated label types:

```kio
labels Color = { red : . } | { green : . } | { blue : . };
```

Values use ordinary label construction (`{red=}`) and widen into the named alias through the normal sum-construction elaborators; the named `labels` declaration does not introduce implicit sum construction.

**Neither `into!` nor `onto!` crosses label or host type boundaries.** The elaborator bridges structural rearrangements inside `&` and `|` (and, for `onto!`, forgets product components), but never manufactures or discards a generated label newtype, and never treats a `host type` as structurally equal to its consumer-supplied binding. To move between a payload type `X` and `Foo`, write `{foo = x}` / `Foo.mk(x)` to construct or `Foo.get(v)` / `v.?{foo}` to destruct; to move values in or out of a `host type`, call a host-provided conversion.

### Type parameters

A type parameter is introduced by writing **`[name]`** — Kio's surface syntax for a `forall` binder. There is no separate `forall` keyword; `[…]` is the only spelling. Binders may appear in two positions:

- **As a declaration-head group** (`fn`, `host fn`, `equiv`, parametric `type`, parametric `newtype` / `labels`), interleaved with ordinary value-parameter groups or as a leading run before any value parameters. The binder scope extends to the groups to its right and (where present) the return type:

    ```kio
    fn id[T](x: T) -> T { x }
    ```

- **As a prefix binder group inside a type expression**, scoping rightward over the rest of the type. The shape `[A1]…[AN] (P0 & P1 & …) -> R` denotes `∀A1 … AN. (P0 & P1 & …) -> R`. Use this whenever a polymorphic function type is needed *as a type expression* — for example, as a parameter type, the type of a `host fn` argument, the body of a `type`, or the expected type at a call site:

    ```kio
    type PolyId   = [T] T -> T;                  // ∀T. T -> T
    fn higher(f: [T] T -> T, x: Int) -> Int { f(Int, x) }
    ```

    `higher` is a rank-2 function: its first parameter has type `∀T. T -> T`, so callers must pass a value of polymorphic-function type. Type-expression binders are prefix-only; nesting function types and putting `Forall` in a return position express deeper rank.

Only those two structural positions interpret `[Name]` as a forall binder.
Fixed operators cannot contain square brackets. A variadic value expression
instead uses nonbare mirrored runs such as `[*` and `*]`, with its `varop`
binding locally declared or selectively imported. Type parameters follow the
**type-name rule** (an optional single underscore followed by an uppercase
initial), not the value-name rule — see [Naming conventions](#naming-conventions).

A callee's type binders do not scope over its caller's arguments. Written type arguments and completed argument types retain their caller-local lexical and nominal identities, including when a callee binder has the same spelling. Renaming a callee's bound type variable consistently cannot change a caller's acceptance or meaning.

**Position is semantically meaningful.** In a type expression, a binder run must appear before its body type and scope over that whole body; `[A] -> R` is rejected because there is no body between the binder run and the arrow. Product domains are written with `&`, not commas. In a declaration signature, type-binder groups and value-parameter groups still form an ordered sequence: a comma-separated value-parameter group folds its parameter types to the same right-associated product domain for one `Function`, and a following type-binder group scopes only over the groups to its right. The names are local binders for that declaration body, not part of the type. Each type-binder run (`[A][B]`, or input shorthand `[A, B]`) lowers to nested `Forall` nodes at its position. The trailing `-> R` consumes the right edge as the function's return type. A few worked type-expression shapes:

- `(P0 & P1 & P2) -> R` is `Function(Product(P0, Product(P1, P2)), R)`.
- `[A] (P0 & P1) -> R` is `Forall([A], Function(Product(P0, P1), R))`.
- `P0 -> [A] P1 -> R` is `Function(P0, Forall([A], Function(P1, R)))`.
- `(P0 & P1) -> [A] R` is `Function(Product(P0, P1), Forall([A], R))`.

`Forall` binds one type parameter. A source binder run is represented by nesting one `Forall` per binder, in source order: `[A][B] P -> R` lowers to `Forall([A], Forall([B], Function(P, R)))`.

Each nested `Forall` is also one ordered term-application boundary. A type
argument carries no runtime value, but applying it performs one β-TLam step and
may expose computation before the next type or value application. For example,
applying the value argument of `.(u: .)[A] -> A -> A { body }` produces a
polymorphic function value; `body` is not entered until `A` is applied.
Consecutive binders remain consecutive one-binder stages rather than one
aggregate stage. Backends may erase the representation of the type argument,
but not this evaluation boundary or its position relative to later value
arguments. Kio and Kio' use the same nested term and the same evaluation
semantics; lowering adds no Prime-only privilege. The rule follows only the
term's nested `Forall` structure and written applications, so it consults no
declaration pool and preserves open-world monotonicity.

Commas are not product type syntax. `A & B` is the type of the two-component product; `(A, B)` is not a type expression. Commas separate value parameters in signatures, value arguments in calls, tuple literal items, type-application arguments, and other list-shaped syntax. Thus `fn foo(x: Int, y: Int) -> R` and `fn baz(x: Int & Int) -> R` both have product-domain type `(Int & Int) -> R`; `foo`'s body sees separate binders `x` and `y`, while `baz`'s body sees one binder for the whole product.

**Same call to a known callee.** `foo(3, 5)` and `foo((3, 5))` both pass `Product(3, 5)` to `foo`. Likewise `baz(3, 5)` and `baz((3, 5))`. Call sites are structural, not name-driven. Higher-order positions use the product-domain function type; ABI wrappers may still adapt between a backend's positional call convention and the local product value.

The zero-component product is the unit value `()`, whose type is `.`. A
completely empty direct/prefix argument list contributes that Unit value. A
function type `. -> R` has unit domain, so direct `f()` and `f(())` both pass
the unit product to a monomorphic unit-domain function. UFCS has no written
empty-list form: omit the list when the receiver is the only value, or write
`(())` to pass an additional Unit; see [UFCS](#ufcs). Extra parentheses around
the unit type are grouping: `(.) -> R`
is the same function type. Multiple unit parameters in a signature fold to
`. & .`, so `fn f(a: ., b: .) -> R` can be called as `f((), ())` or
`f(((), ()))`.

**Within-layer partial application is rejected.** A function whose type is `(A & B) -> R` requires the call site to build the whole `A & B` product. Calling `f(x)` where `x : A` (or any type not equivalent to `A & B`) is a type error — the structural mismatch between `Product(x)` (the call's right-fold of one positional arg of type `A`) and `Product(A, B)` (the function's `param`) is what the typer rejects; there is no implicit currying within a single layer that would supply the missing `B`-typed component. To get a fewer-arg function, the user wraps explicitly (`.(x) { .(y) { … } }`).

This rule is structural, not arity-based: `f(p)` where `p : A & B` is valid — it's the dual of `f(a, b)` covered above, both passing the same `Product(A, B)` to `f` (a one-arg call form passing a product, vs a two-arg call form whose args right-fold into the same product).

**Curry layers are distinct.** `(A & B) -> C` and `A -> B -> C` are different types: one application of the first consumes the whole product, while one application of the second consumes only `A`.

**One written call may cross successive layers.** Surface Kio permits one
parenthesized argument list to continue through successive `Forall` and
`Function` layers of the resolved callee type. This is structural application,
not currying within a layer: every function layer still consumes its complete
product domain before the call advances to the next layer. An explicit second
application, such as `f(pair)(tail)`, ends the first argument packet and makes
that boundary unambiguous.

Within one written list, packet selection is deterministic and greedy. After
eliding or consuming any type binders at the current position, each value that
is followed by another value consumes exactly the next right-spine product
slot. Only the final value before a written type argument, an explicit next
application, or the end of the list may fill the current layer's whole
remaining product packet. The typer chooses those slots before checking value
types and never retries another packing after a mismatch.

For a callee of type `(A & B) -> C -> R`, `f(a, b, c)` consumes the `A`, `B`,
and `C` slots in order. If `pair : A & B`, `f(pair)(c)` explicitly passes the
whole first packet and then applies the `C` layer. In `f(pair, c)`, by contrast,
`pair` is a non-final value and is checked against `A`; its product type does
not cause the typer to reinterpret the call. A mismatch points at the chosen
slot, and when an explicit application boundary would express the likely
packed intent, the diagnostic suggests that spelling.

**Design consequences of right-associated product tuples.** Tuple literals, call argument lists, function value groups, dot-splice calls, parameter patterns, `match!` clause parameters, and bang-call value arguments all use the same right-associated binary-product convention. There is no one-element tuple case in any of those surfaces; a single item is grouping or a single value slot. UFCS/dot-splice inserts a value into the ordinary argument stream, and if that value has product type it fills the same product parameter a parenthesized tuple or comma-separated argument list would fill. Code generators may flatten product spines at the host ABI boundary so a host language sees positional arguments, but that is ABI canonicalization: type equality and local function values continue to use the product type.

Type arguments are passed **positionally, alongside value arguments**, in a single argument list:

```kio
id(String, "hello")           // id instantiated at String, applied to "hello"
higher([U] U -> U, .[V](y) { y }, 42)
                              // pass a polymorphic identity through `higher`
```

Type parameters and value parameters share a single declaration list; they are told apart by how each one is written (`[Name]` versus `name: T`). In call-argument position, an identifier matching the type-reference word rule, such as `A`, `_A`, or `__Item_type`, is a type argument; an identifier matching the value-reference word rule, such as `x`, `_x1`, or `__item_value`, is a value argument. Both rules allow underscore affixes, require each word to begin with letters and end with any digits, and separate words with one underscore. The first letter determines the role: uppercase for types, lowercase for values; all later letters are lowercase. Reserved leading underscores do not grant a declaration or resolution privilege. A spelling matching neither role is rejected rather than falling back to the other namespace. That classification is syntactic and does not consult the set of declarations in scope, preserving open-world monotonicity.

Unit is not ambiguous in this mixed list: `()` is only the Unit value and `.`
is only the Unit type. A type slot cannot consume written `()`. Thus
`id(., ())` supplies the Unit type explicitly, while `id(())` supplies one
Unit value and infers the type from it.

Explicit type arguments consume only the `Forall` binders they name. A
nonempty argument list containing only type arguments, including `_`
placeholders, is a **type-only packet**: it returns the residual value function
without inventing a Unit value application, even when substitution or
transparent-alias unfolding exposes a `. -> R` function. For
`fn id[A](x: A) -> A { x }`, `id(A)` has type `A -> A`; `id(A, x)` has type
`A`; and `id(x)` elides the type argument, infers `A` from `x`, and applies the
value argument. In particular, `id(.)` has type `. -> .`, while `id(., ())`
has type `.`. For `nil : [A] . -> List(A)`, `nil(A)` has type
`. -> List(A)`, while `nil(A)()`, `nil(A, ())`, and `().>nil(A)` each have type
`List(A)`.

A syntactically empty direct/prefix surface call `f()` is different. It omits
any leading type arguments and writes one Unit value, as though the omitted
type slots and `()` were supplied in the same call. Thus `id()` infers `A = .`
from its written Unit value and has type `.`, just like `id(())`. The `nil`
above can be written `nil()` only in a local checked position that solves `A`,
such as `let .(xs: List(Int)) = nil();`; it elaborates to the explicit Kio' call
`nil(Int, ())`. Without a local constraint that solves `A`, `nil()` is a
type-inference error.

UFCS inserts its receiver before the resulting prefix call is checked. A bare
UFCS callee contributes no arguments beyond that receiver: `r.>f` means
`f(r)`. An explicitly empty UFCS list is rejected because it could be read
either as no additional argument or, by analogy with direct `f()`, as an
additional Unit argument. Remove the list (`r.>f`) when the receiver is the
only value, or write the Unit explicitly (`r.>f(())`) when the call also passes
Unit. The form `().>nil(A)` saturates because its receiver is the written Unit
value.

These decisions use only the resolved callee type, the written argument list,
and local type equalities. Declaration parameter groups, runtime ABI arity,
callee identity or provenance, and a user elaborator's private ABI cannot
change whether a call is type-only or value-applying. Alias unfolding may
validate the type of a written value application, but cannot create that
application. When both applications are written, the type application occurs
first and the Unit value application second. Adding an unrelated module-body
declaration changes none of these finite inputs, so it cannot change an
existing call's completion or type.

A bare polymorphic value is not silently instantiated by an expected
monomorphic function type. For `id : [A] A -> A`, bare `id` retains that
polytype, while `id(_)` explicitly asks inference to instantiate `A` and
returns the residual function `A -> A`. Inference may solve that written `_`
from an expected function type, but doing so does not retroactively create a
value application.

A binder run exposed **after the written call has consumed one or more value
layers** is different. If that returned leading `Forall` sits in a position
with a concrete monomorphic expected result, the expected result may
instantiate the returned binders. For `produce : Seed -> [Result] Result`, a
body declared to return `Witness` may write `produce(seed)` and infer
`Result = Witness`. With no expected result, or with a polymorphic expected
result, the returned binder run remains in the residual type. A written `_`
forces that type slot to be solved, and a written type applies it directly.
This leading-versus-returned distinction is identical for ordinary functions,
intrinsics, prefix and UFCS calls, and blockless elaborator bang-calls; only
the elaborator's compile-time execution and Kio' substitution are phase-specific.
Elaborators with trailing descriptors instead require the complete direct
block-call form, with inferred type arguments, described below.

There is no separate `[ ]`-style type-application syntax. Type-argument lists may be written in three forms: fully explicit (every type arg written), partial with `_` placeholders for individual type-args to infer, or fully elided. The typer infers `_`-marked or elided type-args from the finite application-local equality relation over value-argument types, connected nested results, and (when present) the expected return type. Kio has no subtyping or implicit coercion in this relation. Unsolved binders and conflicting equalities are type errors. See § Annotations at bindings and § Type-argument inference and the `_` placeholder for the full rule.

**A Church-style fragment of System F-ω.** Kio admits rank-N polymorphism (every binder is explicitly written, so type-checking remains decidable) and **higher-kinded quantification** — a binder may range over a type *constructor*, not only over a saturated type. What it stops short of is full F-ω: there is no anonymous type-level abstraction (`λα. T`), and the kind language has no kind variables and no higher-order kinds. A parametric alias must be applied to exactly its declared number of parameters: its bare and undersaturated spellings cannot retain unfilled alias parameters. See [Type and literal aliases](#type-and-literal-aliases) and § Higher-kinded types below.

### Higher-kinded types

Kio expresses **higher-kinded types** through kind-annotated binders and direct type application — the same model as [Kio'](prime.md#higher-kinded-types), described here for the surface reader. The kind language `κ ::= * | κ → κ` (see [`grammar.md` § Kind grammar](grammar.md#kind-grammar)) records type-level arity; a binder's kind is the count of leading `*` in its annotation:

| Binder | Kind | Role |
|---|---|---|
| `[A]` | `*` | ordinary type-parameter (default) |
| `[*F]` | `*→*` | arity-1 type constructor |
| `[**G]` | `*→*→*` | arity-2 type constructor |

**Direct type application.** A kind-`*→*`-or-higher head applies to type arguments with ordinary call syntax: `F(A)` where `[*F]` is a kind-`*→*` binder and `A : *`; `Either(String)` where `Either` is an arity-2 newtype. There is no `__App__` wrapper and no application keyword. Kinds compute by consuming one arrow per argument — `Either` has kind `*→*→*`, so `Either(String)` has kind `*→*` and `Either(String, I32)` has kind `*`. Applying a kind-`*` head to an argument, or supplying more arguments than the head's arrow count, is a kind error.

Every type that classifies a runtime value must be saturated to kind `*`. This includes function parameters and returns, newtype payloads, annotations, and every member of an anonymous product or sum: `F(A) & G(A)` and `F(A) | G(A)` are well-kinded when `F` and `G` have kind `*→*`, while bare `F`, `F & G`, and `F | G` are kind errors. A constructor may remain higher-kinded only where another type constructor explicitly expects that kind, such as the `F` argument in `Monad(F)`, or as a partial application such as `Either(String)`. This rule follows only the written binder kinds and the selected nominal declaration; adding unrelated declarations cannot change it.

A transparent alias reference must supply every parameter declared by that alias. Thus `type Pair[A][B] = A & B;` admits `Pair(String, I32)`, while `Pair` and `Pair(String)` are type errors. This exact-arity rule concerns the alias declaration's own binders; ordinary transparent expansion determines the resulting kind of an exactly applied alias. For example, with `type Unary[E] = Either(E);`, `Unary(String)` supplies the alias's only parameter exactly and has kind `*→*`, inherited from the partially applied `Either` body.

**Kinds are explicit and immutable.** A newtype's signature is its declared parameter list, each parameter carrying its own kind: an all-kind-`*` newtype `Either[E][A]` accepts two kind-`*` arguments, while a dictionary newtype `Monad[*F]` accepts one kind-`*→*` argument. A type application checks each argument against the matching parameter's kind. A binder's kind is the annotation the user writes. Nothing is inferred from how a type is used, and a newtype's kind never changes after its definition — this is what preserves open-world monotonicity (see § Open-world design): adding a new source declaration cannot retroactively alter the kind of an existing one. Kind annotations are part of a type's public signature.

**Complete polymorphic function schemes are value types.** One or more `Forall` layers ending in a `Function` form a complete function scheme. The whole scheme has kind `*`, even when one of its binders is higher-kinded, and may therefore appear anywhere a runtime value type may appear: as a parameter or return, inside a product or sum, in an annotation or type argument, as an anonymous function literal's type, or inside a recursive or existential newtype payload. This is a structural rule, not a privilege of a named declaration; unfolding a transparent alias does not change the classification. A higher-kinded `Forall` whose body does not ultimately reach a function is incomplete and is rejected; bare and undersaturated constructors remain non-value types. Newtype existential binders themselves remain kind `*`. The check depends only on the written type structure and explicit binder kinds, so adding unrelated declarations cannot change an existing type's admissibility.

**Higher-kinded application without primitives.** Lifting a value into and projecting it out of a kind-`*→*` position uses ordinary language constructs, not intrinsics. (How a backend carries that kind identity through to host code — a native host type constructor, or the transparent erased rep — is a backend-encoding concern, described in [`backends/README.md` § Higher-kinded types](backends/README.md#higher-kinded-types); at the language level there is only the kind-annotated binder and direct `F(A)` application.)

- At a **concrete type constructor** (a named kind-`*→*` newtype like `Box[A] : A`), the newtype's own constructor / projector pair crosses the boundary: `Box.mk_box(x)` lifts, `Box.un_box(b)` peels. A *canonical* type constructor — payload structurally equal to its single type-parameter, the `Box[A] : A` shape — is a zero-cost optimization the codegen recognizes transparently; the user writes the constructor / projector regardless.
- At an **abstract type constructor** (a kind-`*→*` binder `[*F]`), the lift / projection routes through an explicitly-threaded instance value. A combinator over `[*F]` that needs to inject takes an `Applicative(F)` parameter and dispatches through its `pure` member; one that needs to project takes a `Comonad(F)` parameter and dispatches through `extract`. Instances are explicit `fn` parameters — Kio has no typeclasses — exactly as in the functor / monad dictionary goldens.

**Polymorphic newtype payloads.** A newtype payload may itself be a polymorphic function — the dictionary pattern that underwrites `Functor[*F]`, `Monad[*F]`, and related abstractions. The payload shape is a `Function` nested under one or more unary `Forall` binders, where the binders scope over the args and ret:

```kio
pub newtype Monad[*F] : [A][B] (F(A) & (A -> F(B))) -> F(B) {
  pub constructor mk_monad;  pub projector bind;};
```

The constructor accepts a polymorphic lambda literal whose binders match the payload's; the projector yields a value whose polymorphism survives projection (`Monad.bind(F, monad)` at an abstract `[*F]` produces a value with the same shape as a top-level polymorphic `bind` fn). The imported `do!` accepts that value as its supplied bind function: `do! Monad.bind(F, monad) { … }` uses the same sequence projection as `do! top_level_bind { … }`. Per-backend codegen must support this pattern. A type-erased backend erases the bound types and carriers while retaining the payload's staged callable shape; the newtype constructor / projector remain runtime identities around that complete payload. A native-higher-kinded backend stores the polymorphic field directly. See [`backends/README.md` § Polymorphic newtype payloads](backends/README.md#polymorphic-newtype-payloads) for the cross-cutting scheme.

**Multi-arity type constructors and partial application.** A multi-parameter newtype is a higher-arity constructor whose partial applications are themselves higher-kinded types. `Either[E][A] : E | A` has kind `*→*→*`; `Either(String)` has kind `*→*`, so `Monad(Either(String))` is well-kinded. See [`docs/guides/higher-kinded-types.md`](../docs/guides/higher-kinded-types.md#multi-arity-type-constructors) § Multi-arity type constructors for the worked `Monad(Either(String))` example.

This partial-application rule does not permit a transparent alias reference to omit any parameter declared by that alias.

### Elaborators are imported, not ambient

Every elaborator is an ordinary module item resolved through scoped imports — there is no built-in coercion, no ambient dispatch, and no language-level palette of named forms. A blockless call writes `name!(...)`; an elaborator with trailing-block descriptors uses `name! … { … }`. The bang must be adjacent to the name. Parsing recognizes either shape without consulting declarations; resolution selects the ordinary imported elaborator (see [`grammar.md` § Surface item additions](grammar.md#surface-item-additions)). The reference `elab` library supplies `if!`, `scope!`, `do!`, `match!`, `derive!`, an **algebraic** coercion palette (`iso!`, `into!`, `onto!`, `align!`, `ease!`, `atom!`), and a **spine** palette (`reorder_sum!`, `reorder_prod!`, `narrow_sum!`, `narrow_prod!`, `widen_sum!`, `widen_prod!`, `flatten_sum!`, `flatten_prod!`, `one_sum!`, `one_prod!`, `fit!`). These are ordinary Kio declarations, imported explicitly. Their per-form algorithms are documented with the executable library in [`docs/poc/elab.md`](../docs/poc/elab.md); [`docs/poc/optics.md`](../docs/poc/optics.md) demonstrates the spine palette and `match!`.

**Named-callable contract.** Top-level `fn`s, intrinsics, and elaborator names are all looked up by name and expose a declared call type (a polytype). At a call site, explicit type arguments, `_` placeholders, value arguments, and the surrounding expected type are all solved against that call type by the ordinary call-type rules of § Type system. The difference is that an elaborator's bang-call runs compile-time implementation code over **reflected arguments** rather than the runtime function-call path; the ordinary and fills-marked reflected ABIs are documented below and in [`docs/guides/elaborators.md`](../docs/guides/elaborators.md).

**Trailing-block declarations.** An elaborator may declare one or more final
value arguments with `trailing product;`, `trailing thunk;`, or
`trailing sequence;`. The first descriptor is unlabelled; every later descriptor
has a distinct value-shaped label, for example `trailing thunk else;`.
`impl` and `captures` retain their unordered entry policy; the relative order
of the `trailing` entries determines the order of blocks. Descriptors belong
to final value slots in the resolved public call type, never to the private
implementation ABI. A descriptor whose projection cannot inhabit its public
slot is a type error.

| Descriptor | Value projected from the block |
| --- | --- |
| `product` | Independent expressions: no entries give unit, one gives that expression's value, and several give their right-associated product. Top-level `let` and `<-` bindings are rejected. A local binding inside a scoped entry cannot extend into another entry. |
| `thunk` | An expression-block body wrapped in a zero-argument function. Empty gives a thunk returning unit. Ordinary local bindings are admitted; `<-` bindings are rejected. The elaborator determines when the thunk executes. |
| `sequence` | An ordinary function with structural type `([A][B] (F(A) & (A -> F(B))) -> F(B)) -> F(R)`, parameterized by the bind function. Ordinary lets, `<-` bindings, and sequenced expressions precede an explicit final `F(R)` result. Empty and statement-only sequences are rejected; no implicit lift is inserted. |

The neutral block grammar admits semicolon runs at either edge and between
entries. A final semicolon does not discard a real final expression. Descriptor
validation determines which item forms are admitted. Sequence projection and
its binding rules are detailed in [Monadic do blocks](#monadic-do-blocks);
they apply to every `sequence` descriptor, regardless of the elaborator's name.

**Block calls.** Every declared block is supplied exactly once, in declaration
order. The first is unlabelled and later blocks carry their declared labels.
A block call is a complete direct call through a single value-shaped name.
Its prefixes contain values only; all public type arguments are inferred.
It admits neither UFCS nor partial application, nor a blockless alternate
spelling supplying the projected arguments directly. The public type fixes
the prefix and block slots before any implementation action runs.

`name! value { … }` has one prefix, `name!(a, b) { … }` has two,
`name!((a, b)) { … }` has one product prefix, `name!() { … }` has one
unit prefix, and `name! { … }` has none. A bare prefix reserves its next direct
brace for the enclosing call; this reservation survives operator slots,
including `___`. Group a nested block call, lambda, or row value when it is
part of that prefix. A blockless nested call remains blockless:
`outer! convert!(value) { … }`. See [`grammar.md`](grammar.md#surface-expression-additions)
for the complete expression boundary.

Only the final labelled block may elide its braces, and only when its whole
body is another block call. Thus `if! a { x } else if! b { y } else { z }`
is the generic abbreviated spelling of
`if! a { x } else { if! b { y } else { z } }`; neither `if` nor `else`
has a privileged parsing role.

Malformed syntax is a parse error (`11`). Import and name failures retain
their ordinary categories (`12`/`13`). Once the elaborator is resolved,
missing, duplicate, unknown, or out-of-order blocks, invalid block contents,
and descriptor/public-type mismatches are type errors (`14`), with source and
declaration context. An ignored non-unit sequence step explains the required
`F(.)` and suggests `<-`. Library-reported errors retain their ordinary
`14`/`15` distinction; these categories impose no diagnostic precedence.

Parsing and formatting depend only on written syntax. Block projection depends
only on the resolved declaration's descriptors and public type, and generated
terms obey the ordinary checking, purity, hygiene, and recursion rules.
Adding unrelated declarations cannot reinterpret an existing call. Descriptor
metadata, neutral blocks, and provisional projection state are consumed before
Kio'; none grants persistent authority to the resulting ordinary term.

**Explicit implementation dependency.** An elaborator declaration chooses one
of two implementation schedules. In both modes, the field names its
implementation with an ordinary lexical value path. A bare path may name a
local function or a selectively imported function. A dotted path may select a
function through an explicitly imported module alias (`helpers.build`) or a
constructor/projector on a local or imported newtype, directly or through an
identity alias (`Box.make` or `types.Wrapped.make`). Slash-qualified package
FQNs such as `lib/helpers.build`, calls, and inline lambdas are not
implementation targets; use an explicit `import` and name an ABI-shaped `pure fn`
instead.

Resolution matches an ordinary path expression's source-order classes. A
same-module ordinary or host `fn`, `rec` member, newtype head, or identity-alias
head must precede the declaration. A self-qualified alias or selective
self-import does not turn a later declaration into an earlier binding.

The ordinary field `impl f;` retains the existing late schedule and ABI:

```text
(__Comptime__ & captures... & reflected declared slots...)
  -> __Checked_term__
```

Every declared value slot is a completed `__Checked_term__` before the
implementation runs. A declared type slot is `__Type__`, or `__Type__ | .`
only under the existing optional-result-binder rule. The returned checked
term is related to the declared result and may solve its preallocated result
goal. Ordinary implementations receive no fill context and otherwise behave
exactly as before.

The declaration-local field `impl(fills) f;` selects the fills schedule and
this ABI:

```text
(__Comptime__ & __Fill_ctx__ & captures... & reflected declared slots...)
  -> (__Checked_term__ & __Fill_ctx__)
```

`__Comptime__` remains the immutable leading proof. The compiler-created,
call-local `__Fill_ctx__` is exactly second and is the only threaded fill
state. Captures keep declaration order; every declared type slot is exactly
`__Type__`, including an open declared result represented by an opaque handle;
and every value slot is `__Checked_term__`. The implementation returns one
checked term paired with the final context and never returns `__Comptime__`.
Its checked term is validation-only with respect to an open marked result: an
implementation that determines that result records an explicit fill. The
implementation may return the original context unchanged; the marker supplies
fill authority but does not require an appended relation.

The fills schedule evaluates once after the complete structural headers of all
declared value operands are ready, in declared-slot and written-source order.
Nested product headers and lambda headers that are complete without entering
their bodies are available while a body that still needs contextual typing
remains suspended; the retained body resumes later through ordinary
typechecking. Before the implementation runs, every computed value source must
have a complete projected type, whether or not it retains producer work and
even when the implementation ignores it. Explicit products apply this rule
recursively to their components. Complete types contain no unresolved inference
goals or source placeholders; declared generic variables and explicit universal
binders remain valid. The enclosing expected type and already available sibling
constraints from the resolved public call may complete these types before
expansion. A later fill or later callback-body checking cannot supply missing
information across this boundary. An ordinary call may determine its
public result from a callback's complete written function signature without
entering that callback's body. The signature contributes the ordinary equation
at the already-selected argument slot; it does not select a different grouping
or infer a missing annotation. A closed result may pass through enclosing
ordinary calls while their source terms remain pending. The callback body is
still checked once before publication, including when the elaborator ignores
the operand. No body, nested elaborator action, or immediate-lambda callee is
entered merely to complete such a header. A direct lambda is the
structural exception: its complete universal and value-parameter
header may precede an open final body-result subtree, but no unresolved source
placeholder may remain. Any other open value source is a type error
at that source before the implementation action runs. Binding the whole source
in an ordinary `let` completes its ordinary body checking first, but does not
by itself supply a missing generic choice. A complete binding annotation,
explicit type argument, or concrete callback result type can supply that
information where appropriate. Compile-time reflection
authenticates marked opaque type handles before reduction. After aliases
unfold, `__type_args_fold__` and
`__type_apply__` remain residual when their outer type or head is unresolved;
they do not substitute a zero-argument fold result or a no-op application. The
formal boundary is specified in
[elaborator evaluation](formal/elaboration.md#74-elaborator-bangs). During this
staging, a directly synthesized lambda with explicit type binders may omit its
return annotation or write `-> _` when its written binder and parameter header
is complete. Its provisional final result belongs to the surrounding scope,
outside the lambda's newly introduced type binders. The implementation may
observe that open header and return ordinary fills, but an independently known
result must select checking before the lambda body is entered. An enclosing
expected result or the finite fill relations may supply that result; the body
does not infer it, and an ignored source still needs it. If the result remains
undetermined, reject at the lambda before body checking or publication.
A result depending on a newly introduced lambda binder instead requires a
concrete return annotation or a complete expected function type. Both existing
routes, monomorphic lambda synthesis, and the unit default for omitted
top-level function returns are unchanged. This staging is
selected only by `impl(fills)`: the private ABI never changes the public call
plan, grouping, residualization, UFCS placement, or inferred call type.

A direct call to the normally resolved canonical reserved product constructor
is transparent during this marked staging only when its resolved intrinsic
kind and complete scheme prove exactly two type binders, two value operands,
and the ordered result `(A & B)` corresponding one-to-one to those operands.
The original call is recomposed, completed, and published once. Product-shaped
ordinary functions remain opaque even when their public type is
alpha-equivalent, because an ordinary function may reorder, duplicate, or
discard its inputs. This boundary follows resolved intrinsic kind and the
complete scheme, never a declaration spelling, module identity, result shape
alone, or raw source position.

A named implementation declared in the elaborator's defining module may remain
private regardless of the elaborator's visibility, and any implementation may
call private helpers in its own module. A target declared in another module
must be ordinarily importable into the defining module through a written
`import`, but it need not meet the elaborator's outward visibility. These
functions are compile-time implementation details, not declarations a caller
imports. A newtype constructor or projector keeps its ordinary member type and
cannot satisfy either elaborator ABI; use an ABI-shaped function instead.

The implementation path follows only the declaration module's lexical scope
and its written `import` clauses. Those imports are the only cross-module
dependency edges created by the target: a selectively imported function or an
explicit module alias supplies the binding before any dotted member is
selected. Resolution never scans other modules or treats a slash-qualified FQN
as an expression. Adding an unrelated declaration therefore cannot rebind the
target or alter an existing elaborator; a same-scope duplicate is rejected as
a declaration conflict rather than silently taking precedence. Fully qualified
paths remain available on the separate surfaces that define them, including
type paths, REPL and Kiodoc name queries, and dynamic-loader identity strings.

Every item named by `captures` must have visibility equal to or wider than the elaborator declaration. Captures differ from implementation helpers: a captured type becomes a quoted `__Type__` descriptor and a captured value becomes a quoted `__Checked_term__` that the implementation may place in the generated runtime term. A capture does not execute the captured declaration at compile time, so it may name a host type, host function, or other runtime-provided item. The visibility floor applies because that dependency can survive the bang call; it does not grant the implementation general access to private declarations.

**Fill and specialization helpers.** `import __comptime__;` retains the existing
proof and helpers and additionally introduces the opaque type `__Fill_ctx__`
and these values:

```kio
__fill__ :
  (__Comptime__ & __Fill_ctx__ & __Type__ & __Type__) -> __Fill_ctx__

__term_specialize__ :
  (__Comptime__ & __Checked_term__ & __Type__ & __Type__)
    -> (__Checked_term__ | (. | __Diagnostic_text__))
```

Every callable in the block retains `__Comptime__` as its first value
parameter. Of those callables, only `__fill__` also takes `__Fill_ctx__`
second and returns the descendant context.

`__fill__(ct, fills, destination, candidate)` does not solve or inspect
inference while the implementation executes. It returns a new context with
one relation appended; discarding that returned context discards the entry.
The destination must be an authenticated occurrence of this marked call's
declared result; an arbitrary reflected type, source operand, or constructed
clause tuple is not a fill destination. The candidate may retain staged body
provenance but gains no destination authority from it.

The boundary authenticates the complete returned transcript before solving
it. Explicit Kio state threading preserves authored entry order, but that
order grants no inference priority. Entries for one destination equivalence
class form a bounded symmetric relation: a destination already fixed by an
explicit or enclosing type checks every candidate; otherwise all equations
between the open destination and its candidates are registered together. Any
candidate whose result closes independently may determine or refine the
destination, after which every other candidate must agree. A malformed entry
fails rather than being discarded, no entry is privileged by source position,
and a group with no determining context or candidate is underdetermined.
Conflicts and underdetermination are reported as failures of the common-result
relation without treating one candidate as authoritative. The complete
relation, retained-body writes, and marked result are committed atomically.

`__term_specialize__(ct, term, pattern, target)` specializes only the term's
explicit leading universal binders in an isolated exact structural relation,
not subtyping or containment. It returns a
specialized checked term on success, unit for an ordinary structural route
miss, or diagnostic text for malformed or underdetermined inputs. It neither
takes a fill context nor appends a relation. The successful term's reflected
type is the fully instantiated whole function scheme. Ordinary code may use it
with closed operands, but only a marked implementation receives provisional
operands and a compiler-created fill context.

Neither capability exposes solver state, source indices, declaration
identities, or transcript contents. `__Fill_ctx__` has no source constructor,
cannot be converted to or from `__Comptime__`, and cannot be serialized or
retained in generated runtime code.

**Compile-time purity and runtime result.** An `elab` declaration has no `pure` modifier. Its implementation always executes in a pure context: a named implementation must be a `pure fn`, and every ordinary function reached transitively while evaluating it must also be declared `pure`. Local type annotations and implementation ABI types may mention any well-formed type, including host types; named types in the elaborator call type obey the declaration-visibility floor. Captures remain quoted inputs subject to their separate visibility rule above. This separates the code that computes a term from the term it computes.

A bang call in a `pure fn` is therefore not accepted or rejected from a purity bit on the elaborator. The implementation runs, its checked term replaces the bang call, and the assembled Kio' package is validated again. If the generated term contains a host-function or unmarked-function reference, that final check rejects it in a pure caller; the same term remains admissible in an ordinary caller. A generated pure term is admissible in either.

**Generated terms are hygienic.** The Kio' tree an elaborator emits is constructed through reflected term constructors, not by reaching into the caller's scope:

- generated elaborator terms are hygienic — they bind and reference only what the elaborator builds, independent of names in scope at the call site;
- a caller is **not** required to `import __intrinsics__;` for the intrinsic calls an elaborator emits on its behalf — the elaborator's output carries the intrinsic reference directly;
- the reflected term constructors are split into core `term_*` constructors (for core-language terms) and intrinsic-call `__intrinsic_*__` constructors (for the reserved intrinsic values);
- the `__intrinsic_*__` constructor names mirror the `__intrinsics__` value names — each `__name__` intrinsic has the constructor `__intrinsic_<name>__` (`__pair__` → `__intrinsic_pair__`).

**Target type-argument.** A source-to-target elaborator takes its target as a
type argument *after* the value argument — `name!(e, T)` — subject to the
general `_`-or-elide rule (see § Type-argument inference and the `_`
placeholder): explicit `T`, a `_` placeholder, or a fully elided slot. An
explicit `T` and a surrounding expected type must be `equiv`-equal. An elided
target is normally filled by the surrounding expected type; for a
type-producing elaborator, the checked term returned by its implementation may
instead determine the declaration's pre-existing result variable. If neither
route determines one type, the call reports "cannot infer type argument." Each
name also has the [receiver-first UFCS spelling](#ufcs) `r.>name!(T)` (or
`r.>name!` when no target argument is supplied), equivalent in AST and
semantics.

**No surface form survives into Kio'.** The typer records each elaborator-call elaboration in the `Elaborations` side channel; the substitute pass swaps the recorded Kio' tree in at the Lowered → Prime boundary. The declaration mode, `__Comptime__`, `__Fill_ctx__`, fill transcript, provisional terms, and compile-time helpers are all consumed before that boundary. The ordinary `pure fn` marker remains on Kio' function declarations so the fresh Prime validation can check the substituted body without hidden surface state. `kio-prime` rejects every elaborator declaration and bang-call at parse time — see [`prime.md` § What's not in Kio'](prime.md#whats-not-in-kio). [Kio'](prime.md) has no elaborator and no sugar: identity absorption and reassociation happen only at elaborator-call sites, never at type comparison.

**Open-world.** The implementation schedule belongs to the one elaborator
declaration selected by ordinary resolution, after its public call plan is
fixed. Marked staging walks only the value operands in that plan. Its one
call-shell exception uses the already-resolved compiler-owned intrinsic kind
and the exact canonical scheme above; an ordinary declaration cannot acquire
that kind by matching a name or type shape. Adding an unrelated declaration to
a module body therefore changes neither the selected schedule, staged
operands, transcript order, nor completed result of an existing call.

**Crossing label and host boundaries is not an elaborator's job.** A source-to-target coercion rearranges structural shapes inside `&` and `|`; it never manufactures or discards a generated label newtype, and never treats a `host type` as structurally equal to its consumer-supplied binding. Move between a payload type and a generated nominal type `F` with `{f = x}` / `F.mk(x)` or `F.get(r)` / `r.?{f}` (see [Labels](#labels)); cross a `host type` boundary with a host-provided conversion.

Unlike typeclasses, which insert dictionary lookups at every polymorphic call site automatically, an elaborator only runs where you import its name and write the bang-call.

### Traits as label-generated types

*This section describes a surface encoding that does not exist in [Kio'](prime.md). Trait-style dispatch in Kio' is explicit dictionary-passing: threading the dictionary value as an ordinary parameter and projecting out its methods by name.*

Kio has no `trait` or `class` construct. What other languages call a "trait" — a bundle of named methods — is directly a [label-generated type](#labels) whose label is the method name, or a product of such label-generated types:

```kio
pub labels { show[A] : A -> String };
pub labels { less_than[A] : (A & A) -> Bool };
pub type Ord[A] = Show(A) & Less_than(A);
```

A value of `Ord(Int)` is a dictionary carrying `show` and `less_than` implementations for `Int`. Callers reach the dictionary's methods by explicit projection — `(d.?{show})(42)` and `(d.?{less_than})(1, 2)` — projecting the matching conjunct from `d` first (with `__fst__`/`__snd__`) if `d`'s type is a compound product of label-generated methods. Because each label inhabits its own generated nominal type, two methods with identical underlying signatures never collide — the label distinguishes them. The named form `labels T = { … }` also introduces a `type T` over the product of its labels; when a trait aggregates labels declared elsewhere (as with `Ord` reusing `show`), write the type alias directly. Users may always bypass the alias and write the label-generated forms directly (`Show(Int)`, `Show(Int) & Less_than(Int)`) wherever a type is expected.

Prose in this document may use "trait" for such a label-generated product and "method name" for a label, but these are just labels — the language has only generated nominal types, products, and functions.

## Module system

A **module** is a namespace and typechecking unit. Modules are addressed by their filesystem path inside the current package, with directory separators written as `/` — so a module path reads like its on-disk location (`utils/string` → `utils/string.kio`).

**Every module file begins with a `module` declaration.** A module file starts with `module <path>;`. The `<path>` is the module's location relative to the package root: any subdirectory segments, ending with the module's filename without `.kio`. The package name is not prepended at the declaration site or in `import` paths.

```kio
// hello/main.kio
module main;

pub fn run() -> . { print("hi") }
```

The package root is the directory containing `<name>.pkg.kio`. The file stem is the package name, and regular module paths are checked relative to that root. The declared segments must equal the file's path relative to the package root (`.kio` stripped); mismatch is a parse error (exit code `11`). A module belongs to exactly one package.

A root `.kio` file may declare `host type` / `host fn` items after its `module` declaration and `import` clauses, like any other module. Host declarations are ordinary module items under ordinary lexical scoping: a host item's signature names types brought in by `import`, and another module reaches a host item through a plain `import`. There is no env block.

A `host type` is either roleless and optionally parameterized by ordinary kind-`*` parameters (`host type Array[A];`), or role-bearing and nullary (`host type Int role(i32);`). Higher-kinded binders are not admitted on a host type: `host type Wrapper[*F];` is a type error. The roleless and role-bearing forms are disjoint, so combining any type parameters with `role(...)` is also a type error. A role identifies the literal or conditional syntax the nullary type receives; it cannot describe a family of such atomic types.

**Subdirectories organize the module path.** A module at `utils/string.kio` declares `module utils/string;` and is reached by consumers as `utils/string`. Each `/`-segment in the declaration is a subdirectory, and the final segment is the module's filename. Nesting is unlimited; intermediate directories are path segments only, holding no declarations of their own.

**Paths are absolute inside the current package.** Every `import` path names a module path in the current package, never a file location or a relative point in the module tree. Naming a sibling module is written `import utils/string as s`. There is no relative-path form and no `crate::`/`super::`/`self::`-style prefix.

**Dependency roots.** A package may declare a cross-package dependency in a `<local>.dep.kio` file at its root (see [`package.md` § Dependency files](package.md#dependency-files)). The dependency's module tree is **re-rooted** under its local name: the dependency's `module app;` becomes reachable as `<local>/app`, and its `pub` items are importable with the ordinary `import` forms (`import <local>/app(greet);`). An `import` path's first segment names a declared dependency's local name or one of the consumer's own modules: a first segment that matches a declared dependency resolves in that dependency's re-rooted tree, otherwise in the consumer's own modules. A dependency's local name may not equal the first path segment of any of the consumer's own modules, which keeps that choice unambiguous (see § Open-world design).

**Visibility.** Every definition is private by default. The **`pub`** contextual keyword on a definition makes it exported. `pub` is only recognized in declaration-leading position (top-level, or at member positions inside a `newtype` or braced `rec` group); elsewhere it is an ordinary identifier.

A `pub` may carry a **scope restriction**, written `pub(<module-path>)`: the definition is exported only within the module subtree rooted at that path — importable from that module and anything beneath it, sealed everywhere else. The path must be a **prefix of the declaring module's own path**: visibility relaxes only up to an ancestor, never sideways to an unrelated module or down to a narrower one, so nothing is ever visible where its own module cannot reach — a non-prefix path is a compile error. The restriction is enforced at name resolution as an import gate **and retained into Kio'**, where it also shapes the package's **host interface**: a scoped definition is not exported to the host, which sits outside every module `path`. It is a core visibility form, not a surface-only one. Because a scoped definition is strictly narrower than `pub`, adding one cannot widen any existing import — open-world compilation is preserved. The scope is admissible wherever `pub` is a genuine visibility choice — including a `newtype`'s `constructor`/`projector` members and `rec`-group members. `host` declarations are the sole exception: always public, hence never scoped.

**Declaration dependencies obey the same visibility floor.** Every named type
reachable through a declaration's signature must be visible everywhere that
declaration is visible. The walk is recursive through type arguments,
functions, sums, products, and transparent aliases, so a public alias cannot
conceal a private nominal type. It applies to ordinary and recursive function
signatures, type aliases, host-function signatures, elaborator call types, and
the declarations produced when `labels` lowers. A newtype's outer name remains
an opaque API boundary: its payload need not be as visible as the outer
newtype. Instead, the payload types must meet each constructor or projector's
**effective visibility**: the intersection of the outer newtype's visibility
and that member's own marker. Thus `pub newtype Token : Hidden` with private
members is valid; a private `Token` remains private even if a member is written
`pub`; and a public `Token` with a public constructor or projector requires
`Hidden` to be public. Scoped combinations take the narrower subtree. Type
parameters and local binders are not declaration dependencies.

**Function purity.** An ordinary module-body `fn` is unrestricted by default. Prefix it with **`pure`** to make a transitive promise about executable references in its body:

```kio
pub pure fn id[A](x: A) -> A { x }
```

`pub` and `pure` may appear in either order in source; `kio fmt` emits `pub pure fn`. There is no `impure` keyword. `pure` is admitted only on ordinary `fn` declarations, in both Kio and Kio'. It is not admitted on `type`, `newtype`, `labels`, `elab`, `op`, `literal`, `equiv`, `host` declarations, or any `rec(loop)` group/member.

A `host fn` is excluded by definition: `pure` promises that a function does not call host functions, so a host function cannot itself carry that promise. Its body is opaque to Kio, leaving no body the checker could validate as an exception. A `rec(loop)` member is excluded because its execution requires the group's declared `loop` function. Both exclusions are categorical rather than inferred from a particular implementation.

The restriction applies only to executable references in the function body. Type annotations inside that body use the ordinary well-formed-type rules and may mention host types, transparent aliases, newtypes, or any other in-scope type. A function signature also has no purity classification, but its named types obey the declaration-visibility floor above. Type declarations carry no purity classification.

Purity is transitive and declaration-based. In a pure body, executable value paths may resolve to local binders, pure intrinsics and newtype members, or ordinary functions explicitly declared `pure`. While checking an elaborator implementation, compiler compile-time primitives are admitted as the pure primitive environment; they are consumed before persistent Kio' and are not runtime values. A host function or an ordinary function without `pure` is rejected even if a particular implementation happens not to perform an effect. An unrestricted function may refer to both pure and unrestricted functions. The marker is retained in Kio' and on the public function contract so a dumped, freshly reparsed, or validated cached Prime artifact can check the same promise without producer provenance. A loadable image retains the marker, but the runtime loader trusts precompiled function bodies and does not perform that typecheck (see [`prime.md`](prime.md)).

**`import` statements.** `import` statements appear only at the top of a module region, and each is terminated by a semicolon. Two forms are available:

- **Selective** — `import path/to/module(Type, name, {label}, op _ + __);`
  brings each listed binding into scope directly. Bare names select ordinary
  values/types, braces select label syntax, and an `op` tag followed by a
  complete grammar selects an operator.
- **Qualified** — `import path/to/module as m;`, accessed as `m.name`. The `as` clause is **required**.

Neither form carries a modifier. The selective list is always parenthesized
and nonempty, including for one item. In canonical formatting, its opener
immediately follows the module path on the introducing line; multiline lists
follow the ordinary leading-comma
layout. `as` is contextual in the qualified form. Within a selection, standalone
`op` and `varop` are ordinary names. An `op` tag followed by a complete fixed
pattern or a `varop` tag followed by its delimiter pair selects operator syntax. Builtin block imports use their
separate fixed forms below.

```kio
import runtime(panic);
import result(Result);
import people(Name, {name});
import logger as lg;
import utils/list_ops(fold);
```

Deliberately absent: wildcard imports, hiding forms, exports, and per-name renaming on selective forms. Each ordinary post-lowering value, type, or module-alias name may be introduced once. Repeating a selective name within one clause or across clauses is a name-resolution error, even when both occurrences select the same declaration. Repeating a qualified alias is likewise an error even when both imports name the same module. An import also conflicts with a local declaration of the same name. This rule applies uniformly to functions, host items, `type`, `newtype`, and the ordinary bindings produced when `labels` and imported operators are lowered. The fixed compiler-block imports `import __intrinsics__;` and `import __comptime__;` are idempotent sets; repeating a block adds no introduction. Surface-only declaration registries such as `literal` and `op` are consumed before this environment exists and retain the resolution rules in their own sections.

An identity-only transparent alias is still a local introduction. Thus `import origin(T); import origin as source; pub type T = source.T;` is an error: the selective import and alias both introduce `T`. Omitting the selective import gives a valid re-export through the qualified provider. Alias chains and positional generic forwarding preserve their existing type identity and constructor/projector rules, but do not merge written introductions. Binding checks consume a validated `import` environment; this prerequisite does not establish diagnostic precedence when independent import and binding errors coexist.

The origin set is determined only by declarations and imports written in the consumer module (including bindings produced directly from that module's `labels` and operator imports). A selective import's identity is the triple `(source module, syntactically selected namespace, spelling)`: `item`, `Item`, and `{item}` never infer or widen one another. The compiler never scans an imported module for additional, unwritten candidates. Adding a declaration to a provider in another namespace therefore cannot introduce a new origin or change the meaning of an existing consumer import; only editing the consumer's own declaration or `import` list can do so.

**Compile-time helper import.** A module may import the compiler-provided reflection/helper surface with a single block import:

```kio
import __comptime__;
```

The block imports the complete compile-time helper surface. There is no qualified form, selective form, alias, wildcard, or per-name import for `__comptime__`. Public helper type and value names use the documented double-underscore spellings such as `__Type__`, `__Checked_term__`, `__type_view__`, `__term_call__`, and `__structural_recur__`. See [`specs/formal/elaboration.md`](formal/elaboration.md) for the compile-time boundary: user elaborators return reflected checked terms through the `__comptime__` ABI, and the substitute pass swaps those terms in at the Lowered → Prime boundary.

**Module cycles.** Value-level cycles between `.kio` module files are disallowed: if module A imports from module B, then B may not transitively import from A. Every **explicit `import`** counts as an edge — there is no type-only exemption: `import m(X);` always contributes the same `→ m` edge, including an `import` that brings a `host type` or `host fn` into scope. The package file's `bridge` block does not participate in this graph because it targets the contract surface rather than module bodies.

A consequence: **mutual cross-module type recursion is not expressible.** Two modules that reference each other's types with no runtime dependency (the `Expr`/`Stmt` split) would each `import` the other, forming a cycle, and are rejected. A type that must be mutually recursive across a module boundary has to be moved into a single module (where intra-module mutual recursion is fine).

Kio compiles in two strata: (1) **module bodies** — every `.kio` module's top-level values, types, and `host` declarations; (2) the **contract surface** — the transitive closure of `host` items, exports, and the types they reach, selected by the package file's `bridge` block. The value-cycle rule applies strictly within stratum 1.

## Open-world design

Kio's compilation model is **open-world**: adding definitions to a **module body** — new types, functions, or intended candidates — must never cause a different module to fail to compile. From a consumer module's perspective, such additions are monotonic: they can only enable things that did not previously typecheck, never break things that did.

Open-world is preserved **structurally**, not by bookkeeping. `into!` and `onto!` operate purely on the known source and target types, with no candidate pool at all.

**Scope.** Open-world is a property of `.kio` module bodies — its formal statement ([`formal/prime.md` § 7](formal/prime.md)) is scoped there. It does **not** cover the **contract surface**: the transitive closure of `host` items, exports, and the types those reach, as selected by the package file's `bridge` block. Changing anything inside that closure — adding or removing a `host` item, changing a type a bridged signature reaches — is a versioning event, not an open-world violation. The carve-out is keyed to the contract-surface closure, not to a syntactic block.

**Role-based singleton resolution is a contract-surface coupling, not an open-world hole.** Tier 3 of [literal resolution](#literals) reads the role-bearing host identities in the use site's unqualified lexical scope. Adding a second role-bearing `host type` whose role admits a literal shape already served by tier 3 turns those unannotated literals ambiguous: a previously-resolving `let n = 42;` becomes a type error. The `__if_then_else__` scheme and the reference `if!` elaborator’s branching glue likewise require that selected `role(bool)` identity set to be a singleton, so adding a second identity makes those uses ambiguous even when the condition already has one of the two types. A `host type` is part of a module's contract surface, exactly the category the **Scope** paragraph carves out, so this coupling is a versioning event, not a module-body open-world hole. It does not reach across package boundaries or across unrelated module trees. A qualified module import does not add all of that module's members to the pool; a provider addition behind an existing `import provider as p;` therefore cannot perturb resolution. A role-inheriting alias (a `type` whose body resolves to a role-bearing host type) is a candidate only as a fallback, when no `host type` carries the role — so where a host type is in scope, no alias enters the pool. The alias itself must be in unqualified lexical scope, but its body may name its terminal host identity through a qualified import. Multiple aliases reaching one terminal identity remain one candidate, so adding another spelling for an already-present identity changes nothing. The fallback couples to the same role-bearing-type-in-scope property the host-type case does: it transparently denotes a host type through an explicitly in-scope alias, so it carries the identical contract-surface coupling rather than a new module-body hole.

**Host items preserve open-world.** A module's `host` items are part of its contract surface — peers of a function signature. Adding or removing a `host` declaration is a versioning event, not an ordinary module-body edit, and the break (a newly-required host capability) lands on the host at its recompile against the regenerated interface, caught by the type system at the boundary. No ordinary `pub fn` / `type` added to any other module body can change which host capabilities an existing bridge requires.

**Purity preserves open-world.** A pure function is checked only against executable references already resolved in its body. The checker consults an ordinary function declaration's written `pure` metadata; type paths are irrelevant, and it never scans a growing candidate pool. A bang call likewise names one explicitly imported elaborator, whose implementation and captures are fixed by that declaration; final Prime validation checks only the term that call produced. Adding an unrelated declaration to any module body therefore cannot make an existing pure function impure or change its elaboration. Changing the `pure` modifier on a referenced public function changes that function's contract, the same category as changing its signature.

**Dependencies preserve open-world.** Declaring a cross-package dependency in a `<local>.dep.kio` file (see [Module system § Dependency roots](#module-system) and [`package.md` § Dependency files](package.md#dependency-files)) only *adds* an importable `<local>/…` module root: the dependency's module tree is re-rooted under the local name, so every dependency lives under its own prefix and adding one cannot change the meaning of an existing module — the consumer's own modules, or another dependency's. The one hazard to monotonicity is a local name shadowing one of the consumer's existing root modules, which would make `import <local>/module as m;` ambiguous; the collision rule (**a dependency's local name must not equal the first path segment of any of the consumer package's own modules**) rules that out, so an `import` path's first segment resolves to exactly one of a declared dependency or a local module. Adding a dependency is therefore monotonic on the consumer's existing code. (A dependency's *contract surface* — the env it requires of the host, folded into the consumer's surface — is a versioning concern, the same carve-out as the **Scope** paragraph above, not a module-body open-world hole.)

## The host boundary

Kio has no built-in notion of a runtime. A package starts with nothing concrete in scope: no numeric types, no booleans, no strings, no I/O, no ambient state, no iteration. Everything concrete a package needs — numbers, a clock, a logger, an iteration primitive, even a string type for its own source literals — is named by the package and supplied from outside at invocation. The concrete thing that invokes the package and supplies those names is the **host**.

### Host declarations and the bridge block

A package's contract with its host is declared in two places: ordinary `host` items inside modules, and a single `bridge { ... }` block in the package file that selects which modules participate.

A module declares what it needs from the host with **`host type`** and **`host fn`** declarations, and what it offers back with ordinary **`pub`** items:

```kio
// app.kio
module app;

host type String role(str);
host fn print(s: String) -> .;
```

```kio
// app/main.kio
module app/main;

import app(String, print);

pub fn main() -> . { print("hello\n"(String)) }
```

`host type` / `host fn` are signature-only — they have no right-hand side; the host fills them in at invocation. They are always public and obey ordinary lexical scoping: another module reaches a `host` item through a plain `import`, exactly like any other `pub` item. There is no private host item; `pub host` / `host pub` are accepted but redundant.

The package file names, with module-path globs, which modules form the host boundary:

```kio
package app;

bridge {
  app;
  app/**;
}
```

The `bridge` block selects **modules**; the host boundary is *derived* from
them. From each matched module, every `pub host` item becomes a host
requirement (the **env** — what the host must supply), every other public
callable becomes a host-invocable entry (an **export** — what the host may
call), and public type declarations contribute the type surface those
callables use. Adding a `host fn` or a `pub fn` to a matched module grows the
corresponding surface automatically; there is no item-by-item ledger. Entries
keep their module namespace, so `a/main.main` and `b/main.main` are distinct
host entries even though both leaf names are `main`; there is no export rename.
See [`package.md`](package.md) for the package-file shape and the
well-formedness rules that keep the exposed contract self-contained.

**Polymorphic host declarations.** A roleless `host type` can carry ordinary kind-`*` parameters, and a `host fn` can carry the same type parameters as an ordinary `fn`:

```kio
host type Map[K][V];
host fn fold[A][B](f: (B & A) -> B, seed: B, xs: Seq(A)) -> B;
```

**Host types are opaque.** A `host type` is treated as an abstract name inside the package's modules: the compiler knows its identity and its ordinary kind-`*` type parameters, but never its structural shape. A host type cannot bind a higher-kinded parameter; abstracting over a type constructor remains a top-level `newtype` or `fn` capability. Structural-typing equality is suspended at the host boundary on purpose, so a package's reasoning about its own types cannot depend on which host supplies them. Neither `into!` nor `onto!` crosses host type boundaries.

**Role-assigned host types.** A `host type` declaration may be annotated with `role(...)` to designate the type's **role** in the language. The currently-recognized roles are the Rust-shaped sized numerics plus `bool` and `str`:

```kio
host type I32    role(i32);
host type U64    role(u64);
host type F64    role(f64);
host type Bool   role(bool);
host type String role(str);
```

The full role list: `i8`, `i16`, `i32`, `i64`, `i128`, `u8`, `u16`, `u32`, `u64`, `u128`, `f32`, `f64`, `bool`, `str`. Each role belongs to one of four **shapes** — integer, float, string, boolean — and the [literal admission relation](#literals) keys off the shape. A role-bearing host type carries exactly one role; multiple host types in the same scope may carry the same role. `role` applies only to `host type` declarations. When neither an explicit annotation nor an expected type determines a literal's type, tier 3 requires exactly one admitted role-bearing identity in the use site's unqualified lexical candidate pool.

A role's primary purpose is **literal reception**: source-level literal forms become values of a role-bearing host type via the three-tier resolution in [Literals](#literals). `role(bool)` also identifies the condition type for `__if_then_else__` and for the reference `if!` elaborator's branching glue. Both require one selected Boolean identity in unqualified lexical scope (see [Conditionals](#conditionals)). Multiple Boolean-role declarations remain valid when no construct needs that singleton scheme.

### Embeddability and portability

Kio packages are **embeddable**: a package is a plug-in, dynamic component, or script that runs inside a host, not a standalone program. The language has no package-level concept of a `main` binary; an exported item named `main` is just a common entry-point name, not a language-level one. The host decides what exports to invoke, what to pass in for host items, and how to run the result.

This "start from nothing" stance is what makes Kio ultra-portable: the language itself has no dependency on any particular backend, so the same source can be transpiled to any backend for which a suitable host is written. The *backend* (which fixes the artifact format the transpiler emits — e.g., C, native, a bytecode, a host language's source) is chosen separately from the host, and the two are not in one-to-one correspondence.

## Lexical structure

Source-level basics: naming, identifiers, comments, keywords, and literals. Syntax for specific language features is documented alongside their semantics in the relevant sections.

### Naming conventions

Kio identifier spellings are enforced by the compiler: a violation is a parse-error category failure (exit code `11`; see [`exit-codes.md`](exit-codes.md)).

Ignoring leading and trailing underscore affixes, each word contains one or more ASCII letters followed by zero or more digits. Exactly one underscore separates adjacent words. Thus `a1_b2`, `foo123_bar4`, `sha256`, and `p0` are valid; `a1b`, `foo_123`, and `foo__bar` are invalid. Any number of trailing underscores is admitted.

- **Type names and type parameters** match `_?[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*`: the first letter is uppercase, and every later letter is lowercase. This applies to aliases, host types, newtypes, named label aliases, and type binders. `I32`, `Foo123_bar4`, `_Logger`, and `_Event_stream` are valid type names.
- **Values** match `_?[a-z]+[0-9]*(?:_[a-z]+[0-9]*)*_*`: every letter is lowercase. Functions, elaborators, value binders, and ordinary imported value names obey this rule.
- **Labels** match `[a-z]+[0-9]*(?:_[a-z]+[0-9]*)*_*`, without a leading underscore. The generated nominal name preserves every word and affix and capitalizes only the first letter: `foo1_bar2` becomes `Foo1_bar2`. Generated nominals occupy the ordinary type namespace and participate in duplicate-name checking. A second explicit declaration of the same label is rejected; the exact `name: _` marker described in [Labels](#labels) refers to the first generated type without declaring another one.
- **Modules, subdirectories, packages, and dependency local names** obey the value rule. Module paths and package, dependency, lock, and signature filename stems retain their exact source identity.
- **Leading underscores** have the same rule for both namespaces. Zero or one leading underscore is user space. A single leading underscore is part of the exact identity and permits an unused binding; `Foo` and `_Foo`, or `foo` and `_foo`, are distinct and remain ordinarily referenceable. It grants no different typing, visibility, or capability.
- **Reserved prefixes** begin with two or more underscores. They use the same word and case grammar, with grammatical patterns `_*[a-z]+[0-9]*(?:_[a-z]+[0-9]*)*_*` and `_*[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*`. User declarations reject this prefix class. The intrinsic and compile-time import targets and their documented declarations use reserved spellings such as `__intrinsics__` and `__Type__`. A reserved spelling supplies hygiene only: a reference still requires an ordinary exact binding and its applicable visibility and phase validation.
- **Every name contains a letter.** Pure underscore runs and underscore/digit-only spellings are not names. Bare `_` is admitted separately as a wildcard or inference placeholder, and `_`, `__`, and `___` have the separately specified operator-slot roles.

The lexer retains one broad `IDENT` token, `[A-Za-z_][A-Za-z0-9_]*`; role validation diagnoses a malformed spelling as one token. Invalid-name diagnostics identify the expected role and violated rule and carry only replacements valid for that role. A repair without a trustworthy resolved identity explicitly edits only that token. An identity-preserving rename requires the ordinary collision, capture, and document-version checks.

### Keywords and reserved names

Every keyword in Kio is **contextual**: each keyword-shaped word is lexed as an ordinary identifier and the parser routes it to its keyword role only at positions that demand it. There is no "always reserved" tier. Boolean literals are spelled `.t` / `.f`, so `true` and `false` are ordinary value identifiers. Placeholder-lambda intros such as `.x.` are punctuation-led forms rather than keyword-shaped words (see [Placeholder lambdas](#placeholder-lambdas)).

What "contextual" buys: adding a new keyword later cannot break existing user code that uses the word as an identifier, and the lexer stays small and uncoupled from grammar evolution. What it costs: at the listed positions below, the named word does **bind** to its keyword role — a user who tries to use one as a value identifier at that position sees a parse error pointing at the position rather than a "this is a reserved word" diagnostic.

Per-keyword semantics are documented at the declaration sites; this is the consolidated reference.

- **Item-leading words** (recognized at module-level item position, after declaration modifiers where admitted): `fn`, `rec`, `type`, `literal`, `newtype`, `labels`, `equiv`, `elab`, `op`, `varop`. (`equiv` is not allowed after `pub`; `rec` is allowed after `pub` in the singleton forms `pub rec newtype`, `pub rec labels`, and `pub rec(loop) fn`; either braced `rec` group takes no leading visibility, so `pub` goes on each member; only an ordinary `fn` is allowed after `pure`.) Outside these positions and the recursive-call position below, ordinary identifiers.
- **Module-header words** (recognized at the start of a `*.kio` module file): `module` for a module declaration.
- **Recursive-call introducer**: `rec` introduces a recursive callee, with optional call annotations before that callee: `rec step(x)` or `rec(cont) step(x)`. Without a following callee, a bare `rec` reference or an ordinary call such as `rec(x)` uses the ordinary identifier. In particular, `fn id(rec: .) -> . { rec }` is an ordinary function.
- **Statement-leading word**: `let` introduces a binding only when the following tokens prove the binding form. Elaborator names such as `if`, `do`, `scope`, and `match` are ordinary identifiers; adjacent `!` introduces a generic elaborator call. Continuation labels such as `else` are ordinary value-shaped names in the block-call grammar. Dot-lambdas use punctuation-led forms (`.(...) { ... }` and `.stem. { ... }`).
- **Inside elaborator declarations**: `impl`, `captures`, and `trailing` introduce entries; `product`, `thunk`, and `sequence` select the descriptor after `trailing`.
- **Import-leading and import-context words**: `import` begins a module's
  leading import clause; `as` introduces a module alias. Inside a selective
  list, `op` tags a fixed operator grammar and `varop` tags a variadic delimiter
  pair. These spellings remain ordinary names elsewhere.
- **Inside operator declarations**: a `varop` body contains exactly one of
  `foldl`, `foldr`, `foldl1`, or `foldr1` and optionally `finalize`, in either
  order; a fixed operator body contains exactly one `impl` entry.
- **At the start of a package file**: `package <name>;` declares the package identity.
- **Inside a package file** (`<name>.pkg.kio`): `bridge` leads the package's module-glob block.
- **Inside a `build { ... }` block** (in `<name>.pkg.kio`): `build` opens the block; `cache`, `docs`, and `target` lead its declarations, with `target` opening a `target <id> { … }` block (bare-identifier id).
- **Inside a `newtype` block**: `constructor`, `projector` at item positions.
- **Inside a host type declaration**: `role` opens the `role(...)` annotation.
- **Declaration modifier position** (top-level declarations and `newtype` block items): `pub`; on an ordinary module-body `fn` only, `pure` as well. `host` is a declaration modifier only when immediately followed by `type` or `fn` (optionally with a redundant `pub`); elsewhere it is an ordinary identifier.

A user-declared identifier may freely have any of these names — `fn id[A](x: A) -> A { x }` declares a value named `id`, and `fn pub() -> .` would declare one named `pub`. The contextual recognition only fires at the positions above; in any other position the same word is an ordinary identifier reference.

- **Reserved intrinsic values** — provided by the compiler, not imported by default; brought in as a block by `import __intrinsics__;`: `__left__`, `__right__`, `__either__`, `__pair__`, `__fst__`, `__snd__`, `__if_then_else__`, `__absurd__`.
- **Reserved import targets** — `__intrinsics__`, recognized only inside `import __intrinsics__;`, and `__comptime__`, recognized only inside `import __comptime__;`. Neither is ever a regular module name.

Two type-level literal tokens — `.` (the unit type) and `!` (the bottom type) — are grammar tokens rather than identifiers, so they aren't user-declared names but are worth listing alongside the reserved set.

This list is exhaustive: every reserved word or name recognized by the compiler appears above. When adding or removing a reserved word, update this section in the same change as the implementation. The user-identifier grammar and the `__name__`-prefix reservation are documented in [Naming conventions](#naming-conventions) above.

### Comma-separated lists and operator chains

Every comma-separated production in Kio's grammar — type-binder groups, value-parameter groups, `labels` arm lists, tuple literals, call argument lists, type-application argument lists, label-value label lists, and selective `import` name lists — accepts **any number of commas in any position**: leading runs (before the first item), interior runs (between items, beyond the single separator), and trailing runs (after the last item). Equivalent framing: items are separated by *one or more* commas, with optional comma runs at either edge. All shapes collapse to the same AST as the canonical one-comma-per-separator layout, except type-binder groups, whose canonical form is adjacent singleton binders: `[A, B]` parses like `[A][B]` and formats as `[A][B]`. `(,,, a ,,, b ,,,)` and `(a, b)` parse to the same tuple literal; `(, a ,)` parses as grouping around `a`, not as a unary tuple; `(,,,)` parses as `()`. `fn three( , x: A , y: B , z: C , )` and `fn three(x: A, y: B, z: C)` parse to the same value group. Function-type value groups are not comma-separated: write `(A & B) -> C`, not `(A, B) -> C`. The formatter relies on the comma rule for the leading-comma multi-line layout where commas are part of the canonical production.

Block-like clause bodies use the same semicolon separator rule: repeated semicolons do not create empty clauses, and a final semicolon after the final expression is ignored. This applies to expression blocks, neutral elaborator blocks, and keyed/member blocks such as `newtype`, `build`, `target`, and `docs`. A resolved descriptor determines a neutral block's meaning: empty product and thunk blocks produce unit and a unit-returning thunk respectively; a sequence requires an explicit final expression.

The type chains `&` (product) and `|` (sum) follow the symmetric rule: a chain accepts **any number of operators in any position** — leading, interior, trailing — all collapse to the same right-associated chain. `&&& A &&& B &&&` (when terminated by a non-Type token like `,`, `)`, `;`, `=`, `{`, `}`) and `A & B` parse identically; same for `|`. The rule applies in both unparenthesized and parenthesized form: `( &&& A &&& B &&& )` and `(A & B)` parse identically. Empty and unary chains use the algebraic identities: product `&` with no items (`&` or `(&)`) is `.`, sum `|` with no items (`|` or `(|)`) is `!`, and a one-item chain (`& A`, `(A &)`, `| A`, `(A |)`) is just `A`. Mixing `&` and `|` in one chain still requires explicit parentheses (per [Anonymous sum and product types](#anonymous-sum-and-product-types)) — `A & B | C` is a parse error, and the user writes `(A & B) | C` or `A & (B | C)` to disambiguate. The formatter's multi-line emit for a chain mirrors the leading-comma layout for lists: each chain item lives on its own line at +2 indent, prefixed with `&` (or `|`) followed by a space — the leading-operator A1 analogue. Arrow `->` is not a variadic delimiter: bare unary `A -> B & C` parses as `A -> (B & C)`, but a product/sum chain on the left of an arrow must be parenthesized (`(A & B) -> C`, not `A & B -> C`).

### Comments

Line comments begin with `//` and run to the end of the line:

```kio
// This is a comment.
fn foo() -> String { "hi" }   // trailing comment
```

Block comments are not part of the syntax.

### Doc comments

A doc-comment is a contiguous run of `///`-prefixed lines immediately
preceding a top-level definition.

```kio
/// Returns the identity of its argument.
pub fn id[A](x: A) -> A { x }
```

**Lexer rule.** Three slashes followed by whitespace, end-of-line, or
end-of-file open a doc-comment line. The line's payload is everything
after `///` up to end-of-line (excluding the terminator). A comment
marker — `//` or `///` — must be followed immediately by whitespace,
end-of-line, or end-of-file; a non-whitespace character flush against
the marker is a lex error (`//foo`, `///bar`, `//=` all reject). In
particular a fourth slash (`////`, `////////`, …) is rejected — four or
more slashes are **not** a section ruler. See
[`grammar.md`](grammar.md#lexical-structure-kio) for the lexical rule
and the reserved `//…` operator family it backs.

**Attachment points.** A doc-comment attaches to the immediately
following top-level declaration in a module body (`fn`, `type`,
`literal`, `newtype`, `labels`, `elab`, `op`, `host type`, `host fn`) —
every named, documentable declaration. A
doc-comment placed at the very beginning of a module file — before the
`module` line, with no declarations in between — attaches to the module
itself. `equiv` (a test-claim with no public-API surface) and a package
file's `bridge` globs are **not** declaration attachment points; a
`///` immediately preceding one carries no documentation (the comment is
dropped, not attached).

**Parse errors.** A `///` block before an `import` clause is a parse error —
`import` clauses are not documented. (`//!` inner-form doc-comments are a
permanent non-goal; there is no inner form.)

**Block boundary.** A doc-comment block is a single contiguous run of
`///` lines. An intervening blank line, a regular `//` comment, or any
non-comment content ends the block; only the immediately preceding run
attaches.

## Bindings and expressions

### Top-level bindings

The top level of a module supports these declaration forms:

- **`fn`** — function bindings.
- **`rec(loop)`** — surface-only recursive function groups that lower to an ordinary loop function (see [Recursive functions](#recursive-functions)).
- **`type`** — transparent type aliases (see [Type and literal aliases](#type-and-literal-aliases)).
- **`literal`** — literal aliases, a surface-only way to name bare literal tokens (see [Type and literal aliases](#type-and-literal-aliases)).
- **`newtype`** — nominal type declarations with explicit constructor and projector (see [Type declarations](#type-declarations)).
- **`labels`** — label declarations; surface sugar over `newtype` (see [Labels](#labels)).
- **`elab`** — user-defined elaborators called with bang syntax (see [User-defined elaborators](#user-defined-elaborators)).
- **`op` and `varop`** — user-defined fixed and variadic operator bindings (see [Operators](#operators)).
- **`equiv`** — compile-time equivalence checks consumed by `kio test` (see [Equivalence declarations](#equivalence-declarations)).

Top-level value bindings use the declaration forms above; arbitrary top-level value expressions are not Kio items.

Top-level bindings are private by default; prefix with **`pub`** to export. An ordinary `fn` may also carry **`pure`** as described in [Module system](#module-system).

```kio
pub fn identity[T](x: T) -> T { x }
pub fn greeting() -> String { "hello" }
```

A `fn` always has a parameter list (parens required, even when empty). `let` is never used at the top level — it is reserved for local naming inside blocks.

**Order-sensitive visibility.** Declarations, including `host type` and `host fn`, are processed top-to-bottom. A declaration sees only names introduced *above* it in the same file, plus names brought in by `import`. A host declaration introduces its name after its own signature is checked; host types in that signature must already be in scope. There is no implicit forward reference within a file. The explicit exceptions are the capability-free recursive-data scopes (`rec newtype`, `rec labels`, and bare `rec { ... }` type groups; see [Type declarations](#type-declarations)) and `rec(loop)` function groups. A singleton data declaration sees only the heads its own declaration contributes. A bare type group sees exactly its written member heads. A `rec(loop)` group gives its member bodies access to the group's member names only through explicit `rec name(...)` calls.

A direct consequence: an ordinary **`fn` cannot refer to itself**, because its own name is not yet in scope when its body is checked. Mutual recursion between ordinary `fn`s is equally impossible. Use an explicit `rec(loop)` group when term recursion is intended. The same order-sensitivity rules out self-referential type aliases.

### Type and literal aliases

Kio has two transparent alias forms:

- **`type`** names a structural type expression.
- **`literal`** names one bare literal token for reuse in expression position.

Both forms end with a semicolon and are private by default; prefix with **`pub`** to export.

#### Type aliases

A **type alias** gives a structural type expression a name. Two forms:

- **Nullary** — `type Name = body;` names the `Type` `body` as `Name`.
- **Parametric** — `type Name[A][B](…) = body;` names a family of types parameterized by one or more type parameters. Each reference supplies exactly one argument per declared parameter: `Name(X, Y)` is a type for `type Name[A][B] = …`, while bare `Name` and undersaturated `Name(X)` are type errors. Underapplication cannot retain the alias declaration's own binders as residual constructor slots. This restriction is what rules out anonymous type-level abstraction — the piece separating Kio's Church-style fragment of F-ω from full F-ω (see [Type parameters](#type-parameters)).

```kio
type Logger = String -> String;
pub type Show[A] = show(A);
```

The typer unfolds a type alias structurally wherever it appears; type aliases survive every compilation phase. **An ordinary `type` declaration is non-recursive.** There is no singleton `rec type` form. A transparent alias may be a member of a genuinely mutual `rec { ... }` type group only when every cycle in the component crosses a sibling `newtype` boundary and the alias-only dependency subgraph is acyclic. Kio has no anonymous recursive type expressions or alias-only fixed points: every recursive type component is grounded at an explicit nominal declaration. Parametric declarations behave as **surface sugar** at the parameter level: `Name(X, …)` substitutes the arguments into `body`.

A transparent alias retains a terminal newtype's member namespace only when
every alias edge is a fully saturated positional identity application with the
same effective binder kinds. For example, after
`type Wrapped[A] = Box(A);`, `Wrapped.mk_box` denotes the exact constructor
declared by `Box`; the result remains the `Box(A)` nominal. Partial,
reordered, structural, cyclic, and missing-target aliases do not expose
constructor or projector members. The alias head must already be in lexical
scope, and visibility of the written alias and terminal member is checked
separately. Definition, references, and rename therefore associate the written
head with the alias declaration and the leaf with the terminal member. The
alias does not mint a new member or change reduction.

#### Literal aliases

*Surface only; desugars to Kio'.*

A **literal alias** binds a lowercase name to one bare literal token. A bare reference expands to the stored literal; `name(Type)` expands to the stored literal carrying the written literal-call annotation. Expansion happens at the Surface → Desugared boundary, before Kio' reaches the resolver or typer.

```kio
literal max_size = 100;
literal greeting = "hello";

fn cap(n: I32) -> I32 { min_i32(n, max_size(I32)) }
fn hello() -> String { greeting(String) }
```

After expansion, `hello` is identical to one where `"hello"(String)` is written directly. A literal alias declaration itself carries no type; the literal is checked only after expansion in the use-site expression context.

**Restrictions** (parser-enforced):

- The declaration name follows the value-name rule.
- The right-hand side is exactly one literal token: string, integer, float, or bool.
- Adjacent string-literal folding does not apply in a `literal` declaration body; write the complete string in one token.
- A literal alias accepts either no call arguments or exactly one type annotation argument. `x()`, `x(y)`, and `x(A, B)` are errors.
- Item-leading position only. There is no block-local `literal`; `let x = expr;` covers block-local naming.

**Visibility.** A literal alias is private by default; `pub literal name = …;` exports it for cross-module use via the ordinary `import package/module(name);` selective form. Two literal aliases in the same module can't share a name (same duplicate-name rule as any other top-level declaration).

**Binding origin.** A consumer-visible literal-alias name denotes exactly one
declaration identity, and each explicit literal-alias import may be written
once. Repeating `import source(name);` is a name-resolution error even when
both imports name the same exported literal. Importing that spelling from
distinct modules, or combining an import with a same-named local declaration,
is likewise a name-resolution error; declaration and import order never select
a winner. Every written import and the value-import cycle graph are validated before
this origin check, so a missing module, missing export, visibility failure, or
cycle remains an Import error rather than being masked by the later Name error. The
origin set contains only declarations and imports written in the consumer;
adding any differently named provider export cannot add a candidate or perturb
an existing expansion.

**Shadowing.** A local binder (`let`, `fn` value-parameter, `match!` clause parameter) with the same name as a literal alias shadows the alias within the binder's scope; references to that name inside the scope are the local, not the alias.

**Hygiene.** A literal alias body has no free names. Imported literal aliases expand from the exported declaration's stored literal token, so adding declarations to a module body cannot change the meaning of an existing consumer's literal-alias expansion.

**Why not a general value-binding form?** A literal alias is deliberately narrower than a transparent source rewrite for arbitrary expressions. Users wanting an evaluation-bearing "constant" or shared computation reach for a `fn` — already exists, already pins evaluation semantics, already runs at well-defined sites (every call is explicit).

### Type declarations

The **`newtype`** keyword declares a **nominally-distinct type** with a single payload, plus an explicit user-named constructor and projector. `newtype` is the Kio' primitive that `labels` (below) is sugar over. Two forms:

```kio
newtype Name : Payload {                              // nullary
  pub constructor name;
  pub projector name;
};

newtype Name[A][B] : Payload(A, B) {              // parametric
  pub constructor name;
  pub projector name;
};
```

A top-level `newtype` declaration ends at `}` and accepts one redundant suffix
semicolon. Its `constructor` and `projector` items are both required and form
an **unordered set** — either may be written first. Semicolons separate the
members; leading and trailing runs are accepted. `kio fmt` emits
constructor-then-projector with one separator between them and no outer suffix
(see [`style.md`](style.md#semicolon-clause-blocks)). In a recursive group,
the group's separator follows a non-final declaration instead. The outer
declaration and each inner item carry independent visibility: omitted is
private, leading `pub` is exported. The name follows the type-name rule (an
optional single underscore before the uppercase initial; see
[Naming conventions](#naming-conventions)); constructor and projector names
follow ordinary value-name rules.

Inside the block, `constructor` and `projector` are **contextual keywords** recognized only at the block's item positions; outside a `newtype` block they are ordinary identifiers. Each entry names a function added as a **member of the declared type**, not as a free top-level value in the module. Members are reached by a dotted path on the type:

- The constructor, reached as `Name.c(..., payload) -> Name(...)`, wraps a payload.
- The projector, reached as `Name.p(..., value) -> Payload`, unwraps it.

Example:

```kio
pub newtype Celsius : F64 {
  pub constructor mk_celsius;
  pub projector to_f64;
};
```

This introduces an exported type `Celsius` with two exported members: a
constructor `Celsius.mk_celsius : F64 -> Celsius`, and a projector
`Celsius.to_f64 : Celsius -> F64`. The member names are not added as free
top-level values — callers write `Celsius.mk_celsius(42)`, not an unqualified
`mk_celsius(42)`. `import` brings members into scope together with their type:
`import temps(Celsius);` brings `Celsius` as a type and keeps
`Celsius.mk_celsius` reachable through that type name.

**`newtype` is a nominal boundary.** Each `newtype` declaration mints a type distinct from its payload and from every other `newtype`. Crossing the boundary always goes through the declared constructor and projector — there is no implicit unfolding, no structural coercion with the payload, and no `into!` or `onto!` path into or out of a `newtype`.

**Recursive types.** Recursive data scope is always explicit. A self-recursive nominal uses `rec newtype`; the marker puts exactly that declaration's head in scope in its payload:

```kio
rec newtype List[A] : . | (A & List(A)) {
  pub constructor cons;
  pub projector un_list;
};
```

The newtype declaration is an iso-recursive boundary by construction. At the value level, crossing it always happens through the declared constructor and projector — there is no implicit unfolding, and a type and its one-step unfolding are **not** the same type until you cross the boundary explicitly.

Mutual recursion uses one capability-free bare group. Every member head is in scope throughout the braces; each member carries its own visibility and documentation:

```kio
rec {
  pub type Tree[A] = Branch(A);

  pub newtype Branch[A] : . | (A & Tree(A) & Tree(A)) {
    pub constructor branch;
    pub projector un_branch;
  };
}
```

The group must contain at least two unmarked `type`, `newtype`, or `labels` members and denote exactly one genuinely mutual cyclic strongly connected component after label elaboration. Every written member must contribute to that component. An acyclic group, a one-way helper, multiple independent components, a one-member brace group, or an alias-only cycle is rejected; move an acyclic dependency before its consumer instead. Singleton `rec newtype` and `rec labels` declarations are top-level forms, not nested group members.

Strict positivity is a property of the explicit recursive component. A recursive use site is every occurrence in a newtype payload that reaches a nominal member of the component, directly or through its transparent aliases. Every such use site must sit in a **strictly-positive position**: the **composed variance** from the payload root down to the occurrence must be `+` (covariant) or `0` (unused at that position). Variance composes through every type connective — arrow LHS flips, arrow RHS preserves, `&` / `|` preserve, and entering a parametric `newtype` slot composes with the slot's variance, computed by mutual fixpoint over the component so transitive contravariance through chains of newtypes is caught. Host-type parameter slots are conservatively treated as **invariant**, so any recursive use site under a `host type` type argument fails. A negative occurrence of a nominal outside the written component stays admissible: it names a fixed inductive type rather than part of this recursion. This is what keeps Kio's recursion well-founded; see [`formal/prime.md`](formal/prime.md) § 2.5.

The marker or group is mandatory exactly when it supplies recursive scope. An omitted singleton marker or omitted mutual group receives a precise diagnostic naming the participating declarations; a redundant marker or non-minimal group is also rejected. The editor offers a safe marker insertion or group wrap only when one unambiguous edit preserves declaration order.

These scopes are closed by the written declaration boundary. Imports and earlier declarations remain visible, but neither singleton nor group sees an unrelated later declaration. Adding an unrelated module declaration therefore cannot retarget a reference or change an already-valid program; the candidates inside the scope are exactly its written heads. After the closing declaration, all contributed names enter ordinary source-ordered module scope.

Because the iso-recursive boundary is purely a typing property of the `newtype` declaration, a transpiler is free to realize it however the backend dictates — e.g., boxing the payload on size-sensitive backends, or leaving it inline where the backend's layout permits.

**Folding over a recursive type.** Kio provides no auto-generated fold for recursive `newtype`s. Ordinary `fn`s cannot be self-recursive; user code that walks a recursive payload either uses an explicit [`rec(loop)` group](#recursive-functions) backed by a host function or calls that function directly.

### Existential type binders

An **existential type binder** is written as `<name>` and appears only as a trailing binder run on a `newtype` declaration's header (or a `labels` entry's header) — there is no standalone existential type expression. The binder run sits as whitespace-separated `<X>` atoms between the universal-parameter list and the `:` introducing the payload. Each binder's scope is the payload type that follows the `:`. **Each binder must occur at least once in the payload** — a binder that never appears in the payload is a phantom existential, information-free and rejected at declaration; the typer reports it in the type-error category.

```kio
newtype Pack[A] <U> : A & U {                       // single existential
  pub constructor mk_pack;
  pub projector un_pack;
};

newtype Box[A] <L> <R> : L & R & A { … };           // multiple existentials

newtype Pack_unit <U> : (U -> .) & U { … };        // existentials only, no universals

labels {
  dproduct[A] <L> <R> : L & R & A,                    // existentials on A labels entry
  dnewtype[A] <U> : A & U,
};
```

The newtype's nominal arity stays at the universals' length: `Pack[A] <U>` is `Pack(A)` from any reference site, regardless of whether the header declares existential binders.

**Construction inference.** At a constructor call, including label construction (`{foo = x}` / `Foo.mk(x)`), the elaborator picks the existentials from the concrete argument types — they behave like additional universal binders that the typer infers from the payload's structure, the same way Kio infers any reachable type-parameter from value-arg types. The result type carries only the universals: `Pack[A] <U> : A & U` constructed at `(I32 & String)` produces `Pack(I32)`. Two argument positions that reference the same existential must agree on its concrete type.

**Existential-opening `let` sugar.** The block-statement form

```kio
let .(<U_1> … <U_n> x) = e;
rest
```

is parser-level surface sugar for

```kio
e(_, .[U_1](…)[U_n](x) { rest })
```

— the RHS `e` is applied to a continuation that opens the existential. `e` must evaluate to the curried CPS function returned by an existential-bearing newtype's projector (see the **Projection through a newtype** paragraph below). The leading `_` lets the typer infer the result type `R` from the continuation's body. The witnesses `<U_1>, …, <U_n>` are opaque inside the rest of the block (the System F binder discipline enforces the escape check); the value binder `x` carries the projector's payload type with each `<U_i>` substituted in. The sugar is surface-only — Kio' programs spell out the CPS call directly.

The payload can instead be destructured by a typed parameter pattern, for example `let .(<U> (left: U, right: U)) = e;`. The witnesses scope over the pattern's annotations and the remaining block; projecting the components does not weaken their escape check. Pattern recognition uses the existing parameter as-pattern disambiguation: `(left: _, right: _)` is a pattern, while the untyped opening `(left, right)` is not admitted. Nested patterns and wildcard components follow the ordinary parameter-pattern rules. This is equivalent to opening into a named payload and then destructuring that payload with an ordinary rich-pattern let.

**Projection through a newtype.** Existential-bearing newtypes carry a **CPS projector**: the projector scheme is

```kio
N.<projector> : [Universals] N(Universals) -> [R] ([Existentials] payload -> R) -> R
```

— a curried form whose outer call takes only universals and the newtype value, returning the inner polymorphic CPS function. Applying the inner function to `(result_type, continuation)` opens the existential: the continuation receives the payload under a System F universal binder that scopes the existential witnesses, so they cannot leak into the result `R`. Most consumers use the [`let .(<U> x) = e;` sugar](#existential-type-binders) at the consumption site; the explicit two-step call is always available for code that needs it. Label-generated newtypes use the member name `get`, so an existential label `foo` is opened through `Foo.get`. Non-existential newtypes retain the direct-return projector `N(universals) -> payload`.

### Structural typing

Kio's type system is **structural** throughout, with two deliberate exceptions — at `newtype` boundaries (including `labels`, which is sugar over `newtype`) and at the host boundary. Three consequences follow:

- **`type` declarations are aliases.** `type A = X;` and `type B = X;` produce the same type; a value of `A` is interchangeable with a value of `B`. A type alias never mints a fresh nominal point distinct from its right-hand side.
- **`newtype` mints nominal identity.** Each `newtype` introduces a type that is **not** structurally equal to its payload, and distinct from every other `newtype`. `newtype`s are identified by their declaration site, so two modules independently declaring the same name introduce distinct types. `labels` inherits this property through desugaring: generated type `Foo` is distinct from the label's payload type, and distinct from `Bar` for any other label `bar`.
- **`host type` declarations are opaque.** See the host boundary note below.

An alias body is interpreted in the module that declares it. Once a type reference is qualified to a module identity, type comparison unfolds it only through that module: a caller-local alias with the same leaf cannot reinterpret a callee signature. Thus if module `dep` declares `type Flag = types.Actual`, a caller's unrelated `type Actual = types.Other` has no effect on `dep.Flag`.

The practical consequence: Kio's one-level newtype is either a plain `newtype` (when you want to pick the constructor and projector names explicitly) or a `labels` declaration (the conventional sugar, where the label supplies construction, access, and update syntax while the generated type supplies the type spelling). Writing

```kio
type Celsius = Float;
```

(where `Float` is a `host type`) makes `Celsius` and `Float` the same type inside the package — callers expecting a `Float` will happily accept a `Celsius`. If you want a distinct type that cannot be confused with `Float`, reach for a label:

```kio
labels { celsius : Float };
// `Celsius` is now a distinct type.
```

`Celsius` is now nominally distinct from `Float`; crossing the boundary requires the construction sugar `{celsius = x}` / `Celsius.mk(x)` or the projector application `Celsius.get(r)` / `r.?{celsius}`.

**The host boundary exception.** `host type` declarations are opaque: the package knows a host type's identity and, for a roleless declaration, its type parameters, but never its structural shape, so a host type is never structurally interchangeable with any package type. A role-bearing host type is always nullary. Opaque here means "not structurally interchangeable," not "nominal at the language level"; inside a single package, every non-host type is either pure-structural (products, sums, type aliases) or nominal via `newtype` (including the `labels` sugar).

### Blocks and local bindings

An **expression block**, such as a function body, contains semicolon-separated
clauses ending in a final expression.
Repeated semicolons between clauses are accepted and ignored, and
a final semicolon after the final expression has no meaning. A semicolon
separates clauses; it does not classify an expression as a statement or discard
its value. The block's value
is the final expression; the block's type is that expression's type. When the
block has an expected type, only its final expression receives it. Every
preceding `let` or expression statement is typed first, and information from
the tail never flows backwards into those earlier clauses.
This is a clause-order boundary, not a rule specific to `let`: an earlier
expression clause likewise finishes under its required unit type before the
tail is checked. Its own annotation or required type may guide a source; a
later use may not. Sequence blocks preserve the source-first rule described
under [Monadic do blocks](#monadic-do-blocks), with shared bind-call carrier
and result constraints. Product blocks instead supply separate operands to
their elaborator and may relate them through its public contract.

```kio
{
  log("starting");
  let x = int("42");
  x
}
```

Statements are either:

- A **simple `let` binding** — `let name = expr;` or `let _ = expr;`. The RHS synthesizes its type; a named local retains that type, including a synthesized polytype, and `_` deliberately discards the result.
- A **rich-pattern `let` binding** — `let .(PATTERN) = expr;`. Typed unary `let .(name: Type) = expr;`, tuple `let .(x, y) = expr;`, and as-pattern `let .(whole: (x: A, y: B)) = expr;` forms use the parameter-pattern grammar (see [Parameter patterns](#parameter-patterns)). A concrete pattern supplies the RHS's expected type. An untyped or partially annotated pattern leaves the RHS in synthesis mode; concrete leaves are checked afterward, without partial product unification or let-generalization. The as-pattern binds the whole value alongside its components. A unary name-only pattern formats as an ordinary simple let.
- A **row-let binding** — `let .({foo, bar}) = expr;` or `let .({foo as local}) = expr;`. It evaluates the RHS once and names the payloads of visible label slots for the continuation. Shorthand binds each label's last path segment; `as` changes the local name while preserving the selected label.
- An **existential-opening binding** — `let .(<U> x) = expr;`, as specified in [Existential type binders](#existential-type-binders).
- An **expression statement** — a non-final expression clause. It must have type `.`; deliberately discarding any other type requires `let _ = expr;`. A final expression remains the block's result even when followed by `;`.

`let` is contextual at statement start: a following name or wildcard starts a simple binding, and `.` followed by `(` starts a rich binding. Rich patterns are always parenthesized, including rows and existential openings. There is no keyword-free binding form. A following `(` without the dot instead leaves `let` as an ordinary value name, so `let(f)` is a call; `let(f) <- x` is an ordinary expression when `<-` is declared as an operator.

A block that ends in a statement (no trailing expression) is a compile-time error. A trailing semicolon is accepted only after a real final expression.

Destructuring `let` is a surface-only form. The Surface → Desugared boundary eliminates it by the same pattern-elimination machinery the parameter-pattern rewrite uses (see [Parameter patterns](#parameter-patterns) § Desugaring). Both forms reduce to a `let <outer> = <expr>;` followed by generated per-slot `let` bindings that project with `__fst__` / `__snd__`, where `<outer>` is the user-given `<name>` for the as-pattern form and a synthetic fresh name for the bare form. Wildcards emit no local binding. From the post-desugar AST onward, the destructuring-let leaves no trace beyond ordinary lets and projection calls.

### Function definitions

```kio
pub fn compose[A][B][C](f: A -> B, g: B -> C) -> A -> C {
  .(x) { g(f(x)) }
}
```

- Parameters are comma-separated inside `( )`. Parens are **always required**, even for zero-parameter functions: `fn constant() -> String { "hi" }`.
- Each parameter is `name: T` for some type `T`; annotations are mandatory.
- Return type follows `->`. The `-> Type` annotation may be **elided**, in which case the return type defaults to `.` — `fn say_hi() { print("hi") }` is equivalent to `fn say_hi() -> . { print("hi") }`. The body is then type-checked against the defaulted return type; a non-unit body is a type error. This is the **only** optional type position in the language; every other type position is either grammatically mandatory or grammatically disallowed (see [Type system](#type-system)).
- Body is a block.
- There is **no automatic currying**. A function of three parameters is not a function returning a function returning a function.

### Recursive functions

Surface Kio admits explicit recursion through `rec(loop)`. The name inside the parentheses is an ordinary value path that must resolve to a function with the loop shape:

```kio
loop[S][R](step: S -> S | R, state: S) -> R
```

There is no privileged built-in named `loop`: hosts, package files, root modules, or earlier module declarations provide a normal function, and `rec(loop)` resolves that path by the usual rules.

Single-function recursion uses the shorthand:

```kio
pub rec(loop) fn countdown(n: I32) -> I32 {
  if! leq_i32(n, 0) {
    n
  } else {
    rec countdown(sub_i32(n, 1))
  }
}
```

The shorthand is an exact expansion of a one-member braced group. Visibility
moves with the member:

```text
rec(loop) fn f(...) { ... }            == rec(loop) { fn f(...) { ... } }
pub(path) rec(loop) fn f(...) { ... }  == rec(loop) { pub(path) fn f(...) { ... } }
pub rec(loop) fn f(...) { ... }        == rec(loop) { pub fn f(...) { ... } }
```

Source may also place the visibility after `rec(loop)` in the shorthand;
`kio fmt` emits visibility before `rec` and may collapse any one-member braced
group to that canonical shorthand.

Mutual recursion uses a braced group. Visibility belongs to each member:

```kio
rec(loop) {
  pub fn even(n: I32) -> Bool { if! eq_i32(n, 0) { true } else { rec odd(sub_i32(n, 1)) } }
  fn odd(n: I32) -> Bool { if! eq_i32(n, 0) { false } else { rec even(sub_i32(n, 1)) } }
}
```

The braces group mutually recursive bodies; they do not introduce a visibility
scope. Every member produces an ordinary module function with the member's
visibility, exactly like the independently visible constructor and projector
members of a `newtype`: no modifier is module-private, `pub(path)` is scoped to
that module subtree, and bare `pub` is public. A later declaration in the same
module can therefore call any member as an ordinary function. The member names
also occupy the ordinary module namespace, so conflicts with another top-level
declaration receive the ordinary duplicate-name diagnostic.

Inside a member body, calls to members of that group must be written with the
`rec` marker. A call to a member name without `rec` is rejected, even for
direct self-recursion. Outside the member bodies the lowered wrappers are called
normally. The call marker carries the optimization/shape facts the compiler
relies on:

- `rec name(...)` — monomorphic, tail-position recursive call.
- `rec(poly) name(T, ...)` — tail-position recursive call whose type arguments differ from the current member instantiation.
- `rec(cont) name(...)` — monomorphic recursive call in a non-tail position.
- `rec(poly, cont) name(T, ...)` — type-changing recursive call in a non-tail position.

This is a source-visible lowering contract: the author states the shape being requested, and the compiler checks that the annotation is both necessary and sufficient. `kio fmt` prints combined annotations in the canonical order `poly`, `cont`, `escape`.

Tail positions include the function body's final expression. The reference
`if!` branches, `scope!` body, and selected `match!` clause bodies retain tail
behavior through their ordinary expanded code when the enclosing call is in
tail position. This follows from structural use of those bodies, never a
library name or descriptor: duplicating, discarding, delaying, or
postprocessing a block does not grant tail permission. The condition of
`if!`, the scrutinee of `match!`, ordinary function arguments, tuple/label
fields, and other subexpressions are non-tail positions and require
`rec(cont)`. The `cont` annotation is rejected in tail position so the source
states the intended lowering precisely.

`rec(escape)` is reserved for recursive calls captured by closures that can re-enter the same recursive group after the current loop step has returned. It is not part of the language; such calls are rejected — a recursive call captured inside a nested function or sequence continuation is an error.

The continuation boundary is introduced by a bind or sequenced expression.
In `do! chain { let x <- rec(cont) step(v); x }`, the recursive call supplies
the first ordinary value argument of the expanded
`chain(rec(cont) step(v), .(x) { x })`, outside the bind continuation.
It therefore requires `cont` and is not an escaping call. Recursive calls in
the rest of the block after a bind or sequence are inside its ordinary
generated continuation and remain rejected.

`rec(loop)` supports polymorphic recursion. A recursive member's type parameters may be supplied explicitly at a recursive call site:

```kio
pub rec(loop) fn walk[A](n: I32, marker: A) -> I32 {
  if! leq_i32(n, 0) {
    n
  } else {
    rec(poly) walk(String, sub_i32(n, 1), "next")
  }
}
```

If the leading type arguments are omitted, the call uses the current member instantiation and `rec(poly)` is rejected as redundant. If the type arguments differ from the current member instantiation, `rec(poly)` is required. The number and order of written type arguments must match the callee member's type-parameter list. Mutual-recursion members share the same type-parameter list and return type shape, so every member can return through the one loop result type; individual `rec(poly)` calls can still choose a different instantiation for the next state. The lowering packages each member's value parameters in an existential state packet, so `rec(loop)` type parameters must be kind `*`.

The form is surface-only. The desugar pass replaces a group with private state-packet `newtype`s, a loop-step lambda, and one ordinary wrapper function per member, preserving each member's visibility. Tail recursive calls construct the next state packet and return the left arm of `S | R`; each non-recursive tail result returns the right arm. A group that contains `rec(cont)` uses continuation-carrying state instead: each state packet also carries the rest of the computation as a function from the callee's result to the loop step result, so a non-tail recursive call becomes "enqueue the callee state with an updated continuation" rather than a Kio' fixpoint. A continuation-bearing packet that refers back to itself is emitted as `rec newtype`; packets that refer to one another are emitted as one minimal Kio' `rec { ... }` type group. No recursive-function group or recursive-call marker reaches Kio'.

The loop boundary is semantic, not just an implementation shortcut:

- **Kio' core stays total.** Recursive control flow is an explicit capability supplied to the surface program, not a term-level fixed point in the Kio' core.
- **Recursive depth is backend-independent.** The depth a recursive run can sustain is a property of the supplied `loop` function, not of the host language's native call-stack limit or per-call frame size. The same Kio package must not overflow on one backend or platform only because its recursive calls consumed a different native stack.
- **Polymorphic recursion is Kio-defined.** `rec(poly)` is accepted or rejected by Kio's own recursive-call rules, not by whether the host language's native recursive functions support the same generic-instantiation pattern.

Backends therefore must not lower `rec(loop)` groups to host-native recursive functions. The open-world argument is the same as for ordinary lexical resolution: the `loop` path is resolved as a normal value path, `rec name(...)` only searches the closed member set written in the group, and each wrapper has the fixed identity `(declaring module, member name)`. None of these rules searches an ambient declaration pool by signature or role, so adding an unrelated declaration cannot retarget an existing recursive call or wrapper call.

### Anonymous functions

The dot-led expression form **`.(...)`** produces an anonymous function:

```kio
.(x, y) { add(x, y) }
.[A](x, y) { pair(x, y) }                 // explicit type parameter
.(x, y) -> I32 { add(x, y) }                 // explicit return type
```

Parens are always required, and there is no automatic currying — same as top-level `fn` definitions.

**Value-parameter annotations are optional.** A lambda's parameter types may be
written explicitly (`.(x: T) { … }`), spelled with the `_` placeholder
(`.(x: _) { … }`), or dropped entirely (`.(x) { … }`). There are two local
ways to complete a missing slot:

- When the lambda's position has an expected function type, that type supplies
  the parameter slots. This checking route matches annotations structurally,
  so it can also fill a `_` nested inside a partial annotation.
- When the lambda literal is the direct callee of an application, the value
  arguments of that same application may supply absent or whole-slot `_`
  parameter annotations. Arguments and parameters align by the ordinary call
  rules, including product packing and successive function layers.

The direct-callee rule does not inspect the lambda body to discover a
parameter type, retry with another interpretation, use an argument from a later
application, or look through a binding. A nested `_` is not a whole missing
slot and therefore still requires an expected function type. Concrete `: T`
annotations are checked against whichever local context applies; a
contradiction is a type error. See [Type system](#type-system) for the formal
bidirectional account.

When neither route supplies every missing parameter type, the lambda cannot be
typed. A standalone

```kio
let id = .[A](x) { x };          // type error
```

is rejected: an untyped `let` right-hand side is a synthesizing position, so
neither the binding's later uses nor the lambda body supplies the missing
parameter type. A fully concrete typed binding does supply an expected type:

```kio
let .(id: [A] A -> A) = .[A](x) { x };
```

The lambda may instead sit in a known polymorphic call slot:

```kio
fn identity[T](x: T) -> T { x }

let id = identity([A] A -> A, .[A](x) { x });
```

`identity`'s `x: T` slot pins the lambda's type once `T` is instantiated to `[A] A -> A`.

**Type-parameter declarations are allowed.** A polymorphic lambda declares its
`[A]`-style type-binder slots in the same parameter list as its value
parameters. The user-written binders flow through unchanged; the typer never
invents a universal binder. In checking mode, the expected function type
supplies any missing parameter slots. In synthesis mode, value-parameter
annotations must be concrete unless the same direct application supplies a
whole missing slot as described above. An application solves the lambda's
written type arguments by the ordinary positional call rule.

Each type-parameter declaration introduces its own lexical binding. A nested
lambda may reuse an enclosing type-parameter name; references in its scope,
including explicit type arguments and type-constructor applications, denote
the inner parameter. After that lambda's scope ends, the enclosing binding
applies again. Equal spellings do not identify distinct binders.

**Return-type annotation is also optional.** The body block's type is the
`fn`'s return type (per the standard "a block's type is its final expression's
type" rule). The user may write the return type explicitly
(`.(x) -> R { … }`), use the `_` placeholder (`.(x) -> _ { … }`), or drop the
slot entirely (`.(x) { … }`). `-> _` and a dropped slot are treated
identically — the body's synthesized type drives the `fn`'s return type. A
concrete `-> R` instead supplies the body's expected type and checks the body
against it; the same `R` must agree with any expected return type supplied at
the lambda's position. During `impl(fills)` operand-header staging, a lambda
with explicit type binders and no complete expected function type must write
that concrete `-> R` when the result depends on the lambda's own type binders.
An omitted return or `-> _` is also admitted when independently available
surrounding information or returned fill relations determine a result in the
enclosing scope before the body is checked. The body does not determine that
provisional result, and an unanchored result is rejected before body entry.
See [Elaborators are imported, not ambient](#elaborators-are-imported-not-ambient)
for this marked-staging boundary; ordinary calls and monomorphic lambdas retain
their own inference rules.

Top-level `fn` and `host fn` value-parameter types remain **mandatory**.
`host fn` return types are mandatory too; an ordinary top-level `fn` alone may
omit its return annotation to request the documented `.` default. These are
declaration-contract positions rather than anonymous-lambda inference sites.

**Open-world.** Direct-callee completion reads only the literal lambda's
lexical signature groups, the already-resolved expressions supplied by that
same application node, and any expected result type at that position. It does
not search a module, candidate set, or declaration pool. Extending a module
body therefore cannot change an already-typechecking dependent module through
this completion rule. Each argument retains its ordinary synthesis rules,
including literal-role resolution and its documented contract-surface
coupling; this rule adds no stronger same-module candidate-pool promise. Flat
call packet boundaries additionally depend only on that resolved signature and
the written argument/application boundaries; a synthesized argument type does
not trigger an alternate split.

**Consumer impact.** This rule changes shared typechecking and the
materialization of concrete lambda annotations in the checked Kio' tree.
Both full-language and standalone Kio' checking inherit the new acceptance
behavior. Other compiler entry points that use the shared typer — including
LSP diagnostics and `kio sig` — inherit it too; no LSP-protocol,
VS-Code-extension, or consumer-specific implementation is required.
`equiv` and REPL normalization likewise accept the newly well-typed source,
but their reduction and display semantics do not change. Lowering materializes
unit arguments, nested application boundaries, returned type applications,
and elaborator residual functions as ordinary Kio'. Post-typecheck routing and
backend emission preserve those explicit applications, including one ordered
stage per `Forall` even when the backend erases type representations. This is
an internal realization of the existing Kio' evaluation contract; the public
FFI still has value arguments only. The selector gives a deterministic meaning
to the existing flat-call surface; it introduces no parser token, formatter
rule, `kio-gen-rs` surface, LSP protocol change, VS Code extension change, or
syntax-highlighting grammar. The only grammar correction documents `_` as
Kio-surface syntax, matching the existing Kio' rejection.

### Parameter patterns

*Surface only; desugars to Kio'.* The Kio' form is an ordinary `name: Type` value parameter paired with generated projection lets at the head of the function body — see § Desugaring below.

Where a value parameter would otherwise be written `name: T`, the surface admits **product-destructuring patterns** that name the components of a product type instead of (or in addition to) the whole:

```kio
// Today: name the whole product, project inside the body.
fn project_left(p: (A & B)) -> A { fst(A, B, p) }

// With parameter patterns: name the components directly.
fn project_left((a: A, _: B)) -> A { a }

// As-pattern: name the whole alongside its components.
fn dup_pair(p: (a: A, b: B)) -> ((A & B) & (A & B)) { (p, p) }
```

**Forms.** Inside a parameter position, a value parameter may take any of:

- `name: T` — the existing form. `T` is any type.
- `(<pat-elem>, <pat-elem>, …)` — a bare destructuring tuple. The component types together induce the product type `(T1 & T2 & …)`; the parameter has no user-given outer name (a synthetic name is introduced at the Surface → Desugared boundary).
- `name: (<pat-elem>, <pat-elem>, …)` — an as-pattern. Binds the whole product to `name` *and* destructures it into the component bindings. The whole type is the same `(T1 & T2 & …)` the bare form would induce.

Each `<pat-elem>` is one of:

- `name: T` — binds `name` to a component of type `T`.
- `name` — bare-name form; equivalent to `name: _`. The slot's type is `Type::Infer`, resolved by the surrounding context the same way an un-annotated `.(x) { … }` slot is.
- `_: T` — a wildcard slot of type `T`; the component is required to exist but is not bound.
- `_` — bare-wildcard form; equivalent to `_: _`. Same context-driven inference rule as `name`.
- `(<pat-elem>, …)` — a nested bare tuple. The nested product gets a synthetic name at this level (no user-given outer name).
- `name: (<pat-elem>, …)` — a nested as-pattern. Binds `name` to the nested product *and* descends into it.

Nesting is admissible to any depth. The unary form `(a: A)` is admitted (a single-slot product); wildcards may appear at any depth.

**Banned form: outer annotation on a tuple pattern.** The form `(<pat-elem>, …): T` — pinning a type annotation onto a bare destructuring pattern — is *not* admissible (parse error). The pattern's slot types already constitute the product type; an outer annotation would be redundant. If you want to give the whole product a name, write `name: (<pat-elem>, …)`; if you want only the destructuring, write the bare form.

**Applicable positions.** Parameter patterns are admissible everywhere a value parameter is admissible: top-level `fn` definitions, anonymous lambdas (including `match!` clauses — see [§ Pattern matching](#pattern-matching)), and `equiv` arms. The grammar is uniform across those positions (see [`grammar.md` § Surface parameter additions](grammar.md#surface-parameter-additions)). A `host fn` value parameter is the one signature site that does *not* admit a destructuring pattern — it is always a named `name: Type` slot.

**Slot types follow the same elision rules as the surrounding fn.** A leaf slot's `: T` is optional when the slot's type can be resolved from context, exactly like an un-annotated `.(x) { … }` slot:

- In an ordinary anonymous lambda, the slot's type may be pinned by the
  surrounding expected function type. A `match!` clause instead writes its
  dispatch-pattern parameter types independently: neither the scrutinee nor
  the match result supplies a missing parameter leaf.
- In top-level `fn` definitions and `equiv` signatures (the public-contract positions), no context provides a type, so an elided `_` slot is a type error at the same site that catches an unconstrained anonymous lambda.

The pattern's structure pins the *shape* of the parameter's type; slot types (whether explicit or `_`) pin the *leaves*. Mixing explicit and elided slots is admissible — `.((a, b: I32, c)) { … }` is a valid lambda when the surrounding context types the param as `(_ & I32 & _)` and pins the missing leaves.

**Desugaring.** At the Surface → Desugared boundary, every parameter pattern is eliminated by a syntactic rewrite that:

1. Replaces the pattern-bearing parameter with `<outer-name>: <derived-type>`, where `<outer-name>` is the user-given name for the as-pattern form and a synthetic fresh name for the bare form, and `<derived-type>` is the product type built from the pattern's slot types (in source order, separated by `&`).
2. Wraps the function body in generated `let` bindings that project the product with `__fst__` / `__snd__`. Wildcard slots emit no local binding; nested patterns recurse over the projected nested product.

So `fn foo((a: A, b: B)) -> A { use(a, b) }` desugars to

```kio
fn foo(p: (A & B)) -> A {
  let a = __fst__(A, B, p);
  let b = __snd__(A, B, p);
  use(a, b)
}
```

and `fn foo(((a: A, b: B), c: C)) { use(a, b, c) }` desugars to

```kio
fn foo(p0: ((A & B) & C)) {
  let p1 = __fst__((A & B), C, p0);
  let c = __snd__((A & B), C, p0);
  let a = __fst__(A, B, p1);
  let b = __snd__(A, B, p1);
  use(a, b, c)
}
```

with the synthetic names picked to avoid shadowing anything in scope inside the body.

From the post-desugar AST onward, parameter patterns leave no trace — the typer, the resolver, and every later pass see only ordinary `name: Type` parameters plus generated projection lets that do the destructuring.

The same `ParamPatternTuple` grammar appears in block-statement and sequence rich bindings introduced by `let .(...)` (see [Blocks and local bindings](#blocks-and-local-bindings) and [Monadic do blocks](#monadic-do-blocks)). These forms use the same typed, tuple, and as-pattern rules and projection lowering. A typed unary pattern remains `let .(x: T) = e;` after formatting.

### Placeholder lambdas

*Surface only; desugars to Kio'.* The Kio' form is an explicit function literal with the appropriate signature; see [`prime.md`](prime.md#whats-not-in-kio).

A **placeholder lambda** is a compact function literal spelled `.stem. { expr }`. The stem is an ordinary value-binding name ending in an ASCII letter. Both dots and the stem are adjacent; whitespace and comments may separate the final dot from the block. Parameter annotations use an explicit `.(...) { ... }` lambda instead.

```kio
.x. { add(x1, 1) }          // .(x) { add(x, 1) }
.arg. { foo(1, arg1, arg2) }
.x. { g(x2)(x1) }           // reordering
.x. { concat(x1, x1) }      // repetition
._p. { f(_p3) }             // three parameters; the first two are unused
```

A numbered reference is a single ordinary value-path identifier consisting of the stem followed by a positive decimal index, such as `x1` or `arg12`. Indices have no leading zeroes and fit in `u32`; `x0`, `x01`, and an overflowing index are rejected when they denote references owned by `.x.`. The highest owned index determines the parameter count, including unused lower slots. Each placeholder lambda contains at least one owned reference. The stem alone remains an ordinary identifier.

Ownership follows the written body, not module lookup:

- An authored value binder inside the body shadows its spelling in its ordinary lexical region. This includes explicit-lambda parameters, let and pattern binders, row aliases, existential-opening binders, and neutral-block binders. A let initializer is outside its binder's scope. Shadowing is checked before interpreting or validating an index.
- Explicit lambdas retain the enclosing placeholder owner, subject to their own binders. A nested placeholder lambda starts a fresh owner: the outer owner does not classify any reference inside it.
- Only single-segment value paths participate. Qualified paths such as `module.x1` do not. An ordinary UFCS callee such as `value.>x1` participates; a dedicated bang-call or recursive-call callee does not. Labels, type names, and operator implementation names are not reference positions.
- Operator expansion cannot introduce new owned references. A name selected from an operator declaration is an ordinary resolved callee even when it happens to match the family.

Parameter types come from the same expected-type rules as explicit dot-lambdas. For example, `.x. { add(x1, 1) }` checked against `Int -> Int` gives `x1` type `Int`. No ambient declaration can change which source references the lambda owns.

Lowering replaces owned references with hygienic parameters and removes the placeholder form before [Kio'](prime.md). The resulting function uses only ordinary Kio' constructs. Names such as `x1` remain ordinary identifiers in Kio'; only the `.stem.` introduction and its source ownership rule are surface syntax.

### Function types

A function type has the same grouped shape as a `fn` header without names. Product domains use `&`; commas remain signature/list separators, not type syntax:

```kio
(Int & String) -> Bool
[A] A -> A
```

Single-parameter function types may also be written with a parenthesized domain `(Int) -> Int`. The formatter still emits the canonical bare unary form `Int -> Int`, so write the bare form in examples and committed source. Compound parameter types remain parenthesized: write `(A & B) -> C` or `(A | B) -> C`, not `A & B -> C` or `A | B -> C`.

### Conditionals

*Surface only; elaborates to Kio'.* The reference library's conditional is an
ordinary imported block elaborator:

```kio
import elab/control(if);

if! cond { a } else { b }
```

Its public type is `[Condition][Result] Condition -> (. -> Result) ->
(. -> Result) -> Result`, with an unlabelled `thunk` descriptor followed by
`trailing thunk else;`. Its implementation constructs ordinary
`__if_then_else__` glue from the checked condition and the two projected
thunks. The condition must have the uniquely selected `role(bool)` host
identity in unqualified lexical scope, as required by the branching intrinsic.
Directly in-scope host types form the selected pool when present; transparent
aliases supply the fallback otherwise. Repeated paths to one host declaration
count once, while distinct selected identities remain ambiguous. A qualified
module import alone contributes none of its members. Annotating the condition
does not resolve a multiple-identity pool. The library's generic `Condition`
parameter does not relax this intrinsic contract; see
[`prime.md`](prime.md#branching-intrinsic).

Both arms are blocks, and their results jointly constrain the one result type
of the whole expression. In check mode, the surrounding expected type checks
both arms directly. In synth mode, either arm whose result is independently
determined may establish or refine the common result; swapping the arms does
not change whether the expression is accepted or which result type it infers.
If neither context nor either arm determines the result, the common-result
relation is underdetermined. Independently determined arm results that disagree
produce a non-directional common-result mismatch. The typer never forms an
implicit sum from incompatible arms; a branch that contributes to a sum
constructs it explicitly, normally with `widen_sum!`.

Result inference does not flow backward into `cond`. The library evaluates the
condition once and only the selected arm. Its public type relates exactly the
two written arm results and consults no ambient declaration pool, so adding
an unrelated module-body declaration cannot change its result or evaluation.
The same behavior under a renamed import follows from that declaration and
its ordinary implementation. Other declarations named `if` acquire no such
behavior by their spelling.

### Pattern matching

*Surface only; desugars to Kio'.* Pattern matching is the `match!` dispatch elaborator, imported like any other elaborator (see [§ Elaborators are imported, not ambient](#elaborators-are-imported-not-ambient)). It is an elaborator because compile-time code synthesizes branch-selection glue at the call site; it is not a source-to-target coercion. The Kio' form `let`-binds each clause as a `fn`, discriminates the scrutinee's sums with nested `__either__` calls, and projects products with `__fst__` / `__snd__` / projector-member calls — see § Desugaring below.

```kio
import elab/match(match);

match! <value> {
  .(<param-list>) { <body> };
  .(<param-list>) { <body> }
}
```

`match!` takes a single scrutinee prefix and one `product` block containing its
clause expressions. **Each clause is an expression of function type** — there
is no dedicated clause construct and no `case` keyword. A clause may be an
inline lambda, a name, a member projection, or a call. Semicolons separate
block entries. One entry preserves its value unchanged, so a preassembled
product of clauses remains expressible as `match! value { clauses }`.
Ordinary tuple grouping inside an entry retains its ordinary meaning. The
block-call rules require direct spelling; there is no blockless or UFCS
`match!` alternate.

Whatever expression a clause is, it must independently determine the
**parameter type** of its function. That parameter type is the **dispatch
pattern**, declaring which DNF branch of the scrutinee the clause handles.
`match!` supplies no expected parameter shape from the scrutinee: the
independently determined parameter type specifies which source branches the
clause accepts rather than receiving the scrutinee's type by context. A clause lambda
therefore writes its value-parameter types; a name or other expression must
synthesize a function type whose parameter component is already determined.
Either way, `match!` reads that parameter component to decide dispatch. This
independence applies only to the parameter component; the clause result follows
the common-result rule below.

```kio
match! r {
  .(v: ok(I32)) { handler_ok(ok(I32, v)) };
  .(e: err(Str)) { handler_err(err(Str, e)) }
}
```

**Dispatch.** Dispatch is **per DNF branch**, not whole-scrutinee. The imported `match!` implementation normalizes the reflected scrutinee type to DNF (a sum of products) and considers each DNF branch separately. Each clause's parameter type is *also* DNF-normalized; for each scrutinee branch, that implementation walks clauses in source order and tests whether the branch's factors structurally fit **some** DNF branch of the clause's parameter type (product projection plus structural rearrangement, never partial sum narrowing) — the **first clause that fits** wins for that branch. The generated term calls the chosen clause function with a value reconstructed to *exactly* the parameter type from the matched scrutinee components. Different DNF branches may select different clauses, with sum *discrimination* handled by the generated decision tree. The exact per-branch fit and decision-tree algorithm — which library coercion fires at each slot — is documented in the case studies [`docs/poc/optics.md`](../docs/poc/optics.md) (which exercises `match!` directly) and [`docs/poc/elab.md`](../docs/poc/elab.md).

Concretely — a clause slot's annotated type may be **any structural type** — atom, product, sum, or sum-of-products — not only a fully-destructured atom:

- A clause `.(v: T) { … }` with `T` an atom fires on every DNF branch that fits `T`. For a sum-typed scrutinee, that's typically a single branch; for a product-typed scrutinee `X & Y` it projects to `T` when `T` is a sub-product.
- A **compound product slot** `.(pair: (A & B), c: C) { … }` on a `((A & B) & C)` scrutinee binds `pair` to the `(A & B)` sub-product and `c` to the `C` — the slot type is matched and the value reconstructed by the same structural coercion that fits any slot.
- A **sum slot** `.(x: (A | B)) { … }` covers *every* scrutinee branch matching either arm — a single clause can cover multiple branches, with `x` bound (via the appropriate sum arm) differently per branch. A sum-of-products slot works the same way, one DNF branch of the slot type per arm.
- A multi-parameter clause `.(a: A, b: B) { … }` fires on branches whose factors fill both slots — slots are filled left-to-right (the source-order rule, lifted one level).
- A zero-parameter clause `.() { … }`, and a `.`-typed slot, fire on any branch: every type fits `.` by projecting to the empty product. `.() { … }` is the **catch-all**. No special catch-all syntax is needed.

Type-parameter declarations (`[A]`) written in a clause `fn` work exactly as in any `fn`. The one twist is who supplies the type arguments: `match!`, as the caller, specializes the clause's explicit leading binders against each compatible scrutinee branch rather than taking them from a written call site. Writing `.[A](v: ok(A)) -> String { … }` matches any `ok(_)` branch and binds `A` to the payload type. A polymorphic clause used by several source branches contributes one specialized result per branch, in source-branch order.

An inline polymorphic clause follows the marked-lambda staging rules in
[Elaborators are imported, not ambient](#elaborators-are-imported-not-ambient).
Its written binder and parameter header is complete before staging. A result
that depends on a newly introduced clause type binder requires a concrete
return annotation or a complete expected function type; an independently
known common result may check an omitted result before its body is entered.
A named or otherwise already-typed polymorphic clause value retains its type.

**Result type.** Every clause function must return the same result type. The
marked implementation records candidate relations in written-clause order;
within one polymorphic clause, compatible specializations follow source-branch
order. That authored order remains separate from runtime dispatch and gives no
candidate inference priority. Every written clause contributes validation even
when first-match reachability later reports it as shadowed.

In check mode, the surrounding expected type checks every clause result.
A concrete local binding or function return annotation can supply that type;
block calls have no explicit trailing type-argument spelling. In synth mode
without an expected result, all lawful returned result occurrences jointly
constrain one common result. Any occurrence whose type is independently
determined may establish or refine it, so reordering clauses does not change
whether their results can be inferred or which result type they infer. If no
context or occurrence determines the result, the common-result relation is
underdetermined; independently determined occurrences that disagree produce a
non-directional common-result mismatch. The typer does not form an implicit
sum; a clause that contributes to a sum constructs it explicitly, normally
with `widen_sum!`. This result flow checks a clause-lambda body without changing
its independently written parameter types. A clause lambda may carry an
explicit `-> R` return annotation; that annotation is checked against the
common result and also seeds its body when no match result is otherwise
available (see [Anonymous functions](#anonymous-functions)).

**Exhaustiveness.** The DNF branches of the scrutinee's type must be **covered**: every DNF branch must fit at least one clause's parameter-product type. A `match!` whose clauses leave some DNF branch uncovered is a compile-time error in the elaborator-error category (exit code `15` — `match!` is a user-defined elaborator, so its coverage failures surface via `__elab_error__`; see [`exit-codes.md`](exit-codes.md)). A trailing `.() { … }` catch-all covers everything, so it always makes a `match!` exhaustive regardless of earlier clauses.

**Reachability.** Every clause must cover at least one DNF branch that earlier clauses do not cover. A clause that adds no new coverage is unreachable — a compile-time error in the same elaborator-error category (exit code `15`). A clause that covers nothing at all is also a `15` elaborator error — including a slot whose type DNF-normalizes to zero branches (a `!`-typed slot covers no branch). First-match ordering means an earlier clause can eclipse a later one. A slot type **wider** than the branches it ends up covering is *not* an error: a sum slot whose later arms were already taken by narrower sibling clauses is fine — the slot type is an upper bound, not a coverage obligation.

**Desugaring.** `match!` lowers to ordinary Kio': it `let`-binds each clause once (evaluating the clause expressions in source order, before any branch is taken), builds a decision tree of nested `__either__` calls over the scrutinee's DNF branches, and at each leaf calls the selected clause function with its argument reconstructed by `let` / `__either__` / projection-intrinsic glue. The reconstruction is duplication-free — a scrutinee factor is never threaded through two slot positions of one clause leaf, and the clause's return value is evaluated once. The elaboration is recorded in the side channel and substituted at the Lowered → Prime boundary; no `match!` form survives into Kio'. The per-leaf coercion algorithm is worked through in [`docs/poc/optics.md`](../docs/poc/optics.md) and [`docs/poc/elab.md`](../docs/poc/elab.md).

**Type flow.** `match!` is an elaborator position (see [Type system](#type-system)
for the bidirectional-flow mechanism). The scrutinee synthesizes bottom-up, and
each clause independently determines the parameter type that serves as its
dispatch target; the elaborator decides which clause handles each branch and
what projection produces its argument. Only after those prerequisites are
known do the authenticated returned occurrences participate in their finite
symmetric common-result relation. A surrounding expected type checks every
result, including an inline clause body;
without one, any independently determined returned occurrence may determine
the shared result. Result flow never supplies the scrutinee type or a clause's
dispatch-pattern parameter shape.

The relation contains only occurrences from the written clauses and the finite
authenticated fill transcript produced by the already-resolved imported
elaborator. It never searches visible declarations, so adding an unrelated
module-body declaration cannot add an occurrence, change the common result, or
alter dispatch.

`match!` takes an explicit scrutinee/domain value, checks coverage and reachability against that scrutinee's DNF, binds polymorphic clause type parameters from each concrete branch, and returns the common selected-clause result type. It is imported explicitly like any other elaborator (see [§ Elaborators are imported, not ambient](#elaborators-are-imported-not-ambient)).

```kio
import elab/match(match);

match! r {
  .(v: ok(I32))  { handle_ok(ok(I32, v)) };
  .(e: err(Str)) { handle_err(err(Str, e)) }
}
```

**Type.** `match! v { clauses }` has the common clause result type `R`;
every clause must return `R`. Coverage/reachability are checked against `v`'s
DNF, and polymorphic clause binders (`.[A](…)`) are solved from the branch
selected for each leaf.

**Clauses.** The projected block value is a single clause or a product of
clauses. A non-function clause is a type error at the offending position.
Grouping a single clause does not construct a unary product.

**Result type derived from the clauses.** `match!` carries a **same-result type
rule** (see [§ Type-argument inference and the `_` placeholder](#type-system)):
every clause result must conform to one result type. `match! v { clauses }`
derives the result from the clauses' joint finite relation when no surrounding
expected type is available. Any independently
determined clause result may determine the common result; source order does not
choose an inference anchor. When the common result is known, it checks every clause result and
flows into an inline clause body; it does not determine the clause's
dispatch-pattern parameters.

**Elaboration.** `match!` elaborates to the decision tree over the scrutinee's DNF branches, the per-leaf reconstruction at each value-parameter slot and the return position, and the `let`-bind-each-clause-once discipline described above. Behind the sugar it is ordinary Kio' — `let`, `__either__`, and the projection intrinsics — and no `match!` form survives into Kio'.

### The `derive!` elaborator

*Surface only; desugars to Kio'.*

`derive!` constructs a value of a requested **target type** by composing a caller-supplied set of **rule functions**, threading their outputs into one another's parameters until the target is produced. It is imported explicitly like any other elaborator (see [§ Elaborators are imported, not ambient](#elaborators-are-imported-not-ambient)). The motivating use case is building typeclass-like *instances* — most importantly deriving a `Monad` / `Applicative` / `Functor` instance for a composite type constructor (a monad-transformer stack) from base instances plus per-transformer rules — but the form is type-constructor-agnostic: any family of `fn`s whose signatures read as inference rules participates.

```kio
import derive(derive);        // imported like any elaborator

derive!((
  , <candidate1>
  , <candidate2>
  , …
), <TargetType>)
```

The leading-comma layout inside the multi-candidate product matches Kio's existing multi-item-list style. The candidate argument is the first (value) argument: `()` supplies no candidates, one function expression supplies one candidate directly, and a product tuple supplies multiple candidates. The target type is the trailing slot, the same convention the other elaborator bangs use for their trailing target slot. The target may be elided — omitted, or written `_` — when a check-mode position (a function's declared return type, a value-argument slot, or the like) pins it, since a plain `let` carries no annotation. Because `_` marks the type slot rather than a value, it may equivalently precede the candidate argument (`derive!(_, <candidates>)`); a written target *type* is accepted only in the trailing slot:

```kio
fn stack_instance() -> Monad(State_t(Int, Maybe_t(Maybe))) {
  derive!((
    , state_t_monad
    , maybe_t_monad
    , maybe_monad
  ))
}
```

**Type.** `derive!` has scheme `[A] A -> A` over the target type `A`: `derive!(<candidates>, T)` synthesizes a value of type `T`. The candidate argument is a side input to resolution, not a value the form returns. The empty-candidate argument `derive!((), T)` means "no candidates" and therefore fails with "no rule applies" for every target, including `.` and structurally inhabited product or sum types. Values arise only from caller-supplied candidate rules.

**Candidates are function expressions.** A singleton candidate, and each element of a multi-candidate product, is an expression of function type — idiomatically a reference to a top-level or local `fn`, or an inline [`fn`](#anonymous-functions-fn) literal. A candidate's signature *is* an inference rule:

```
fn <rule-name>[T1](…)[TN](// binders, each with optional kind annotation
               h1: H1, // preconditions
               …, hk: Hk) -> R               // R is the result type the rule produces
```

**Every logical value slot is a precondition.** The imported `derive!` implementation treats each `Hi` as a sub-goal to discharge from the same candidate set. A base instance is the zero-precondition case (`. -> R`) and can explicitly produce any result type `R`, including `.`, a host type, a product, a sum, or an opaque `newtype`. Kio canonicalizes both `fn base() -> R` and `fn base(_unit: .) -> R` to that same logical zero-slot function shape; neither requires a candidate that derives `.`. A Unit parameter within a multi-slot value group remains an ordinary precondition. To admit a rule that takes an ordinary parameter that is *not* a precondition (a literal, a flag), partially apply it inside an inline function literal in the candidate argument. Binders may carry any kind (`[A]` is `*`, `[*F]` is `*→*`, `[**G]` is `*→*→*`); the implementation respects the annotation when unifying.

A singleton candidate that is not of function type, or an element of a multi-candidate product that is not of function type, is a type error pointing at the offending position (`derive_non_function_candidate`).

**Resolution.** Given the goal `T` and the candidate set, the `derive!` implementation finds the **unique** composition of candidates whose result types thread together to produce `T`, treating each candidate's value parameters as sub-goals to discharge from the same set. Resolution is kind-aware (a candidate binder of kind `*→*` binds only to a kind-`*→*` argument), reports an elaborator error when no composition applies or when two distinct compositions exist, and is total by the structural-decrease rules governing its compile-time recursion. The resolution algorithm, the exactly-one-coherence rule, the static termination conditions, and a worked monad-transformer-stack derivation are documented in the case studies [`docs/poc/hkt.md`](../docs/poc/hkt.md) (which exercises `derive!`) and [`docs/poc/elab.md`](../docs/poc/elab.md) (which exercises every resolution shape, including polymorphic and recursive derivations).

The goal may carry **unbound binders inherited from the enclosing `fn`** (a type-constructor-generic combinator with a `derive!` site whose target mentions the function's own `[*F]`). The implementation's structural unifier treats those as logic variables; the generated derivation is then polymorphic in them and the enclosing function compiles to a single type-constructor-generic body.

**Open-world.** The candidate set is exactly the function expressions the candidate argument literally supplies; `derive!` cannot reach module-level `fn`s or explicitly imported items implicitly, and the ordinary import brings only `derive` itself into scope, never any candidate. A "module handle" candidate (passing some module's exports as the candidate set) is **rejected** — it would let another package change resolution at an existing site by adding or removing exports. Adding a `fn` anywhere in the local package source provably cannot change resolution at any existing `derive!` site, because there is no implicit pool to grow. This is the same local open-world guarantee every elaborator carries.

**Elaboration.** The imported elaborator implementation executes at compile time and returns a checked nested call tree of the chosen candidates applied to their bound type arguments and recursively generated precondition values — exactly what the user would write by hand. The front-end records that checked term in the same boundary-local side channel used by every bang call; the substitute pass swaps the `derive!` call for the recorded tree at the Lowered → Prime boundary, and standalone Prime validation checks the assembled package. No `derive!` form or compile-time machinery survives into Kio': what remains is an ordinary call tree of the caller-supplied rule functions, executed at runtime like equivalent hand-written code. The rules themselves need no `pure` marker because they are runtime values placed in that tree; the elaborator's own implementation and helpers obey the ordinary compile-time purity contract.

`derive!` imposes no backend constraint — it synthesizes an ordinary call tree, so a derived instance emits exactly as the equivalent hand-written composition would. This holds for the transformer-stack goal too: a transformer type constructor (`State_t`, `Maybe_t` — a newtype with a kind-`*→*` parameter) in a kind-`*→*` position emits on every backend, including `rust`, where it rides the erased `Rc<dyn Any>` body like every other value (see [`specs/backends/rust.md` § Higher-kinded types](backends/rust.md#higher-kinded-types)).

### Scoped expressions

*Surface only; elaborates to Kio'.*

The reference `scope!` elaborator accepts one `thunk` block and invokes it
once. Its body uses ordinary scoped local bindings, row and existential-opening
lets, and expression statements. Its final expression is the result; an empty
body returns unit. Import it explicitly to put a scoped computation in an
expression position, such as an `equiv` arm:

```kio
import elab/control(scope);

equiv id_unfolds[A](x: A) {
  scope! {
    let y = id(x);
    y
  };
  x
}
```

`scope!` and `do!` are distinct declarations with distinct descriptors and
public types. `scope!` does not admit `<-` bindings. Its ordinary expansion
retains the body's recursive tail behavior when the call is in tail position.

### Monadic `do!` blocks

*Surface only; desugars to Kio'.*

The reference `do!` elaborator runs a `sequence` block with an explicitly
supplied bind function. The library exports the transparent aliases:

```kio
pub type Bind[*F] = [A][B] (F(A) & (A -> F(B))) -> F(B);
pub type Sequence[*F][R] = Bind(F) -> F(R);
```

Its public type is `[Receiver][Result] Receiver -> (Receiver -> Result) -> Result`,
with one `trailing sequence;` descriptor. Descriptor validation requires the
receiver's structural type to be `[A][B] (M(A) & (A -> M(B))) -> M(B)` for
one fixed carrier `M`, and the block's final result to have that same carrier.
`A` and `B` remain independently polymorphic at each bind call. `M` may be a
concrete constructor such as `Box`, an abstract kind-`*→*` binder, or a
structural carrier such as identity `M(A) = A` or constant `M(A) = .`;
the `Bind` alias is not a restriction to first-class constructor arguments.
The compiler recognizes the structural bind type, not the aliases' names or
the identity of `do`. Every sequence block projects to an ordinary
`Receiver -> Result` function, independently of which elaborator receives it.

The receiver's carrier is determined by available ordinary public-call and
header constraints. The sequence body does not infer a missing carrier
argument of a receiver factory. For a factory declared as
`make_bind[*M]() -> [A][B] (M(A) & (A -> M(B))) -> M(B)`,
`make_bind(Box, ())` supplies that carrier and its unit value argument;
`make_bind()` cannot obtain `M` from the block's actions or final expression.
Ordinary value arguments to a factory may determine its carrier as usual.

`do! bind { … }` evaluates the receiver expression exactly once per execution,
before running the projected sequence, including when the block contains no
bind steps. Zero, one, or multiple steps use the same resulting bind value.
An impure receiver expression remains an executable dependency in a `pure fn`
even when the block never invokes the bind value.

**Statement forms.** A sequence block admits three statement shapes plus a
trailing final expression. A binder is an ordinary name or `_`, or an explicit
rich pattern `.(PATTERN)`: typed `.(x: T)`, tuple `.(x, y)`, or as-pattern
`.(whole: (x: A, y: B))`. Ordinary `=` bindings also admit row and
existential-opening forms under [Blocks and local bindings](#blocks-and-local-bindings).
The `<-` connective uses the name, wildcard, typed, tuple, or as-pattern forms;
it does not introduce a row or existential-opening binder.

| Form | Surface | Desugared |
|---|---|---|
| **Bind** | `let <binder> <- e;` | A bind call with a continuation whose parameter uses the binder's pattern; the bind type connects `e`'s `F(T)` payload to that parameter |
| **Pure let** | `let <binder> = e;` | An ordinary lexical let over the continuation, with the block-let typed/untyped RHS rule; no bind step |
| **Sequence** | `e;` | `bind(e, .(_: .) { <rest> })` — `e` must type as `F(.)`; the continuation's unit payload is discarded |
| **Final expression** | `e` | The block's overall value; must type as `F(R)` for the same `F` |

Empty blocks and statement-only blocks are type errors after descriptor
resolution. The parser accepts their neutral syntax. A final semicolon after
a real final expression is accepted and ignored.

Continuation functions preserve a written binder annotation; otherwise the
bind function's expected continuation type supplies the parameter type. A
sequenced expression requires `F(.)`; an explicit `let _ <- e` is an ordinary
bind and may discard a non-unit payload. The resolved bind shape and the
ordinary call's expected result connect the carrier and result types. Later
uses in the continuation never infer the earlier source's payload type across
its binding boundary. Ordinary lets and actions retain their source-first,
once-only checking rules.
This extends the expression-block clause-order principle to sequence clauses,
not just named bindings: an earlier unbound action checks against `F(.)`
without obtaining its payload from later clauses. The nested bind calls still
share their ordinary carrier and result constraints; `;` is not an additional
ban on those equations, and a trailing separator changes none of them.

A written bind-pattern annotation is classified at its original source
occurrence by the same post-materialization rule as an ordinary local binding.
Desugaring may carry that annotation into the generated continuation lambda,
but it neither duplicates the source occurrence nor changes its diagnostic
span. A sequence's `=` binding follows the ordinary local-`let` rule.

**`<-` vs user-defined operator.** The `<-` token is an ordinary operator
character run. Only a structurally introduced `let NAME` / `let _` /
`let .(PATTERN)` binding consumes it as a binding connective. Calls such as
`let(f)` remain ordinary expressions, including as operands of `<-`.
Neutral blocks record the binding form without declaration lookup; a resolved
`sequence` descriptor admits it, while `product` and `thunk` reject it with a
type error. Ordinary function bodies accept only `=` bindings.

**Nesting.** `do!` calls nest freely. An inner call is an ordinary monadic
expression used in the outer block like any other RHS; bind functions may
differ across nested levels.

**Explicit final result.** The user writes `pure(v)` or another `F(R)` result
explicitly. Neither the sequence descriptor nor the `do!` library introduces
an implicit lift.

**Projection.** A sequence block becomes an ordinary function taking a bind
function. Its body right-folds onto the final expression:

- `let x <- e;` / `let .(x: T) <- e;` followed by `rest` become `recv(e, .(x[: T]) { rest })`.
- `let x = e;` / `let .(x: T) = e;` followed by `rest` become an ordinary checked or synthesized local binding over `rest`.
- `e;` followed by `rest` becomes `recv(e, .(_do_seq: .) { rest })`.
- The trailing final expression is the innermost continuation body.

Each projected bind call is typed against its ordinary public function type.
The `do!` implementation applies the projected function to its supplied bind
value. All descriptor and projection metadata is consumed before Kio'; the
result contains ordinary functions, calls, and lets. The generated
continuations remain ordinary nested functions: a sequence descriptor grants
no additional recursive tail positions.

**Worked example.**

```kio
import elab/sequence(do);

newtype Box[A] : A { pub constructor mk_box; pub projector un_box; };

fn box_pure[A](x: A) -> Box(A) { Box.mk_box(A, x) }

fn box_bind[A][B](m: Box(A), k: A -> Box(B)) -> Box(B) { k(Box.un_box(A, m)) }

fn run() -> Box(String) {
  do! box_bind {
    let a <- box_pure("hello");
    let b = "world";                       // pure let — no bind step
    let s <- box_pure(string_concat(a, b));
    box_pure(string_concat(s, "\n"))
  }
}
```

Desugars to a nested `box_bind`-and-`fn` chain with `let b = …` interposed in the second continuation's body — the pure-let stays lexical, only the bind steps emit `box_bind` calls.

### UFCS

*Surface only; substituted before Kio'.*

**UFCS** — *Uniform Function Call Syntax* — is the dot-led family of call splices. The four spellings share one rule for ordinary functions, type-member functions, and blockless bang-call elaborators:

```kio
r.>f           // = f(r)
r.>f(args)     // = f(r, args)
r.>f(T, x)     // = f(T, r, x)
r.>>f(T, x)    // = f(T, x, r)
r.>m.f(args)   // = m.f(r, args)
r.>T.member(A) // = T.member(A, r)

f.<r           // = f(r)
f(T, x).<r     // = f(T, x, r)
f.<<r          // = f(r)
f(T, x).<<r    // = f(T, r, x)

r.>iso!(T)     // = iso!(r, T)
iso!(T).<<r    // = iso!(r, T)
```

For `.>` and `.>>`, the left-hand side is the value being inserted and the right-hand side is a path-shaped callee — a single name `f`, a qualified `m.f` (module alias `m`), or a type-member `T.f`. The trailing argument list is optional: argless `r.>f` and `r.>>f` both mean `f(r)`, while `r.>f(args)` inserts `r` first and `r.>>f(args)` inserts `r` last.

For `.<` and `.<<`, the left-hand side is the path-shaped callee, optionally with one direct argument list already written; the right-hand side is the value being inserted. The zero-existing-arg forms omit `()`: `f.<r` and `f.<<r` both mean `f(r)`. With existing args, `.<` appends the inserted value and `.<<` prepends it. The accepted left side is syntactic — a path or direct path call — so `f(args).<r` is admitted, while an arbitrary expression on the left is not retroactively treated as a function by type.

A present UFCS argument list must contain at least one argument. The explicit-empty spellings `r.>f()`, `r.>>f()`, `f().<r`, and `f().<<r` are parse errors, uniformly for ordinary, member, and bang-call callees. Use the corresponding bare spelling (`r.>f`, `r.>>f`, `f.<r`, `f.<<r`) when the receiver is the only value, or put an explicit Unit in the list (`r.>f(())`, `r.>>f(())`, `f(()).<r`, `f(()).<<r`) when Unit is an additional argument. The parse diagnostic points at the empty list and names both repairs. Bang callees retain bare receiver-only forms in every direction, including `r.>transform!`, `r.>>transform!`, `transform!.<r`, and `transform!.<<r`.

The right-hand inserted value for `.<` / `.<<` is tight: it is a primary expression plus ordinary call applications, stopping before another dot-splice suffix. Thus `f.<x.>g` parses as `(f.<x).>g`; to insert the result of a UFCS expression, write `f.<(x.>g)`. Parentheses around any splice expression work normally and decide grouping.

UFCS chains compose by ordinary nesting. `r.>f.>>g(x)` parses as the outer splice over the result of `r.>f`. `f.<x.>g` parses as the outer `.>` over the result of `f.<x`, while `f.<(x.>g)` inserts the result of `x.>g` into `f`.

**Call rule.** A dot-splice resolves the explicitly written callee first, then inserts the receiver into that callee's value-argument stream:

1. Resolve the callee as the written callable: a regular path for non-bang forms, or a scoped user-defined elaborator name for bang forms.
2. Read the resolved callee type as a sequence of `Forall` type slots and
   value slots derived from each `Function` domain. Explicit type arguments in
   the surface argument list remain in type slots. At the receiver boundary,
   type-looking candidates are reserved for the consecutive binder run
   immediately after the receiver; the adjacent binder run before the
   receiver consumes only the surplus. An unfilled value layer is a hard
   boundary, so reservation never crosses it. The split depends only on this
   type and the written argument kinds, never on declaration parameter groups,
   callee provenance, inferred argument types, or ambient declaration candidates.
3. Insert the receiver into a value slot only. `.>` and `.<<` fill the first value slot; `.>>` and `.<` fill the last value slot. Dot-splice never inserts a value into a type slot.
4. Typecheck the resulting call plan with the same argument-slot planner used for prefix calls. A mismatch at the receiver's planned slot is an ordinary call-argument type error.

Thus `r.>f(T, x)` has the same call plan as `f(T, r, x)`, and `r.>>f(T, x)` has the same call plan as `f(T, x, r)`. The same rule applies to bang-call callees: `r.>iso!(T)` and `iso!(T).<<r` both plan as `iso!(r, T)`.

Each UFCS spelling has the same evaluation order as its equivalent prefix call above. For an ordinary call, the callee is evaluated once and value arguments are evaluated once from left to right: insertion-first forms evaluate the receiver before the written value arguments, while insertion-last forms evaluate the written value arguments before the receiver. For a bang call, the equivalence applies to elaborator argument planning and quotation; UFCS does not turn quoted operands into runtime evaluations.

**Type-argument solving.** UFCS accepts the same explicit type-argument syntax as prefix calls. A type argument still has to land in a type slot; a type argument supplied where the planned slot is a value slot produces the same diagnostic a prefix call would. When a callee binder is not explicitly supplied, the ordinary call machinery solves it from value-argument types and, in checking mode, the expected result type. Any callee type parameter that remains unsolved is a type error.

**Bidirectional flow through UFCS chains.** A splice step's expected return type — set by the surrounding context (e.g., the next chain step's inserted-argument position, or a `let` binding's expected type when one is present) — propagates down to the step being checked. This is the same expected-type-at-the-position mechanism the typer uses for elaborator targets and `fn` parameter slots; UFCS chains pick it up through the rewritten call shape, so elided elaborator targets in chained positions (e.g. `r.>onto!.>callee` with `onto!`'s target unspoken) get filled from the next callee's required argument type.

**Errors.** Each gets a clear diagnostic:

- "dot-splice receiver has no value-argument slot to fill" — the callee has only type slots, or no argument slots at all.
- "an explicitly empty UFCS argument list is ambiguous" — remove the list for the bare receiver-only form, or replace it with `(())` to pass Unit explicitly.
- "type mismatch: expected `<T>`, found `<R>`" — the receiver doesn't match its planned value slot.
- "callee `<path>` is not callable: its type is not a function" — the resolved value isn't a function.
- "callee `<path>`'s type parameter `[A]` could not be resolved" — the receiver and arguments don't pin every type parameter.

**No projector fallback.** A name `f` that isn't a free value in scope cannot be reached by UFCS — even when it's a member of a newtype `F` that is in scope. To call a projector-shaped member receiver-style, bring it in as a free value (`import m(f);`) or use the newtype-member spelling (`F.f(r)`).

**Worked examples.**

Non-compound receiver:

```kio
fn shout(s: String) -> String { …}

let s = "hello\n";
let msg = s.>shout;     // = shout(s)
print(msg)
```

Compound receiver — narrow with `onto!`, then chain:

```kio
newtype Tag : . { pub constructor mk_tag; pub projector un_tag; };

fn shout(s: String) -> String { … }

let pair = (Tag.mk_tag(()), "hello\n");   // pair : Tag & String
pair.>onto!.>shout.>print                  // pair narrowed to String, then chained;
                                          // onto!'s target is filled from the next
                                          // callee's first-param via bidirectional flow.
```

Polymorphic callee (type-arg backsolved from the matched conjunct):

```kio
fn id[A](x: A) -> A { x }

let s = "hi\n";
let result = s.>id(String);  // explicit [A] = String;
                        // elaborates to `id(String, s)`
print(result)
```

Qualified callee (module-aliased):

```kio
import mymod as m;
let r = …;
r.>m.f(x)               // = m.f(r, x)
r.>>m.f(x)              // = m.f(x, r)
m.f(x).<r               // = m.f(x, r)
m.f(x).<<r              // = m.f(r, x)
```

**Lowering.** UFCS survives through the resolved and lowered typing phase. The typer resolves the named callee, plans the value-slot insertion from the callee's actual call type, and records the normalized replacement. Regular UFCS records an ordinary prefix `Expr::Call`; bang UFCS records the same user-elaborator expansion the equivalent prefix spelling would have produced. The substitute pass swaps the recorded replacement in at the Lowered → Prime boundary.

The Kio' grammar has no UFCS syntax in expression position; the kio-prime backend's parser rejects it the same way it rejects every other Kio-surface-only form. **Surface preservation:** `kio fmt` round-trips the user-authored spelling (UFCS vs prefix, and `.>` / `.>>` / `.<` / `.<<`) for both regular calls and elaborator bang-call splices — see [`style.md` § UFCS preservation](style.md).

**Elaborator bang-call splices.** Blockless elaborator calls participate in the same dot-splice rule as ordinary calls. Elaborators with trailing-block descriptors require their complete direct block spelling. The imported elaborator's public type determines the blockless call slots, including which surface arguments are type arguments and which are values. The parens after a right-callee elaborator name carry the ordinary post-`!` argument list and follow the same parens-iff-args rule as the rest of UFCS — dropped when there is no trailing argument to write:

```kio
r.>iso!           // = iso!(r)
r.>iso!(T)        // = iso!(r, T)
r.>>iso!          // = iso!(r)
r.>>iso!(T)       // = iso!(r, T)     — `T` stays in the type slot
iso!(T).<<r       // = iso!(r, T)
r.>into!(T)       // = into!(r, T)
r.>onto!(T)       // = onto!(r, T)
r.>align!(T)      // = align!(r, T)
r.>ease!(T)       // = ease!(r, T)
r.>atom!           // = atom!(r)     — target inferred from source DNF
r.>atom!(T)        // = atom!(r, T)
derive!(T).<cs     // = derive!(cs, T)
```

The rule is intentionally uniform: `r.>>iso!(T)` is equivalent to `r.>iso!(T)` because `iso!` has only one value slot and `T` stays in its type slot. Direction matters for a blockless elaborator with several value slots, following the same ordinary first/last-slot rule. A bang-suffix callee that resolves to no scoped user-defined elaborator is reported as an unresolved elaborator call.

### Operators

*Surface only; desugars to Kio'.*

Kio supports user-defined fixed operators via top-level `op` declarations and
variadic operators via `varop` declarations. An operator
binds to ordinary callables in scope. Parsing records an operator expression
under its explicit grammar; operator lowering replaces it with ordinary calls:

```kio
fn add(a: Int, b: Int) -> Int { … }
op _ + __ { impl add; };

fn sum_three(x: Int, y: Int, z: Int) -> Int {
  x + y + z      // = add(x, add(y, z))
}
```

The `impl` field uses the same lexical value-path grammar as every fold
callable and elaborator implementation. It may name a local or selectively
imported function, select through an explicit module alias, or select a
constructor/projector on a local or imported newtype, directly or through an
identity alias. It is not an arbitrary expression and does not admit a
slash-qualified package FQN.

These callable fields use ordinary path-expression source-order classes. A
same-module ordinary or host `fn`, `rec` member, newtype head, or identity-alias
head must precede the `op` or `varop`. Self-qualification and selective self-imports
do not bypass the declaration-order rule.

Operator syntax is source ordered too: an expression may use an exactly
imported operator or a local `op` or `varop` declared before its enclosing item.
Function bodies and recursive-group members do not see operators declared
later in the module. Deferring a body's parsing does not change that scope.

**Pattern shape and associativity.** An `op` pattern alternates **slot tokens** with **operator tokens**. The slot kinds are:

- `_` — ordinary slot, admits a single atom or paren-expression.
- `__` — **self-recursive slot**: admits the surrounding operator's same-op chain. Position relative to the operator pins associativity.
- `___` — **greedy slot**: admits a chain of *any* single operator (possibly different from the surrounding one), with cross-operator mixing *inside* the slot still rejected.

At most one of `__` or `___` per pattern. Two adjacent slots are a parse error (`op _ _ + _ { impl …; };` is rejected). Underscore runs of four or more (`____` and longer) are reserved; only `_`, `__`, and `___` are slot tokens.

| Shape                | Pattern                | Chain meaning                 |
|----------------------|------------------------|-------------------------------|
| Binary non-assoc     | `_ OP _`               | parse error on chain          |
| Binary right-assoc   | `_ OP __`              | `a OP b OP c` → `OP(a, OP(b, c))` |
| Binary left-assoc    | `__ OP _`              | `a OP b OP c` → `OP(OP(a, b), c)` |
| **Right-greedy binary** | `_ OP ___`          | `f $ a + b` → `OP(f, add(a, b))`; same-op chains right-fold |
| **Left-greedy binary**  | `___ OP _`          | terminates any preceding chain so the chain-so-far becomes the left operand |
| Prefix unary         | `OP __` *(or `OP _`)*  | `OP a` → `OP(a)`; `OP OP a` → `OP(OP(a))` (right-assoc) |
| **Prefix-greedy**    | `OP ___`               | `throw $ a + b` → `OP(add(a, b))`; operand admits a chain |
| Postfix unary        | `__ OP` *(or `_ OP`)*  | `a OP` → `OP(a)`; `a OP OP` → `OP(OP(a))` (left-fold)  |
| Ternary right-assoc  | `_ OP1 _ OP2 __`       | C-style: `a ? b : c ? d : e` → `OP(a, b, OP(c, d, e))` |
| Ternary mid-rec      | `_ OP1 __ OP2 _`       | `a ? b ? c : d : e` → `OP(a, OP(b, c, d), e)`          |
| Ternary non-assoc    | `_ OP1 _ OP2 _`        | parse error on chain          |

**Greedy slot semantics.** `___` subsumes `__`'s same-op chain behavior — a user could in principle use only `___` — but `__` stays in the grammar as the stricter shape, useful when the writer wants to *forbid* cross-op chains on the slot side. Dual-greedy patterns (`___ OP ___`) are rejected: without an asymmetric position, there is no associativity hint to drive parsing.

Higher-arity multi-token patterns (`_ OP1 _ OP2 _ OP3 _`, etc.) are accepted by the parser but follow the same rules: alternating slots and tokens, at most one `__` or `___` per pattern, and the recursive slot drives chain continuation when at one of the two end positions.

**Variadic operators.** `varop` declares delimited collection syntax. Its head
contains an opening symbol run and its mirrored closing run, separated by
whitespace:

```kio
varop [* *] { foldr cons nil; };
varop [! !] { foldl insert_entry empty_builder; };
```

OPEN is one maximal symbol run containing `[` and no `]`, and is not bare `[`.
CLOSE reverses that run and replaces each `[` with `]`, preserving every other
character. Thus `[*` pairs with `*]`, `*[` with `]*`, and `[[` with `]]`.
Both runs obey the symbol-run and reserved-spelling rules below. A head always
writes the two complete runs separately: `varop [* *]`, not `varop [**]`.

Each comma-separated element at a use site is one ordinary expression. The
ordinary comma-list rule applies, including leading, repeated and trailing
commas. For example, `[* a, b, c *]` contains three expressions. A fixed
operator can construct an element: with `op _ => _ { impl entry; };`, the
literal `[! k1 => v1, k2 => v2 !]` folds two values produced by ordinary `entry`
calls. The variadic operator has no separate key/value or multi-slot grammar.

The body contains exactly one primary clause and at most one
`finalize target;` clause, in either order:

| Primary clause | Initialization | Remaining steps | Empty literal |
|---|---|---|---|
| `foldl step base;` | `base()` | left to right: accumulator, then element | accepted |
| `foldr step base;` | `base()` | right to left: element, then accumulator | accepted |
| `foldl1 step seed;` | first element passed to unary `seed` | left to right, excluding that element | rejected |
| `foldr1 step seed;` | last element passed to unary `seed` | right to left, excluding that element | rejected |

The mode fixes direction independently of the delimiters. A nullary-base mode
calls its base exactly once, including for an empty literal, and calls its step
once per element. A nonempty mode calls its seed exactly once with the selected
element. A singleton therefore calls the seed and no step.

With elements `a`, `b`, and `c`, the nested calls are:

```text
foldl:  step(step(step(base(), a), b), c)
foldr:  step(a, step(b, step(c, base())))
foldl1: step(step(seed(a), b), c)
foldr1: step(a, step(b, seed(c)))
```

An optional unary finalizer receives the completed accumulator exactly once on
every admitted path, including the empty nullary-base path and the singleton
nonempty path. It may change the result type. Without a finalizer the literal's
result is the accumulator itself. Calls follow ordinary Kio typing and
application rules; the grammar introduces no special accumulator type, library
identity, or element-specific argument adaptation.

Every `step`, base/seed, and finalizer is a lexical value path: a bare local or
selectively imported value, a dotted path through an explicit module alias, or
an ordinary newtype member through a local/imported type or identity alias.
Calls, lambdas, and slash-qualified package FQNs are not callable fields.
Argument adaptation belongs in an ordinary named helper. Callable source order
and visibility are the same as for fixed operators.

Variadic delimiter runs are distinct from fixed-operator tokens, which contain
neither `[` nor `]`. Their nesting and comma-separated expression boundaries
are determined by the written source, without consulting operator declarations
or imports. A literal still requires its `varop` binding to be in scope for
resolution and lowering.

{:#operator-grammar}
**Complete operator grammar.** The public identity of an operator selection is
its complete tagged grammar. Fixed forms include every literal run, every slot
kind, all later slots, and significant lenient groups. Variadic forms contain
the same OPEN and CLOSE in declarations and imports; the import omits only
the callable body. For example:

| Declaration | Import selection |
|---|---|
| `op _ + __ { impl add; };` | `op _ + __` |
| `op _ ? _ : ___ { impl cond; };` | `op _ ? _ : ___` |
| `op _ ( <\| _ \|> ) { impl index; };` | `op _ ( <\| _ \|> )` |
| `varop [* *] { foldr cons nil; };` | `varop [* *]` |
| `varop [! !] { foldl insert empty; };` | `varop [! !]` |

Canonical rendering retains distinctions between complete grammars, including
lenient groups, slot kinds, maximal-run boundaries, and required contextual
operator-token quotation in fixed patterns. Variadic heads consume exactly two
unquoted maximal runs separated by whitespace; the comma separator is implicit
in the `varop` construct and is not written in its head.

**Dispatch and conflicts.** Parsing dispatches on the operator's expression-start
position and leading run-sequence; variadic bindings dispatch on OPEN. This
shorter key is deliberately distinct from the complete public grammar. It
prevents two incompatible parses in one scope: two bindings with the same
position and leading run conflict even if their tails or slot kinds differ.
Two variadic bindings with the same OPEN also conflict; CLOSE is fixed by its
mirror. Fixed and variadic keys cannot collide because fixed tokens exclude
square brackets. The prefix-forbidden rule below additionally rejects a strict
fixed-op-token prefix. Public imports, diagnostics, and
queries retain the complete grammar rather than displaying a shortened key.

**Module-local by default; `pub` exports the binding.** Importing an ordinary
function does not import an operator bound to it. An unmarked fixed or variadic
operator is local; `pub` and `pub(path)` export it under the ordinary visibility
rules. Every directly named implementation, step, base/seed, and finalizer must
have visibility at least as wide as the operator. For a newtype-member path,
both the type and selected member meet that floor. An identity-alias-qualified
member is the terminal member for this rule: the written alias and terminal
member each cover the referring operator's visibility.

{:#operator-pattern-imports}
**Operator imports.** An explicitly tagged selection imports an exported
operator alongside ordinary names and label selectors:

```kio
import syntax(Mytype, my_function, op _ + __, varop [* *]);
```

The consumer restates the complete grammar. Its parser installs the written
pattern without loading any provider declaration, then parses bodies using
those imports and preceding local declarations. Consequently `kio fmt`, the
REPL parser, and syntax-only language-server requests can parse operator-bearing
text without a provider file, package, cache, or declaration catalogue. A
standalone `op` or `varop` selection is an ordinary value name; the following
pattern or delimiter pair identifies an operator tag. A `varop` import has no
internal comma, so a following comma belongs to the surrounding selection list.

Semantic resolution then checks the exact explicitly named provider's exported
record and compares the whole grammar. An unavailable export or a mismatching
slot, token, group, or delimiter pair is an import error; provider data never
changes the already-written consumer grammar. The selected declaration supplies
the callable paths, fold mode, and finalizer for lowering. Changing those fields
does not change the grammar a consumer must write.

The implementation function need not be co-imported. The operator import itself
is the explicit dependency; lowering records the exact resolved implementation
and the ordinary qualified imports needed by the resulting calls. A provider's
callable field reaches another module only through an ordinary selective import
or explicit module alias, never by a slash-qualified implementation field.
Visibility is checked on every directly named callable as described above.

**Conflict rule across import layers.** Each explicit operator import may be
written once, even when repetitions name the same provider and declaration.
Imports also conflict with other imports or local fixed/variadic declarations
that occupy the same dispatch key. These are Name errors. There is no
same-provider exception. Validation consumes the written import environment
and module-cycle graph; this prerequisite does not establish diagnostic
precedence when independent errors coexist. Distinct dispatch keys still obey
the structural prefix-forbidden rule.

**Open-world.** A consumer's parse depends only on its complete written imports
and preceding local declarations. Resolution compares each selection with the
one explicitly named provider and grammar; it does not search newly added
exports for omitted pattern details or callable candidates. Adding an unrelated
provider declaration therefore cannot change the consumer's syntax, chosen
operator, or callable resolution. A duplicate provider key is a same-module
conflict, not a silent rebind. Lowering makes the resolved ordinary dependencies
explicit, and downstream phases perform no fresh provider search.

**No precedence; only associativity.** Different operators in one expression require explicit parentheses — `2 + 3 * 4` is a parse error. The user writes `(2 + 3) * 4` or `2 + (3 * 4)`. This is deliberate: precedence tables grow into a long-tail of opinion, and parens are cheap. The rule mirrors the type-chain mixing rule (`(A & B) | C`) — operator nesting requires the same explicit grouping that nested-type chains do.

**Nesting summary.** The full rule, in one place:

| Composition                              | Without parens | Required form           |
|------------------------------------------|----------------|-------------------------|
| Same operator, associative                | OK             | per the operator's assoc |
| Same operator, non-associative            | parse error    | `(a OP b) OP c` etc.    |
| Different binary operators                | parse error    | `(a + b) * c`           |
| Different operators inside a recursive ternary slot | parse error    | `a ? b : (c + d)`       |
| Postfix unary then binary                 | parse error    | `(a OP1) OP2 b`         |
| **Prefix unary then binary**              | **OK**         | `- a + b` is `add(neg(a), b)` |
| Same prefix unary, chained                | OK             | `! ! a` is `not(not(a))` (right-assoc) |
| Same postfix unary, chained               | OK             | `a ? ?` is `?(?(a))` (left-fold) |

Prefix unary is the lone composition that doesn't require parens — it binds tightly to its operand and the result becomes the LHS of any subsequent binary/ternary. Every other cross-operator composition requires explicit grouping, the same way type-chain mixing does.

**Operator-character set.** An `OpToken` is a maximal contiguous run drawn from the **allow-list** ``+ - * / % ^ ~ ? @ # $ \ ' ` < > = ! & | : . [ ]`` (the apostrophe `'` and the backtick `` ` `` have no other role in the language). The lexer produces every such run as a single `SymbolRun` — the one unified symbol-token kind — so there is no standalone-vs-fused distinction at the token-kind level: `[!`, `]]`, `]-`, and `][*` are single runs just as `<=`, `&&`, or `->` are. Whitespace is load-bearing when two runs are intended: `[ !`, `] ]`, `] -`, and `] [ *` are distinct run sequences. `&` and `|` join runs like any other op-char; the type-product and type-sum chain parsers recognize a `SymbolRun` whose content is all `&` (resp. all `|`) as a chain separator. `.` joins greedy runs alongside the rest, so `.>`, `.>>`, `.<`, and `.<<` lex as ordinary fused `SymbolRun`s. Dotted runs are admissible as user operator tokens when they do not start with `.` (`+.` / `<.>`) or, if they do start with `.`, when they contain at least two dots (`..` / `.+.`). Leading-dot runs with exactly one dot (`.`, `.>`, `.+`, `.###$`) are reserved for dot-led syntax. The lexer is **uniformly greedy** over op-chars — `->`, `.>`, `<-`, and all other multi-char sequences flow through the same fusion rule, with no per-arrow carve-out.

At grammar positions that structurally require a delimiter, the parser may peel that leading delimiter from a fused run: this covers function arrows, existential closers, bang-call suffixes, unit-type dots, type-chain separators, and the `[` / `]` around forall binders. Contextual bracket peeling preserves compact spellings such as `[*F]`, `[A][B]`, `[A][*F]`, and `.[*F]`. The peel is opt-in per structural call site — user-`op` patterns, variadic patterns, operator expressions, and operator imports read each `SymbolRun` whole. There is no bracket-only compatibility decomposition in operator grammar. Whitespace, identifiers, digits, and structural punctuators (`, ; ( ) { }`) all break the run. **The op-char allow-list is ASCII-only.** Unicode math operators, arrows, set-theoretic symbols, and composition dots are not op-chars: an `op ∘ _ { impl compose; };` is a lex error. Identifiers are likewise ASCII-only under the standard `IDENT` grammar; operator tokens have this separate character allow-list. The homoglyph hazard (`×` vs `x`, `−` vs `-`, `∘` vs `o`), the synonym-pair maintenance cost (every Unicode op-char would need an ASCII fallback for users without IMEs / copy-paste), and the audience fit (Kio targets host-language developers embedding a small language, not Agda / Lean / APL users with Unicode-friendly editor setups) all point at ASCII-only as the right floor for operator tokens.

A small **block-list** governs sequences an operator run will not absorb:

- `//` — opens a line comment (handled by trivia, before operator lexing).
- Any run carrying two or more `/` characters — reserved (a run includes at most one `/`).

`#` is an ordinary operator character and joins greedy runs (`##`, `#-`, …). Placeholder lambdas use ordinary identifier stems and numbered references, as specified in [Placeholder lambdas](#placeholder-lambdas).

**Reserved operator-token spellings.**

- Fixed-operator tokens contain neither `[` nor
  `]`, including in quoted operator patterns. Square brackets retain their
  structural type roles and form the delimiter runs of `varop` literals.
- `=` has fixed roles in `let`, `type`, `literal`, and `op` bodies. An unquoted standalone `=` in op pattern position is rejected before any operator-token can be recorded; the quoted form `op _ (=) _ { impl my_eq; };` (operator-token quotation) admits `=` as an op-token.
- Any op-token that **starts with `.`** must contain at least two dots. This reserves dot-led one-dot runs **unquoted and quoted both**: `.`, `.>`, `.+`, `.###$`, and the corresponding quoted forms `(.)`, `(.>)`, `(.+)`, `(.###$)`. The reservation holds in any operator component, not only when the component is the sole token of the pattern. Valid dotted examples include `..`, `.+.`, `+.` and `<.>`.

`:` was previously reserved the same way, but is now admissible directly: `op _ : _ { impl annot; };` parses, because the parser disambiguates `:` between binding-position contexts (fn parameters, `newtype` headers — type annotation) and expression position (operator) via the surrounding parser state. `->` is similarly admissible directly — its structural role (function-type arrow / fn return-type marker) is pinned by surrounding-grammar context. Multi-token patterns containing `=`, `:`, `->`, or admissible dotted runs (e.g. `op _ ? _ : _ { impl cond; };`, `op _ .. _ { impl range; };`, `op _ .+. _ { impl compose; };`, `op _ +. _ { impl trailing_dot; };`, `op _ <-> _ { impl bidir; };`) parse without ceremony.

The stricter reservation on dot-led one-dot runs (no quotation escape) is deliberate. `=` is admissible via `(=)` because `=` never appears in expression position structurally — any `=` in expression position is, by definition, a user-op call. Dot-led syntax appears in expression position **as** structural roles — member access, UFCS dispatch, `.(...)` lambdas, and `.stem.` placeholder lambdas — so the one-dot family stays held back for those forms and future dot-led extensions.

**Reserved `//…` operator family.** Independently of the dot-led rule above, an operator component whose spelling **starts with** `//` is reserved for future syntax: an `op` whose maximal run of adjacent op-tokens begins with `//` is a parse error, holding the whole `//…` family (`//!`, `//?`, `//=`, …) back. Because the lexer caps a single `SymbolRun` at one `/` (a second `/` opens a line comment — see [`grammar.md`](grammar.md#lexical-structure-kio-surface)), a `//` prefix is only reachable as two adjacent `/` op-tokens (`op _ / / _ { impl …; };`); the parser joins the adjacent run and rejects it. This is a `starts_with("//")` **prefix** reservation — it reserves the entire family, not a single spelling. The reservation keeps the surface coherent with the line-comment marker `//`, so the `//…` family can later take on operator meaning without colliding with existing user operators.

**Operator-token quotation `(tok)`.** The binding separator `=` can still be used as an `op` operator-token by wrapping it in parens: `op _ (=) _ { impl my_eq; };` declares a binary `=` operator that binds to `my_eq`. The `(...)` is pattern syntax, retained in declarations and complete imports — it tells the parser to read the inner token as a literal operator-token rather than its structural role. At use sites, the operator is invoked without the parens: `a = b` desugars to `my_eq(a, b)`. The quotation form is non-conflicting with the [lenient-grouping marker](#operators) `( … )` (which wraps slots, not lone tokens); the parser distinguishes by content. Quotation does **not** bypass the leading-dot one-dot reservation: `(.)`, `(.>)`, `(.+)`, and `(.###$)` remain reserved, while `(..)` and `(.+.)` are admitted for the same reason as their unquoted forms. Open-world is preserved by the same keyspace argument as in [Open-world](#operators) above — the unquoted token contributes to the (shape, op-tokens, prefix-vs-non-prefix) tuple identically to any other op-token, so an added `pub op _ (=) _ { impl ...; };` in a provider occupies a fresh keyspace slot and cannot perturb an existing import.

**Pattern tokens** are bracket-free `OpToken` runs. Quotation cannot admit `[` or `]` into a fixed pattern. A symbol used as a pattern token keeps its structural meaning elsewhere. Adjacent operator tokens are permitted when whitespace keeps their runs separate: `op _ && ++ _ { impl combine; };` has two runs, while `op _ &&++ _ { impl combine; };` has one. Only adjacent slots are rejected. Matched-pair patterns such as `_ <| _ |>` use the same rules; their separate runs remain separate at use sites. The keyspace is the sequence of runs, not a concatenated string, so an imported complete pattern continues to identify the same target when a provider adds unrelated patterns.

A recursive last slot can be followed by fixed closing runs while retaining
same-operator chaining. For example, `_ <| __ |>` admits
`a <| b <| c |> |>`; `a<|b<|c|>|>` instead ends in the single maximal run
`|>|>` and does not supply two closes. When the next operator would select a
different binding, the complete closing-run sequence belonging to the enclosing
pattern terminates its recursive operand. Thus a separate `_ |> _` in scope
does not consume the closing `|>` of `_ <| __ |>` in `a <| b |>`.
The cross-operator grouping rules apply within recursive operands; closing
runs do not establish operator precedence.

**Explicit `( … )` lenient-grouping marker.** An `op` pattern may wrap a contiguous sub-pattern (slots and tokens, any mix) in `( … )` to mark the slots inside as **lenient**: at use sites, a lenient slot admits a full single-op chain instead of the default atom-or-paren operand. `op _ ( <| _ |> ) { impl idx; };` declares an indexing operator whose inner slot admits `a <| b + c + d |>` directly; the strict-atom default `op _ <| _ |> { impl idx; };` would require explicit parens around the inner chain. The `( … )` themselves are not typed at use sites — they are pattern syntax and remain present in complete imports. Cross-operator mixing inside the lenient slot still requires explicit parens (no precedence). Groups don't nest, must contain at least one part, and the greedy slot `___` is rejected inside `( … )` (already full-chain by definition).

**Conflict rule — prefix-forbidden op-token sequences.** Subject to dispatch-key uniqueness, two `op`s in one module may share leading operator-token sequences *as long as neither's op-token sequence is a strict prefix of the other's*. Prefix-shaped patterns (`OP _`, `OP __`) live in a separate keyspace from non-prefix patterns, so the same op-token spelling can be both prefix and non-prefix simultaneously (e.g. unary `op - __ { impl neg; };` plus binary `op _ - __ { impl sub; };` coexist). Within either keyspace:

- ✓ `_ && ++ _` and `_ && -- _` — share `&&`, branch at `++` vs `--`. No prefix relation. Coexist.
- ✓ `_ && _` and `_ &&-- _` — `&&` (one op-token; the lexer fuses adjacent op-chars) and `&&--` (one op-token) are disjoint at the token level; neither is a prefix of the other. Coexist.
- ✗ `_ && _` and `_ && ++ _` — whitespace separates `&&` from `++`, so the second pattern's sequence is `[OpToken("&&"), OpToken("++")]` — `&&` is a strict prefix. Conflict; the second declaration is a name-conflict error. (Re-spelling the second as `_ &&++ _` (no whitespace) makes them disjoint single tokens and resolves the conflict.)
- ✗ `_ + _` and `_ + _ ? _ : _` — `+` is a strict prefix of `+ ? :`. Conflict.

The implementation stores operators in a trie keyed by op-token sequence; inserting an op whose pattern's sequence lands on an existing entry, or whose sequence is a strict prefix of an existing entry, is a name-conflict error. At parse time the parser walks the trie forward token-by-token; bounded lookahead equals the maximum declared pattern depth.

**Whitespace as the disambiguator.** Operator characters fuse into a run only when adjacent (no whitespace), so `&&` and `&&--` each lex as one `SymbolRun` and the trie keys are disjoint, while `& &` (whitespace between) is two one-character runs. There is no dedicated-token-kind distinction for any symbol: a standalone `&`, `|`, `!`, `=`, `:`, `<`, `>`, `.`, `[`, or `]` is just a one-character `SymbolRun`, and its structural role (chain separator / bang-call suffix / binding separator / type annotation / existential binder / member access / forall delimiter) is recognized from content and parser position; the same character fused into a longer run is part of that run. The "leading/trailing/repeated `&`s collapse" rule from [`grammar.md`](grammar.md) — `A &&& B` and `A &&&&&& B` parsing the same as `A & B` — is implemented by the chain parser recognizing any `SymbolRun` whose content is all `&` characters as a product separator (and the symmetric rule for `|`).

**Lowering.** The operator-fold pass replaces fixed-operator applications and variadic literals with ordinary calls using the consumer's explicit operator scope. Fixed and variadic declaration records and tagged import selections are consumed before Kio'. Kio' shares the maximal symbol-run lexer but has no `op` or `varop` declaration, operator import, or user-operator expression production.

**Out of scope.** Bare-operator-as-value (`map(+, xs)`) is not supported.

### Equivalence claims (`equiv`)

*Surface only; desugars to Kio'.* `equiv` items are filtered before codegen — they contribute nothing to the build artifact. The Kio' grammar has no `equiv` production; `kio-prime` rejects `equiv` items at parse time.

`equiv` declares a **test**: a claim that several expressions reduce to the same normal form. The `kio test` tool discharges these claims by partial evaluation; outside that tool, `equiv` items are silently filtered before codegen and contribute nothing to the build artifact.

```kio
fn id_unit() -> . { () }

equiv id_unit_eq {
  id_unit();
  ()
}
```

Surface form:

```
equiv <name>[A](x: A) {
  [expr];
  [expr];
  ...
  [expr]
}
```

- **Name is mandatory.** `equiv { ... }` without a name is a parse error. Names are scoped to the module like `fn` names but never resolve as values — they exist only for diagnostic identification.
- **Parameter list reuses `fn`'s grammar.** Type and value parameters mixed positionally; every value parameter annotated. The list is optional (no parens parses a zero-param `equiv`). Parameters bind in the body's scope; each arm sees the same binding set. Parametric equivs run: type parameters are erased at evaluation time; each value parameter is bound, once per equiv block, to a fresh opaque atom shared across every arm, so equal references on different sides compare equal. The arms are claimed equal *for every instantiation* of the parameters — the runner discharges the universal quantification by reasoning about the bound names symbolically.
- **At least two arms.** `N=0` and `N=1` blocks are parse errors — there is nothing to compare.
- **No visibility.** `pub equiv …` is a parse error. Equivs are not part of any public API surface.
- **Arm syntax.** Arms are semicolon-separated expressions. Repeated semicolons are accepted and ignored; a final semicolon after the final arm is accepted and ignored. Comma-separated arms are rejected with a migration diagnostic. An arm that needs local statements uses the imported `scope! { ... }` elaborator.

**Typing.** Each arm is typechecked in the parameter scope; all arms must synthesize the same type under ordinary definitional equality. A complete polymorphic function scheme is a value type here just as it is in every other value position; incomplete schemes remain ill-kinded as values. A type mismatch is a regular type error, not a test failure.

**Build artifact.** `equiv` is surface-only: the compiler's substitution pass at the `Lowered → Prime` boundary filters `Item::Equiv` out, so codegen never sees these items and the emitted artifact contains nothing for them. `kio-prime` (the Kio'-only compiler) rejects `equiv` at parse — it is not part of the Kio' grammar.

**Discharge** is the responsibility of the `kio test` tool: see [`specs/cli.md`](cli.md) for the subcommand and [`specs/exit-codes.md`](exit-codes.md) for the test-failure exit code. The tool's reduction strategy and equivalence relation (α + η on residual normal forms, with host items treated as opaque atoms) are documented formally in [`specs/formal/equiv.md`](formal/equiv.md). Host calls are compared by residual structure; an extra unit-returning host call in an expression statement is not erased just because its result is unused. Reduction strategy doesn't matter — Kio' is strongly normalizing and confluent, so every sensible strategy reaches the same residual normal form.

### Literals

Four literal **shapes** are recognized — string, integer, float, and bool. String, integer, and float are lexer tokens; bool is the parser-recognized token sequence `.t` or `.f`. A literal carries no width suffix; its type is a `host type` carrying a `role(...)` marker (see [Host declarations and the bridge block](#host-declarations-and-the-bridge-block)), resolved by the three-tier rule below.

- **String** — double-quoted, with JSON-style escapes interpreted at compile time: `\"`, `\\`, `\/`, `\b`, `\f`, `\n`, `\r`, `\t`, and `\uXXXX` for code points in the Basic Multilingual Plane.

    ```kio
    let s = "hello, world\n";   // assuming `type String role(str);` — s : String
    ```

    **Adjacent literal concatenation.** Two or more string literal tokens with only trivia (whitespace, newlines, comments) between them fold at parse time into a single string literal whose value is the concatenation. The chunking present in the source is informational only — the AST stores the joined value, and `kio fmt` re-chunks deterministically on emission.

    ```kio
    let greeting = "hello, "  "world"  "\n";          // == "hello, world\n"
    ```

    Pure parser-side rule: no operator is added, and the rule does not extend to any other literal shape (integers, floats, booleans don't fold).

- **Integer** — decimal digits, optionally grouped with `_` as a digit separator (`1_000_000`). A leading `-` adjacent to a digit is consumed by the lexer as the literal's sign when the preceding token does not end an expression (BOF, after `(`, `,`, `=`, an op-token, etc.); after an expression-ending token (identifier, literal, `)`, or `}`), `-` is a separate op-token and `a-1` / `1-2` parse as the subtraction shape. A `]` is an ordinary op-token, not a lexer-level expression closer. See [`grammar.md`](grammar.md) § Notes (Kio surface) for the precise rule. There is no hex/binary/octal syntax. Magnitude is unbounded at the source level; representability in a given host language is the host's concern.

- **Float** — decimal digits with a `.`-separated fractional part and/or an `e`/`E` exponent with an optional sign (`3.14`, `1.0e-9`, `2e10`). Digit separators are allowed in both mantissa and exponent. An integer-shaped run with neither a fractional part nor an exponent is an integer literal, not a float. The leading-`-` sign rule above applies to float literals too (`-3.14`, `-1.0e-9` at expression-starting positions).

- **Bool** — `.t` or `.f`. These spellings are recognized only in expression-leading literal position as `.` followed immediately by identifier `t` or `f`; the words `true` and `false` are ordinary identifiers.

**The literal-call annotation form.** Any literal may carry a trailing parenthesized type — `42(I32)`, `3.14(F64)`, `"hi"(String)`, `.t(Bool)` — naming the host type the literal should take. This is the **`LiteralCall`** form (see [`grammar.md`](grammar.md)). The annotation is optional in the full surface language and **mandatory in Kio'** — Kio' admits no unannotated-literal form, so every Kio' literal carries its annotation by parse rule (see [`grammar.md` § Kio' grammar](grammar.md#kio-grammar) and [`prime.md` § Host declarations](prime.md#host-declarations)). Full-Kio checking records the three-tier resolution on the Lowered tree, and the Lowered → Prime substitute pass materializes it on the literal node.

**The admission relation.** A literal of a given shape may be typed as a role only when the role's shape *admits* the literal's:

- An **integer** literal admits integer-shaped roles (`i8`, `i16`, `i32`, `i64`, `i128`, `u8`, `u16`, `u32`, `u64`, `u128`) **and** float-shaped roles (`f32`, `f64`) — an integer-shaped run with no fractional part is a valid float value.
- A **float** literal admits float-shaped roles only.
- A **string** literal admits the string-shaped role (`str`) only.
- A **bool** literal admits the boolean-shaped role (`bool`) only.

**Three-tier resolution (surface only).** Tiers 2 and 3 below apply only to the full Kio surface — Kio' admits no unannotated-literal form (see [`grammar.md` § Kio' grammar](grammar.md#kio-grammar)), so a Kio' literal's host type is always tier 1. A surface literal's host type is resolved in order:

1. **Explicit annotation.** If the literal carries a `(Type)` annotation, that host type is its type. The annotation must name a role-bearing `host type` whose role the literal's shape admits. This is the only tier Kio' admits.
2. **Expected type.** Otherwise, if the surrounding position supplies an expected type — a function-return type, a call argument checked against a parameter type, an elaborator target, or a fully concrete typed `let` binder — and that expected type is a role-bearing `host type` whose role the literal's shape admits, the literal takes it. An untyped or partially typed `let` right-hand side remains a synthesis position and supplies no expected type; only the fully concrete binding pattern creates the checked RHS position (see [Bindings and expressions](#bindings-and-expressions)).
3. **Unique in-scope candidate.** Otherwise, if exactly one role-bearing `host type` whose role the literal's shape admits is in lexical scope, the literal takes it.
4. **Otherwise, a type error** — either no admitting host type exists, or more than one does and none of tiers 1–2 disambiguated. Annotate the literal with the intended host type to resolve it.

**Role-inheriting aliases.** A `type` alias whose body resolves — directly or through a chain of aliases — to a role-bearing `host type` **inherits** that host type's role, since the alias names the same type. Such an alias is accepted everywhere a role-bearing host type is in tiers 1 and 2: `42(I32)` typechecks when `I32` is `type I32 = provider.I32`, and a literal in a position whose expected type is that alias takes it. Tier 3, however, stays keyed to host types: an alias and the host type it names are the same type, so admitting both into the bare-literal candidate pool would spuriously make an unannotated literal ambiguous. A role-inheriting alias serves as a tier-3 candidate only when **no** `host type` in scope carries the literal's role — the case of a module whose host type was replaced by such an alias (the local form a dependency `rehost` materializes; see [`package.md` § Dependency files](package.md#dependency-files)).

Role candidates are grouped by the terminal host declaration's identity, `(declaring module, host-type name)`, not by the spelling used in the consumer. The selected pool is the nonempty set of host types in unqualified lexical scope, or otherwise the fallback set reached through aliases in that scope. Two aliases or repeated imports that reach one declaration contribute one candidate. Declarations in different modules remain different candidates even when they have the same leaf name. A lexical lookup that requires one exact role, including the `__if_then_else__` scheme and the reference `if!` elaborator's branching glue for `role(bool)`, reports a type error when this selected identity-keyed set is empty or has more than one member; declaration order and map iteration never choose a winner.

Tier 3 is keyed to the role-bearing `host type`s **in unqualified lexical scope** at the literal's site: those declared in the same module plus those brought in by a selective `import module(Name);` clause (plus, as a fallback when no host type carries the role, a role-inheriting alias in that same scope). `import module as qualifier;` makes qualified member references possible but does not add those members to the pool. An in-scope alias may use such a qualifier in its body; in that case the alias contributes the terminal host identity to the fallback pool, while the qualified member does not contribute independently. A `host type` is an ordinary scoped declaration, so importing one selectively adds it to the tier-3 pool, and a module that imports no role-bearing host type for a given shape resolves no tier-3 candidate for that shape. See [Open-world design](#open-world-design) for the contract-surface coupling this introduces.

The full-Kio typer records each tier-2 or tier-3 resolution while checking the Lowered tree. The Lowered → Prime substitute pass writes that resolution into the literal's `annotation` field, so every resulting Kio' literal carries its tier-1 annotation.

The lexer emits the same token shape regardless of whether an admitting host type exists — three-tier resolution at type-check time is what rejects an unresolvable literal. Literals are not calls: `42` is a single token, and `42(I32)` is a literal with an annotation, not a function invocation.

**Source-syntax vs emitted-syntax.** The lexer productions above are Kio's surface syntax. The compiler is free to normalize each literal into whatever form its chosen host language accepts — for example, emitting an `i32`-roled literal into JavaScript as a numeric literal and an `i64`-roled one as a BigInt, or into another host language as a call to a host-provided constructor. The source shape is fixed; the emitted shape is a backend concern.
