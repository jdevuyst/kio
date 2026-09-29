---
name: audit-backend-family-conformance
description: Verify family declarations, semantic decision propagation, and a human-usable public FFI across every applicable backend occurrence
allowed-tools: Read, Grep, Glob, Bash
---

# Backend family conformance audit

`specs/backends/README.md` § Language families groups backends into families (`dynamic`, `erased-static`, `native-HKT`, …) by the constraints their host language imposes on emission. The table is the current contract authority for membership and shared idioms; membership is not evidence that a semantic capability or mechanism applies. This skill verifies each backend independently before grouping results, checks semantic decisions propagate to every applicable peer, and checks that the actual public FFI remains idiomatic and usable.

Read AGENTS.md § Universal rules — Backend decisions propagate by semantic
applicability, [`ai/topics/emit.md`](../../topics/emit.md) §§ Stable host release
and capability floor; Decision record and propagation gate, and
[`specs/backends/README.md`](../../../specs/backends/README.md) § Language
families before starting. The README is the source of truth for the family
list, member backends, and family-required shared idioms; this skill does not
hardcode them.

## 1. Family declaration

For each `specs/backends/*.md` other than `README.md`, check:

- The page's family declaration uses the form `Family: name` where `name` is one of the family identifiers in the README's table. It is independent of any concrete-caveat banner.
- The family name appears in `specs/backends/README.md` § Language families.
- The family declaration in the page matches the family that the README's Language families table assigns to that page.

A page without a family declaration, or with a family that doesn't exist in the README, is a finding. A page whose declared family differs from the README's table is a finding (either side could be the canonical one — surface both for the user to reconcile).

Keep Host API stability separate from family classification. It is a
per-backend compatibility status, not a family trait, and neither family
membership nor a shared emitter/runtime implies equal values. Field mechanics
belong to [`audit-backends-shape`](../audit-backends-shape/SKILL.md). When a
change reaches a shared artifact, enumerate each exposed backend's value: every
`stable` row must remain compatible or use a compatibility-preserving split,
while an `evolving` row still needs explicit approval for a concrete break.

## 2. Shared-idiom conformance

For each family, the README's Language families section describes the family's defining traits (type erasure / generics shape / dispatch mechanism / ownership model / etc.). Establish each member's capability and realization from its own spec, emitter, emitted artifact, and runtime evidence before using a family summary. For each per-backend page in that family, check the page either:

- **Conforms** — its FFI surface, host record contract, item naming, and runtime-support choices realize the family's defining traits.
- **Diverges with mutual citation** — the page explicitly documents a divergence in how the host realizes the family idiom, the code emit site cross-references that spec divergence, and the backend still implements the full Kio contract.

A page that diverges without the mutual-cite is a finding. A divergence that rejects, omits, or degrades a spec-admitted Kio feature is also a finding: it is a per-backend caveat, not a family divergence, and mutual citation cannot turn it into a family-level escape hatch.

