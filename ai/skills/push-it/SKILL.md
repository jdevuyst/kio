---
name: push-it
description: Verify prepared publication history, contract authority, checks and authorized DCO sign-off before an explicitly requested push
allowed-tools: Bash, Skill
---

# Push it

Pre-push gate: nothing reaches the remote unchecked, unreviewed, unauthorized, or unsigned. The pre-push `review-commits` pass surfaces behavioral-authority blockers and AGENTS.md pairing gaps (spec / docs / tests / version-mirror updates that should accompany an implementation change) before the push goes out.

## 1. Preflight

Authenticate the intended remote, destination and exact push range, including commits introduced through merges. With no upstream, resolve the actual destination and its published tip rather than using `main..HEAD` as a substitute. Surface dirty work and ask which committed revision is intended; do not automatically include, stash or commit it. A dirty tree cannot supply validation for a different committed candidate.

Verify that the range has coherent publication commits, concise accurate messages and no session links under AGENTS.md. If preparation is missing, route to [`prepare-commits`](../prepare-commits/SKILL.md), present the necessary plan and request rewrite authority; a push request does not automatically authorize rewriting history. Normal consolidation is unpublished; preserve published main/release history. An exceptional shared-development-branch rewrite requires separate explicit range/ref/dependent-consumer approval.

Check DCO before spending the broad-gate budget. Every commit reaching a pull request or remote main must carry a real `Signed-off-by: Name <email>` trailer. If missing, stop and report the range and needed certification; ask for explicit sign-off authority and the real certifier's identity/agreement, rather than signing automatically. Only when that push-preparation authority is already explicit may the agent add the requested trailers, preserving authorship and authenticating the resulting range/tree. Never infer DCO authorization from a general push request or add it at ordinary local commit creation.

Before signing commits that introduce or change blog posts, require a successful
[`review-blog-post`](../review-blog-post/SKILL.md) result matching their final
post contents and relevant compiler context. Resolve missing or stale review
before sign-off; a pre-existing DCO trailer does not waive this gate.

## 2. Run the project check suite

`sh ci/all.sh SAMPLE_IMPL` — the baseline broad local gate (AGENTS.md § Universal rules); escalate per [`ai/topics/local-ci.md`](../../topics/local-ci.md) § Local gate coverage when the pushed range warrants it (backend-sensitive codegen changes, golden changes). Abort on failure — do not push broken code.

## 3. Run review-commits

Invoke the `review-commits` skill on the authenticated outgoing range, normally `@{u}..HEAD` when that matches the intended destination. If it surfaces an **unverified** normative delta, stop until the prior authority record is identified; if no prior authority exists, classify it as unauthorized. If it surfaces an **unauthorized** delta, remove or revert that delta from the push range. A later user decision may authorize a new work unit, but the retained implementation is then an unlanded proposal that must receive a fresh contract review and every required spec, docs, regression-test, and CI gate before it can re-enter the push range. Pairing gaps, unauthorized scope expansion, partial work, failed tests, and mandatory hygiene defects are blockers: fix them, narrow the range, or meet the repository's explicit incremental-slice contract. A request to proceed does not waive these gates.

## 4. Push

Reauthenticate the destination and prepared/reviewed candidate; verify the exact outgoing range is fully signed off. Refuse an unsigned remote-main range. If the remote changed, reconsider range, publication boundary and applicable evidence before pushing; do not overwrite it. A clean tree or successful check never supplies rewrite, DCO or publication authority.

Push only the resolved candidate to the explicitly requested remote/destination.
No `--force`. An exact `--force-with-lease` requires a separately approved
range/ref/dependent-consumer plan: either a shared development branch, or the
unreleased release-preparation tip under [`publish-release`](../publish-release/SKILL.md)'s
recovery rules. The latter is allowed only before immutable publication and
must preserve all earlier published main/release history. Recheck channel
occupancy and certify changed content before that release-tip push.
