# Surface forms in non-Kio' files

Trigger: editing a `.kio` file.

Kio' is the small, intrinsic-bearing core; the full surface language layers operators, tuple literals, `.stem. { ... }` placeholder lambdas, `lit` declarations, UFCS, and imported macro calls (including the block forms `if`, `scope`, `do`, and `match`) on top of it. When a file is *not* Kio' (i.e., it does not parse against the Kio' grammar in [`specs/prime.md`](../../specs/prime.md)), prefer the surface form to the Kio' equivalent it elaborates to:

- No `import __intrinsics__;` and no direct `__intrinsic__` calls. Use operators, tuple literals, spine macros, `match`, `if`/`else`, etc.
- For value coercions, reach for the right macro. Prefer the spine palette for ordinary surface code: `widen_sum` / `widen_prod` (widen), `narrow_sum` / `narrow_prod` (narrow), `reorder_*`, `flatten_*`, `one_*`, and `fit` for composed coercions. Use the algebraic palette (`iso`, `into`, `onto`, `align`, `ease`) when the file is specifically exercising those forms or the algebraic rule is the subject. `atom` is the exception in that palette: reach for it freely to pick a single value from an aggregate — that everyday case is ordinary surface code, not subject-gated (see the `atom` guidance below) — and reserve the subject-gating only for `atom`'s broader DNF-flatten-to-a-nonobvious-arm generality.
- Sum elimination goes through `match`, not `__either__` or hand-rolled `__if_then_else__` chains.
- Product construction is the tuple literal `(a, b, …)`, not `__pair__`.
- Sum construction is `widen_sum(e)`, not `__left__` / `__right__` (except when the goldens' subject is the intrinsic itself).
- Conditionals use imported `if` with its `else` block, not `__if_then_else__`; use `scope { … }` for an ordinary scoped sequence and `do(bind) { … }` for bind-based sequencing.

The exception is when the file's *subject* is the intrinsic (e.g., a golden whose point is to assert the intrinsic typechecks, builds, or executes — `build_intrinsics`, `typecheck_intrinsics`). Those cases keep the direct intrinsic usage; what they're testing is the Kio'-level form. A second flavor of the same exception: any golden that carries an `IS_KIO_PRIME` marker is *required* to be valid Kio' — the marker is biconditional (see `ci/checks/per-case/prime-marker.sh`), so introducing a surface form into a regular-module *.kio file inside such a case removes the marker, narrowing the corpus the Kio' verifier validates. When the case's subject is Kio'-level typechecking, building, or execution (`typecheck_rec_deftypes`, `typecheck_existentials`, `exec_panic`, `build_sum_elimination`, …), the intrinsic stays.

**Use the coercion macro's explicit-target form for inline coercions.** The coercion macros below accept a trailing target type: `widen_sum(e)[T]`, `narrow_prod(e)[T]`, `fit(e)[T]`, etc. The palette is split in two: the **spine** forms (`reorder_sum` / `reorder_prod` / `narrow_sum` / `narrow_prod` / `widen_sum` / `widen_prod` / `flatten_sum` / `flatten_prod` / `one_sum` / `one_prod` / `fit`) and the **algebraic** forms (`iso` / `into` / `onto` / `align` / `ease` / `atom`). When the expression's surrounding context doesn't pin the type (a `let` RHS, an arg in a generic call), pass the target explicitly inline. **Do not** introduce a one-off helper fn (`fn mk_left(s: String) -> (String | !) { widen_sum(s) }`) just to anchor the expected type — the inline `widen_sum(s)[String | !]` is shorter, keeps the test's intent local, and avoids polluting the module with a wrapper. Helper defns are warranted when they capture genuine reuse, not when they exist solely as type-annotation scaffolding.

**Single-macro coercions use the axis-specific form; multi-macro coercions use `fit`.** Each spine form (`reorder_sum` / `reorder_prod` / `narrow_sum` / `narrow_prod` / `widen_sum` / `widen_prod` / `flatten_sum` / `flatten_prod` / `one_sum` / `one_prod`) covers exactly one axial operation. When a coercion does one thing — permute, drop, widen, flatten an outer axis, or pick — name that thing: `narrow_prod((a, b, c))[A & C]`, where the values have distinct types `A`, `B`, and `C`, over a hand-built composition. When a coercion needs more than one — narrow on one axis while widening on another, or recurse through nested products and sums — reach for `fit`, which composes the narrow-before-widen discipline, recurses on slots, and walks function arrows contravariantly/covariantly. Composing axis-specific forms by hand (`narrow_prod(widen_sum(e)[T])[U]`) is discouraged when `fit(e)[U]` would produce the same result; the composer reads better and lets the engine validate the multi-axis story end to end.

**Consider UFCS for macro calls.** Prefer a receiver spelling when it makes the
source value easier to follow: `r.>fit` and `fit(r)` can infer the same target;
`(r.>fit)[T]` and `fit(r)[T]` supply it explicitly. The target's brackets stay
after the source stage, rather than moving onto the callee before receiver
insertion. The same rule applies to the other source-to-target macros.
Receiver-only UFCS omits its value list; an empty selected UFCS list rejects,
and `(())` supplies an additional Unit. Block calls can likewise use
`value.>match { clauses }` rather than `match(value) { clauses }`.
Forward insertion selects the first prefix group and reverse the last, before
all projected blocks; reverse syntax follows the complete block chain. Keep
braces around a spliced final-label child. See the [UFCS guide](../../docs/guides/ufcs.md).
When a test's subject is a specific prefix or splice spelling, preserve it.

A reverse-spliced block call requires explicit braces around its own final
labelled child, even through grouping around the whole parent. The diagnostic
fix adds those braces without moving the receiver. Direct and forward calls,
and deeper ordinary chains inside an explicit block, retain ordinary elision.

**Prefer UFCS for type-member access.** Write `r.>T.member` over `T.member(r)` when `T.member` takes a single value-arg that the receiver fills (see [`specs/language.md` § UFCS](../../specs/language.md#ufcs)). Same exception applies — keep `T.member(r)` when the test's subject is the projector/constructor call form itself.

**Consider UFCS for nested ordinary calls.** When a value flows through several ordinary functions, use the receiver-first or receiver-last splice if it makes that flow easier to read; see [`docs/guides/ufcs.md` § Choosing a spelling](../../docs/guides/ufcs.md#choosing-a-spelling). Prefix calls remain appropriate when they show the argument roles more clearly or the call spelling itself is the test subject. Preserve the original argument grouping and evaluation order: a chain that reassociates nested calls can change host-visible behavior.

**`let _ = e;` is an antipattern; use the expression-statement form instead.** Kio block bodies admit both `LetStmt` (`let name = e;`) and `ExprStmt` (`e;`). The expression-statement form discards `e`'s value directly — the typer requires `e : .`, which most side-effecting calls already produce. Write `print(s); print(rest)` (one expression statement followed by the trailing expression `print(rest)`), not `let _ = print(s); print(rest)`. The bound name `_` carries no information, the `let` adds visual noise, and the surrounding code reads less clearly. Use `let _ = e;` only when `e`'s type is genuinely non-unit and you need to deliberately discard the value — those cases are rare.

**Prefer omitted type arguments when local inference determines them.**
For a typed value `text: String` and `fn f[A](value: A) -> A`, prefer
`f(text)` over `f[_](text)` or `f[String](text)`. The same preference applies to
`Box.mk(v)` over `Box.mk[T](v)` and `push(x, xs)` over
`push[T](x, xs)`. Use `[_]` when an inferred slot must precede a later explicit
type argument, or write a concrete type when the finite local context does not
determine it. An unannotated literal is not by itself a concrete carrier cue.
Do not add type arguments or carrier annotations to evade a compiler defect;
retain explicit forms when their behavior is the test's subject. Formatting
must not insert omitted type arguments.

**Don't introduce a alias as a call-arg type-arg workaround.** Compound types `(A & B)` / `(A | B)` are valid in every type-arg position, including the trailing target slot of `iso` / `into` / `onto` / `align` / `ease`. A alias is justified by reuse or by giving a domain concept a name (`Cnf`, `Dnf`, `Lens`); it is not justified by "the slot can't carry a compound type" — that is never a constraint.

**Prefer `atom` only for picking a single value from an aggregate.** `atom(e)[T]` (or its target-elided form `atom(e)`) is the focused macro for "extract a single value of type `T` from this aggregate." The common case is single-component projection from a record / product: `(r.>atom)[Field]` picks the nominal field type from a product, or `(pair.>atom)[T]` picks the `T`-typed component from a product. `if`/`else` and `match` already return the common arm type directly; don't add `atom` just to collapse their result.

`atom`'s target must DNF-normalize to a single arm (atom, nominal, or product); a multi-arm sum target is a type error. An atomic source is admitted as a degenerate identity, but a plain reference reads better than `(a.>atom)[A]` for that case — reach for `atom` when there's actually something to pick from.

Use the axis-specific spine macro (`narrow_sum`, `widen_sum`, `fit`, etc.) for sum narrowing, widening, or other length-changing rearrangements. Same exception as above: when the file's *subject* is a broader algebraic macro being parsed, lowered, or rejected, keep the original spelling.

**Repeated same-typed sum arms should usually be named, not hand-injected.** `into` is deterministic across like-typed sum arms via the source-order rule (see [`specs/language.md`](../../specs/language.md) § Anonymous sum and product types, *Commutativity*): the kth source DNF branch of type `T` maps to the kth target slot of type `T`. So `into(100[I32])[(I32 | String) | I32]` always lands the `I32` in the *leftmost* (inner) `I32` arm. For ordinary source, that is a signal to introduce nominal types for the domain alternatives, or to write the example so the target branch is identified by type. Do not solve routine modeling ambiguity by importing `__intrinsics__`.

If a focused test must construct a non-leftmost like-typed structural arm, make that test's subject explicit: it is exercising Kio' / intrinsic construction, not ordinary surface authoring. Keep any raw intrinsic usage confined to that intrinsic-focused case; castles, docs examples, and natural goldens should use nominal types, `widen_sum`, and `match` unless the user explicitly asks for the core form itself.