A page that adopts an idiom outside the family contract (e.g., a `dynamic` backend that mints host-language type names for structural shapes — something `dynamic` family contract says you don't need) is a finding *if and only if* the README's family description doesn't admit it. Some idioms are permissive ("`dynamic` family may mint types if its host has them; the contract is they don't have to").

## 3. Human-usable host facade

For every backend, inspect representative **actual emitted artifacts and host
call sites**, not just the backend page. The public FFI is a human-authored API:
a host author must be able to discover, name, construct, pass, and inspect its
values through ordinary host-language tools and idioms, using
`specs/backends/<lang>.md` and `docs/hosts/<lang>.md` rather than reverse-
engineering generated source.

Prefer a fixed independent runner protocol and cross-implementation golden:
the runner uses the documented public interface and ordinary host tooling, so
an incompatible or unusable facade fails without golden-owned source scraping.
The golden passes the output directory opaquely; it is not the place for
case-owned host imports, native compilation, or generated-file inspection.
Use backend-first `test-data/emissions/<backend>/*/HOST_INTERFACE` evidence only
for a backend-specific public-host fact that the runner cannot naturally and
independently demonstrate. An `ARTIFACT_SHAPE` emission corroborates only a
durable spec- or measurement-backed filesystem fact, not facade usability.
An emission duplicating a runner-proven contract is misplaced; emissions never
satisfy a backend-completeness runtime cell.

Cover enough of the boundary matrix to expose each naming and organization
policy: module-qualified host functions and types, duplicate leaves in
different modules, anonymous products and sums, explicit newtypes with each
constructor/projector visibility shape, and parametric/recursive/existential
forms where the backend presents them publicly. Inspect both directions across
the boundary. A fixed protocol driver authored from the backend spec and
compiled or loaded by ordinary host tooling is representative public-interface
evidence; it must not tolerate an API a person reasonably could not use.
Host-guide snippets corroborate normal authoring. Require separate
`HOST_INTERFACE` evidence only for a public relationship the fixed runner
cannot naturally express; both missing needed evidence and redundant emission
evidence are findings.

Establish the backend's selected stable public language floor independently of
its family and private body representation. For every public relationship,
check whether a capability in that floor's GA/final stable surface, available
through stable defaults or an ordinary stable source pragma, manifest/edition
declaration, or language mode named by the floor, can state the relationship
across the generated package/module boundary in every runtime/toolchain class
the backend contract admits. When it can, erasing the relationship or
requiring public casts is a finding even if an older host release could not
express it or the backend stores values in a private universal carrier.
Preview, experimental, release-candidate, nightly-only, unstable
feature-gated, or experiment-only opt-in capabilities do not establish
applicability. A suspected better host mechanism is evidence, not authority for
a new public contract; report an unresolved contract decision when the
effective authorized behavior does not settle the change.

Report a finding when, despite compiling correctly, the facade:

- flattens unrelated module- or declaration-owned nominal types into one
  package-wide bucket when the host has a natural namespace mechanism;
- makes encoded paths, hashes, ordinal suffixes, or other opaque generated
  identities the ordinary spelling instead of a documented readable facade
  (a collision-proof fallback used only when needed is fine);
- leaks an erased-body carrier (`Any`, `Object`, `any`, unchecked casts) into a
  statically typed public signature without a host-language necessity;
- requires generated-source inspection, private metadata, or string-keyed
  lookup to discover a public entry that the host's normal type/module system
  can expose;
- uses casing, ownership, construction/projection, sum/product, or namespace
  conventions materially unlike idiomatic code in that host language without a
  concrete host-language reason; or
- presents a polished documentation helper while the actual emitted facade it
  wraps remains substantially worse.

Usability does not license a contract change or subjective cosmetic churn.
State the concrete host-author operation that is awkward or undiscoverable,
show the emitted spelling/call site, and identify the simpler host-native
organization that preserves Kio identity, additivity, and collision freedom.
If no such improvement exists, do not report taste as a defect.

## 4. Cross-backend decision propagation

Build the candidate set from every normative shared heading in
`specs/backends/README.md` (including every `**Applies when:**` clause), plus
each backend-local rule or defect whose cause may be reusable, especially one
added or changed in the audited range. For each candidate:

1. **Classify authority first.** A baseline spec plus specific user authority
   may settle observable behavior. An emitter, test, audit finding, or
   similarity among peers is evidence only. Report an ambiguity instead of
   turning it into a contract or a cross-backend repair.
2. **Restate the backend-neutral law and semantic predicate.** Classify it as
   universal, capability-cohort, genuinely family-common after independent
   agreement, or backend-specific host syntax/integration. The cohort may
   cross family boundaries.
3. **Enumerate every shipping backend and semantically distinct occurrence**
   to which the predicate may apply. Record the canonical applicability,
   conformance, and evidence columns from `ai/topics/emit.md` § Decision record
   and propagation gate. Do not infer a row from family membership or from the
   mechanism used by the backend that exposed the rule.
4. **Check placement and closure.** A current host-observable shared law lives
   in the README, with each backend page referencing it and recording only its
   host realization. Shared implementation policy and planning live at the
   same semantic level; host syntax stays local. Public specs state only
   implemented current behavior. An applicable row closes only with
   conformance and complete-workflow evidence; a non-applicable row needs a
   predicate proof.

A missing row, a family conclusion without member evidence, a reusable law
left only in one emitter/page, a shared rule copied across per-backend pages,
or an unresolved/incremental row presented as overall completion is a finding.

## 5. Cross-page consistency

For the shared deprecated-host-items law, inventory every public declaration
by live or history-only provenance independently of family. Verify live
dominance, a recognized deprecation marker on the complete emitted
history-only closure, genuine optionality for a current host (including type
and generic selection), and exclusion from loader/runtime/package execution.
Verify incompatible same-identity declaration epochs do not merge or select by
encounter order: live wins, while every affected retained root and closure is
omitted under matching concrete caveats. If a target cannot retain an old
spelling under all four constraints, verify it likewise omits the shim and
publishes the concrete source-compatibility caveat instead.

Walk all per-backend pages in the same family and all peers selected by the
same semantic predicate. Pages in one family should realize its defining
constraints consistently; peers in a cross-family capability cohort should
reference the same semantic law while documenting their own host syntax. An
unexplained disagreement on the same construct is a finding, but identical
syntax is not required.

Once a family has two or more shipping members and shared idioms start to repeat, the family deserves its own page under `specs/backends/families/<name>.md`. The audit doesn't enforce that page exists, but it surfaces the repetition as a refactor candidate.

## 6. New family rows

If a new family row has been added to the README's Language families table since the last audit (`git log -p specs/backends/README.md | grep -A 5 "Language families"` or similar), check:

- At least one shipping per-backend page declares the new family. An empty row
  presents an unbuilt public family as current and is a finding under
  AGENTS.md § Universal rules — Aspirational content lives only in
  `ROADMAP.md`.
- If the new family's defining traits overlap heavily with an existing family, surface the overlap — the taxonomy refinement may have introduced redundancy.

## 7. Stale family declarations

If a per-backend page declares a family that the README no longer lists (the family was renamed, merged, or split), the page's declaration is stale. Surface so the page can update its declaration to the new family name.

## How to report

Group findings into:

1. **Missing family declarations** — per-backend pages without a `Family:` line.
2. **Mismatched family declarations** — pages declaring a family that doesn't match the README's table.
3. **Undeclared family members** — pages whose family the README lists but they don't declare themselves.
4. **Silent or contract-weakening divergences** — per-backend pages that diverge from their family idioms without mutual citation, or label an unsupported, omitted, or degraded Kio feature as a family divergence.
5. **Human-usability failures** — public facades that compile but are
   materially opaque, flattened, cast-heavy, undiscoverable, or non-idiomatic
   for a normal host author, including public erasure of a relationship the
   selected stable language floor can express, or a backend with no independent
   fixed-host emission exercising representative public operations.
6. **Unpropagated semantic decisions** — missing backend/occurrence rows,
   family-inferred applicability, shared laws left local or copied instead of
   referenced, misplaced shared code/spec policy, incomplete evidence, and
   residual work presented as closure.
7. **Cross-page inconsistencies** — same-family or same-predicate peers handling the same construct differently without justification.
8. **Empty/aspirational family rows** — README rows with no shipping member;
   these are findings, not admissible placeholders.
9. **Stale family declarations** — pages declaring a family the README no longer lists.

For each finding, cite the per-backend page, the relevant section, and the line in `specs/backends/README.md` if applicable.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).
