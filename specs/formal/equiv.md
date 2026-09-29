# Formal semantics of `equiv` discharge

Companion to the prose pages [`../language.md`](../language.md) § Equivalence claims and [`../cli.md`](../cli.md) § kio test. This page pins down the reduction strategy and equivalence relation that `kio test` uses to discharge `equiv` items, as inference rules and meta-statements.

The reduction relation is borrowed from [`prime.md`](prime.md) § 3 — the small-step rules apply uniformly. This page restricts attention to `equiv`-discharge specifics: the residual normal form, host-call atomicity, the literal-token recognition rule for `__if_then_else__`, and the equivalence relation on residual NFs.

## 1. What `kio test` discharges

An `equiv` declaration carries `N ≥ 2` arms:

```text
equiv name[A](x: A) {
  e₁;
  e₂;
  ...
  e_N
}
```

The runner, conceptually:

1. Parses, type-checks, and substitutes the module to `Lowered` and finally to `Prime`-shaped terms. Every block and blockless elaborator call, including the library's `if!`, `scope!`, `do!`, `match!`, and `derive!`, has its ordinary Kio' expansion substituted at the Lowered → Prime boundary.
2. For each `equiv` item: binds each value parameter `pᵢ : Tᵢ` to a **fresh opaque atom** `aᵢ` shared across every arm, ignores the type parameters (they're erased), and partial-evaluates each arm body to a residual normal form `vᵢ`.
3. Compares the `N` residual NFs pairwise. If every pair is equivalent under the relation in § 4, the `equiv` passes; otherwise it fails with the partition into NF-equivalence groups (see [`../cli.md`](../cli.md) § kio test for the diagnostic shape).

Termination: every body is a closed well-typed Kio' term (parameters substituted with opaque atoms, which the partial evaluator treats as values that don't reduce). Strong normalization (per [`prime.md`](prime.md) § 5) holds for the substituted bodies, so the partial evaluator terminates.

## 2. Reduction rules

The partial evaluator applies the small-step rules from [`prime.md`](prime.md) § 3 (β / ι), the `__if_then_else__` literal-recognition rule (§ 2.2), and the symbolic case-split over an opaque sum (§ 2.6). **No other rules fire**; in particular, host calls are opaque (§ 2.4) — the evaluator commits to nothing about what a host item computes.

### 2.1. Inherited rules

```text
β-Lam, β-TLam, β-Let           (function / type / let reduction)
β-SeqUnit                      (`(); rest` expression-statement collapse)
ι-LeftEither, ι-RightEither    (sum eliminator with known injection)
ι-Fst, ι-Snd                   (product projection with known pair)
ι-Newtype                      (newtype unwrap-of-wrap)
```

(See [`prime.md`](prime.md) § 3 for full rule statements.)

