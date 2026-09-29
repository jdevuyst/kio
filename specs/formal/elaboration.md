# Kio elaboration formal semantics

Companion to the prose page [`../language.md`](../language.md) § Type system. This page pins the elaborator's typing rules — the rules by which a Kio surface program is checked, inferred, and translated into [Kio'](../prime.md). The level of detail is "a careful PLT reader can reconstruct every step." Proof sketches are inlined; full machine-checked development is out of scope.

The elaborator is the **surface-to-core boundary**. Its contract:

- **Input** — a resolved `Lowered` Kio module. Earlier passes have folded operators, removed tuple, placeholder-lambda, and recursive-call sugar, and elaborated labels. Generic block calls project neutral blocks according to the resolved elaborator descriptors and public type. Forms that need typing information remain; UFCS receiver insertion likewise depends on the resolved callee's type slots.
- **Output** — a checked `Lowered` tree plus boundary-local completion records. The Lowered → Prime substitute pass consumes those records to materialize inferred annotations and arguments, normalize UFCS calls, and replace every type-directed surface form. The assembled Kio'-shaped package is then checked by the standalone Prime checker; no side channel crosses that boundary.
- **Discipline** — bidirectional type inference for arbitrary-rank types, following Peyton Jones, Vytiniotis, Weirich, and Shields (2007), *Practical type inference for arbitrary-rank types* (henceforth **PJ-V-W-S**). Source-bounded exact type-argument inference; shallow skolemization; no let-generalization; no backward type flow across a `let` or expression-statement prefix; no expression-level `e : T` annotations.
- **Decidability** — every quantifier the program needs is written somewhere in the source (signature, type-argument slot, declaration header), so rank-N inference is decidable. See § 10.
- **One-shot local discipline** — elaboration is syntax-directed. Each
  connected application-local inference problem has a finite domain bounded by
  its written call tree and resolved callee types. A nested call whose unfinished
  result participates in its ancestor's inference joins that domain while
  retaining its own lexical owner; an independently completed nested call
  contributes only its closed result. Each source premise is entered fresh
  once and each retained source action is consumed at most once. Each
  application owner may retain at most one pending expected-result equation;
  a connected tree may contain one for each owner. After an initial blocked
  attempt, the equation records its two zonked sides. Another attempt is
  admitted only when a retained child closes or one of those sides changes
  structurally; another deferral replaces the snapshot. Merely entering the
  next schedule mode never retries it. There is no global constraint solving,
  derivation replay, backtracking, fixed-point iteration, or worklist, and no
  inference goal escapes its finite domain or lexical owner (see § 10). This
  pins the *shape* of the algorithm, not an asymptotic bound: substitution and
  rank-N polytype comparison can be super-linear.

This page formalizes the elaboration; the residual Kio' tree's behavior is the subject of [`prime.md`](prime.md).

## 1. Modes and judgments

Two modes:

- **Synthesize (⇑)** — given Γ and e, compute T such that Γ ⊢ e ⇑ T.
- **Check (⇓)** — given Γ, e, and an expected type T, decide whether Γ ⊢ e ⇓ T.

Both determine, alongside the type, the **eventual materialized term**. We write it as `e ↪ e′`, with `e` the Lowered input and `e′` the Kio'-shaped term obtained when the boundary-local records from the derivation are substituted. The implementation's immediate typing result is the checked Lowered node plus those records; the standalone Prime checker validates `e′` after substitution. When the materialized form is uninteresting (a trivial copy modulo annotation insertion), we suppress it.

Formal judgments:

```text
Γ ⊢ e ⇑ T ↪ e′           e synthesizes type T and eventually materializes as e′
Γ ⊢ e ⇓ T ↪ e′           e checks against T and eventually materializes as e′
Γ ⊢ T type               T is a well-formed type under Γ (≡ prime.md § 2.1)
```

The mode of a position is determined by the surrounding rule: a sub-expression is either in a checking position (its expected type is fixed by the parent rule) or a synthesizing position (no expected type; the parent reads off the synthesized one). The two modes do not commute — a position is **never both at once**. Bidirectional flow happens at one boundary at a time, namely the rules below where the antecedent's mode differs from the conclusion's.

Contexts `Γ` carry the same shape as in [`prime.md`](prime.md) § 2.1:

```text
Γ ::= · | Γ, α | Γ, x : T | Γ, N(α₁, ..., αₙ) := …
```

The α form admits both ordinary universal binders (introduced by user-written `[α]`) and **skolems** (introduced by checking against a polytype, see § 4). Skolems are distinct from ordinary type variables only when an escape check fires at a binder boundary; the elaborator treats them uniformly elsewhere.

## 2. Polytypes and instantiation

The elaborator distinguishes **polytypes** (which may contain top-level ∀ quantifiers) from **monotypes** (no top-level ∀). PJ-V-W-S's syntactic split:

```text
σ (polytype)   ::= ∀ᾱ. ρ
ρ (rho)        ::= τ | σ → σ          (an arrow may carry polytype slots —
                                          this is what makes the system rank-N)
τ (monotype)   ::= α | . | !  | τ → τ  | τ & τ  | τ | τ  | N(τ̄)
                | ∃ᾱ. τ                (existential, surface `<U>` — present
                                          only inside newtype payload positions)
```

A polytype's leading binders sit at the outermost position; binders nested inside an arrow's parameter or return slot belong to that slot's nested σ. **Shallow skolemization** is the rule that drives the system: when checking against ∀ᾱ. ρ, only the leading binders ᾱ skolemize; binders deeper in ρ are left intact and surface as expected polytypes for the sub-expressions they cover. The two types

```text
σ₁ = ∀α. (α → α) → Int
σ₂ = (∀α. α → α) → Int
```

are **not** interchangeable: σ₂'s parameter is a polytype, σ₁'s is a monotype. A `fn` value-arg that takes a polymorphic identity must be the σ₂ shape; σ₁ types take only monomorphic identities. The elaborator preserves this distinction strictly.

**Source-bounded exact polytype inference.** An inference goal created for one
whole `_`-marked or elided type-argument slot may be solved by a monotype or by
a complete polytype σ. The polytype case applies only when a premise in the
finite connected application domain already exposes the entire well-kinded σ,
closed with respect to inference goals. The elaborator assigns that whole tree
to the goal and uses alpha-equivalent structural comparison for any later
premise. It does not synthesize a `∀`, generalize a free variable, guess binder
placement, solve underneath an incomplete `∀`, search an ambient declaration
set, or backtrack.

Thus `consume[T](value: T)` may infer `T = [A] A -> A` from a polymorphic
`identity` value whose declared scheme is exactly that tree. In contrast, a
polymorphic value cannot fill a proper subterm of a declared monomorphic slot
such as `T -> T`; that would require rearranging or instantiating binders rather
than assigning one whole goal. A user-written explicit polytype argument keeps
the same structural checking behavior. This restricted whole-tree assignment
is not general impredicative inference; § 10 gives the decidability argument.

**No `e : T` form.** Kio's surface grammar has no expression-level type-annotation form. The only annotations are at signatures, local binding patterns, type-argument positions in calls, and declaration headers (`fn`, `host fn`, `newtype`, `type`, parametric `labels`). The PJ-V-W-S paper's `e :: σ` annotation form has no Kio analogue; whenever the paper uses it, Kio relies on an alternative source of the expected type (the signature of the enclosing binder, a fully concrete local binding pattern, the receiving parameter slot, or a return-type annotation on the enclosing `fn`). § 4 makes this concrete for `fn`-against-polytype checking.

## 3. Variables, paths, and the named-callable registry

```text
(x : T) ∈ Γ
─────────────────────────  E-Var
Γ ⊢ x ⇑ T ↪ x
```

Variables in the local context synthesize their bound type. Three categories of identifier share this rule shape but read their type from a per-category source.

**Local binders.** `x` bound by a `fn` value-parameter, a `let`, or a `match!` clause's parameter list. E-Var as above, with the type read from Γ.

**Module-scoped identifiers** — top-level `fn`s and intrinsics (`__left__`, `__right__`, `__either__`, `__pair__`, `__fst__`, `__snd__`, `__if_then_else__`, `__absurd__`) — share a single **scope-aware scheme registry** keyed by name:

```text
(name ↦ scheme(name, Γ_roles)) ∈ Registry
─────────────────────────────────────────────  E-Named
Γ ⊢ name ⇑ scheme(name, Γ_roles) ↪ name
```

