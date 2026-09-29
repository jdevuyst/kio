---
name: audit-skills
description: Verify load-bearing AGENTS.md sections have covering audit skills, and that each audit skill's anchor section still exists and hasn't materially drifted
allowed-tools: Read, Grep, Glob, Bash
---

# Audit-skills audit

Audit skills exist to enforce load-bearing rules in AGENTS.md and its companion topic files under `ai/topics/`. They are only as useful as the coverage they provide — if a new rule lands and no skill is added to cover it, the rule becomes implicit; if a rule is removed but a skill still references it, the skill misleads. This skill is the self-bootstrapping check that catches both gaps.

The repo's convention (per AGENTS.md design discussion) is that **skills point at AGENTS.md / ai/topics/, not the reverse**: each SKILL.md cites its anchor section, and AGENTS.md does not carry bookkeeping back-references. This audit relies on that convention.

Read AGENTS.md before starting; navigate to relevant `ai/topics/<topic>.md` files as the trigger table directs.

## 1. Enumerate load-bearing rules

Walk AGENTS.md and every file under `ai/topics/` and identify sections that read as **contracts** — sections whose violation would be considered a bug rather than a style nit. Heuristics:

- Sections whose heading or body contains "non-negotiable", "hard rule", "must never", "must", "MUST", "is non-negotiable", "is forbidden".
- Sections that name a file as a contract (e.g. `ai/topics/roadmap.md` carries explicit rules; `ai/topics/specs.md` says specs are contracts).
- The numbered structural rules sprinkled throughout (open-world checks, partial-implementation rules, surface-form removal, etc.).

Compile the list. Each entry should map to one or more audit skills.

## 2. Enumerate audit skills

