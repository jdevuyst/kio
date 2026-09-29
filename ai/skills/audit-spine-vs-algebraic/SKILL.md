---
name: audit-spine-vs-algebraic
description: Verify algebraic-elaborator names (iso!/into!/onto!/align!/atom!/ease!) appear in goldens and POCs only where the algebraic form is the subject — everything else uses spine forms (reorder_*, narrow_*, widen_*, flatten_*, one_*, fit!)
allowed-tools: Read, Grep, Glob, Bash
---

# Spine-vs-algebraic usage audit

ai/topics/surface-forms.md enumerates the broader "prefer the higher-level form unless the file's *subject* is the lower-level form" discipline. This skill specializes the same rule to the spine vs. algebraic elaborator palette: once the spine palette has landed, the algebraic-elaborator bang-calls (`iso!`, `into!`, `onto!`, `align!`, `atom!`, `ease!`) appear in goldens **only when the golden's subject is the algebraic elaborator itself**. Everything else — match cases, structural-coercion cases, error-handling cases, optics cases, row-record cases — uses spine forms (`reorder_sum!`, `reorder_prod!`, `narrow_sum!`, `narrow_prod!`, `widen_sum!`, `widen_prod!`, `flatten_sum!`, `flatten_prod!`, `one_sum!`, `one_prod!`, `fit!`).

Read ai/topics/surface-forms.md and `docs/poc/elab.md` before starting.

The exception is files whose **subject is an algebraic elaborator** — typically goldens named `exec_iso_*`, `exec_into_*`, `exec_onto_*`, `exec_align_*`, `exec_atom_*`, `exec_ease_*`, `typecheck_iso_*`, `typecheck_into_*`, `typecheck_onto_*`, `typecheck_align_*`, `typecheck_atom_*`, `typecheck_ease_*`, and failure-tier goldens whose directory name has an `iso_` / `into_` / `onto_` / `align_` / `atom_` / `ease_` prefix (`15_elaborator_error/iso_rejects_collapse`, `14_type_error/onto_rejects_widening`, …). Those keep the algebraic spelling — what they're testing is the algebraic form's behavior, rejection, or roundtrip. A second carve-out: a per-case comment that explicitly names the algebraic form as the test subject (e.g., `// subject: into!'s source-order rule`). Documentation comments and spec fragments embedded in test sources also pass.

## 1. The scan

Deterministic, read-only scan over both corpora that carry hand-written Kio source — `test-data/goldens/` and `test-data/poc/`. From the repo root:

```bash
grep -rnE '\b(iso|into|onto|align|atom|ease)!' test-data/goldens/ \
  | grep -vE '/(exec|typecheck)_(elaborator_)?(iso|into|onto|align|atom|ease)_' \
  | grep -vE '/(iso|into|onto|align|atom|ease)_[a-z_]+/'

grep -rnE '\b(iso|into|onto|align|atom|ease)!' test-data/poc/
```

The `goldens/` scan's first `grep -v` excludes `00_success/` and equivalent success-tier goldens whose name starts `exec_<form>_` / `typecheck_<form>_`. The second excludes failure-tier goldens (`15_elaborator_error/iso_rejects_collapse`, etc.) whose directory leaf begins with one of the form names. Refine the patterns to match the live corpus — verify directory naming via `ls test-data/goldens/00_success/ | grep -E '^(exec|typecheck)_(iso|into|onto|align|atom|ease)_'` and `ls test-data/goldens/15_elaborator_error/` before relying on the regex.

The `poc/` scan needs one naming exclusion: `test-data/poc/elab/`, whose demo entry point (`workdir/demo/elab/main.kio`) opens with a header comment stating its subject IS the elaborator mechanism itself (spine, row, tuple, and algebraic elaborators, `derive`, `match`, `show`) — every algebraic-form hit there is expected and passes. The remaining POC topics are data-structure libraries (`list/`, `option/`, `result/`, `queue/`, …), none of which has an algebraic elaborator as its subject. Every algebraic-form hit under `test-data/poc/` outside `elab/` is a finding unless the file carries an explicit `// subject: <form>!'s …` comment marking the form as the local test subject.

Run the equivalent scan over `docs/` for Kiodoc fences and over `kio-rs/tests/` for any in-Rust inline `.kio` snippets:

```bash
grep -rnE '\b(iso|into|onto|align|atom|ease)!' docs/ kio-rs/tests/
```

The docs corpus is pedagogical — snippets using algebraic forms outside an algebraic-elaborator subject page teach the wrong style.