For each role, let `D_role` be the set of role-bearing host identities in unqualified lexical scope and `A_role` the terminal identities reached by transparent aliases in that scope. A qualified module import does not add its members to either set; an in-scope alias may nevertheless reach its terminal identity through one. A terminal identity is `(declaring module, host-type name)`; repeated imports and aliases reaching one identity are one member, while equal leaf names declared by different modules remain distinct. `Γ_roles(role) = D_role` when `D_role` is nonempty, and `Γ_roles(role) = A_role` otherwise. The scope parameter threads in because `__if_then_else__`'s scheme is `∀α. (B, . → α, . → α) → α` only when `Γ_roles(bool) = {B}`. A missing or non-singleton set is a type error. Most registry entries ignore `Γ_roles`; the `__if_then_else__` entry uses it to construct its scheme.

Each registry entry pairs a polytype scheme with an optional **side condition** that fires after the standard call-typing rule succeeds. For intrinsics, side conditions are trivial or absent.

The registry's contract is

```text
scheme : Name × RoleEnv → σ
side_condition : Name × Γ × bound_args → Elaboration | Error
```

with `bound_args` the type and value arguments after the standard call rule has substituted them in. User-defined elaborator bang calls have their own rule in § 7.4: they resolve through ordinary scope to an elaborator declaration, then use that declaration's call type and compile-time implementation ABI.

> **Implementation note.** The kio-rs implementation routes intrinsics through `intrinsic_scheme(name, span)` and standard application typing. User-defined elaborators route through `Expr::UserElaborator`; their implementation returns reflected checked terms through the `__comptime__` ABI, and the substitute pass swaps those terms in at the Lowered → Prime boundary.

### Kind-checking discipline

Higher-kinded type application (`F(A)`, `Either(String)`) is a **type form**,
not a callable: it has no scheme entry in the registry and does not participate
in E-App. Binder kinds are always explicit. Kind validation is a
phase-polymorphic structural operation applied whenever a declared or completed
type crosses its boundary: named `fn`, `host`, `newtype`, alias, and `elab`
signatures are checked as module declarations are processed, and completed
expression types are checked before they enter Kio'. While an
application-local type goal is open, first-order unification also records its
required explicit kind and rejects an incompatible solution. This is kind
checking, not kind inference or higher-order kind solving. It verifies:

1. **Application is kind-consistent.** Every type application consumes one arrow per supplied argument. A head of kind `*→…→*` with `n` arrows applied to `k ≤ n` arguments has kind `*→…→*` with `n − k` arrows; a kind-`*` head applied to any argument, or an over-application (`k > n`), is a kind error. A newtype's kind is read from its declared parameter arity; a binder's kind is its star annotation (`[A]` ⇒ `*`, `[*F]` ⇒ `*→*`, …). A transparent alias reference instead supplies exactly the number of parameters declared by that alias: bare and undersaturated parametric aliases are ill-formed and cannot retain residual alias binders. This exact-arity restriction does not by itself determine the reference's resulting kind. See [`grammar.md` § Kind grammar](../grammar.md#kind-grammar).
2. **A complete polymorphic function scheme has kind `*`.** Structurally, one or more `Forall` layers ending in a `Function` form a complete scheme, even when a binder is higher-kinded. The complete scheme may occur in any value-type position, and unfolding a transparent alias does not change its classification. A higher-kinded `Forall` that does not ultimately reach a function is incomplete and is rejected; newtype existential binders remain kind `*`. The rule is identical for Kio and Kio' and depends only on explicit binder kinds and the written type tree, never on a declaration name or producer provenance.

