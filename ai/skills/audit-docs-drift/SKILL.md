---
name: audit-docs-drift
description: Audit docs/ for contract drift, missing coverage, a broken standalone learning path, and inconsistent linked-document structure
allowed-tools: Read, Grep, Glob, Bash
---

# Documentation audit

This skill checks the two governing promises in `ai/topics/docs.md`: "`docs/` may lag in *coverage* but never in *correctness*" and "`docs/` is a standalone introduction to Kio."

`specs/` and the implementation are the source of truth. `docs/` is derivative. If they diverge, `docs/` is the bug.

Read ai/topics/docs.md before starting.

## 1. Per-doc spec anchors

For each file under `docs/` (`tutorials/`, `guides/`, `hosts/`):

- Identify which spec section(s) the doc is teaching.
- Read both side by side. Flag any factual claim in the doc that the spec contradicts or doesn't support.
- Pay particular attention to syntax, semantics of elaborators, type-system rules, package structure, and host-integration details — these are where drift bites hardest.
- For each `docs/hosts/<lang>.md`, compare its publication header with
  `specs/backends/<lang>.md`: both must carry exactly one equal `Host API
  stability: evolving|stable` field immediately below the title, followed by
  an adjacent link to the shared contract. Neither may assign the backend a
  version, maturity tier, or other stability label, and any concrete
  known-caveat banner must appear in both with the same breakage and host
  action. Run `sh ci/checks/repo-lint/backend-api-stability.sh` for exact
  values, complete backend/guide coverage, the new-backend `evolving` default,
  rename semantics, timely field introduction, and transition records.

## 2. Kiodoc snippets

`docs/` snippets use Kiodoc (GitHub-flavored Markdown + fence attributes for runnable / validatable Kio snippets). The default expectation is that docs snippets are checked by `kio doc check`; `{ignore}` is a last resort, not a convenience flag.

- Find every Kio code block in `docs/**/*.md`.
- Run `kio doc check` from `docs/` when the working tree has a `kio` binary; report any failures. If no binary is available, inspect snippets manually against the current spec and implementation.
- Count every `kio {ignore}` fence. For each ignored snippet, decide whether a Kiodoc feature could validate it: hidden/visible harnesses, document-scoped `{file}` fences plus a file-backed harness for multi-file context, `variant=KIND`, `check_exit_code=N`, or `placeholder={"...":"valid_replacement()"}` for display-only placeholders.
- Flag any ignored snippet that appears compilable, or that uses placeholder text such as `...` without a `placeholder=JSON_OBJECT` substitution when a valid replacement is apparent.
- Accept `{ignore}` only for true syntax/type fragments or examples that still cannot be made meaningful under Kiodoc. `variant=KIND ignore` is acceptable for skipped file-kind fragments.

## 3. Recently-changed specs vs docs

Run `git log --since="3 months ago" --stat -- specs/` and find commits that changed spec content. For each:

- Identify the docs pages that teach the affected spec section.
- Check whether those docs pages were updated in the same commit or in a follow-up.
- If not, flag the page as a drift candidate.

## 4. Removed / renamed features

Grep `docs/` for symbol names, keywords, syntax forms, or elaborator names. Cross-check each against `specs/`. Anything in docs that no longer appears in the spec is either:

- A teaching-purposes naming choice (fine if explained).
- A stale reference to a removed/renamed feature (flag).

Particular suspects to grep: any keyword or intrinsic name; every elaborator name in the current palette — take the enumeration (both the spine and the algebraic families) from [`ai/topics/surface-forms.md`](../../topics/surface-forms.md) rather than hardcoding a list here; any `if` / `else` / `fn#` / `alias` / `labels` / `op` / `equiv` usage.

## 5. Standalone learning path

Start at `docs/README.md` and follow the order and branches it recommends as a new Kio user would.

- Map each major language and ordinary-development concept to the first document that meaningfully explains it. “Major” means central to everyday Kio work, not every specification edge case or repeated host-specific detail.
- Flag central concepts that the documentation never teaches, concepts relied on before they are introduced, circular prerequisite chains, and unexplained terminology that prevents a reader from following the path.
- Flag any core explanation that requires the reader to consult specifications, source code, tests, blog history, agent guidance, or an external source. Links to those sources may add precision, but cannot replace the learner-facing explanation.
- Check that `docs/README.md` routes readers through a sensible primary learning path and clearly separates reference, task-oriented, case-study, and host-integration branches.
- Audit tutorials as a sequence. Audit each guide as a standalone topic: it must either explain its prerequisites or link to where the learning path teaches them. Treat case studies and host guides according to the specialized audiences in `ai/topics/docs.md`.
- Follow advertised reusable-library adoption routes. Apply the dependency-example and fetch/setup contract in `ai/topics/docs.md` § Keeping a case study paired with its POC; use [`audit-poc-docs-sync`](../audit-poc-docs-sync/SKILL.md) § Usable library adoption for the package-specific checks. A library description without a usable route to importing it is a standalone-workflow gap.

## 6. Internal link structure

For every link from one document under `docs/` to another, compare any surrounding description and reading order with the destination itself.

- Flag descriptions or sequences that overstate or understate the destination's coverage, name the wrong audience or purpose, conceal a prerequisite, promise material the destination does not contain, or place the page before its prerequisites or after material that relies on it.
- Do not presume that either the description, sequence, or destination is the stale side. Classify the mismatch according to the truthful, coherent standalone learning path: the description may be wrong; the destination may be incomplete, misdirected, or misplaced; or the reading order may need to change.
- If resolving a mismatch would weaken or remove a meaningful description, identify the coverage or routing obligation it carried and verify that the revised docs still discharge it. Re-check the linking document's surrounding explanation, prerequisites, outgoing route, and place in the learning path; flag a newly hidden concept or broken transition as a coverage gap, not a successful wording cleanup.
- Judge each link structure by the documents' roles: catalogs and routing pages must direct readers accurately, sequences must preserve prerequisite order, and standalone pages must fulfill their advertised scope.
- A bare link whose destination is clear from context need not acquire redundant prose; this check is about the accuracy of descriptions that exist.

## 7. Linked-list consistency

Check that every doc file in `docs/` is reachable from some index (the [`docs/README.md`](../../../docs/README.md) catalog or a forward link from another doc). Orphan docs are a finding.

## How to report

Order findings by their effect on a reader's ability to understand and use Kio, not by the directory or document class in which they occur.

Group findings into:

1. **Docs contradict spec** — the most serious; user-visible incorrect information.
2. **Stale references** — names / features that have moved on.
3. **Drift candidates** — spec changes whose docs weren't updated.
4. **Broken snippets** — Kio code blocks that no longer build.
5. **Unjustified ignored snippets** — ignored Kio fences that should be validated with Kiodoc.
6. **Backend publication drift** — Host API stability field shape/value/parity,
   default, or transition-record failures; backend versions, maturity/ad hoc
   stability labels; or concrete caveats whose spec and host-guide banners do
   not match.
7. **Orphan pages** — docs not reachable from any index.
8. **Learning-path or standalone failures** — central coverage gaps, concepts used before introduction, circular prerequisites, missing routing, or explanations that require non-docs material.
9. **Link-structure mismatches** — a description, destination, or reading order disagrees about coverage, audience, purpose, or prerequisites; state which part should change and why, without presuming that any one side is authoritative.

For each finding, cite the affected doc pages and the governing evidence: the relevant spec section for correctness drift, or the description, destination, ordering context, and `ai/topics/docs.md` contract for learning-path, coverage, reachability, and link-structure findings.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).
