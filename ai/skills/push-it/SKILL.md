---
name: push-it
description: Pre-push gate — run ci/all.sh and review-commits, verify contract authority and DCO sign-off, then push
allowed-tools: Bash, Skill
---

# Push it

Pre-push gate: nothing reaches the remote unchecked, unreviewed, unauthorized, or unsigned. The pre-push `review-commits` pass surfaces behavioral-authority blockers and AGENTS.md pairing gaps (spec / docs / tests / version-mirror updates that should accompany an implementation change) before the push goes out.

## 1. Preflight

Abort if `git status` shows uncommitted changes. The user should commit or stash first.

## 2. Run the project check suite

`sh ci/all.sh SAMPLE_IMPL` — the baseline broad local gate (AGENTS.md § Universal rules); escalate per [`ai/topics/local-ci.md`](../../topics/local-ci.md) § Local gate coverage when the pushed range warrants it (backend-sensitive codegen changes, golden changes). Abort on failure — do not push broken code.

## 3. Run review-commits

Invoke the `review-commits` skill on the default unpushed window (`@{u}..HEAD`). If it surfaces an **unverified** normative delta, stop until the prior authority record is identified; if no prior authority exists, classify it as unauthorized. If it surfaces an **unauthorized** delta, remove or revert that delta from the push range. A later user decision may authorize a new work unit, but the retained implementation is then an unlanded proposal that must receive a fresh contract review and every required spec, docs, regression-test, and CI gate before it can re-enter the push range. Pairing gaps, unauthorized scope expansion, partial work, failed tests, and mandatory hygiene defects are blockers: fix them, narrow the range, or meet the repository's explicit incremental-slice contract. A request to proceed does not waive these gates.

## 4. Push

First confirm every commit in the push range (`@{u}..HEAD`, or `main..HEAD` when there's no upstream) carries a `Signed-off-by` trailer. If any commit is unsigned, **stop and report** — do not sign them yourself; DCO sign-off is the user's explicit act (AGENTS.md § Universal rules — Commits, sign-off, and push). Only once the range is fully signed off:

`git push` to the remote tracking branch. No `--force`; `--force-with-lease` only if the user has explicitly asked for a force push.
