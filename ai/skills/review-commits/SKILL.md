---
name: review-commits
description: Review recent commits for behavioral authority, hygiene, pairing, portability, proportionality, partial work, and useful follow-up audits
allowed-tools: Read, Grep, Glob, Bash, Skill
---

# Review commits

Walk a recent slice of commits as a narrative pass: what landed, how it landed, what's missing, what's notable. Different from the snapshot-shaped `audit-*` skills — those ask "is the current state consistent?" This asks "is this slice of work in good shape?"

## Window selection

- **Default**: unpushed commits on the current branch (`git log @{u}..HEAD`). If the branch has no upstream, fall back to commits not present on `main` (`git log main..HEAD`); if on `main` itself with no upstream, fall back to the last seven days and say so in the report.
- **Explicit window**: if the user names a time range ("this week", "since Monday", "the last three days", "since v0.2"), translate it to a `git log` argument and use that instead. Take the user's words as authoritative — don't second-guess by also showing the default.

Name the window in the report's headline so the reader knows what slice was reviewed.

Determine a diff base separately from the authority baseline: use the left endpoint of an explicit revision range, the branch merge base for the default branch range, or the parent of the oldest selected commit for a time window. A selected review window never resets contract authority.

## 1. Collect the window's commits

```
git log <window> --pretty=format:"%h %ad %an %s" --date=short
git log <window> --stat
```

Cluster the commits by area (parser, typer, formatter, backend X, specs, docs, tests, CI, repo config). Read each commit message and its diff.

## 2. Commit hygiene

For each commit in the window:

