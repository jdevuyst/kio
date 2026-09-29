---
name: audit-versioning
description: Verify version and license mirrors agree — version-check.sh and license-check.sh cover every manifest, README version line current
allowed-tools: Read, Grep, Glob, Bash
---

# Versioning audit

AGENTS.md § Universal rules — Versioning: "There is a single repo-wide version number, mirrored in every subproject's manifest and named in `README.md`. The canonical list of mirror points lives in `ci/checks/repo-lint/version-check.sh`."

The same rule extends to license metadata: every subproject manifest carries the repo's `MIT OR Apache-2.0` SPDX expression, with the canonical list in `ci/checks/repo-lint/license-check.sh`.

This skill verifies both mirror lists are comprehensive and the actual values all agree.

## 1. Run the canonical checks

The repo already has the checks: `sh ci/checks/repo-lint/version-check.sh` and `sh ci/checks/repo-lint/license-check.sh`. Run both and report the outcomes. If either fails, the mismatch it surfaces *is* the finding.

## 2. Verify the mirror list is comprehensive

Even if `version-check.sh` passes, it only checks the files it knows about. The list can lag when a new subproject is added.

- Find every manifest in the repo that carries a version (Cargo.toml, package.json, pyproject.toml, go.mod, any `*-rs` subcrate's manifest, etc.).
- Confirm each appears in `ci/checks/repo-lint/version-check.sh`'s mirror list.
- Confirm each package manifest also appears in `ci/checks/repo-lint/license-check.sh`'s mirror list or its documented exclusions.
- Confirm `README.md`'s version line is also covered.

## 3. Cross-check actual versions

Independently of the script, read each manifest's version field and confirm they all agree with each other and with `README.md`. The script does this, but reading the files directly is a useful belt-and-suspenders pass.

## 4. New subprojects since the last bump

Run `git log --since="3 months ago" --name-only --diff-filter=A -- '**/Cargo.toml' '**/package.json' '**/pyproject.toml'` and check whether any newly-added manifest is missing from the mirror list.

## 5. AGENTS.md instructions

AGENTS.md § Universal rules — Versioning says: "When a new subproject joins the repo, append its manifest to `ci/checks/repo-lint/version-check.sh`'s mirror list at the same time." Find recent commits adding manifests and verify the version-check script was updated in the same change.

## How to report

Group findings into:

1. **Version mismatches** — concrete file/version pairs that disagree.
2. **Missing mirrors** — manifests with a version field not covered by the check.
3. **License-mirror gaps** — manifests missing the `MIT OR Apache-2.0` SPDX expression or absent from `license-check.sh`'s list.
4. **New-manifest commits without paired mirror-list updates** — historical drift.

For each finding, cite file/path.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).
