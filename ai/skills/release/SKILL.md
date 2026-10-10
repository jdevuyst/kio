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

## Front-load decisions and prerequisites

Before expensive builds or external mutations, inspect the intended release
path and collect foreseeable decisions: source and version, commit preparation,
exact protected-text edits, DCO and push, workflow coverage, publication channels,
and any manual consumer checks. Reuse existing approvals. Present unresolved
choices with concrete recommendations early, respecting a preference for one
question at a time; do not wait until each action becomes the next step.

Check account, credential, environment and artifact prerequisites early through
non-mutating probes. Identify user setup or access requirements before promising
an unattended release. If verification itself depends on an authorized push or
dispatch, explain that dependency up front rather than treating secret presence
as readiness. Keep authority scoped to specific actions and candidates; early
approval does not waive later checks. Continue independent preparation while
answers are pending. Reopen decisions only for changed scope or new findings,
not to reconfirm settled choices.

## Prepare before freezing

Run dependency review before finalizing the release candidate. Use
[`upgrade-deps`](../upgrade-deps/SKILL.md) to discover advisory, unsupported-line,
and fatal-deprecation reasons; a newer version alone is insufficient. Complete
justified upgrades and their validation, or report the skill's stop condition.
Distinguish checked conclusions from detector failures and unknowns.

Use the version supplied by the user; ask if none was supplied. Check it against
published versions and the release's compatibility changes, and confirm with the
user if it is unexpected or already occupied. Do not choose a different version
automatically. The canonical mirror inventory is
[`version-check.sh`](../../../ci/checks/repo-lint/version-check.sh): update every
release-version mirror, including committed lockfiles and the release-pinned
docs URL, together. Its fuzz and dist-tool pins are separate agreement sets.
Present the exact README version-line replacement for approval before editing;
the same exact-text gate applies to other protected public files.

Prefer one concise release-preparation commit, such as `Release 0.2.0`, containing
the version mirrors and related release fixes. Keep unrelated feature history
separate and consolidate it only under an approved `prepare-commits` plan.

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

Before creating a release tag or hosting state, run the manually dispatchable
`Release credential preflight` on trusted `main`, with dispatch authority, and
record its source SHA and results. It uses the same `publish` environment as the
registry publishers and gates release builds. Missing credentials, rejected access,
service failures and unsupported responses fail the check. Read its proof limits:
crates.io token authentication does not establish publish scope or crate access,
and Marketplace publisher access does not establish a publishing role. Open VSX
checks namespace authorization. A green preflight is not permission to publish
and does not replace artifact inspection or channel acceptance.

When adding, removing or renaming a stored publication secret, update
[`release-credentials.json`](../../../ci/infra/release-credentials.json), its
implemented verifier and workflow mapping together. The repository inventory
guard follows actual publication workflow consumers and rejects missing or stale
checks and mismatched publisher targets. Generated `GITHUB_TOKEN` is governed
separately by workflow permissions; it is not a stored publisher PAT.

Regenerate the release workflow with `node ci/infra/release-workflow.cjs --generate`
and check locally with the helper's `--check`, using the configured optional mise
cargo-dist tool. It compares complete generated bytes from an isolated
metadata-only sandbox, adding only credential execution guards to builds and hosting. The
reusable preflight runs this exact freshness check before credential verification.
Failed, cancelled or skipped preflight blocks builds, hosting and downstream publication;
both builds and hosting require its explicit successful-verification output.
pull-request planning never selects the publish environment.

The pinned generator's hosting condition accepts skipped builds and exposes no
custom-plan prerequisite there. `allow-dirty = ["ci"]` permits the narrow generated
adjustment; the helper replaces the waived built-in freshness check in full.
Recheck this workaround on cargo-dist upgrades. New workspace, target or
file-bearing generator inputs must extend the bounded sandbox deliberately.
Planning can create hosting state in parallel with preflight, so the manual
pre-tag check remains required.

