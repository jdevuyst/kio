---
name: audit-design
description: Audit DESIGN.md for supported goals and design rationale, contract authority, and approval protection for the whole design
allowed-tools: Read, Grep, Glob, Bash
---

# Project design audit

Anchor: AGENTS.md § Universal rules — "Project design is approval-gated."
Read that rule, [`DESIGN.md`](../../../DESIGN.md), and
[`ai/topics/design.md`](../../topics/design.md) § Goals first, then the design
that delivers them.

## 1. Purpose and authority

Read each topic's goal alongside the design that serves it. Report
contradictions and unsupported rationale; do not redesign the language on the
user's behalf. The entire document is binding: its structure does not make
any goal or design decision optional or exempt from approval.

Check the goal-first organization against the owning topic: `##` sections
state goals in their opening prose, and `###` subsections explain mechanisms
and how they serve those goals. Flag disconnected feature lists, mechanisms
introduced before their purpose, and public prose narrating the document's
layout. Do not require repeated commitment/choice labels or duplicate a
mechanism under every goal it serves.

Design rationale must not override the effective authorized behavioral
contract, turn an implementation gap into an accepted limitation, or imply
authority to change specifications. Check that this relationship is explicit.

## 2. Factual support

Follow the document's links into specifications and POC case studies. For each
claim about Kio behavior, identify supporting evidence or report the precise
contradiction or missing support. Distinguish a design priority from a claim
that an implementation achieves it.

Compare the priorities with the framing in `README.md` and
`website/.vitepress/theme/components/Landing.vue`. Surface possible omissions
for discussion; a design document need not enumerate every feature.

In particular, check the boundaries of the type-safety and normalization
claims, the Kio'/surface distinction, explicit derivation candidates, and
interface compatibility versus behavioral equivalence.

Apply the public-document rules in [`no-leak.md`](../../topics/no-leak.md) and
[`no-future-extensions.md`](../../topics/no-future-extensions.md). Report broken
links, unbuilt-feature promises, and unsupported completeness or optimality
claims. Do not expand the audit into implementation work.

## 3. Routing and approval

Confirm that AGENTS.md links to `DESIGN.md` and explicitly requires user
authorization both for departures from its design and for its creation,
edits, deletion, and renaming. Check the trigger to `ai/topics/design.md`,
the repository-layout entry, and the `audit` umbrella's reference to this skill.

When reviewing a caller-supplied change range, include removed or renamed
copies of the file and changes to its approval rule. Trace authorization to
the available user-interaction record. A commit message, this audit, or a
same-work rule edit cannot supply permission. If the interaction record is
unavailable, report approval as unverified; a repository snapshot alone cannot
establish it. For a snapshot audit without a change range, check the gate's
presence without inventing an approval history.

## Report

Separate factual drift, authority or approval issues, and broken routing.
Cite file and line, the supporting contract or source, and the decision needed.
Alternative designs are discussion suggestions, not automatic findings.

**Default: report only.** With a fix directive, follow
[`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md). A generic fix
directive does not authorize edits to `DESIGN.md` or departures from it;
present a concrete proposed change for approval unless that specific change is
already authorized.
