---
name: audit-spine-vs-algebraic
description: Audit spine-first macro usage in goldens, POCs, and docs, retaining algebraic-subject cases and ordinary single-component atom picks.
allowed-tools: Read, Grep, Glob, Bash
---

# Spine-vs-algebraic usage audit

Anchor: [`ai/topics/surface-forms.md`](../../topics/surface-forms.md), **Single-macro coercions use the axis-specific form; multi-macro coercions use `fit`** and **Prefer `atom` only for picking a single value from an aggregate**. Algebraic macro calls (`iso`, `into`, `onto`, `align`, `ease`, and broader DNF uses of `atom`) are subject-gated. Ordinary single-component `atom` picks are an explicit exception, valid outside algebraic-subject files. Other ordinary coercions use the spine palette appropriate to their shape.

Read ai/topics/surface-forms.md and `docs/poc/macros.md` before starting.

The exception is files whose **subject is an algebraic macro** — typically goldens named `exec_iso_*`, `exec_into_*`, `exec_onto_*`, `exec_align_*`, `exec_atom_*`, `exec_ease_*`, `typecheck_iso_*`, `typecheck_into_*`, `typecheck_onto_*`, `typecheck_align_*`, `typecheck_atom_*`, `typecheck_ease_*`, and failure-tier goldens whose directory name has an `iso_` / `into_` / `onto_` / `align_` / `atom_` / `ease_` prefix (`15_macro_error/iso_rejects_collapse`, `14_type_error/onto_rejects_widening`, …). Those keep the algebraic spelling — what they're testing is the algebraic form's behavior, rejection, or roundtrip. A second carve-out: a per-case comment that explicitly names the algebraic form as the test subject (e.g., `// subject: into's source-order rule`). Documentation comments and spec fragments embedded in test sources also pass.

## 1. The scan

Collect candidates from hand-written Kio in `test-data/goldens/`,
`test-data/poc/`, Kiodoc fences in `docs/`, and embedded Kio in
`kio-rs/tests/`. Search both familiar palette spellings and explicit macro
selectors so a local import alias cannot escape the inventory:

```sh
rg -n '\b(iso|into|onto|align|atom|ease)\b|\bmacro[[:space:]]+' \
  test-data/goldens/ test-data/poc/ --glob '*.kio'
rg -n '\b(iso|into|onto|align|atom|ease)\b|\bmacro[[:space:]]+' \
  docs/ kio-rs/tests/ --glob '*.md' --glob '*.rs' --glob '*.kio'
```

These are candidate searches, not a semantic oracle. Resolve each selector to
its provider declaration and follow its local `as` destination through the
actual source scope. Inspect direct, grouped and UFCS calls under that local
spelling; an alias need not contain any palette name. Track earlier same-module
macro declarations, ordinary local/parameter bindings and numbered-family
shadowing. Separate actual macro calls from ordinary functions or values,
prose, comments and unrelated same-spelled declarations. A familiar name alone
does not establish a palette operation or justify rewriting it.

Trace a materialized dependency to its canonical source through the declared
package/import edges. The `poc/macros/` package teaches the macro mechanism
itself; other POC topics may contain focused algebraic subject cases. Directory
names and explicit subject comments are classification evidence, rather than
reasons to omit a source from the candidate inventory. Verify current case
names before relying on a naming convention. The docs corpus is pedagogical;
apply the same subject and ordinary-`atom` exceptions there.

Record both selector identities and their actual calls. Retain arbitrary-alias
and same-spelled ordinary-binding controls when assessing whether the scan is
complete: the former must be followed to its calls, and the latter must not
become a palette finding. Report unavailable binding context as unresolved
instead of guessing from a name.

## 2. Classify hits

For each authenticated palette call, decide:

- **Subject-of-algebraic-form** — the enclosing file's purpose is to assert behavior of the algebraic macro. Pass.
- **Ordinary single-component `atom` pick** — an aggregate projection such as `r.>atom(Field)`. Pass; no subject marker is required. Redundant result-collapse and atomic-identity uses belong to `audit-surface-forms-usage`.
- **Pre-migration residual** — a golden that pre-dates the spine palette and hasn't been migrated yet. Flag for migration per the cheatsheet below.
- **Inadvertent algebraic use** — a newly authored golden whose subject is not the algebraic form. Flag as a discipline violation.

