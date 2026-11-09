---
name: audit-dead-elaborator-machinery
description: Verify elaborator machinery is reachable from spec-current syntax and that Kio-authored macros receive no compiler privilege from a user-controlled name or library identity
allowed-tools: Read, Grep, Glob, Bash
---

# Dead elaborator-machinery audit

Audit the compiler for obsolete compiler-private elaboration paths and for
privilege granted to Kio-authored macros by name or library identity. The
current surface uses ordinary resolved `Expr::UserMacro` declarations;
native field elaboration has no producer or compiler-private enum. Ordinary
nominal declarations and post-typecheck `Expr::Enriched*` recovery are live
and are not native field elaboration.

Read AGENTS.md § Universal rules — No partial implementations, Bugs surface;
never hide them, and User declarations never become compiler primitives by
name before starting.

## 1. Trace obsolete elaboration paths

Search `kio-rs/src` for `ElaboratorKind`, `ElaboratorCall`,
`Expr::Elaborator`, former field-access/update preparation and scheduling
helpers, and any replacement scaffolding. Check both construction sites and
read-only traversal or test sites. A surviving implementation path without
spec-current syntax is a finding even if tests or clone walks keep it used.
Trace exclusive helpers, cache/serialization handling and tooling to the last
consumer; preserve ordinary user-macro scheduling, compact nominal lowering,
and structural recovery for ordinary projector calls.

For an authorized builtin-reflection removal, also trace public-name tables,
schemes, evaluator dispatch, projected-value fields, traversal helpers and test
constructors to remaining real consumers. Flag compatibility wrappers or
reflection-only metadata kept alive by tests. Distinguish these from live
body-binding, host-interface and kind-arity metadata using
[the reflection boundary](../../topics/macros.md#semantic-function-reflection).
Lack of a current library caller alone does not authorize deleting a public API.

## 2. Verify live macro reachability

Trace a real imported macro call through parser, lexical resolution,
projection, typing, substitution and Prime validation. Use existing focused
behavioral tests and confirm that the intended operation executes; a synthetic
constructor or read-only walk does not establish a live source route. If
instrumentation is needed, use the local CI and compiler-cache guidance, and
remove the probe after use. Bounded zero firing alone does not prove deadness.

## 3. User-macro name and identity privilege

Kio-authored macros are ordinary imported declarations. Audit every
semantic pass that handles `Expr::UserMacro` or macro UFCS calls, and every
hard-coded constant, string, or distinguished identity matching a shipped
elaborator. Include the parser/desugarer, typer, substitution, recursive
lowering, structural recovery (including `route`), capability
annotation, evaluator, optimizer, and every backend; do not limit the sweep to
the old elaborator enums or to files changed by the triggering commit.

Classify each use of a macro name or resolved declaration identity:

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
macro with the tempting spelling remains ordinary, while the intended
general behavior also applies to a differently named or rehomed macro with
the same relevant ordinary structure. If an authorized explicit capability is
selected instead, vary the name while preserving the capability. The intended
library behavior must also survive the reference and call spellings the
language admits (qualified/selective imports and prefix/UFCS where applicable).
A corpus case that calls only the canonical shipped name is non-discriminating
and does not clear the finding.

## 4. Report

Report obsolete producer, consumer and helper paths separately from live
ordinary-macro and nominal paths. For every name/identity privilege finding,
identify the behavior it grants and the missing negative-control case. Cite
source evidence and distinguish static reachability from executed coverage.
Default is report-only; under a fix-it directive, follow
[`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).
