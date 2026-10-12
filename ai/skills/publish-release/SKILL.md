---
name: publish-release
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

Package and publish `kio-lang` through
`sh ci/package-kio.sh package|publish <Cargo flags>` from the repository root.
This shared entry regenerates the ignored manual snapshot before Cargo's file
inventory and guards tracked manual sources plus Cargo's exact packaged source
inventory. Only its two fresh ignored manual files receive a dirty exception;
Cargo's recorded source metadata is not rewritten.
Inspect the actual `.crate` manual contents against canonical source bytes and
verify a Git-free, sibling-free build. Ordinary checkout builds require no
manual preparation. The helper does not grant publication authority or alter
the preparation-before-immutable-publication order.

Record the candidate's full commit ID, version, intended tag and channels, source
tree, and artifact provenance in an untracked release receipt. Subsequent source
changes require a new candidate and renewed affected verification.

### Independent review gate

Before dispatch or push, obtain an independent adversarial review of release-
workflow changes, including repairs and cleanup. The reviewer must not be the
author of those changes. Review the actual generated/reusable workflow graph,
artifact identity, credentials, failure/skip paths, and publication/rollback
boundaries against the authorized plan. Self-review and automated checks are
separate evidence, not substitutes. Record the reviewer, exact revision, findings
and dispositions in the release receipt. Later changes require review of the
affected delta and its interactions before proceeding; an earlier review does
not clear a revised candidate. Reuse valid reviews of unchanged work.

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

Resolve each secret's actual scope and forwarding through every reusable-call
edge. Selecting an environment does not automatically forward repository secrets.
Check both missing mappings and absent values; never expose secret contents.

When adding, removing or renaming a stored publication secret, update
[`release-credentials.json`](../../../ci/infra/release-credentials.json), its
implemented verifier and workflow mapping together. The repository inventory
guard follows actual publication workflow consumers and rejects missing or stale
checks and mismatched publisher targets. Generated `GITHUB_TOKEN` is governed
separately by workflow permissions; it is not a stored publisher PAT.

Regenerate the release workflow with `node ci/infra/release-workflow.cjs --generate`
and check locally with the helper's `--check`, using the configured optional mise
cargo-dist tool. It compares complete generated bytes from an isolated
metadata-only sandbox, adding credential/preparation execution guards and carrying
forward the prepared VSIX identity. The
reusable preflight runs this exact freshness check before credential verification.
Failed, cancelled or skipped preflight blocks builds, hosting and downstream publication;
both builds and hosting require its explicit successful-verification output.
Hosting additionally requires successful binary builds and preparation with its
explicit ready output; skipped jobs never establish release readiness.
Pull-request planning never selects the publish environment.

The pinned generator's hosting condition accepts skipped builds and exposes no
custom-plan prerequisite there. `allow-dirty = ["ci"]` permits the narrow generated
adjustment; the helper replaces the waived built-in freshness check in full.
Recheck this workaround on cargo-dist upgrades. New workspace, target or
file-bearing generator inputs must extend the bounded sandbox deliberately.
Planning can create hosting state in parallel with preflight, so the manual
pre-tag check remains required.

Tests and Checks is scheduled/manually dispatched; a branch push does not run it.
A normal release requires successful applicable Tests and Checks results for the
exact frozen candidate before launching publication. Reuse valid existing
evidence; when it is missing, resolve any needed dispatch authority and run the
chosen ref with stated case, implementation, and platform coverage. Do not ask
again for already-authorized dispatch scope. Reconcile results against the exact
candidate SHA and track all launched work through completion. Sampling or skipped
jobs must remain visible in the receipt. The publishing workflows do not enforce
this prerequisite; establish it before launching them.

### Explicit point of no return

The first immutable publication is the GitHub release. Name this **point of no
return** before launch; it is not the later crates.io or editor-registry upload.
A release tag starts automation that can cross this boundary without another
pause. Obtain the selected publication authority before that launch, and honor
any requested human hold before the boundary.

The effective workflow order is:

1. Credential and exact-source CI readiness; build binaries, verify the packaged
   crate, and package/inspect one VSIX.
2. Build a distinct candidate image containing those VSIX bytes, run full native
   acceptance against its immutable digest, then promote that same digest.
   Build and smoke-test the website from the selected source and accepted image,
   retaining its deployment artifact.
3. Only after all preparation actually succeeds, publish the immutable GitHub
   release and binary assets — **point of no return**.
4. Publish the crate and the same VSIX to their registries, then deploy the
   prepared website and verify actual channel results.

The accepted container may precede the public GitHub version; it contains
already-available source. Its promotion remains reversible before step 3. Failed
acceptance must never replace the working image. Trace every preparation path,
including failures, cancellation and skips, through the actual generated and
reusable workflow graph. A downstream registry gate alone does not protect the
earlier GitHub publication. The graph still does not enforce Tests and Checks;
establish those results before the tag.