If the classification is uncertain (no naming signal, no explicit subject comment), list separately so the user can confirm.

## 3. How to report

Group findings by file and rule. For each:

- File path and line.
- Which algebraic form leaked (`iso`, `into`, …).
- The spine-form rewrite per the migration cheatsheet below; when multiple rewrites are plausible, list them in order of preference.

Skip subject-of-algebraic-form uses and ordinary single-component `atom` picks; if the classification is uncertain, list those separately.

## Migration cheatsheet

When rewriting a hit to a spine form, follow the mapping below. The mapping is derived from the call's outer-axis analysis; see `docs/poc/macros.md` (§ The spine palette, § The algebraic palette) for the per-macro specs.

- **`iso(e, T)`** — reorder along the outer axis of `T`. Sum-outer → `reorder_sum(e, T)`. Product-outer → `reorder_prod(e, T)`. **Associativity-only rearrangement** (e.g., `(A | B) | C → A | (B | C)`, no arm-multiset change) → `flatten_sum(e, T)` / `flatten_prod(e, T)` per axis.
- **`into(e, T)`** widening — `widen_sum(e, T)`. If the call relies on like-typed duplication (source-order rule across `&`), `widen_prod(e, T)`. When the rewrite isn't a single-axis widening (mixed-axis, recursive), `fit(e, T)`. **`into(x: !, T)` where the source is genuinely `!`** → `fit(x, T)` (the spine palette's Bottom-source carve-out covers this via the initial-object law; the elaboration is the same `__absurd__` glue at the leaf). NOT `__absurd__(x)` directly — that would change the file's Kio'-grammaticality state if the file isn't already importing intrinsics.
- **`onto(e, T)`** narrowing — `narrow_prod(e, T)` for product narrowing (drop slots), `narrow_sum(e, T)` for sum collapse over same-typed extras. Mixed-axis → `fit(e, T)`.
- **`align(e, T)`** — usually `fit(e, T)` (the recursive composer handles align's same-type-collapse + new-arm widening pattern natively). Pure same-type collapse alone → `narrow_sum`. Pure payload-preserving sum injection → `widen_sum`. **Pure associativity rearrangement on the sum axis** → `flatten_sum`.
- **`ease(e, T)`** — `fit(e, T)`. `fit` is the spine sibling of `ease`; it admits sum widening and product projection without value duplication, matching `ease`'s coverage. **`ease` paired with `iso` for associativity reshuffle** → `flatten_sum` / `flatten_prod` plus `fit` composition, with the flatten step naming the associativity move explicitly.
- **`atom(e, T)`** — sum source with uniform-typed arms → `one_sum(e, T)`. Product source picking the unique-typed slot → `one_prod(e, T)`. UFCS receiver-style picker → `r.>one_prod(T)`. **Not every `atom` hit is a finding**: `ai/topics/surface-forms.md`'s "prefer `atom` only for picking a single value from an aggregate" guidance is blanket, not subject-gated — that pick case (single-component projection, e.g. `r.>atom(Field)`) is idiomatic in any golden. Subject-gating still applies to `atom`'s more general DNF-flatten-to-nonobvious-arm case (the target only normalizes to a single arm after DNF flattening) — that generality stays confined to files whose subject is the algebraic macro itself.

Prefer the axis-specific spine form when a single axis suffices; reach for `fit` when the rewrite needs recursive composition (per `ai/topics/surface-forms.md` — *single-macro coercions use the axis-specific form; multi-macro coercions use `fit`*). When the call's purpose is associativity rearrangement on one axis, `flatten_sum` / `flatten_prod` is the sharpest match — the call site reads as "re-group this axis," which neither `iso` nor `fit` names.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md). Rewrite authorized findings to their spine equivalents per the cheatsheet and group related fixes into coherent publication commits; golden counts do not dictate commit counts. Scope validation under [`local-ci.md`](../../topics/local-ci.md), retaining the required exact full-matrix coverage for new or behaviorally edited goldens and the distinct rules for reviewed mechanical migrations.
