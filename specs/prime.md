# Kio'

*Pronounced "Kio prime". The apostrophe form (**Kio'**) is the canonical spelling in prose; identifiers that need a separator-friendly form use **`kio-prime`** in code (internal compiler binary, build-target id, file names) and **`KIO_PRIME`** in environment variables and marker files (e.g. `IS_KIO_PRIME`).*

**Kio'** is the small, desugared subset of Kio. It contains no elaborator and no syntactic sugar. Every full-language feature is either present here in its minimal form, or defined in [`language.md`](language.md) by how it expands into a Kio' construct.

Kio' exists for three reasons:

- **Self-hosting.** Kio' is small enough to be a plausible compilation target for a Kio-in-Kio compiler.
- **Semantic ground.** Kio' gives the evaluator and open-world argument their explicit core language and has its own standalone checker. The full Kio checker first types the surface-lowered tree, then substitutes every recorded completion into Kio' and runs that standalone checker over the assembled artifact. This is the same core users observe through `equiv` discharge (see [`cli.md` § `kio test`](cli.md#kio-test)), which compares Kio' normal forms, and `:normalize` in `kio repl` (see [`cli.md` § `kio repl`](cli.md#kio-repl)), which renders the evaluator's Kio'-core residual normal form. A residual that mentions evaluator-local closure or context state is not necessarily valid Kio' source text.
- **Loadable image.** Building a package to the `kio-prime` target (see [`cli.md` § `kio build`](cli.md#kio-build)) emits the whole package as a loadable image — one Kio' text file per module plus the emitted `.pkg.kio` package manifest, whose `bridge { … }` block declares the contract surface — which a host can bring online at runtime against a statically-typed interface. A loader trusts the image's bodies exactly as build-time linking trusts a compiled artifact; it does not run the Kio' typechecker, and successful loading is not a body-typing judgment. Before evaluation, the runtime gate matches every bridge-required host type and function against exact declarations offered by the host adapter; it separately contract-matches the callable export surface the host intends to use (worked through in the [`dyn_load_prime` case study](../docs/poc/dyn_load_prime.md) and [Loading Kio' packages at runtime](../docs/guides/dynamic-loading.md)).

## Kio' as Church-style fragment of System F-ω

Kio' is a **Church-style fragment of System F-ω**: every universal `[A]` and
existential `<U>` type binder is written explicitly, every type argument at a
call site is written out positionally, and — the F-ω part — type binders carry
an explicit **kind** drawn from the closed kind language
`κ ::= * | κ → κ` (see [`grammar.md` § Kind grammar](grammar.md#kind-grammar)).
A kind-`*→*`-or-higher binder admits direct type application `F(A)`, so Kio'
can quantify over type constructors, not only over saturated types. The typer
never invents a quantifier, kind, or universal call argument: every quantifier
and kind is user-written (the binder-annotation surface `[A]` / `[*F]` /
`[**G]` spells the kind out), so rank-N quantification stays admissible and
kind-checking is a finite structural walk.

**Fragment, not full F-ω.** Kio' admits higher-kinded *quantification* (a binder may range over a `*→*` constructor) and higher-kinded *application* (`F(A)`), but it does **not** admit anonymous type-level *abstraction* (`λα. T`) — there is no way to write an unnamed type-level function, only to name and apply constructors a `newtype` or binder introduces. The kind language has no kind variables and no higher-order kinds (the domain of every arrow is `*`). These restrictions keep the fragment strongly normalizing and the type system decidable, exactly as the System F core was.

The standalone Kio' checker is syntax-directed and local. Ordinary terms
synthesize bottom-up, while checking may pass an expected function type into a
lambda literal. One bounded synthesis rule is also local to a single
application node: when a literal lambda is the direct callee, that node's
independently synthesized
value arguments may supply whole missing parameter annotations before its body
is checked. Neither route infers a universal type argument, inspects a body to
discover a parameter type, looks through a `let`, or searches declarations
globally.

The full Kio front-end may use a finite connected application-local domain of
goals and one-shot retained source actions while it elaborates omitted
universal arguments. A nested call whose unfinished result participates in the
same relation may share that domain without sharing lexical binders. That is
typing administration, not Kio' syntax or semantics. Every goal and retained
action is consumed before Kio': inferred universal arguments are written into
calls, and no parallel inference state accompanies the resulting tree.

The Lowered → Prime boundary consumes an elaborator declaration's
ordinary-or-fills implementation mode and every compile-time capability used
by it.
`__Comptime__`, `__Fill_ctx__`, fill transcripts, provisional checked recipes,
and `__comptime__` helpers are not Kio' terms or types. A marked elaborator is
replaced by its completed checked term exactly as an ordinary elaborator is;
the resulting tree carries no marker, transcript, certificate, or staging
privilege and is validated as standalone Kio'.

This document is a strict subset of [`language.md`](language.md); read that one for the full surface.

## What's in Kio'

### Type system

System F + nominal existentials: functions and type parameters written as `[Name]` (universal binders), either in a declaration's parameter list or as a prefix binder run inside a function-type expression (`[A] A -> A` is `∀A. A -> A`). Each `Forall` node binds exactly one parameter; source binder runs are represented as nested unary `Forall` nodes. Rank-N is permitted; every binder is explicitly written, so type-checking stays decidable. **Existential** binders are written `<Name>` and appear only as a trailing binder run on a `newtype` declaration's header (`newtype Box[A] <U> : A & U`) — there is no standalone existential type expression. Constructor / projector pairs declared by the newtype are the introduction and elimination forms; the projector for an existential-bearing newtype carries a CPS scheme (see § The newtype primitive). Two anonymous type constructors — sum (`A | B`) and product (`A & B`) — are binary at the AST level. Product types model finite cartesian products, and tuple values are their elements, but Kio' commits to one binary representation: product type syntax uses `&`, and same-operator chains (`A & B & C`, `A | B | C`) are parse-time sugar that folds to the same right-associated tree, so `A & B & C` is `A & (B & C)` by definition. Empty product chains (`&`, `(&)`) fold to `.`, empty sum chains (`|`, `(|)`) fold to `!`, and one-item chains fold to the item; there is no one-element tuple type. Mixing `&` and `|` outside parens is a parse error — the user must write `A & (B | C)` or `(A & B) | C`. The unit type is written `.`, and the unit value is written `()`. The **bottom type** is written `!`: it has no values and no introduction form. Type equality is purely structural in Kio' — `A | !`, `A`, `(A & B) & C`, and `A & (B & C)` are distinct unless they have the same tree — with identity absorption and reassociation happening only at elaborator-call sites (see [`language.md` § Elaborators are imported, not ambient](language.md#elaborators-are-imported-not-ambient)).

**Functions are unary over one domain type.** Kio''s `Function` is a strict System F arrow with one `param` and one `ret`. Product domains are written with `&`: `(A & B) -> R` is `Function(Product(A, B), R)`. Declaration signatures may have comma-separated value parameters, but those commas are signature-list syntax; the function type they synthesize still has the right-folded product domain. A bare unary arrow `A -> B` is accepted and right-associates (`A -> B -> C` is `A -> (B -> C)`), but a product/sum parameter must be parenthesized (`(A & B) -> R`, not `A & B -> R`). The arrow's right side is a full type, so `A -> B & C` parses as `A -> (B & C)`. The formatter emits bare atomic domains and parenthesizes compound domains. See [`language.md` § Type parameters](language.md#type-parameters) for the right-fold rule.

**Function purity is part of Kio'.** An ordinary function declaration may be marked `pure`; no other declaration admits that modifier. The marker restricts executable value references in the function body to local binders, intrinsics and newtype members, and other ordinary functions marked `pure`. It does not classify types: local annotations may mention any well-formed type, including a host type. Named types in a declaration signature separately obey the visibility rule below. An unmarked function may refer to either kind of function. Surface compile-time primitives used by an elaborator implementation are consumed before persistent Kio' and are not Kio' values.

The marker remains in the Kio' artifact and its public function contract. Consequently a freshly parsed package checks the same promise as direct compilation, without relying on the producer's surface AST. The runtime loader recognizes the modifier but trusts the precompiled body and does not perform that check. Retaining the marker is also what lets the full Kio pipeline substitute an elaborator's generated term and then revalidate a pure caller's completed body.

**Type-expression binders are prefix-only.** A binder run scopes over the complete body to its right: `[A][B] (A & B) -> R` is `Forall(A, Forall(B, Function(Product(A, B), R)))`, and `[A] R` is `Forall(A, R)`. A binder run must have a body type; `[A] -> R` is not a type expression. Declaration signatures still use ordered type/value groups, with comma-separated value parameters inside one value group folding to that layer's product domain.

Nominal types are introduced by `newtype` declarations (see [The `newtype` primitive](#the-newtype-primitive)); each declaration is itself an **iso-recursive** wrap/unwrap boundary at the nominal name (see [Explicit recursive data declarations](#explicit-recursive-data-declarations)). There is no anonymous record or label type constructor in Kio': surface `labels` declarations generate ordinary `newtype`s, described in [`language.md`](language.md). Anonymous recursive type expressions are not in Kio' either — explicit singleton or mutual declaration scopes ground every recursive cycle at a nominal `newtype` boundary.

### Value construction and elimination

Kio' has **no value-level syntax** for constructing or destructuring sums or products. Every introduction and elimination is an ordinary function call — either a compiler-provided intrinsic or a user-named constructor/projector pair declared by a `newtype`. The six type-form intrinsics listed below, together with the branching intrinsic `__if_then_else__` (see [Branching intrinsic](#branching-intrinsic)) and the bottom-elimination intrinsic `__absurd__` (see [Bottom-elimination intrinsic](#bottom-elimination-intrinsic)), are not in scope by default; the magic phrase `import __intrinsics__;` at the top of a file brings all eight in at once. There is no per-name selective form: it's all or nothing.

| Type form | Introduce | Eliminate |
|---|---|---|
| `A \| B` | `__left__ : [A][B] A -> A \| B`, `__right__ : [A][B] B -> A \| B` | `__either__ : [A][B][C] ((A \| B) & (A -> C) & (B -> C)) -> C` |
| `A & B` | `__pair__ : [A][B] (A & B) -> A & B` | `__fst__ : [A][B] (A & B) -> A`, `__snd__ : [A][B] (A & B) -> B` |

Like every value application, `__either__(s, f, g)` evaluates `s`, `f`, and
`g` exactly once from left to right before dispatch. It then invokes only the
handler selected by `s`; constructing both handler values is eager, while
invoking the selected handler is branch-dependent.

Binary only at the value-construction layer: n-ary tuple values live in [`language.md`](language.md) and lower to intrinsic calls before Kio'. Type-chain spellings are parse-time sugar only; they fold to binary `&` / `|` ASTs before later phases. Nominal types are introduced via [`newtype`](#the-newtype-primitive), which attaches a user-named constructor and projector as members of the type, reached as `Name.c(...)` and `Name.p(...)`.

Kio' uses the same spelling-based namespaces as Kio. User type names and type
parameters match `_?[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*`: one optional
leading underscore, letter-first words separated by single underscores, and
any trailing underscores. Only the first letter is uppercase; each word's
digits follow all its letters. Thus
`_Box` and `_A` are type-shaped while `_box` is value-shaped; `_1Box`,
`__Box`, and `BoxCar` are invalid user type names. Classification is purely
lexical and never depends on declarations in scope. The optional underscore
is part of the exact name, so `Box` and `_Box` are distinct bindings and
references preserve the marked spelling. Its only special effect is to permit
that binding to remain unused; it grants no other semantic privilege.

Two or more leading underscores reserve a name for compiler hygiene; a trailing underscore is not required. Reserved names follow the same word and type/value role grammar as other names. User-written declarations may not use that prefix. Reservation grants no typing, visibility, or execution authority: a reference still resolves to its ordinary binding or one of the specified intrinsic declarations. The intrinsic spellings listed here and the `__intrinsics__` import target retain their exact spelling.

### Bindings

Top-level `fn`, `type` (nullary and parametric), and `newtype` (nullary and
parametric); `pub` visibility modifier; dot-led anonymous functions
(`.(...) { ... }`). **Block bodies** carry zero or more `;`-terminated
statements followed by a final expression — `let x = e;` binds a single name
(no annotation, no patterns, no destructuring) and `e;` is an expression
statement whose value is discarded (the typer requires `e : .`; use
`let _ = e;` to discard a non-unit value deliberately). Same shape as the
surface after surface-only local annotations have been erased — see
[language.md § Blocks and local bindings](language.md#blocks-and-local-bindings).
A top-level `fn`'s `-> Type` return-type annotation may be elided, in which case
the return type defaults to `.` (see
[language.md § Function definitions](language.md#function-definitions)).

A Kio' dot-lambda's value-parameter and return-type annotations are optional:
each slot is either concrete or absent. The `_` type placeholder is
surface-only and never appears in Kio'. A checked lambda may omit a
value-parameter annotation because its expected function type supplies that
slot. In synthesis mode, value parameters must otherwise be concrete, except
that the same application node may use its value arguments to supply whole
missing slots when the literal is its direct callee. An absent anonymous-lambda
return annotation reads its type from the body; only an absent top-level `fn`
return defaults to `.`. Every other type position — top-level binding
signatures, `newtype` bodies, `match!` clause parameters — is grammatically
mandatory and takes no `_`.

### The `newtype` primitive

A `newtype` declaration mints a nominally-distinct type with a single payload, plus an explicit user-named constructor and projector:

```
newtype Celsius : F64 {
  pub constructor mk_celsius;
  pub projector to_f64;
};

rec newtype List[A] : . | (A & List(A)) {
  pub constructor cons;
  pub projector un_list;
};
```

The outer `pub` on `newtype` and the `pub` on `constructor` / `projector` are independent; each defaults to private when omitted. Both items are required. The block names a constructor function with scheme `Name.c : [A...] Payload -> Name(A...)` and a projector function whose scheme depends on whether the newtype declares existential binders (see § Existential binders below for the CPS form); both are **members of the type**, reached by dotted path (e.g., `Celsius.mk_celsius(3.14)`, not an unqualified `mk_celsius(3.14)`). They are not free top-level values in the declaring module. A top-level newtype ends at `}` with at most one optional suffix semicolon; members are separated by semicolon runs, with optional edge runs. Within a recursive type group, declarations use the group's single separators, with optional leading/trailing separators. Canonical formatting omits edge separators and redundant outer suffixes. Inside the spec block, `constructor` and `projector` are **contextual keywords** recognized only at the block's item positions; outside a `newtype` block they are ordinary identifiers.

A fully saturated **identity alias** may qualify those existing members. Each
edge in such an alias chain must apply its next type head to every alias binder
once, in order, with the same effective kind. Thus
`type Wrapped[A] = Box(A);` admits `Wrapped.mk_box`, with exactly the member
scheme and nominal result of `Box.mk_box`. A partial, reordered, structural,
cyclic, or missing alias target exposes no member namespace. The alias head
must be lexically in scope, and the terminal constructor or projector must be
visible independently. Tooling therefore resolves the written head to the
alias declaration and the member leaf to the terminal newtype member. The
alias does not mint a constructor, projector, nominal boundary, or reduction
rule; this rule is identical for fresh textual Kio' and for Kio lowered from
the surface language.

**Existential binders.** A `newtype` declaration may carry an existential binder run, written as whitespace-separated `<X>` atoms trailing the universal-parameter list on the header (e.g., `newtype Pack[A] <U> : A & U`, or `newtype Pack[A] <L> <R> : A & L & R` for multi-binder runs). The names are bound inside the payload only and do not appear in the type's nominal arity — `Pack[A] <U> : A & U` is referenced as `Pack(A)`, not `Pack(A, U)`. **Each binder must occur at least once in the payload** — a phantom existential (a binder that never appears in the payload) is information-free and rejected at declaration.

The constructor scheme extends with the existentials as additional inference-driven type-params: `Name.c : [A...][U...] payload -> Name(A...)`. At the construction call site the existentials are recovered from the value-arg's type, the same way reachable universals are.

The projector scheme is **CPS-shaped** for existential-bearing newtypes:

```text
Name.p[A...] Name(A...) -> [R] ([U...] payload -> R) -> R
```

— a curried form whose outer call returns the inner polymorphic CPS function. The continuation's universal binders scope the existential witnesses, so they cannot escape into the result `R` by ordinary System F scope rules. Non-existential newtypes keep the direct-return projector `Name.p[A...] Name(A...) -> payload`. Standalone `(<U>, body)` type expressions are not admissible anywhere in Kio'.

### Higher-kinded types

Kio' expresses higher-kinded types directly, with no application primitive. A binder annotated with a kind-`*→*`-or-higher signature (`[*F]`, `[**G]`; see [`grammar.md` § Kind grammar](grammar.md#kind-grammar)) admits **direct type application**: `F(A)` is the type obtained by applying the kind-`*→*` binder `F` to the kind-`*` argument `A`, and `Either(String)` is the kind-`*→*` type obtained by partially applying the arity-2 newtype `Either` to one argument. A transparent alias reference must instead supply every parameter declared by that alias; bare and undersaturated spellings cannot retain the alias declaration's own binders. There is no `__App__`, no `__inj__`, no `__prj__`: lifting a value into and projecting it out of a kind-`*→*` position (see [`language.md`](language.md#higher-kinded-types)) is an ordinary language construct — the `newtype` constructor / projector pair at a named kind-`*→*` newtype (a *concrete type constructor*), and an explicitly-threaded instance value (an `Applicative(F)` / `Comonad(F)` argument and its `pure` / `extract` members) at a kind-`*→*` binder `[*F]` (an *abstract type constructor*).

The kind discipline is what keeps the fragment decidable and strongly normalizing:

- **Kinds are explicit, never inferred.** A newtype's signature is its declared parameter list, each parameter carrying its own kind (an all-kind-`*` newtype `Either[E][A]` accepts two kind-`*` arguments; a dictionary newtype `Monad[*F]` accepts one kind-`*→*` argument); a binder's kind is the star annotation the user writes (`[A]` ⇒ `*`, `[*F]` ⇒ `*→*`, …). Kind validation is a finite structural operation at every declaration and completed-type boundary. While surface application inference is open, an owner-scoped equality goal also carries the explicit kind required by its binder and rejects a solution of another kind. This participates in first-order type-argument solving without inferring kinds or introducing higher-order kind constraints.
- **Runtime value types are saturated.** A function parameter or return, newtype payload, annotation, or product/sum member must have kind `*`. Thus `F(A) & G(A)` is a value type, while bare `F`, `F & G`, and `F | G` are not. A higher-kinded constructor remains admissible as an argument to a matching higher-kinded parameter (`Monad(F)`) or as a partial nominal application (`Either(String)`). The check follows explicit binder kinds and the selected nominal declaration, so extending a module with unrelated declarations cannot change an existing type's meaning.
- **Complete polymorphic function schemes are structural value types.** A chain of one or more `Forall` nodes ending in a `Function` has kind `*`, even when a binder in that chain is higher-kinded. The complete scheme may occur in any value-type position, including products, sums, annotations, type arguments, anonymous function types, and recursive or existential newtype payloads. No declaration name or producer provenance is consulted, and unfolding a transparent alias does not change the classification. A higher-kinded `Forall` whose body does not ultimately reach a function is incomplete and is rejected; bare and undersaturated constructors likewise remain non-value types. Newtype existential binders themselves remain kind `*`. Kio applies this same structural rule before lowering, so Kio' does not gain a Prime-only form.

A Kio' call that writes only the current `Forall` arguments performs only
those type applications. It contributes no Unit value application, even when
substitution and transparent-alias unfolding expose a `. -> R` function.
Consequently `id(.)` is residual with type `. -> .` for
`id : [A] A -> A`, and `nil(A)` is residual with type `. -> List(A)` for
`nil : [A] . -> List(A)`. The saturated forms write the value application as
well: `id(., ())`, `nil(A, ())`, or the explicitly nested `nil(A)()`.
Application structure comes from the written term and its artifact-visible
type; declaration arity, parameter spelling, provenance, and private ABI data
are not authority. Alias unfolding can validate a written application but
cannot invent one or move it across a type-application stage.

Per-backend codegen materializes the kind-`*→*` type constructor where the host language's type system needs it. How a backend carries that kind identity is its own concern — a native-HKT backend (Haskell) maps it onto a host type constructor (`F(A)` → `f a`), while a type-erased backend (JS, Rust, Go, Swift) treats application as transparent, carrying `F(A)` as the body's universal erased value; both strategies are described per family in [`backends/README.md` § Higher-kinded types](backends/README.md#higher-kinded-types). The kind annotations are part of a type's public signature.

### Order-sensitive visibility

Ordinary top-level declarations in a file, including `host type` and `host fn`, are processed top-to-bottom; a declaration sees only names introduced above it plus imports. A host declaration's signature is checked before its name enters scope. Forward references within a file are not allowed. This makes recursive term bindings impossible by construction — a `fn` cannot call itself (or another `fn` declared below it), since the name is simply not in scope inside its body. An ordinary type-alias or unmarked newtype body likewise cannot self-reference or forward-reference.

The two capability-free recursive-data forms introduce closed, written scopes:

- `rec newtype N` puts exactly `N` in scope in its payload; and
- `rec { ... }` puts every member type head in scope throughout that one group.

After either declaration closes, its names enter the ordinary source-ordered module scope. Neither form sees an unrelated later declaration. Adding declarations outside a recursive scope therefore cannot retarget any reference inside it: the candidate heads are fixed by the written singleton or braces, and imports still resolve by exact path.

### Explicit recursive data declarations

A self-recursive nominal uses `rec newtype`. The declaration itself is the **iso-recursive** wrap/unwrap boundary: crossing it in values goes through the declared constructor and projector, never by implicit unfolding.

```kio
rec newtype List[A] : . | (A & List(A)) {
  pub constructor cons;
  pub projector un_list;
};
```

A genuinely mutual component uses one bare group, with visibility on each member rather than on the group:

```kio
rec {
  pub type Tree = Branch;
  pub newtype Branch : . | (Tree & Tree) {
    pub constructor branch;
    pub projector un_branch;
  };
}
```

The group must contain at least two declarations and denote exactly one cyclic strongly connected component: every member must reach and be reached by every other member through written type references. An acyclic group, a group containing an unrelated helper, multiple independent components, or a one-member group is rejected as non-minimal. A transparent alias may participate, but its alias-to-alias dependency subgraph must be acyclic; every cycle must cross a nominal `newtype` boundary. Thus aliases still unfold along a finite path to a nominal knot, and an alias-only cycle remains invalid.

The singleton marker is required exactly when its newtype payload refers back to its own head. Omitting it leaves that reference unresolved; writing it on an acyclic newtype is rejected as redundant. There is no `rec type` singleton form. Acyclic forward dependencies stay outside a group and are written dependency-first.

Visibility also closes over declaration signatures. Every named type reachable
from a `fn`, `type`, or `host fn` signature must be at least as visible as the
declaration; transparent aliases do not hide narrower types. A newtype's
payload is checked against the effective visibility of each constructor and
projector: the intersection of the outer nominal's visibility and the member's
own marker. An exported opaque newtype may therefore keep both members and its
payload private, while a `pub` marker on a member of a private outer type does
not make that member externally reachable. The same checks run when Kio' is
parsed and validated independently, rather than relying on the surface
producer.

**Strict positivity.** Strict positivity is a property of the explicitly written recursive component, not of an unrelated module declaration. For `rec newtype`, its one nominal is the component. For `rec { ... }`, follow references among the exact written member heads; the group-validity rule has already established one strongly connected component. A newtype's recursive use sites are every `Type::Path` in its payload whose head reaches a nominal member of that component, directly or through its transparent aliases. Every such use site must sit in a **strictly-positive position**: the **composed variance** from the root of the payload down to the occurrence must be `+` (covariant) or `0` (the parameter is unused at that position). Variance composes through every type connective: arrow LHS flips, arrow RHS preserves, `&` / `|` preserve, and entering a parametric `newtype` slot composes with the slot's variance — computed by mutual fixpoint over the component so transitive contravariance through chains of newtypes is caught. Host-type parameter slots are conservatively treated as **invariant** (their bodies are opaque to the package), so any recursive use site under a `host type` type-argument fails the check. Non-strictly-positive occurrences (`T -> a`, transitively contravariant chains of newtypes, recursive use sites under a `host type` slot, and mutual / wrapper-indirect cycles such as `N₁ : … (N₂ → …); N₂ : … (N₁ → …)` or `P : … (Q → …); Q : P`) are rejected at declaration time with a type error. A negative occurrence of a nominal outside the written recursive component stays admissible — it names a fixed inductive type, not part of this recursion. This rule is what keeps Kio' strongly normalizing: without it, iso-recursive wrap/unwrap alone is enough to encode `Ω` and other fixed-point combinators, with no `fn` self-reference required. See [`formal/prime.md`](formal/prime.md) § 2.5 for the full account.

**Inductive recursion; no codata.** Kio' commits to `newtype` recursion as an **inductive**, least-fixed-point boundary: every value has finite wrap depth, and strong normalization depends on it. Codata — greatest-fixed-point, infinite values, anamorphism, productivity — is not a language feature. Host-supplied codata works without any language change: a host declares an opaque `host type Stream[A];` plus `host fn unfold`/`head`/`tail` observers, and Kio packages consume them like any other host item. Opacity of host types keeps Kio from introspecting the infinite structure; the absence of `fn` self-reference keeps users from writing non-terminating step functions; productivity and laziness live inside the host.

**No language-provided fold.** Kio' itself provides no fold over recursive `newtype`s — `fn` cannot be self-recursive, so user code that needs to walk a recursive payload depends on a host function (typically a `host fn loop : …`).

### Modules and `import`

Same as [`language.md`](language.md#module-system).

### Host declarations

Same as [`language.md`](language.md#the-host-boundary): module-level `host type` / `host fn` declarations (opaque, signature-only, always public Kio' items) with `role(...)` markers on `host type`. A role-bearing host type is nullary; only a roleless host type may declare type parameters, those parameters must have kind `*`, and combining any parameters with a role is a type error. Higher-kinded binders remain exclusive to top-level `newtype` and `fn` parameter slots. Which modules' host items reach the host is selected by the package file's `bridge { … }` glob block. Kio' literals **always carry their `(Type)` host-type annotation** — the unannotated-literal form admitted by the full surface (see [`language.md` § Literals](language.md#literals) tier 2 and tier 3) is not part of Kio'. See [`grammar.md` § Kio' grammar](grammar.md#kio-grammar) — `LiteralCall`'s annotation slot is mandatory. Full-Kio checking records a tier-2 or tier-3 resolution on the Lowered tree, and the Lowered → Prime substitute pass materializes that annotation; every resulting Kio' literal is fully annotated.

### Branching intrinsic

A seventh intrinsic, `__if_then_else__ : [A] (Bool & (. -> A) & (. -> A)) -> A`, is also gated behind `import __intrinsics__;`. Here `Bool` is selected from the nonempty set of `role(bool)` host identities in unqualified lexical scope, or otherwise from the fallback set of terminal identities reached through transparent aliases in that scope; that selected set must be a singleton. A qualified module import alone contributes none of its members, though an in-scope alias may reach its terminal host identity through one. Identities are `(declaring module, host-type name)`, so repeated paths to one declaration count once. A file with zero or multiple selected Boolean-role identities cannot reference `__if_then_else__`. Given a condition `c` and arm-thunk expressions `t` and `e`, it evaluates `c`, `t`, and `e` exactly once from left to right, then invokes either the resulting `t` value or the resulting `e` value with `()` and returns that call's result. Constructing both thunk values is eager; invoking the selected thunk is branch-dependent. The reference library's `if!` elaborator constructs this ordinary Kio' branching operation. Multiple same-role declarations remain valid when no construct asks for this singleton scheme, and explicitly annotated `.t(B)` / `.f(B)` literals remain valid at any in-scope `role(bool)` type `B`.

### Bottom-elimination intrinsic

An eighth intrinsic, `__absurd__ : [A] ! -> A`, is gated behind the same `import __intrinsics__;`. Given a value of the bottom type `!` — which can only come from a host function declared `-> !` — it produces a value of any type. There is no symmetric introduction: `!` has no values, so there is nothing to construct.

### Existential introduction and elimination

Existentials are a `newtype`-only feature in Kio'. There are no standalone introduction or elimination intrinsics: a value of an existential-bearing newtype is constructed by its declared constructor (with the existential witnesses inferred from the value-arg's type, see [Existential binders](#the-newtype-primitive)) and consumed by its CPS projector, whose curried scheme

```text
N.<projector> : [A...] N(A...) -> [R] ([U...] payload -> R) -> R
```

bakes the escape-checking continuation into the projector itself. There is no separate `__pack__` / `__unpack__` pair — the surface `let .(<U> x) = e;` sugar (see [`language.md` § Existential type binders](language.md#existential-type-binders)) desugars at parse time to the projector's two-step CPS call.

## What's not in Kio'

These features live only in `language.md`; each one either desugars to Kio' or runs an elaborator pass on top:

- **Elaborator declarations and compile-time implementation modes** — both
  `impl <ValuePath>` and `impl(fills) <ValuePath>`, plus `trailing product`,
  `trailing thunk`, and `trailing sequence`, are full-language declaration
  forms. Their lexical value paths, block labels and descriptors, implementation ABI,
  `import __comptime__;` block, `__Comptime__` proof, `__Fill_ctx__`,
  fill/specialization helpers, and provisional staging state are consumed
  before the Lowered → Prime boundary. Kio' has no corresponding declaration,
  type, term, import, or privilege.
- **Source-to-target elaborator coercion palettes** — imported user-defined elaborator bang-calls (`name!(e, T)`) that synthesize Kio' glue between a source value and a target type. The reference libraries ship an algebraic palette and a spine palette; both are surface-only, removed at the Lowered → Prime boundary, and their per-form rule sets are library content (see [`language.md` § Elaborators are imported, not ambient](language.md#elaborators-are-imported-not-ambient) for the call mechanism, and [`docs/poc/optics.md`](../docs/poc/optics.md) / [`docs/poc/elab.md`](../docs/poc/elab.md) for per-form semantics). What reaches Kio' is ordinary `__pair__` / `__left__` / `__right__` / `__either__` / `__fst__` / `__snd__` / `__absurd__` glue.
- **Block elaborator calls** — `name! … { … }` with optional declared labelled blocks. Neutral syntax is projected as independent product entries, an ordinary thunk, or an ordinary bind-parameterized sequence, according to the resolved declaration's final public value slots. The reference library's `if! cond { … } else { … }` constructs ordinary `__if_then_else__` glue from its checked Boolean condition and projected thunks; `scope! { … }` invokes its projected thunk once. No block descriptor, label, neutral syntax, or projection metadata reaches Kio'. The names `if`, `else`, `scope`, `do`, and `match` remain ordinary identifiers in both Kio and Kio'.
- **`rec(loop)` recursive-function groups and annotated `rec name(...)` calls** — surface-only recursion sugar. The desugar pass lowers each group to private state-packet `newtype`s, one ordinary wrapper function per member with that member's visibility, and an ordinary call to the resolved loop function. `rec(cont)` calls carry explicit continuations in the loop state; `rec(poly)` calls select a different instantiation for the next state. When continuation-bearing state packets refer back to themselves or one another, their generated declarations use the same Kio' `rec newtype` or minimal `rec { ... }` type scope as any other recursive nominal component. Kio' has no term-level fixpoint, recursive-function group, or recursive-call marker.
- **`match!`** — the imported dispatch elaborator, `match! value { .(<param-list>) { <body> }; … }`. Its `product` block supplies clause values whose function parameter types are the dispatch patterns. The library selects a clause per DNF branch of the scrutinee and reconstructs its argument. It elaborates to a `let`-bound value per clause plus nested `__either__` calls and `__fst__` / `__snd__` / projector-member calls. A single product-valued entry may supply preassembled clauses. Like every block call, `match!` has no blockless or UFCS alternate. See [`language.md`](language.md#pattern-matching).
- **`derive!`** — the instance-deriving imported elaborator `derive!(<candidates>, <TargetType>)`. The candidate argument encodes no functions as `()`, one function directly, or multiple functions as a product tuple. The typer resolves the unique composition of those candidate functions that produces the target type, then synthesizes the corresponding nested call tree. The substitute pass swaps the `derive!` call for that synthesized tree at the Lowered → Prime boundary, so what reaches Kio' is an ordinary call tree of `fn` applications — no `derive!` form survives, and no new runtime machinery is added. See [`language.md`](language.md#the-derive-elaborator).
- **Sequence blocks** — the reference `do! bind { … }` applies a projected `sequence` function to the supplied bind value. The projection uses the structural type `[A][B] (F(A) & (A -> F(B))) -> F(B)` for its bind parameter. Bind statements and sequenced expressions become ordinary calls with nested continuations, ordinary lets remain lexical, and the final expression explicitly produces `F(R)`. A sequenced expression requires `F(.)`; `let _ <- e` remains a bind that may discard a non-unit payload. Only ordinary functions, applications, and lets reach Kio'. The Kio' grammar admits neither block calls nor `<-` binding statements. See [`language.md`](language.md#monadic-do-blocks).
- **Placeholder lambdas** — surface lambda form `.stem. { expr }` with source-local numbered references such as `x1` / `x2`. Desugaring produces an explicit `.(...) { … }` with hygienic parameters. Kio' admits those identifier spellings as ordinary names, but not the `.stem.` introduction or its ownership rule. See [`language.md`](language.md#placeholder-lambdas).
- **UFCS calls `r.>f(…)`, `r.>>f(…)`, `f(…).<r`, `f(…).<<r`** — surface shorthand for inserting one value into a callable's value-argument stream. A present trailing argument list is nonempty: receiver-only calls use `r.>f` / `f.<r`, while an additional Unit is explicit as `r.>f(())` / `f(()).<r`; written empty tails are rejected before lowering. The typer resolves the explicitly named callee, uses the ordinary call-slot planner to place the receiver in the first or last value slot, and records the equivalent prefix call (regular call or bang-call elaboration). The substitute pass swaps that recorded replacement in at the Lowered → Prime boundary. Kio' has no UFCS syntax at all — the only arrow token in Kio' is `->`, the type-level function arrow. See [`language.md`](language.md#ufcs).
- **Existential-opening `let .(<U> x) = e;`** — surface form that binds a fresh universe of existential witnesses `U` and a value `x` from `e`'s existential-bearing newtype. Desugars at parse time to the two-step CPS call against the newtype's CPS projector (see [Existential introduction and elimination](#existential-introduction-and-elimination) above).
- **`labels`** — surface sugar for generated `newtype`s and optional aliases. A `labels { f : X, g : Y, … }` block desugars entry-by-entry into one `newtype` per explicit payload entry, each with a compiler-minted type name (the label spelling with the first letter capitalized — `foo` -> `Foo`) and two members: a constructor `mk` and a projector `get`. Each entry `f : X` produces `newtype F : X { pub constructor mk; pub projector get; }` when the `labels` is `pub`, and the private form otherwise. In a later named form, an exact `f: _` marker contributes an ordinary reference to that earlier generated `F` and emits no declaration; the marker and its source-order resolution are absent from Kio'. The named form `labels T = { f : X } | { g : Y, h : Z };` also emits `type T = F | (G & H)` with the same outer visibility. An unmarked declaration lowers to source-ordered standalone declarations. `rec labels` instead gives the one surface declaration an atomic recursive scope over its generated heads and, for a named form, its alias head; lowering emits each genuine cyclic component as the minimal ordinary Kio' `rec newtype` or `rec { ... }` scope and leaves acyclic generated declarations outside it. Every cycle remains grounded at a generated nominal. A generated newtype whose name collides with an existing top-level type in the same module is a duplicate-name error by the ordinary rule.
- **Braced selective label imports** — `import m({f});` selects only label syntax. Label lowering resolves that identity, rewrites its generated-newtype references through a collision-free qualified module binding, and removes the braced item. Kio' selective imports contain ordinary identifiers only; a Kio'-only parser rejects a braced import item.
- **Nonminting label forwarding** — `type {local} = {source.label};` introduces only a Surface label spelling. Label elaboration follows its explicit label/import edges to the original nominal family, uses ordinary imports and member calls for actual uses, and removes the declaration. Kio' contains neither the forwarding item nor provenance describing its route. A Kio'-only parser rejects the braced declaration; ordinary uppercase positional aliases remain shared Kio/Kio' syntax.
- **Label value construction** — the surface tuple literal `(a, b)` (and n-ary `(a, b, c)`) is sugar for `__pair__(a, b)` (resp. nested `__pair__(a, __pair__(b, c))`). Label construction `{f = e}` constructs `F.mk(e)` through the same type-directed row-update elaboration path as field update, with `()` as the receiver. `{f}` is sugar for `{f = f}`; `{m.f}` uses the last path segment and means `{m.f = f}`; `{f=}` is sugar for `{f = ()}`; `{}` is the unit value. Multi-label `{f = e, g = e'}` further desugars to `__pair__(F.mk(e), G.mk(e'))`.
- **Field access/update** — `x.?{f}` / `x.?{f, g}` / `x.?{}` and `x.!{f = y}` / `x.!{f}` / `x.!{f=}` / `x.!{}` are type-directed surface forms. The typer records their elaboration and the substitute pass lowers them to ordinary Kio' terms using `__fst__`, `__snd__`, `__pair__`, `F.get`, and `F.mk`; conceptual `__access__` / `__filtered__` helpers are not Kio' and must not survive this boundary. In checking mode construction and update may emit a product-spine permutation when the expected type is exactly a reorder of the form's default result. Access always emits unwrapped payloads in written order. No row-form AST node survives into Kio'.
- **Type-level sugar** — `A & B & C` / `A | B | C` for nested binary type operators. Label types use generated nominal names such as `F` and `F(T)`; lowercase labels and braced label forms are not type syntax.
- **`equiv`** — declarative test claim discharged by the `kio test` runner. `kio-prime` rejects `equiv` items at parse time. See [`language.md`](language.md#equivalence-claims-equiv) and [`cli.md`](cli.md#kio-test).
- **literal aliases** — `literal name = <literal>;` is surface-only. The desugar pass expands a bare reference to the stored literal token, expands `name(Type)` to the stored literal with that annotation, and drops the declaration at the Surface → Desugared boundary. Kio' keeps only the already-expanded literal expression; a Kio'-only parser rejects `literal` items.
- **Fixed and variadic operators** — `op <pattern> { impl <ValuePath>; };`
  binds a fixed pattern, while `varop OPEN CLOSE` binds mirrored delimiters
  around comma-separated ordinary expressions.
  A variadic body selects `foldl`, `foldr`, `foldl1`, or `foldr1` with a
  step and base/seed path, then optionally a unary `finalize` path. These are
  ordinary lexical value paths, not expressions or slash-qualified FQNs.
  Declarations are local unless exported with `pub`. Before Kio', operator
  lowering replaces every use with ordinary calls, consumes the declarations,
  and removes tagged operator selections from imports. Kio' shares the maximal
  operator-run lexer, but has no fixed/variadic operator declaration, tagged
  operator selection, or operator expression. See [language.md](language.md#operators).

## Grammar

**Imports.** Kio' uses the same module-first imports as Kio:
`import module/path(Name, value);`, `import module/path as alias;`, and the
builtin block `import __intrinsics__;`. Selective lists are parenthesized and
nonempty, with ordinary identifiers only. In canonical formatting, the opener
follows the module path on the introducing line. Braced label selectors, tagged
operator grammars, and
`import __comptime__;` are surface-only and are rejected by the Kio' grammar.
Keyword-shaped identifiers keep their ordinary contextual behavior: importing
an ordinary value named `op` does not introduce an operator.

Kio''s grammar is the **regular-module** file shape — every `*.kio` file with a `module` declaration. The productions live in [`grammar.md`](grammar.md) § Kio' grammar; this section narrates the Kio'-specific properties that the productions encode. Package boundary files have their own grammars in [`grammar.md`](grammar.md) § Package files (see also [`package.md`](package.md)).

**Lexical structure.** Kio' uses the same maximal `SymbolRun` lexer as Kio over the ASCII operator-character allow-list, including `[` and `]`. Its grammar then admits only runs assigned a fixed structural role. In particular, parser-contextual peeling of forall delimiters keeps `[A]`, `[*F]`, `[A][B]`, and `.[*F]` compact even though their raw maximal runs can include `[*`, `][`, or `.[*`. No Kio' production admits an `op` or `varop` declaration, operator import, or operator expression, and structural peeling never creates one. Two other shared lexer rules carry over from [`grammar.md` § Lexical structure (Kio')](grammar.md#lexical-structure-kio) unchanged: the **negative-literal carve-out** (a leading `-` flush against a digit fuses into the literal at expression-starting positions) and the **comment-marker rule** (`//` / `///` must be followed by whitespace, end-of-line, or end-of-file; `////` and a marker flush against a non-whitespace character are lex errors).

**Contextual keywords.** Every Kio' keyword (`module`, `import`, `as`, `pub`, `pure`, `type`, `host`, `fn`, `rec`, `newtype`, `let`, `constructor`, `projector`, plus the magic phrase `__intrinsics__`) lexes as an ordinary `IDENT`; the parser recognizes the keyword role only at the positions where the productions show it as a literal terminal. `pure` modifies only an ordinary `fn`, `rec` marks an explicitly recursive data declaration or mutual type group, and `host` is a host-declaration modifier only when immediately followed by `type` or `fn`. Surface-only forms have no productions in the Kio' grammar — a Kio'-only parser rejects them as syntax errors. The rejected surface set covers:

- Label declaration syntax: `labels`, including `rec labels`.
- Braced label-forwarding declarations: `type {local} = {target};`.
- Recursive-function syntax: top-level `rec(loop)` groups and `rec name(...)` / `rec(poly, cont) name(...)` calls.
- Placeholder-lambda introduction `.stem.` and its source-local ownership rule; numbered reference spellings remain ordinary identifiers.
- Every elaborator bang-call form — blockless and trailing-block calls through `IDENT '!'`, including the reference coercion palettes, `if!`, `scope!`, `do!`, `match!`, and `derive!`. The unmarked names themselves remain ordinary identifiers.

Direct higher-kinded type application — `F(A)` for a kind-`*→*` binder and partial application of a multi-arity newtype like `Either(String)` — is, in contrast, part of Kio' (see § Higher-kinded types). A parametric transparent alias remains an exact-arity abbreviation: every parameter declared by the alias must be supplied.

A program that parses against the Kio' productions is *syntactically* Kio'. It is not necessarily well-typed or well-resolved — those are separate properties checked by later compiler phases. The corpus check `ci/checks/orchestrators/golden-tests.sh` verifies that every `IS_KIO_PRIME` case parses against the Kio' productions, using the `kio-prime-check` tool under `ci/infra/kio-prime-check-rs/` as the oracle.

## Kio' semantics

Type flow in Kio' is syntax-directed and bidirectional. Function calls supply
all universal type arguments explicitly. Ordinary expressions synthesize,
while a known function position may check a lambda against an expected
function type; the bounded direct-lambda-callee rule described above is the
only same-application completion rule. Kio' has no coercion or dispatch
elaborator, no omitted universal call argument, and no retained surface
inference state.

Surface call shorthand is fully materialized at this boundary. A syntactically
empty direct/prefix surface call omits leading universal arguments and
contributes an explicit Unit value; those omitted types must first be solved by
the call's local constraints. Thus surface `nil()` in a position expecting
`List(Int)` becomes `nil(Int, ())`. A receiver-only UFCS spelling omits its
argument list; a written empty UFCS list is rejected, and `r.>f(())` carries an
additional explicit Unit after receiver insertion. A nonempty surface
type-only packet remains type-only: `id(_)` checked against
`. -> .` becomes residual `id(.)`, not `id(., ())`. Fresh Kio' has no omitted
universal call arguments, so it writes `nil(Int, ())` (or `nil(Int)()`) to
saturate that example.

A flat surface call that consumed successive function layers becomes nested
Kio' applications, and a returned `Forall` instantiated from an expected
result becomes an explicit type application at the corresponding nested
boundary. Each type application consumes exactly one unary `Forall`;
consecutive binders therefore remain consecutive applications in the Kio'
tree. A partially applied elaborator becomes an ordinary typed function value;
no bang form or callable-kind provenance survives. Consequently a freshly
parsed Kio' artifact contains all information needed to validate the same call
tree without reconstructing surface argument grouping or expected-type
history.

Structural typing (see [`language.md`](language.md#structural-typing)) governs products, sums, and type aliases; each `newtype` mints a nominally-distinct type at its declaration site; `host type` declarations remain opaque to the importing package. These rules are identical in Kio' and the full language — none of them depend on the elaborator.

The **open-world property** — that adding definitions to a module body never breaks a package — holds trivially in Kio': with no `into!` / `onto!` coercion target, there is no implicit choice to disturb. As in the full language, this property is scoped to module bodies; a package file's contract surface is governed by versioning, not open-world.

**Evaluation.** Kio' evaluates **call-by-value**. A term abstraction — value
lambda or type lambda — is a value, so its body is not entered until the
matching application. A type application evaluates its callee exactly once
and consumes exactly one `Forall` before evaluation continues with the exposed
term. A value application then evaluates its callee exactly once, evaluates
its value arguments exactly once from left to right, and only then invokes the
call. Thus, when a type application precedes a value application, computation
exposed by the type application runs before those later value arguments are
evaluated. A type argument itself carries no runtime value; erasing that
representation does not erase or move the application stage. The pure
reduction is confluent and strongly normalizing, so the choice of normalization
strategy never changes the *result* ([`formal/prime.md`](formal/prime.md)
§ 3.1); but a `host fn` call is observable, and the call-by-value order fixes
the order of host effects.

**Formal semantics.** [`formal/prime.md`](formal/prime.md) pins the typing rules, reduction relation, and meta-theorems (type safety, strong normalization, decidability, open-world) into inference-rule notation with proof sketches. This page is the prose contract; the formal page is the meta-theory companion — they are kept in step.
