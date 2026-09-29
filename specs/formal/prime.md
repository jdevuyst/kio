# Kio' formal semantics

Companion to the prose page [`../prime.md`](../prime.md). This page pins down the typing rules, reduction relation, and meta-theorem statements that the prose claims, in inference-rule notation. The level of detail is "a careful PLT reader can reconstruct every step." Proof sketches are inlined; full machine-checked development is out of scope.

This page formalizes **Kio'** — the small core and the input to its standalone
checker. Surface forms are not formalized here. The full Kio front-end removes
them before codegen through:

- Early surface-language passes for forms that do not need completed typing:
  operator folding; desugaring of tuple literals, recursive-call
  syntax, literal aliases, and `.stem. { ... }` placeholder lambdas; and label elaboration.
- Full-Kio typing plus Lowered → Prime substitution for inferred
  annotations and call arguments, UFCS normalization, and imported elaborator
  bang calls, including their generic trailing-block projections. An `equiv` item survives through Lowered but substitution drops
  it, so it does not enter the Prime package used for code generation. The
  reference source-to-target coercion palettes, `if!`, `scope!`, `do!`,
  `match!`, and `derive!` are
  user-defined Kio libraries whose
  implementations execute at compile time. Their per-form semantics live with
  the executable POC they ship in (see [`../../docs/poc/elab.md`](../../docs/poc/elab.md)
  and [`../../docs/poc/optics.md`](../../docs/poc/optics.md)); the common call
  mechanism is in [`../language.md` § Elaborators are imported, not ambient](../language.md#elaborators-are-imported-not-ambient).

`kio test` is a separate consumer of the checked Lowered package. It retains
`equiv` items and first validates the ordinary substituted Prime package. From
the same checked package and completion records it also builds an independently
resolved Kio'-shaped evaluator artifact, substitutes each checked `equiv` arm
into that evaluator phase, and compares the resulting normal forms with the
partial evaluator specified in [`equiv.md`](equiv.md). This test-only route does
not make `equiv` part of the frontend-to-codegen Prime artifact.

Substitution produces a Kio'-shaped package, and the standalone Kio' checker
validates it before codegen. The term grammar below is the concrete,
post-completion core used by the typing judgments. Raw Kio' source may omit a
whole lambda value-parameter annotation: an expected function type, or the
independently synthesized arguments when the literal is a direct callee,
complete it before T-Lam. Kio' has no `_` type-placeholder spelling, and a
checked or emitted artifact contains the concrete annotation.

An elaborator declaration's `impl` or `impl(fills)` mode belongs entirely to
that surface-to-core bridge. So do the compile-time carriers `__Comptime__`,
`__Type__`, `__Checked_term__`, and, for `impl(fills)`, `__Fill_ctx__`, along
with fill transcripts, provisional checked recipes, and the `__comptime__`
helper environment. The bridge consumes them and substitutes one completed
checked term before this grammar applies. No syntax, typing premise, reduction
rule, or phase privilege is added to Kio'; the completed term must
independently satisfy the judgments below.

## 1. Syntax

### 1.1. Types

```text
Type ::=
    α                          (type variable)
  | .                         (unit type)
  | !                          (bottom type)
  | T → U                      (function type)
  | T & U                      (anonymous product)
  | T | U                      (anonymous sum)
  | ∀α. T                      (universal quantifier; the surface
                                  type-expression spelling is `[α] T`)
  | ∃α. T                      (existential quantifier; surface
                                  newtype declarations introduce it
                                  with trailing `<α>` binders)
  | N(T₁, ..., Tₙ)             (applied named type — `newtype`,
                                  `type`, or `host type` head. Each `newtype`
                                  declaration is itself an iso-
                                  recursive boundary at its name —
                                  see § 1.4)
```

Every binary type connective (`→`, `&`, `|`) is binary in Kio'; n-ary surface sugar (`A & B & C`, `A | B | C`) right-associates and has been desugared.

A **named type** `N` is one of:

- A `newtype` declaration head, parameterized over zero or more `α`s, with a payload `Payload[α₁, ..., αₙ]`. Its payload may refer to its own head only when the declaration is written `rec newtype`, or to sibling heads when it is a member of a written `rec { ... }` type group. The head carries two members: a constructor `N.c` and a projector `N.p` (see § 3).
- A `type` declaration head, parameterized over zero or more `α`s. A reference supplies exactly one argument per declared parameter; a bare or undersaturated parametric alias is ill-formed and cannot retain residual alias binders. Exact-arity references substitute their arguments and unfold the body structurally for type formation and equality. An alias introduces neither a nominal boundary nor a term-level constructor or reduction rule. When every edge of an alias chain is instead the fully saturated positional application of its next head, with the same binder kinds, the alias may qualify the terminal `newtype`'s existing constructor and projector; it does not mint members of its own. There is no singleton `rec type`; an alias may participate in a mutual group only when every cycle reaches a sibling nominal head and the alias-only subgraph is acyclic.
- A `host type` declaration head. A roleless host type may have zero or more kind-`*` parameters; a role-bearing host type is nullary. It is an opaque nominal identity: neither typing nor reduction introspects a host representation (see § 1.2).

### 1.2. Terms

```text
e ::=
    x                          (variable)
  | ()                        (unit value)
  | .t(B) | .f(B)             (Bool literals; the annotation B must
                                  be a role(bool) type in scope)
  | n(N_k)                     (integer literal at type N_k;
                                  N_k must carry role(k) for some
                                  k ∈ {i8, ..., u128, f32, f64} — an
                                  integer literal admits float roles
                                  too)
  | r(N_k)                     (float literal at type N_k; N_k
                                  must carry role(k) for some
                                  k ∈ {f32, f64})
  | s(S)                       (string literal at type S; S
                                  must carry role(str))
  | λ(x₁: T₁, ..., xₙ: Tₙ). e  (anonymous function — `fn`)
  | Λα. e                      (one-binder type abstraction — `fn`
                                  with a type-parameter binder)
  | e (T₁, ..., e₁, ...)       (call: type and value arguments
                                  passed positionally in one list,
                                  matching the binder order)
  | { stmt ; ... ; stmt ; e }  (block — zero or more statements
                                  followed by a final expression;
                                  block's value is e, block's type
                                  is e's type)
  | let x = e₁ ; e₂            (statement-form local binding; `let`
                                  has no annotation slot. `e₂` is
                                  the rest of the enclosing block.
                                  Notation here desugars to the
                                  nested-binding shape used in T-Let.)
  | e₁ ; e₂                    (statement-form expression statement;
                                  e₁ must have type ., and e₂ is
                                  the rest of the enclosing block)
  | N.c | N.p                  (constructor / projector member of
                                  a terminal `newtype`, optionally
                                  qualified through an identity alias)
  | __left__ | __right__ | __either__
  | __pair__ | __fst__ | __snd__
  | __if_then_else__ | __absurd__
                                (the eight intrinsic identifiers,
                                  brought in by the magic phrase
                                  `import __intrinsics__;`)
  | h                          (host item — opaque)
```

The grammar elides the `module` / `import` boilerplate and the public-private contour. From the typer's perspective, a Kio' module is a sequence of top-level `fn` / `type` / `newtype` / `rec newtype` / `rec { ... }` / `host type` / `host fn` declarations processed top-to-bottom under [order-sensitive visibility](../prime.md#order-sensitive-visibility). A `host type` / `host fn` is an opaque, signature-only declaration: it adds a name to scope (a `host type` as an opaque nominal type, a `host fn` as a value of its declared type — the `h` term above) but has no body to type-check. The typer checks that its signature types are well-formed, that every `host type` parameter has kind `*`, and that a role-bearing `host type` has arity zero; type parameters are admitted only on a roleless host type.

### 1.3. Top-level declarations

```text
SigGroup ::=
    [<α₁>, ...]                (type-binder group)
  | (x₁: T₁, ...)             (value-parameter group)

TopDecl ::=
    pub? pure? fn x SigGroup+ (-> R)? { e }
  | pub? type N(<α₁>, ...) = T;
  | pub? newtype N(<α₁>, ...) : Payload[α] { pub constructor? c, pub projector? p, };
  | pub? rec newtype N(<α₁>, ...) : Payload[α] { pub constructor? c, pub projector? p, };
  | rec { RecTypeDecl RecTypeDecl+ }

RecTypeDecl ::=
    pub? type N(<α₁>, ...) = T;
  | pub? newtype N(<α₁>, ...) : Payload[α] { pub constructor? c, pub projector? p, };
```

Elaborating the ordered signature groups is a right fold: each type binder
contributes one nested `Λ`, and each value-parameter group contributes one
product-domain `λ`. Hence `[A][B](x: P)` denotes `ΛA. ΛB. λ(x: P). e`,
while `[A](x: P)[B](y: Q)` denotes
`ΛA. λ(x: P). ΛB. λ(y: Q). e`. Type binders are neither aggregated nor
hoisted across a value group, and value groups are not flattened across an
intervening type binder.

The `(-> R)?` annotation on `fn` is the one optional type position; an absent annotation defaults to `R = .`. Every other position is mandatory or grammatically disallowed.

The presentation puts `pub` before `pure`; concrete syntax accepts either order and formats as `pub pure fn`. `pure` is admitted only on the ordinary `fn` alternative. It is not a modifier on either type alternative, a host declaration, or a surface declaration that later elaborates to Kio'.

`fn` has no recursion: by order-sensitive visibility, the name `x` is not in scope inside its own body, and forward references to later declarations are equally out of scope. Recursive type references exist only in the two written recursive-data scopes below.

### 1.4. Explicit recursive scopes and the newtype boundary

A `rec newtype N` declaration checks its payload under the incoming context extended by exactly `N`'s head. A `rec { D₁ ... Dₙ }` group first extends the incoming context with every member head, then checks every member body in that shared context. When the scope closes, its heads extend the ordinary source-ordered context for following declarations. An unmarked `newtype` or `type` declaration is checked before its own head extends the context and sees no later head.

Let `G(D̄)` be the directed graph whose vertices are the heads written in a type group and whose edges are exact type references between those heads, after transparent-alias paths are followed. A group is well formed only when it has at least two members and `G(D̄)` is one cyclic strongly connected component. The alias-induced subgraph is acyclic, so every cycle crosses a `newtype` head. A one-member, acyclic, multi-component, helper-containing, or alias-only group is rejected. For `rec newtype N`, the payload must contain a path back to `N`; otherwise the marker is redundant.

Each `newtype` declaration is an **iso-recursive boundary** — from the typing rules' perspective, `N(α)` is **not** equal to its one-step unfold `Payload[α]`. Crossing the boundary in values uses the newtype's declared constructor and projector explicitly. Transparent group-member aliases only provide finite paths to those nominal knots.

**Strict positivity.** Every recursive use site inside a written recursive component — including one reached through a transparent group-member alias — must sit in a strictly-positive position, never under the LHS of a `→` even transitively through nested newtype applications. The check ranges over the complete written component and composes through chains by mutual fixpoint variance inference (formalized in § 2.5).

## 2. Typing

### 2.1. Judgments

```text
Γ ⊢ e : T              term e has type T under context Γ
Γ ⊢ T type             T is a well-formed type under Γ
⊢ M ok                 module M is well-formed
```

Contexts `Γ` carry both type and value bindings:

```text
Γ ::= · | Γ, α | Γ, x : T | Γ, N(α₁, ..., αₙ) := …
```

Module checking additionally carries a finite declaration map `Π` from each resolved top-level value identity in scope to one of `pure-fn`, `ordinary-fn`, `host-fn`, `intrinsic`, or `newtype-member`. Local term binders are deliberately absent from `Π`: the function-purity rule constrains declaration references, not values received through parameters or `let` bindings.

The newtype-binding form `N(α₁, ..., αₙ) := { payload: Payload, constructor: c, projector: p }` records the `newtype`'s shape so the constructor / projector typing rules can reach it. An ordinary declaration adds it after its payload is checked; a recursive singleton or group predeclares the applicable head set as described in § 1.4 and completes each binding after checking the bodies.

### 2.2. Term typing rules

Variables, units, and literals:

```text
─────────────────  T-Var
Γ, x : T ⊢ x : T

──────────────  T-Unit
Γ ⊢ () : .

(B is a role(bool) type in Γ)
───────────────────────────────────  T-True
Γ ⊢ .t(B) : B

(B is a role(bool) type in Γ)
───────────────────────────────────  T-False
Γ ⊢ .f(B) : B

(N_k is a role(k) type in Γ; k ∈ {i8, ..., u128, f32, f64})
─────────────────────────────────────────────────────────────────  T-Int
Γ ⊢ n(N_k) : N_k

(N_k is a role(k) type in Γ; k ∈ {f32, f64})
───────────────────────────────────────────────────  T-Float
Γ ⊢ r(N_k) : N_k

(S is a role(str) type in Γ)
──────────────────────────────────  T-Str
Γ ⊢ s(S) : S
```

Each literal carries its host-type annotation as a syntactic slot of the term — Kio' admits no unannotated-literal form (see [`../grammar.md` § Kio' grammar](../grammar.md#kio-grammar)). The single premise on each rule is that the annotation matches the literal's lexical shape via the role-admission relation in [`../language.md` § Literals](../language.md#literals). Type-checking is local: there is no context-driven resolution, no candidate pool, no unification. The surface language's tier-2 and tier-3 resolution rules (see [`../language.md` § Literals](../language.md#literals)) elaborate every unannotated literal to one of the annotated forms above before the program reaches Kio'; that elaboration is part of the surface→Kio' bridge described in [`elaboration.md` § 7.1](elaboration.md#71-unit-literals-and-absurd), not part of the Kio' core.

`let` and functions:

```text
Γ ⊢ e₁ : T₁    Γ, x : T₁ ⊢ e₂ : T₂
────────────────────────────────────  T-Let
Γ ⊢ let x = e₁ ; e₂ : T₂

Γ ⊢ e₁ : .    Γ ⊢ e₂ : T₂
──────────────────────────  T-Seq
Γ ⊢ e₁ ; e₂ : T₂

Γ ⊢ T₁ type    ...    Γ ⊢ Tₙ type
Γ, x₁ : T₁, ..., xₙ : Tₙ ⊢ e : R
─────────────────────────────────────────────────  T-Lam
Γ ⊢ λ(x₁: T₁, ..., xₙ: Tₙ). e : (T₁, ..., Tₙ) → R

Γ, α ⊢ e : T
──────────────────  T-TLam
Γ ⊢ Λα. e : ∀α. T
```

Application threads type arguments and value arguments through one positional list. Presented as right-leaning curried instantiation:

```text
Γ ⊢ e : ∀α. T    Γ ⊢ U type
─────────────────────────────  T-TApp
Γ ⊢ e[U] : T[α := U]

Γ ⊢ e : (T₁, ..., Tₙ) → R    Γ ⊢ aᵢ : Tᵢ  (1 ≤ i ≤ n)
───────────────────────────────────────────────────────  T-App
Γ ⊢ e(a₁, ..., aₙ) : R
```

T-TApp is implicitly invoked once per leading `<α>` slot in the joint
argument list — surface `e(U₁, ..., Uₘ, a₁, ..., aₙ)` corresponds to
`e[U₁]...[Uₘ](a₁, ..., aₙ)`. The nesting is semantically
significant: each T-TApp eliminates one `Forall` and leaves the next
application boundary in place. A backend may erase `U` as data, but it cannot
collapse that boundary past a later type or value application.

T-TApp never implies T-App. A Kio' argument list containing only type
arguments stops after the corresponding type applications, even if the
resulting function domain is Unit after substitution or alias unfolding. For
`nil : ∀α. Unit → List(α)`, `nil[A]` therefore has type
`Unit → List(A)`; saturation is the explicitly nested `nil[A](())`, rendered
as `nil(A, ())` or `nil(A)()` in source. The written term and its explicit type
are the whole authority for this distinction. Declaration grouping,
provenance, and private ABI metadata cannot add an application, and structural
type equality cannot move T-App before its preceding T-TApp.

T-Lam requires concrete parameter types at the point where it checks the
lambda body. Ordinary bidirectional checking may recover an absent annotation
from an expected function type at the lambda's position, in both Kio and Kio'.
One application-local route is distinct: when a lambda literal is the direct
callee, an absent value-parameter annotation may take the independently
synthesized type of its corresponding argument at that same node. The
application follows Kio''s ordinary slot and product-packing rules, completes
the lambda, and then applies T-Lam and T-App to that concrete term. A Kio' flat
call still cannot span multiple function layers.

Neither route inspects the lambda body to infer a parameter, crosses a `let` or
another application node, or infers an omitted universal call argument.
Surface `_` syntax does not belong to Kio'; a Surface `_` resolved by either
route is emitted as its concrete annotation. The complete elaboration rule is
in [`elaboration.md` § Function application](elaboration.md#5-function-application).

### 2.3. `newtype` constructor and projector

Given a `newtype` declaration:

```text
newtype N(<α₁>, ..., <αₘ>) : Payload[α] { constructor c; projector p; };
```

the constructor `N.c` and projector `N.p` are typed by:

```text
(N(α₁, ..., αₘ) := { payload: Payload, constructor: c, projector: p } in Γ)
─────────────────────────────────────────────────────────────────────────  T-NewtypeC
Γ ⊢ N.c : ∀α₁ ... αₘ. (Payload[α]) → N(α₁, ..., αₘ)

(N(α₁, ..., αₘ) := { payload: Payload, constructor: c, projector: p } in Γ)
─────────────────────────────────────────────────────────────────────────  T-NewtypeP
Γ ⊢ N.p : ∀α₁ ... αₘ. (N(α₁, ..., αₘ)) → Payload[α]
```

The `Payload[α]` may mention only type heads in its lexical context: earlier declarations and imports, plus the exact head set of an enclosing explicit recursive singleton or group. Each newtype's nominal name is itself an iso-recursive boundary, so `N(α)` ↔ `Payload[α]` is **not** equated by the typing rules. To cross the boundary in values, apply the constructor / projector explicitly (T-App on `N.c` or `N.p`). The reduction relation (§ 3) makes wrap-then-unwrap reduce, formalizing the iso-recursive law at the value level.

Write `A ⇝id N` when an in-scope alias `A` reaches `newtype N` through a
finite chain in which every declaration has the same binder kinds and every
body is the fully saturated positional application of the next head:
`A[α₁]…[αₘ] = B(α₁, …, αₘ)`. Cycles, missing targets, partial applications,
reordered arguments, and structural or otherwise transformed bodies do not
establish this relation. If `A ⇝id N`, `A.c` and `A.p` are alternate qualified
paths to the exact terms `N.c` and `N.p`, with the schemes above. The alias
head must be visible at the written source position and the terminal member
must independently be visible from that use site. The head occurrence retains
`A`'s declaration identity while the member occurrence retains the terminal
member's identity. No constructor, projector, nominal boundary, or reduction
rule belongs to `A`; reduction remains the terminal newtype's rule.

### 2.4. Intrinsic typing

The intrinsics are typed by their schemes (the same schemes that appear in [`../prime.md`](../prime.md) § Value construction and elimination):

```text
__left__       : ∀α. ∀β. α → (α | β)
__right__      : ∀α. ∀β. β → (α | β)
__either__        : ∀α. ∀β. ∀γ. (α | β, α → γ, β → γ) → γ
__pair__       : ∀α. ∀β. (α, β) → α & β
__fst__           : ∀α. ∀β. (α & β) → α
__snd__           : ∀α. ∀β. (α & β) → β
__if_then_else__  : ∀α. (B, . → α, . → α) → α   (Γ_roles(bool) = {B})
__absurd__        : ∀α. ! → α
```

`__if_then_else__` requires `Γ_roles(bool) = {B}`. The candidate set is the nonempty set of Boolean-role host identities in unqualified lexical scope, or otherwise the fallback set of terminal identities reached through transparent aliases in that scope. A qualified module import alone contributes no member, though an in-scope alias may reach its terminal identity through one. Identities are `(declaring module, host-type name)`: repeated paths to one declaration collapse to one member, while declarations from different modules remain distinct even when their leaves match. With zero or multiple candidates, the intrinsic has no scheme and referencing it is a type error. This is deliberately stricter than T-True / T-False: those literal terms carry `B` explicitly and remain well-typed at either of two same-role types. The literal forms `.t(B)` / `.f(B)` and the `__if_then_else__` intrinsic are the two places the language has built-in awareness of the `role(bool)` boundary.

`__absurd__` is well-typed at every `α`; it is sound because the bottom type `!` has no introduction form (see § 4 — Type safety), so its precondition can never be discharged at runtime.

All eight intrinsics are ordinary System F polymorphic constants — their schemes name uniform `∀α. …` binders that range over arbitrary types, and the typing rule for a call site is the standard `apply_scheme` with positional substitution.

**Existentials live inside `newtype`.** Kio' has no separate existential introduction or elimination intrinsic. The `newtype`-declared existential binder run (see § 1.4) is the only existential-introducing construct; the newtype's CPS projector

```text
N.p : ∀α₁ … αₘ. (N(α₁, …, αₘ)) → ∀ρ. (∀β₁ … βₙ. (Payload[α, β]) → ρ) → ρ
```

is the only elimination form. The continuation's `∀β₁ … βₙ.` binders are System F universals scoping the existential witnesses — the body type cannot leak into `ρ` by ordinary scope rules, so no separate skolemization machinery is needed.

### 2.5. Strict positivity

Strict positivity is stated as a side condition on `newtype` declarations. Let `αᵢ` be the i-th type-parameter of a parametric `newtype` `N([α₁], ..., [αₘ])`. The **variance** of `αᵢ` in `N`'s payload is computed by mutual fixpoint over the finite set of nominal declarations reachable through written type references:

```text
Variance v ::=  0       (parameter is unused / phantom)
             |  +       (covariant)
             |  −       (contravariant)
             |  *       (invariant)

Composition (descending into nested positions, sign-multiplication-style):
  + ∘ v     = v
  − ∘ +     = −
  − ∘ −     = +
  * ∘ v     = *
  0 ∘ v     = 0

Join (combining variances when a parameter appears in multiple positions):
  0 ∨ v     = v
  v ∨ v     = v
  + ∨ −     = *
  * ∨ v     = *
```

The **variance environment** `V` maps each reachable parametric `newtype` plus the slot index to a variance. `V` is computed by the monotone fixpoint:

1. Initialize every parameter to `0`.
2. For each parametric `newtype` `N(<α₁>, ..., <αₘ>) : Payload[α]`, walk `Payload[α]` computing the join over all occurrences of each `αᵢ`, composing with the variance of each enclosing position:
   - sum / product preserve (composition with `+`),
   - arrow LHS flips (composition with `−`) on the parameter side,
   - arrow RHS preserves (composition with `+`),
   - entering a nested newtype application `M(...)` composes via `V(M, j)` for the j-th argument slot.
3. Repeat until `V` reaches a fixed point. (Monotone over the four-element lattice; finitely many parameters; terminates.)

**Roleless host types** have unknown variance: every parameter slot of a parameterized `host type` is conservatively `*` (invariant). Any self-reference under a `host type` type-argument therefore fails the strict-positivity check. Role-bearing host types have no parameter slots.

Strict positivity is a property of one **explicit recursive component**, not of an unrelated declaration in the module. The singleton component for `rec newtype N` contains `N`. A bare group already denotes exactly one SCC under the written-head reference graph from § 1.4; transparent aliases contribute finite paths but no alias-only cycle. The strict-positivity side condition on a `newtype` declaration `N` in that component is then:

> For every occurrence in `N`'s payload that reaches a nominal member `M` of the explicit component (including through a transparent member alias), the **composed variance** of every enclosing position (working outward from the occurrence to the root of the payload) is `+` or `0`.

Checking only `N`'s direct self-occurrences and treating a sibling nominal or alias as opaque is unsound: a mutual cycle (`N₁ : … (N₂ → …); N₂ : … (N₁ → …)`) or a wrapper-indirect cycle (`P : … (Q → …); Q : P`) unfolds to a self-reference under an arrow LHS, which is enough to construct `Ω` and break strong normalization. Ranging the check over the whole explicit component closes that gap. A negative occurrence of a newtype outside the component (a one-directional reference to a previously declared nominal) stays admissible — that nominal is a fixed inductive type, not part of this recursion.

A declaration whose body fails this check is rejected at declaration time.

### 2.6. Function purity

Function purity is a second, declaration-reference judgment over an already well-typed body:

```text
Γ ; Π ⊢ e pure            every executable declaration reference in e is
                           admitted by the function-purity contract
```

Let `ExecRefs(e)` be the resolved top-level value identities appearing in term positions anywhere in `e`, including callee paths, values passed or returned without being called, and bodies of nested lambdas. Type syntax is not a term position: function signatures, literal annotations, local binder annotations, reflected type arguments, and every other `Type` subtree contribute nothing to `ExecRefs`.

```text
Γ ⊢ e : R
∀d ∈ ExecRefs(e).
  Π(d) ∈ {pure-fn, intrinsic, newtype-member}
────────────────────────────────────────────────  T-PureBody
Γ ; Π ⊢ e pure
```

References resolved to local parameters or `let` binders are admitted independently of `Π`. A `host-fn` or `ordinary-fn` entry is not admitted. Because every referenced `pure-fn` declaration is itself checked by T-PureFn below, the property is transitive without inspecting or inferring another declaration's body at the use site.

```text
Γ, ᾱ ⊢ T₁ type  ...  Γ, ᾱ ⊢ R type
Γ, ᾱ, x₁ : T₁, ..., xₙ : Tₙ ⊢ e : R
Γ, ᾱ, x₁ : T₁, ..., xₙ : Tₙ ; Π ⊢ e pure
─────────────────────────────────────────────────────  T-PureFn
Γ ; Π ⊢ pure fn x[ᾱ](x₁ : T₁, ..., xₙ : Tₙ) -> R { e } ok
```

An ordinary unmarked function has the same type premises without T-PureBody. Notice that T-PureFn checks every signature and annotation with the ordinary `Γ ⊢ T type` judgment: a host type, alias, or newtype is admissible there. Only executable body references are restricted.

The `pure` marker is retained in Kio' source and package contracts. A fresh parse therefore reconstructs `Π`, and direct compilation, dump-and-reparse, and validated cache reload validate the same judgment without producer provenance. The runtime loader recognizes the grammar but trusts the precompiled function bodies and does not reconstruct or check `Π`; successful loading is not a derivation of T-PureBody.

### 2.7. Module well-formedness

```text
M = TopDecl₁; TopDecl₂; ...; TopDeclₖ

Introductions(M, x) = [ each explicit ordinary import occurrence or
                       ordinary post-lowering local declaration in M
                       whose visible name is x ]
                     ++ [ each distinct fixed __intrinsics__ entry named x ]
For every x: length(Introductions(M, x)) ≤ 1

For each i: ⊢ TopDeclᵢ ok in (Γᵢ₋₁, Πᵢ₋₁),
            Γᵢ = Γᵢ₋₁ ∪ binding(TopDeclᵢ),
            Πᵢ = Πᵢ₋₁ ∪ declaration-class(TopDeclᵢ)

(Γ₀ and Π₀ come from the module's `import` imports — including any imported `host type` /
`host fn`. Ordinary imports contribute only declarations visible from the importing
module. A module's own `host` items are ordinary `TopDecl`s in the sequence above;
see ../language.md § Module system.)

──────────────────────  T-Module
⊢ M ok
```

Order-sensitive visibility falls out of the `Γᵢ₋₁` premise: each declaration is checked under a context that contains *only* prior declarations.

For a function declaration, `declaration-class` records `pure-fn` exactly when the declaration carries `pure`, and `ordinary-fn` otherwise. It records the corresponding fixed classes for host functions, intrinsics, and newtype members. Type declarations extend `Γ` but not `Π`; types have no purity class.

`Introductions` counts written occurrences, not distinct terminal identities. Repeated selective imports within or across clauses, repeated qualified aliases, and an import sharing a name with a local alias violate the premise even when they resolve to the same declaration. An ordinary alias's nominal identity and member namespace do not exempt its introduction. The fixed `__intrinsics__` block set is idempotent. Surface-only declaration registries and `__comptime__` imports have already been consumed at this Kio' boundary and are not members of `Introductions`. The premise consumes imports whose validity and cycles have been checked; this dependency does not establish diagnostic precedence when independent errors coexist.

## 3. Reduction

### 3.1. Reduction relation

The reduction relation `e → e'` is small-step. Its compatible closure is
used for pure normalization; Kio' is confluent and strongly normalizing (see
§ 4 / § 5), so all sensible normalization strategies reach the same normal
form. Observable execution uses the call-by-value order specified in
[`../prime.md` § Kio' semantics](../prime.md#kio-semantics): both `λ` and `Λ`
are values, and execution does not enter either body until the matching
application. We give the redexes below in the standard context + redex
presentation.

Redexes:

```text
(λ(x₁: T₁, ..., xₙ: Tₙ). e)(a₁, ..., aₙ)  →  e[xᵢ := aᵢ]                     (β-Lam)

(Λα. e)[U]                                →  e[α := U]                       (β-TLam)

let x = a ; e                             →  e[x := a]                       (β-Let)

() ; e                                    →  e                                (β-SeqUnit)

__either__(A, B, C, __left__(A, B, a), fl, fr)   →  fl(a)                 (ι-LeftEither)
__either__(A, B, C, __right__(A, B, b), fl, fr)  →  fr(b)                 (ι-RightEither)

__fst__(A, B, __pair__(A, B, a, b))    →  a                               (ι-Fst)
__snd__(A, B, __pair__(A, B, a, b))    →  b                               (ι-Snd)

N.p(T̄, N.c(T̄, v))                          →  v                               (ι-Newtype)
                                              (where N is a `newtype` and
                                              T̄ matches both sides)

__if_then_else__(A, .t(B), t, e)       →  t(())                              (ι-IfTrue)
__if_then_else__(A, .f(B), t, e)       →  e(())                              (ι-IfFalse)
```

`__absurd__` has no reduction rule — `!` is uninhabited, so no reducible application of `__absurd__` can arise in a closed well-typed term (by the type-safety theorem in § 4).

**Stuck redexes.** A reduction sequence may end at a normal form that contains:

- A host call applied to fully-evaluated arguments (host calls are opaque — see § 3.3).
- An expression statement whose left side is stuck on a host call. The statement waits on that opaque action and is itself a residual sequence; it does not reduce to the rest of the block.
- An `__if_then_else__` whose condition is a stuck host call (the condition is not a literal `.t(B)` / `.f(B)`, so neither ι rule applies).
- An `__absurd__` whose argument is a stuck host call (a host function declared `-> !`, applied but not reduced).

These are normal forms by definition; they are precisely the residuals the partial evaluator and equivalence relation in [`equiv.md`](equiv.md) compare structurally.

`β-TLam` consumes exactly one abstraction. Thus
`(Λα. Λβ. e)[U][V]` takes two ordered steps; evaluation exposed by the
first step completes before the second application proceeds. In the
call-by-value operational strategy, a type application first evaluates its
callee to a `Λ`, performs that one substitution, and then evaluates the
exposed term. If a later value application follows, this type-application
stage completes before the later value arguments are evaluated. The type
argument has no runtime representation, but the stage remains observable when
the exposed term reaches a host call.

**Reduction contexts** for pure normalization are the standard congruence
contexts: every subterm position is admissible. This full compatible closure
is what permits normalization under abstractions. It does not override the
call-by-value operational order for observable execution described above. The
choice of pure normalization strategy (CBV / normal-order / hybrid) does not
change the set of programs that converge — see § 5.

### 3.2. Substitution

Substitution `e[x := a]` is capture-avoiding term substitution; `T[α := U]` is capture-avoiding type substitution; `e[α := U]` lifts type substitution into the term. All three are standard.

### 3.3. Host calls are opaque

Host items `h : T` from `host fn` declarations participate in typing (T-App, T-TApp) but never reduce. A reduction sequence that hits a redex whose head reduces to a host call gets stuck on that host call as a leaf; the partial evaluator in [`equiv.md`](equiv.md) compares such stuck residuals structurally (same head, same recursively-equivalent arguments).

This is by design: the host boundary is opaque ([`../language.md`](../language.md#the-host-boundary)), so the language commits to nothing about what host calls compute. The opacity is what lets a host implement, e.g., a `loop` primitive without compromising Kio's SN guarantee on the language-level core (the host is free to diverge; the package can't observe that within the language-level reduction relation).

## 4. Type safety

**Theorem (Type safety).** If `· ⊢ e : T` (closed, well-typed) then either:

1. `e` is a value (an introduction form, a closure, or a `Λ`-form); or
2. `e` reduces (`e → e'` for some `e'`); or
3. `e` is stuck on a host call, possibly as the left side of an expression statement waiting on that host call (see § 3.3).

Furthermore, if `· ⊢ e : T` and `e → e'`, then `· ⊢ e' : T` (preservation).

**Proof sketch.** Standard progress + preservation by induction on the typing derivation. The two non-standard pieces:

- **Iso-recursive `newtype` boundary.** A value of type `N(T̄)` for a `newtype` `N` can only have arisen from `N.c(T̄, v)` for some payload `v`; ι-Newtype is the only redex involving `newtype`-typed values, and it preserves the type (by T-NewtypeC / T-NewtypeP composing trivially).
- **Bottom elimination.** `__absurd__` is sound because the only way to type-check a value of type `!` is via the host boundary (a `host fn` returning `!`); host calls are opaque and either get stuck (case 3) or are non-terminating in the host (the reduction relation never fires under a host call). Either way, a redex `__absurd__(A, v)` for an actual value `v` of type `!` cannot arise — the typing rules permit no closed value at type `!`.

The full proof (per the standard Wright-Felleisen technique) is straightforward; the two clauses above are the only places Kio' departs from textbook System F.

## 5. Strong normalization

**Theorem (Strong normalization).** If `· ⊢ e : T` (closed, well-typed, in a module satisfying the strict-positivity check of § 2.5), then every reduction sequence starting from `e` is finite.

**Proof sketch.** Standard reducibility-candidates argument adapted to the System F + iso-recursive presentation:

1. Define a reducibility predicate `R_T` for each type `T`, by induction on the type:
   - `R_α` for type variables uses a placeholder fixed by the substitution interpretation.
   - `R_{T → U}` is `{e | for all v ∈ R_T, e(v) ∈ R_U}`.
   - `R_{T & U}` is `{e | __fst__(e) ∈ R_T and __snd__(e) ∈ R_U}` (or equivalently, the introduction-form's components are in the respective sets).
   - `R_{T | U}` is reducibility for either-arm: `{e | e reduces to __left__(v) with v ∈ R_T, or to __right__(w) with w ∈ R_U}`.
   - `R_{∀α. T}` is `{e | for all U, e[U] ∈ R_{T[α := U]}}`.
   - `R_{N(T̄)}` for a `newtype` `N` is given by the **least fixed point** of the operator built from `N`'s payload; for a mutually-recursive SCC `{N₁, …, Nₖ}` it is the **simultaneous** least fixed point of the tuple of payload operators `(X₁, …, Xₖ) ↦ (R_{Payload₁}, …, R_{Payloadₖ})` (well-defined because no SCC member appears at a non-strictly-positive position in any member's payload, so the operator is monotone in every component; the product lattice is complete by standard argument — see Mendler 1991, "Inductive Types and Type Constraints in the Second-Order Lambda Calculus").
   - `R_!` is empty (no value of bottom type — the strong-normalization argument's invariant).
   - `R_{N(T̄)}` for a `host type` `N` is `{e | e is a value or stuck on a host call}` (host items are opaque atoms; their reducts are not under the language's control, but the language-level reduction never fires inside a host call, so they are SN-trivial from the language's perspective).
2. Show that every term reducible at its type is strongly normalizing (by induction on the type for the "soundness of reducibility" property).
3. Show that every well-typed term is reducible at its type (by induction on the typing derivation, using the substitution interpretation).
4. Conclude: every closed well-typed term is reducible at its type, hence strongly normalizing.

The strict-positivity precondition appears in step 1 (the newtype clause) — without it, the operator (per-newtype, or simultaneous over the SCC for a mutually-recursive group) wouldn't be monotone and the fixed point wouldn't exist.

The host-type clause makes this an **SN-relative-to-host** result: the language-level reduction strongly normalizes given that host calls don't reduce. A host that internally diverges is outside the scope of this theorem, by design (the opacity is what makes the proof go through).

This is a routine adaptation of standard literature; Mendler 1991 covers iso-recursive types in System F, and Girard's reducibility candidates handle the rest.

## 6. Decidability of typechecking

**Theorem (Decidability).** Given a closed expression `e` and a type `T`, deciding whether `· ⊢ e : T` is decidable. Given a closed expression `e`, computing a `T` such that `· ⊢ e : T` (or rejecting `e`) is decidable.

**Proof sketch.** Typechecking is a finite, syntax-directed bidirectional
recursion:

- Variables and units synthesize by direct lookup. A literal's written
  annotation is its type, subject only to the finite role-admission check.
- A `let` synthesizes its RHS, extends `Γ`, and recurses on its body. A
  top-level function body is checked against its declared result.
- A lambda synthesizes after its parameter annotations are concrete. In a
  checking position, an expected function type may supply whole absent
  parameter annotations. When the lambda literal is the direct callee of one
  application, the independently synthesized arguments at that node may
  supply those whole annotations before T-Lam and T-App are applied. Neither
  route inspects the body to discover a parameter type.
- `Λα. e` extends `Γ` with the written binder and recurses. Every universal
  application argument is written explicitly, so T-TApp performs a finite
  structural substitution. Each value application consumes one function
  layer and checks its finite argument packet against that layer.
- A newtype constructor may infer only its finite existential-witness suffix
  from the immediate value argument. Its universal prefix remains explicit.
  Constructor/projector and intrinsic schemes otherwise come from finite
  lookup.
- A `pure fn` performs the ordinary body check, then walks its finite resolved
  term tree once and checks each `ExecRefs` entry against the finite `Π` map
  (§ 2.6). Type subtrees are skipped.

Standalone Kio' completion is confined to one syntax node at a time. At a
direct lambda-callee node, a fixed finite set of omitted parameter annotations
may be filled from that node's written parameters and independently synthesized
immediate value arguments; a constructor may similarly infer its local finite
existential-witness suffix. The checker allocates no full-front-end goal store
and shares no equation across nodes. A retained source continuation is confined
to its current node and consumed before that node completes. Each node-local
unification fills one of its fixed slots or rejects.

**Type equality** is purely structural — recursive descent on the structure, with `newtype` / `host type` identity by declaration site (compare names plus the type arguments). This is decidable in linear time in the size of the compared expansions: type expressions are finite trees, the transparent-alias subgraph of every recursive group is acyclic, and every cyclic path reaches a nominal `newtype` boundary whose identity stops unfolding. There is no anonymous recursive type binder, and crossing a nominal boundary is an explicit operator at the value level.

The strict-positivity check (§ 2.5) is decidable because the variance lattice is finite (4 elements), the parameter set per module is finite, and the fixpoint iteration converges in at most O(parameters × lattice height) steps.

The function-purity check (§ 2.6) is decidable because `ExecRefs` is a finite structural walk over the body and each declaration-class lookup in `Π` is finite. It performs no body inference or candidate search.

Therefore standalone Kio' synthesis and checking terminate on every
well-formed module. Each recursive premise visits a finite subterm or type, and
each bounded local completion step fills one of the current node's fixed slots,
so the algorithm decides Kio' typechecking. Full-Kio elaboration has its own
finite connected-domain argument in [`elaboration.md`](elaboration.md) § 10;
this theorem does not collapse that earlier phase into the Kio' checker.

## 7. Open-world

**Theorem (Open-world preservation).** Adding a new top-level declaration to a *module body* never breaks a previously-typechecking dependent module. Equivalently: if `M` extends `M_0` by adding declarations after every existing one, and `M_0`-using-`P` was well-typed, then `M`-using-`P` remains well-typed without modification.

**Scope.** `M` and `M_0` here are `*.kio` module bodies. The theorem says nothing about a package's *contract surface* — the transitive closure of `host` items, exports, and the types they reach, as selected by the package file's `bridge { … }` glob block. Adding or removing a `host` item, or changing a type a bridged signature reaches, is a versioning event, not an open-world violation; see [`../package.md` § The bridge block](../package.md#the-bridge-block). (Adding an ordinary `pub fn` / `type` to a bridged module is covariant — it grows the export surface additively — and stays within the theorem.)

**Proof sketch.** Type-checking each declaration in `P` is local to its own context, which sees only the names brought in by `import`. Adding declarations to `M` either adds new exports `P` could `import` (which is an `import`-line edit, not an `M`-edit — `P`'s `import` graph is unchanged) or adds non-exported items (irrelevant to `P`). Within `M`, the candidate heads of a recursive singleton or group are exactly the declarations written inside that lexical boundary; adding another declaration outside it cannot add an edge, change its SCC, or retarget a reference. Function-purity validation reads only the already-resolved identities in `ExecRefs(e)` and their written declaration classes in `Π`; adding an unrelated declaration changes neither set.

Application is likewise structural. Whether a term contains only T-TApp or
continues with T-App follows from the written term and its explicit,
artifact-visible types. Alias unfolding may check that a written value has the
function's domain, but declaration grouping, callee provenance, and private ABI
metadata never add an application. Extending a module body changes none of
those inputs for an already-resolved call.

There is no candidate-pool / implicit-dispatch mechanism that could change a bridge type by inserting a different choice. Kio has no typeclasses; every elaborator call — source-to-target coercion, `match!`, or `derive!` — resolves through an **explicit import** and runs only at a written bang-call site. Adding declarations to some other module body cannot alter the scoped elaborator binding an existing call has already imported. An elaborator's rule set, structural pre-conditions, and (for `derive!`) candidate set are purely syntactic over the source / target types `P` wrote down and the candidates the call site literally lists — there is no implicit candidate pool to grow, so adding a new declaration cannot change the source / target types or candidates `P` already wrote, hence cannot change the elaboration `P` gets. (This is the open-world property of elaborator-call resolution; the per-library proofs that each coercion's rule subset is genuinely syntactic are in the POC case studies, [`../../docs/poc/elab.md`](../../docs/poc/elab.md) and [`../../docs/poc/optics.md`](../../docs/poc/optics.md).)

For a fills-marked implementation, the schedule is fixed on that already
resolved elaborator declaration and its staged operands come only from the
already-fixed public call plan. The one transparent direct-call shell is
recognized from compiler-owned resolved-intrinsic kind plus the unique exact
canonical product-constructor scheme; an ordinary declaration with the same
name or an equivalent product-shaped type remains opaque. Adding an unrelated
module declaration changes none of those inputs and cannot alter the produced
Kio' term.

Therefore `P`-against-`M` typechecks identically before and after. □

This is the "open-world by structural construction" claim from the prose pages, made formal. The proof is genuinely short because Kio takes deliberate care to avoid every mechanism that would make the property fragile.

## 8. References

- **Girard 1972** — *Interprétation fonctionnelle et élimination des coupures*. The reducibility-candidates technique used in § 5.
- **Mendler 1991** — *Inductive Types and Type Constraints in the Second-Order Lambda Calculus*. The iso-recursive presentation of recursive types and the SN proof for that fragment.
- **Wright & Felleisen 1994** — *A Syntactic Approach to Type Soundness*. The progress + preservation framing used in § 4.
- **Pierce 2002** — *Types and Programming Languages*. Standard reference for System F, recursive types, and structural type equality.

These are the four pillars; the formal account in this page assembles them with the iso-recursive boundary as the only non-textbook composition.