- **DCO sign-off** — unsigned commits are normal on unpushed/local work; sign-off is added only when the user asks, immediately before a push (AGENTS.md § Universal rules — Commits, sign-off, and push). Don't flag a missing `Signed-off-by:` as a hygiene defect — just report whether the window is fully signed, since it must be before it can be pushed.
- **Message accuracy** — does the message accurately describe the diff? Watch for overclaim ("fix issue #X" when only one of X's sub-cases is handled), generic verbs that hide content ("update", "tweak"), and missing scope prefixes if the repo uses them.
- **Granularity** — is the same file touched many times across separate commits in a way that should have been one? Are there commits that should have been split (one commit doing two unrelated things)?
- **Style consistency** — subject lines follow the conventions visible in adjacent commits.

## 3. Behavioral-contract authority

This is the commit-range check for AGENTS.md § Universal rules — "Behavioral contracts cannot be rewritten retroactively."

For every normative behavior delta in the window, record:

- the exact behavior that changed;
- its authority under the effective authorized contract: the relevant specification at the start of the user-requested unit of work plus any specific user-authored instruction or approval given before implementation that deliberately superseded or settled it; and
- which nearby artifacts are supporting evidence rather than authority.

Inspect the baseline contract with `git show <authority-base>:specs/<file>`; do not judge authority from the final tree alone. Start from the diff base, then use `git log` and `git blame` on changed normative lines to find precursor spec edits belonging to the same user-requested work; extend the authority review behind the selected window and move `<authority-base>` to the parent of the earliest such edit. A commit, branch, session, delegation, or spec-only precursor does not reset the baseline. A delegated task must cite authority traceable to the user or baseline contract; its own wording is not authority. If the review has no access to that authority record, mark the delta **unverified** and blocking until the user confirms it, rather than inferring authority from the diff.

Flag a normative spec change without prior authority and a workaround that introduces a new cross-phase capability, metadata channel, cache contract, or accepted program. A specific user instruction given before implementation may supersede the baseline specification; absent that instruction, selecting between multiple contract-compatible normative behaviors is a finding. If a later user decision approves the behavior, record the original delta as unauthorized and treat the retained implementation as a proposal requiring a fresh review against the new effective contract; the later approval does not retroactively clear the process violation or waive any pairing, test, or review gate. Editorial spec clarifications and implementation details the contract leaves incidental, unspecified, or evolving do not need separate authority.

Before treating an existing encoding, artifact shape, internal version tag, or
draft protocol as compatibility authority, establish whether it was actually
released or explicitly promised stable. Migration code, compatibility framing,
legacy read/write paths, and format-version bumps require that real compatibility
obligation; an unreleased implementation experiment does not acquire one merely
because it already carries a version label.

Also classify fixes in the window under AGENTS.md § Universal rules — "Findings do not broaden scope." Flag unrelated pre-existing work added without user authorization, including issue, ROADMAP, or other tracker mutations; a review or audit finding alone does not put that work in scope.

### Host API stability transitions

For every host-backend API change, read the backend's status at the authority
baseline and classify the changed surface against
`specs/backends/README.md` § Host API stability. The protected surface includes
target/configuration keys, public output and loading entry points, namespaces
and handles, exposed names and types, call stages, documented FFI
representations and runtime interaction, and the documented host-language or
runtime floor. Do not treat private helpers, cached generated artifacts,
unspecified layout, or an intentional `kio sig` package-contract change as a
backend API break.

- A compatibility break while the backend is `stable` is blocking. An
  explicitly approved demotion must already have been published before the
  break; approval of the break alone does not override `stable`.
- A compatibility break while the backend is `evolving` still needs explicit
  user approval for that concrete break. The value is not standing authority.
- Promotion and demotion each need explicit prior user approval, matching spec
  and host-guide fields, and the exact commit footer trailer
  `Host-API-Stability-Transition: <backend> <old> -> <new>`. Promotion fixes the
  then-documented API as the non-retroactive compatibility floor. A shared
  emitter/runtime change must satisfy every `stable` backend it reaches or use
  a compatibility-preserving split.
- A backend spec/guide rename is removal of the old ID plus addition of the new
  ID, regardless of Git's similarity result. The old status does not transfer:
  a stable ID needs an earlier published demotion, and the new ID starts
  `evolving` with its field present in the addition commit.
- On a merge, a matching policy-bearing parent can supply an already-validated
  state. A weakening or removal cannot inherit from a stale branch that
  predates the stable state; a state made only by merge resolution is new in
  that merge and needs the same transition or removal proof.

The repository lint proves the mechanical field/default/mirror/trailer facts;
the review proves authority from the user-interaction record. Status never
supplies evidence of quality, correctness, completeness, or conformance.

For compiler changes, enforce AGENTS.md § Universal rules — "User declarations
never become compiler primitives by name." Inspect new or modified semantic
branches in parsing/desugaring, typing/substitution, recursive lowering,
structural recovery including `recover_to_low`, capability annotation,
evaluation/optimization, and backends for hard-coded user-spellable names,
constants naming shipped Kio libraries, comparisons against compiler-hard-coded
or otherwise distinguished ordinary-library identities, and raw source argument
indices used as a remembered library ABI. Block any typing, lowering,
tail-position, evaluation, visibility, capability, or argument-selection
behavior selected that way. Replacing a bare name with a hard-coded fully
qualified library identity is not a repair. Generic use of the identity selected
by the resolved program remains legitimate for exact capture/visibility-edge
validation, nominal equality, ordinary dispatch, host linking/backend naming,
diagnostics, and caching. Require general structural semantics or an explicitly
authorized language construct, plus a same-spelling unrelated negative control,
a differently named or rehomed structural positive control (or a renamed
declaration carrying the authorized explicit capability), and every applicable
qualified/selective and prefix/UFCS route.

### Complexity checkpoint

For a compiler-design or repository-automation delta, also inspect the observable half of AGENTS.md § Universal rules — "Disproportionate machinery is a design-stop signal" — together with [`ai/topics/implementation.md`](../../topics/implementation.md) § Complexity checkpoint for compiler design or [`ai/topics/local-tools.md`](../../topics/local-tools.md) § Script portability, as applicable. When the reviewed change adds or materially expands machinery for one narrow behavior, and that machinery plausibly appears disproportionate to the contract, name the lasting special state, dedicated code branch or execution path, and cross-phase or cross-consumer machinery it adds or expands. Compare that shape with simpler representations or contract-compatible designs, including an existing external supervisor for lifecycle automation, and check cache and performance consequences plus deletion and consolidation opportunities. Do not use line, file, or touched-phase counts as a threshold: a small diff can add a lasting exception, while a large diff can remove states or paths. Routine maintenance that merely preserves pre-existing state or paths does not trigger this checkpoint and is not made unverified by a missing historical interaction record.

For a change that meets that trigger, inspect the available user-interaction and delegation record for evidence that the checkpoint occurred before the reviewed change expanded the machinery and, when the contract was ambiguous, that the selected design was authorized. If that record is absent or does not establish the checkpoint, mark the process half **unverified**; do not infer it from a polished final diff. Report separately whether the observable implementation is justified by the effective contract and remains within authorized scope.

## 4. Pairing discipline

For changes touching signature replay, host descriptors, retained facade
planning, or deprecated emission, review the complete retained closure for the
four coupled properties: emitted history-only declarations are visibly
deprecated, optional for current hosts (including type selection), inert in
live loader/runtime/package paths, and live-dominant when provenance overlaps.
Check incompatible same-identity declaration epochs explicitly: live wins, and
affected retained roots and dependencies are omitted rather than merged,
widened, or selected by replay order. Where a target cannot satisfy the four
properties, require omission plus the documented source-compatibility caveat
rather than a dead host obligation.

AGENTS.md ties many implementation changes to specific spec / docs / tooling updates. For each cluster in the window, check the matching update happened:

- **Parser / lexer** (`kio-rs/src/pass/parser/`, `kio-rs/src/pass/lexer.rs`) ↔ `specs/grammar.md`.
- **Typer** (`kio-rs/src/pass/typecheck_core/`, `kio-rs/src/pass/typecheck_full.rs`, `kio-rs/src/prime/`) ↔ `specs/language.md` / `specs/prime.md`.
- **Formatter** (`kio-rs/src/cmd/fmt.rs`, `kio-rs/src/kiodoc/fmt.rs`, `kio-rs/src/lsp/formatting.rs`) ↔ `specs/style.md`.
- **CLI** (`kio-rs/src/main.rs`, `kio-rs/src/cmd/`, `kio-rs/src/bin/kio-prime.rs`) ↔ `specs/cli.md` (excluding internal debug flags, which should carry the no-impl-in-spec comment).
- **Exit codes** (any `process::exit` change) ↔ `specs/exit-codes.md`.
- **Backends** (`kio-rs/src/backends/<lang>*`) ↔ `specs/backends/<lang>.md`.
- **Package files** (`kio-rs/src/package*`) ↔ `specs/package.md`.
- **New manifest** (any `Cargo.toml`/`package.json`/etc. added) ↔ `ci/checks/repo-lint/version-check.sh` mirror list.
- **New `.github/workflows/*.yml`** ↔ ai/topics/repo-layout.md naming.
- **New `specs/backends/*.md`** ↔ ai/topics/specs.md.
- **New `specs/backends/*.md`** ↔ matching `docs/hosts/*.md` with equal `Host
  API stability: evolving` fields.
- **Host API stability transition** ↔ matching spec/guide values, the exact
  transition footer trailer, prior user approval, and no forbidden break in the same
  change.
- **`specs/` change** ↔ `docs/` page that teaches it (per ai/topics/docs.md, "`docs/` may lag in coverage but never in correctness").
- **New language feature** ↔ golden in `test-data/goldens/` (and per `TESTING.md`, possibly generative).
- **Repository automation** ↔ applicable-platform classification and evidence per `local-tools.md` § Script portability; a changed shared path must not deepen a known OS-specific dependency.

For each commit cluster, list the *expected* pairings and the *observed* ones. Mismatches are the primary finding.

### Bulk corpus migration evidence

For broad source rewrites, apply the classification and evidence requirements
in [`ai/topics/local-ci.md`](../../topics/local-ci.md) § Proof-carrying bulk
corpus migrations. Changed-file ownership is not behavioral classification.
Verify the canonical-owner map, transformation-appropriate equivalence evidence,
preserved case contracts and explicit treatment of uncertain rows. Classify
actual compiler interactions per slice; neither a shared batch nor unchanged
expected output alone establishes the answer. Syntax/formatter-subject changes
remain behavioral; ordinary adopters may be mechanical migrations.

Check that genuine new/behavioral goldens have exact full-matrix, all-case
evidence, and migrations have bounded risk representatives plus normal harness
coverage. Apply the owning policy's risk-based sampling rules, including for
dynamic Prime; do not manufacture a full-leaf obligation from touched-file
counts. Reuse valid results and scope invalidation to what later changes affect.
Missing classification evidence or an unrepresented interaction is a finding
requiring diagnosis, not automatic full-matrix coverage of the inventory.

Review the aggregate case × implementation × phase scope across commands.
Effectively corpus-wide full-matrix work needs the policy's explicit user
direction and risk argument even when written as exact lists or split runs.
Flag disproportionate coverage plans as well as insufficient coverage; an
agent-authored plan is not authority to enlarge the gate.

## 5. Partial implementations introduced

Look at the diff for the window:

- New `TODO` / `FIXME` / `XXX` / `for now` / "residual" / "stays as" comments added by the slice's commits. (See `audit-partial-implementations` for the full phrase list.)
- Commit messages claiming "fix issue #X" — verify the full issue is handled, not just a slice. If incremental, the commit message should say so.
- New `unreachable!` / "not supported" arms without a spec rule backing them.

## 6. ROADMAP and authorization

Check `ROADMAP.md` changes in the window:

- Threads that settled into `specs/` should have had their ROADMAP entry removed in the same commit. Flag mismatches.
- New ROADMAP entries — does the commit message reference the explicit authorization ai/topics/roadmap.md requires?

## 7. Scope / velocity smells

- Files touched many times by independent commits — could indicate insufficient planning or branch-management friction.
- Commits that look like rebase residue (empty bodies, repeated subjects, very small diffs in unrelated files).
- (When reviewing a time-window rather than unpushed work: branches present locally but unpushed for longer than the window — surface for triage rather than judgment.)

## 8. Smart routing to audits

If the window's commits touched specific areas, suggest running the matching audit skill *after* the review:

- Touched name resolution or feature design? → `audit-open-world`.
- Touched the front-end pipeline or Prime AST? → `audit-surface-forms-survival`.
- Added `*.kio` corpus or docs snippets? → `audit-surface-forms-usage`.
- Touched specs without docs/, or vice versa? → `audit-spec-drift`, `audit-docs-drift`.
- Touched `.github/` or `ci/`? → `audit-github`.
- Bumped versions or added a manifest? → `audit-versioning`.
- Touched `specs/backends/*`? → `audit-backends-shape`.
- Added tests or features without tests? → `audit-test-strategy`.

Don't run them automatically — recommend them in the report so the user can choose.

## 9. Working-tree context

Surface (don't act on):

- Uncommitted changes (`git status`).
- Local branches with commits not present on `main` (when reviewing a time-window — for the default unpushed scope, this is what was already reviewed).

This is context for the user, not findings — they may be in-progress on purpose.

## How to report

Structure the report as a narrative, not a flat list. Suggested shape:

1. **Headline** — one sentence: which window was reviewed, how many commits, what areas, anything striking.
2. **What landed** — bulleted summary of the clusters with a line each.
3. **Hygiene** — sign-off status, message accuracy, granularity issues.
4. **Contract-authority and scope ledger** — each normative behavior delta and its authority, any unverified/unauthorized blocker, and any finding that expanded scope.
5. **Pairing gaps** — implementation that landed without its expected spec / docs / test / version-mirror counterpart.
6. **Partial work introduced** — TODOs, carve-outs, overclaimed fixes.
7. **ROADMAP / authorization** — settled threads still listed, new entries without paper trail.
8. **Recommended follow-up audits** — which `audit-*` skills are worth running now.
9. **Working-tree context** — what else is in flight locally.

Cite commits by short hash + subject.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md) — apply fixes for the pairing gaps and partial-work findings, then produce the report.
