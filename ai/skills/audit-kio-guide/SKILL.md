---
name: audit-kio-guide
description: Verify portable KIO-FOR-AGENTS.md and its repository supplement — resolving pointers, accurate workflow, docs-first discovery, and no repository-only leakage or hidden workarounds
allowed-tools: Read, Grep, Glob, Bash
---

# Kio authoring guidance audit

Read `KIO-FOR-AGENTS.md`, `ai/topics/kio-authoring.md`, and the relevant
`AGENTS.md` triggers. The root guide owns portable agent authoring workflow;
the topic owns only compiler-repository obligations. Neither replaces the
public documentation catalog or specifications.

## 1. Pointers and discovery

Resolve every Markdown link relative to its containing file, including
heading anchors. Check plain-text spec-section citations against real headings.
Verify that both files route ordinary learning to `docs/README.md` rather than
maintaining a second catalog. Every checked-in `docs/guides/*.md` page must be
listed there; discharge any intentional exception explicitly.

Confirm the root README lists `KIO-FOR-AGENTS.md`, the repository topic links
it, and the root guide works when read with `kio manual cat KIO-FOR-AGENTS.md`
from an unrelated directory. Relative documentation paths describe the virtual
bundle, not the consuming project's filesystem. Check the guide's manual
commands against the actual binary and use `audit-bundled-docs` for full
inventory, packaging, and native extraction acceptance.

## 2. Portable workflow versus repository maintenance

The root guide must make sense in an unrelated project with an installed
compiler and no compiler checkout. Reject maintenance-only dependencies on
`ai/topics/`, `ci/`, internal corpus ownership, repository paths, or local
machine context. General project instructions still apply; the guide cannot
override mandatory repository or skill instructions.

The repository supplement must retain the surface-forms/open-world routing,
test-layer boundaries, shared abstraction ownership, and public debugging
guide pointer without copying the portable workflow. Verify the
`surface-forms.md` topic remains linked from both `AGENTS.md` and the supplement.

## 3. Documentation authority and learning path

Require docs-first navigation for ordinary language and idiom learning, with
specifications available for precise contract questions. Report contradictions
between docs and specs, and ordinary authoring knowledge found only in the
agent guide or specifications that public docs should teach. Source inspection
for integration or bug diagnosis is distinct evidence, not proof that the
documentation learning path succeeded. Do not impose artificial isolation or
a universal ban on reading specifications; a bounded external docs-first
acceptance trial has its own evidence boundary.

## 4. Claims, preferences, and workarounds

For each substantive claim, identify the supporting specification or working
example. Separate style defaults from language requirements. The guides may
route, recommend an authoring workflow, and explain how to find knowledge;
ordinary language teaching belongs in the public docs. Discharge sizeable
duplicated explanations and conflicting advice individually.

Sweep both authoring files and public guides for workaround language such as
`does not work`, `workaround`, `the emitter`, and `avoid`. Each hit needs a
verdict: a backed idiom, a legitimate boundary, or a hidden compiler defect.
Never turn a defect into an idiom; follow `AGENTS.md`'s bug and scope rules.

## Reporting

Lead with broken links or commands, then portability/split findings, learning
gaps and contradictions, and claim/workaround discharges. Identify the proper
owner of each correction. Protected public files retain exact-text approval
requirements. If invoked with a fix directive, follow
[`audit-fix-mode.md`](../../topics/audit-fix-mode.md).
