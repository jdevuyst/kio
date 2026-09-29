---
name: audit-dead-elaborator-machinery
description: Verify elaborator machinery is reachable from spec-current syntax and that Kio-authored elaborators receive no compiler privilege from a user-controlled name or library identity
allowed-tools: Read, Grep, Glob, Bash
---

# Dead elaborator-machinery audit

The compiler carries an internal enum pair for elaborator positions:
[`ElaboratorKind`] (the kind tag) and [`ElaboratorCall`] (the per-kind
payload), both in `kio-rs/src/ast.rs`. Each variant is meant to back a
real surface elaborator. A variant can rot into dead scaffolding —
constructed only by `#[cfg(test)]` fixtures and traversed only by
read-only walks (resolve, desugar clone-walks, pretty, tokens, LSP,
equiv-cache) — while still **compiling clean**: the read-only walks and
test constructors keep `dead_code` quiet, so no mechanical lint fires.

This is the gap that let a built-in `match!` apparatus (`ElaboratorKind`
/ `ElaboratorCall` `Match` variants plus `synth_match_elaborator`)
survive after the surface `match!` migrated to a Kio-authored user
elaborator (`match.kio`, imported via `use match from match;`). A
spelling/keyword/grammar sweep is structurally blind to it: the residue
is internal Rust enum identifiers, not user-facing syntax, and a
name-based sweep cannot tell "variant backing a removed built-in" from
"variant backing a surviving feature" — both read `::Match`. The
deadness is only provable by reachability-from-real-input plus empirical
firing coverage. This skill standardizes those two checks.

Read AGENTS.md § Universal rules — No partial implementations, Bugs surface;
never hide them, and User declarations never become compiler primitives by
name before starting. Dead scaffolding reachable only through test constructors
and read-only walks is a no-partial-implementations breach; semantic dispatch on
a Kio-authored elaborator's name is a user-declaration-privilege breach.

## What this catches vs. does not

The built-in `match!` apparatus was Apparatus A. Do **not** confuse it
with [`Expr::EnrichedMatch`] (Apparatus B) — the live post-typecheck
sum-dispatch IR produced by `structural_recovery`, carried by
`recover_to_low`, optimized by `optimize.rs`, and emitted by the Rust /
JS backends. `EnrichedMatch` is a different node, has firing coverage
through every sum elimination, and is not in this audit's scope. A
finding here is an `ElaboratorKind` / `ElaboratorCall` variant, never an
`Expr::Enriched*` variant.

## 1. Enumerate the variants

List every variant of both enums:

```sh
grep -n 'enum ElaboratorKind\|enum ElaboratorCall' kio-rs/src/ast.rs
```

Read the two `enum` bodies. For each variant, record its name.

## 2. Surface → IR mapping table

Maintain (in this section, updated as the surface evolves) the mapping
from each variant to the **spec-current** surface form that produces it.
Any variant with no spec mapping is a finding.

| Variant | Surface form (spec-current) | Notes |
| --- | --- | --- |
| `ElaboratorKind::Access` / `ElaboratorCall::FieldAccess` | `x.?{foo}` postfix field access | Compiler-private; parser-produced |
| `ElaboratorKind::Filtered` / `ElaboratorCall::FieldUpdate` | `x.!{foo = y}` postfix field update | Compiler-private; parser-produced |

There is deliberately **no** `Match` row: the surface `match!` is the
imported Kio-authored user elaborator, carried by `Expr::UserElaborator`
(name `"match"`), typed through `synth_user_elaborator`. It does not back
any `ElaboratorKind` / `ElaboratorCall` variant. If a `Match` variant
reappears in either enum, that is the regression this skill exists to
catch.

There is likewise deliberately **no** `Iso` / `Into` / `Onto` / `Align` /
`Ease` / `Atom` / `Fit` / `Derive` row: the spine/algebraic elaborator
palette (`iso!` / `into!` / `onto!` / `align!` / `ease!` / `atom!` /
`reorder_*!` / `narrow_*!` / `widen_*!` / `flatten_*!` / `one_*!` /
`fit!`) and `derive!` are, same as `match!`, imported Kio-authored user
elaborators carried by `Expr::UserElaborator` and typed through
`synth_user_elaborator`. They do not back any `ElaboratorKind` /
`ElaboratorCall` variant. If any of those variants reappear in either
enum, that is the same class of regression.

