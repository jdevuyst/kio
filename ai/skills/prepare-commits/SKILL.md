---
name: prepare-commits
description: Propose coherent publication commits and concise messages, then consolidate an explicitly approved unpublished range while preserving its final tree, attribution, and published history
allowed-tools: Read, Grep, Glob, Bash, Skill
---

# Prepare commits

Anchor: [`AGENTS.md`](../../../AGENTS.md) § Universal rules — Commits, sign-off, and push; Behavioral contracts cannot be rewritten retroactively. Prepare publication history when asked; ordinary local implementation commits do not automatically invoke a rewrite. A push/release request alone does not authorize consolidation or DCO sign-off.

## Propose the range and groups

Authenticate the working-tree state, intended branch/remote and published boundary. Inspect local and remote branches/tags and relevant other publication destinations; an upstream tracking ref alone does not establish that commits are unpublished everywhere. Use read-only remote queries when needed. Normal consolidation covers unpublished history and preserves published main/release history. An exceptional shared-development-branch rewrite needs separate explicit approval of its exact range, refs and dependent-consumer handling; preparation or push authority does not imply that exception. If the boundary is uncertain, identify the uncertainty for the user before mutation.

Inventory subjects, authors, net diffs and dependencies economically. Prefer far fewer large coherent commits: related compiler features, fixes, optimizations and refactors can share one reviewed outcome; separate unrelated compiler/CI changes when that improves review. Local checkpoint or finding counts do not dictate publication commit counts. Include paired specs/docs/tests/migrations with their implementation, and retain mandated metadata such as Host API stability transition trailers. Reorder within the proposed unpublished range when useful, preserving dependencies and meaningful intermediate states; do not hide unsupported partial work or erase behavioral authority.

Present a concrete old-to-new grouping, boundary, order and proposed messages before execution. Subjects are short and name salient features, breaks, fixes, optimizations or refactors; bodies add only necessary technical impact, rationale and validation. Never put session links, scratchpad/evidence paths, orchestration or private context in messages. Relevant public technical references and required attribution/metadata remain appropriate. Keep detailed authority/review receipts separately available to the reviewer. Preserve original author attribution and meaningful co-authorship rather than assigning all consolidated work to the operator.

Identify branches/worktrees/tags and later tasks that depend on old commits; propose how their references and evidence remain usable. State the recovery archive, author/committer timestamp policy, verification plan and any uncertain merge/reorder boundary. Ask for explicit approval of this exact rewrite range/grouping/order/messages; until approved, return the proposal without creating commits, rewriting refs or changing worktrees. A proposal-only request ends here.

## Execute only the approved preparation

Use one local recovery archive per consolidation, named
`archive/pre-consolidation-YYYY-MM-DD` using its UTC start date. For a distinct
consolidation whose name is occupied, append the first unused `-2`, `-3`, etc.
Preserve the original tip before rewriting. With explicit approval, an existing
branch preserving that history may be renamed and reused as the archive.
Follow-up amendments to the same consolidation reuse that archive: append
recovery checkpoints for additional approved edits and retain the intervening
prepared tips in its ancestry rather than replacing its history or creating
another archive. Keep the archive local unless publication is explicitly
requested; identify it in the proposal and completion report.

Recheck the frozen source/ref state and publication boundary. Preserve recoverable original refs and the approved old-to-new grouping BEFORE rewriting; record the actual new commit-hash mapping during and after execution. Keep dependent branches and historical evidence intact under the approved dependency plan. Use an isolated preparation branch/worktree when needed. Apply only the approved plan, retaining the exact final tracked tree, file modes and symlinks; stop for a revised decision if conflicts or dependencies require a different grouping, order or source change. Never rewrite published main/release history or silently rewrite shared/dependent branches or broaden the range.

Authenticate final tree identity against the frozen original and review the resulting commits with [`review-commits`](../review-commits/SKILL.md). Reuse applicable prior authority and validation receipts; a history-only rewrite does not require repeating every old semantic audit or full matrix by default. Check new merge/conflict resolutions and changed intermediate boundaries proportionately. Every commit presented as independently valid needs appropriate checks for that state; unchanged final-tree evidence is not proof that reordered intermediates are valid. Follow [`local-ci.md`](../../topics/local-ci.md) for actual checks and clearly distinguish reused evidence from new execution. Unverified authority remains blocking despite tree equality.

Report the approved range, original/archive refs, new commits/mapping, tree identity, intermediate/final review and validation, and any dependency holds. Consolidation never grants publication authority. Do not add DCO trailers at local commit creation. Only separately authorized push preparation may sign with the real certifier's identity and DCO agreement, preserving authorship; otherwise hand off unsigned prepared history to [`push-it`](../push-it/SKILL.md).
