---
name: audit
description: Run every audit-* skill and produce a single rolled-up report
disable-model-invocation: true
allowed-tools: Read, Grep, Glob, Bash, Skill
---

# Full audit

Run every `audit-*` skill in this repo and combine the findings into a single prioritized report.

This is the umbrella entry point: **every `audit-*` skill is intended to be invoked by `/audit`**. When a new `audit-*` skill is added, wire it into § 2 (Order) below so the umbrella keeps running it. The `audit-skills` skill cross-checks this — every `audit-*` directory under `ai/skills/` must be referenced here.

Tiers 1–6 are static-analysis skills that run quickly and have no external pre-reqs; the umbrella runs them by default. Tier 7 is harness-running skills (cargo-fuzz, cargo-mutants, cargo-llvm-cov) that take significant wall-clock time and require their respective tools to be installed locally. Tier 7 is **opt-in** — skip unless the user explicitly asked for the deep harness audit, e.g. by invoking with `--with-harnesses` or naming the harness skills directly.

## 1. Discover

List directories under `ai/skills/` matching `audit-*`, excluding `audit` itself. For each, read its `SKILL.md` to understand scope.

## 2. Order

Run the skills in an order where earlier findings inform later ones:

1. `audit-skills`, `audit-agents-md`, `audit-local-running-guidance` first — surface gaps in audit coverage, AGENTS.md structural integrity, and the local-running topic split before relying on the others.
2. `audit-no-leak`, `audit-no-future-extensions`, `audit-versioning` — mechanical and fast; surface concrete fail/pass signals.
3. `audit-github`, `audit-install`, `audit-website`, `audit-roadmap`, `audit-readme`, `audit-design`, `audit-backends-shape`, `audit-backend-family-conformance`, `audit-upgrade-deps` — repo-shape audits (`audit-upgrade-deps` covers the dependency surface).
4. `audit-spec-drift`, `audit-docs-drift`, `audit-blog-history`, `audit-host-docs-snippets`, `audit-kio-guide`, `audit-comment-quality`, `audit-prime-grammar`, `audit-cli-output`, `audit-lsp-completion` — contract / docs / chronological blog truth / Kio-authoring guide / comment / Kio'-grammar / CLI-output / completion alignment (`audit-cli-output` covers diagnostics + general output + `--help` conformance; `audit-lsp-completion` statically checks exact LSP/REPL eligibility, identity, and evidence coverage without launching protocol harnesses; `audit-host-docs-snippets` covers the host-fence gate's coverage).
5. `audit-open-world`, `audit-generated-binder-hygiene`, `audit-surface-forms-survival`, `audit-surface-forms-usage`, `audit-spine-vs-algebraic`, `audit-refl-intrinsic-constructors`, `audit-partial-implementations`, `audit-dead-elaborator-machinery`, `audit-ir-family-coverage`, `audit-compiler-performance` — deep language-invariant and compiler-performance architecture audits.
6. `audit-test-strategy`, `audit-runner-host-fidelity`, `audit-corpus`, `audit-backend-completeness`, `audit-package-coexistence`, `audit-poc-docs-sync`, `audit-runner-arg-safety` — corpus-shape audits; rely on the spec-drift and prime-grammar passes to know what should be covered. `audit-backend-completeness` builds the (shape × backend × direction) runtime-coverage matrix; `audit-package-coexistence` keeps the two-package coexistence witness honest; `audit-runner-arg-safety` checks the untrusted-contrib attack surface (run.args tokens reaching the runner, run.sh sandbox escapes).
7. **Opt-in, harness-running audits** (skip in the default umbrella run): `audit-fuzz` (cargo-fuzz, nightly Rust), `audit-mutation` (cargo-mutants), `audit-kio-gen-coverage` (cargo-llvm-cov). These take minutes to tens of minutes per invocation and surface findings that need triage, not just structural checks. Include only when explicitly requested.

## 3. Run

Invoke each skill via the Skill tool. Collect each skill's report.

## 4. Combine

Produce a single rolled-up report:

- **Severity tiers**, top to bottom:
  1. **Contract violations** — broken open-world, surface-form leaks into Kio', spec ↔ impl divergence on documented behavior.
  2. **Hard-rule breaches** — scratchpad references, missing version mirrors, backend-page shape gaps, ROADMAP weasel-words, partial-implementation carve-outs.
  3. **Drift** — docs lagging specs, recent commits without paired spec / docs / test updates.
  4. **Hygiene** — TODO sweeps, SKILL.md frontmatter issues, stale annotations.
  5. **Coverage gaps** — spec features without tests, AGENTS.md sections without audit skills.

- **Deduplicate** — if multiple skills flag the same file or rule, merge the findings.

- For each finding, cite:
  - File and line.
  - The rule / spec section violated.
  - Which audit skill surfaced it.

## 5. Report

Output the combined report.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md) — pass the directive through to each sub-skill, then produce a single combined report of what was fixed across all of them.

If a tier is empty, skip it.