Cross-check the table against `specs/grammar.md` (bang-calls are
`IDENT '!'` token conjunctions — there is no reserved elaborator-name
terminal — and the relevant productions are `FieldAccess` /
`FieldUpdate`). A variant in the enum with no row, or a row whose
surface form is no longer in `specs/grammar.md`, is a finding.

## 3. Reachability: originate vs. destructure-and-rebuild

For each variant, list its construction sites:

```sh
grep -rn 'ElaboratorCall::<Variant>\|ElaboratorKind::<Variant>' kio-rs/src --include='*.rs'
```

Classify every site as one of:

- **Originating** — a front-end pass that builds the node from a
  *non-Elaborator* input (the parser, or a desugar that rewrites some
  other surface form into this variant). At least one originating site
  is required.
- **Read-only walk / clone** — resolve, desugar clone-walks
  (`walk.rs`), `op_fold`, `label_elab`, pretty, tokens, equiv-cache,
  LSP. These traverse a pre-existing node; they originate nothing.
- **Test-only** — `#[cfg(test)]` constructors and test helpers.
- **Self-referential-dead** — a site whose constructor merely
  *destructures an already-existing same-variant node and rebuilds it*
  (the rec/CPS desugar pattern: match an input `Variant { .. }` and emit
  a `Variant { .. }` with the fields copied through). Such a site
  originates nothing — it only re-processes a node some other pass must
  have produced.

**Finding:** a variant whose construction sites are *exclusively*
read-only / test-only / self-referential-dead. Such a variant cannot be
reached from real input — the parser never originates it and no desugar
turns another form into it — so its synth/elaborate entry point is dead.
This is exactly the Apparatus-A shape: the only "producers" of
`ElaboratorCall::Match` destructured an input `Match` node nothing
originates, and the parser produced `Expr::Elaborator` only as
`Access` / `Filtered`.