Verify deployment-environment ref policies as well as workflow permissions:
tag-driven Pages deployment must be permitted for release tags, while recovery
from main needs main permitted. Report missing authorization before launch;
do not silently change repository settings. If the required order or human hold
is absent, stop before the tag and explain the concrete conflict. Never push a
tag merely to obtain a VSIX.

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
uses the same acceptance-before-promotion boundary. Normal release Pages
deployment consumes the already-built artifact; standalone site updates retain
the manual Pages workflow. Establish the actual source and image input, preserve image
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

Every release-failure update states whether the point of no return has been
crossed, which channels have published, and which repair or rollback actions
remain reversible. Establish actual channel state rather than inferring it from
the failed job's position. State whether `Resume release` can complete the
remaining publication with the existing artifacts, needs an automation repair,
or cannot resolve the problem without a new user-selected version.

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

### Before the point of no return

First cancel and drain affected publication work, then query actual channel
occupancy: cancellation alone does not prove that publication never happened.
For an authorized retry, fold repairs into the release-preparation commit where
practical, renew affected review/checks and DCO certification, and update the
approved refs using exact force-with-lease expectations. Reuse the existing
consolidation archive rather than adding an archive for every amendment. An
ordinary release request does not supply rewrite or branch-push authority.

An authorized abort may restore the pre-attempt state. Inventory the exact
task-owned resources first. Promotion writes a receipt **before** mutating tags:
it records each tag's prior digest (or absence) and the candidate digest. Restore
a prior container tag only if it still points to this attempt's candidate;
leave a newer or unrelated publication untouched. Restore retained digests
without rebuilding. A previously absent tag requires registry-specific removal:
do not delete a shared image version or component manifest to remove one tag.
Preserve the known-good index and all its platform/component manifests.

After preserving useful local diagnostic evidence, remove the authorized failed
attempt's provisional tags/drafts, disposable refs, obsolete Actions runs and
artifacts, and unreferenced candidate images. Keep successful publication
provenance and unrelated work. Failed runs may contain the only recoverable VSIX
or receipt: do not remove them before those artifacts are secured or no longer
needed. Aim for a clean public history of the successful release, without
claiming that GitHub's internal audit records, notifications or caches were erased.
Report removed resources and any remaining visible residue.

### After the point of no return

Once GitHub or a registry publishes an immutable artifact, preserve its
source, tag and provenance. Report the affected channels and advise whether a new
commit suffices or a user-selected patch version is needed. Never assume tag
deletion, yanking or removal of a registry listing makes a version reusable;
check the channel's actual rules before proposing recovery. An immutable GitHub
release cannot be repaired by moving or reusing its tag. Use a follow-up
automation commit when the frozen payload is sound; source or payload changes
need a user-selected new version. A repair must authenticate the original
source, run and artifacts, satisfy all still-unmet acceptance gates, and preserve
successful publications. Explain any new recovery workflow and whether it is
temporary before introducing it; remove temporary recovery machinery after use.

When temporary recovery needs a GitHub workflow, reuse the name `Resume release`
and path `.github/workflows/release-recovery.yml`, with the release version in
its run title. Keep every run that performed an irreversible publication and its
exact automation commit in main's history, even if a later step failed. Further
repairs use a new commit; never amend away that publishing revision or delete
its run as failed-attempt cleanup. Check actual channel state before classifying
a failed or cancelled run as publication-free. Successful recovery runs also
remain as evidence; they do not turn the original failed run green. Remove only
superseded failed runs without irreversible publication, after preserving useful
evidence and securing any still-needed artifacts. Remove the
temporary definition and recovery-only helpers/guards after completion, without
deleting successful run history merely because the workflow was temporary.
Retain the successful repair commit in main's history and remove temporary code
in a separate cleanup commit; a run's recorded SHA alone is not its source archive.
Necessary permanent fixes may accompany recovery. The cleanup commit may also
complete the normal release-flow repairs rather than being deletion-only. Keep
both commit messages brief. Unsuccessful repair attempts without irreversible
publication may be amended only under the separately authorized rewrite and
preservation rules above.

Remove only task-owned temporary resources within the requested authority and
preserve useful failure evidence. Registry deletion needs an exact inventory of
objects, tags/digests, and manifest dependencies, plus explicit deletion authority.
An untagged container manifest is not automatically obsolete. Initial-release
snapshot replacement is not a recurring release default. Before-boundary cleanup
and rollback follow the scoped procedure above, not blanket resource deletion. Report what was removed and
recoverability. Finish only after all requested work is verified or each remaining
blocker and required user/external action is clearly reported.
