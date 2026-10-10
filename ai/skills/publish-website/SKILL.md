---
name: publish-website
description: Publish the Kio website from a selected reviewed and DCO-signed revision, with website checks, authorized push and Pages deployment, and live verification; not a Kio release
allowed-tools: Read, Grep, Glob, Bash, Edit, Write, Skill
---

# Publish the website

Anchors: AGENTS.md's protected-public-file and commit/push rules, and
[`ai/topics/repo-layout.md`](../../topics/repo-layout.md)'s website and Pages
workflow entries. Deploy the website without starting a Kio release.

## 1. Establish the publication

Identify the intended source revision and deployment destination. Inspect dirty
and unpublished changes; do not silently include unrelated work or choose an
older remote revision. Front-load missing source-selection, commit-preparation,
DCO, push and deployment authority. Reuse existing approvals. A deployment
request does not authorize content edits or history rewrites.

## 2. Verify the prepared source

Require reviewed, DCO-signed publication commits. If preparation is needed, use
[`prepare-commits`](../prepare-commits/SKILL.md) and
[`push-it`](../push-it/SKILL.md), which own review and sign-off prerequisites.
Never add a trailer just to satisfy this skill. Reuse applicable checks and
reviews rather than repeating them; source edits invalidate affected evidence.

## 3. Validate and push

Follow [`local-ci.md`](../../topics/local-ci.md) for validation. Run the website
build, audit and smoke through `ci/checks/orchestrators/website-e2e.sh`, plus
checks relevant to the changed sources. Inspect changed rendered pages for
layout, code blocks and links; landing-page edits require visual review.
Establish any substitution for the push skill's broad gate before spending that
budget; website publication does not itself waive a required gate.

For an authorized push, authenticate the exact prepared range and destination
through `push-it`. If already published, verify the remote revision instead;
do not create a commit merely to deploy it.

## 4. Deploy and verify

Inspect [pages.yml](../../../.github/workflows/pages.yml) and current Pages
settings. With deployment authority, dispatch **Deploy Pages** on the approved
`main` revision and verify the run's actual SHA. Standalone deployment uses the
accepted `latest` container; do not trigger release tags, version bumps, registry
uploads or a container rebuild merely to deploy the website.

Monitor through completion. Verify changed live pages, navigation, code rendering,
links and assets, accounting for deployment/cache delay. Report the live URL,
commit and deployment result. A green build alone is not proof of publication.
For failure, preserve useful evidence, repair only within scope and retry the
affected website work. Remove superseded runs or temporary files only
with the relevant cleanup authority; retain successful publication evidence.