A quick discriminator for the self-referential-dead case: if removing
the variant turns a desugar arm into a compile error whose fix is pure
deletion (the arm's only job was to copy the node through), the arm was
self-referential-dead.

## 4. Empirical firing gate

The reachability analysis in § 3 is static. Confirm it empirically with
the procedure that originally exposed Apparatus A: build kio with a
hard abort in each elaborator synth entry point, run the corpus, and
fail if any synth fn fires zero times.

The synth entry points are the per-variant arms in
`kio-rs/src/pass/typecheck_full.rs` `synth_expr` / `check_value_against`
under `Expr::Elaborator { call, .. }` (the `ElaboratorCall::FieldAccess`
→ `synth_field_access`, `ElaboratorCall::FieldUpdate` →
`synth_field_update` dispatch), plus `Expr::UserElaborator` →
`synth_user_elaborator` for the path that backs `match!` and other
imported elaborators.

Procedure (manual one-off; promote to a standing probe when convenient):

1. In each synth entry point, insert an env-gated abort, e.g.:

   ```rust
   if std::env::var_os("KIO_DEBUG_ELAB_FIRE_ABORT").is_some() {
       eprintln!("FIRED: synth_<variant>");
       std::process::abort();
   }
   ```

   Use a counter or a per-fn marker file instead of `abort()` when you
   want the run to continue and tally firings across the whole corpus
   rather than stopping at the first.

2. Use the configured compiler cache per [`ai/topics/local-tools.md`](../../topics/local-tools.md)
   § Compiler cache, then build kio with the probe and run the full
   surface corpus plus the lib suite:

   ```sh
   sh ci/checks/orchestrators/golden-tests.sh --impls=FULL_IMPL_MATRIX
   ( cd kio-rs && sh ../ci/cargo.sh test --lib --all-features )
   ```

   (See [`ai/topics/local-ci.md`](../../topics/local-ci.md) for scoping;
   `00_success` and the elaborator buckets are the load-bearing corpus.)

3. **Finding:** any synth entry point that fires zero times across the
   whole run has no covering test reaching it via a spec-current surface
   spelling. Either the variant is dead (delete it, per § 3) or the
   corpus is missing a golden for a live shape (add the golden — the
   missing golden is itself the bug, per AGENTS.md Bugs surface).

   The gate's discriminating power is the point: the surviving imported
   `match!` fires `synth_user_elaborator` (hundreds of times across the
   `match!` goldens), while a dead built-in `synth_match_elaborator`
   fires zero. A name-based sweep cannot make that distinction; the
   firing gate can.

4. Revert the probe (it is a throwaway instrumentation build, not a
   checked-in change; `KIO_DEBUG_*` naming follows AGENTS.md
   Debug-only surfaces are internal if you ever keep a counter version).

## 5. Mechanizable now vs. documented procedure

- §§ 1–3 (enumerate, surface→IR mapping, reachability classification)
  are mechanizable from `grep` over `kio-rs/src` plus the mapping table
  here — run them every pass.
- § 4 (empirical firing gate) is documented as a precise manual
  procedure. It requires an instrumented build, so it is opt-in rather
  than part of the default static sweep. Run it when § 3 flags a
  suspect variant, or when an elaborator surface form is removed /
  migrated (e.g. a built-in becoming a user elaborator) — that
  migration is exactly when a variant goes dead while still compiling.

## 6. Related dead-built-in paths

When a surface elaborator migrates from a built-in to a Kio-authored
user elaborator, the *whole* built-in apparatus for it goes dead at
once: the `ElaboratorKind` / `ElaboratorCall` variants, the synth fn,
the per-clause / decision-tree helpers it alone called, and any
factor-list engine entry kept "for symmetry" with it. After deleting a
variant, follow the compiler's `dead_code` cascade — deleting the synth
fn surfaces its now-unused helpers, deleting those surfaces theirs.
Delete to fixpoint; an `#[allow(dead_code)]` left behind to silence the
cascade is itself a finding (it re-hides what this audit exists to
surface).

## 7. User-elaborator name and identity privilege

Kio-authored elaborators are ordinary imported declarations. Audit every
semantic pass that handles `Expr::UserElaborator` or bang-UFCS calls, and every
hard-coded constant, string, or distinguished identity matching a shipped
elaborator. Include the parser/desugarer, typer, substitution, recursive
lowering, structural recovery (including `recover_to_low`), capability
annotation, evaluator, optimizer, and every backend; do not limit the sweep to
`ElaboratorKind` or to files changed by the triggering commit.

Classify each use of an elaborator name or resolved declaration identity:

- **Ordinary identity use** — following the exact identity selected by the
  resolved program: resolving an import; validating the selected capture or
  visibility edge; comparing nominal identities; ordinary call/member dispatch;
  host linking and backend symbol/namespace selection; rendering a diagnostic;
  or keying a cache. These generic uses are not semantic privilege.
- **Grammar/intrinsic use** — implementing a grammar form or reserved intrinsic
  whose compiler semantics are explicit in the specification. Record the exact
  authority; a familiar library name is not enough.
- **Semantic privilege — FINDING** — changing typing, lowering, recursive-tail
  positions, evaluation, visibility, capabilities, or argument interpretation
  because a bare name matched or because the resolved identity equals a
  compiler-hard-coded or otherwise distinguished ordinary-library declaration.
  Treat raw source-argument indices tied to that library's remembered ABI as the
  same finding.

Do not discharge a finding by changing a bare-name comparison to a hard-coded
qualified module/item comparison. The library declaration remains ordinary.
This does not condemn a generic identity-keyed table or equality check whose key
comes from the resolved program rather than a compiler-maintained list of
privileged library declarations. The repair must use general elaborated
structure or an explicitly authorized language construct/capability.

For each semantic path, require discriminating evidence: an unrelated
elaborator with the tempting spelling remains ordinary, while the intended
general behavior also applies to a differently named or rehomed elaborator with
the same relevant ordinary structure. If an authorized explicit capability is
selected instead, vary the name while preserving the capability. The intended
library behavior must also survive the reference and call spellings the
language admits (qualified/selective imports and prefix/UFCS where applicable).
A corpus case that calls only the canonical shipped name is non-discriminating
and does not clear the finding.

## 8. Report

For each enum variant: its surface mapping (or "no spec mapping —
FINDING"), its construction-site classification (originating site
present, or "exclusively read-only/test/self-referential — FINDING"),
and — when § 4 was run — its firing count. Report every name/identity privilege
site separately, including the behavior it grants and which negative-control
case is missing. Cite file and line for every finding. Default is report-only;
under a fix-it directive, follow
[`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md) and
delete dead variants to fixpoint plus add any missing golden for a live
shape.

[`ElaboratorKind`]: ../../../kio-rs/src/ast.rs
[`ElaboratorCall`]: ../../../kio-rs/src/ast.rs
[`Expr::EnrichedMatch`]: ../../../kio-rs/src/ast.rs
