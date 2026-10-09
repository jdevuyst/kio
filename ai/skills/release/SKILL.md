---
name: release
description: Prepare, publish, and verify a Kio release across binaries, crates.io, editor registries, website, and dev containers. Use for release preparation or an actual release, not an ordinary commit or push.
allowed-tools: Read, Grep, Glob, Bash, Edit, Write, Skill
---

# Release Kio

Produce a release from a selected, reviewed source revision, with evidence for
every requested publication channel and consumer check. For preparation-only
requests, finish the reviewable candidate and report readiness without publishing.

## Select the work

Establish the requested version, channels, and source revision. Inspect the
worktree, unpublished commits, existing remote refs, and published versions.
When the checkout is dirty or has unpublished work, ask which revision to release;
do not silently choose the older remote tip, commit/stash changes, or consolidate
history. Continue preparation that does not depend on that choice.

A release request does not implicitly authorize a branch push, DCO sign-off,
history rewrite, account settings change, or deletion. If publishing the selected
revision requires a branch push, obtain explicit push authority and use
[`push-it`](../push-it/SKILL.md). An unsigned push range blocks that push: report
the exact commits and route to explicitly authorized commit preparation; never
add sign-offs or rewrite commits to satisfy the gate yourself. Existing specific
authorization persists; do not ask again for an action already authorized.

## Prepare before freezing

Run dependency review before finalizing the release candidate. Use
[`upgrade-deps`](../upgrade-deps/SKILL.md) to discover advisory, unsupported-line,
and fatal-deprecation reasons; a newer version alone is insufficient. Complete
justified upgrades and their validation, or report the skill's stop condition.
Distinguish checked conclusions from detector failures and unknowns.

Confirm the version choice rather than inferring patch versus minor from the
amount of work. The canonical mirror inventory is
[`version-check.sh`](../../../ci/checks/repo-lint/version-check.sh): update every
release-version mirror, including committed lockfiles and the release-pinned
docs URL, together. Its fuzz and dist-tool pins are separate agreement sets.
Present the exact README version-line replacement for approval before editing;
the same exact-text gate applies to other protected public files.

Review the candidate changes and run the required local gates under the
repository's [`local-ci.md`](../../topics/local-ci.md) rules. The broad baseline
is `ci/all.sh SAMPLE_IMPL`; changed behavioral goldens need exact anchored
full-matrix coverage. A release alone does not justify a repository-wide full
implementation matrix. Run the version and license checks as well.

Record the candidate's full commit ID, version, intended tag and channels, source
tree, and artifact provenance in an untracked release receipt. Subsequent source
changes require a new candidate and renewed affected verification.

## Verify readiness and publication order

Read [channels.md](references/channels.md) and the actual candidate workflow
files before any dispatch, tag, upload, or deployment. Establish which actions
the request authorizes and which prerequisites must complete first. Check live
workflow enablement, environments and permitted refs, registry identities and
version occupancy, credential readiness, and existing releases without exposing
secret values. Local wiring and historical receipts do not prove current settings
or token validity. Missing access is an unknown; request only the necessary access.
Do not infer authority to change visibility, security settings, spending budgets,
or registry configuration.

Tests and Checks is scheduled/manually dispatched; a branch push does not run it.
A normal release requires successful applicable Tests and Checks results for the
exact frozen candidate before irreversible tag/publication. Reuse valid existing
evidence; when it is missing, resolve any needed dispatch authority and run the
chosen ref with stated case, implementation, and platform coverage. Do not ask
again for already-authorized dispatch scope. Reconcile results against the exact
candidate SHA and track all launched work through completion. Sampling or skipped
jobs must remain visible in the receipt. The publishing workflows do not enforce
this prerequisite; establish it before launching them.

The publication workflows are independent. The current
`releases/v<major>.<minor>.<patch>` tag starts binary, crate, and extension work
together; neither registry waits for CI or completed binary artifacts. If the
requested order requires CI/artifact completion before irreversible publication,
and current automation cannot guarantee it, stop before the tag and report the
concrete conflict and available choices. Do not claim this skill changes workflow
ordering, disable publishers, or alter automation without separate authority.
Never push a tag merely to obtain a VSIX.

Preflight candidate packages before the tag, preserving their source SHA and
artifact hashes. Automated publishing can rebuild/package and immediately publish
inside the job; an inspected local hash does not establish the published payload's
hash, and no review pause is implied. Reconcile actual workflow and published
artifact provenance afterward. If the user requires inspecting the final payload
before publication and automation supplies no hold, report that ordering conflict
before launch. Manual handoffs and uploads must use the exact inspected payload.
Follow
[`tools/vscode-kio/DEVELOPMENT.md`](../../../tools/vscode-kio/DEVELOPMENT.md)
for VSIX packaging and archive inspection; validate the crate's packaged contents
and metadata against its candidate. Resolve publication authorization for the
concrete version, revision, and channels before the publishing action. A release
request authorizes only its selected publication scope; it does not expand the
branch-push or sign-off authority above.

## Publish and verify

Monitor every triggered workflow and verify each actual channel result. A green
extension workflow can mean a registry was skipped; an uploaded VSIX alone is not
Marketplace or Open VSX publication. Hand over the exact inspected artifact when
manual upload is required, then wait for the registry result. Verify independent
version listings and downloadable payloads rather than relying on run status.

Pages and devcontainer publishing require separate dispatches. Establish their
actual source and image inputs, preserve image index/platform digests, and verify
the deployed site and anonymous image access where intended. Floating `latest`
and SHA-named tags are insufficient provenance. Do not claim an image build proves
editor startup or Codespaces acceptance.

Complete applicable consumer checks for the selected channels against published
artifacts: released installer execution, crate installation, desktop extension
behavior, local devcontainer attach/reconnect, and browser Codespaces. Check compiler resolution,
highlighting, diagnostics, diagnostic/type hover, completion, and formatting.
Follow the existing published-image acceptance documentation for its native
Linux scope; record separate Desktop, architecture, and hosted results. Human
checks remain pending until their outcomes arrive. Record URLs, run IDs, source
SHAs, versions, artifact hashes, image digests, platforms, and outcomes in the
receipt, with blocked/skipped channels explicitly identified.

## Recover and clean up

Before retrying, reconcile which versions and artifacts already exist. Retry only
failed or unpublished arms after checking the workflow's idempotence; do not
blindly rerun successful crate publishing. Preserve published source and artifact
provenance. Follow-up fixes use new commits and, when required, a new version;
moving a published tag or replacing history needs its own explicit decision.
Deleting a tag does not undo registry publication.

Remove only task-owned temporary resources within the requested authority and
preserve useful failure evidence. Registry deletion needs an exact inventory of
objects, tags/digests, and manifest dependencies, plus explicit deletion authority.
An untagged container manifest is not automatically obsolete. Initial-release
snapshot replacement, provisional-tag cleanup, and bespoke container deletion are
one-time actions, not recurring release defaults. Report what was removed and
recoverability. Finish only after all requested work is verified or each remaining
blocker and required user/external action is clearly reported.
