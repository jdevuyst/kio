---
name: audit-surface-forms-usage
description: Verify non-Kio' .kio files prefer surface forms over intrinsic spellings — no __intrinsic__, prefer UFCS, spine-first elaborators, imported if!/else, elided type-args
allowed-tools: Read, Grep, Glob, Bash
---

# Surface-form usage audit

ai/topics/surface-forms.md enumerates conventions for `*.kio` files that are *not* the kio-prime subject: prefer the surface form over the Kio'-level intrinsic / desugared form. This skill audits the corpus for those conventions.

The exception for intrinsic/desugared-form checks is files whose **subject is the intrinsic / desugared form itself** — typically goldens named `build_intrinsics`, `typecheck_intrinsics`, or per-feature roundtrip tests pinning the elaborated AST. Those keep the lower-level spelling. Detect subject-of-intrinsic files by name pattern or by an explicit comment in the case header; flag candidates rather than auto-excluding them. The Unit-result check below still applies in those files when the written result is incidental.

Read ai/topics/surface-forms.md before starting.

## 1. Intrinsic leakage

In `*.kio` files outside the intrinsic-subject set, grep for:

- `use __intrinsics__;` — should not appear except in intrinsic-subject files.
- `__intrinsic_*` direct calls — should not appear; the surface has operators, tuple literals, spine elaborators, `match!`, imported `if!`/`else`, etc.
- `__either__`, `__if_then_else__`, `__pair__`, `__left__`, `__right__` — same; sum elimination goes through `match!`, product construction through tuple literals, sum construction through `widen_sum!`.

For each hit, decide whether the file is genuinely subject-of-intrinsic. If unsure, flag it.

## 2. Elaborator-shape preferences

Per [`ai/topics/surface-forms.md`](../../topics/surface-forms.md), ordinary (non-subject) surface code prefers:

- **Spine-first coercions** — the axis-specific spine forms (`reorder_sum!` / `reorder_prod!` / `narrow_sum!` / `narrow_prod!` / `widen_sum!` / `widen_prod!` / `flatten_sum!` / `flatten_prod!` / `one_sum!` / `one_prod!`) for single-axis coercions, `fit!` for multi-axis or recursive ones. The algebraic palette (`iso!` / `into!` / `onto!` / `align!` / `ease!`) is subject-gated; sweeping for out-of-subject algebraic uses is [`audit-spine-vs-algebraic`](../audit-spine-vs-algebraic/SKILL.md)'s job — don't duplicate it here.
- **`atom!` for picking a single value from an aggregate** — single-component projection (`r.>atom!(Field)`, `pair.>atom!(T)`). `if!`/`else` and `match!` already return the common arm type directly, so an `atom!` added just to collapse their result is itself a finding, as is `atom!` on a plain atomic reference where a bare name reads better.
- **UFCS for blockless elaborator calls that admit it** — `r.>fit!(T)` over `fit!(r, T)`, `r.>atom!` over `atom!(r)`, and so on across the parenthesized palette; the list is omitted when there's no trailing target type argument, because a written empty UFCS list is invalid. Imported trailing-block calls such as `if!` and `match!` retain their direct block form.
- **UFCS for type-member access** — `r.>T.member` over `T.member(r)` when `T.member` takes a single value-arg the receiver fills.
- **UFCS for readable ordinary-call chains** — review nested calls where a value's path through ordinary functions would be clearer with `.>` or `.>>`; retain prefix calls when they express argument roles or the test subject better. Never infer that a left-associated rewrite preserves a right-nested call's order or behavior.
- **Explicit-target form for inline coercions** — `widen_sum!(e, T)` inline rather than a wrapper `fn mk_left(s: String) -> (String | !) { widen_sum!(s) }`.

For each `*.kio` outside the intrinsic-subject set:

- Find hand-compositions of axis-specific forms (`narrow_prod!(widen_sum!(e, T), U)`) where a single `fit!(e, U)` produces the same result.
- Find `match! value { clauses }` / `if! c { a } else { b }` results wrapped in `atom!` just to collapse the arms — the wrap is redundant.
- Find blockless elaborator calls that admit UFCS whose first argument is a single name or short expression spelled call-first — those read better as UFCS. Do not flag imported trailing-block calls for lacking UFCS.
- Find nested ordinary calls whose value flow is obscured and sketch a grouping-preserving UFCS rewrite. Treat this as a readability judgment, not a blanket conversion; classify host effects, syntax-focused cases, and intentional teaching examples before proposing a change.
- Find one-off helper fns whose only purpose is to anchor an expected type for an elaborator call (`fn mk_*(…) -> (… | !) { widen_sum!(…) }`) — those should be inlined as `widen_sum!(…, (T | !))`.
- Find type aliases introduced solely to dodge "compound type in a type-arg slot" — that constraint doesn't exist; compound types are valid in every type-arg position.

## 3. Other antipatterns

- `let _ = e;` discarding a unit value where `e;` (expression statement) would work. Find every `let _ =` and check whether the RHS is unit-typed — if so, flag it. Reserve `let _ = e;` for genuinely non-unit values being deliberately discarded.
- Explicit type-arguments that inference could fill. Find call-sites like `id(T, x)`, `Box.mk_box(T, v)`, `push(T, x, xs)` and check whether the type-arg is forced (case subject is the explicit-type-arg path) or could be elided.
- Redundant `-> .` on ordinary named functions. Follow the preference in [`kio-authoring.md`](../../topics/kio-authoring.md) and its linked public tutorial: an omitted result defaults to Unit for a named function, unlike an anonymous function. Inventory `*.kio` and Kiodoc fences under `docs/`, including multiline/commented headers; a line-only grep is discovery, not completion. Classify each occurrence before reporting: incidental ordinary result (suggest omission), written result itself taught/asserted or needed for exact source/parser/formatter/diagnostic coverage (retain), grammar-required host declaration or recorded signature artifact (retain), derived dependency copy (report its canonical owner), or uncertain (flag for review). Check recursive declarations against their own grammar and case subject. A Prime marker or Kio' artifact alone does **not** require the written result; retain it only when that spelling is the subject. Do not rewrite hits automatically or treat function-type arrows and anonymous-result inference as this rule.

## 4. docs/

Apply the same checks to `*.kio` snippets in `docs/` (Kiodoc fences). The docs corpus is at least as much in scope as the test corpus — these are pedagogical, and snippets that use intrinsic spellings teach the wrong style.

## How to report

Group findings by file and rule. For each:

- File path and line.
- Which [`ai/topics/surface-forms.md`](../../topics/surface-forms.md) convention is violated (cite its bold lead-in or section).
- The surface-form rewrite, sketched.

Skip files identified as subject-of-intrinsic; if the classification is uncertain, list those separately so the user can confirm.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).