List directories under `ai/skills/` matching `audit-*` (excluding `audit` itself, which is the umbrella, and excluding `audit-skills` if found — this skill's coverage of itself is degenerate). For each, read the SKILL.md and identify the AGENTS.md / `ai/topics/<topic>.md` section(s) it cites.

## 3. Coverage matrix

Cross-reference. For each load-bearing section:

- **Covered** — at least one audit skill names this section as its anchor.
- **Uncovered** — flag as a gap. The fix is either (a) extend an existing skill to cover the rule, or (b) add a new skill.

For each audit skill:

- **Anchored** — the section it cites still exists in AGENTS.md or in the named `ai/topics/<topic>.md`.
- **Stale anchor** — the section it cites has been renamed, restructured, or removed. Flag the skill for update.
- **Drifted anchor** — the section exists but its content has materially changed since the skill was written (compare the section's recent git history against the skill's git history). Surface as a candidate, since "materially" is a judgment call.

## 4. SKILL.md hygiene

For each `ai/skills/*/SKILL.md`:

- Frontmatter has `name`, `description`, `allowed-tools` at minimum.
- The `name` matches the directory.
- `description` is a single line, specific enough to convey what the skill audits.
- The body cites the AGENTS.md / `ai/topics/<topic>.md` section(s) it anchors against by section heading (so this very audit can find them).
- File references in the body point at files that exist.

Hits are findings.

## 5. Umbrella coverage

The `audit` skill (`ai/skills/audit/SKILL.md`) runs every `audit-*` skill. Per its own preamble, **every `audit-*` directory under `ai/skills/` must be referenced in `ai/skills/audit/SKILL.md`** — otherwise the umbrella silently skips that skill.

For each `ai/skills/audit-*/` directory (excluding `audit` itself):

- Grep `ai/skills/audit/SKILL.md` for the directory's name.
- If absent, flag as a finding — the audit exists but the umbrella doesn't run it.

Conversely, look for names in `ai/skills/audit/SKILL.md`'s § Order that don't correspond to a `ai/skills/audit-*/` directory. Those are stale references — the skill was renamed or removed without the umbrella being updated.

## 6. Carve-outs

A rule may be load-bearing yet not warrant an audit skill — e.g. the rule is structurally enforced by the type system, or the cost of an audit pass exceeds the value. If a skill exists that explicitly documents "this AGENTS.md rule is intentionally not auto-audited because X," that's fine. Otherwise, uncovered load-bearing rules are findings.

**Documented carve-out: [`ai/topics/language-surface-tooling.md`](../../topics/language-surface-tooling.md)'s "move the whole surface together" checklist**, anchored by AGENTS.md § Universal rules ("Language changes move the whole surface together") and its trigger-table row. The checklist spans parser/diagnostics, the formatter/style contract, the lowering boundary, typechecker rules, normalization (`equiv` / `:normalize` / elaborators), the `kio-gen-rs` generated corpus, file-kind surface parity, eight LSP request kinds, editor grammars/the VS Code extension, five `specs/*.md` contracts, public docs, and tests — most of the compiler-and-tooling surface. It has no single anchor skill because it states a per-change *impact argument* ("which surfaces must change, which are intentionally unchanged, and why"), not a static invariant: unlike `audit-prime-grammar`'s tight grammar/subset views, most checklist pairs are legitimately untouched by any single change (an LSP-hover-text fix has no reason to touch the VS Code grammar), so a blind "touched-A-without-B" git-log sweep across the whole list would be mostly false positives rather than a real signal. Existing skills anchor the static, snapshot-checkable slices:

- `audit-prime-grammar` — parser acceptance, Kio' lowering rejection, `specs/grammar.md` ↔ `specs/prime.md` agreement.
- `audit-surface-forms-survival` — lowering-boundary purity.
- `audit-open-world` — the open-world argument for resolution/import/dispatch/inference syntax.
- `audit-kio-gen-coverage` — `kio-gen-rs`'s marginal coverage contribution (not its grammar-support currency).
- `audit-spec-drift` / `audit-docs-drift` — `specs/grammar.md`, `specs/language.md`, `specs/prime.md`, `specs/style.md`, `specs/cli.md`, and their docs, drifting from the implementation over time.
- `audit-test-strategy` — spec→test coverage generally.
- `audit-cli-output` § B3 — only `kio lsp`'s printed-text output, not LSP protocol behavior.
- `audit-lsp-completion` — `language-surface-tooling.md` § Completion currency: exact lexical/contextual candidate sets, insertion/retraction, current binding identity, and evidence coverage across LSP, terminal REPL, and browser/wasm completion. Its default is static; protocol execution is opt-in or scoped fix validation.

Genuinely uncovered by any skill, even partially: LSP protocol correctness outside completion (diagnostics, formatting, semantic tokens, hover, definition, references, prepare-rename, rename) and editor grammars/the VS Code extension (tree-sitter, TextMate, bundled grammar/configuration, fixtures, highlight corpus).

**Documented carve-out: AGENTS.md § Universal rules ("Tooling friction feeds back into the instructions")** — a per-session process rule (when reality contradicts the instructions, propose an update to the owning file) whose output is session behavior, not repo state, so there is no static artifact for an audit to check. Its one checkable half — that any landed update passes the portability filter — is enforced by `audit-no-leak` against [`ai/topics/no-leak.md`](../../topics/no-leak.md) § The portability filter.

**Documented carve-out: AGENTS.md § Universal rules ("Disproportionate machinery is a design-stop signal"), [`ai/topics/implementation.md`](../../topics/implementation.md) § Complexity checkpoint for compiler design, and [`ai/topics/local-tools.md`](../../topics/local-tools.md) § Script portability** — this is a per-session judgment about whether one behavior's machinery is disproportionate, which alternatives were compared, and whether the comparison happened before expansion. A repository snapshot cannot reconstruct that decision point. Line, file, or touched-phase counts are not sound proxies: a large diff may consolidate states while a small one may add a lasting exception. [`review-commits`](../review-commits/SKILL.md) checks the observable half for compiler and repository-automation changes that plausibly add disproportionate machinery, naming lasting special state, dedicated code branches or execution paths, and cross-phase or cross-consumer machinery alongside behavioral-contract authority and authorized scope. Routine maintenance that merely preserves pre-existing machinery is outside that prospective trigger; for a change that meets it, `review-commits` treats missing checkpoint evidence in the interaction record as unverified rather than inventing it. Do not add a static size threshold for this checkpoint.

**Documented carve-out: the per-session approval and process gates** — AGENTS.md § Universal rules "Behavioral contracts cannot be rewritten retroactively", "Findings do not broaden scope", "Builtin modules are approval-gated", "Host-backend API stability is explicit and approval-gated", "Commits, sign-off, and push", "Don't bypass or break a configured compiler cache", and "Delegated and background work remains owned until reconciled", plus [`ai/topics/language-surface-tooling.md`](../../topics/language-surface-tooling.md) § Gate 0: establish contract authority. Each governs in-session conduct; what distinguishes authorized work from unauthorized behavior or scope lives partly in the user interaction, not solely in repo state, so no static sweep can fully adjudicate it after the fact. [`review-commits`](../review-commits/SKILL.md) checks the observable half by tracing behavior changes and Host API stability transitions to their effective authorized contract and reviewing scope expansion, but treats missing interaction evidence as a blocker rather than inventing it. Delegation ownership is likewise visible in the live agent/process ledger rather than the repository snapshot. [`audit-agents-md`](../audit-agents-md/SKILL.md) covers the structural routing from AGENTS.md into the lifecycle topic, while [`audit-local-running-guidance`](../audit-local-running-guidance/SKILL.md) owns semantic and polarity enforcement for the scoped authority-wait and safe-work wording; neither static check replaces the live ledger. The other checkable halves are owned elsewhere: `ci/checks/repo-lint/backend-api-stability.sh`, `audit-backends-shape`, and `audit-docs-drift` enforce the status field, mirror, default, rollout boundary, rename/removal semantics, and explicit commit record but cannot prove user approval; `audit-backend-family-conformance` rejects family-inferred status and checks shared-artifact propagation; the DCO trailer itself is enforced by the GitHub DCO check on every PR; and compiler-cache misconfiguration surfaces through the fingerprints in [`ai/topics/local-tools.md`](../../topics/local-tools.md) § Compiler cache.

## How to report

Group findings into:

1. **Uncovered load-bearing rules** — AGENTS.md sections that should have an audit skill and don't. List the section and sketch what the skill would check.
2. **Stale anchors** — audit skills whose cited section no longer exists or has been renamed. List the skill and the missing section.
3. **Drifted anchors** — audit skills whose cited section has materially changed. Surface for review.
4. **SKILL.md hygiene** — frontmatter, naming, file-reference issues.
5. **Umbrella coverage gaps** — `audit-*` skills not wired into `ai/skills/audit/SKILL.md`, or names in the umbrella that no longer correspond to a directory.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).
