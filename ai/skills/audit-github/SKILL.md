---
name: audit-github
description: Check .github/ and ci/ consistency — workflow→script references, CONTRIBUTING/template mirror set, CODEOWNERS, ci/ bucket organization, least-privileged pull_request_target workflows
allowed-tools: Read, Grep, Glob, Bash
---

# GitHub config audit

`.github/` and `ci/` together carry the repo's automation contract. ai/topics/repo-layout.md describes both. This skill verifies they remain internally consistent and consistent with the documented organization.

## 1. Workflow → script references

For each `.github/workflows/*.yml`:

- Every script the workflow invokes (typically `sh ci/<bucket>/<name>.sh` or `sh ci/all.sh`) must exist at that path.
- Every CI bucket the workflow references must match the documented buckets in ai/topics/repo-layout.md (`ci/checks/orchestrators/`, `ci/checks/per-case/`, `ci/checks/repo-lint/`, `ci/checks/hygiene/`).
- Linux runs every gating script inside a single `ci/all.sh` job; macOS / Windows run an explicit portability subset of named steps in one job per OS, per ai/topics/repo-layout.md.
- Tool installation versions match what ai/topics/local-ci.md says ("Tool requirements match what the root `mise.toml` / `mise.lock` pins."). If the README or AGENTS.md names specific versions, confirm.
- Trace scheduled and default manual coverage through the effective `ci/all.sh` argv to each corpus owner: implementation sampling and case sampling are independent axes. Check actual selectors, caps, fixed verifiers, and narrowed-out invariant routing against [`local-ci.md`](../../topics/local-ci.md), not step names or labels. Use executable forwarding evidence and bounded case/implementation counts when the wiring is uncertain.
- Account for generated-program count separately from corpus case sampling. Keep the dynamic-Prime verifier's smoke policy distinct from ordinary loader goldens and the loader POC, which follow their own corpus policies.
- Tool provisioning covers every mandatory check's dependencies, including availability-driven host tools outside a runtime selection. A command or shim on `PATH` does not prove that the repository-pinned version is installed; trace the installation closure before the checks run.

Conversely, every script under `ci/` should be reachable from `ci.yml`. Scripts under `reports/` are skill-driven (not invoked from any workflow today) and only need to be reachable from their corresponding `audit-*` skill under `ai/skills/`.

## 2. Contribution-policy mirror set

`CONTRIBUTING.md`, `.github/ISSUE_TEMPLATE/*`, `.github/PULL_REQUEST_TEMPLATE.md`, `.github/workflows/pr-auto-close.yml`, `.github/workflows/pr-policy-check.yml`, `.github/workflows/contrib-run.yml`, and `test-data/contrib/README.md` form a single policy mirror set — see [`ai/topics/contributing.md`](../../topics/contributing.md) for the canonical statement of what propagates between them. Read all seven and check:

- The required-fields and checklists in the templates match what `CONTRIBUTING.md` says contributors must do.
- DCO sign-off requirement is mentioned where appropriate.
- Any process change in `CONTRIBUTING.md` (acceptance policy, contrib-lane shape, reproducer shape) is reflected in the templates.
- `pr-auto-close.yml`'s triggers, grace period, bot-author exemption, and contrib-lane exemption (path prefix + **either** a linked issue **or** a follow-up to a case directory that already exists on the base branch) match what `CONTRIBUTING.md` and the PR template say will happen to PRs, sanctioned and unsanctioned.
- Any human-readable statement of the automation's **timing** — the PR template's and `CONTRIBUTING.md`'s "immediately when it's opened or updated" / "weekly scan (Mondays at 06:00 UTC)" / "at least 48h old" — matches the actual mechanics: the `pull_request_target` event `types:` (opened/reopened/synchronize — note there is no `edited`, so "open/edit" phrasing is wrong), the `schedule:` cron (`0 6 * * 1` = Monday 06:00 UTC), and the grace-period constant (`ageThresholdMs`) in `pr-auto-close.yml`. A prose day/time or "immediately on _event_" claim that names an event the workflow doesn't fire on, or a cron the workflow doesn't use, is drift.
- The contrib-lane naming scheme (`<github-username>-<issue#>`) and case contract agree across `test-data/contrib/README.md`, `CONTRIBUTING.md`, the "Contribute an example program or library" issue template, and the contract validation in `ci/checks/orchestrators/contrib-tests.sh`.
- `pr-policy-check.yml` and `pr-auto-close.yml` encode the same _keep-open_ decision — contrib path prefix; a linked issue **or** a follow-up to a case dir already on base; or the `accepting prs` label. The **follow-up exemption in particular must match**, or a valid follow-up passes policy-check yet gets auto-closed (that exact mismatch was a real bug). `pr-policy-check.yml` additionally enforces owner-only follow-ups (dir owner == PR author); `pr-auto-close.yml` deliberately does not gate on ownership (a non-owner follow-up gets a failing check but isn't spam, so it stays open). The file-level contract subset `pr-policy-check.yml` validates matches what `contrib-tests.sh` enforces.
- The **execution-file rule** (exactly one of `run.args` / `run.sh` / `run.test-only`, no other `*.sh`) and the **file allowlist** agree across `contrib-tests.sh`'s `validate_contrib_contract`, `pr-policy-check.yml`'s `ALLOWED_FILES`, and `ci/run-tests.sh`'s `assert_case_run_contract`. The **`test-contrib` run flow** is consistent: `pr-policy-check.yml` strips `test-contrib` on `synchronize` and warns on a `run.sh` / `KNOWN_FAILING` case; `contrib-run.yml` fires only on the `test-contrib` label, re-checks the label-adder's write permission, **requires the PR to already carry the `contrib case` label** (so it runs only what `pr-policy-check` validated, not merely what a maintainer clicked), **re-asserts that every changed file is under `test-data/contrib/`** at the run SHA (it executes repo scripts — `ci/impl-toolchain.sh`, `contrib-tests.sh` — from the PR checkout, so a non-lane-confined PR must never reach the run steps), and checks out the approved head SHA. Label names are identical across the workflows. **Dropping either the `contrib case` gate or the confinement re-check is a security finding, not mere drift** — the runner would then execute attacker-controlled repo scripts under the base-repo token.
- **`pr-policy-check.yml` accepts every valid case** — it must not reject a case the corpus considers valid. Cross-check its rules against the live corpus and the canonical validator: every top-level filename that appears in any `test-data/contrib/*/` case is in `pr-policy-check.yml`'s `ALLOWED_FILES`; every contract clause `contrib-tests.sh`'s `validate_contrib_contract` enforces (exec-file set, `KNOWN_FAILING`, library-`equiv`, empty `expected.stdout` for a `run.test-only` library, stderr policy, `<pkg>.pkg.kio`, owner naming) has a matching clause in `pr-policy-check.yml`, and vice-versa. A clause or allowed file added to the corpus/validator but missing from the PR check would false-reject a valid contributor PR — a finding. This is the freshness guard for the PR-checking script.
- **Least privilege on the `pull_request_target` workflows.** `pr-policy-check.yml`, `pr-auto-close.yml`, and `contrib-run.yml` run with the base-repo token against untrusted fork PRs, so each `permissions:` block must grant only what the workflow's API calls actually use. Cross-check the declared scopes against the `github.rest.*` / `github.graphql` calls: label and comment mutations go through the **issues** API (`issues: write`), `pulls.listFiles` / `getContent` need only **read**, and `pulls.update` (the close) needs `pull-requests: write`. A `write` scope with no corresponding write call is over-broad — flag it (e.g. a policy-check that only labels/comments should declare `pull-requests: read`). The check is bidirectional: a call whose scope is **not** granted is under-privileged and 403s at runtime — e.g. reading a PR's labels via `issues.listLabelsOnIssue` needs `issues: read`, whereas `pulls.get` returns the same labels inline under `pull-requests: read`; prefer the latter so a runner that touches no issues can hold no `issues` scope. Also confirm each such workflow's checkout, if any, uses `persist-credentials: false` and no `actions/checkout` runs before the confinement gate.

## 3. CODEOWNERS

Read `.github/CODEOWNERS`. Verify:

- Every path pattern resolves to at least one actual file or directory.
- Every team / user mentioned exists (cannot fully verify locally — flag any that look suspicious).
- The patterns cover the major repo regions (specs/, kio-rs/, ci/, docs/, test-data/).

## 4. CI bucket organization

ai/topics/repo-layout.md commits to gating buckets under `ci/checks/` (`orchestrators/`, `per-case/`, `repo-lint/`, `hygiene/`), with the support-crate sources under `ci/infra/` (one directory per crate, no further nesting); plus a top-level `reports/` directory for the reporting harnesses. Verify:

- Every gating script under `ci/` lives in one of the `ci/checks/<bucket>/` directories; entrypoints (`ci/all.sh`, `ci/run-tests.sh`, `ci/cargo.sh`) live at the top of `ci/`.
- Each bucket's contents match its stated purpose.
- `ci/all.sh` discovers every gating script and invokes it.
- The `reports/` directory holds the reporting harnesses (skill-driven; not paired with any workflow) and the `reports/all.sh` umbrella. `reports/` is deliberately a sibling of `ci/`, not a child — `ci/` holds gating scripts, `reports/` holds analyses for human triage.

## 5. Other workflows

ai/topics/repo-layout.md names `ci.yml`, `devcontainer-publish.yml`, `pages.yml`, `markdown-link-check.yml`, `pr-auto-close.yml`, `pr-policy-check.yml`, `contrib-run.yml`, `stale.yml`, and the version-tag release channel (`release.yml`, `publish-crate.yml`, `publish-vscode.yml`). Confirm each exists and serves its documented purpose — repo-layout.md is the source of truth for this list; when it gains a workflow, refresh this enumeration alongside it. Flag any workflow not listed in ai/topics/repo-layout.md (might be fine, might be unreviewed).

## 6. Linter configs

`.markdownlint.json` and `.yamllint` live at the repo root (ai/topics/repo-layout.md names them). Confirm they exist and are consumed: `ci/checks/repo-lint/markdown-lint.sh` reads `.markdownlint.json`, and `ci/checks/repo-lint/github-lint.sh` reads `.yamllint` (not `sh-lint.sh`, which lints shell).

## How to report

Group findings into:

1. **Broken references** — workflow → missing script, CODEOWNERS → missing path.
2. **Bucket organization** — scripts in the wrong bucket or at the wrong level.
3. **Template / CONTRIBUTING drift** — templates that don't match the documented workflow.
4. **Undocumented workflows** — workflows present but not named in ai/topics/repo-layout.md.
5. **Workflow security** — a `pull_request_target` workflow with an over-broad `permissions:` scope, a missing `contrib case` / confinement gate on `contrib-run.yml`, or a sanction-rule mismatch between `pr-policy-check.yml` and `pr-auto-close.yml` that mis-closes valid PRs or keeps unsanctioned ones open.

For each finding, cite the file and the missing/extra reference.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).