## 2. Classify hits

For each hit, decide:

- **Subject-of-algebraic-form** — the enclosing file's purpose is to assert behavior of the algebraic elaborator. Pass.
- **Pre-migration residual** — a golden that pre-dates the spine palette and hasn't been migrated yet. Flag for migration per the cheatsheet below.
- **Inadvertent algebraic use** — a newly authored golden whose subject is not the algebraic form. Flag as a discipline violation.

If the classification is uncertain (no naming signal, no explicit subject comment), list separately so the user can confirm.

## 3. How to report

Group findings by file and rule. For each:

- File path and line.
- Which algebraic form leaked (`iso!`, `into!`, …).
- The spine-form rewrite per the migration cheatsheet below; when multiple rewrites are plausible, list them in order of preference.

Skip files identified as subject-of-algebraic-form; if the classification is uncertain, list those separately.

## Migration cheatsheet

When rewriting a hit to a spine form, follow the mapping below. The mapping is derived from the call's outer-axis analysis; see `docs/poc/elab.md` (§ The spine palette, § The algebraic palette) for the per-elaborator specs.

- **`iso!(e, T)`** — reorder along the outer axis of `T`. Sum-outer → `reorder_sum!(e, T)`. Product-outer → `reorder_prod!(e, T)`. **Associativity-only rearrangement** (e.g., `(A | B) | C → A | (B | C)`, no arm-multiset change) → `flatten_sum!(e, T)` / `flatten_prod!(e, T)` per axis.
- **`into!(e, T)`** widening — `widen_sum!(e, T)`. If the call relies on like-typed duplication (source-order rule across `&`), `widen_prod!(e, T)`. When the rewrite isn't a single-axis widening (mixed-axis, recursive), `fit!(e, T)`. **`into!(x: !, T)` where the source is genuinely `!`** → `fit!(x, T)` (the spine palette's Bottom-source carve-out covers this via the initial-object law; the elaboration is the same `__absurd__` glue at the leaf). NOT `__absurd__(x)` directly — that would change the file's Kio'-grammaticality state if the file isn't already importing intrinsics.
- **`onto!(e, T)`** narrowing — `narrow_prod!(e, T)` for product narrowing (drop slots), `narrow_sum!(e, T)` for sum collapse over same-typed extras. Mixed-axis → `fit!(e, T)`.
- **`align!(e, T)`** — usually `fit!(e, T)` (the recursive composer handles align!'s same-type-collapse + new-arm widening pattern natively). Pure same-type collapse alone → `narrow_sum!`. Pure payload-preserving sum injection → `widen_sum!`. **Pure associativity rearrangement on the sum axis** → `flatten_sum!`.
- **`ease!(e, T)`** — `fit!(e, T)`. `fit!` is the spine sibling of `ease!`; it admits sum widening and product projection without value duplication, matching `ease!`'s coverage. **`ease!` paired with `iso!` for associativity reshuffle** → `flatten_sum!` / `flatten_prod!` plus `fit!` composition, with the flatten step naming the associativity move explicitly.
- **`atom!(e, T)`** — sum source with uniform-typed arms → `one_sum!(e, T)`. Product source picking the unique-typed slot → `one_prod!(e, T)`. UFCS receiver-style picker → `r.>one_prod!(T)`. **Not every `atom!` hit is a finding**: `ai/topics/surface-forms.md`'s "prefer `atom!` only for picking a single value from an aggregate" guidance is blanket, not subject-gated — that pick case (single-component projection, e.g. `r.>atom!(Field)`) is idiomatic in any golden. Subject-gating still applies to `atom!`'s more general DNF-flatten-to-nonobvious-arm case (the target only normalizes to a single arm after DNF flattening) — that generality stays confined to files whose subject is the algebraic elaborator itself.

Prefer the axis-specific spine form when a single axis suffices; reach for `fit!` when the rewrite needs recursive composition (per `ai/topics/surface-forms.md` — *single-elaborator coercions use the axis-specific form; multi-elaborator coercions use `fit!`*). When the call's purpose is associativity rearrangement on one axis, `flatten_sum!` / `flatten_prod!` is the sharpest match — the call site reads as "re-group this axis," which neither `iso!` nor `fit!` names.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md). Per-finding fix: rewrite the algebraic-form call to the spine equivalent per the cheatsheet, group fixes per golden into one commit, run the touched golden (or its bucket via `ci/checks/orchestrators/golden-tests.sh --impls=SAMPLE_IMPL -- <bucket>`) before committing.