Because the kind language is closed (no kind variables or higher-order kinds),
each structural validation and each goal-kind comparison is total and
decidable. Neither route invents a binder kind or reruns a source typing
derivation. See [`prime.md` § Higher-kinded types](../prime.md#higher-kinded-types)
for the language-level statement.

## 4. Functions and lambdas

A lambda literal's typing rule depends on whether it appears in checking or synthesizing position.

### 4.1. Checking a lambda against a polytype (E-FnCheck)

```text
σ = ∀ᾱ. (T₁, …, Tₙ) → R
Γ, ᾱ_skol ⊢ body ⇓ R ↪ body′         (with each Tᵢ substituted into the binder
                                          context, and each xᵢ : Tᵢ)
xᵢ's surface annotation Aᵢ is admitted by the post-materialization predicate
  and is absent or structurally matches Tᵢ
  (with each admitted "_" as a wildcard at its type position)
R-surface annotation A_R is admitted by the same predicate and is absent or
  structurally matches R (with each admitted "_" as a wildcard)
─────────────────────────────────────────────────────────────────  E-FnCheck
Γ ⊢ .[ᾱ_surf](x₁: A₁, …, xₙ: Aₙ) -> A_R { body } ⇓ σ ↪
    Λᾱ. λ(x₁: T₁, …, xₙ: Tₙ). body′
```

The rule **skolemizes** ᾱ — replaces each universal binder with a fresh constant `α_skol` introduced into Γ. The body is checked at the substituted return type R under the skolemized context. The binders are then re-bound at the elaborated term's outer Λ. Skolemization is **shallow** — only the leading ∀ binders of the immediate σ are skolemized; binders inside Tᵢ or R are not skolemized at this rule.

**Surface-arity match.** The user's `[ᾱ_surf]` binder list must match σ's leading binders in length (the syntactic forall positions correspond). The skolemization runs only when the lengths agree; a mismatch is a type error at this rule, not elsewhere.

**Annotation verification.** Each Aᵢ the user wrote is first materialized
through its already-resolved transparent aliases, retaining the exact route of
every written `_`. A source occurrence is inadmissible when any materialized
copy lies strictly beneath a structural `forall` introduced inside Aᵢ. The
first offending source occurrence in the ordinary source/structural order is
reported; within a duplicated occurrence, the first enclosing binder in
deterministic materialized order supplies the secondary location. Identity and
reordering preserve an occurrence, duplication rejects once if any copy is
inadmissible, and a dropped occurrence has no output and remains admitted.
Header binders ᾱ_surf are outside the separate Aᵢ and A_R trees, so they do
not trigger this predicate. Alias resolution, visibility, arity and kind
errors retain precedence, and the predicate runs before any annotation goal
or publication is created.

Each admitted Aᵢ is then matched **structurally** against Tᵢ under Γ. A whole
or nested `_` passes at its position, with the corresponding piece of Tᵢ
supplying the resolution; every concrete sub-position must be syntactically
equivalent to the corresponding piece of Tᵢ under Γ. A contradiction at a
concrete leaf is a type error. The same rules apply to A_R against R. Absent
annotations behave as the whole-slot `_`.

**Escape check.** After checking, any skolem α_skol appearing in body′'s synthesized type that is not also in σ's binder list is a type error — the skolem leaks. This is the rule that gives the existential-projector elimination its safety: a continuation `.[β₁]…[βₙ](p) { … }` checked against `∀β₁ … βₙ. (Payload) → ρ` rejects any body whose synthesized type mentions a `βᵢ` outside the skolemized scope.

### 4.2. Synthesizing a lambda (E-FnSynth)

```text
Every xᵢ : Aᵢ has a concrete annotation Aᵢ (not absent, not "_")
A_R is absent, exactly "_", or concrete
Γ, ᾱ ⊢ Aᵢ type   (each i)
A_R absent or exactly "_" and
  Γ, ᾱ, x₁ : A₁, …, xₙ : Aₙ ⊢ body ⇑ R′ ↪ body′
  or A_R concrete, Γ, ᾱ ⊢ A_R type, and
     Γ, ᾱ, x₁ : A₁, …, xₙ : Aₙ ⊢ body ⇓ A_R ↪ body′ with R′ = A_R
─────────────────────────────────────────────────────────────────  E-FnSynth
Γ ⊢ .[ᾱ](x₁: A₁, …, xₙ: Aₙ) -> A_R { body } ⇑
    ∀ᾱ. (A₁, …, Aₙ) → R′ ↪
    Λᾱ. λ(x₁: A₁, …, xₙ: Aₙ). body′
```

E-FnSynth is the rule for a lambda literal that sits in a **synthesizing
position with no expected polytype** — for example, the right-hand side of an
untyped `let`. The rule does not inspect the body to determine a
value-parameter type.

The synthesizing rule **rejects** any lambda whose value-parameter slot is
`_`-marked or dropped: without a parent checking position to fill it in, that
slot is unconstrained. The rule fires only when every value-parameter slot has
a fully concrete annotation (no `_` anywhere — outermost or nested).
Otherwise the rule does not apply, and the typer reports the standard
"lambda with unconstrained metavariables" error at the offending slot.
E-FnCheck may resolve `_` structurally from an expected function type. The
direct-literal E-App refinement below may instead resolve an absent or
whole-slot `_` before invoking E-FnSynth; it never admits a nested `_`.

**A `_`-marked or dropped return slot** is admissible under E-FnSynth: R′ is
read from the body's synthesized type. A concrete return annotation A_R
instead supplies the body's expected type, and the body is checked against it.
An otherwise-structural return annotation containing a nested `_` is not
body-synthesized: it requires a checking position and is first subject to the
post-materialization `forall` predicate above. Thus E-FnSynth gains no new
partial-annotation domain.
The default-to-`.` rule applies only at top-level `fn` declarations (see
[`prime.md`](prime.md) § 1.3), not at anonymous lambda expressions; a lambda
with a dropped return slot reads its return type from the body.

### 4.3. Why no let-generalization

E-FnSynth synthesizes a polytype only when the user explicitly writes the `[ᾱ]` binders. A monomorphic `.(x: Int) { … }` synthesizes the monomorphic type `(Int) → R′`, not a generalized scheme. The synthesized polytypes are precisely the polytypes the user wrote.

In particular, `let id = .[A](x) { x };` is a type error: the RHS is a polymorphic lambda literal in a synthesis-only position (E-FnSynth's domain), but the parameter `x` has no annotation. There is no way for the rule to determine `x`'s type; the user must wrap it in a polymorphic call that pins the type from the receiving slot:

```kio
fn identity[T](x: T) -> T { x }
let id = identity([A] A -> A, .[A](x) { x });
```

Now the `.[A](x) { x }` sits in a checking position (the value-arg slot of `identity`'s scheme after substituting `T := [A] A -> A`), and E-FnCheck applies.

## 5. Function application

Kio's call grammar admits a **single positional list** interleaving type-arguments and value-arguments. The elaborator reads them in source order, threading each through the callee's scheme.

```text
Γ ⊢ f ⇑ ∀ᾱ. (T₁, …, Tₙ) → R ↪ f′
explicit type-args  U₁, …, Uₘ  (each Γ ⊢ Uⱼ type)
inferred type-args  Uₘ₊₁, …, U_|ᾱ|  (solved by §§ 5.1–5.2)
each value-arg aᵢ is elaborated against Tᵢ[ᾱ := Ū]   ↪ aᵢ′
return-type expectation R[ᾱ := Ū] is consistent with the context's expected type
                                                              (if any)
─────────────────────────────────────────────────────────────────  E-App
Γ ⊢ f(Ū_explicit, a₁, …, aₙ) ⇑ R[ᾱ := Ū] ↪ f′[Ū](a₁′, …, aₙ′)
```

The rule presents one combined argument list; the elaborated tree uses the
curried Λ/λ presentation of [`prime.md`](prime.md) § 2.2 (T-App / T-TApp).
The premises describe the completed derivation. The syntax-directed procedure
that obtains it is the finite retained schedule below.

Surface E-App may consume successive `Forall` and `Function` layers from one
written list, but it fixes each function packet without type-directed
backtracking. At a product domain, every value followed by another value
consumes one right-spine slot. Only the packet-final value — the last value
before a written type argument, an explicit nested application boundary, or
the end of the list — may be checked against the whole remaining product.
Slot selection uses only the resolved callee type and written argument
structure; a value's synthesized type cannot make the procedure revisit a
different split. Kio' retains its one-layer-per-call syntax, so elaboration
records an explicit nested call at each consumed surface layer.

A syntactically empty direct/prefix surface call list contributes one Unit
value and omits the leading type arguments. E-App solves those omissions from
that written Unit value and any local expected result. The call succeeds only
when every omitted binder is solved. Thus `id()` applies
`id : ∀α. α → α` at `α := Unit`, while `nil() : List(A)` elaborates to
`nil(A, ())` only when the checked position supplies `A`; the same call in an
unconstrained position is an inference error.

A nonempty packet containing type arguments or `_` placeholders and no written
value is instead **type-only**. It performs the selected type applications and
returns the residual result without inventing Unit. Consequently `id(_)`
checked against `Unit → Unit` elaborates to residual `id(Unit)`, concrete
`id(Unit)` has type `Unit → Unit`, and `nil(A)` has type
`Unit → List(A)`. Saturation requires a written value application:
`nil(A)()`, `nil(A, ())`, and the UFCS form `().>nil(A)` elaborate to the same
typed call tree. Substitution and alias unfolding may establish that the
written value fits a Unit domain, but cannot create a value application.

UFCS inserts its receiver before E-App checks the resulting prefix plan. A
receiver-only spelling omits the argument list: `r.>f` produces the E-App plan
`f(r)`. The surface parser rejects a written empty UFCS list before E-App;
`r.>f(())` instead produces `f(r, ())` because the additional Unit is explicit.
The form `().>nil(A)` produces `nil(A, ())` because its receiver is the written
Unit value.

The distinction is determined only by the written packet, resolved callee
type, and local equality constraints. Declaration groups, runtime ABI arity,
parameter spelling, callee identity or provenance, and an elaborator's private
ABI are unavailable to E-App. The elaborated tree places every selected type
application before the written Unit application, preserving observable stage
order. Neither packet pads a product domain or advances through an unwritten
later curry layer.

After E-App has consumed at least one `Function` layer, its result may expose a
leading returned `Forall`. A concrete monomorphic expected result may
instantiate that returned binder run through one ordinary type-only successor
of the same application derivation. With no expected result, or a polymorphic
expected result, the binder run remains residual. An explicit type applies
directly and a written `_` allocates a goal that must close. These rules are
shared by ordinary, intrinsic, UFCS, and user-elaborator calls; elaborator
evaluation changes the produced term, not application selection or inference.

The successor is materialized as ordinary Kio' type application at the point
where the returned binder is consumed. It is not folded into the preceding
value call or a following value call: each returned `Forall` contributes one
nested T-TApp, in binder order. This preserves the β-TLam boundary at which the
returned polymorphic value exposes its next computation, including when the
type argument has no backend runtime representation. The completed tree alone
records that order; neither the expected-type history nor the producer's
declaration-group spelling serves as parallel authority for where the
application was inserted.

### 5.1. Application-local retained inference

An E-App establishes one finite inference domain unless it completes
independently and contributes a closed result. A descendant application whose
unfinished result must be related to an ancestor's parameter slot joins the
ancestor's domain; each application still owns its goals and retained actions,
so binders and skolems keep their lexical scope. The domain is bounded by the
resolved callee types and written syntax of the connected call tree. Here
"connected" means linked by those nested-argument/result relationships, not
merely occurring in the same function or block. These connections do not bypass
local-binding or source-first clause boundaries (§ 6 and § 7.6): later uses
cannot infer an earlier source's missing type. A sequence's generated bind
calls still share the ordinary carrier and result constraints described there.
The application domain's
schedule is fixed:

1. Establish or join the finite domain. Instantiate unsupplied call binders
   with fresh owner-scoped goals, apply written type arguments, and constrain
   the result with the enclosing expected type when one exists. If descendants
   must close before that equation can be decided, defer that owner's single
   pending equation transactionally. A returned leading binder run is admitted here only after
   a preceding value layer has completed and only under the contextual rule
   above.
2. Enter each immediate value source fresh once, from left to right. A value
   that completes independently constrains its parameter slot. A value blocked
   only on information in the connected domain retains its unconsumed source
   action; an unannotated literal without an exact expected slot is delayed
   rather than defaulted immediately.
3. Run the **expected-only pass** from left to right. Consume a retained action
   whose parameter slot is now usable, with no lexical literal fallback.
   Information from a completed sibling or connected descendant may constrain
   owner-scoped goals; a still-blocked action contributes nothing.
4. Run the **lexical-fallback pass**. Each still-untyped literal uses the
   documented lexical candidate rule from [`language.md`](../language.md)
   § Literals. Its closed type may further refine the finite domain.
5. Run a **final preflight** without executing retained source actions. It
   verifies that all remaining parameter slots and terminal states can close,
   and advances a deferred expected-result equation only after a retained-child
   closure or structural change to one of its zonked sides.
6. Run the **final pass** from left to right. Execute each now-ready retained
   action exactly once, reuse every completed derivation, and require every
   owner-scoped goal to have one consistent closed solution.

A pass may descend through several newly ready connected actions, but it does
not re-enter a source premise or replay an elaborator action. Each deferred
equation is a bounded equality check: after its initial attempt, every further
attempt requires a distinct child closure or a structural change to a zonked
equation side since the preceding attempt. A further deferral records the new
snapshot. There is no mode-driven retry, backtracking, alternate call
interpretation, global worklist, or fixed point. Scope ownership prevents a
nested binder or skolem from escaping; an independently completed child
contributes only a result closed with respect to its own domain.

The retained action does not change the typing rule of its expression. A
lambda whose parameter type is not yet known remains governed by E-FnCheck or
E-FnSynth. Its independently synthesizable body result may constrain a
surrounding return slot, but the body is never inspected to discover a missing
value-parameter type. A `let` or expression-statement prefix inside that body
must complete before its tail and blocks expected information from flowing
backwards into that earlier clause (§ 6). Semicolons separate clauses; a
trailing semicolon does not change which expression is the final result.

**Direct literal callee completion.** When `f` itself is a lambda literal, the
current connected domain may complete an absent or exactly `_`
value-parameter annotation from the exact independently synthesized type of
the corresponding value argument. It aligns slots by the ordinary E-App plan,
including product packing and successive function layers, then invokes
E-FnSynth once on the now-concrete lambda. A nested `_` still requires a
checking position. The completion does not inspect the lambda body to infer a
parameter, cross a `let`, or use an argument belonging to a later application.

A whole missing value-parameter slot may take a user-written polytype carried
by a polymorphic value. A whole inferred type-argument goal may likewise take
that exact complete polytype under § 5.2; a goal embedded as a proper subterm
of a declared monomorphic parameter shape may not. Each consumed function
layer solves only the binders introduced for that layer; binders in an
unconsumed layer remain quantified in the residual result. Kio' uses the same
direct-callee completion for an absent annotation, but has no `_` spelling and
still requires every universal call argument explicitly. Surface calls retain
their ordinary ability to span successive function layers; Kio' retains its
restriction that one flat call cannot span multiple function layers. The
completed Kio' term carries concrete lambda annotations and every inferred
type argument materialized at its ordinary type-application boundary.

### 5.2. Type-argument inference

Three forms are admissible at a call:

1. **Fully explicit** — every type-argument written out: `f(T₁, …, Tₘ, a₁, …, aₙ)` with `m = |ᾱ|`.
2. **Partial** — `_` placeholders at individual type-arg slots: `f(T₁, _, T₃, a₁, …)`. The elaborator solves each `_` from unification.
3. **Fully elided** — no type-arg slots, no `_`s: `f(a₁, …, aₙ)`. The elaborator solves every binder from unification.

In every case, the type-argument slot is **positional**: the kth written type
argument (or `_`) corresponds to the kth binder in the current `∀ᾱ` run. Type
arguments precede values within that universal/function layer; consuming its
value layer may expose a later binder run.

**Unification sources.** Each unsolved binder αⱼ may be constrained by written
type arguments, the enclosing expected result, an independently synthesized
value, a retained value checked against its refined slot, and the type of a
checked term returned by a user elaborator under § 7.4. All sources belong to
the same finite application derivation. The rule solves αⱼ if and only if its
equalities reduce to one concrete, well-kinded type.

**Exact polytype solutions.** An inferred Uⱼ may be a complete polytype only
when one of those finite premises supplies the whole type tree, with no open
inference goal beneath its `Forall` nodes. The owner binds αⱼ to that tree as a
single solution; subsequent premises must be structurally alpha-equivalent.
The rule never constructs a `Forall`, moves one across an arrow, or recursively
searches for a binder arrangement that would make a partial shape fit. An
explicit user-written Uⱼ, for example `f([A] A -> A, value)`, is checked by the
same structural relation but does not require inference.

**Failure modes.** A binder that cannot be solved (no equality pins it, or
equalities conflict) is a type error. An unresolved goal is never interpreted
as `_`, a wildcard, or a fresh universal. Once every binder has a unique
solution, the call's value arguments are concrete and satisfy their substituted
parameter types (`aᵢ ⇓ Tᵢ[ᾱ := Ū]`).

**Arguments without standalone synthesis.** An unannotated lambda and a nested
call whose result-only binders remain open cannot contribute a completed type
during their first visit. Their retained actions let the owning slot become
more precise first. For example, a sibling value may pin the outer `S` in
`view(fst_lens(), value)`, after which the expected `Lens(S, A)` result can
close `fst_lens`'s nested binders. The nested call is not re-run: its unfinished
source action is consumed once under that expected result.

**UFCS provenance.** A UFCS call resolves the written callee, reads its type/value slot sequence, and inserts the receiver into a value slot before applying E-App. `.>` and `.<<` insert into the first value slot; `.>>` and `.<` insert into the last value slot. Type arguments remain in their declared type slots, so `r.>f(T, x)` has the same E-App plan as `f(T, r, x)`, and `r.>>f(T, x)` has the same plan as `f(T, x, r)`. At the receiver boundary, type-looking candidates are reserved for the consecutive binder run immediately after the receiver; the adjacent binder run before the receiver consumes only the surplus. An unfilled value layer is a hard boundary: reservation never crosses it. This split uses only the resolved slot sequence and written argument kinds, with no inferred-type feedback or retry. Bang-call UFCS uses the same slot rule before invoking the user-elaborator call rule.

### 5.3. Value-argument checking against polytype slots

When a callee parameter slot is itself a polytype — i.e., the callee's signature includes a rank-2-or-higher position — the value at that slot is checked against the polytype. E-FnCheck (§ 4.1) is the rule that fires when the value is a lambda literal. For non-lambda values (a path expression referring to a top-level polymorphic `fn`, an existential-projector continuation, etc.), the standard equality check between the value's synthesized polytype and the parameter polytype applies — shallow skolemization is not needed because the value already has a polytype shape; the typer checks structural equivalence.

## 6. Let bindings

```text
Γ ⊢ e ⇑ T ↪ e′                    (E synthesized with no expected type)
Γ, x : T ⊢ rest ⇑ U ↪ rest′
─────────────────────────────────────  E-LetSynth
Γ ⊢ let x = e ; rest ⇑ U ↪ let x = e′ ; rest′

Γ ⊢ T type                         (T is fully concrete: contains no `_`)
Γ ⊢ e ⇓ T ↪ e′
Γ, x : T ⊢ rest ⇑ U ↪ rest′
─────────────────────────────────────  E-LetCheck
Γ ⊢ let .(x : T) = e ; rest ⇑ U ↪ let x = e′ ; rest′
```

**The rules.** `let x = e;` types `e` in ⇑ (synthesize) mode and binds `x` to the synthesized type T, unchanged. No narrowing, no widening, no generalization, no defaulting — the binding takes the RHS's synthesized type as-is. A polytype RHS yields a polytype binding; a monotype RHS yields a monotype binding; no extra rule is needed for either case.

`let .(x : T) = e;` is the unary binding-pattern form. When `T` is fully
concrete, the binder supplies a checked RHS position: `e` is checked against
`T`, and `x` is bound at `T`. The annotation is a surface checking boundary;
the elaborated Kio' output erases it and retains the ordinary
`let x = e′; rest′` shape.

Binding patterns generalize the unary rule. Let `PatType(pat) = T` when every leaf of `pat` carries a concrete type and the pattern's product shape derives a concrete product type. Let `Binders(pat)` be the vector of local names and their leaf/product types. Then:

```text
PatType(pat) = T
Γ ⊢ e ⇓ T ↪ e′
Γ, Binders(pat) ⊢ rest ⇑ U ↪ rest′
────────────────────────────────────────  E-LetPatternCheck
Γ ⊢ let pat = e ; rest ⇑ U ↪ desugar_project(pat, e′, rest′)
```

If `PatType(pat)` is undefined because at least one leaf is `_` or omitted,
the written annotation occurrences are first classified by the same exact
post-materialization `forall` predicate as E-FnCheck. An inadmissible source
occurrence is rejected before an annotation equation or publication is
created. Otherwise the RHS uses E-LetSynth. Its synthesized type is
structurally compared with the written concrete leaves and product shape while
elided leaves are filled from the synthesized type. Kio deliberately does
**not** push a partial expected product into the RHS and does not solve an RHS
from only the annotated leaves of a mixed pattern. Recursive body-first
processing transports the one written source occurrence without duplicating
it; a generated body-first route with no written annotation cannot trigger
this diagnostic.

### 6.1. Why no narrowing

The Hindley-Milner-era defaulting rules (ML's value restriction, Haskell's `default` declarations) exist because un-annotated inference can produce ambiguous polymorphic types — a choice the elaborator has to resolve before the program can run. The classical `let f = .(x) { x };` is the canonical case: the RHS synthesizes a constraint that admits the polytype `∀α. α → α`, the monotype `Int → Int`, and infinitely many other instantiations; such a system picks one by a defaulting rule.

**Kio has no such ambiguity.** Every type-binder in the language is user-written:

- Universal binders (`[A]`) at top-level `fn` signatures, prefix-binder type expressions, and `fn`-literal headers.
- Existential binders (`<U>`) at `newtype` payload positions.

Any polytype the elaborator synthesizes traces back to a declared binder in the source. The typer never invents a `∀`. By § 4.2 (E-FnSynth), a lambda literal in synthesizing position synthesizes a polytype only when the user wrote the `[ᾱ]` binder list; a monomorphic `.(x: Int) { … }` synthesizes the monomorphic `(Int) → R′`. By § 5.2, an inferred type-argument slot may copy an exact complete polytype already supplied by finite application-local evidence, but cannot generalize, invent, or rearrange its binders.

Therefore the RHS's synthesized type is always unambiguous — there is no defaulting choice to make. E-LetSynth does not narrow because there is nothing to narrow from: the RHS already names its own type, polytype or monotype, exactly.

**Consequence — polytype RHS.** `let f = id;` where `id : [T] T -> T` is a path expression to a top-level polymorphic `fn` (E-Named, § 3): `id` synthesizes its declared polytype; `f` inherits it. The bound `f` is usable at every instantiation `id` accepts. This is the principal example — the RHS already has a polytype, the let-rule preserves it.

**Consequence — lambda-literal RHS without explicit binders.** `let id = .(x) { x };` is rejected, but the rejection is at the **lambda-literal rule**, not the let rule. The RHS sits in synthesizing position (E-LetSynth's RHS is ⇑); E-FnSynth (§ 4.2) requires every value-parameter slot to carry a concrete annotation, and `x` has none. The error names the lambda literal, not the let. The user fixes it by giving the lambda a checking position (so E-FnCheck applies), either with a concrete unary binder such as `let .(f: Int -> Int) = .(x) { x };` or by wrapping it in a polymorphic call:

```kio
fn identity[T](x: T) -> T { x }
let id = identity([A] A -> A, .[A](x) { x });
```

Now `.[A](x) { x }` sits in a checking position (the value-arg slot of `identity`'s scheme after substituting `T := [A] A -> A`), and E-FnCheck applies.

**Consequence — checked factory call.** A fully concrete typed let can solve local call-site type arguments from the checked return position. If `make_id : [A] . -> A -> A`, then `let .(f: Int -> Int) = make_id();` checks the call against `Int -> Int` and solves `A := Int`. The bare path `let .(f: Int -> Int) = make_id;` still fails when `make_id` synthesizes a polymorphic scheme rather than a value of exactly `Int -> Int`; the checked-let rule is not implicit instantiation of a path outside the ordinary call rule.

**Consequence — no let-generalization.** "No let-generalization" is not a separate restriction the rule imposes; it is what falls out of "the RHS's synthesized type is taken as-is" plus "there are no free inference variables to generalize over." Hindley-Milner generalization closes a synthesized type over its free metavariables; in Kio, the unification machinery solves every metavariable before the let-rule sees the synthesized type (§ 5.2's failure mode catches anything else), so there is no free metavariable for the let-rule to generalize over even if it wanted to.

**No backward type flow.** The RHS mode is chosen before the body is examined: synthesize for E-LetSynth and partial patterns, check against the binder-derived type for E-LetCheck and E-LetPatternCheck. The body's uses of `x` cannot propagate an expected type back to E. This is the rule that closes the loop on PJ-V-W-S's discipline — backward flow into a `let` RHS would force the typer to delay RHS typing until the body is examined, and that delay is what makes inference undecidable in the impredicative case. By splitting `let` into "type RHS from the local binder only, bind, then synthesize the body," the elaborator's discipline stays first-order. This is a separate property from § 6.1's "no narrowing": § 6.1 says an untyped RHS's synthesized type is taken as-is once obtained; "no backward flow" says nothing from the body informs the RHS in the first place.

**A statement-form `let x = e;` followed by further block contents** is presented here in the nested-binding shape used by [`prime.md`](prime.md) § 1.2 (T-Let). The block-form-versus-nested-form distinction is purely syntactic; the typing rule is the same either way.

## 7. Other forms

### 7.1. Unit, literals, and absurd

Unit and the three literal forms (integers, floats, strings — admitted at the elaborator with the role-admission check from [`prime.md`](prime.md) § 2.2) synthesize their type from the literal's lexical shape and the role environment. The rules are inherited verbatim from [`prime.md`](prime.md):

```text
──────────────────  E-Unit
Γ ⊢ () ⇑ . ↪ ()

(B : role(bool) ∈ Γ_roles)
─────────────────────────────  E-True/False
Γ ⊢ .t(B) ⇑ B ↪ .t(B)
   etc.
```

Unannotated surface literals (the tier-2 / tier-3 resolution described in [`language.md`](../language.md) § Literals) are resolved to one of these annotated forms before E-True / E-Float / etc. fires. That resolution is part of the elaborator's responsibility; the rule fires only on the resolved form.

### 7.2. Blocks

A block `{ stmt₁; …; stmtₖ; e }` desugars to nested `let` and expression-statement forms (each statement `s;` desugars to `let _ = s; rest` after `s`'s type is checked equal to `.` per [`language.md`](../language.md) § Blocks and local bindings). The elaborator threads the block via the E-Let rule for each statement and synthesizes the final expression's type as the block's type.

### 7.3. Tuples, sums, and products

Tuple literals `(a, b, …)` parse-desugar to nested `__pair__` calls. The elaborator sees standard E-App calls on `__pair__`. Similarly, the value-level sum / product accessors (`__fst__`, `__snd__`, `__left__`, `__right__`, `__either__`) are ordinary named callables; their registry entries give the schemes (per [`prime.md`](prime.md) § 2.4) and E-App handles the typing.

### 7.4. Elaborator bangs

A bang call `name!(args...)` resolves `name` through ordinary scope to a user-defined elaborator declaration imported explicitly from a regular or root module. The declaration supplies a call type and names the code that implements it:

1. Resolve the declaration and instantiate its call type in the enclosing
   E-App inference domain. Explicit type arguments, `_` placeholders, value
   arguments, and any surrounding expected type constrain that declared
   shape. Any still-open result positions receive call-local goals before the
   implementation is considered. Call completion, grouping, partial
   application and residualization, and UFCS placement use only this resolved
   public type, the written syntax, and those local constraints. The private
   implementation ABI does not participate in that choice.
2. Resolve every declared capture. A captured type contributes a quoted `__Type__` descriptor and a captured value contributes a quoted `__Checked_term__`; neither executes the captured declaration at compile time. A capture may therefore name a host item. Because the quoted dependency can survive in the generated runtime term, each capture must be at least as visible as the elaborator.
3. Resolve the implementation's lexical value path in the declaration module.
   A bare head denotes a local declaration or selective import; dotted
   segments select through an explicitly imported module alias or a
   local/imported newtype. A slash-qualified package FQN and every non-path
   expression are rejected. Written imports alone create cross-module
   dependency edges; resolution performs no ambient module scan. Check the
   resolved implementation under the function-purity judgment from
   [`prime.md`](prime.md#26-function-purity), extended only with the explicit
   compile-time primitive environment. The implementation and every ordinary
   function it reaches transitively must be declared `pure fn`. An
   implementation declared in the elaborator's defining module may remain
   private, and any implementation may call private helpers in its own module.
   A target declared in another module must satisfy ordinary visibility for its
   written import into the defining module, but need not meet the elaborator's
   outward visibility. Callers import the elaborator, not its compile-time
   implementation details. Local annotations and implementation ABI types use
   ordinary type well-formedness; named types in the elaborator's call
   signature obey the declaration-visibility floor.
4. Close every dependency according to the declaration's implementation mode.
   For ordinary `impl f`, the types reflected for captures and checked value
   slots contain no inference goal before they become `__Type__` or
   `__Checked_term__` values. A type slot represented by `__Type__ | .` may
   receive unit only when that exact declaration binder occurs in the declared
   result and has no remaining value dependency. For `impl(fills) f`, captures
   still close, every declared type slot is an exact `__Type__` handle, and the
   typer advances every declared value operand through its complete structural
   header frontier in declared-slot and written-source order. A pending body is
   represented by a provisional checked recipe and is not evaluated by the
   compile-time implementation. After those recipes are snapshotted and before
   the implementation request is built or evaluated, traverse every declared
   source and its recipe in structural source order, including sources whose
   private ABI value is ignored. Traverse explicit product components
   recursively. Every computed non-lambda leaf requires a closed recipe type,
   including a leaf without retained producer work. Declared type variables
   and explicit universal binders are closed; unresolved `Goal` or `Infer`
   occurrences are not. The enclosing expected type and already available
   sibling constraints from the public call may close the type before this
   check; fills or callback-body checking after the action cannot do so.
   Ordinary call-header
   preparation may relate a callback's complete written function type to its
   already-selected public argument expectation without entering the body.
   Missing or partial callback annotations supply no such complete type. This
   uses the ordinary public call plan and local equations; it neither changes
   grouping nor evaluates a nested elaborator or immediate-lambda callee.
   A goal-free result may cross each authenticated direct-parent scope into an
   enclosing ordinary call without closing or publishing the pending child.
   Every retained callback body is checked once before publication, even for
   an ignored operand. The only open
   leaf admitted is a direct lambda whose canonical
   signature has a complete `forall` and value-parameter prefix, contains no
   `Infer`, and confines every remaining `Goal` to the final body-result
   subtree. The ordinary signature grouping and ABI arity determine that
   prefix; in particular, one product-valued parameter remains one slot rather
   than being flattened. Reject the first other open value source as
   a type error at that source before the implementation action, retained-body
   resumption, or publication. A lambda literal with explicit type binders may
   obtain this frontier from E-FnCheck or a concrete return annotation. When
   marked staging instead synthesizes an omitted or `_` return with a complete
   written binder and parameter prefix, reserve its provisional final result
   in the surrounding premise scope, outside the newly introduced binders.
   Abstract the written binders over the header and export it through each
   direct parent with the ordinary scope and free-variable checks. The
   implementation may observe that open header. Before entering the one
   generic body, the enclosing expectation or adopted finite fills must
   independently determine its result and select E-FnCheck; an internally
   retained open header alone is not such evidence. Reject an undetermined
   result at the source before body entry or publication, including for an
   ignored source. The provisional result cannot depend on the newly
   introduced binders; dependent results retain the concrete-annotation and
   complete-expected-type routes. E-FnSynth remains unchanged outside this
   marked dependency-closing step, as does the unit default for an omitted
   top-level function return.
5. Build exactly one request using the selected ABI:

   ```text
   impl:
     (__Comptime__ & captures... & slots...) -> __Checked_term__

   impl(fills):
     (__Comptime__ & __Fill_ctx__ & captures... & slots...)
       -> (__Checked_term__ & __Fill_ctx__)
   ```

   The public call plan from step 1 is unchanged by this choice.
   `__Comptime__` is the immutable first argument in both modes. Only marked
   mode receives the fresh call-local `__Fill_ctx__`, exactly second.
6. Evaluate the selected implementation once against the explicit compile-time
   primitive environment, or replay an equivalent completed request, never
   both. Ordinary mode yields one checked term. Marked mode yields one
   provisional checked recipe and the final immutable fill context. The
   implementation cannot inspect or solve through that context while it runs.
   Reflected operands are authenticated before reduction. After aliases
   unfold, folding a type's applied arguments and applying one argument to a
   type head reduce only when that outer type or head, respectively, is
   determined; otherwise the operation remains residual rather than returning
   the fold initializer or the unchanged head.
7. In ordinary mode, match the returned checked term's actual type against the
   declaration result as before; it may solve only result goals allocated in
   step 1. In marked mode, flatten the returned context to its ordered
   transcript `F = [(d₁, c₁), ..., (dₙ, cₙ)]`, authenticate every entry,
   and partition the entries by live destination equivalence class before
   solving any of them. An externally closed destination contributes checks
   only. For an open destination `d` with candidates `c₁, ..., cₖ`, register
   the bounded equation set `d = c₁, ..., d = cₖ` as one symmetric relation.
   Transcript order preserves authored traversal; it does not select an
   inference anchor. Any candidate that closes from its own complete producer
   set may determine or refine `d`, after which all other candidates check that
   same result. A malformed entry fails the group. If no context or candidate
   closes `d`, the relation is underdetermined; independently closed candidates
   that conflict produce a non-directional common-result mismatch. Register the
   complete relation set before any retained body resumes.
8. Resume each retained source body at most once under the ordinary bounded
   typing modes. Validate the returned recipe against the declared result;
   unlike ordinary mode, that validation cannot implicitly solve an open
   marked result. The transcript relations, retained-body writes, and
   elaboration output become visible atomically, or none does on failure.
   Record the closed Kio' term in the side channel for substitution at the
   Lowered → Prime boundary.
9. After all substitutions, validate the assembled Kio' package as standalone
   input. This final validation checks the generated term under its actual
   caller: a host-function or unmarked-function reference is rejected when
   substituted into a `pure fn`, while the same term is admissible in an
   ordinary function. No trust in the elaborator declaration, evaluator, or
   side channel replaces that check.

For a blockless elaborator declaration without trailing descriptors,
applying only a type prefix of its declared call type produces the
same residual value function as applying that prefix to an ordinary function.
Because bang syntax cannot survive into Kio', the front-end represents that
residual as an ordinary typed Kio' function whose symbolic value parameters
are supplied to the same closed elaborator request. This is phase lowering,
not a separate inference rule: prefix, UFCS, and bang spellings select the
same slots, and the residual cannot be rejected merely because its source
callable is an elaborator.

An `elab` declaration has no `pure` modifier or purity bit. Compile-time execution of its implementation is unconditionally pure; purity of the generated runtime term is a separate property checked after substitution.

The marked header projection treats syntax-owned product shells structurally.
A direct callable shell is exposed only when ordinary resolution identifies a
reserved intrinsic and its complete alpha- and alias-aware scheme is uniquely
the canonical two-binder, two-value `(A & B)` constructor with each input
corresponding to the same ordered result component. The original call wrapper
is then completed and published once. An ordinary function remains opaque even
with an equivalent scheme or product result, since it may reorder, duplicate,
or discard an input. This classification depends on resolved intrinsic kind
and complete scheme, never a name, module identity, result shape alone, or raw
argument index.

The compile-time primitive

```text
__fill__ :
  (__Comptime__ & __Fill_ctx__ & __Type__ & __Type__) -> __Fill_ctx__
```

appends exactly one inert relation to the supplied context. Its only semantic
ordering is the order induced by explicit immutable state threading. It reads
neither inference state nor how its destination relation is later solved.
The destination evidence must denote a focus path rooted in this call's
declared result; source operands, constructed tuples, and arbitrary reflected
types do not acquire that authority.
`__Fill_ctx__` has no source constructor and no conversion to or from
`__Comptime__`, so ordinary implementations cannot obtain fill authority.

The primitive

```text
__term_specialize__ :
  (__Comptime__ & __Checked_term__ & __Type__ & __Type__)
    -> (__Checked_term__ | (. | __Diagnostic_text__))
```

relates a term's explicit leading `forall` binders to a pattern and target by
exact structural specialization, not subtyping or containment, in an isolated
finite goal domain. Success returns the corresponding binder-order
type applications whose term type is the fully instantiated whole scheme, an
ordinary structural mismatch returns unit, and malformed
or underdetermined input returns diagnostic text. No temporary goal or fill
authority escapes.

The dedicated `__structural_recur__` compile-time helper uses ordinary call
syntax, the fixed `__comptime__` helper scheme, and the evaluator's
structural-decrease check. A direct projected type handle is measured over its
authenticated alias-unfolded structural carrier, captured while ordinary alias
authority is available. The exact scoped snapshot remains the
authority for whole-handle reflection and fills, but it does not determine a
structural view or fuel measure. Transparent aliases are unfolded throughout
the carrier in one structural pass for each captured snapshot, and structural
children use the corresponding semantic carrier children. For the initial
root, a `Goal` or `Infer` at the carrier root
is unmeasurable; a known outer constructor instead has a conservative
structural lower bound in which nested `Goal` and `Infer` children contribute
zero. A direct projected handle used on a recursive edge requires the complete
measure and is unmeasurable if either form occurs anywhere in its carrier. When
an ordinary finite data constructor contains a projected handle as a field,
that field uses the same known-root lower bound in both initial and recursive
measurements; a root-`Goal` or root-`Infer` field still makes the enclosing fuel
unmeasurable. Closed projected carriers therefore retain their full structural
measure, and a structurally complete child of an open carrier may still have
one. Projected checked-term recipes remain atomic fuel. The evaluator retains
the root measure for diagnostics. Every recursive callback computes the
recursive measure of its next fuel and requires it to be strictly below both
the current fuel measure and the callback's stored root measure. An
evaluator-created callback starts with `current = root` and preserves
`current ≤ root`, so current descent also establishes the root bound for those
callbacks. A callback constructed directly through the public evaluator API
can carry inconsistent stored measures and is checked against both
independently.

Each resolved source occurrence of the `__structural_recur__` value has a
phase-local helper origin. Direct and grouped calls use that origin, and a
helper value retains it through proof/type application stages and higher-order
use. A local value with the same spelling is an ordinary local and acquires
no helper origin. Ordinary artifact-backed local, function, member, and import
resolution runs before the evaluator attaches an origin; later origin
comparison and consumption require no declaration search. One preparation of
a validated evaluator artifact supplies a nominal generation namespace for
its origins: clones of that prepared artifact retain the namespace, while an
independent preparation receives a distinct namespace even when its source is
byte-identical. A helper value retains its namespace when it crosses into a
different evaluator context. A public mutable evaluator root that is not held
behind an immutable authenticated artifact context receives a fresh nominal
namespace for that root. The helper's three operands and the callback's two
operands follow E-App's ordinary product-packet equivalence: each layer accepts
either flattened slots or one right-folded product value. Call operands are
evaluated before a completed direct or indirect application enters its origin.
An origin with no active invocation uses the initial measure above. A re-entry
at the same helper origin instead computes the recursive measure and requires
it to be strictly below the nearest active invocation's measure. A direct
projected handle's conservative initial lower bound is therefore never reused
as its re-entry measure.
Equal-measure fuel is rejected independently of value identity,
representation, or whether it contains a projected handle. The oldest active
entry at that origin supplies the diagnostic root, while a callback chain
retains the measure of the invocation that created it as its own root. A
callback rejects the edge when `next ≥ current` or `next ≥ root`. When both
bounds fail, the diagnostic identifies the current edge; the root-specific
diagnostic applies only when `next < current` but `next ≥ root`.

Strict descent along an evaluator-created callback chain keeps every recursive
measure below that chain's root measure. Invocations from distinct helper
origins do not compare measures. One evaluator operation can reach only
finitely many retained helper origins, so an infinite nesting would revisit
one active origin infinitely often and contradict strict natural-number
descent at that origin. Infinite or unbounded sequential repetition can only
be driven by a callback chain, whose measures also strictly descend; finite
ordinary calls may repeat an origin sequentially. Once an evaluated helper
entry or callback edge fails this
check, enclosing call-by-value computation cannot discard the failure: the
outer evaluator operation returns its first such totality failure, and the
next operation starts clean. Memoization neither replaces evaluation while a
helper origin is active nor stores an apparent result after that operation has
recorded a failure. Helper origins exist only in the compile-time evaluator:
within their nominal generation they are derived from resolved module identity
and the exact retained helper-expression node, not from diagnostic span
equality, and they do not enter Kio', serialized artifacts, or backend output.
Cloning a prepared artifact retains those exact immutable nodes; an independent
preparation or public mutable evaluator root receives a new nominal generation.
Adding unrelated declarations may shift source offsets, but within one prepared
artifact or root it cannot change which invocations share a helper origin,
merge distinct resolved helper occurrences, change an existing occurrence's
resolution, or change its behavior.

### 7.5. Existential newtype CPS projector

A newtype with existential binders `<u₁> … <uₙ>` carries a CPS projector whose scheme is

```text
N.un_N : ∀ᾱ. (N(ᾱ)) → ∀ρ. (∀β₁ … βₙ. (Payload[ᾱ, β̄]) → ρ) → ρ
```

(see [`language.md`](../language.md) § Existential type binders). Applying the projector to a value `v : N(T̄)` yields the inner polymorphic CPS function; the user then applies that to `(R, k)` where `k` is the continuation. The continuation `k` is checked against `∀β₁ … βₙ. (Payload[T̄, β̄]) → R`:

- The inner `∀β₁ … βₙ.` skolemizes via E-FnCheck (§ 4.1).
- The body of `k` is typed under the skolemized context.
- The escape check rejects any body whose synthesized type mentions a `β_skol` outside the continuation's scope. This is what gives the existential's safety: the witnesses cannot leak out into `R`.

The `let .(<U_1> … <U_n> x) = e;` block-statement sugar (per [`language.md`](../language.md) § Existential-opening `let` sugar) desugars at parse time to a CPS-call wrapping the continuation, so the elaborator sees a standard E-App on the projector with a `fn`-literal value-arg in continuation position.

### 7.6. Block elaborator projection

A marked block call parses independently of declarations. After ordinary name
resolution, each `trailing` descriptor selects the projection for one final
value slot of the resolved public call type. Every declared block is required
once, in declaration order. Prefixes are value-only, type arguments are
inferred, and the call is complete and direct; there is no blockless or UFCS
alternate for a declaration with descriptors.

- `product` projects independent expressions to unit, a single value, or a
  right-associated product. Top-level bindings are rejected; a local scope
  inside one entry does not bind names in another entry.
- `thunk` projects an expression-block body to a zero-argument function. Empty
  gives unit in its body. Ordinary lets are admitted, and `<-` is rejected.
- `sequence` projects an ordinary function taking a bind of structural type
  `[A][B] (F(A) & (A -> F(B))) -> F(B)` and returning `F(R)`. Bind and sequence
  steps become ordinary calls with nested continuations; the final expression
  is explicit. Empty and statement-only sequences are type errors.

These projections enter the ordinary elaborator application rules of § 7.4.
The private implementation ABI does not choose public slots or type flow.
Local lets use E-LetSynth / E-LetCheck, completing the prefix before the tail.
A written bind annotation retains its one original source occurrence and the
ordinary post-materialization `forall` predicate. The bind type connects
`e : F(A)` to its continuation parameter, and ordinary expected-result flow
may determine `B`. Later continuation uses do not infer the earlier payload
across its binding boundary. A sequenced expression requires `F(.)`, whereas
`let _ <- e` may discard a non-unit payload. No implicit lift is inserted.

The reference `if!`, `scope!`, `do!`, and `match!` are ordinary library
declarations using these projections. `if!` relates its two thunk results
through the repeated result in its public type. Its condition synthesizes
first and must have the singleton Boolean-role host type required by the
branching intrinsic; result information never flows backwards into it. A surrounding result checks
both arms. Otherwise either independently determined arm may determine or
refine the common result, with no source-order priority.

The reference `match!` independently determines each clause's dispatch-pattern
parameter type, then relates returned occurrences through the authenticated
fill transcript. Every written clause participates in validation, including
a later shadowed clause. Compatible polymorphic specializations are recorded
in source-branch order, without giving that order inference priority. The
marked-lambda staging rules of § 7.4 apply to inline clauses: an independently
known result may check an omitted return before entering the body; a return
depending on a newly introduced clause binder requires a concrete annotation
or a complete expected function type. A surrounding common result checks
clause bodies without supplying dispatch-pattern parameter types. First-fit
dispatch remains a separate library operation; its algorithm is documented in
[`../../docs/poc/elab.md`](../../docs/poc/elab.md).

An incompatible alternative group reports a non-directional common-result
mismatch; an underdetermined group needs an ordinary source of result
information. The same symmetric relation preserves late expected-result flow
without replaying a source body or elaborator action. Every equation is fixed
by the resolved public type and finite authenticated transcript. No rule
searches ambient declarations, so adding unrelated declarations cannot change
the existing call's interpretation, result, or dispatch.

Runtime behavior follows ordinary expanded code: reference `if!` evaluates
its condition once and only its selected branch; `scope!` invokes its thunk
once; `do!` applies the sequence to its supplied bind value. Recursive tail
behavior follows structural use of the expanded bodies, never their names or
descriptors. Sequence continuations remain ordinary nested functions.

All block syntax, descriptors, projection metadata, and implementation state
are consumed before Kio'. The persistent result contains ordinary functions,
applications, lets, and intrinsic calls validated under the existing Kio'
contract. See [`language.md`](../language.md#elaborators-are-imported-not-ambient)
for syntax and diagnostic categories.

### 7.7. Bottom and `__absurd__`

The bottom type `!` has no introduction form in Kio surface (see [`language.md`](../language.md) § Anonymous sum and product types). It enters a program only via a host function's return-type annotation or via `__absurd__`. The `__absurd__` intrinsic's scheme `∀α. (!) → α` is in the named-callable registry; E-App handles the call.

## 8. Kio-info-erasure property

**Theorem (Surface-form erasure).** After elaboration, every surface-only form has been removed from the AST. The residual tree contains:

- Variables, `let`, function literals (`λ`/`Λ`), applications.
- Unit, role-annotated literals.
- Named callables — intrinsics and `__if_then_else__` route through the `intrinsic_scheme` registry; user-defined elaborators route through `Expr::UserElaborator`. Elaborator forms record their Kio' tree in the side channel and are substituted at the Lowered → Prime boundary.
- Newtype constructor / projector members `N.c` / `N.p`.
- Host items.

**No block call, trailing descriptor, neutral block, projection metadata,
UFCS node, surface tuple, or user-elaborator node** survives into Kio'.
Type-directed forms and UFCS record a Kio'-shaped elaboration or normalized
call for substitution at the Lowered → Prime boundary. Purely syntactic
forms such as tuple sugar are rewritten earlier. Elaboration-bearing variants
become statically uninhabited after substitution — see [`prime.md`](prime.md)
for the phase-typed AST discipline.

The `pure` marker on an ordinary top-level `fn` is Kio' syntax rather than a surface-only elaborator form, so it remains on the assembled package for standalone validation. Elaborator declarations, their ordinary-or-fills implementation mode, implementation-path selection, `__Comptime__`, `__Fill_ctx__`, provisional recipes, fill transcripts, and the compile-time primitive environment do not remain in that package. An implementation or helper remains an ordinary function declaration; before the persistent Kio' boundary, a body gated by a leading `__Comptime__` proof is replaced with an ordinary `__absurd__` body and compile-time-only types are erased. `__Fill_ctx__` is never an alternate erasure receiver.

**Proof sketch.** By induction over the syntactic forms admissible in Kio
surface but not in Kio'. Each type-directed form records a Kio'-shaped
elaboration, which the substitute pass installs. Generic block projection
produces ordinary value operands for § 7.4 and is fully consumed with that
call. Each purely syntactic form has a desugar rule before typing. The set of
surface-only forms is finite and enumerated; the rules cover every one.

**Corollary (Kio' soundness inheritance).** Type safety, strong normalization, and decidability — proved for Kio' in [`prime.md`](prime.md) §§ 4 / 5 / 6 — apply to every Kio program whose elaboration succeeds. The elaborator's contract is that successful elaboration yields a Kio'-shaped tree; that tree satisfies the residual properties by inheritance.

## 9. IR-preservation contract

**Definition.** A pass after the typer (Lowered, Routed, Prime, backend-emit) **preserves** the elaborator's decisions when, for every typing decision made during elaboration, the IR node consuming the source for that decision carries enough information to reconstruct it. Concretely:

- **Type-arguments solved at each call site** are recorded on the IR call node. A later consumer that needs the type-arg substitution reads it off the node; it never re-runs unification.
- **Returned or computed type applications** remain ordered one-binder
  applications at their Kio' position. Backend routing may keep immediately
  adjacent leading applications with a classified call, but it never erases
  the stage, moves it across a value application, or reconstructs it from a
  declaration signature.
- **Expected-type-from-context pinning** at a `fn`-literal site is recorded on the elaborated Λ/λ. A later pass that needs the inferred parameter types reads them; it never re-derives them.
- **Polymorphic-payload erasure choices** at newtype constructor / projector calls are recorded on the call node.
- **Per-branch dispatch choices** produced by the `match!` library, the exact Boolean type at `__if_then_else__`, the carrier at projected sequence calls, and the sequence step's generated Unit parameter distinct from an explicit `let _ <- e` bind — all recorded in ordinary elaborated terms.

**Theorem (IR-preservation).** Every Routed `Expr` variant whose source carried a typing decision preserves that decision at the IR node. The elaborator's outputs flow through the IR; no later pass needs to re-derive a typing fact.

**Proof.** By case analysis over the Routed `Expr` variants: each variant that consumes a typing decision carries that decision in the IR node that later passes read. □

**Why it matters.** Bugs in the "elaboration → backend" pipeline historically traced to type information being re-derived (or guessed at) later instead of carried. The IR-preservation contract is the structural rule that closes that bug class. Each Routed `Expr` variant must carry the typing decisions its later consumers need.

## 10. Decidability

**Theorem (Decidability).** For every well-formed Kio module M, type-checking each top-level declaration's body — and, transitively, every sub-expression — is decidable. The elaborator either produces a Kio'-shaped tree (with all type-args solved and all surface forms erased) or rejects with a structured type error.

**Proof sketch.** Adapted from PJ-V-W-S 2007 § 6 (Theorem 1, Decidability of `infer`):

1. **Source-bounded exact instantiation** — an inferred type argument is either
   a monotype solved by ordinary equality or one complete polytype tree copied
   from a premise in the finite connected domain. The polytype is assigned only
   to a whole goal, is already closed with respect to inference goals, and is
   compared structurally modulo alpha-renaming. The typer never invents or
   generalizes a binder, guesses its placement, or performs higher-order
   matching.
2. **Shallow skolemization** — the rank of a checked polytype is bounded by the syntactic rank of the user-written annotation; the typer never synthesizes higher-rank types implicitly. Each E-FnCheck call processes one ∀ layer; recursive calls into sub-expressions are at strictly lower or equal rank.
3. **Explicit binders** — every ∀ in the elaborated tree corresponds to a user-written `[α]` binder somewhere in the source (signature, prefix-binder type expression, or declaration header). The set of binders is bounded by the source size.
4. **No backward type flow into `let` RHS** — a let RHS is either synthesized with no expected type or checked against the fully concrete binder-derived type; the typer never waits on the body before deciding the RHS mode. Each let-binding is processed independently, and partial patterns do not introduce product-shaped constraint solving.
5. **Finite owner-scoped application domains** — every equality goal is
   introduced by written syntax and a resolved callee type in one connected call
   tree; unification only solves or joins those finite goals. Source premises
   and elaborator actions are entered once, retained actions are consumed once,
   and each application owner has at most one pending expected-result equation,
   whose attempts are bounded by its retained-child closures and structural
   changes to its zonked sides. A connected tree may therefore contain
   finitely many such equations, one per owner, and finitely many attempts per
   equation. Later uses do not infer an earlier expression- or sequence-clause
   source's missing type; ordinary bind carrier/result constraints remain
   available. No goal or binder escapes its lexical owner.

Together, these give a terminating algorithm: each rule reduces to
sub-derivations on smaller subexpressions or shallower polytypes; monotype
unification and alpha-equivalent comparison of already-complete polytype trees
are decidable and monotone over a source-bounded goal set; each retained action
is one-shot, and every deferred-equation attempt consumes a distinct
child-closure or structural-side progress event from that same finite domain.
The recursion therefore bottoms out at the leaves without a fixed point.

PJ-V-W-S supplies the bidirectional arbitrary-rank base. Kio's additional
connected application schedule establishes termination through the separate
finite-domain and one-shot arguments above. No asymptotic complexity bound for
the combined implementation is asserted here.

## 11. Open / out of scope

The following concerns are deliberately not formalized on this page; the prose specs are the contract.

- **Early surface forms.** Tuple-literal expansion, `.stem. { ... }` placeholder lambda expansion, and the existential-opening `let` sugar happen before the elaborator sees the AST. Dot-splice normalization is type-directed and happens during typing, because the receiver can only be inserted after the callee's type/value slots are known. The grammar in [`grammar.md`](../grammar.md) pins each surface shape.
- **Order-sensitive visibility and the module-level import graph.** Top-level declaration ordering rules, `import` imports, and the cycle-prevention property of the module graph belong to the module system. The elaborator assumes a well-formed module context; the rules for constructing that context live in [`language.md`](../language.md) § Module system and [`prime.md`](prime.md) § 2.7.
- **Surface naming and resolution.** The name-resolution rules (single-segment paths vs. qualified `m.f` paths vs. type-member `T.f` paths, the value/type namespace split, and label construction/access resolution) belong to the front-end. Once resolved, the elaborator sees a registry entry by name — the namespace and path-resolution details live in [`language.md`](../language.md) § Lexical structure and [`grammar.md`](../grammar.md).
- **Elaborator bang-call semantics.** A bang call is not an E-App registry side condition: § 7.4 resolves a user-defined elaborator declaration, uses its call type for ordinary per-call inference, evaluates the compile-time implementation, and records the returned Kio' tree. The rule sets, search disciplines, and soundness arguments of the reference coercion-elaborator libraries are library content of the POC they ship in (see [`../../docs/poc/elab.md`](../../docs/poc/elab.md) and [`../../docs/poc/optics.md`](../../docs/poc/optics.md)), not language meta-theory. This page commits only to the call-typing path and substitution contract; the open-world property of that resolution is in [`prime.md`](prime.md#7-open-world) § 7 Open-world.
- **Invalid block contents.** A resolved `sequence` descriptor rejects an empty or statement-only block; a `product` or `thunk` descriptor has its own content rules. Library implementations may additionally reject projected operands, such as a `match!` clause set that does not cover its scrutinee. These failures occur before Kio' emission; [`language.md`](../language.md) specifies their categories.
- **Codegen contracts.** The per-backend emission rules in `../backends/*.md` are outside the elaborator's contract: the elaborator's output is the Kio'-shaped tree; what each backend does with it is the backend's responsibility.

## 12. References

- **Peyton Jones, Vytiniotis, Weirich, Shields 2007** — *Practical type inference for arbitrary-rank types*. Journal of Functional Programming, 17(1):1–82. The bidirectional algorithm with shallow skolemization and explicit binders that this page adapts; Kio's finite connected-domain rule additionally admits exact whole-polytype solutions without generalization or higher-order matching.
- **Pierce 2002** — *Types and Programming Languages*, ch. 22 (Recursive Types) and ch. 23 (Universal Types). Standard reference for System F and the iso-recursive presentation of recursive types.
- **Reynolds 1974** — *Towards a theory of type structure*. The original predicative-vs-impredicative distinction.
- **Cardelli 1993** — *An implementation of FSub*. Bidirectional typechecking for System F-subtype, the algorithmic core PJ-V-W-S extends.

These pin the discipline; the page itself reads them through Kio's lens — no typeclasses, no let-generalization, positional type-args, the named-callable registry for intrinsics and top-level functions, and the separate user-defined elaborator bang-call rule.