Tests and Checks is scheduled/manually dispatched; a branch push does not run it.
A normal release requires successful applicable Tests and Checks results for the
exact frozen candidate before irreversible tag/publication. Reuse valid existing
evidence; when it is missing, resolve any needed dispatch authority and run the
chosen ref with stated case, implementation, and platform coverage. Do not ask
again for already-authorized dispatch scope. Reconcile results against the exact
candidate SHA and track all launched work through completion. Sampling or skipped
jobs must remain visible in the receipt. The publishing workflows do not enforce
this prerequisite; establish it before launching them.

The `releases/v<major>.<minor>.<patch>` tag starts the cargo-dist binary release.
After successful binary hosting, one custom publisher coordinates extension
packaging, candidate-container building, acceptance and promotion before it
allows crates.io, Marketplace or Open VSX publication. All consume the same
tagged source revision. The image and both editor registries consume the same
inspected VSIX artifact, not independently rebuilt packages. A failed candidate
must leave the working `latest` image intact. This dependency does not gate on
Tests and Checks; establish that prerequisite before the tag. If a
requested order or inspection hold is absent from the actual candidate workflows,
stop before the tag and report the concrete conflict and choices. Do not disable
publishers or alter automation without separate authority. Never push a tag merely
to obtain a VSIX.

Preflight candidate packages before the tag, preserving their source SHA and
artifact hashes. A local preflight hash does not establish the workflow payload's
hash: inspect and test the actual candidate artifacts, retaining their identities
through publication. Build candidate images under distinct tags and test by
immutable digest; promote that digest without rebuilding. Publish its
source-specific tag even when a proven newer `latest` must remain unchanged,
and record both identities. Match source revision,
VSIX artifact ID/hash and acceptance evidence before allowing registry uploads.
Automated acceptance does not imply a human review pause. If the user requires
one and automation supplies no hold, report that conflict before launch. Manual
handoffs and uploads must use the exact inspected payload.
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

The release coordinator publishes the accepted container. Reuse that accepted
image when completing the same release; do not rebuild it merely to retry
publication. Standalone monthly/manual image publication
uses the same acceptance-before-promotion boundary. Pages still requires a
separate dispatch. Establish its actual source and image input, preserve image
index/platform digests, and verify the deployed site and anonymous image access
where intended. Floating `latest` and SHA-named tags are insufficient provenance.
Do not claim an image build proves editor startup or Codespaces acceptance.

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
failed or unpublished leaf jobs after checking the workflow's idempotence; never
rerun the aggregate release coordinator after partial registry publication.
Marketplace and Open VSX consume the same inspected `kio-vsix` artifact as the
accepted image. Preserve successful packaging, image and acceptance evidence;
do not rebuild them merely to retry a registry upload. A failed acceptance or
promotion leaves registry jobs blocked. Rebuilding a package or image requires
renewed acceptance and can collide with immutable run artifacts.
A GitHub Actions rerun uses the
original source SHA: after a source amendment, launch the necessary checks on
the new SHA instead of treating an old run's retry as validation of new code.

Before immutable publication, fold required release repairs into the same
release-preparation commit where practical, renewing affected review and checks.
Amending an already-pushed release-preparation tip requires explicit approval
of the commit/range, remote ref and dependent-consumer handling, plus fresh DCO
certification for changed content. Preserve recovery history in the same
consolidation archive where one exists and use an exact `--force-with-lease`.
This narrow exception never rewrites earlier published main or release history.
An ordinary release request does not supply rewrite or push authority.

Once a registry or other immutable channel publishes an artifact, preserve its
source and provenance. Report the affected channels and advise whether a new
commit suffices or a user-selected patch version is needed. Never assume tag
deletion, yanking or removal of a registry listing makes a version reusable;
check the channel's actual rules before proposing recovery. Moving a published
tag is not an ordinary retry and needs its own explicit decision.

Remove only task-owned temporary resources within the requested authority and
preserve useful failure evidence. Registry deletion needs an exact inventory of
objects, tags/digests, and manifest dependencies, plus explicit deletion authority.
An untagged container manifest is not automatically obsolete. Initial-release
snapshot replacement, provisional-tag cleanup, and bespoke container deletion are
one-time actions, not recurring release defaults. Report what was removed and
recoverability. Finish only after all requested work is verified or each remaining
blocker and required user/external action is clearly reported.