β-Lam consumes one λ-layer per application — curry layers are distinct ([`../language.md`](../language.md#type-parameters)) — so an application that saturates a group-boundary prefix of a multi-group function reduces to a closure over the remaining groups, with the prefix's bindings captured in its environment; that closure is an ordinary closure residual (§ 3).

### 2.2. `__if_then_else__` literal recognition

```text
__if_then_else__(A, .t(B), t, e)   →  t(())     (ι-IfTrue)
__if_then_else__(A, .f(B), t, e)   →  e(())     (ι-IfFalse)
```

The two ι rules fire **only** when the condition slot is the parser-recognized bool literal `.t` or `.f` (carrying its mandatory `(B)` role(bool) annotation per [`prime.md`](prime.md) § 2.2). An `__if_then_else__` whose condition is a stuck host call (or any other residual) is itself a residual — no implementation knowledge of "what the host returns" is allowed to leak in.

This rule is the one place the language has built-in awareness of the `role(bool)` boundary. It works because `.t` / `.f` are specific literal AST nodes, not host-supplied values: the partial evaluator can recognize them syntactically without consulting any `role(bool)` host type.

### 2.3. `__absurd__`

`__absurd__` has no reduction rule. The bottom type `!` has no values, so no reducible application of `__absurd__` can arise in a closed well-typed term that comes from `equiv`-discharge. (A host call `host : -> !` *can* appear under `__absurd__` in source, but the host call is opaque — see § 2.4 — and `__absurd__` applied to a stuck host call stays stuck. This is the right behavior: `__absurd__` doesn't claim anything about what the host's never-returning function "computes," so leaving the construct stuck preserves the structural-equality discipline of § 4.)

### 2.4. Host calls are opaque atoms

A host item `h` from a `host fn` participates in typing but does not reduce under partial evaluation. Two derivations of the same closed well-typed term share the same residual host-call structure: same head, same recursively-equivalent argument list. The partial evaluator therefore treats every host call as a leaf atom keyed by the host item's fully-qualified path.

Concretely:

- `print("hi")` reduces no further; it remains as a structural residual `Stuck(host:print, ["hi"])`.
- Two different `equiv` arms that mention `print("hi")` produce structurally identical residuals and compare equal.
- Two `equiv` arms that mention `print("hi")` and `print("bye")` produce different residuals and compare unequal.
- An expression statement whose left side is a stuck host call remains as a residual sequence until comparison. With the reference `scope` imported, `scope! { print("hi"); print("hi") }` and `scope! { print("hi") }` are not equivalent: the first normal form still contains the extra `print("hi")` action.

This is by design: the host boundary is opaque ([`../language.md`](../language.md#the-host-boundary)), so the test runner commits to nothing about what host calls compute. The opacity is what makes the reduction relation total under SN — a host that internally diverges is outside the scope.

### 2.5. Reduction strategy

**The normal-form strategy doesn't matter after totality observation succeeds.** Kio' is strongly normalizing (per [`prime.md`](prime.md) § 5) and confluent (β + ι reductions on a System F presentation with `newtype`-mediated recursion form a confluent rewrite system; see Pierce 2002 § 23 for the standard argument). For an arm whose evaluation observes no § 2.7 fault, every reduction strategy reaches the same residual normal form.

Totality observation is an error effect around that pure reduction relation, not another residual or rewrite rule. It follows strict call-by-value evaluation contexts: a binding initializer is evaluated before its body; the left side of a sequence before its continuation; and a computed callee and its value arguments, from left to right, before application. After their handler values have been evaluated, a known conditional or sum elimination applies only the selected closure. Constructing the unselected closure does not enter its body. An opaque sum applies both handlers under § 2.6; an opaque conditional retains both closures, which a later α- or η-equivalence probe in § 4 may apply. The first structural-recursion fault reached by those contexts is the absorbing outcome of the enclosing public evaluation or equality operation, even if a later β- or ι-step could discard the value that exposed it.

Implementations may use call-by-value, normal-order, weak-head, or a hybrid to construct a residual normal form, but a lazier normalizer must preserve the same totality observations. The Rust implementation uses one CBV-flavored walk for both jobs.

### 2.6. Symbolic case-split over an opaque sum

`__either__` carries one rule beyond the two inherited ι-rules (§ 2.1), for a scrutinee closed-term reduction never decides: an **opaque sum** — an `equiv` sum-typed parameter (§ 4.4), or any other residual that is not a known injection. The branch cannot be picked, but the result is still fully determined by each handler's action on its own branch, so instead of going stuck the evaluator residualizes symbolically:

```text
__either__(s, f_l, f_r)  →  case s { left(l) → f_l(l), right(r) → f_r(r) }   (ι-CaseSplit)

  (s an opaque residual — a free atom or a stuck term, not a __left__ /
   __right__ injection; l / r fresh opaque payload atoms, one per branch)
```

Each handler is applied to a **fresh opaque payload atom** and partial-evaluated under it; the result is the § 3 residual grammar's `case` form, carrying the scrutinee and the two branch residuals. Soundness is § 4.4's universal-parameter discipline at the value level: a well-typed handler is a total function of its payload, and a sum value is exactly one of its two injections, so each branch residual over a fresh atom characterizes the handler's action on every payload the opaque sum could carry — "for all `l`, for all `r`."

**Distributivity.** When the scrutinee is itself a symbolic case-split, `__either__` distributes into its branches rather than nesting: the outer scrutinee and payload binders are preserved, and each branch residual is re-eliminated with the same handlers — which may then fire ι-LeftEither / ι-RightEither on a branch that reduced to a known injection, or split / distribute again:

```text
__either__(case s { left(l) → v_l, right(r) → v_r }, f_l, f_r)
  →  case s { left(l)  → __either__(v_l, f_l, f_r),
              right(r) → __either__(v_r, f_l, f_r) }                      (ι-CaseDistrib)
```

Distributivity is what discharges composed eliminations over one opaque sum: in `swap ∘ swap`, the inner `swap` splits `s`, the outer `swap` distributes into the two branches, each doubly-flipped payload reduces back to its original injection by ι-LeftEither / ι-RightEither, and the resulting identity case-split collapses to `s` by sum-η (§ 4.6).

**Only `__either__` splits.** The other eliminators gain no symbolic rule. A stuck `__fst__` / `__snd__` or newtype projection loses nothing by staying stuck — projection has no branches to decide, and the stuck-projection residual is already structurally comparable (product-η in § 4.2 reads it directly). `__if_then_else__` has no analogous split either: a sum's inhabitants are closed by the language — exactly `__left__` and `__right__`, which is what makes the two-branch split exhaustive — but a `role(bool)` condition is a host-typed value whose inhabitants belong to the opaque host boundary (§ 2.2, § 2.4); the evaluator recognizes the literal tokens `.t` / `.f` and commits to nothing else.

ι-CaseSplit and ι-CaseDistrib extend the `equiv`-discharge evaluator only. The core reduction relation of [`prime.md`](prime.md) § 3 is unchanged — there, `__either__` over anything other than a known injection is simply stuck. The extension is conservative over the shared rules: where the scrutinee is a known injection, § 2.1's ι-LeftEither / ι-RightEither fire and a case-split never arises.

### 2.7. Structural-recursion totality faults

An `__structural_recur__` entry or callback edge executed by § 2.5's totality
observation contexts that cannot measure
its fuel or does not strictly descend is a totality fault, not a residual
normal form. The runner aborts that `equiv` discharge before grouping normal
forms and reports the Totality category (exit `16`). This applies equally when
the fault occurs in an evaluated value that an enclosing term discards and
when α- or η-equivalence probes a closure by applying it. A fault outcome is
never stored as an ordinary passing or failing `equiv` cache entry. The
structural descent law and diagnostics are specified in
[`elaboration.md` § 7.4](elaboration.md#74-elaborator-bangs).

## 3. Residual normal forms

A term `e` is in **residual normal form** when no rule from § 2 applies. After reduction, the term is one of:

```text
v ::=
    ()                              (unit)
  | .t(B) | .f(B)                  (bool literal at role(bool) type B)
  | n(N_k) | r(N_k) | s(S)          (numeric / string literal at its role-bearing type)
  | λ(x₁: T₁, …, xₙ: Tₙ). e         (closure, with body residualized)
  | Λα. v                           (type abstraction over a residual)
  | __left__(T̄, v)               (sum injection with residual payload)
  | __right__(T̄, v)
  | __pair__(T̄, v₁, v₂)          (product with residual components)
  | N.c(T̄, v)                       (newtype constructor with residual payload)
  | a                               (free atom — equiv parameter, unbound name,
                                     or host item)
  | case v_s { left(l) → v_l,       (symbolic case-split over an opaque sum
              right(r) → v_r }       v_s; l / r are fresh per-branch payload
                                     atoms — see § 2.6)
  | v_unit ; v_body                  (residual expression statement; v_unit
                                     is stuck at type .)
  | Stuck(v_callee, [v_arg₁, …])    (residual application: callee couldn't
                                     reduce — stuck on an atom or another
                                     stuck residual)
```

A **stuck residual** carries through composition: applying a stuck callee to further arguments produces a deeper stuck residual.

**Stuck shapes the runner expects:** host calls applied to fully-evaluated arguments; `__if_then_else__` whose condition is a stuck host call; `__absurd__` whose argument is a stuck host call; newtype member access against an opaque atom. All four are valid residuals — the equivalence relation in § 4 compares them structurally.

## 4. Equivalence relation

Two residual normal forms `v₁` and `v₂` are **equivalent**, written `v₁ ≃ v₂`, iff they coincide up to **α + η + structural atom equality**:

### 4.1. α-equivalence (binder renaming)

`λ(x). e ≃ λ(y). e[x := y]` — closures are equal up to consistent renaming of bound parameters (and the same for `Λα. e`). The implementation uses fresh-name normalization on closure bodies before comparing.

### 4.2. η-equivalence (function pointwise equality)

```text
λ(x). f(x)  ≃  f                              (when x ∉ FV(f))
__pair__(__fst__(p), __snd__(p))  ≃  p     (η for products)
__either__(s, __left__, __right__)  ≃  s   (η for sums)
```

Any of these can be applied in either direction during NF comparison; the implementation may β/η-normalize before comparing. (η-equivalence is preserved under reduction in System F + sums + products with η laws — see Pierce 2002 § 23.)

Function comparison is pointwise over the semantic domain of one value group,
not over the source binder partition used to spell that group. For a product
domain, the comparison therefore supplies one fresh opaque right-associated
product packet to both closures. A unary binder receives the packet whole,
while positional binders receive its components. This consumes exactly one
value group; later curry groups remain distinct and are compared in turn.

### 4.3. Atom equality

Two `Atom(name)` residuals are equal iff `name` is byte-identical. The atom's name is the term's fully-qualified path: parameter names are plain identifiers (`p0`, `acc`, …), unbound names are plain identifiers, host items are their fully-qualified paths (`print`, `io.println`, `pdt.int_to_string`).

Two `Stuck(callee, args)` residuals are equal iff `callee₁ ≃ callee₂` and
their argument packets are pairwise equivalent. For this comparison only, a
final right-spined `__pair__` argument is the semantic packet spelling of its
flat final components: `[a, __pair__(b, c)] ≃ [a, b, c]`. A non-final pair,
an opaque argument, and a nested `Stuck` application remain structural
boundaries; in particular `[__pair__(a, b), c] ≄ [a, b, c]` and
`Stuck(Stuck(f, [a]), [b]) ≄ Stuck(f, [a, b])`.

Two residual expression statements `v₁ ; body₁` and `v₂ ; body₂` are equal iff `v₁ ≃ v₂` and `body₁ ≃ body₂`. A residual expression statement is not equal to its body alone; the discarded unit result does not imply the action that produced it can be erased.

### 4.4. `equiv` parameters as fresh opaque atoms

When an `equiv` block carries value parameters `(p₁: T₁, …, pₙ: Tₙ)`, the runner binds each `pᵢ` to a fresh opaque atom `aᵢ` **shared across every arm**. Reduction in arm `j` may produce residuals containing `aᵢ`; comparison across arms (§ 4.3) then equates references to the same parameter from different arms.

The "shared across arms" rule is what makes parametric `equiv` express a universal claim: the arms are claimed equal *for every instantiation* of the parameters. The runner discharges the universal quantification by reasoning about the bound names symbolically — the same atom in different arms compares equal, which is the symbolic-evaluation reading of "for all `aᵢ`."

### 4.5. Inequivalence cases

The relation rejects the following:

- Different head atoms: `Atom("foo") ≄ Atom("bar")`.
- Different application skeletons after final product-packet canonicalization:
  `Stuck(f, [p]) ≄ Stuck(f, [a, b])` for opaque `p`;
  `Stuck(f, [a]) ≄ Stuck(g, [a])` (different head).
- Different sequence skeletons: `Stuck(f, []) ; Stuck(g, []) ≄ Stuck(g, [])` (extra stuck action).
- Different injected branches: `__left__(v) ≄ __right__(w)`; `__left__(v) ≄ __left__(w)` when `v ≄ w`.
- Different newtype heads: `N.c(v) ≄ M.c(v)` when `N ≠ M`; `N.c(v) ≄ N.d(w)` when `c ≠ d`.
- Literals that disagree: `.t ≄ .f`; `42^i32 ≄ 7^i32`; `42^i32 ≄ 42^i64` (same digits, different role).

### 4.6. Symbolic case-split equivalence

A symbolic case-split (§ 2.6) is compared **structurally up to consistent renaming of its fresh payload atoms** — the same fresh-binder discipline α-equivalence (§ 4.1) uses for closures:

```text
case s₁ { left(l₁) → b_l₁, right(r₁) → b_r₁ }  ≃  case s₂ { left(l₂) → b_l₂, right(r₂) → b_r₂ }
  iff   s₁ ≃ s₂
   and  b_l₁ ≃ b_l₂[l₂ := l₁]      (left branches, payload atoms identified)
   and  b_r₁ ≃ b_r₂[r₂ := r₁]      (right branches, payload atoms identified)
```

Two case-splits over the same opaque sum are equal iff they act identically on each branch — the value-level reading of "for all `l`, for all `r`." This is what discharges parametric `equiv` laws over opaque sums.

**Interaction with sum-η (§ 4.2).** The sum-η law generalizes to a case-split: an **identity case-split** — one that re-injects each branch's payload unchanged — collapses back to its scrutinee:

```text
case s { left(l) → __left__(l), right(r) → __right__(r) }  ≃  s
```

This is the η for sums in its residual-form guise: `__either__(s, __left__, __right__) ≃ s` (§ 4.2) is exactly the case-split that does nothing. Combined with the distributivity rule of § 2.6, it discharges involution-style laws (`swap ∘ swap ≃ id`): the nested eliminations distribute into the branches, each branch double-flips to its original injection, and the resulting identity case-split collapses to `s`.

**Inequivalence.** A non-identity case-split is *not* equal to its scrutinee: `case s { left(l) → __right__(l), right(r) → __left__(r) }` (a single `swap`, on a sum `A | A` where the result type-checks) keeps the branches flipped and so `≄ s`. The branch-by-branch comparison is what distinguishes it.

## 5. Worked examples

### 5.1. Pass — same NF

```kio
equiv id_unit_eq {
  (.(x) { x })(());
  ()
}
```

Both arms reduce to `()`. NF-equivalent. ✓

### 5.2. Pass — η-equivalence

```kio
equiv eta {
  .(x) { f(x) };
  f
}
```

(With `f` an opaque atom — say a host call.) The first arm reduces to `λx. Stuck(f, [x])`; η on the closure yields `Stuck(f, [])` ≃ the second. ✓

### 5.3. Fail — different stuck shapes

```kio
equiv neq_stuck {
  f(());
  g(())
}
```

Both arms are stuck residuals; the heads differ (`f` vs `g`). Inequivalent. ✗

### 5.4. Pass — parametric equiv with shared atoms

```kio
equiv id_with_arg[A](v: A) {
  v;
  (.(x) { x })(v)
}
```

The runner binds `v` to a fresh atom `a₀` (shared across both arms). Arm 1 reduces to `Atom(a₀)`; arm 2 reduces to `(.(x). x)(a₀)` → `a₀` by β. NF-equivalent. ✓

### 5.5. Fail — parametric equiv that distinguishes positions

```kio
equiv distinct(v: a, w: a) {
  v;
  w
}
```

`v` binds to atom `a₀`; `w` binds to atom `a₁`. Arm 1 = `a₀`, arm 2 = `a₁`. Different atoms, inequivalent. ✗ — the equiv claim is that *every* `(v, w)` pair makes the arms equal, so a counterexample with `v ≠ w` defeats it.

### 5.6. Stuck `__if_then_else__` on an opaque condition

```kio
import elab/control(if);

equiv stuck_if_eq(c: Bool) {
  if! c { foo() } else { bar() };
  if! c { foo() } else { bar() }
}
```

The condition `c` binds to an atom `a₀`; `__if_then_else__` against an atom doesn't reduce. Both arms have the same stuck shape `Stuck(__if_then_else__, [a₀, λ_. Stuck(foo, []), λ_. Stuck(bar, [])])`. NF-equivalent. ✓

By contrast:

```kio
equiv stuck_if_ne(c: Bool) {
  if! c { foo() } else { bar() };
  if! c { foo() } else { baz() }
}
```

The else-arm closures differ (`Stuck(bar, [])` vs `Stuck(baz, [])`). Inequivalent. ✗

## 6. References

- [`prime.md`](prime.md) — Kio' typing rules, reduction, type safety, SN, decidability. The reduction rules in § 3 of this page reuse the rules from `prime.md` § 3.
- The `equiv` discharge sees the **elaborated** calls — the typer substitutes every elaborator bang-call (the reference coercion palettes, `match!`, `derive!`) at the `Lowered → Prime` boundary, so only the ordinary intrinsic glue they expand to reaches the partial evaluator. The per-library soundness arguments for those expansions live with the executable POC ([`../../docs/poc/elab.md`](../../docs/poc/elab.md), [`../../docs/poc/optics.md`](../../docs/poc/optics.md)), not in this formal directory.
- Pierce 2002 § 23 — confluence of β + ι in System F with iso-recursive `newtype` boundaries; the basis for § 2.5's "strategy doesn't matter" claim.
