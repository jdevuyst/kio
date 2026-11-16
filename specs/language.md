# Kio language reference

This document is the source of truth for Kio's **syntax and semantics**. It describes only what is settled. The canonical grammar productions live in [`grammar.md`](grammar.md) — this document narrates *what each form means and how it desugars*; that one pins *what parses*.

Kio is layered. The small subset **Kio'** — described in [`prime.md`](prime.md) — carries the formal semantics and is the target for self-hosting; this document describes the full surface, which adds sugar and macro calls on top of Kio'. Macro calls are resolved through explicit imports. Compile-time reflection/helpers are imported only through the compiler-provided `import __comptime__;` block. Every feature here is either already in Kio' or expands into Kio' as noted inline.

## Overview

Kio is an ultra-portable language. Kio' deliberately contains nothing host-specific, so a program can be transpiled to any backend for which a host is provided.

- Built on **System F** (polymorphic lambda calculus with explicit universal quantification): functions, universal types (`forall`), type application. Binder kinds are first-order chains (`κ ::= * | * → κ`): every arrow domain is `*`, so a binder may range over a constructor of ordinary types, not over a higher-order type-level function. Nominal declarations separately list each parameter's kind; a dictionary such as `Monad[*F]` can accept a constructor argument. There is no type-level lambda and there are no kind variables — a Church-style fragment of F-ω rather than full F-ω (rank-N permitted; quantifiers nest freely under arrows because every binder is explicitly written by the user; see [Higher-kinded types](#higher-kinded-types)). Anonymous type expressions cannot be self-referential. Recursive types exist, but the recursion is always grounded inside a nominal payload (see [Type declarations](#type-declarations)), giving an iso-recursive wrap/unwrap at the nominal boundary.
- Supports **anonymous sum**, **anonymous product**, and **nominal** types.
- **Host-provided runtime capabilities.** Ordinary runtime code receives numeric, Boolean, string and I/O capabilities from host declarations; Kio has no ambient runtime or special runtime `String`. The core provides structural constructors (`->`, `forall`, `&`, `|`), Unit `.` and bottom `!`. Modules introduce or name types with `type` and `host type`; the compiler-provided `__comptime__` vocabulary is a separate compile-time interface. Literal tokens in ordinary runtime source — integers (`42`), floats (`3.14`), booleans, and strings — are typed against the `role(...)`-marked host types in scope (see [Host declarations and the bridge block](#host-declarations-and-the-bridge-block)).
- **No implicit recursive definitions.** Ordinary `fn` bindings may not refer to themselves, nor may two ordinary bindings mutually refer to each other. Surface Kio has one explicit term-level escape hatch: `rec(loop)` groups, which lower annotated recursive calls to an ordinary host-supplied `loop` function and do not introduce a Kio' fixpoint. Recursive data scope is separately explicit and capability-free: `rec type` for a compact declaration and bare `rec { ... }` for one genuinely mutual type component. Every cyclic type path crosses a nominal boundary; there is no alias-only fixed point. This keeps Kio' strongly normalizing while still letting hosted packages opt into general iteration through an explicit capability.
- Surface syntax aims to feel close to **modern curly-brace languages**.
- **Trait-like machinery uses nominal dictionaries.** A bundle of methods is an ordinary product of nominal values carrying functions. Callers pass and project the dictionary explicitly. Imported macros provide opt-in structural transformations through ordinary resolved calls; they are not ambient coercions.
- **Open-world design.** Adding definitions to a module body never causes another module to fail to compile. (Scoped to module bodies — a package file's contract surface is governed by versioning, not open-world; see [Open-world design](#open-world-design).)

## Type system

Kio's type discipline is **bidirectional**: every type a program needs is anchored somewhere in the source — at a binder, in a top-level signature, as an explicit type argument at a call site, or as the expected type of the position the expression sits in. There is no Hindley-Milner-style global inference. Calls expose their stages in the source: `f[A](a)[B](b)` supplies type arguments in square brackets and one value argument in each parenthesized group. A value group may use comma-separated components of a product. A call site may write each type argument out explicitly, mark a slot with the `_` placeholder, or fully elide the type-argument list. Elided and `_`-marked slots are inferred by a finite application-local relation over value arguments, connected nested applications, and (when the call sits in a checked position) the expected type at that position — see [Bidirectional elaboration](#bidirectional-elaboration) below for the formal account.

What this rules out: no inference variable escapes its finite connected
application or lexical owner, no let-generalization, and no global constraint
propagation. What it allows: an application can relate its resolved callee type,
surrounding expected result, immediate value arguments, and connected nested
application results using first-order equality constraints. An independently
completed nested application contributes only its closed result. When a child
still needs information from that finite relation, its unfinished typing edge
is retained until the relevant parameter slot becomes usable. Source typing
derivations and macro actions are never replayed; no backtracking or
global worklist is involved. Every local unknown and retained edge is gone
before the program reaches Kio'.

This most visibly lets an enclosing call provide a function type to a lambda,
or a result type to a nested polymorphic call, after another sibling has pinned
a shared type argument. A direct lambda callee can also fill whole missing
parameters from that same node's value arguments. The source-to-target
macro calls are the only direct user-written positions whose primary job
is source-to-target glue; `match` may synthesize glue internally as part of
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
retained child to close or one side of the equation to change structurally
after applying the currently known substitutions; another deferral replaces
that progress snapshot. Advancing to a
different schedule mode is not progress and never retries the equation. These
bounded deferrals do not add a third typing mode: source premises are entered
once, completed subexpressions are not rechecked, and macro actions are
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
- Type-argument slots in singleton square brackets at call sites (positional, optionally `_`-marked or fully elided).
- `host type` / parametric `type` alias / nominal entry declaration headers.
- The body of a `type` declaration.
- Scalar literal and literal-alias annotations, such as `42[I32]`.

The user has to seed the expected type from one of these positions; everywhere else, the elaborator either synthesizes bottom-up or pulls the expected type from the immediately-enclosing context.

**Untyped `let` takes the RHS's type as-is.** A `let x = e;` types `e` in synthesizing mode with no expected type and binds `x` to whatever type `e` synthesized — polytype or monotype, taken unchanged. No narrowing, no generalization, no defaulting. Every type-binder in Kio is user-written (universal `[A]` at signatures and `fn` headers, existential `<U>` at nominal payloads), so any polytype the elaborator synthesizes traces back to a binder the user wrote in the source. Unlike Hindley-Milner let-generalization, this rule introduces no new universal binders. The principal example: `let f = id;` where `id` is an in-scope top-level polymorphic `fn`, e.g. `fn id[T](x: T) -> T { x }`. `id` synthesizes its declared polytype `[T] T -> T`; `f` inherits it; the local `f` is usable at every instantiation `id` accepts.

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

**Call stages.** A square-bracket group explicitly instantiates the current
universal binders; a parenthesized value group applies exactly one `Function`
layer. For `f : [A] A -> [B] B -> R`, write `f[A](a)[B](b)`.
For `g : (A & B) -> R`, `g(a, b)` and `g((a, b))` supply the same
right-associated product. In contrast, `h : A -> B -> R` is applied as
`h(a)(b)`. One written group never crosses into the next function layer.
A generic domain can itself be a product: `id(a, b)` instantiates identity at
`A & B`; it does not turn identity into a two-stage function.

Explicit type arguments are positional within the currently exposed binder
run. Each bracket contains exactly one type: write `f[A][B]`, never
`f[A, B]`. This singleton rule also applies to declaration and prefix-forall
binders, type-constructor arguments and literal annotations. Empty brackets
and comma runs inside a bracket are rejected. `f[A]` supplies a type and retains the residual value; it does not supply
Unit. `f[_](a)` leaves that type slot for inference, and `f(a)` omits the type
group entirely. Omission retains the same finite local inference rule across
connected written stages; an intermediate group is not an independent
inference boundary. A `let` or semicolon still ends that connection.
A value group with no written components, `f()`, supplies Unit, just as
`f(())` does. Empty type brackets `f[]` are rejected. The Unit type is `.`,
so `f[.]` is type application, not a Unit value call.

The same type-supply convention applies to types and literals:
`Map[K][V]`, `Monad[List]`, `F[A]`, `123[U32]`, `.t[Bool]`, and
`"abc"[String]`. A literal alias can select its carrier as `count[I32]`.
These forms retain distinct meanings: a constructor argument is kind-checked,
a value type argument instantiates a universal, and a literal annotation
selects its role-bearing carrier. Brackets do not add a coercion or make a
literal alias polymorphic. Each literal annotation has exactly one carrier.
Type-path arguments are collected before existing arity and kind checks;
adjacent groups do not make an undersaturated transparent alias valid.
`Foo.mk[A](x)` specializes a member value; `Foo[A].mk(x)` is not a new
member-selection spelling.

Postfix brackets belong to the preceding expression regardless of whitespace:
`f[A]` and `f [A]` have identical structure. At expression start, square
brackets retain their variadic-operator meaning. A value frame is passed as
`f([a, b])`, not `f[a, b]`; the latter occupies a type-supply position and rejects both value entries
and comma lists without retrying another interpretation. Types inside brackets
use the full type grammar, including `f[[A] A -> A](identity)`.

**Named callables share a uniform user-facing contract.** Top-level `fn`s, intrinsics (`__left__`, `__right__`, …, `__absurd__`, `__if_then_else__`), and macro names (such as `iso`, `fit`, `match`, `derive`, or a package-defined `checked` after the corresponding import) are all looked up by name and expose a declared polytype. Macro names use direct local call syntax and run compile-time implementation code over reflected arguments; from the caller's perspective, explicit type arguments, `_` placeholders, value arguments, and surrounding expected types are solved against the macro's declared call type by the ordinary call-type rules. `__if_then_else__`, `__and__` and `__or__` have a checked Boolean-specialization prefix: their first type argument must resolve to a concrete `role(bool)` host type before the intrinsic becomes an ordinary value. The conditional's result-type argument then follows the ordinary call rules; see [Conditionals](#conditionals).

A macro implementation produces checked Kio' terms through its
compile-time interface rather than making a runtime call. Those terms replace
the macro call at the Lowered → Prime boundary.

**Surface forms reduce to standard call shapes.** UFCS, tuple literals, and macro calls reduce to ordinary Kio' trees. `r.>f[T](x)` records the same call plan as `f[T](r, x)`; `(a, b)` becomes `__pair__(a, b)`. A block macro call projects its declared blocks into ordinary value arguments and invokes its resolved implementation. The reference library supplies `if`, `match`, `scope`, and `do`; these names have no compiler-owned control semantics. Macro calls and dot-splices retain boundary-local elaborations until their Kio' terms or normalized calls are substituted at the Lowered → Prime boundary.

The decidability argument combines PJ-V-W-S with Kio's finite application
boundary: source-bounded exact instantiation, shallow skolemization, explicit
binders, no let-generalization, forward-only block prefixes, structural
equality over a source-bounded goal set, and bounded one-shot retained actions.
The formal account in [`formal/elaboration.md`](formal/elaboration.md) pins the
schedule and proof obligation.

### Anonymous sum and product types

In addition to System F, Kio has **anonymous sum** (`|`) and **anonymous product** (`&`) type constructors — ordinary logical disjunction and conjunction on types:

- `A | B` — a value of this type carries one selected branch, with a payload of type `A` or `B`.
- `A & B` — a value of this type carries two components, one of type `A` and one of type `B`.

So Kio's type system is effectively **System F with disjunction, conjunction, and nominal types**, with recursion grounded at nominal boundaries.

**Anonymous structure and nominal names.** These product and sum types need no user-supplied type name, named constructors, or named fields. Nominal types can supply domain-specific field and alternative identities. A sum is tagged by position, not by a user-provided name or payload type; repeated payload types still form distinct branches.

**Product types are finite cartesian products, represented as right-associated binary products.** Tuple values are elements of those products, not members of a separate nominal family indexed by arity; tuple notation is syntax for product introduction. Mathematics normally treats different parenthesizations of a finite product as canonically isomorphic rather than as one literal binary tree. Kio chooses a right-associated tree as its definitional surface and core representation. Parentheses group expressions and types unless comma/product structure gives them product meaning. The nullary product is `()`; a one-item parenthesized form is just grouping (`(x)` and `(x,)` are `x`, not a one-tuple); two or more comma items fold to the right. Thus `(a, b, c)` is by definition `(a, (b, c))`, inhabiting `A & (B & C)`. The left-nested product `((a, b), c)` inhabits `(A & B) & C`, which is a different structural type under ordinary equality and is bridged only by an explicit macro such as `flatten_prod` / `fit`.

**Positional and type-indexed.** The operators are ordered and do not collapse duplicates. `A & A` is a two-component product, not one; `A | A` is a two-branch sum, not one. Repeated types are distinguished **positionally** (first-match wins at elimination). This is *not* Boolean algebra on types: `A & B` ≠ `B & A` and `A & A` ≠ `A` up to type equality. At runtime a `|` value carries a small positional integer tag, not a reflective type tag.

**Construction and parenthesization.** Kio accepts same-operator `&` / `|` chains **without** outer parentheses in both surface and Kio' syntax: `A & B`, `A | B`, `A & B & C`, `A | B | C` are valid `Type` expressions, with grouping required in the arrow positions described below. Chains right-associate: `A & B & C` denotes `A & (B & C)`. Outer parens become **load-bearing** where the grammar requires grouping: when the chain is a child of the *other* operator (`(A & B) | C`, `A & (B | C)`), and directly on either side of a function-type arrow (`(A & B) -> C`, `A -> (B & C)` and `A -> (B | C)`). Values of `A & B` are written as the surface tuple literal `(a, b)`, which is sugar for the Prime intrinsic call `__pair__(a, b)`; correspondingly, `(a, b, c)` is sugar for `__pair__(a, __pair__(b, c))` and inhabits `A & B & C`, whereas `__pair__(__pair__(a, b), c)` inhabits the distinct type `(A & B) & C`. Values of `A | B` are normally produced by the spine macro via `widen_sum(e)`. For explicit construction (the only path in [Kio'](prime.md)), the intrinsic injections `__left__ : [A][B] A -> (A | B)` and `__right__ : [A][B] B -> (A | B)` are available but **not in scope by default**: pull them in (along with the rest of the value intrinsics) with a top-of-file `import __intrinsics__;`. The friction of that declaration reinforces that `widen_sum` is the usual surface path. Since `A | B | C` is sugar for `A | (B | C)`, the third branch is constructed explicitly as `__right__[A][B | C](__right__[B][C](c))`. The same applies to the product intrinsics (`__pair__`, `__fst__`, `__snd__`) and the sum eliminator (`__either__`) — those six, together with the branching intrinsic `__if_then_else__` (see [Conditionals](#conditionals)), are all gated behind the same `import __intrinsics__;`.

**The zero-component product.** The anonymous product with no components has type `.` and the sole value `()`. It is typically used as the return type of a function whose effect happens at the host boundary (`String -> .`). The positional non-collapsing rule still applies: `A & .` is a 2-component product distinct from `A`.

**The bottom type.** The type `!` has no values — no surface syntax constructs one, and no nominal constructor or intrinsic ever produces one. `!` enters a program only as the return type of a host function that does not return (e.g., `fn panic(msg: String) -> !;`) or through the `__absurd__` intrinsic described below. `!` is a lexer token like `.`, not a user identifier, and cannot appear at the value level.

`!` is the **sum identity at the macro layer**: a macro may absorb `A | ! ↔ A` in either direction. The mirror at the product layer is `()` (the zero-component product): `A & . ↔ A` is likewise a macro-layer absorption. **Neither identity holds at type equality** — `A | !` and `A` are distinct types under structural comparison, just as `A & .` and `A` are; each pair is bridged only at a macro-call site, never by the type checker. Which coercions a particular macro admits is that macro library's contract; see [§ Macros are imported, not ambient](#macros-are-imported-not-ambient).

The asymmetry with type-equality identities is **deliberate**: Kio has no algebra of type equality. Associativity, commutativity, distribution, duplication/collapse, and identity absorption all live at the macro layer, not at type comparison. Where other languages teach the equality procedure to rewrite identities away, Kio's equality is purely structural and identity bridging is opt-in. One consequence: a direct sum value `s : ! | T` used where `T` is expected requires an explicit macro call — identity absorption (`! | T ↔ T`) is a macro-layer rewrite, so an imported coercion macro can bridge it.

**Elimination.**

- For `x : A & B`, project components with the `__fst__` / `__snd__` intrinsics (after `import __intrinsics__;`): `__fst__(x) : A`, `__snd__(x) : B`. For longer products, chain them (`__fst__(__snd__(x))` for the middle component of a three-tuple).
- For `x : A | B`, discriminate with the `__either__` intrinsic: `__either__(x, handle_a, handle_b)` picks the arm matching the injected branch. Surface pattern matching over `|` is the `match` dispatch macro — see [Pattern matching](#pattern-matching).
- For `x : !`, the `__absurd__` intrinsic — `__absurd__ : [A](x: !) -> A` — produces a value of any type. The usual shape is `__absurd__(panic("boom"))` at a call site that expects some `T`. There is no introduction form to mirror this — `!` has no values, so there is nothing to construct.

**Two modes, scoped narrowly.** Per the bidirectional discipline (see [Bidirectional elaboration](#bidirectional-elaboration)), every typing position runs in either synthesize (⇑) or check (⇓) mode. Synthesize is the default — most positions read an expression and compute its type from its children, with no Hindley-Milner-style global unification and no inference variable that escapes its finite connected application or lexical owner. Check mode fires at specific positions where the surrounding context supplies an expected type: a binding annotation, a parent call's parameter slot, a return-type position, or a `fn` value-parameter or return-type slot. A polymorphic application may retain a blocked immediate argument while other arguments or connected nested results refine that finite local relation, but it does not create a third typing mode or a program-wide constraint problem. Check-mode flow does not cross binders or semicolon-separated block prefixes.

A **macro** is a library-defined type-driven metaprogram declared with `macro`. A **macro call** is an explicit surface macro call that asks a scoped macro implementation to synthesize Kio' glue at that call site rather than relying on ambient inference. The umbrella includes source-to-target coercion macros, the `match` dispatch macro, and the `derive` instance-deriving macro. This document specifies the call **mechanism** here; the rule set a particular macro admits is that library's contract (see [§ Macros are imported, not ambient](#macros-are-imported-not-ambient)).

At a source-to-target macro position the source type is synthesized from
the source expression, and the target is normally fixed by a surrounding
expected type or an explicit type argument before the implementation runs.
When an unresolved type variable occurs in the macro declaration's result,
the type of the single checked term returned by the implementation may instead
solve that already-existing result variable. This remains part of the same
finite application relation: the returned term cannot introduce an inference
variable or trigger replay of the implementation.

A source-to-target macro takes its target as a type argument that follows
the value application — `name(e)[T]` — subject to the general `_`-or-elide
inference rule. It is a common reference-library use of Kio's general
successive-layer call shape: a later type-binder layer may follow a completed
value-parameter layer. The position keeps `name(e)` reading as "coerce `e` to
the surrounding expected type" without giving macros a special calling
rule.

Related local elaboration positions use the same "known shape, generate glue" discipline but are not source-to-target coercions:

- `match` clause dispatch — the dispatch macro selects a clause per DNF branch of the scrutinee's type and synthesizes the projection/injection glue that reconstructs each clause's argument. See [Pattern matching](#pattern-matching).
- **Surface forms that desugar to polymorphic intrinsic calls with elided type arguments.** The tuple literal `(a, b, …)` desugars to a `__pair__` chain whose leading `[A][B]` type arguments are not written; each operand constrains its corresponding value-parameter slot. Imported block macros likewise relate their projected operands through their resolved public types and, when declared, authenticated fills. Neither route searches declarations.

Ordinary function calls supply type arguments — fully explicit, partially
explicit with `_` placeholders, or fully elided — under the same finite
application-local rules. Written arguments, expected results, and connected
nested results constrain the call's owner-scoped goals. A value argument is
synthesized when it can complete independently, or checked after its parameter
slot becomes usable; an unannotated lambda receives its parameter type only
from that checking position. No goal escapes the connected call tree or crosses
a semicolon boundary.

The reference conditional requires a single result type: `if(cond) { a } else { b }` checks
both arms against the surrounding expected type when one exists; otherwise its
two arm results form one finite symmetric common-result relation. Either arm
whose result is independently determined may establish or refine the common
result, and source order gives neither arm inference priority. If neither the
context nor either arm determines the result, inference is underdetermined.
Incompatible arm results are rejected rather than combined into an implicit
sum; write the sum construction explicitly in each contributing branch,
normally with `widen_sum`.

Alternative-result inference is open-world. The reference conditional relates
exactly its two written arms through its public type; a fills-marked macro
may also supply its finite authenticated fill transcript. Neither route
consults ambient declarations, so adding an unrelated declaration to a module
body cannot add an alternative, nominate a result producer, or change the
common result.

**Annotations at bindings.** Every top-level binding (`fn`, `host fn`, `host
type`, `type`) carries an explicit type — these are
public-contract positions where the type is part of the declared interface.
Top-level positions accept no inferred `_` placeholder; the type must be
written out. An omitted result
annotation on `fn` or `host fn` means `.`; this is a fixed Unit default, not
inference (see [Function definitions](#function-definitions)).

Local `let` bindings may annotate the binding pattern: the unary form is
`let .(x: T) = e;`, and tuple destructuring and as-patterns write the type leaves
inside the pattern. A fully concrete local binding pattern checks the RHS
against the pattern-derived type. An untyped or partially typed binding pattern
leaves the RHS in synthesis mode; written concrete leaves are verified after
the RHS synthesizes. For every partial local form, a source `_` is first
classified under the post-materialization annotation rule below; a hole
beneath an annotation-local structural `forall` is rejected before the RHS
relation is opened. There is no let-generalization: inference never synthesizes
a `forall` on its own. Polymorphism comes from explicit `[…]` type-parameter
groups on declarations, anonymous functions, or type expressions.

**Type-argument inference and the `_` placeholder.** A call site may write `[_]` for individual type arguments to infer, or omit its type groups. When the application-local equality relation over value arguments, connected nested results, and any expected result determines the inferred type uniquely, the call is well-typed. The solution may be a monotype or a complete polytype already present as one closed type tree in that finite evidence. Inference never constructs a new `forall` tree or guesses where binders belong, and a polymorphic value does not unify with a proper subterm of a declared monomorphic parameter shape. Kio has no subtypes or implicit coercions in this relation. Unsolved binders and conflicting equalities are compile-time errors. `_` placeholders may be dropped from the call's syntactic surface; full elision is accepted for any binder count, with the same rule.

This rule is open-world: its evidence is fixed by the resolved callee type, the
written connected application tree, and the immediately enclosing expected
type. It never consults an ambient declaration pool, so adding an unrelated
declaration to a module body cannot introduce another solution or change an
existing one.

The `_` placeholder occupies a whole type-argument slot — it cannot appear nested inside a larger type expression at a call site (e.g., `f[List[_]](x)` is rejected). The user must either write the type-argument out fully (`f[List[I32]](x)`) or drop the slot entirely and let inference fill it from value-arg unification (`f[_](x)`, or just `f(x)`).

**`fn` signatures and the `_` placeholder.** `fn` value parameters and the return type may carry annotations or `_` placeholders. `: _` on a value param may be dropped to write `(a)` for "value param `a` with type inferred from context"; `-> _` may be dropped to write `.(…)` for "return type inferred from context." Each parameter's annotation is independent — there is no all-or-nothing rule. Concrete annotations (`: T`, `-> R`) are verified against the bidirectional flow's expected type from the call site; a contradicting annotation is a type error. Beyond verification, a concrete `-> R` return annotation also *seeds* the body's expected type: when a `fn` sits in a synthesis-only position — no call-site expected type to flow in — its body is still checked against `-> R`, so a macro or other expected-type-consuming form in the body's tail position finds its target from the annotation the author already wrote, with no redundant restatement. This is bidirectional *checking*, not return-type inference — `-> _` and a dropped return annotation stay synthesized bottom-up. A `fn` with `_`-marked or dropped types must be applied or bound at a position that determines them; standalone `fn`-expressions with unconstrained metavariables are a compile-time error.

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

Top-level annotations otherwise remain unaffected — `fn` / `host fn` / `host type` / `type` payloads are public-contract positions and admit no inferred `_` anywhere in their signatures.

### Nominal fields and row-typed records

A nominal entry in a `type` declaration introduces a distinct type and its
`mk` and `get` members. Ordinary products of these types can be used as
row-typed records; no separate field namespace, record type, or native field
construction, access, update, or row-let syntax is involved. For example:

```kio
type Person = { Name: String; Age: I32; };
```

This declares `Name` and `Age` and the transparent alias
`Person = Name & Age`. `Name.mk(value)` constructs a `Name` and
`Name.get(value)` projects its payload. Import the type to use its accessible
members. A type alias forwarding the terminal nominal's parameters in order
retains its member namespace under the rules in [Type aliases](#type-aliases).

Products are ordered. A row tail such as `Name & R` uses an ordinary type
parameter, and neither row polymorphism nor field identity makes product type
equality unordered. Imported library macros can construct, select, and update
ordinary product slots; they are ordinary declarations, not ambient language
forms. See [Structural products and row types](../docs/guides/products.md)
and the [macro case study](../docs/poc/macros.md). The macro calls use their
written operands and the resolved imported declarations; adding unrelated
declarations cannot add field candidates or change the identity of a selected
member.

### Type parameters

A type parameter is introduced by writing **`[name]`** — Kio's surface syntax for a `forall` binder. There is no separate `forall` keyword; `[…]` is the only spelling. Binders may appear in two positions:

- **As a signature or declaration-head group** (ordinary `fn`, dot-led lambda, `host fn`, `equiv`, parametric `type`, parametric nominal entries). In callable signatures, type groups appear in source order alongside value groups; an ordinary `fn` or lambda may have only type groups. The binder scope extends to the groups to its right and (where present) the return type and body:

    ```kio
    fn id[T](x: T) -> T { x }
    ```

- **As a prefix binder group inside a type expression**, scoping rightward over the rest of the type. The shape `[A1]…[AN] (P0 & P1 & …) -> R` denotes `∀A1 … AN. (P0 & P1 & …) -> R`. Use this whenever a polymorphic function type is needed *as a type expression* — for example, as a parameter type, the type of a `host fn` argument, the body of a `type`, or the expected type at a call site:

    ```kio
    type PolyId   = [T] T -> T;                  // ∀T. T -> T
    fn higher(f: [T] T -> T, x: Int) -> Int { f[Int](x) }
    ```

    `higher` is a rank-2 function: its first parameter has type `∀T. T -> T`, so callers must pass a value of polymorphic-function type. Type-expression binders are prefix-only; nesting function types and putting `Forall` in a return position express deeper rank.

Only those two structural positions interpret `[Name]` as a forall binder.
Fixed operators cannot contain square brackets. A variadic value expression
instead uses bare brackets or a marked square frame such as `[* *]`, with its `varop`
binding locally declared or selectively imported. Type parameters follow the
**type-name rule** (an optional single underscore followed by an uppercase
initial), not the value-name rule — see [Naming conventions](#naming-conventions).

A callee's type binders do not scope over its caller's arguments. Written type arguments and completed argument types retain their caller-local lexical and nominal identities, including when a callee binder has the same spelling. Renaming a callee's bound type variable consistently cannot change a caller's acceptance or meaning.

**Position is semantically meaningful.** In a type expression, a binder run must appear before its body type and scope over that whole body; `[A] -> R` is rejected because there is no body between the binder run and the arrow. Product domains are written with `&`, not commas. In a declaration signature, type-binder groups and value-parameter groups still form an ordered sequence: a comma-separated value-parameter group folds its parameter types to the same right-associated product domain for one `Function`, and a following type-binder group scopes only over the groups to its right. The names are local binders for that declaration body, not part of the type. Each type-binder run (`[A][B]`) lowers to nested `Forall` nodes at its position. The trailing `-> R` consumes the right edge as the function's return type. A few worked type-expression shapes:

- `(P0 & P1 & P2) -> R` is `Function(Product(P0, Product(P1, P2)), R)`.
- `[A] (P0 & P1) -> R` is `Forall([A], Function(Product(P0, P1), R))`.
- `P0 -> [A] P1 -> R` is `Function(P0, Forall([A], Function(P1, R)))`.
- `(P0 & P1) -> [A] R` is `Function(Product(P0, P1), Forall([A], R))`.

`Forall` binds one type parameter. A source binder run is represented by nesting one `Forall` per binder, in source order: `[A][B] P -> R` lowers to `Forall([A], Forall([B], Function(P, R)))`.

A type-only ordinary signature constructs exactly those `Forall` layers:
`fn nil[A] -> List[A] { body }` has type `[A] List[A]`, not
`[A] . -> List[A]`. Its body is evaluated when the written type stage is
applied by `nil[A]`. In contrast, `fn nil[A]() -> List[A] { body }` retains
the Unit value stage, so `nil[A]` returns a function and `nil[A](())` reaches
its body. Neither merely referring to `nil` nor binding it to a local
implicitly specializes or executes it. The same rules apply to `.[A] { body }`
and `.[A]() { body }`.

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

Commas are not product type syntax. `A & B` is the type of the two-component product; `(A, B)` is not a type expression. Commas separate value parameters in signatures, value arguments in calls, tuple literal items, and other value/list syntax. Type arguments and binders instead use one entry per square-bracket group, with no commas. Thus `fn foo(x: Int, y: Int) -> R` and `fn baz(x: Int & Int) -> R` both have product-domain type `(Int & Int) -> R`; `foo`'s body sees separate binders `x` and `y`, while `baz`'s body sees one binder for the whole product.

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

**One value group per function layer.** For a callee of type
`(A & B) -> C -> R`, write `f(a, b)(c)` or `f((a, b))(c)`.
If `pair : A & B`, `f(pair)(c)` passes the same first product.
`f(a, b, c)` instead constructs one `A & B & C` argument for the first
layer and is rejected when that product does not match its domain. The typer
never moves surplus components into a returned function. A single complete
stage can leave an ordinary function as its result; applying that result
requires another value group.

**Design consequences of right-associated product tuples.** Tuple literals, call argument lists, function value groups, dot-splice calls, parameter patterns, `match` clause parameters, and macro call value arguments all use the same right-associated binary-product convention. There is no one-element tuple case in any of those surfaces; a single item is grouping or a single value slot. UFCS/dot-splice inserts a value into the ordinary argument stream, and if that value has product type it fills the same product parameter a parenthesized tuple or comma-separated argument list would fill. Code generators may flatten product spines at the host ABI boundary so a host language sees positional arguments, but that is ABI canonicalization: type equality and local function values continue to use the product type.

Explicit type arguments use square brackets, separately from values:

```kio
id[String]("hello")             // instantiate, then apply
higher[[U] U -> U](.[V](y) { y }, 42)
                                // pass a complete polymorphic type
```

A declaration's ordered type and value groups expose the same stages at a
call site. Type-reference words such as `A` and `_A` belong in type positions;
value-reference words such as `x` and `_x1` belong in value positions.
Delimiter ownership is structural, before declaration lookup: a lowercase
entry in `f[x]` is an invalid type, not a reason to reinterpret the brackets
as a variadic value. Uppercase `A` in an ordinary value call `f(A)` likewise does not become a
value. A literal or literal-alias annotation `token[A]` remains a separate
annotation form.
Reserved underscore prefixes grant no resolution privilege.

Type application performs only the written instantiations. For
`fn id[A](x: A) -> A { x }`, `id[A]` has type `A -> A`,
`id[A](x)` has type `A`, and `id(x)` infers the type from the value.
`id[.]` has type `. -> .`, while `id[.](())` has type `.`.
For `fn nil[A]() -> List[A]`, `nil[A]` has type `. -> List[A]`;
`nil[A]()` and `().>nil[A]` apply its Unit stage and have type `List[A]`.
This differs from a type-only `fn nil[A] -> List[A]`, which has no Unit
value stage to call.

A syntactically empty direct/prefix surface call `f()` is different. It omits
any leading type arguments and writes one Unit value, as though the omitted
type slots and `()` were supplied in the same call. Thus `id()` infers `A = .`
from its written Unit value and has type `.`, just like `id(())`. The `nil`
above can be written `nil()` only in a local checked position that solves `A`,
such as `let .(xs: List[Int]) = nil();`; it elaborates to the explicit Kio' call
`nil[Int](())`. Without a local constraint that solves `A`, `nil()` is a
type-inference error.

UFCS inserts its receiver before the resulting prefix call is checked. A bare
UFCS callee contributes no arguments beyond that receiver: `r.>f` means
`f(r)`. An explicitly empty UFCS list is rejected because it could be read
either as no additional argument or, by analogy with direct `f()`, as an
additional Unit argument. Remove the list (`r.>f`) when the receiver is the
only value, or write the Unit explicitly (`r.>f(())`) when the call also passes
Unit. The form `().>nil[A]` saturates because its receiver is the written Unit
value.

These decisions use only the resolved callee type, the written argument list,
and local type equalities. Declaration parameter groups, runtime ABI arity,
callee identity or provenance, and a user macro's private ABI cannot
change whether a call is type-only or value-applying. Alias unfolding may
validate the type of a written value application, but cannot create that
application. When both applications are written, the type application occurs
first and the Unit value application second. Adding an unrelated module-body
declaration changes none of these finite inputs, so it cannot change an
existing call's completion or type.

A bare polymorphic value is not silently instantiated by an expected
monomorphic function type. For `id : [A] A -> A`, bare `id` retains that
polytype, while `id[_]` explicitly asks inference to instantiate `A` and
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
intrinsics, prefix and UFCS calls, and blockless macro calls; only
the macro's compile-time execution and Kio' substitution are phase-specific.
Block calls infer type arguments and supply every declared block; their projected
ordinary call follows the same residualization rules, as described below.

Explicit type application uses singleton brackets. Write every supplied type
as its own group, use `[_]` for a whole inferred slot, or omit the type groups
when local constraints determine them. These forms use the same finite local
equality relation over value arguments, connected written stages and nested
results, and any expected return type. There is no subtyping, implicit coercion
or declaration search. Unsolved binders and conflicting equalities are type
errors; see § Type-argument inference and the `_` placeholder.

**A Church-style fragment of System F-ω.** Kio admits rank-N polymorphism (every binder is explicitly written, so type-checking remains decidable) and **higher-kinded quantification** — a binder may range over a type *constructor*, not only over a saturated type. What it stops short of is full F-ω: there is no anonymous type-level abstraction (`λα. T`), and the kind language has no kind variables and no higher-order kinds. A parametric alias must be applied to exactly its declared number of parameters: its bare and undersaturated spellings cannot retain unfilled alias parameters. See [Type and literal aliases](#type-and-literal-aliases) and § Higher-kinded types below.

### Higher-kinded types

Kio expresses **higher-kinded types** through kind-annotated binders and direct type application — the same model as [Kio'](prime.md#higher-kinded-types), described here for the surface reader. The binder-kind language `κ ::= * | * → κ` (see [`grammar.md` § Kind grammar](grammar.md#kind-grammar)) records a chain of ordinary-type arguments; every arrow domain is `*`. Nominal parameter signatures can separately accept higher-kinded arguments, as described below. A binder's kind is the count of leading `*` in its annotation:

| Binder | Kind | Role |
|---|---|---|
| `[A]` | `*` | ordinary type-parameter (default) |
| `[*F]` | `*→*` | arity-1 type constructor |
| `[**G]` | `*→*→*` | arity-2 type constructor |

**Direct type application.** A kind-`*→*`-or-higher head takes one type argument per square-bracket group: `F[A]` where `[*F]` is a kind-`*→*` binder and `A : *`; `Either[String]` where `Either` is an arity-2 nominal type. There is no `__App__` wrapper and no application keyword. Kinds compute by consuming one arrow per argument — `Either` has kind `*→*→*`, so `Either[String]` has kind `*→*` and `Either[String][ I32]` has kind `*`. Applying a kind-`*` head to an argument, or supplying more arguments than the head's arrow count, is a kind error.

Every type that classifies a runtime value must be saturated to kind `*`. This includes function parameters and returns, nominal payloads, annotations, and every member of an anonymous product or sum: `F[A] & G[A]` and `F[A] | G[A]` are well-kinded when `F` and `G` have kind `*→*`, while bare `F`, `F & G`, and `F | G` are kind errors. A constructor may remain higher-kinded only where another type constructor explicitly expects that kind, such as the `F` argument in `Monad[F]`, or as a partial application such as `Either[String]`. This rule follows only the written binder kinds and the selected nominal declaration; adding unrelated declarations cannot change it.

A transparent alias reference must supply every parameter declared by that alias. Thus `type Pair[A][B] = A & B;` admits `Pair[String][ I32]`, while `Pair` and `Pair[String]` are type errors. This exact-arity rule concerns the alias declaration's own binders; ordinary transparent expansion determines the resulting kind of an exactly applied alias. For example, with `type Unary[E] = Either[E];`, `Unary[String]` supplies the alias's only parameter exactly and has kind `*→*`, inherited from the partially applied `Either` body.

**Kinds are explicit and immutable.** A nominal type's signature is its declared parameter list, each parameter carrying its own kind: an all-kind-`*` nominal type `Either[E][A]` accepts two kind-`*` arguments, while a dictionary nominal type `Monad[*F]` accepts one kind-`*→*` argument. A type application checks each argument against the matching parameter's kind. A binder's kind is the annotation the user writes. Nothing is inferred from how a type is used, and a nominal type's kind never changes after its definition — this is what preserves open-world monotonicity (see § Open-world design): adding a new source declaration cannot retroactively alter the kind of an existing one. Kind annotations are part of a type's public signature.

**Complete polymorphic function schemes are value types.** One or more `Forall` layers ending in a `Function` form a complete function scheme. The whole scheme has kind `*`, even when one of its binders is higher-kinded, and may therefore appear anywhere a runtime value type may appear: as a parameter or return, inside a product or sum, in an annotation or type argument, as an anonymous function literal's type, or inside a recursive or existential nominal payload. This is a structural rule, not a privilege of a named declaration; unfolding a transparent alias does not change the classification. A higher-kinded `Forall` whose body does not ultimately reach a function is incomplete and is rejected; bare and undersaturated constructors remain non-value types. Nominal existential binders themselves remain kind `*`. The check depends only on the written type structure and explicit binder kinds, so adding unrelated declarations cannot change an existing type's admissibility.

**Higher-kinded application without primitives.** Lifting a value into and projecting it out of a kind-`*→*` position uses ordinary language constructs, not intrinsics. (How a backend carries that kind identity through to host code — a native host type constructor, or the transparent erased rep — is a backend-encoding concern, described in [`backends/README.md` § Higher-kinded types](backends/README.md#higher-kinded-types); at the language level there is only the kind-annotated binder and direct `F[A]` application.)

- At a **concrete type constructor** (a named kind-`*→*` nominal type like `Box[A]`), the nominal type's own constructor / projector pair crosses the boundary: `Box.mk(x)` lifts, `Box.get(b)` peels. The backend contract determines the host representation; equality between the payload and the type parameter does not erase the type's nominal identity.
- At an **abstract type constructor** (a kind-`*→*` binder `[*F]`), the lift / projection routes through an explicitly-threaded instance value. A combinator over `[*F]` that needs to inject takes an `Applicative[F]` parameter and projects its operation with `Applicative.get`; one that needs to project takes a `Comonad[F]` parameter and uses `Comonad.get`. Instances are explicit `fn` parameters — Kio has no typeclasses. The [higher-kinded-types case study](../docs/poc/hkt.md) shows this dictionary-passing pattern.

**Polymorphic nominal payloads.** A nominal payload may itself be a polymorphic function — the dictionary pattern that underwrites `Functor[*F]`, `Monad[*F]`, and related abstractions. The payload shape is a `Function` nested under one or more unary `Forall` binders, where the binders scope over the args and ret:

```kio
pub type { Monad[*F]: [A][B] (F[A] & (A -> F[B])) -> F[B]; }
```

The constructor accepts a polymorphic lambda literal whose binders match the payload's; the projector yields a value whose polymorphism survives projection (`Monad.get[F](monad)` at an abstract `[*F]` produces a value with the same shape as a top-level polymorphic `bind` fn). The imported `do` accepts that value as its supplied bind function: `do(Monad.get[F](monad)) { … }` uses the same sequence projection as `do(top_level_bind) { … }`. Per-backend codegen must support this pattern. Construction and projection preserve the payload's staged callable shape; erasing bound types does not erase their application stages. The backend contract determines the nominal carrier and its construction and projection operations. See [`backends/README.md` § Polymorphic nominal payloads](backends/README.md#polymorphic-newtype-payloads) for the cross-cutting scheme.

**Multi-arity type constructors and partial application.** A multi-parameter nominal type is a higher-arity constructor whose partial applications are themselves higher-kinded types. `Either[E][A] : E | A` has kind `*→*→*`; `Either[String]` has kind `*→*`, so `Monad[Either[String]]` is well-kinded. See [`docs/guides/higher-kinded-types.md`](../docs/guides/higher-kinded-types.md#multi-arity-type-constructors) § Multi-arity type constructors for the worked `Monad[Either[String]]` example.

This partial-application rule does not permit a transparent alias reference to omit any parameter declared by that alias.

<!-- markdownlint-disable-next-line MD033 -->
<a id="macros-are-imported-not-ambient"></a>

### Macros are imported, not ambient

Every macro is an ordinary module item resolved through scoped imports — there is no built-in coercion, no ambient dispatch, and no language-level palette of named forms. An ordinary call writes `name(...)`; a macro with trailing-block descriptors also admits `name(args) { … }` or zero-prefix `name { … }`. Transparent parentheses may group the direct head, including `(name)(args) { body }`. Parsing recognizes either shape without consulting provider declarations; ordinary lexical ownership selects the macro (see [`grammar.md` § Surface item additions](grammar.md#surface-item-additions)). The reference `macros` library supplies `if`, `scope`, `do`, `match`, `derive`, an **algebraic** coercion palette (`iso`, `into`, `onto`, `align`, `ease`, `atom`), and a **spine** palette (`reorder_sum`, `reorder_prod`, `narrow_sum`, `narrow_prod`, `widen_sum`, `widen_prod`, `flatten_sum`, `flatten_prod`, `one_sum`, `one_prod`, `fit`, with natural-result variants `narrow_sum_`, `one_sum_`, and `one_prod_`). These are ordinary Kio declarations, imported explicitly. Their per-form algorithms are documented with the executable library in [`docs/poc/macros.md`](../docs/poc/macros.md); [`docs/poc/optics.md`](../docs/poc/optics.md) demonstrates the spine palette and `match`.

**Explicit selection and lexical ownership.** External macros require a tagged
selector such as `import library(macro build as make);`. The selected export
must be a visible macro; an ordinary named import or module alias does not
introduce macro authority. Kind and visibility checks apply even when no body
uses the import. Macros and ordinary values share the value-introduction
namespace, with the same eager conflict checks. An earlier same-module macro
is available locally without an import. The nearest ordinary lexical binder,
including a numbered placeholder owner, shadows an imported macro; resolution
does not fall back to it based on type or attempted call shape.

A macro is usable only as a direct local named call head, transparently through
parentheses and UFCS. Bare macro values and qualified macro invocations are
rejected. A macro's result may be an ordinary function. Declaration inspection
continues to accept its ordinary name, alias or fully qualified identity; the
macro import selector is not a new inspection query form. At statement start,
native binding syntax owns `let value { body }`; `let(value) { body }` or a
macro alias supplies the unambiguous macro call. Formatting preserves that
distinction.

**Named-callable contract.** Top-level `fn`s, intrinsics, and macro names are all looked up by name and expose a declared call type (a polytype). At a call site, explicit type arguments, `_` placeholders, value arguments, and the surrounding expected type are all solved against that call type by the ordinary call-type rules of § Type system. The difference is that a macro's macro call runs compile-time implementation code over **reflected arguments** rather than the runtime function-call path; the ordinary and fills-marked reflected ABIs are documented below and in [`docs/guides/macros.md`](../docs/guides/macros.md).

A macro receives a checked input as a quoted term and may omit it from its
generated code. When that code evaluates a retained product input, ordinary
strict product evaluation evaluates its components in order. To make a
component independently skippable, pass it as a thunk and invoke that thunk
only when needed. This runtime behavior follows the generated code, not the
macro's call spelling.

**Whole-chain public admission.** A maximal written application chain rooted
at a resolved direct macro head is one connected public inference and
admission unit; transparent grouping does not split it. Its written stages
and resolved public type gather the available local constraints before source
admission and the macro's one implementation action. No incomplete prefix
executes early to discover which inputs should have been admitted.
Applications to an ordinary function generated as the macro's result remain
ordinary result applications, not extra reflected macro operands. Contextual
lookahead stops where the public result has no known callable shape; after
ordinary admission and expansion, a callable generated result may be applied
normally. A `let` ends the connected unit. These rules grant no first-class
macro value or implementation-derived public call shape.

**Trailing-block declarations.** A macro may declare one or more block
projections with `trailing product;`, `trailing thunk;`, or
`trailing sequence;`. The first descriptor is unlabelled; every later descriptor
has a distinct value-shaped label, for example `trailing thunk else;`.
`impl` and `captures` retain their unordered entry policy; the relative order
of the `trailing` entries determines the order of blocks. Each descriptor
selects a projection from block syntax to one value. Declaration checking
validates descriptor structure; the projected call is checked against the
public type at each invocation. Descriptors impose no additional predicate on
an ordinary direct call.

| Descriptor | Value projected from the block |
| --- | --- |
| `product` | Independent expressions: no entries give unit, one gives that expression's value, and several give their right-associated product. Top-level `let` and `<-` bindings are rejected. A local binding inside a scoped entry cannot extend into another entry. |
| `thunk` | An expression-block body wrapped in a zero-argument function. Empty gives a thunk returning unit. Ordinary local bindings are admitted; `<-` bindings are rejected. The macro determines when the thunk executes. |
| `sequence` | An ordinary function with structural type `([A][B] (F[A] & (A -> F[B])) -> F[B]) -> F[R]`, parameterized by the bind function. Ordinary lets, `<-` bindings, and sequenced expressions precede an explicit final `F[R]` result. Empty and statement-only sequences are rejected; no implicit lift is inserted. |

The neutral block grammar admits semicolon runs at either edge and between
entries. A final semicolon does not discard a real final expression. Descriptor
validation determines which item forms are admitted. Sequence projection and
its binding rules are detailed in [Monadic do blocks](#monadic-do-blocks);
they apply to every `sequence` descriptor, regardless of the macro's name.

**Block calls.** Every declared block is supplied exactly once, in declaration
order. The first is unlabelled and later blocks carry their declared labels.
After UFCS insertion, the whole final written prefix value, if present, and
whole projected block values form one right-associated product supplied to one
`Function` layer. Earlier prefix groups remain separate applications. Thus
`m(a,b) { c; d }` projects to `m((a,b),(c,d))`, equivalent to
`m((a,b),c,d)`. Omission and Unit differ: `m {}` supplies Unit, while
`m() {}` supplies a product of two Units. No implicit currying splits a product.

Block calls infer their public type arguments; explicit type arguments are not
part of the block-call spelling. Once all syntactic blocks are supplied, the
ordinary macro residualization rules apply. Direct calls may supply projected
values explicitly, for example `if(condition, .() { yes() }, .() { no() })`. Direct calls use the public
type and ordinary macro semantic checks, without descriptor-specific checks.
The public call plan is fixed before any implementation action runs.

Every nonempty written prefix uses a parenthesized value-argument list:
`name(value) { … }` has one prefix group; `name(a, b) { … }` and
`name((a, b)) { … }` both supply the same whole prefix product.
`name(a)(b) { … }` has an earlier `(a)` stage and combines `(b)` with
the blocks; `name() { … }` includes a Unit prefix value, and `name { … }` has none. The parentheses are required for all
prefix expressions, including names, signed numbers, Boolean literals and
operator expressions. UFCS can supply a receiver with no written prefix list:
`receiver.>name { body }` and `name { body }.<receiver` insert that receiver
before the projected block arguments. A nested call's own block stays inside the list:
`outer(convert(value)) { … }` has a blockless prefix argument, while
`outer(convert(value) { child }) { … }` has a nested block call.
See [`grammar.md`](grammar.md#surface-expression-additions) for the complete
expression boundary.

Only the final labelled block may elide its braces, and only when its whole
body is another unspliced block call and its parent is not reverse-spliced.
Thus `if(a) { x } else if(b) { y } else { z }`
is the generic abbreviated spelling of
`if(a) { x } else { if(b) { y } else { z } }`; neither `if` nor `else`
has a privileged parsing role.

A reverse-spliced call requires explicit braces around its own final labelled
block. Thus `outer(a) { A } label inner(b) { B }.<r` and the corresponding
`.<<r` form are parse errors. Grouping the whole call does not remove this
restriction. Write `outer(a) { A } label { inner(b) { B } }.<r` to insert into
`outer`, or `outer(a) { A } label { inner(b) { B }.<r }` to insert into `inner`.
The diagnostic's brace-insertion fix retains the outer attachment; it does not
move the receiver into the child. These label braces cannot elide.
Direct and forward-spliced chains retain the shorthand, and a deeper ordinary
chain inside an explicit outer block may still use it.

Malformed syntax is a parse error (`11`). Import and name failures retain
their ordinary categories (`12`/`13`). Once the macro is resolved,
missing, duplicate, unknown, or out-of-order blocks, invalid block contents,
and projected-call type mismatches are type errors (`14`), with source and
declaration context. An ignored non-unit sequence step explains the required
`F[.]` and suggests `<-`. Library-reported errors retain their ordinary
`14`/`15` distinction; these categories impose no diagnostic precedence.

Parsing and formatting depend only on written syntax. Block projection depends
only on written structure and the resolved declaration's descriptors, and generated
terms obey the ordinary checking, purity, hygiene, and recursion rules.
Adding unrelated declarations cannot reinterpret an existing call. Descriptor
metadata, neutral blocks, and provisional projection state are consumed before
Kio'; none grants persistent authority to the resulting ordinary term.

**Explicit implementation dependency.** A macro declaration chooses one
of two implementation schedules. In both modes, the field names its
implementation with an ordinary lexical value path. A bare path may name a
local function or a selectively imported function. A dotted path may select a
function through an explicitly imported module alias (`helpers.build`) or a
constructor/projector on a local or imported nominal type, directly or through an
identity alias (`Box.make` or `types.Wrapped.make`). Slash-qualified package
FQNs such as `lib/helpers.build`, calls, and inline lambdas are not
implementation targets; use an explicit `import` and name an ABI-shaped `pure fn`
instead.

Parentheses may group the whole reference repeatedly, as in
`impl ((helpers.build))`, without changing its identity. This also applies to
each named `captures` entry. The exact `(fills)` spelling after `impl` selects
the fills schedule only when another target follows: `impl (fills)` names an
ordinary callable `fills`, while `impl(fills) (build)` selects `build` with the
fills schedule.

Resolution matches an ordinary path expression's source-order classes. A
same-module ordinary or host `fn`, `rec` member, nominal head, or identity-alias
head must precede the declaration. A self-qualified alias or selective
self-import does not turn a later declaration into an earlier binding.

The ordinary field `impl f;` selects the late schedule and this ABI:

```text
(__Comptime__ & captures... & reflected declared slots...)
  -> __Checked_term__
```

Every declared value slot is a completed `__Checked_term__` before the
implementation runs. Each declared type slot is `__Type__`, except that an
eligible result-only binder may use `.`. Eligibility follows the exact binder:
it must occur in the terminal result and must not occur in any later value
domain, after transparent aliases unfold and with lexical shadowing respected.
A dead binder is not result-only. The `.` slot always receives unit, including
when an expected result is available; the implementation cannot observe that
result through this slot. A result-only `__Type__` slot instead requires a
complete target before evaluation, even if the implementation ignores it.
`__Type__ | .` is not an implementation-ABI option.
The returned checked term is related to the declared result and may solve its
preallocated result goal. Ordinary implementations receive no fill context.

The declaration-local field `impl(fills) f;` selects the fills schedule and
this ABI:

```text
(__Comptime__ & __Fill_ctx__ & captures... & reflected declared slots...)
  -> (__Checked_term__ & __Fill_ctx__)
```

`__Comptime__` remains the immutable leading proof. The compiler-created,
call-local `__Fill_ctx__` is exactly second and is the only threaded fill
state. Captures keep declaration order. The same natural `.` and required
`__Type__` choices apply to eligible result-only binders. Other type slots are
`__Type__` and may carry authenticated open handles under the marked header
rules below. Every value slot is `__Checked_term__`. The implementation returns
one checked term paired with the final context and never returns `__Comptime__`.
A natural result may be determined by the returned term's complete type after
all retained producers finish. Explicit fills can relate that result to
provisional operands before their bodies resume. The implementation may return
the original context unchanged; fill authority does not require an append.

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
still checked once before publication, including when the macro ignores
the operand. No body, nested macro action, or immediate-lambda callee is
entered merely to complete such a header.

If a computed source remains open after these public header constraints, its
retained ordinary derivation completes before the enclosing macro prepares its
inputs, using the information already available. This can complete an inner
call, its callback body, or an inner macro invocation whose own inputs are
ready. Each source and macro invocation is checked or evaluated once, without
retrying an alternative interpretation. A source whose type is already complete
retains the header-first order above. Information supplied only by the enclosing
macro's action cannot complete one of its own inputs.

A direct lambda is the
structural exception: its complete universal and value-parameter
header may precede an open final body-result subtree, but no unresolved source
placeholder may remain. A computed source that remains undetermined after
ordinary completion is a type error at that source before the implementation
action runs. Binding the whole source
in an ordinary `let` completes its ordinary body checking first, but does not
by itself supply a missing generic choice. A complete binding annotation,
explicit type argument, or concrete callback result type can supply that
information where appropriate. Compile-time reflection
authenticates marked opaque type handles before reduction. An evaluated
operation that needs unresolved inference information raises an uncatchable
macro error (exit 15); it does not return a route mismatch, diagnostic arm, or
residual value. Known rigid generic variables are determined information.
A known product, sum, function, or forall view may return opaque children;
viewing an unknown child itself fails. Named-head views also require the
argument-root information they report. Root predicates need only their outer
constructor; spine folds need each traversed right-spine head, while their
left elements may remain opaque. Applied-argument folds need a known outer
head. `__type_forall_prefix_fold__` visits the determined leading universal
binders, outermost first, and returns its accumulator paired with the remaining
type. It requires a known stopping head but may return a Function with opaque
children. Its callback receives the same binder handles as repeated type views;
kind arity is available through `__type_var_arity__`. It establishes the finite
immutable prefix before calling the callback once per binder.
Type equality, type display, and type instantiation require complete
operands. Corresponding parameters opened from equivalent complete generic
types compare equal, including when those types were independently rebuilt.
The comparison preserves nominal identities, binder positions and genuine free
variable environments. A comparison that needs missing type information fails
rather than guessing equality or inequality; this also applies after converting
a variable handle to a Type. Opening, storing and transporting a fragment does
not itself compare its missing structure.

A new quantified Type or polymorphic checked term may use its own parameter,
a live enclosing constructor parameter, or a genuine free caller parameter.
The result of `__type_forall__` or `__term_type_fn__` must not contain a free
parameter opened from another generic type. A complete generic type may remain
intact within the result: its own parameters are bound there. To use an opened
body under a new parameter, explicitly instantiate the original generic type
with the chosen parameter before constructing the result. Ordinary generic
helper calls substitute their type arguments normally; they do not turn a
substituted free opened parameter into a genuine caller parameter or waive the
constructor boundary. Inspection and storage of opened fragments outside these
construction results remain valid. The check covers type occurrences inside a
checked term, not just its outer type. An unknown region cannot establish the
absence of a forbidden reference without sufficient information.

Type and term constructors may transport opaque children when their
own required outer shape is known, subject to this quantified-construction
boundary. Fill operations record opaque candidates
without inspecting them. An evaluated fault cannot be caught, masked by a
later error, or discarded, and is not cached as a successful result. An
unevaluated branch does not fault. A semantic reflected Type nested inside
ordinary recursion data counts as one atom, regardless of whether its contents
are closed or opaque. A Type used directly as fuel retains structural
measurement: an initial known head admits a conservative lower bound, while a
recursive edge requires a complete measure.
The formal boundary is specified in
[macro evaluation](formal/elaboration.md#74-macro-calls). During this
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

A named implementation declared in the macro's defining module may remain
private regardless of the macro's visibility, and any implementation may
call private helpers in its own module. A target declared in another module
must be ordinarily importable into the defining module through a written
`import`, but it need not meet the macro's outward visibility. These
functions are compile-time implementation details, not declarations a caller
imports. A nominal constructor or projector keeps its ordinary member type and
cannot satisfy either macro ABI; use an ABI-shaped function instead.

The implementation path follows only the declaration module's lexical scope
and its written `import` clauses. Those imports are the only cross-module
dependency edges created by the target: a selectively imported function or an
explicit module alias supplies the binding before any dotted member is
selected. Resolution never scans other modules or treats a slash-qualified FQN
as an expression. Adding an unrelated declaration therefore cannot rebind the
target or alter an existing macro; a same-scope duplicate is rejected as
a declaration conflict rather than silently taking precedence. Fully qualified
paths remain available on the separate surfaces that define them, including
type paths, REPL and Kiodoc name queries, and dynamic-loader identity strings.

Every item named by `captures` must have visibility equal to or wider than the macro declaration. Captures differ from implementation helpers: a captured type becomes a quoted `__Type__` descriptor and a captured value becomes a quoted `__Checked_term__` that the implementation may place in the generated runtime term. A capture does not execute the captured declaration at compile time, so it may name a host type, host function, or other runtime-provided item. The visibility floor applies because that dependency can survive the macro call; it does not grant the implementation general access to private declarations.

**Fill and specialization helpers.** Alongside the compile-time proof and
reflection helpers, `import __comptime__;` introduces the opaque type
`__Fill_ctx__` and these values:

```kio
__fill__ :
  (__Comptime__ & __Fill_ctx__ & __Type__ & __Type__) -> __Fill_ctx__

__fill_result__ :
  (__Comptime__ & __Fill_ctx__ & __Type__) -> __Fill_ctx__

__term_specialize__ :
  (__Comptime__ & __Checked_term__ & __Type__ & __Type__)
    -> (__Checked_term__ | [. | __Diagnostic_text__])
```

Every callable in the block retains `__Comptime__` as its first value
parameter. Both fill helpers take `__Fill_ctx__` second and return the
descendant context.

`__fill__(ct, fills, destination, candidate)` does not solve or inspect
inference while the implementation executes. It returns a new context with
one relation appended; discarding that returned context discards the entry.
The destination must be an authenticated occurrence of this marked call's
declared result; an arbitrary reflected type, source operand, or constructed
clause tuple is not a fill destination. The candidate may retain staged body
provenance but gains no destination authority from it.

`__fill_result__(ct, fills, candidate)` appends equality between the candidate
and this invocation's whole terminal result. It neither returns nor observes
the result type. For a partially applied macro this is the body result in its
retained lexical scope, not the residual function introduced around that body.
A candidate may contain the residual body's rigid binders. The candidate's
producer dependencies are retained; no result-type producer is introduced.
Discarding the returned context discards the entry. Old and whole-result
entries share the same authenticated, atomic adoption below.

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
specialized checked term on success, unit for an ordinary known structural
route miss, or diagnostic text for malformed or underdetermined specialization
of complete inputs. The pattern and target must be complete, and the term's
leading forall prefix and its end head must be known; a known function may
still have an opaque result child. Missing information needed for this
observation is an uncatchable macro error, not a diagnostic arm. It neither
takes a fill context nor appends a relation. The successful term's reflected
type is the fully instantiated whole function scheme. Ordinary code may use it
with closed operands, but only a marked implementation receives provisional
operands and a compiler-created fill context.

Neither capability exposes solver state, source indices, declaration
identities, or transcript contents. `__Fill_ctx__` has no source constructor,
cannot be converted to or from `__Comptime__`, and cannot be serialized or
retained in generated runtime code.

**Compile-time purity and runtime result.** A `macro` declaration has no `pure` modifier. Its implementation always executes in a pure context: a named implementation must be a `pure fn`, and every ordinary function reached transitively while evaluating it must also be declared `pure`. Local type annotations and implementation ABI types may mention any well-formed type, including host types; named types in the macro call type obey the declaration-visibility floor. Captures remain quoted inputs subject to their separate visibility rule above. This separates the code that computes a term from the term it computes.

A macro call in a `pure fn` is therefore not accepted or rejected from a purity bit on the macro. The implementation runs, its checked term replaces the macro call, and the assembled Kio' package is validated again. If the generated term contains a host-function or unmarked-function reference, that final check rejects it in a pure caller; the same term remains admissible in an ordinary caller. A generated pure term is admissible in either.

**Generated terms are hygienic.** The Kio' tree a macro emits is constructed through reflected term constructors, not by reaching into the caller's scope:

- generated macro terms are hygienic — they bind and reference only what the macro builds, independent of names in scope at the call site;
- a caller is **not** required to `import __intrinsics__;` for the intrinsic calls a macro emits on its behalf — the macro's output carries the intrinsic reference directly;
- the reflected term constructors are split into core `__term_*__` constructors (for core-language terms) and intrinsic-call `__intrinsic_*__` constructors (for the reserved intrinsic values);
- the `__intrinsic_*__` constructor names mirror the `__intrinsics__` value names — each `__name__` intrinsic has the constructor `__intrinsic_<name>__` (`__pair__` → `__intrinsic_pair__`).

The Boolean constructors `__intrinsic_and__` and `__intrinsic_or__` take
`(__Comptime__ & __Checked_term__ & (. -> __Checked_term__))` and return
`__Checked_term__`. The checked left operand supplies the concrete Boolean
carrier; the callback constructs a right operand of that same type. As with
the conditional constructor, this callback builds code at compile time:
only the resulting runtime right operand is conditional. Both constructors
retain the ordinary checked-term purity, capture and validation requirements.

**Target type-argument.** A source-to-target macro takes its target as a
type argument *after* the value application — `name(e)[T]` — subject to the
general `_`-or-elide rule (see § Type-argument inference and the `_`
placeholder): explicit `T`, a `_` placeholder, or a fully elided slot. An
explicit `T` and a surrounding expected type must be `equiv`-equal. An elided
target is normally filled by the surrounding expected type; for a
type-producing macro, the checked term returned by its implementation may
instead determine the declaration's pre-existing result variable. If neither
route determines one type, the call reports "cannot infer type argument." Each
name also has the [receiver-first UFCS spelling](#ufcs) `(r.>name)[T]` (or
`r.>name` when no target argument is supplied), with the same ordinary call
stages. Brackets on the UFCS callee itself precede receiver insertion; they
do not supply a returned target binder.

**No surface form survives into Kio'.** The typer records each macro-call elaboration in the `Elaborations` side channel; the substitute pass swaps the recorded Kio' tree in at the Lowered → Prime boundary. The declaration mode, `__Comptime__`, `__Fill_ctx__`, fill transcript, provisional terms, and compile-time helpers are all consumed before that boundary. The ordinary `pure fn` marker remains on Kio' function declarations so the fresh Prime validation can check the substituted body without hidden surface state. `kio-prime` rejects macro declarations, tagged macro imports and trailing-block calls at parse time; an ordinary call-shaped expression cannot acquire macro authority in Prime — see [`prime.md` § What's not in Kio'](prime.md#whats-not-in-kio). [Kio'](prime.md) has no macro and no sugar: identity absorption and reassociation happen only at macro-call sites, never at type comparison.

**Open-world.** The implementation schedule belongs to the one macro
declaration selected by ordinary resolution, after its public call plan is
fixed. Marked staging walks only the value operands in that plan. Its one
call-shell exception uses the already-resolved compiler-owned intrinsic kind
and the exact canonical scheme above; an ordinary declaration cannot acquire
that kind by matching a name or type shape. Adding an unrelated declaration to
a module body therefore changes neither the selected schedule, staged
operands, transcript order, nor completed result of an existing call.

**Crossing nominal and host boundaries is not a macro's job.** A source-to-target coercion rearranges structural shapes inside `&` and `|`; it never manufactures or discards a nominal wrapper, and never treats a `host type` as structurally equal to its consumer-supplied binding. Cross a nominal boundary with its accessible `mk` and `get` members; cross a `host type` boundary with a host-provided conversion.

Unlike typeclasses, which insert dictionary lookups at every polymorphic call site automatically, a macro only runs where you import its name and write the macro call.

### Traits as nominal dictionaries

Kio has no `trait` or `class` construct. A nominal type can wrap a function,
and an ordinary product can bundle several such values as an explicit
dictionary:

```kio
pub type { Show[A]: A -> String; }
pub type { Less_than[A]: (A & A) -> Bool; }
pub type Ord[A] = Show[A] & Less_than[A];
```

The nominal identities distinguish methods even when their function types
coincide. Callers pass the dictionary explicitly and project the desired
nominal slot and its payload through ordinary functions or imported macros.

## Module system

A **module** is a namespace and typechecking unit. Modules are addressed by their filesystem path inside the current package, with directory separators written as `/` — so a module path reads like its on-disk location (`utils/string` → `utils/string.kio`).

**Every module file begins with a `module` declaration.** A module file starts with `module <path>;`. The `<path>` is the module's location relative to the package root: any subdirectory segments, ending with the module's filename without `.kio`. The package name is not prepended at the declaration site or in `import` paths.

```kio
// hello/main.kio
module main;

pub fn run() { print("hi") }
```

The package root is the directory containing `<name>.pkg.kio`. The file stem is the package name, and regular module paths are checked relative to that root. The declared segments must equal the file's path relative to the package root (`.kio` stripped); mismatch is a parse error (exit code `11`). A module belongs to exactly one package.

A root `.kio` file may declare `host type` / `host fn` items after its `module` declaration and `import` clauses, like any other module. Host declarations are ordinary module items under ordinary lexical scoping: a host item's signature names types brought in by `import`, and another module reaches a host item through a plain `import`. There is no env block.

A `host type` is either roleless and optionally parameterized by ordinary kind-`*` parameters (`host type Array[A];`), or role-bearing and nullary (`host type Int role(i32);`). Higher-kinded binders are not admitted on a host type: `host type Wrapper[*F];` is a type error. The roleless and role-bearing forms are disjoint, so combining any type parameters with `role(...)` is also a type error. A role identifies the literal or conditional syntax the nullary type receives; it cannot describe a family of such atomic types.

**Subdirectories organize the module path.** A module at `utils/string.kio` declares `module utils/string;` and is reached by consumers as `utils/string`. Each `/`-segment in the declaration is a subdirectory, and the final segment is the module's filename. Nesting is unlimited; intermediate directories are path segments only, holding no declarations of their own.

**Paths are absolute inside the current package.** Every `import` path names a module path in the current package, never a file location or a relative point in the module tree. Naming a sibling module is written `import utils/string as s`. There is no relative-path form and no `crate::`/`super::`/`self::`-style prefix.

**Dependency roots.** A package may declare a cross-package dependency in a `<local>.dep.kio` file at its root (see [`package.md` § Dependency files](package.md#dependency-files)). The dependency's module tree is **re-rooted** under its local name: the dependency's `module app;` becomes reachable as `<local>/app`, and its `pub` items are importable with the ordinary `import` forms (`import <local>/app(greet);`). An `import` path's first segment names a declared dependency's local name or one of the consumer's own modules: a first segment that matches a declared dependency resolves in that dependency's re-rooted tree, otherwise in the consumer's own modules. A dependency's local name may not equal the first path segment of any of the consumer's own modules, which keeps that choice unambiguous (see § Open-world design).

**Visibility.** Every definition is private by default. The **`pub`** contextual keyword on a definition makes it exported. `pub` is only recognized in declaration-leading position (top-level, or at member positions inside a nominal member body or braced `rec` group); elsewhere it is an ordinary identifier.

A `pub` may carry a **scope restriction**, written `pub(<module-path>)`: the definition is exported only within the module subtree rooted at that path — importable from that module and anything beneath it, sealed everywhere else. The path must be a **prefix of the declaring module's own path**: visibility relaxes only up to an ancestor, never sideways to an unrelated module or down to a narrower one, so nothing is ever visible where its own module cannot reach — a non-prefix path is a compile error. The restriction is enforced at name resolution as an import gate **and retained into Kio'**, where it also shapes the package's **host interface**: a scoped definition is not exported to the host, which sits outside every module `path`. It is a core visibility form, not a surface-only one. Because a scoped definition is strictly narrower than `pub`, adding one cannot widen any existing import — open-world compilation is preserved. The scope is admissible wherever `pub` is a genuine visibility choice — including nominal `mk`/`get` members and `rec`-group members. `host` declarations are the sole exception: always public, hence never scoped.

**Declaration dependencies obey the same visibility floor.** Every named type
reachable through a declaration's signature must be visible everywhere that
declaration is visible. The walk is recursive through type arguments,
functions, sums, products, and transparent aliases, so a public alias cannot
conceal a private nominal type. It applies to ordinary and recursive function
signatures, type aliases, host-function signatures, macro call types, and
nominal entries. A nominal type remains
an opaque API boundary: its payload need not be as visible as the outer
type. Instead, the payload types must meet each constructor or projector's
**effective visibility**: the intersection of the nominal type's visibility
and that member's own marker. Thus `pub type { Token: Hidden { mk; get; }; }` with private
members is valid; a private `Token` remains private even if a member is written
`pub`; and a public `Token` with a public constructor or projector requires
`Hidden` to be public. Scoped combinations take the narrower subtree. Type
parameters and local binders are not declaration dependencies.

**Function purity.** An ordinary module-body `fn` is unrestricted by default. Prefix it with **`pure`** to make a transitive promise about executable references in its body:

```kio
pub pure fn id[A](x: A) -> A { x }
```

`pub` and `pure` may appear in either order in source; `kio fmt` emits `pub pure fn`. There is no `impure` keyword. `pure` is admitted only on ordinary `fn` declarations, in both Kio and Kio'. It is not admitted on `type`, `macro`, `op`, `literal`, `equiv`, `host` declarations, or any `rec(loop)` group/member.

A `host fn` cannot carry `pure`: its body is opaque to Kio, leaving no body whose executable references the checker could validate. A `rec(loop)` member is excluded because its execution requires the group's declared `loop` function. Both exclusions are categorical rather than inferred from a particular implementation.

The restriction applies only to executable references in the function body. Type annotations inside that body use the ordinary well-formed-type rules and may mention host types, transparent aliases, nominal types, or any other in-scope type. A function signature also has no purity classification, but its named types obey the declaration-visibility floor above. Type declarations carry no purity classification.

Purity is transitive and declaration-based. In a pure body, executable value paths may resolve to local binders, pure intrinsics and nominal members, or ordinary functions explicitly declared `pure`. While checking a macro implementation, compiler compile-time primitives are admitted as the pure primitive environment; they are consumed before persistent Kio' and are not runtime values. A host function or an ordinary function without `pure` is rejected even if a particular implementation happens not to perform an effect. An unrestricted function may refer to both pure and unrestricted functions. The marker is retained in Kio' and on the public function contract so a dumped, freshly reparsed, or validated cached Prime artifact can check the same promise without producer provenance. A loadable image retains the marker, but the runtime loader trusts precompiled function bodies and does not perform that typecheck (see [`prime.md`](prime.md)).

This is a restriction on declaration references, not an effect qualifier on function types. A pure function may invoke a callback supplied through a local binder, including a host callback supplied by an unrestricted caller. A call to a pure function is effect-free when argument evaluation and all invoked callbacks are effect-free, including callbacks carried in data or captured by closures; the `pure` marker alone does not establish that condition.

**`import` statements.** `import` statements appear only at the top of a module region, and each is terminated by a semicolon. Two forms are available:

- **Selective** — `import path/to/module(Type, name, op _ + __);`
  brings each listed binding into scope directly. Bare names select ordinary
  values/types, and an `op` tag followed by a
  complete grammar selects an operator.
- **Qualified** — `import path/to/module as m;`, accessed as `m.name`. The `as` clause is **required**.

Neither form carries a modifier. The selective list is always parenthesized
and nonempty, including for one item. In canonical formatting, its opener
immediately follows the module path on the introducing line; multiline lists
follow the ordinary leading-comma
layout. `as` is contextual in qualified imports and selective aliases. Within a selection, standalone
`op` and `varop` are ordinary names. An `op` tag followed by a complete fixed
pattern or a `varop` tag followed by its delimiter pair selects operator syntax. A complete `macro name` selector selects a macro, optionally followed by
`as local`; an untagged name cannot select a macro. A standalone `macro`
or `macro as local` selects an ordinary identifier named `macro`, whereas
`macro as` selects a macro named `as`. Provider contents never choose between
these forms. Builtin block imports use their separate fixed forms below.

```kio
import runtime(panic);
import result(Result);
import people(Name);
import logger as lg;
import utils/list_ops(fold);
```

An ordinary selection may give its local spelling with `as`, for example
`import runtime(panic as fail, Handle as Resource);`. The destination preserves
the source identifier class: value names remain value names and type names
remain type names. This applies to ordinary functions, host functions, host
types, transparent aliases, nominal types, and literal aliases. A macro instead
requires `macro name`, optionally followed by `as local`; this tag selects
the declaration kind and preserves the same identity and alias rules. The import
retains the original declaration's identity, public type and kind, purity,
visibility, and declaration role. In particular, a renamed host type retains
the same literal-admission metadata; a renamed nominal type retains
partial type application. An import alias is not a wrapper, transparent type
declaration, or re-export.

Distinct local aliases of one declaration are allowed, as are distinct module
aliases of one module. Each local spelling is nevertheless introduced once:
`import m(f as first, f as second);` is valid, while
`import m(f as first, f as first);` and `import m(f as first, g as first);`
conflict. The same rule applies across clauses and against local declarations,
regardless of whether the selected declarations ultimately share a nominal
identity or implementation callable.

Deliberately absent: wildcard imports, hiding forms, and exports. Each ordinary post-lowering value, type, or module-alias name may be introduced once. Repeating a selective local name within one clause or across clauses is a name-resolution error, even when both occurrences select the same declaration. Repeating a qualified alias is likewise an error even when both imports name the same module. An import also conflicts with a local declaration of the same name. This rule applies uniformly to functions, host items, `type` declarations and the ordinary bindings produced when imported operators are lowered. The fixed compiler-block imports `import __intrinsics__;` and `import __comptime__;` are idempotent sets; repeating a block adds no introduction. Surface-only declaration registries such as `literal` and `op` are consumed before this environment exists and retain the resolution rules in their own sections.

An identity-only transparent alias is still a local introduction. Thus `import origin(T); import origin as source; pub type T = source.T;` is an error: the selective import and alias both introduce `T`. Omitting the selective import gives a valid re-export through the qualified provider. Alias chains and positional generic forwarding preserve their existing type identity and constructor/projector rules, but do not merge written introductions. Binding checks consume a validated `import` environment; this prerequisite does not establish diagnostic precedence when independent import and binding errors coexist.

The origin set is determined only by declarations and imports written in the consumer module (including bindings produced directly from operator imports). A selective import selects the triple `(source module, syntactically selected namespace, source spelling)` and separately introduces its local destination: `item` and `Item` never infer or widen one another. The compiler never scans an imported module for additional, unwritten candidates. Adding a declaration to a provider in another namespace therefore cannot introduce a new origin or change the meaning of an existing consumer import; only editing the consumer's own declaration or `import` list can do so.

**Compile-time helper import.** A module may import the compiler-provided reflection/helper surface with a single block import:

```kio
import __comptime__;
```

The block imports the complete compile-time helper surface. There is no qualified form, selective form, alias, wildcard, or per-name import for `__comptime__`. Public helper type and value names use the documented double-underscore spellings such as `__Type__`, `__Checked_term__`, `__type_view__`, `__term_call__`, and `__structural_recur__`. See [`specs/formal/elaboration.md`](formal/elaboration.md) for the compile-time boundary: user macros return reflected checked terms through the `__comptime__` ABI, and the substitute pass swaps those terms in at the Lowered → Prime boundary.

The exported names of `__comptime__` and `__intrinsics__` are disjoint.
Importing both blocks introduces no name conflict between them.

Function reflection observes semantic function types. The function arm of
`__type_view__(ct, typ)` is the pair `(domain & result)`, and
`__type_arrow__(ct, domain, result)` constructs `domain -> result`. The domain
is the complete logical argument packet. These helpers do not expose or
accept declaration value-parameter counts; value-parameter grouping remains
internal to function-body binding and host-interface generation. Reflected
kind arities continue to describe type variables, type constructors, and
polymorphic binders.

Macro type diagnostics may display the semantic expansion of a transparent
alias. Nominal types retain their names and identities. Diagnostic text is
opaque presentation; type comparisons use `__type_equal__`.

**Module cycles.** Value-level cycles between `.kio` module files are disallowed: if module A imports from module B, then B may not transitively import from A. Every **explicit `import`** counts as an edge — there is no type-only exemption: `import m(X);` always contributes the same `→ m` edge, including an `import` that brings a `host type` or `host fn` into scope. The package file's `bridge` block does not participate in this graph because it targets the contract surface rather than module bodies.

A consequence: **mutual cross-module type recursion is not expressible.** Two modules that reference each other's types with no runtime dependency (the `Expr`/`Stmt` split) would each `import` the other, forming a cycle, and are rejected. A type that must be mutually recursive across a module boundary has to be moved into a single module (where intra-module mutual recursion is fine).

Kio compiles in two strata: (1) **module bodies** — every `.kio` module's top-level values, types, and `host` declarations; (2) the **contract surface** — the transitive closure of `host` items, exports, and the types they reach, selected by the package file's `bridge` block. The value-cycle rule applies strictly within stratum 1.

## Open-world design

Kio's compilation model is **open-world**: adding definitions to a **module body** — new types, functions, or intended candidates — must never cause a different module to fail to compile. From a consumer module's perspective, such additions are monotonic: they can only enable things that did not previously typecheck, never break things that did.

Open-world is preserved **structurally**, not by bookkeeping. `into` and `onto` operate purely on the known source and target types, with no candidate pool at all.

**Scope.** Open-world is a property of `.kio` module bodies — its formal statement ([`formal/prime.md` § 7](formal/prime.md)) is scoped there. It does **not** cover the **contract surface**: the transitive closure of `host` items, exports, and the types those reach, as selected by the package file's `bridge` block. Changing anything inside that closure — adding or removing a `host` item, changing a type a bridged signature reaches — is a versioning event, not an open-world violation. The carve-out is keyed to the contract-surface closure, not to a syntactic block.

**Consumer import extension.** An accepted addition to a module's imports
preserves the validity and meaning of its existing function bodies. Admission
checks the complete import and declaration headers, including namespace
collisions and operator grammar; it does not inspect
function bodies to discover whether a conflict is used. A conflicting import
addition is rejected even if no body mentions the conflicting name or syntax.
The guarantee preserves grouping, resolved identities, inferred types and
meaning, not diagnostics, completion menus, dependency inventories, generated
bytes or package host/export contracts.

**Literal inference preserves meaning.** A role-bearing type admits literals
of its category but does not select itself by being visible. A literal's
explicit annotation or concrete contextual type determines its carrier within
the existing finite local inference boundary. Adding another role-bearing type
cannot participate in that inference or change a successful result. No rule
counts visible roles or searches an ambient candidate pool.

**Host items preserve open-world.** A module's `host` items are part of its contract surface — peers of a function signature. Adding or removing a `host` declaration is a versioning event, not an ordinary module-body edit, and the break (a newly-required host capability) lands on the host at its recompile against the regenerated interface, caught by the type system at the boundary. No ordinary `pub fn` / `type` added to any other module body can change which host capabilities an existing bridge requires.

**Purity preserves open-world.** A pure function is checked only against executable references already resolved in its body. The checker consults an ordinary function declaration's written `pure` metadata; type paths are irrelevant, and it never scans a growing candidate pool. A macro call likewise names one explicitly imported macro, whose implementation and captures are fixed by that declaration; final Prime validation checks only the term that call produced. Adding an unrelated declaration to any module body therefore cannot make an existing pure function impure or change its elaboration. Changing the `pure` modifier on a referenced public function changes that function's contract, the same category as changing its signature.

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
host fn print(s: String);
```

```kio
// app/main.kio
module app/main;

import app(String, print);

pub fn main() { print("hello\n"[String]) }
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
host fn fold[A][B](f: (B & A) -> B, seed: B, xs: Seq[A]) -> B;
```

**Host types are opaque.** A `host type` is treated as an abstract name inside the package's modules: the compiler knows its identity and its ordinary kind-`*` type parameters, but never its structural shape. A host type cannot bind a higher-kinded parameter; abstracting over a type constructor remains a nominal type or `fn` capability. Structural-typing equality is suspended at the host boundary on purpose, so a package's reasoning about its own types cannot depend on which host supplies them. Neither `into` nor `onto` crosses host type boundaries.

**Role-assigned host types.** A `host type` declaration may be annotated with `role(...)` to designate the type's **role** in the language. The currently-recognized roles are the Rust-shaped sized numerics plus `bool` and `str`:

```kio
host type I32    role(i32);
host type U64    role(u64);
host type F64    role(f64);
host type Bool   role(bool);
host type String role(str);
```

The full role list: `i8`, `i16`, `i32`, `i64`, `i128`, `u8`, `u16`, `u32`,
`u64`, `u128`, `f32`, `f64`, `bool`, `str`. These form four **literal categories**:
integer (all signed and unsigned widths), float (`f32`, `f64`), Boolean, and
string. A role-bearing host type carries exactly one role; multiple types may
carry the same role. `role` applies only to host types and establishes literal
admission, not an implicit type-selection rule. The spelling of a numeric literal fixes its
integer or float category independently of its expected or explicit type;
see [Literals](#literals).

Literal carriers are determined by explicit literal annotations or concrete
contextual inference; see [Literals](#literals). Merely declaring or importing
a role-bearing type does not make an otherwise unresolved literal typable.

### Embeddability and portability

Kio packages are **embeddable**: a package is a plug-in, dynamic component, or script that runs inside a host, not a standalone program. The language has no package-level concept of a `main` binary; an exported item named `main` is just a common entry-point name, not a language-level one. The host decides what exports to invoke, what to pass in for host items, and how to run the result.

This "start from nothing" stance is what makes Kio ultra-portable: the language itself has no dependency on any particular backend, so the same source can be transpiled to any backend for which a suitable host is written. The *backend* (which fixes the artifact format the transpiler emits — e.g., C, native, a bytecode, a host language's source) is chosen separately from the host, and the two are not in one-to-one correspondence.

## Lexical structure

Source-level basics: naming, identifiers, comments, keywords, and literals. Syntax for specific language features is documented alongside their semantics in the relevant sections.

### Naming conventions

Kio identifier spellings are enforced by the compiler: a violation is a parse-error category failure (exit code `11`; see [`exit-codes.md`](exit-codes.md)).

Ignoring leading and trailing underscore affixes, each word contains one or more ASCII letters followed by zero or more digits. Exactly one underscore separates adjacent words. Thus `a1_b2`, `foo123_bar4`, `sha256`, and `p0` are valid; `a1b`, `foo_123`, and `foo__bar` are invalid. Any number of trailing underscores is admitted.

- **Type names and type parameters** match `_?[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*`: the first letter is uppercase, and every later letter is lowercase. This applies to aliases, host types, nominal entries, and type binders. `I32`, `Foo123_bar4`, `_Logger`, and `_Event_stream` are valid type names.
- **Values** match `_?[a-z]+[0-9]*(?:_[a-z]+[0-9]*)*_*`: every letter is lowercase. Functions, macros, value binders, and ordinary imported value names obey this rule.
- **Modules, subdirectories, packages, and dependency local names** obey the value rule. Module paths and package, dependency, lock, and signature filename stems retain their exact source identity.
- **Leading underscores** have the same rule for both namespaces. Zero or one leading underscore is user space. A single leading underscore is part of the exact identity and permits an unused binding; `Foo` and `_Foo`, or `foo` and `_foo`, are distinct and remain ordinarily referenceable. It grants no different typing, visibility, or capability.
- **Reserved prefixes** begin with two or more underscores. They use the same word and case grammar, with grammatical patterns `_*[a-z]+[0-9]*(?:_[a-z]+[0-9]*)*_*` and `_*[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*`. User declarations reject this prefix class. The intrinsic and compile-time import targets and their documented declarations use reserved spellings such as `__intrinsics__` and `__Type__`. A reserved spelling supplies hygiene only: a reference still requires an ordinary exact binding and its applicable visibility and phase validation.
- **Every name contains a letter.** Pure underscore runs and underscore/digit-only spellings are not names. Bare `_` is admitted separately as a wildcard or inference placeholder, and `_`, `__`, and `___` have the separately specified operator-slot roles.

The lexer retains one broad `IDENT` token, `[A-Za-z_][A-Za-z0-9_]*`; role validation diagnoses a malformed spelling as one token. Invalid-name diagnostics identify the expected role and violated rule and carry only replacements valid for that role. A repair without a trustworthy resolved identity explicitly edits only that token. An identity-preserving rename requires the ordinary collision, capture, and document-version checks.

### Keywords and reserved names

Every keyword in Kio is **contextual**: each keyword-shaped word is lexed as an ordinary identifier and the parser routes it to its keyword role only at positions that demand it. There is no "always reserved" tier. Boolean literals are spelled `.t` / `.f`, so `true` and `false` are ordinary value identifiers. Placeholder-lambda intros such as `.x.` are punctuation-led forms rather than keyword-shaped words (see [Placeholder lambdas](#placeholder-lambdas)).

What "contextual" buys: adding a new keyword later cannot break existing user code that uses the word as an identifier, and the lexer stays small and uncoupled from grammar evolution. What it costs: at the listed positions below, the named word does **bind** to its keyword role — a user who tries to use one as a value identifier at that position sees a parse error pointing at the position rather than a "this is a reserved word" diagnostic.

Per-keyword semantics are documented at the declaration sites; this is the consolidated reference.

- **Item-leading words** (recognized at module-level item position, after declaration modifiers where admitted): `fn`, `rec`, `type`, `literal`, `equiv`, `macro`, `op`, `varop`. (`equiv` is not allowed after `pub`; `rec` is allowed after `pub` in the singleton forms `pub rec type` and `pub rec(loop) fn`; either braced `rec` group takes no leading visibility, so `pub` goes on each member; only an ordinary `fn` is allowed after `pure`.) Outside these positions, ordinary identifiers.
- **Module-header words** (recognized at the start of a `*.kio` module file): `module` for a module declaration.
- **Recursive-call introducer**: `recur` introduces a recursive callee, with optional call annotations before that callee: `recur step(x)` or `recur(cont) step(x)`. Without a following callee, a bare `recur` reference or an ordinary call such as `recur(x)` uses the ordinary identifier. Both `rec` and `recur` remain ordinary names outside their grammatical positions; `fn id(rec: .) { rec }` is an ordinary function.
- **Statement-leading word**: `let` introduces a binding only when the following tokens prove the binding form. Macro names such as `if`, `do`, `scope`, and `match` are ordinary identifiers; their direct calls become macro calls only through ordinary lexical ownership of an explicit macro binding. Continuation labels such as `else` are ordinary value-shaped names in the block-call grammar. Dot-lambdas use punctuation-led forms (`.(...) { ... }` and `.stem. { ... }`).
- **Inside macro declarations**: `impl`, `captures`, and `trailing` introduce entries; `product`, `thunk`, and `sequence` select the descriptor after `trailing`.
- **Import-leading and import-context words**: `import` begins a module's
  leading import clause; `as` introduces a module alias. Inside a selective
  list, `op` tags a fixed operator grammar and `varop` tags a variadic delimiter
  pair, while `macro` tags a named macro selection once the complete selector
  is known.
  These spellings remain ordinary names elsewhere.
- **Inside operator declarations**: a `varop` body contains exactly one of
  `foldl`, `foldr`, `foldl1`, or `foldr1` and optionally `finalize`, in either
  order; a fixed operator body contains exactly one `impl` entry.
- **At the start of a package file**: `package <name>;` declares the package identity.
- **Inside a package file** (`<name>.pkg.kio`): `bridge` leads the package's module-glob block.
- **Inside a `build { ... }` block** (in `<name>.pkg.kio`): `build` opens the block; `cache`, `docs`, and `target` lead its declarations. A target opens with `target <backend> { … }` or `target (<backend>, "<name>") { … }`; the backend is a bare identifier and the explicit target name is a string. See [Build target files](package.md#build-target-files).
- **Inside a nominal member body**: `mk` and `get` at item positions.
- **Inside a host type declaration**: `role` opens the `role(...)` annotation.
- **Declaration modifier position** (top-level declarations and nominal member items): `pub`; on an ordinary module-body `fn` only, `pure` as well. `host` is a declaration modifier only when immediately followed by `type` or `fn` (optionally with a redundant `pub`); elsewhere it is an ordinary identifier.

A user-declared identifier may freely have any of these names — `fn id[A](x: A) -> A { x }` declares a value named `id`, and `fn pub()` would declare one named `pub`. The contextual recognition only fires at the positions above; in any other position the same word is an ordinary identifier reference.

- **Reserved intrinsic values** — provided by the compiler, not imported by default; brought in as a block by `import __intrinsics__;`: `__left__`, `__right__`, `__either__`, `__pair__`, `__fst__`, `__snd__`, `__if_then_else__`, `__and__`, `__or__`, `__absurd__`.
- **Reserved import targets** — `__intrinsics__`, recognized only inside `import __intrinsics__;`, and `__comptime__`, recognized only inside `import __comptime__;`. Neither is ever a regular module name.

Two type-level literal tokens — `.` (the unit type) and `!` (the bottom type) — are grammar tokens rather than identifiers, so they aren't user-declared names but are worth listing alongside the reserved set.

This list is exhaustive: every reserved word or name recognized by the compiler appears above. The user-identifier grammar and the `__name__`-prefix reservation are documented in [Naming conventions](#naming-conventions) above.

### Comma-separated lists and operator chains

Every comma-separated production in Kio's grammar — value-parameter groups, tuple literals, value-call argument lists, and selective `import` name lists — accepts **any number of commas in any position**: leading runs (before the first item), interior runs (between items, beyond the single separator), and trailing runs (after the last item). Equivalent framing: items are separated by *one or more* commas, with optional comma runs at either edge. All shapes collapse to the same AST as the canonical one-comma-per-separator layout. Type-binder and type-supply brackets are not comma lists: each contains exactly one entry, so `[A, B]`, `f[A, B]`, and their edge-comma variants are rejected. `(,,, a ,,, b ,,,)` and `(a, b)` parse to the same tuple literal; `(, a ,)` parses as grouping around `a`, not as a unary tuple; `(,,,)` parses as `()`. `fn three( , x: A , y: B , z: C , )` and `fn three(x: A, y: B, z: C)` parse to the same value group. Function-type value groups are not comma-separated: write `(A & B) -> C`, not `(A, B) -> C`. The formatter relies on the comma rule for the leading-comma multi-line layout where commas are part of the canonical production.

Block-like clause bodies use the same semicolon separator rule: repeated semicolons do not create empty clauses, and a final semicolon after the final expression is ignored. This applies to expression blocks, neutral macro blocks, and keyed/member blocks such as nominal members, `build`, `target`, and `docs`. A resolved descriptor determines a neutral block's meaning: empty product and thunk blocks produce unit and a unit-returning thunk respectively; a sequence requires an explicit final expression.

The type chains `&` (product) and `|` (sum) follow the symmetric rule: a chain accepts **any number of operators in any position** — leading, interior, trailing — all collapse to the same right-associated chain. `&&& A &&& B &&&` (when terminated by a non-Type token like `,`, `)`, `;`, `=`, `{`, `}`) and `A & B` parse identically; same for `|`. The rule applies in both unparenthesized and parenthesized form: `( &&& A &&& B &&& )` and `(A & B)` parse identically. Empty and unary chains use the algebraic identities: product `&` with no items (`&` or `(&)`) is `.`, sum `|` with no items (`|` or `(|)`) is `!`, and a one-item chain (`& A`, `(A &)`, `| A`, `(A |)`) is just `A`. Mixing `&` and `|` in one chain still requires explicit parentheses (per [Anonymous sum and product types](#anonymous-sum-and-product-types)) — `A & B | C` is a parse error, and the user writes `(A & B) | C` or `A & (B | C)` to disambiguate. The formatter's multi-line emit for a chain mirrors the leading-comma layout for lists: each chain item lives on its own line at +2 indent, prefixed with `&` (or `|`) followed by a space — the leading-operator A1 analogue. Arrow `->` is not a variadic delimiter. Product/sum chains directly on either side of a type-expression arrow must be parenthesized: `(A & B) -> C`, `A -> (B & C)` and `A -> (B | C)`. A named function or lambda's own return annotation retains bare chains; see [Function types](#function-types).

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
`literal`, `macro`, `op`, `host type`, `host fn`) —
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
- **`macro`** — user-defined macros selected explicitly and called by local name (see [User-defined macros](#user-defined-macros)).
- **`op` and `varop`** — user-defined fixed and variadic operator bindings (see [Operators](#operators)).
- **`equiv`** — compile-time equivalence checks consumed by `kio test` (see [Equivalence claims](#equivalence-claims-equiv)).

Top-level value bindings use the declaration forms above; arbitrary top-level value expressions are not Kio items.

Top-level bindings are private by default; prefix with **`pub`** to export. An ordinary `fn` may also carry **`pure`** as described in [Module system](#module-system).

```kio
pub fn identity[T](x: T) -> T { x }

host type String role(str);

pub fn greeting() -> String { "hello" }
```

A `fn` has at least one type-binder or value-parameter group. A type-only
signature needs no parentheses; a zero-parameter value stage is written `()`.
See [Function definitions](#function-definitions). `let` is never used at the
top level — it is reserved for local naming inside blocks.

**Order-sensitive visibility.** Declarations, including `host type` and `host fn`, are processed top-to-bottom. A declaration sees only names introduced *above* it in the same file, plus names brought in by `import`. A host declaration introduces its name after its own signature is checked; host types in that signature must already be in scope. There is no implicit forward reference within a file. The explicit exceptions are the capability-free recursive-data scopes (`rec type` and bare `rec { ... }` type groups; see [Type declarations](#type-declarations)) and `rec(loop)` function groups. A singleton data declaration sees only the heads its own declaration contributes. A bare type group sees exactly its written member heads. A `rec(loop)` group gives its member bodies access to the group's member names only through explicit `recur name(...)` calls.

A direct consequence: an ordinary **`fn` cannot refer to itself**, because its own name is not yet in scope when its body is checked. Mutual recursion between ordinary `fn`s is equally impossible. Use an explicit `rec(loop)` group when term recursion is intended. The same order-sensitivity rules out self-referential type aliases.

### Type and literal aliases

Kio has two transparent alias forms:

- **`type`** names a structural type expression.
- **`literal`** names one bare literal token for reuse in expression position.

Both forms end with a semicolon and are private by default; prefix with **`pub`** to export.

#### Type aliases

A **type alias** gives a structural type expression a name. Two forms:

- **Nullary** — `type Name = body;` names the `Type` `body` as `Name`.
- **Parametric** — `type Name[A][B] = body;` names a family of types parameterized by one or more type parameters. Each reference supplies exactly one argument per declared parameter: `Name[X][ Y]` is a type for `type Name[A][B] = …`, while bare `Name` and undersaturated `Name[X]` are type errors. Underapplication cannot retain the alias declaration's own binders as residual constructor slots. This restriction is what rules out anonymous type-level abstraction — the piece separating Kio's Church-style fragment of F-ω from full F-ω (see [Type parameters](#type-parameters)).

```kio
host type String role(str);

type Logger = String -> String;
pub type Endomorphism[A] = A -> A;
```

The typer unfolds a type alias structurally wherever it appears; type aliases survive every compilation phase. **An ordinary type alias is non-recursive.** A transparent alias may be a member of a genuinely mutual `rec { ... }` type group only when every cycle in the component crosses a sibling nominal boundary and the alias-only dependency subgraph is acyclic. Kio has no anonymous recursive type expressions or alias-only fixed points: every recursive type component is grounded at an explicit nominal declaration. Parametric declarations behave as **surface sugar** at the parameter level: `Name[X]…` substitutes the arguments into `body`, one argument per bracket.

A transparent alias retains a terminal nominal type's member namespace only when
every alias edge is a fully saturated positional identity application with the
same effective binder kinds. For example, after
`type Wrapped[A] = Box[A];`, `Wrapped.mk` denotes the exact constructor
declared by `Box`; the result remains the `Box[A]` nominal. Partial,
reordered, structural, cyclic, and missing-target aliases do not expose
constructor or projector members. The alias head must already be in lexical
scope, and visibility of the written alias and terminal member is checked
separately. Definition, references, and rename therefore associate the written
head with the alias declaration and the leaf with the terminal member. The
alias does not mint a new member or change reduction.

#### Literal aliases

*Surface only; desugars to Kio'.*

A **literal alias** binds a lowercase name to one bare literal token. A bare reference expands to the stored literal; `name[Type]` expands to the stored literal carrying the written literal-call annotation. Expansion happens at the Surface → Desugared boundary, before Kio' reaches the resolver or typer.

```kio
literal max_size = 100;
literal greeting = "hello";

fn cap(n: I32) -> I32 { min_i32(n, max_size[I32]) }
fn hello() -> String { greeting[String] }
```

After expansion, `hello` is identical to one where `"hello"[String]` is written directly. A literal alias declaration itself carries no type; the literal is typed only after expansion, using its explicit annotation or
concrete context at the use site. The provider contributes the token, not a
preselected carrier.

**Restrictions** (parser-enforced):

- The declaration name follows the value-name rule.
- The right-hand side is exactly one literal token: string, integer, float, or bool.
- Adjacent string-literal folding does not apply in a `literal` declaration body; write the complete string in one token.
- A literal alias is referenced bare or with one singleton carrier annotation,
  `x[T]`. Value calls `x()` and `x(y)` are errors. Empty `x[]` and comma-list
  `x[A, B]` brackets are rejected, and a second bracket does not add another
  carrier annotation or make the literal polymorphic.
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

**Shadowing.** A local binder (`let`, `fn` value-parameter, `match` clause parameter) with the same name as a literal alias shadows the alias within the binder's scope; references to that name inside the scope are the local, not the alias.

**Hygiene.** A literal alias body has no free names. Imported literal aliases expand from the exported declaration's stored literal token, so adding declarations to a module body cannot change the meaning of an existing consumer's literal-alias expansion.

**Why not a general value-binding form?** A literal alias is deliberately narrower than a transparent source rewrite for arbitrary expressions. Users wanting an evaluation-bearing "constant" or shared computation reach for a `fn` — already exists, already pins evaluation semantics, already runs at well-defined sites (every call is explicit).

### Type declarations

A `type` alias names an existing type. A nominal entry introduces a distinct
type identity around one payload, with the fixed constructor `mk` and
projector `get` as members of that type:

```kio
pub type { Celsius: F64; }
type { Box[A]: A; }
```

`Celsius.mk : F64 -> Celsius` constructs the nominal value, and `Celsius.get :
Celsius -> F64` projects its payload. These members are not free module values;
importing the type makes its accessible members reachable through that type.
Each nominal entry is distinct from its payload and every other nominal entry.
Crossing that boundary requires its constructor or projector: there is no
implicit unfolding or structural coercion to the payload.

An omitted entry body gives both members the entry's visibility. A written body
must contain both `mk` and `get`; each unmarked member is private and each may
independently carry `pub` or `pub(path)`. The effective visibility is the
intersection with the nominal type's visibility. For example,
`pub type { Token: I32 { mk; pub get; }; }` exposes projection but not
construction. See [Visibility](#module-system).

A surface declaration can introduce several entries in one anonymous group or
name a product or sum composed from groups and existing types:

```kio
type { Name: String; Age: I32; }
type Message = { Text: String; } | { Code: I32; Fatal: .; };
type Audit = Text & { Timestamp: I32; };
type Boxed[A] = { Box[A]: A; };
```

`Message` is a transparent alias for `Text | (Code & Fatal)`; it is not a
new nominal wrapper. `Audit` combines the existing `Text` with the new
`Timestamp`. `Boxed[A]` aliases `Box[A]`; the entry declares its own `[A]`
binder independently of the alias binder. Each entry's binders are local to
that entry. Payloads
and ordinary leaves are ordinary types, not nested nominal declarations.
Kio' admits a single nominal entry per declaration and ordinary aliases; the
surface conveniences expand to those forms. See [`prime.md`](prime.md).

**Recursive types.** Recursive data scope is always explicit. A self-recursive nominal uses `rec type`; the marker puts that declaration's contributed heads in scope in its payload:

```kio
rec type { List[A]: . | (A & List[A]); }

rec type Tree = { Leaf: I32; } | { Branch: Tree & Tree; };
```

A compact recursive declaration creates one atomic scope for all its written
nominal heads and its named alias head, when present. It does not capture
unrelated later declarations. Its resulting cyclic components must each cross
a nominal boundary and satisfy strict positivity; acyclic entries do not join a
mutual component solely because they share the declaration.

The nominal declaration is an iso-recursive boundary by construction. At the value level, crossing it always happens through the declared constructor and projector — there is no implicit unfolding, and a type and its one-step unfolding are **not** the same type until you cross the boundary explicitly.

Mutual recursion uses one capability-free bare group. Every member head is in scope throughout the braces; each member carries its own visibility and documentation:

```kio
rec {
  pub type Tree[A] = Branch[A];

  pub type { Branch[A]: . | (A & Tree[A] & Tree[A]); }
}
```

The group must contain at least two unmarked type members and denote exactly one genuinely mutual cyclic strongly connected component after declaration lowering. Every written member must contribute to that component. An acyclic group, a one-way helper, multiple independent components, a one-member brace group, or an alias-only cycle is rejected; move an acyclic dependency before its consumer instead. Singleton `rec type` declarations are top-level forms, not nested group members.

Strict positivity is a property of the explicit recursive component. A
recursive use site is every occurrence in a nominal payload that reaches a
nominal member of the component, directly or through its transparent aliases.
Every such use site must sit in a **strictly-positive position**: the
**composed variance** from the payload root down to the occurrence must be `+`
(covariant) or `0` (unused at that position).

Variance composes through every type connective: arrow LHS flips, arrow RHS
preserves, and `&` / `|` preserve. Entering a parametric nominal slot composes
with the slot's variance, computed by mutual fixpoint over the component so
transitive contravariance through chains of nominal types is caught. Host-type
parameter slots are conservatively **invariant**. A recursive occurrence
through such a slot fails unless an unused (phantom) parameter on its path
makes the total composed variance `0`.

A higher-kinded type variable has no declared variance for its argument slots.
Each slot is therefore conservatively invariant when composing recursive
occurrences through an application headed by that variable. Knowing the kind
alone does not establish covariance. For example,
`rec type { N[*F]: F[N[F]]; }` is rejected:
the recursive occurrence is an argument of the abstract `F`. A known nominal
head instead uses its computed parameter variance, and transparent aliases
preserve that resolved structure.

A negative occurrence of a nominal outside the written component stays
admissible: it names a fixed inductive type rather than part of this recursion.
This is what keeps Kio's recursion well-founded; see
[`formal/prime.md` § 2.5](formal/prime.md#25-strict-positivity).

The marker or group is mandatory exactly when it supplies recursive scope. An omitted singleton marker or omitted mutual group receives a precise diagnostic naming the participating declarations; a redundant marker or non-minimal group is also rejected. The editor offers a safe marker insertion or group wrap only when one unambiguous edit preserves declaration order.

These scopes are closed by the written declaration boundary. Imports and earlier declarations remain visible, but neither singleton nor group sees an unrelated later declaration. Adding an unrelated module declaration therefore cannot retarget a reference or change an already-valid program; the candidates inside the scope are exactly its written heads. After the closing declaration, all contributed names enter ordinary source-ordered module scope.

Because the iso-recursive boundary is purely a typing property of the nominal declaration, a transpiler is free to realize it however the backend dictates — e.g., boxing the payload on size-sensitive backends, or leaving it inline where the backend's layout permits.

**Folding over a recursive type.** Kio provides no auto-generated fold for recursive nominal types. Ordinary `fn`s cannot be self-recursive; user code that walks a recursive payload either uses an explicit [`rec(loop)` group](#recursive-functions) backed by a host function or calls that function directly.

### Existential type binders

An **existential type binder** is written as `<name>` after a nominal entry's
universal parameters and before the `:` introducing its payload. Its scope is
the payload type. Each binder must occur at least once in the payload; a
phantom existential is rejected in the type-error category. There is no
standalone existential type expression.

```kio
type { Pack[A] <U>: A & U; }
type { Box[A] <L> <R>: L & R & A; }
type { Pack_unit <U>: (U -> .) & U; }
```

The nominal arity counts only universals: `Pack[A] <U>` is referenced as
`Pack[A]`. Construction infers existential arguments from the payload;
repeated occurrences must agree. Constructing `Pack` from `I32 & String`
produces `Pack[I32]`.

**Existential-opening `let` sugar.** The block-statement form

```kio
let .(<U_1> … <U_n> x) = e;
rest
```

is parser-level surface sugar for

```kio
e[_](.[U_1]…[U_n](x) { rest })
```

— the RHS `e` is applied to a continuation that opens the existential. `e` must evaluate to the curried CPS function returned by an existential-bearing nominal's projector (see the **Projection through a nominal type** paragraph below). The leading `_` lets the typer infer the result type `R` from the continuation's body. The witnesses `<U_1>, …, <U_n>` are opaque inside the rest of the block (the System F binder discipline enforces the escape check); the value binder `x` carries the projector's payload type with each `<U_i>` substituted in. The sugar is surface-only — Kio' programs spell out the CPS call directly.

The payload can instead be destructured by a typed parameter pattern, for example `let .(<U> (left: U, right: U)) = e;`. The witnesses scope over the pattern's annotations and the remaining block; projecting the components does not weaken their escape check. Pattern recognition uses the existing parameter as-pattern disambiguation: `(left: _, right: _)` is a pattern, while the untyped opening `(left, right)` is not admitted. Nested patterns and wildcard components follow the ordinary parameter-pattern rules. This is equivalent to opening into a named payload and then destructuring that payload with an ordinary rich-pattern let.

**Projection through a nominal type.** Existential-bearing nominal types carry a **CPS projector**: the projector scheme is

```kio
N.get : [Universals] N[Universals] -> [R] ([Existentials] payload -> R) -> R
```

— a curried form whose outer call takes only universals and the nominal value, returning the inner polymorphic CPS function. Instantiating the inner function with `[Result]` and applying `(continuation)` opens the existential: the continuation receives the payload under a System F universal binder that scopes the existential witnesses, so they cannot leak into the result `R`. Most consumers use the [`let .(<U> x) = e;` sugar](#existential-type-binders) at the consumption site; the explicit two-step call is always available for code that needs it. Non-existential projectors return the payload directly, with scheme `[Universals] N[Universals] -> payload`.

### Structural typing

Kio's type system is **structural** throughout, with two deliberate exceptions — at nominal boundaries and at the host boundary. Three consequences follow:

- **Transparent `type` declarations are aliases.** `type A = X;` and `type B = X;` produce the same type; a value of `A` is interchangeable with a value of `B`. A type alias never mints a fresh nominal point distinct from its right-hand side.
- **Nominal entries mint identity.** Each entry introduces a type distinct from its payload and from every other nominal entry, identified by its declaration site.
- **`host type` declarations are opaque.** See the host boundary note below.

An alias body is interpreted in the module that declares it. Once a type reference is qualified to a module identity, type comparison unfolds it only through that module: a caller-local alias with the same leaf cannot reinterpret a callee signature. Thus if module `dep` declares `type Flag = types.Actual`, a caller's unrelated `type Actual = types.Other` has no effect on `dep.Flag`.

For example, `type Celsius = Float;` makes `Celsius` and `Float` the same
type. In contrast, `type { Celsius: Float; }` introduces a distinct type;
`Celsius.mk(x)` and `Celsius.get(value)` cross its boundary.

**The host boundary exception.** `host type` declarations are opaque: the package knows a host type's identity and, for a roleless declaration, its type parameters, but never its structural shape, so a host type is never structurally interchangeable with any package type. A role-bearing host type is always nullary. Opaque here means "not structurally interchangeable," not "nominal at the language level"; inside a single package, every non-host type is either pure-structural (products, sums, type aliases) or introduced by a nominal entry.

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
their macro and may relate them through its public contract.

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
- A **numbered binding** — `let .stem. = expr;`, with its unannotated slots determined by the highest owned numbered reference in the remaining block (see [Numbered bindings](#numbered-bindings)).
- An **existential-opening binding** — `let .(<U> x) = expr;`, as specified in [Existential type binders](#existential-type-binders).
- An **expression statement** — a non-final expression clause. It must have type `.`; deliberately discarding any other type requires `let _ = expr;`. A final expression remains the block's result even when followed by `;`.

`let` is contextual at statement start: a following name or wildcard starts a simple binding, `.` followed by `(` starts a rich binding, and `.stem.` starts a numbered binding. Rich patterns are always parenthesized, including existential openings; numbered stems use adjacent dots without parentheses. There is no keyword-free binding form. A following `(` without the dot instead leaves `let` as an ordinary value name, so `let(f)` is a call; `let(f) <- x` is an ordinary expression when `<-` is declared as an operator. Qualified `let.x` references retain their ordinary meaning.

A block that ends in a statement (no trailing expression) is a compile-time error. A trailing semicolon is accepted only after a real final expression.

Destructuring `let` is a surface-only form. The Surface → Desugared boundary eliminates it by the same pattern-elimination machinery the parameter-pattern rewrite uses (see [Parameter patterns](#parameter-patterns) § Desugaring). Both forms reduce to a `let <outer> = <expr>;` followed by generated per-slot `let` bindings that project with `__fst__` / `__snd__`, where `<outer>` is the user-given `<name>` for the as-pattern form and a synthetic fresh name for the bare form. Wildcards emit no local binding. From the post-desugar AST onward, the destructuring-let leaves no trace beyond ordinary lets and projection calls.

### Numbered bindings

`let .stem. = expr;` introduces an unannotated flat pattern with slots `stem1`
through `stemN`. As with [placeholder lambdas](#placeholder-lambdas), the stem
is an ordinary value-binding name ending in an ASCII letter; both dots and
the stem are adjacent. Indices are positive decimal integers without leading
zeroes and fit in `u32`. The remaining block must contain at least one owned
reference, and its highest owned index determines `N`, including unused lower
slots. An index does not select an independently counted leaf of the RHS type.
The final slot receives the remaining product tail: with `N = 1` it receives
the whole value, with `N = 2` it is the second component of the right-associated
product, and with `N = 3` it is the third component.

The initializer evaluates once, synthesizes its type before the continuation
is checked, and is outside the new family's scope. Reference ownership follows
the placeholder rule over the written lexical continuation, including ordinary
binders, qualified paths, type positions, labels, and dedicated callee positions.
An actual nested placeholder lambda starts a fresh owner and its body is not
scanned. An explicit lambda retains the family subject to its own binders.
A later numbered binding of the same stem shadows the entire previous family
in its continuation, while its initializer may use the previous family.
A different-stem numbered binding leaves earlier families in scope.

Thus `let .x. = a; print(x2); let .x. = b; print(x3); ()` gives the first
binding two slots and the second three. In
`let .x. = a; let .y. = b; pair(x1, y1)`, both families remain visible.

`let .stem. <- expr;` uses the same numbered pattern in a sequence block;
the resolved sequence descriptor and ordinary bind-call type determine its
payload, exactly as for an unannotated `let .(stem1, ..., stemN) <- expr;`.
Neither form admits an annotation on the stem. Numbered bindings lower to
hygienic ordinary pattern bindings before Kio'; no owner or index rule survives
that boundary. Ownership is fixed before operator expansion, so an operator's
implementation name cannot introduce a new reference. Parsing, ownership and
arity depend only on written local syntax: adding a module declaration cannot
change them.

### Function definitions

```kio
pub fn compose[A][B][C](f: A -> B, g: B -> C) -> A -> C {
  .(x) { g(f(x)) }
}
```

- A signature has one or more ordered type-binder or value-parameter groups. A type-only signature is valid: `fn nil[A] -> List[A] { body }`. A declaration with no group, such as `fn constant -> String { "hi" }`, is rejected.
- Value parameters are comma-separated inside `( )`. A zero-parameter value stage is written `()`: `fn constant() -> String { "hi" }`. It is not implied by a type-only signature.
- Each written value parameter is `name: T` for some type `T`; annotations are mandatory.
- Return type follows `->`. The `-> Type` annotation may be **elided**, in which case the return type defaults to `.` — `fn say_hi() { print("hi") }` is equivalent to `fn say_hi() -> . { print("hi") }`. The body is then type-checked against the defaulted return type; a non-unit body is a type error. This default also applies to type-only named declarations. Anonymous functions instead infer an omitted result from their body or expected type (see [Anonymous functions](#anonymous-functions)).
- Body is a block.
- There is **no automatic currying**. A function of three parameters is not a function returning a function returning a function.

Type-only signatures retain the ordinary kind and purity rules. The complete
type `[A] List[A]` is a value type. With a higher-kinded binder, the existing
complete-function-scheme rule still applies: `[*F] .` is rejected, while
`[*F] F[.] -> F[.]` is admitted, whether its value arrow comes from a written
value group or the result type. See [Higher-kinded types](#higher-kinded-types).
Host imports, `rec(loop)` members, and parameterized `equiv` declarations
require at least one written value group.

### Recursive functions

Surface Kio admits explicit recursion through `rec(loop)`. The name inside the parentheses is an ordinary value path that must resolve to a function with the loop shape:

```kio
loop[S][R](step: S -> (S | R), state: S) -> R
```

There is no privileged built-in named `loop`: hosts, package files, root modules, or earlier module declarations provide a normal function, and `rec(loop)` resolves that path by the usual rules.
The selected path admits transparent repeated grouping: `rec((loop))` names
the same capability as `rec(loop)`.

Escaping groups require a re-entry-capable provider whose step also receives
an ordinary `S -> R` callback:

```text
loop[S][R](step: ((S -> R) & S) -> (S | R), state: S) -> R
```

The transition sum still means next state on the left and final result on the
right. The host calls `step(reenter, state)`; `reenter(next_state)` runs that
same state machine and returns its result, including after the creating step
has returned. Ordinary groups retain the simple provider contract above. A
general provider can serve an ordinary group through an ordinary adapter that
ignores the step's re-entry argument and forwards the state; the compiler does
not dispatch on the provider's type or give any provider privileged identity.

Single-function recursion uses the shorthand:

```kio
pub rec(loop) fn countdown(n: I32) -> I32 {
  if(leq_i32(n, 0)) {
    n
  } else {
    recur countdown(sub_i32(n, 1))
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
  pub fn even(n: I32) -> Bool { if(eq_i32(n, 0)) { .t } else { recur odd(sub_i32(n, 1)) } };
  fn odd(n: I32) -> Bool { if(eq_i32(n, 0)) { .f } else { recur even(sub_i32(n, 1)) } }
}
```

The braces group mutually recursive bodies; they do not introduce a visibility
scope. Every member produces an ordinary module function with the member's
visibility, exactly like the independently visible constructor and projector
members of a nominal declaration: no modifier is module-private, `pub(path)` is scoped to
that module subtree, and bare `pub` is public. A later declaration in the same
module can therefore call any member as an ordinary function. The member names
also occupy the ordinary module namespace, so conflicts with another top-level
declaration receive the ordinary duplicate-name diagnostic.

Inside a member body, calls to members of that group must be written with the
`recur` marker. A call to a member name without `recur` is rejected, even for
direct self-recursion. Outside the member bodies the lowered wrappers are called
normally. The call marker carries the optimization/shape facts the compiler
relies on:

- `recur name(...)` — monomorphic, tail-position recursive call.
- `recur(poly) name[T](...)` — tail-position recursive call whose type arguments differ from the current member instantiation.
- `recur(cont) name(...)` — monomorphic recursive call in a non-tail position.
- `recur(poly, cont) name[T](...)` — type-changing recursive call in a non-tail position.
- `recur(escape) name(...)` — a recursive call captured by an ordinary closure that may outlive the current loop step.
- `recur(poly, escape) name[T](...)` — an escaping call at a different type instantiation.

This is a source-visible lowering contract: the author states the shape being requested, and the compiler checks that the annotation is both necessary and sufficient. `escape` entails `cont`; source may write both, but `kio fmt` removes the entailed `cont`. `poly` remains orthogonal and comes first in canonical combined annotations. Duplicate annotations are errors.

Tail positions include the function body's final expression. The reference
`if` branches, `scope` body, and selected `match` clause bodies retain tail
behavior through their ordinary expanded code when the enclosing call is in
tail position. This follows from structural use of those bodies, never a
library name or descriptor: duplicating, discarding, delaying, or
postprocessing a block does not grant tail permission. The condition of
`if`, the scrutinee of `match`, ordinary function arguments, tuple
components, and other subexpressions are non-tail positions and require
`recur(cont)`. The `cont` annotation is rejected in tail position so the source
states the intended lowering precisely.

`recur(escape)` permits recursive calls captured by ordinary explicit or
placeholder closures, including generated monadic sequence continuations. The
closure may be returned, stored, called repeatedly, or passed to an unknown
higher-order consumer. Each invocation re-enters the same closed recursion
group at the named member and selected instantiation. The call's arguments are
evaluated left-to-right, exactly once, before its state is passed to re-entry.
An ordinary closure is an escaping boundary even when a particular consumer
calls it immediately; inspecting a callee's body or name does not change that
classification. Authenticated structural continuations that execute as part
of the current step retain their existing tail/non-tail rules instead.

A call beneath an escaping boundary requires `escape`; adding only `cont`
does not suffice. Conversely, `escape` without such a boundary is unnecessary
and rejected. Written `cont` beside `escape` is accepted even when the call is
the closure body's final expression, because the closure may outlive the
step. The independent necessity checks for `poly` still apply.

The continuation boundary is introduced by a bind or sequenced expression.
In `do(chain) { let x <- recur(cont) step(v); x }`, the recursive call supplies
the first ordinary value argument of the expanded
`chain(recur(cont) step(v), .(x) { x })`, outside the bind continuation.
It therefore requires `cont` and is not an escaping call. Recursive calls in
the rest of the block after a bind or sequence are inside its ordinary
generated continuation and require `escape`.

`rec(loop)` supports polymorphic recursion. A recursive member's type parameters may be supplied explicitly at a recursive call site:

```kio
pub rec(loop) fn walk[A](n: I32, marker: A) -> I32 {
  if(leq_i32(n, 0)) {
    n
  } else {
    recur(poly) walk[String](sub_i32(n, 1), "next")
  }
}
```

If the leading type arguments are omitted, the call uses the current member instantiation and `recur(poly)` is rejected as redundant. If the type arguments differ from the current member instantiation, `recur(poly)` is required. The number and order of written type arguments must match the callee member's type-parameter list. Mutual-recursion members share the same type-parameter list and return type shape, so every member can return through the one loop result type; individual `recur(poly)` calls can still choose a different instantiation for the next state. The lowering packages each member's value parameters in an existential state packet, so `rec(loop)` type parameters must be kind `*`.

The form is surface-only. The desugar pass replaces a group with private nominal state packets, a loop-step lambda, and one ordinary wrapper function per member, preserving each member's visibility. Tail recursive calls construct the next state packet and return the left arm of `S | R`; each non-recursive tail result returns the right arm. A group that contains `recur(cont)` uses continuation-carrying state instead: each state packet also carries the rest of the computation as a function from the callee's result to the loop step result, so a non-tail recursive call becomes "enqueue the callee state with an updated continuation" rather than a Kio' fixpoint. A continuation-bearing packet that refers back to itself is emitted as `rec type`; packets that refer to one another are emitted as one minimal Kio' `rec { ... }` type group. No recursive-function group or recursive-call marker reaches Kio'.

The loop boundary is semantic, not just an implementation shortcut:

- **Kio' core stays total.** Recursive control flow is an explicit capability supplied to the surface program, not a term-level fixed point in the Kio' core.
- **Loop-managed depth is backend-independent.** Ordinary transitions and deep runs begun after earlier calls have returned depend on the supplied loop, not native recursive calls. An escaped closure can also invoke the host re-entry callback synchronously while an earlier host call waits for its result. Such nested waiting calls retain host frames and can exhaust the host stack; the capability does not capture an opaque host continuation or promise stack safety for arbitrarily deep synchronous re-entry.
- **Polymorphic recursion is Kio-defined.** `recur(poly)` is accepted or rejected by Kio's own recursive-call rules, not by whether the host language's native recursive functions support the same generic-instantiation pattern.

Backends therefore must not lower `rec(loop)` groups to host-native recursive functions. The open-world argument is the same as for ordinary lexical resolution: the `loop` path is resolved as a normal value path, `recur name(...)` only searches the closed member set written in the group, and each wrapper has the fixed identity `(declaring module, member name)`. None of these rules searches an ambient declaration pool by signature or role, so adding an unrelated declaration cannot retarget an existing recursive call or wrapper call.

### Anonymous functions

The dot-led expression form produces an anonymous function with one or more
type-binder or value-parameter groups:

```kio
.(x, y) { add(x, y) }
.[A](x, y) { pair(x, y) }                 // explicit type parameter
.(x, y) -> I32 { add(x, y) }                 // explicit return type
.[A] { body }                               // type-only abstraction
```

Parentheses delimit each written value group, including an explicit `()`;
they are absent from a type-only signature. `. { body }` is rejected because
it has no group. There is no automatic currying within a value group — same
as top-level `fn` definitions.

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

let id = identity[[A] A -> A](.[A](x) { x });
```

`identity`'s `x: T` slot pins the lambda's type once `T` is instantiated to `[A] A -> A`.

**Type-parameter declarations are allowed.** A polymorphic lambda declares its
`[A]`-style type-binder groups in signature order, with or without value
groups. The user-written binders flow through unchanged; the typer never
invents a universal binder. In checking mode, the expected function type
supplies any missing parameter slots. In synthesis mode, value-parameter
annotations must be concrete unless the same direct application supplies a
whole missing slot as described above. An application solves the lambda's
written type arguments by the ordinary positional call rule.

A type-only lambda has no value-parameter annotations to complete. It can
synthesize its type from its written binders and body, as in
`let make = .[A] { body };`, or check against an expected polymorphic type.
Only the written groups are consumed from that expected type; a remaining
polymorphic result is the body's expected type. Every completed scheme still
obeys the kind rules in [Higher-kinded types](#higher-kinded-types).

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
See [Macros are imported, not ambient](#macros-are-imported-not-ambient)
for this marked-staging boundary; ordinary calls and monomorphic lambdas retain
their own inference rules.

Top-level `fn` and `host fn` value-parameter types remain **mandatory**.
Both declarations may omit their return annotation to request the `.` default;
the result is never inferred from a named function's body. These are
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

**Explicit core applications.** Lowering materializes
unit arguments, nested application boundaries, returned type applications,
and macro residual functions as ordinary Kio'. Post-typecheck routing and
backend emission preserve those explicit applications, including one ordered
stage per universal binder even when the backend erases type representations.
These stages preserve the [Kio' evaluation contract](prime.md#kio-semantics).
Kio type applications do not add runtime Kio type values or host-supplied
representation codecs to carry their type arguments across the FFI.
Backend-documented native host type
selectors may still be required; see the
[shared type-erasure contract](backends/README.md#2-type-erasure-at-the-ffi).
Surface `_` placeholders are resolved before Kio' and remain rejected in Kio'
input.

### Parameter patterns

*Surface only; desugars to Kio'.* The Kio' form is an ordinary `name: Type` value parameter paired with generated projection lets at the head of the function body — see § Desugaring below.

Where a value parameter would otherwise be written `name: T`, the surface admits **product-destructuring patterns** that name the components of a product type instead of (or in addition to) the whole:

```kio
// Name the whole product, then project inside the body.
fn project_left(p: (A & B)) -> A { fst[A][B](p) }

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

**Applicable positions.** Parameter patterns are admissible everywhere a value parameter is admissible: top-level `fn` definitions, anonymous lambdas (including `match` clauses — see [§ Pattern matching](#pattern-matching)), and `equiv` arms. The grammar is uniform across those positions (see [`grammar.md` § Surface parameter additions](grammar.md#surface-parameter-additions)). A `host fn` value parameter is the one signature site that does *not* admit a destructuring pattern — it is always a named `name: Type` slot.

**Slot types follow the same elision rules as the surrounding fn.** A leaf slot's `: T` is optional when the slot's type can be resolved from context, exactly like an un-annotated `.(x) { … }` slot:

- In an ordinary anonymous lambda, the slot's type may be pinned by the
  surrounding expected function type. A `match` clause instead writes its
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
  let a = __fst__[A][B](p);
  let b = __snd__[A][B](p);
  use(a, b)
}
```

and `fn foo(((a: A, b: B), c: C)) { use(a, b, c) }` desugars to

```kio
fn foo(p0: ((A & B) & C)) {
  let p1 = __fst__[(A & B)][C](p0);
  let c = __snd__[(A & B)][C](p0);
  let a = __fst__[A][B](p1);
  let b = __snd__[A][B](p1);
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
- Only single-segment value paths participate. Qualified paths such as `module.x1` do not. Ordinary direct and UFCS call heads such as `x1(value)` and `value.>x1` participate; a numbered owner shadows an imported macro of that spelling. A dedicated recursive-call callee does not. Labels, type names, and operator implementation names are not reference positions.
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

A direct product or sum result also requires parentheses: `A -> (B & C)`
or `A -> (B | C)`, never `A -> B & C` or `A -> B | C`. This includes
leading, trailing, empty and one-item chain spellings. A named function or
lambda's own return annotation may remain bare (`fn pair() -> A & B` and
`.() -> A & B`); an arrow nested inside that annotation obeys the ordinary
type-expression rule (`fn factory() -> A -> (B & C)`). Arrow-only chains
remain right-associative: `A -> B -> C` means `A -> (B -> C)`. A prefix
forall keeps its own body scope, so `A -> [B] B & B` is a function returning
a universally quantified product, not a direct product result.

### Conditionals

The Boolean intrinsics accept a concrete host `role(bool)` carrier.
Compile-time predicates instead return `Comptime_bool`, which has no host role.
The separate `__comptime_if_then_else__` helper accepts a `__Comptime__` proof,
then a condition of that type and two Unit thunks. It evaluates the complete
argument packet in order and invokes only the selected thunk; the result type
is generic. It has no runtime counterpart: retained code eliminates its
bottom-typed proof. `rehost` preserves a source host declaration's role and
therefore cannot replace the reference library's host `Bool` with `Comptime_bool`.

*Surface only; elaborates to Kio'.* The reference library's conditional is an
ordinary imported block macro:

```kio
import macros/control(Bool, macro if);

if(cond) { a } else { b }
```

Its public type is `[Result] (Bool & (. -> Result) &
(. -> Result)) -> Result`, with an unlabelled `thunk` descriptor followed by
`trailing thunk else;`. Its implementation constructs ordinary
`__if_then_else__` glue from the checked condition and the two projected
thunks. The library declares its own concrete `Bool` host type with
`role(bool)` and checks the condition against that type. Consumers can import
that same type or rehost the library's control module to their own Boolean
capabilities. The generated intrinsic is explicitly specialized at this
concrete Boolean type; the library's name grants no compiler privilege. See
[`prime.md`](prime.md#branching-intrinsic).

In the block spelling, each arm projects a lambda, and their results jointly
constrain the one result type of the whole expression. In check mode, the surrounding expected type checks
both arms directly. Concrete arm contexts can determine unannotated scalar
literal carriers through ordinary local inference. In synth mode, either arm whose result is independently
determined may establish or refine the common result; swapping the arms does
not change whether the expression is accepted or which result type it infers.
If neither context nor either arm determines the result, the common-result
relation is underdetermined. Independently determined arm results that disagree
produce a non-directional common-result mismatch. The typer never forms an
implicit sum from incompatible arms; a branch that contributes to a sum
constructs it explicitly, normally with `widen_sum`.

Result inference does not flow backward into `cond`. The library evaluates the
combined input product once, in order: the condition, then the two expressions
that produce thunks. It invokes only the selected thunk body. In the block
spelling those expressions are the projected lambdas, so only the selected
block body runs. In a direct call, both thunk-producing expressions run even
though only the selected thunk body runs. Its public type relates exactly the
two written arm results and consults no ambient declaration pool, so adding
an unrelated module-body declaration cannot change its result or evaluation.
The same behavior under a renamed import follows from that declaration and
its ordinary implementation. Other declarations named `if` acquire no such
behavior by their spelling.

### Pattern matching

*Surface only; desugars to Kio'.* Pattern matching is the `match` dispatch macro, imported like any other macro (see [§ Macros are imported, not ambient](#macros-are-imported-not-ambient)). It is a macro because compile-time code synthesizes branch-selection glue at the call site; it is not a source-to-target coercion. The Kio' form `let`-binds the combined scrutinee and clauses once, projects them, discriminates the scrutinee's sums with nested `__either__` calls, and projects products with `__fst__` / `__snd__` / projector-member calls — see § Desugaring below.

```kio
import macros/match(macro match);

match(<value>) {
  .(<param-list>) { <body> };
  .(<param-list>) { <body> }
}
```

`match` takes a single scrutinee prefix and one `product` block containing its
clause expressions. **Each clause is an expression of function type** — there
is no dedicated clause construct and no `case` keyword. A clause may be an
inline lambda, a name, a member projection, or a call. Semicolons separate
block entries. One entry preserves its value unchanged, so a preassembled
product of clauses remains expressible as `match(value) { clauses }`.
Ordinary tuple grouping inside an entry retains its ordinary meaning. The
block-call rules require the clause block, including in a UFCS spelling such
as `value.>match { clauses }`. The ordinary direct call `match(value, clauses)`
supplies the same product.

Whatever expression a clause is, it must independently determine the
**parameter type** of its function. That parameter type is the **dispatch
pattern**, declaring which DNF branch of the scrutinee the clause handles.
`match` supplies no expected parameter shape from the scrutinee: the
independently determined parameter type specifies which source branches the
clause accepts rather than receiving the scrutinee's type by context. A clause lambda
therefore writes its value-parameter types; a name or other expression must
synthesize a function type whose parameter component is already determined.
Either way, `match` reads that parameter component to decide dispatch. This
independence applies only to the parameter component; the clause result follows
the common-result rule below.

For example, suppose `type { Ok[A]: A; Err[A]: A; }` has introduced `Ok`
and `Err`, `r` has type `Ok[I32] | Err[Str]`, and `handler_ok: I32 -> R` and
`handler_err: Str -> R` are in scope. Each clause receives the nominal value
and projects its payload before calling the handler:

```kio
match(r) {
  .(v: Ok[I32]) { handler_ok(Ok.get(v)) };
  .(e: Err[Str]) { handler_err(Err.get(e)) }
}
```

**Dispatch.** Dispatch is **per DNF branch**, not whole-scrutinee. The imported `match` implementation normalizes the reflected scrutinee type to DNF (a sum of products) and considers each DNF branch separately. Each clause's parameter type is *also* DNF-normalized; for each scrutinee branch, that implementation walks clauses in source order and tests whether the branch's factors structurally fit **some** DNF branch of the clause's parameter type (product projection plus structural rearrangement, never partial sum narrowing) — the **first clause that fits** wins for that branch. The generated term calls the chosen clause function with a value reconstructed to *exactly* the parameter type from the matched scrutinee components. Different DNF branches may select different clauses, with sum *discrimination* handled by the generated decision tree. The exact per-branch fit and decision-tree algorithm — which library coercion fires at each slot — is documented in the case studies [`docs/poc/optics.md`](../docs/poc/optics.md) (which exercises `match` directly) and [`docs/poc/macros.md`](../docs/poc/macros.md).

Concretely — a clause slot's annotated type may be **any structural type** — atom, product, sum, or sum-of-products — not only a fully-destructured atom:

- A clause `.(v: T) { … }` with `T` an atom fires on every DNF branch that fits `T`. For a sum-typed scrutinee, that's typically a single branch; for a product-typed scrutinee `X & Y` it projects to `T` when `T` is a sub-product.
- A **compound product slot** `.(pair: (A & B), c: C) { … }` on a `((A & B) & C)` scrutinee binds `pair` to the `(A & B)` sub-product and `c` to the `C` — the slot type is matched and the value reconstructed by the same structural coercion that fits any slot.
- A **sum slot** `.(x: (A | B)) { … }` covers *every* scrutinee branch matching either arm — a single clause can cover multiple branches, with `x` bound (via the appropriate sum arm) differently per branch. A sum-of-products slot works the same way, one DNF branch of the slot type per arm.
- A multi-parameter clause `.(a: A, b: B) { … }` fires on branches whose factors fill both slots — slots are filled left-to-right (the source-order rule, lifted one level).
- A zero-parameter clause `.() { … }`, and a `.`-typed slot, fire on any branch: every type fits `.` by projecting to the empty product. `.() { … }` is the **catch-all**. No special catch-all syntax is needed.

Type-parameter declarations (`[A]`) written in a clause `fn` work exactly as in any `fn`. The one twist is who supplies the type arguments: `match`, as the caller, specializes the clause's explicit leading binders against each compatible scrutinee branch rather than taking them from a written call site. Writing `.[A](v: Ok[A]) -> String { … }` matches any compatible `Ok` branch and binds `A` to the payload type. A polymorphic clause used by several source branches contributes one specialized result per branch, in source-branch order.

An inline polymorphic clause follows the marked-lambda staging rules in
[Macros are imported, not ambient](#macros-are-imported-not-ambient).
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
with `widen_sum`. This result flow checks a clause-lambda body without changing
its independently written parameter types. A clause lambda may carry an
explicit `-> R` return annotation; that annotation is checked against the
common result and also seeds its body when no match result is otherwise
available (see [Anonymous functions](#anonymous-functions)).

**Exhaustiveness.** The DNF branches of the scrutinee's type must be **covered**: every DNF branch must fit at least one clause's parameter-product type. A `match` whose clauses leave some DNF branch uncovered is a compile-time error in the macro-error category (exit code `15` — `match` is a user-defined macro, so its coverage failures surface via `__macro_error__`; see [`exit-codes.md`](exit-codes.md)). A trailing `.() { … }` catch-all covers everything, so it always makes a `match` exhaustive regardless of earlier clauses.

**Reachability.** Every clause must cover at least one DNF branch that earlier clauses do not cover. A clause that adds no new coverage is unreachable — a compile-time error in the same macro-error category (exit code `15`). A clause that covers nothing at all is also a `15` macro error — including a slot whose type DNF-normalizes to zero branches (a `!`-typed slot covers no branch). First-match ordering means an earlier clause can eclipse a later one. A slot type **wider** than the branches it ends up covering is *not* an error: a sum slot whose later arms were already taken by narrower sibling clauses is fine — the slot type is an upper bound, not a coverage obligation.

**Desugaring.** `match` lowers to ordinary Kio': it `let`-binds the combined scrutinee and clauses once, evaluating the scrutinee and then clause expressions in source order before any branch is taken; it projects the bound values, builds a decision tree of nested `__either__` calls over the scrutinee's DNF branches, and at each leaf calls the selected clause function with its argument reconstructed by `let` / `__either__` / projection-intrinsic glue. The reconstruction is duplication-free — a scrutinee factor is never threaded through two slot positions of one clause leaf, and the clause's return value is evaluated once. The elaboration is recorded in the side channel and substituted at the Lowered → Prime boundary; no `match` form survives into Kio'. The per-leaf coercion algorithm is worked through in [`docs/poc/optics.md`](../docs/poc/optics.md) and [`docs/poc/macros.md`](../docs/poc/macros.md).

**Type flow.** `match` is a macro position (see [Type system](#type-system)
for the bidirectional-flow mechanism). The scrutinee synthesizes bottom-up, and
each clause independently determines the parameter type that serves as its
dispatch target; the macro decides which clause handles each branch and
what projection produces its argument. Only after those prerequisites are
known do the authenticated returned occurrences participate in their finite
symmetric common-result relation. A surrounding expected type checks every
result, including an inline clause body;
without one, any independently determined returned occurrence may determine
the shared result. Result flow never supplies the scrutinee type or a clause's
dispatch-pattern parameter shape.

The relation contains only occurrences from the written clauses and the finite
authenticated fill transcript produced by the already-resolved imported
macro. It never searches visible declarations, so adding an unrelated
module-body declaration cannot add an occurrence, change the common result, or
alter dispatch.

`match` takes an explicit scrutinee/domain value, checks coverage and reachability against that scrutinee's DNF, binds polymorphic clause type parameters from each concrete branch, and returns the common selected-clause result type. It is imported explicitly like any other macro (see [§ Macros are imported, not ambient](#macros-are-imported-not-ambient)).

```kio
import macros/match(macro match);

match(r) {
  .(v: Ok[I32]) { handler_ok(Ok.get(v)) };
  .(e: Err[Str]) { handler_err(Err.get(e)) }
}
```

**Type.** `match(v) { clauses }` has the common clause result type `R`;
every clause must return `R`. Coverage/reachability are checked against `v`'s
DNF, and polymorphic clause binders (`.[A](…)`) are solved from the branch
selected for each leaf.

**Clauses.** The projected block value is a single clause or a product of
clauses. A non-function clause is a type error at the offending position.
Grouping a single clause does not construct a unary product.

**Result type derived from the clauses.** `match` carries a **same-result type
rule** (see [§ Type-argument inference and the `_` placeholder](#type-system)):
every clause result must conform to one result type. `match(v) { clauses }`
derives the result from the clauses' joint finite relation when no surrounding
expected type is available. Any independently
determined clause result may determine the common result; source order does not
choose an inference anchor. When the common result is known, it checks every clause result and
flows into an inline clause body; it does not determine the clause's
dispatch-pattern parameters.

**Elaboration.** `match` elaborates to the decision tree over the scrutinee's DNF branches, the per-leaf reconstruction at each value-parameter slot and the return position, and the `let`-bind-each-clause-once discipline described above. Behind the sugar it is ordinary Kio' — `let`, `__either__`, and the projection intrinsics — and no `match` form survives into Kio'.

<!-- markdownlint-disable-next-line MD033 -->
<a id="the-derive-macro"></a>

### The `derive` macro

*Surface only; desugars to Kio'.*

`derive` constructs a value of a requested **target type** by composing a caller-supplied set of **rule functions**, threading their outputs into one another's parameters until the target is produced. It is imported explicitly like any other macro (see [§ Macros are imported, not ambient](#macros-are-imported-not-ambient)). The motivating use case is building typeclass-like *instances* — most importantly deriving a `Monad` / `Applicative` / `Functor` instance for a composite type constructor (a monad-transformer stack) from base instances plus per-transformer rules — but the form is type-constructor-agnostic: any family of `fn`s whose signatures read as inference rules participates.

```kio
import derive(macro derive);        // imported like any macro

derive((
  , <candidate1>
  , <candidate2>
  , …
), <TargetType>)
```

The leading-comma layout inside the multi-candidate product follows Kio's
multi-item-list style. The candidate value is the first value argument: `()`
supplies no candidates, one function expression supplies one candidate
directly, and a product tuple supplies multiple candidates. Its type fills the
leading `Candidates` binder in the scheme below. The target type fills the
`Target` binder exposed after that value argument, so a written target type
appears in the trailing slot.

The target may be omitted or written `_` when a checking position supplies it,
for example a function's declared return type, a fully annotated local binding,
or a value-argument slot. An untyped `let` supplies no expected target. A leading
`_` in `derive[_](<candidates>)` instead marks the `Candidates` type slot; it
does not move the target slot before the candidate value. In the following
schematic example, the enclosing result supplies the target:

```kio
fn stack_instance() -> Monad[State_t[Int][ Maybe_t[Maybe]]] {
  derive((
    , state_t_monad
    , maybe_t_monad
    , maybe_monad
  ))
}
```

**Type.** The imported `derive` has scheme
`[Candidates] Candidates -> [Target] Target`. The candidate value determines
`Candidates`, and an explicit trailing type argument or expected result
determines `Target`: `derive(<candidates>)[T]` produces a value of type `T`,
not the candidate value. The empty-candidate argument `derive(())[T]` means
"no candidates" and therefore fails with "no rule applies" for every target,
including `.` and structurally inhabited product or sum types. Values arise
only from caller-supplied candidate rules.

**Candidates are function expressions.** A singleton candidate, and each element of a multi-candidate product, is an expression of function type — idiomatically a reference to a top-level or local `fn`, or an inline [`fn`](#anonymous-functions) literal. A candidate's signature *is* an inference rule:

```
fn <rule-name>[T1](…)[TN](// binders, each with optional kind annotation
               h1: H1, // preconditions
               …, hk: Hk) -> R               // R is the result type the rule produces
```

**The semantic domain determines preconditions.** The imported `derive` implementation takes no preconditions from a Unit domain. For every other domain, it walks the right product spine: each left component is one sub-goal, and traversal continues through the right component. A non-product is one sub-goal. Declaration parameter grouping does not affect this rule: split parameters, one product parameter, and a grouped final product tail have the same prerequisites when their semantic domains agree. A base instance is the zero-precondition case (`. -> R`) and can explicitly produce any result type `R`, including `.`, a host type, a product, a sum, or an opaque nominal type. Kio canonicalizes both `fn base() -> R` and `fn base(_unit: .) -> R` to that same logical zero-slot function shape; neither requires a candidate that derives `.`. A Unit parameter within a multi-slot value group remains an ordinary precondition. To admit a rule that takes an ordinary parameter that is *not* a precondition (a literal, a flag), partially apply it inside an inline function literal in the candidate argument. Binders may carry any kind (`[A]` is `*`, `[*F]` is `*→*`, `[**G]` is `*→*→*`); the implementation respects the annotation when unifying.

A singleton candidate that is not of function type, or an element of a multi-candidate product that is not of function type, is a type error pointing at the offending position (`derive_non_function_candidate`).

**Resolution.** Given the goal `T` and the candidate set, the `derive` implementation finds the **unique** composition of candidates whose result types thread together to produce `T`, treating each candidate's domain prerequisites as sub-goals to discharge from the same set. Resolution is kind-aware (a candidate binder of kind `*→*` binds only to a kind-`*→*` argument), reports a macro error when no composition applies or when two distinct compositions exist, and is total by the structural-decrease rules governing its compile-time recursion. The [HKT case study](../docs/poc/hkt.md#deriving-an-instance-with-derive) shows dictionary derivation. The [macro case study](../docs/poc/macros.md#derive--instance-deriving) explains the resolution algorithm, exactly-one-coherence rule, termination conditions, and recursive and polymorphic compositions of wrapper rules.

The goal may carry **unbound binders inherited from the enclosing `fn`** (a type-constructor-generic combinator with a `derive` site whose target mentions the function's own `[*F]`). The implementation's structural unifier treats those as logic variables; the generated derivation is then polymorphic in them and the enclosing function compiles to a single type-constructor-generic body.

**Open-world.** The candidate set is exactly the function expressions the candidate argument literally supplies; `derive` cannot reach module-level `fn`s or explicitly imported items implicitly, and the ordinary import brings only `derive` itself into scope, never any candidate. A "module handle" candidate (passing some module's exports as the candidate set) is **rejected** — it would let another package change resolution at an existing site by adding or removing exports. Adding a `fn` anywhere in the local package source provably cannot change resolution at any existing `derive` site, because there is no implicit pool to grow. This is the same local open-world guarantee every macro carries.

**Elaboration.** The imported macro implementation executes at compile time and returns a checked nested call tree of the chosen candidates applied to their bound type arguments and recursively generated precondition values — exactly what the user would write by hand. The front-end records that checked term in the same boundary-local side channel used by every macro call; the substitute pass swaps the `derive` call for the recorded tree at the Lowered → Prime boundary, and standalone Prime validation checks the assembled package. No `derive` form or compile-time machinery survives into Kio': what remains is an ordinary call tree of the caller-supplied rule functions, executed at runtime like equivalent hand-written code. The rules themselves need no `pure` marker because they are runtime values placed in that tree; the macro's own implementation and helpers obey the ordinary compile-time purity contract.

`derive` imposes no backend constraint — it synthesizes an ordinary call tree, so a derived instance emits exactly as the equivalent hand-written composition would. This holds for the transformer-stack goal too: a transformer type constructor (`State_t`, `Maybe_t` — a nominal type with a kind-`*→*` parameter) in a kind-`*→*` position emits on every backend, including `rust`, where it rides the erased `Rc<dyn Any>` body like every other value (see [`specs/backends/rust.md` § Higher-kinded types](backends/rust.md#higher-kinded-types)).

### Scoped expressions

*Surface only; elaborates to Kio'.*

The reference `scope` macro accepts one `thunk` block and invokes it
once. Its body uses ordinary scoped local bindings, row and existential-opening
lets, and expression statements. Its final expression is the result; an empty
body returns unit. Import it explicitly to put a scoped computation in an
expression position, such as an `equiv` arm:

```kio
import macros/control(macro scope);

equiv id_unfolds[A](x: A) {
  scope {
    let y = id(x);
    y
  };
  x
}
```

`scope` and `do` are distinct declarations with distinct descriptors and
public types. `scope` does not admit `<-` bindings. Its ordinary expansion
retains the body's recursive tail behavior when the call is in tail position.

### Monadic `do` blocks

*Surface only; desugars to Kio'.*

The reference `do` macro runs a `sequence` block with an explicitly
supplied bind function. The library exports the transparent aliases:

```kio
pub type Bind[*F] = [A][B] (F[A] & (A -> F[B])) -> F[B];
pub type Sequence[*F][R] = Bind[F] -> F[R];
```

Its public type is `[Receiver][Result] (Receiver & (Receiver -> Result)) -> Result`,
with one `trailing sequence;` descriptor. Sequence projection requires the
receiver's structural type to be `[A][B] (M[A] & (A -> M[B])) -> M[B]` for
one fixed carrier `M`, and the block's final result to have that same carrier.
`A` and `B` remain independently polymorphic at each bind call. `M` may be a
concrete constructor such as `Box`, an abstract kind-`*→*` binder, or a
structural carrier such as identity `M[A] = A` or constant `M[A] = .`;
the `Bind` alias is not a restriction to first-class constructor arguments.
The compiler recognizes the structural bind type, not the aliases' names or
the identity of `do`. Every sequence block projects to an ordinary
`Receiver -> Result` function, independently of which macro receives it.

The receiver's carrier is determined by available ordinary public-call and
header constraints. The sequence body does not infer a missing carrier
argument of a receiver factory. For a factory declared as
`make_bind[*M]() -> [A][B] (M[A] & (A -> M[B])) -> M[B]`,
`make_bind[Box](())` supplies that carrier and its unit value argument;
`make_bind()` cannot obtain `M` from the block's actions or final expression.
Ordinary value arguments to a factory may determine its carrier as usual.

`do(bind) { … }` evaluates the receiver expression exactly once per execution,
before running the projected sequence, including when the block contains no
bind steps. Zero, one, or multiple steps use the same resulting bind value.
An impure receiver expression remains an executable dependency in a `pure fn`
even when the block never invokes the bind value.

**Statement forms.** A sequence block admits three statement shapes plus a
trailing final expression. A binder is an ordinary name or `_`, or an explicit
rich pattern `.(PATTERN)`: typed `.(x: T)`, tuple `.(x, y)`, or as-pattern
`.(whole: (x: A, y: B))`. Ordinary `=` bindings also admit row and
existential-opening forms under [Blocks and local bindings](#blocks-and-local-bindings).
The `<-` connective uses the name, wildcard, typed, tuple, as-pattern, or
[numbered](#numbered-bindings) forms;
it does not introduce a row or existential-opening binder.

| Form | Surface | Desugared |
|---|---|---|
| **Bind** | `let <binder> <- e;` | A bind call with a continuation whose parameter uses the binder's pattern; the bind type connects `e`'s `F[T]` payload to that parameter |
| **Pure let** | `let <binder> = e;` | An ordinary lexical let over the continuation, with the block-let typed/untyped RHS rule; no bind step |
| **Sequence** | `e;` | `bind(e, .(_: .) { <rest> })` — `e` must type as `F[.]`; the continuation's unit payload is discarded |
| **Final expression** | `e` | The block's overall value; must type as `F[R]` for the same `F` |

Empty blocks and statement-only blocks are type errors after descriptor
resolution. The parser accepts their neutral syntax. A final semicolon after
a real final expression is accepted and ignored.

Continuation functions preserve a written binder annotation; otherwise the
bind function's expected continuation type supplies the parameter type. A
sequenced expression requires `F[.]`; an explicit `let _ <- e` is an ordinary
bind and may discard a non-unit payload. The resolved bind shape and the
ordinary call's expected result connect the carrier and result types. Later
uses in the continuation never infer the earlier source's payload type across
its binding boundary. Ordinary lets and actions retain their source-first,
once-only checking rules.
This extends the expression-block clause-order principle to sequence clauses,
not just named bindings: an earlier unbound action checks against `F[.]`
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

**Nesting.** `do` calls nest freely. An inner call is an ordinary monadic
expression used in the outer block like any other RHS; bind functions may
differ across nested levels.

**Explicit final result.** The user writes `pure(v)` or another `F[R]` result
explicitly. Neither the sequence descriptor nor the `do` library introduces
an implicit lift.

**Projection.** A sequence block becomes an ordinary function taking a bind
function. Its body right-folds onto the final expression:

- `let x <- e;` / `let .(x: T) <- e;` followed by `rest` become `recv(e, .(x[: T]) { rest })`.
- `let x = e;` / `let .(x: T) = e;` followed by `rest` become an ordinary checked or synthesized local binding over `rest`.
- `e;` followed by `rest` becomes `recv(e, .(_do_seq: .) { rest })`.
- The trailing final expression is the innermost continuation body.

Each projected bind call is typed against its ordinary public function type.
The `do` implementation applies the projected function to its supplied bind
value. All descriptor and projection metadata is consumed before Kio'; the
result contains ordinary functions, calls, and lets. The generated
continuations remain ordinary nested functions: a sequence descriptor grants
no additional recursive tail positions.

**Worked example.**

```kio
import macros/sequence(macro do);

type { Box[A]: A; }

fn box_pure[A](x: A) -> Box[A] { Box.mk[A](x) }

fn box_bind[A][B](m: Box[A], k: A -> Box[B]) -> Box[B] { k(Box.get[A](m)) }

fn run() -> Box[String] {
  do(box_bind) {
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

**UFCS** — *Uniform Function Call Syntax* — inserts a receiver into one
written value-call stage. All four spellings work with ordinary functions,
type-member functions and direct local macro names:

```kio
r.>f(x)       // = f(r, x)
r.>>f(x)      // = f(x, r)
f(x).<r       // = f(x, r)
f(x).<<r      // = f(r, x)
r.>f[A](x)    // = f[A](r, x)
r.>m.f[A](x)  // = m.f[A](r, x)
r.>T.member[A] // = T.member[A](r)
```

`.>` and `.>>` select the immediately following value group, after any
explicit type arguments on the written callee. `.<` and `.<<` select the
immediately preceding outermost value application. `.>` and `.<<` prepend;
`.>>` and `.<` append. Each selected group is one product argument for one
`Function` layer. Insertion never searches another layer for a type that fits.

```kio
r.>f(a)(b)       // = f(r, a)(b)
f(a)(b).<r       // = f(a)(b, r)
f(a)(b).<<r      // = f(a)(r, b)
(f(a).<r)(b)     // = f(a, r)(b)
(f(a)(b)).<r     // = f(a)(b, r): whole-expression grouping is transparent
f(a)[B].<r      // = f(a)[B](r): brackets end the earlier value stage
f(a)[B](b).<r   // = f(a)[B](b, r)
```

A reverse splice with no preceding value application calls its selected
callee with the receiver: `f.<r` and `f[A].<r` mean `f(r)` and `f[A](r)`.
A forward splice likewise needs no list when its receiver is the only value:
`r.>f` and `r.>f[A]` mean `f(r)` and `f[A](r)`.
An explicitly empty selected UFCS group is rejected in every direction:
`r.>f()`, `r.>>f()`, `f().<r`, and `f().<<r` are parse errors.
Omit that group for a receiver-only stage, or write `(())` for an additional
Unit. In `factory()(b).<r`, the selected group is `(b)`, so the earlier
ordinary Unit call stays intact.

Type brackets on a forward callee precede receiver insertion. A binder
exposed by consuming the receiver is a later stage: `iso(r)[T]` can also be
written `(r.>iso)[T]`. It is not `r.>iso[T]`, which instantiates a binder
before the receiver. Omitted type groups and whole-slot `_` arguments retain
the ordinary finite local inference rule.

Forward callees are paths, including transparent grouping such as
`r.>((m.f))[A](x)`. Reverse syntax selects the immediately preceding written
stage, including a computed callee; it does not discover a different stage by
typing the receiver. Calls on a function result remain ordinary applications:
`f(a)(b)(r)` applies that result and differs from `f(a)(b).<r`.

The right-hand receiver of `.<` / `.<<` is tight: a primary expression and
its ordinary type/value applications, stopping before another dot-splice.
`f.<x.>g` therefore means `(f.<x).>g`; write `f.<(x.>g)` to insert the splice
result instead. Existing operator atom boundaries also apply. An
operand-ending prefix such as `! _` needs `f.<(!x)`; an operator ending in a
fixed word already completes an atom.

A splice has the same call plan and runtime behavior as its equivalent prefix
call. For an ordinary function, the callee and selected value arguments are
evaluated once: prepending places the receiver before the group's written
values, while appending places it after them. A macro receives quoted inputs;
its generated code determines whether and how those inputs run. The equivalent
prefix macro call has the same behavior.

**Block calls.** A complete trailing-block macro call inserts into its prefix
stages, before all projected blocks. Forward forms select its first prefix
group; reverse forms select its last. With no written prefix, insertion
creates a receiver-only prefix group, which combines with the projected blocks.
No splice inserts into a block or
combines two prefix groups. The written call is admitted as a connected public
plan before its implementation runs; ordinary residualization still applies
after all declared blocks are supplied.

**Bidirectional flow through UFCS chains.** A splice step's expected return type — set by the surrounding context (e.g., the next chain step's inserted-argument position, or a `let` binding's expected type when one is present) — propagates down to the step being checked. This is the same expected-type-at-the-position mechanism the typer uses for macro targets and `fn` parameter slots; UFCS chains pick it up through the rewritten call shape, so elided macro targets in chained positions (e.g. `r.>onto.>callee` with `onto`'s target unspoken) get filled from the next callee's required argument type.

**Errors.** Each gets a clear diagnostic:

- "dot-splice receiver has no value-argument slot to fill" — the callee has only type slots, or no argument slots at all.
- "an explicitly empty UFCS argument list is ambiguous" — remove the list for the bare receiver-only form, or replace it with `(())` to pass Unit explicitly.
- "type mismatch: expected `<T>`, found `<R>`" — the receiver doesn't match its planned value slot.
- "callee `<path>` is not callable: its type is not a function" — the resolved value isn't a function.
- "callee `<path>`'s type parameter `[A]` could not be resolved" — the receiver and arguments don't pin every type parameter.

**No projector fallback.** A name `f` that isn't a free value in scope cannot be reached by UFCS — even when it's a member of a nominal type `F` that is in scope. To call a projector-shaped member receiver-style, bring it in as a free value (`import m(f);`) or use the nominal-member spelling (`F.get(r)`).

**Worked examples.**

Non-compound receiver:

```kio
fn shout(s: String) -> String { …}

let s = "hello\n";
let msg = s.>shout;     // = shout(s)
print(msg)
```

Compound receiver — narrow with `onto`, then chain:

```kio
type { Tag: .; }

fn shout(s: String) -> String { … }

let pair = (Tag.mk(()), "hello\n");   // pair : Tag & String
pair.>onto.>shout.>print                  // pair narrowed to String, then chained;
                                          // onto's target is filled from the next
                                          // callee's first-param via bidirectional flow.
```

Polymorphic callee (type-arg backsolved from the matched conjunct):

```kio
fn id[A](x: A) -> A { x }

let s = "hi\n";
let result = s.>id[String];  // explicit [A] = String;
                        // elaborates to `id[String](s)`
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

**Lowering.** A block-call splice arranges the receiver in its first or last
prefix group before block projection. Projection combines the final prefix value and descriptor-selected block
values into one product application checked against the resolved public type. Other UFCS forms survive through the resolved and
lowered typing phase: the typer plans value-slot insertion from the actual
call type and records a normalized prefix call or user-macro expansion. The
substitute pass swaps these recorded replacements in at the Lowered → Prime
boundary. No splice or block-call metadata survives into Kio'.

The Kio' grammar has no UFCS syntax in expression position; the kio-prime backend's parser rejects it the same way it rejects every other Kio-surface-only form. **Surface preservation:** `kio fmt` round-trips the user-authored spelling (UFCS vs prefix, and `.>` / `.>>` / `.<` / `.<<`) for both regular calls and macro call splices — see [`style.md` § UFCS preservation](style.md).

**Macro call splices.** Blockless macros use the same written stages as
ordinary calls. Parentheses on the forward callee hold value arguments;
brackets on it instantiate binders before receiver insertion. For a
source-to-target macro, the target binder follows the source stage:

```kio
r.>iso           // = iso(r), target inferred
(r.>iso)[T]      // = iso(r)[T]
r.>>iso          // = iso(r)
(r.>>iso)[T]     // = iso(r)[T]
(iso.<<r)[T]     // = iso(r)[T]
(r.>into)[T]     // = into(r)[T]
(r.>onto)[T]     // = onto(r)[T]
(r.>align)[T]    // = align(r)[T]
(r.>ease)[T]     // = ease(r)[T]
r.>atom          // = atom(r), target inferred from source DNF
(r.>atom)[T]     // = atom(r)[T]
(derive.<cs)[T]  // = derive(cs)[T]
```

Direction changes placement only within the selected value group. These
unary source stages have no other written value, so first and last insertion
agree. No type argument is reserved by searching around the receiver.

**Trailing-block splices.** A macro with trailing descriptors still supplies
every block, in declaration order. All four directions insert the receiver
into the first prefix group forward or the last prefix group in reverse,
before projecting the blocks:

| Spelling | Equivalent prefix call |
| --- | --- |
| `r.>f(a) { B }` | `f(r, a) { B }` |
| `r.>>f(a) { B }` | `f(a, r) { B }` |
| `f(a) { B }.<<r` | `f(r, a) { B }` |
| `f(a) { B }.<r` | `f(a, r) { B }` |

Here `f` denotes an explicitly selected macro with the appropriate public
prefix and block values; `B` denotes its block contents. Each equivalence
preserves both the public call plan and runtime behavior of the generated code.
Insertion-last does not put the receiver after the projected block arguments.
The final prefix value and projected block values form one right-associated
product for the ordinary public-type call planner.

Prefixes use ordinary argument-list parentheses as in direct calls: `r.>f(a) { B }` and
`f(a) { B }.<r` are admitted. Reverse splices follow the complete chain,
including every labelled block. `f(a).<r { B }` is not an alternate placement,
and no further labels attach after the receiver. A reverse splice inside a
prefix, such as `f(g.<r) { B }`, remains an ordinary prefix expression.

A call admits one splice; grouping does not reset that boundary.
`((f(a) { B })).<r` is equivalent to the ungrouped form, but cannot splice a
second receiver into a call already spliced. A subsequent forward suffix
uses the completed result: `f(a) { B }.<r.>g` passes the result to `g`.
The reverse receiver remains tight: `f(a) { B }.<r(x).>g` inserts `r(x)`,
while `f(a) { B }.<(r.>g)` inserts the grouped expression's result.
A result application such as `f { B }(x)` cannot be reopened as the original
block call by a reverse splice. Nor does a block result become a callee merely
because its inferred type is callable.

When the selected call has a final labelled child, reverse UFCS requires that
label's braces to be explicit, even through parentheses around the whole call.
This is a source-structure rule independent of names, declarations and types;
see [Block calls](#macros-are-imported-not-ambient). The diagnostic offers
braces around the final child, preserving the outer receiver insertion.

The nonempty-list rule applies to block-call splices too. `f() { B }.<r`
and `r.>f() { B }` reject; omit the list for no extra prefix value, or write
`(())` for an extra Unit. This does not change the direct call
`f() { B }`, which has one Unit prefix.

### Operators

*Surface only; desugars to Kio'.*

Kio supports function-backed fixed operators via top-level `op` declarations,
macro-backed fixed operators via `op macro`, and variadic operators via `varop`.
Parsing records an operator expression under its explicit grammar; operator
lowering replaces it with a call to the explicitly selected function or macro:

```kio
fn add(a: Int, b: Int) -> Int { … }
op _ + __ { impl add; };

fn sum_three(x: Int, y: Int, z: Int) -> Int {
  x + y + z      // = add(x, add(y, z))
}
```

The `impl` field uses the same lexical value-path grammar as every fold
callable and macro implementation. It may name a local or selectively
imported function, select through an explicit module alias, or select a
constructor/projector on a local or imported nominal type, directly or through an
identity alias. It is not an arbitrary expression and does not admit a
slash-qualified package FQN.

An `op macro` target is an earlier same-module macro or an explicitly imported
macro. The kind is part of the declaration and import contract: an ordinary
`op` cannot select a macro, and `op macro` cannot select a function. Macro
operators expand through the ordinary macro-call rules and introduce no
compiler privilege for a particular library or operation name.

These callable fields use ordinary path-expression source-order classes. A
same-module ordinary or host `fn`, `rec` member, nominal head, or identity-alias
head must precede the `op` or `varop`. Self-qualification and selective self-imports
do not bypass the declaration-order rule.

Operator syntax is source ordered too: an expression may use an exactly
imported operator or a local `op` or `varop` declared before its enclosing item.
Function bodies and recursive-group members do not see operators declared
later in the module. Deferring a body's parsing does not change that scope.

**Pattern shape and operand permissions.** A fixed pattern alternates value
slots and single contiguous symbol words, starting and ending with either.
It contains at least one slot and at least one word. Thus `!? _`, `_ + _`,
and `_ ? _ : _` are shapes; `! ? _`, adjacent slots, and a word alone are
rejected. A fixed operator has no zero-argument form; use an ordinary named
call for that operation. All slots take value expressions, never raw types.
An implementation's generic arguments are inferred by the ordinary call rules.
Grouping does not turn `A` into a value operand; an ordinary value expression
such as `f[A]` or `A.member` retains its value role. This syntactic separation also applies to ordinary calls: square brackets
supply types and parentheses supply values, independently of the imported
operator set.

The slot kinds specify the same permissions at every position:

- `_` admits an atom, including an explicitly parenthesized expression.
- `__` additionally admits an unparenthesized chain of this same operator.
- `___` additionally admits an unparenthesized chain of another operator.

Each nested operator still obeys its own slot permissions. An any-chain slot
does not establish precedence between operators inside it. Patterns may have
any number of these slots, subject to the structural restrictions below.
Runs of four or more underscores are reserved.

**Delayed operands.** Prefixing a slot with a dot (`._`, `.__`, or `.___`)
preserves its parsing permission and wraps the entire parsed operand in an
ordinary Unit-argument lambda. For example, `op macro _ && .__ { impl and; };`
makes `a && b && c` call `and(a, .() { and(b, .() { c }) })`. The same
markers work with function-backed operators. All transformed operands form
one ordinary product argument in written order; a delayed slot does not add
a function-application stage. The dot accepts ordinary token trivia and is
formatted adjacent to its underscores.

Wrapping occurs exactly once, even when the written operand is already a
thunk. There is no type-directed unwrapping. Constructing a projected lambda
does not execute its body; the selected callable determines whether and how
often it invokes that lambda. An unmarked operand retains ordinary call
semantics. Macro trailing-block descriptors do not affect operator operands:
they govern only direct block syntax, such as `and(a) { b }`. That spelling
and `a && b` both supply `and(a, .() { b })` under the declarations above.

**Atoms and final words.** Native atomic expressions, grouped expressions,
variadic literals, and completed native calls or forward-UFCS expressions are
atoms. A fixed expression whose pattern ends in a fixed word also completes
an atom, regardless of whether the pattern is prefix, postfix, or has several
operands. A pattern ending in a slot has no such boundary. Consequently `_ ?`
admits `a ? ?`, and `_ < _ >` admits `row<key>(arg)` with the call applied to
the indexing result. In contrast, `! _` requires `!(!a)` for self-nesting;
`! __` admits `! !a`. There is no automatic prefix-unary exception.

| Shape                | Pattern                | Chain meaning                 |
|----------------------|------------------------|-------------------------------|
| Binary non-assoc     | `_ OP _`               | parse error on chain          |
| Binary right-assoc   | `_ OP __`              | `a OP b OP c` → `OP(a, OP(b, c))` |
| Binary left-assoc    | `__ OP _`              | `a OP b OP c` → `OP(OP(a, b), c)` |
| **Right-greedy binary** | `_ OP ___`          | `f $ a + b` → `OP(f, add(a, b))`; same-op chains right-fold |
| **Left-greedy binary**  | `___ OP _`          | terminates any preceding chain so the chain-so-far becomes the left operand |
| Prefix self-nesting   | `OP __`               | `OP a` → `OP(a)`; `OP OP a` → `OP(OP(a))` (right-assoc) |
| **Prefix-greedy**    | `OP ___`               | `$ a + b` → `OP(add(a, b))` when `OP` is `$`; operand admits a chain |
| Postfix unary        | `_ OP`                | `a OP` → `OP(a)`; `a OP OP` → `OP(OP(a))` (each result is an atom) |
| Ternary right-assoc  | `_ OP1 _ OP2 __`       | C-style: `a ? b : c ? d : e` → `OP(a, b, OP(c, d, e))` |
| Ternary mid-rec      | `_ OP1 __ OP2 _`       | `a ? b ? c : d : e` → `OP(a, OP(b, c, d), e)`          |
| Ternary non-assoc    | `_ OP1 _ OP2 _`        | parse error on chain          |

**Structural restrictions.** A `__` must add an actual self-nesting permission.
It is rejected in every pattern ending in a fixed word, because such a child
is already an atom. In an operand-ending non-prefix pattern, a nonleading
`__` is also ineffective when its immediately following word equals the
pattern's first word: that following boundary owns the word needed to start
the child. For example, `_ ? __ ? _` is rejected, while `_ ? __ : __` has two
useful self-recursive positions. Other `__` positions in operand-ending
patterns are useful: a prefix slot, a leading slot, or a later slot with no
following word or a different following word.

An operand-ending non-prefix pattern cannot enable chaining at both exposed
ends. Its leading and final slots cannot both be `__` or `___`, in any
combination; `__ + __`, `__ + ___`, `___ + __`, and `___ + ___` are rejected.
Bounded interior chain slots do not conflict merely by being multiple.
Parentheses in a pattern quote one operator word, such as `(=)`; they do not
change a slot's permission. Pattern groups containing slots are rejected.

**Variadic operators.** `varop` declares delimited collection syntax. Its head
is a structural square frame, either bare `[]` or marked by a symbol word
inside each square edge:

```kio
varop [* *] { foldr cons nil; };
varop [! !] { foldl insert_entry empty_builder; };
```

For a marker `M`, OPEN is `[` followed by `M` and CLOSE is the reversed `M`
followed by `]`. The marker contains neither dots nor brackets and obeys the
ordinary symbol-word restrictions. The empty marker gives bare brackets.
Examples are `[]`, `[* *]`, `[** **]`, and `[!? ?!]`. Brackets outside the
square edges, multiple bracket pairs in a head, and fixed `_ [ _ ]` patterns
are rejected. Square-bracket indexing is not native syntax; a library can
declare a fixed pattern such as `_ < _ >` instead.

Compact and spaced spellings select the same frame at legal lexical
boundaries: `[**]` is empty marker `*`, `[****]` is empty marker `**`, and
`[*[**]*]` is marker `*` containing one empty marker-`*` literal. An otherwise
empty uninterrupted symbol word is split only at its mirrored midpoint;
an odd or nonmirrored word is rejected. Declarations and imports accept the
same empty-frame spellings. Canonical output uses `[]` for bare frames and
spaces for marked frames: `[* *]`, `[** **]`, and `[* a, b *]`.

Frame recognition uses written bracket nesting and whole edge words before
binding lookup or element parsing. An opening edge's adjacent ordinary-symbol
word determines the whole marker; a closing edge must match it in full.
Multiple separately written symbolic words at an edge are rejected rather
than merged, and a wrong edge never retries a shorter marker. Thus
`[*|x|*]` selects marker `*|`, not `*` around `|x|`; to put `|x|` inside marker
`*`, write `[*(|x|)*]`. Spaces alone in `[* |x| *]` do not supply that grouping.
Commas can supply element boundaries, but formatting may replace an edge
comma with grouping to preserve the selected frame. Signed literal tokens
retain their lexical role: `[* -1 *]` has marker `*` and element negative one,
whereas `[*-1*]` starts marker `*-` and has the wrong closing edge. A compact
spelling of the former is `[*(-1)*]`.

Precisely, an empty square pair is bare. A pair whose first direct token is
not an ordinary symbol word is also bare, and its contents are the value
comma-list region. If the whole contents are one legal word, the unique
mirrored midpoint must divide it into the two empty-frame markers. Otherwise
the first direct word is the whole opening marker and the last direct token
must be its exact reversal. Between marked edges, a nonempty region cannot
begin or end with an ordinary symbol word: grouping, nested structural frames,
or edge commas provide boundaries, and signed numeric tokens are literals.
Parse that fixed region as the value comma-list, then resolve the marker's
binding. For bare frames a final postfix word needs no marker protection,
so `[x?]` can contain `x?`; a leading symbol needs grouping or an edge comma.

Each comma-separated element at a use site is one value expression, not a
type or forall-binder entry. Grouping a raw type does not make it a value.
Native `[A]` and `[*F]` type binders keep their type grammar; they cannot serve
as value elements or operator slots. The ordinary comma-list rule applies,
including leading, repeated and trailing
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
result is the accumulator itself; `finalize ()` explicitly selects that same
absence. A repeated `finalize` field is rejected even if one value is `()`.
Calls follow ordinary Kio typing and
application rules; the grammar introduces no special accumulator type, library
identity, or element-specific argument adaptation.

Every `step`, base/seed, and finalizer is a lexical value path: a bare local or
selectively imported value, a dotted path through an explicit module alias, or
an ordinary nominal member through a local/imported type or identity alias.
Calls, lambdas, and slash-qualified package FQNs are not callable fields.
As with a fixed operator's `impl` field, parentheses may group each complete
reference repeatedly without changing its meaning. Required step and base/seed
fields still require names; only the optional finalizer admits `()`.
Argument adaptation belongs in an ordinary named helper. Callable source order
and visibility are the same as for fixed operators.

Variadic square frames are distinct from fixed-operator words, which contain
neither dots nor brackets. Their nesting and comma-separated expression boundaries
are determined by the written source, without consulting operator declarations
or imports. A literal still requires its `varop` binding to be in scope for
resolution and lowering.

{:#operator-grammar}
**Complete operator grammar.** The public identity of an operator selection is
its complete tagged grammar. Fixed forms include function/macro kind, every
literal run, every slot's extent and delay marker, and all later slots.
Variadic forms contain
the same OPEN and CLOSE in declarations and imports; the import omits only
the callable body. For example:

| Declaration | Import selection |
|---|---|
| `op _ + __ { impl add; };` | `op _ + __` |
| `op macro _ && .__ { impl and; };` | `op macro _ && .__` |
| `op _ ? _ : ___ { impl cond; };` | `op _ ? _ : ___` |
| `op _ <\| ___ \|> { impl index; };` | `op _ <\| ___ \|>` |
| `varop [* *] { foldr cons nil; };` | `varop [* *]` |
| `varop [! !] { foldl insert empty; };` | `varop [! !]` |

Canonical rendering retains distinctions between complete grammars, including
slot kinds, whole-word boundaries, and required contextual operator-token
quotation in fixed patterns. Variadic heads retain the selected square frame
and marker; the comma separator is implicit and is not written in the head.

**Dispatch and conflicts.** Parsing dispatches on the operator's expression-start
position and first fixed word; variadic bindings dispatch on their marker. This
shorter key is deliberately distinct from the complete public grammar. It
prevents two incompatible parses in one scope: two bindings with the same
position and first word conflict even if their tails, slot kinds, delay
markers, or function/macro kinds differ.
Two variadic bindings with the same OPEN also conflict; CLOSE is fixed by its
mirror. Fixed and variadic keys cannot collide because fixed tokens exclude
square brackets. Public imports, diagnostics, and
queries retain the complete grammar rather than displaying a shortened key.

**Module-local by default; `pub` exports the binding.** Importing an ordinary
function does not import an operator bound to it. An unmarked fixed or variadic
operator is local; `pub` and `pub(path)` export it under the ordinary visibility
rules. Every directly named implementation, step, base/seed, and finalizer must
have visibility at least as wide as the operator. For a nominal-member path,
both the type and selected member meet that floor. An identity-alias-qualified
member is the terminal member for this rule: the written alias and terminal
member each cover the referring operator's visibility.

{:#operator-pattern-imports}
**Operator imports.** An explicitly tagged selection imports an exported
operator alongside ordinary names:

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
slot, token, or delimiter pair is an import error; provider data never
changes the already-written consumer grammar. The selected declaration supplies
the callable paths, fold mode, and finalizer for lowering. Changing those fields
does not change the grammar a consumer must write.

The implementation function or macro need not be co-imported. The operator import itself
is the explicit dependency; lowering records the exact resolved implementation
and the ordinary qualified imports needed by the resulting calls. A provider's
callable field reaches another module only through an ordinary selective import
or explicit module alias, never by a slash-qualified implementation field.
Visibility is checked on every directly named callable as described above.

**Import aliases.** `import arithmetic(op _ + __ as _ ++ __);` selects the
complete source grammar and installs only the complete destination grammar.
The function/macro kind, every token/slot position, slot kind, delay marker, and word boundary
is preserved. Literal tokens may change by a consistent injective substitution:
each repeated token keeps one replacement and distinct tokens stay distinct.
`import collections(varop [] as [* *]);` similarly changes only the admitted
mirrored delimiter pair, preserving fold direction, initializer, step and
finalizer. Reserved syntax and structural pattern checks, including `__`
usefulness, apply to the local grammar. An alias never changes the callable's
type or argument order.
Distinct destination grammars may select one source declaration; they remain
distinct operators for chaining, even when their callables coincide.

**Conflict rule across import layers.** Each explicit local operator grammar
may be introduced once, even when repetitions name the same provider and declaration.
Imports also conflict with other imports or local fixed/variadic declarations
that occupy the same local dispatch key. These are Name errors. There is no
same-provider exception. Validation consumes the written import environment
and module-cycle graph; this prerequisite does not establish diagnostic
precedence when independent errors coexist. Distinct dispatch keys remain
separate; a word is never decomposed to select an imported shorter spelling.

**Open-world.** A consumer's parse depends only on its complete written imports
and preceding local declarations. Resolution compares each selection with the
one explicitly named provider and grammar; it does not search newly added
exports for omitted pattern details or callable candidates. Adding an unrelated
provider declaration therefore cannot change the consumer's syntax, chosen
operator, or callable resolution. A duplicate provider key is a same-module
conflict, not a silent rebind. Lowering makes the resolved ordinary dependencies
explicit, and downstream phases perform no fresh provider search.

**Grouping and chain extent.** There is no numeric operator precedence.
With `_ + _` and `_ * _`, `a + b * c` is rejected; write
`a + (b * c)` or `(a + b) * c`. Declared chain slots and final-word atoms
provide the unparenthesized compositions described here.

A following fixed word bounds its operand: the innermost pending following
word owns that whole word before another operator can dispatch on it. A prefix
whose final slot has no following word inherits the enclosing boundary. Words
are compared in full; `|>|>` cannot supply two separately written `|>` closes.
Thus `_ <| ___ |>` admits `a <| b + c |>` when `_ + _` is declared;
a separate `_ |> _` cannot steal that closing word. Grouping
`a <| (b |> c) |>` instead exposes the different inner operator.

At a non-prefix operator, a leading chain slot takes the widest structurally
finished preceding expression that its permission admits. For this decision,
the expression's outermost operator determines whether it is the same
operator. A leading `__` takes the widest expression with that same outermost
operator, or the immediate atom; a leading `___` takes the widest expression.
Parentheses, native
argument/list boundaries, and an enclosing pattern still awaiting its own
fixed word bound that extent. A preceding operand-ending pattern is
structurally finished once its final operand has started. This rule selects
the grouping before checking operand validity; a failure never retries a
shorter extent.

For example, with `_ + _`, `! _`, and `___ < _ >`, the source
`a + !b < k >` gives `<` the whole preceding `a + !b`. That expression then
fails because the strict right slot of `+` does not admit bare `!b`.
The invalid operand does not make `<` take only `b` to rescue the expression.

If that left extent crosses an operand-ending operator whose final slot
also admits the incoming operator's bare chain, the two declarations make
opposing claims. Parentheses are required regardless of which alternative
would typecheck. With `_ $ ___` and `___ < _ >`, therefore,
`a $ b < k >` is rejected. Write `(a $ b) < k >` or
`a $ (b < k >)`.

Native calls and forward UFCS after a final fixed word attach to that completed
result. After a pattern ending in an operand they attach to its final operand.
A suffix never repairs an invalid operand within the selected grouping.

| Declared patterns | Source | Meaning or required grouping |
|---|---|---|
| `_ + _`, `_ ?` | `a ? + b` | `(a ?) + b`; the postfix result is an atom |
| `_ + _`, `! _` | `!a + b` | rejected; write `(!a) + b` |
| `_ + _`, `! _` | `a + !b` | rejected; write `a + (!b)` |
| `___ + _`, `! _` | `!a + b` | `(!a) + b` by the declared left extent |
| `_ + _`, `___ < _ >` | `a + b < k >` | `(a + b) < k >`; group `b < k >` for the other reading |
| `_ + _`, `_ < _ >` | `a + b<k>(x)` | `a + ((b<k>)(x))` |
| `_ + _` | `a + b.>f` | `a + (b.>f)` |
| `_ + _`, `! _`, `___ < _ >` | `a + !b < k >` | rejected: the maximal left extent contains an invalid strict operand |

**Symbol words and native structure.** Ordinary operator words are maximal
contiguous runs from the ASCII allow-list
``+ - * / % ^ ~ ? @ # $ \ ' ` < > = ! & | :``.
Dots and square brackets are structural tokens and cannot occur in a user
operator word or marker. Native dot-led forms include qualification, member
access, UFCS, booleans, lambdas, and placeholder stems. These roles are
recognized independently of operator imports. Compact native syntax such as
`type A=.;`, `[*F]`, `[A][B]`, and `.[*F]` remains structural.

Whole words are never split according to shorter imported operators:
`!? _` is one component; `! ? _` is rejected. At use sites, `?+`,
`??`, and `><` are each one word, regardless of available bindings.
Whitespace may separate complete words; it cannot split or join them without
changing the spelling. Required native adjacency and numeric/string token
boundaries remain load-bearing. In particular `! !a` and `!!a` differ,
and `1 + -2` and `1+-2` have different token boundaries.

A symbol word contains at most one slash; `//` opens a comment and comment
markers retain their trailing-whitespace requirement. `#`, apostrophe, and
backtick are ordinary operator characters. Unicode symbols are not operator
characters.

**Operator-token quotation.** A standalone `=` requires quotation in a
declaration or import: `op _ (=) _ { impl my_eq; };`. Use sites write
`a = b`. Quotation encloses exactly one word and does not admit dots,
brackets, or slots. `:` and `->` can appear directly in a pattern; their
native structural meanings remain fixed by the surrounding grammar.

**Conflict keys.** Prefix and non-prefix fixed patterns have separate
keyspaces, so prefix `- __` and binary `_ - __` can coexist. In either
keyspace, the first whole fixed word uniquely selects a binding. Patterns
`_ + _` and `_ + _ ? _ : _` conflict even though their complete grammars
differ; `_ && _` and `_ &&-- _` have distinct whole words and can coexist.
Variadic keys are the structurally selected markers, including the empty
marker for bare brackets. No provider census or successful trial parse chooses
a key.

**Lowering.** The operator-fold pass replaces fixed-operator applications and variadic literals with ordinary calls using the consumer's explicit operator scope. Fixed and variadic declaration records and tagged import selections are consumed before Kio'. Kio' shares the symbol-word lexer and structural delimiters but has no `op` or `varop` declaration, operator import, or user-operator expression production.

**Out of scope.** Bare-operator-as-value (`map(+, xs)`) is not supported.

### Equivalence claims (`equiv`)

*Surface only; desugars to Kio'.* `equiv` items are filtered before codegen — they contribute nothing to the build artifact. The Kio' grammar has no `equiv` production; `kio-prime` rejects `equiv` items at parse time.

`equiv` declares a **test**: a claim that several expressions reduce to the same normal form. The `kio test` tool discharges these claims by partial evaluation; outside that tool, `equiv` items are silently filtered before codegen and contribute nothing to the build artifact.

```kio
fn id_unit() { () }

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
- **Parameters reuse `fn`'s grammar.** Each square-bracket group binds one type parameter; each parenthesized group binds value parameters, all annotated. The groups are optional (a name with no parameter groups declares a zero-parameter `equiv`). Parameters bind in the body's scope; each arm sees the same binding set. Parametric equivs are claims for every instantiation of their parameters. The comparison rules are described below.
- **At least two arms.** `N=0` and `N=1` blocks are parse errors — there is nothing to compare.
- **No visibility.** `pub equiv …` is a parse error. Equivs are not part of any public API surface.
- **Arm syntax.** Arms are semicolon-separated expressions. Repeated semicolons are accepted and ignored; a final semicolon after the final arm is accepted and ignored. Comma-separated arms are rejected with a migration diagnostic. An arm that needs local statements uses the imported `scope { ... }` macro.

During discharge, type parameters represent arbitrary types shared across the
arms. Each value parameter is a fresh opaque atom shared across the arms. A
type application to an opaque polymorphic value retains its type argument for
comparison; distinct type arguments do not become equal merely through erasure.
Known type abstractions reduce by substitution, and known type-insensitive
primitives consume type stages under their own rules. Generic function
comparison uses an arbitrary type, not a concrete choice or a particular
caller's rigid type variable. See [the formal comparison](formal/equiv.md#4-equivalence-relation).

**Typing.** Each arm is typechecked in the parameter scope; all arms must synthesize the same type under ordinary definitional equality. A complete polymorphic function scheme is a value type here just as it is in every other value position; incomplete schemes remain ill-kinded as values. A type mismatch is a regular type error, not a test failure.

**Build artifact.** `equiv` is surface-only: the compiler's substitution pass at the `Lowered → Prime` boundary filters `Item::Equiv` out, so codegen never sees these items and the emitted artifact contains nothing for them. `kio-prime` (the Kio'-only compiler) rejects `equiv` at parse — it is not part of the Kio' grammar.

**Discharge** is the responsibility of the `kio test` tool: see [`specs/cli.md`](cli.md) for the subcommand and [`specs/exit-codes.md`](exit-codes.md) for the test-failure exit code. The tool's reduction strategy and equivalence relation (α + η on residual normal forms, with host items treated as opaque atoms) are documented formally in [`specs/formal/equiv.md`](formal/equiv.md). Host calls are compared by residual structure; an extra unit-returning host call in an expression statement is not erased just because its result is unused. Reduction strategy doesn't matter — Kio' is strongly normalizing and confluent, so every sensible strategy reaches the same residual normal form.

### Literals

Four literal **shapes** are recognized — string, integer, float, and bool. String, integer, and float are lexer tokens; bool is the parser-recognized token sequence `.t` or `.f`. A literal carries no width suffix; its type is a `host type` carrying a `role(...)` marker (see [Host declarations and the bridge block](#host-declarations-and-the-bridge-block)), selected by the literal-type rule below.

- **String** — double-quoted, with JSON-style escapes interpreted at compile time: `\"`, `\\`, `\/`, `\b`, `\f`, `\n`, `\r`, `\t`, and `\uXXXX` for code points in the Basic Multilingual Plane.

    ```kio
    let s = "hello, world\n"[String]; // s : String
    ```

    **Adjacent literal concatenation.** Two or more string literal tokens with only trivia (whitespace, newlines, comments) between them fold at parse time into a single string literal whose value is the concatenation. The chunking present in the source is informational only — the AST stores the joined value, and `kio fmt` re-chunks deterministically on emission.

    ```kio
    let greeting = "hello, "  "world"  "\n";          // == "hello, world\n"
    ```

    Pure parser-side rule: no operator is added, and the rule does not extend to any other literal shape (integers, floats, booleans don't fold).

- **Integer** — decimal digits, optionally grouped with `_` (`1_000_000`).
  Integer literals have no exponent notation. Every point-free exponent form
  is rejected, including `42e0`, `42E+3`, `42e-3`, and `42e-0`. Write ordinary
  integer digits, or `42.0e3` when a floating-point literal was intended.
  A leading `-` adjacent to a digit is part of the literal where the preceding
  token cannot end an expression; after an identifier, literal, `)`, `}` or
  `]`, it is an operator instead. Thus `[-1]` contains a signed literal,
  while `a-1` has subtraction shape. See [`grammar.md`](grammar.md#lexical-structure-kio).
  There is no hex/binary/octal syntax or numeric width suffix.
  Once its host type is selected, the mathematical value must fit that
  type's role: `iN` admits −2^(N−1) through 2^(N−1)−1, and `uN` admits
  0 through 2^N−1. Out-of-range values are Kio type errors. Leading zeros
  and digit separators do not change the value; integer `-0` is zero,
  including at unsigned roles. An unused literal alias remains untyped;
  these checks apply when it is expanded at a typed use.

- **Float** — decimal digits with a `.` followed by fractional digits and an
  optional `e`/`E` exponent with optional sign (`3.14`, `1.0e-9`, `2.0e10`).
  A decimal point is required: `2e10` is rejected, even in a floating-point
  expected position. Digits are required
  on both sides of the point; `.5` and `1.` are not numeric literals.
  Digit separators and the leading-`-` rule apply as for integers.
  The selected role gives the literal its IEEE 754 binary32 (`f32`) or
  binary64 (`f64`) value. The exact decimal is rounded once directly to
  that format, using round to nearest, ties to even. Overflow to infinity
  is a Kio type error. Subnormal values and the sign of zero are preserved;
  a nonzero decimal may underflow to signed zero. Host operations on the
  resulting value retain their host-defined behavior.

- **Bool** — `.t` or `.f`. These spellings are recognized only in expression-leading literal position as `.` followed immediately by identifier `t` or `f`; the words `true` and `false` are ordinary identifiers.

**The literal-call annotation form.** Any literal may carry a trailing type in square brackets — `42[I32]`, `3.14[F64]`, `"hi"[String]`, `.t[Bool]` — naming the host type the literal should take. This is the **`LiteralCall`** form (see [`grammar.md`](grammar.md)). The annotation is optional in the full surface language and **mandatory in Kio'** — Kio' admits no unannotated-literal form, so every Kio' literal carries its annotation by parse rule (see [`grammar.md` § Kio' grammar](grammar.md#kio-grammar) and [`prime.md` § Host declarations](prime.md#host-declarations)). Full-Kio checking records the inferred literal carrier on the Lowered tree, and the Lowered → Prime substitute pass materializes it on the literal node.

**The admission relation.** A literal of a given shape may be typed as a role only when the role's shape *admits* the literal's:

- An **integer** literal admits integer-shaped roles (`i8`, `i16`, `i32`, `i64`, `i128`, `u8`, `u16`, `u32`, `u64`, `u128`) only.
- A **float** literal admits float-shaped roles only.
- A **string** literal admits the string-shaped role (`str`) only.
- A **bool** literal admits the boolean-shaped role (`bool`) only.

**Literal-type inference.** An explicit literal annotation such as `42[I32]`
selects the carrier. It must normalize to a role-bearing host type whose role
admits the literal's category. Compiler-owned Boolean and text types have no
host literal role; their construction uses the separate proof-bearing form
below.

Without a concrete annotation, a scalar literal obtains its carrier from
concrete local context: an expected return, argument or binding type, or the
finite connected call constraints described in [Bidirectional elaboration](#bidirectional-elaboration).
A placeholder annotation such as `42[_]` leaves the carrier to that same
inference. If it remains unresolved at the existing local boundary, the
literal is a type error; write an explicit annotation. A role declaration or
import never supplies a candidate merely by being visible. There is no
ambient role census, fallback carrier or program-wide inference.

A concrete `let` binder can provide the expected type of its initializer.
An untyped or partially typed `let` initializer instead synthesizes before
its tail is examined: later uses cannot select the earlier literal's type.
For example, `fn number() -> I32 { 42 }` determines `I32`, while
`let number = 42; consume_i32(number);` requires an annotation at the literal
or a concrete binding pattern before the semicolon.

Admission follows the selected type. `42[Float]` is an error; use
`42.0[Float]` or a floating literal in a concrete float context. Conversely,
`42.0[Int]` does not become an integer because its value is integral.
For `same : [A] (A & A) -> A`, both `same(42, 42.0)` and
`same(42.0, 42)` reject: argument order cannot authorize cross-category
conversion. Numeric ranges and IEEE rounding are checked at the resolved
role; failure never retries a wider type or another carrier.

**Role-inheriting aliases.** A transparent alias names the same type as its
normalized body, including through saturated alias applications. If that body
is a role-bearing host type, annotations and concrete contextual inference
use its role. An alias changes neither literal category nor host identity,
and its presence does not select an unresolved literal.

**Compile-time literals.** With `import __comptime__;`, the full surface
language admits `__.t__(ct)`, `__.f__(ct)` and `__"text"__(ct)`. The argument
is one ordinary expression of type `__Comptime__`, evaluated once to provide
the proof that construction occurs during compile-time evaluation. The Boolean
forms construct `Comptime_bool`. A concrete expected type selects
`Comptime_str` or `__Diagnostic_text__` for the string form, including through
transparent aliases; an unresolved or different carrier is an error. String
contents use the ordinary string escape rules. These forms construct values
used by the macro implementation, not quoted runtime literals.

Compiler-owned types and operations do not survive into persistent Kio'.
Their runtime type is bottom, including Boolean and text carriers; a retained
proof-taking operation eliminates its bottom-typed proof. Compile-time
evaluation uses an explicit primitive environment instead. The proof-bearing
literal syntax itself is surface-only. See
[compile-time values](../docs/guides/macros.md#reflected-types-and-checked-terms).

The full-Kio typer records each inferred scalar carrier on the Lowered tree.
The Lowered → Prime substitute pass materializes it as an explicit annotation.
Kio' requires these concrete annotations; it admits no unannotated scalar
literal or `_` literal annotation. The lexer emits the same token shape
regardless of the available types; unresolved or inadmissible carriers are
type errors. `42[I32]` is a literal with an annotation, not a function call.

**Source-syntax vs emitted-syntax.** The lexer productions above are Kio's surface syntax. Checking establishes the numeric value before normalization or host emission. Kio' and each backend preserve that value independently of source spelling: an integer denotes its checked mathematical value, and a float denotes its selected IEEE value, including its zero sign. For example, an `i32` literal may become a JavaScript number and an `i64` literal a BigInt. The host representation or constructor is a backend concern; interpreting source spelling or choosing a different numeric value is not. Custom host carriers must preserve their role's literal values; the compiler does not police host implementations at runtime.
