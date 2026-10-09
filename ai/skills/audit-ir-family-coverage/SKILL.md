---
name: audit-ir-family-coverage
description: Trace IR phase applicability and direct/shared/delegated consumers by backend and family, separating source coverage from runtime evidence and identifying justified coverage concerns
allowed-tools: Read, Grep, Glob, Bash
---

# IR family coverage audit

Kio's IR (the `Expr` / `Type` enums in `kio-rs/src/ast.rs`, plus the enriched / lowered variants) absorbs work that backends would otherwise re-derive. Trace which phases and semantic consumers handle each variant before aggregating by family; a missing backend-local match arm does not establish missing support.

This audit walks the IR variants and reports per-family coverage. The output is **informational**, not pass/fail: a maintainer reads it to spot IR constructs that lean on one family's properties and decide whether to refactor before more backends calcify the bias.

Read [`specs/backends/README.md`](../../../specs/backends/README.md) § Language families before starting. The family list (and current member backends) lives there as data; this skill reads it at run time rather than hardcoding family names.

## 1. Enumerate IR variants

For each of the following AST enums in `kio-rs/src/ast.rs`, list all variants:

- `Expr` — the surface-level expression variants.
- `Expr::Low*` — the routed IR variants actually present in the enum; do not infer the inventory from this skill's examples.
- `Expr::Enriched*` — the structurally-recovered variants (`EnrichedTuple`, `EnrichedProject`, `EnrichedFieldGet`, `EnrichedInject`, `EnrichedMatch`, `EnrichedConditional`).
- `Type` — top-level type variants.
- The capability annotations on closure / fn nodes: `Lifetime` (on `Type::Function` via `FnTypeCapabilities`). `Capabilities.captured_from` on `Expr::FnExpr` is an internal cross-pass annotation no emitter consumes, and the Rust build-block `thread_safety` key is a post-emit transform, not an AST annotation — neither joins the coverage matrix.

The enumeration starts mechanically from the enum declarations, then reads the
phase witnesses and [`pass/mod.rs`](../../../kio-rs/src/pass/mod.rs). Record
where each variant is inhabited, introduced, consumed or eliminated. A surface
form eliminated before host emission is N/A for a direct host-emitter arm, not
exempt from semantic backend coverage: its lowering and resulting behavior
still require evidence on applicable implementations.

## 2. Map variants to families

For each applicable backend and occurrence, trace the complete consumer path:
phase lowering/routing, shared preparation or value plans, direct emission,
delegated emission, or serialization plus runtime dispatch. TypeScript uses JS
runtime emission; Java/Python use the shared serialized runtime IR and their
runtime interpreters. A static skin may consume a shared plan rather than match
`Type` directly; an erased body need not emit general host type syntax.

Record actual handling and its semantic obligation, not just match-arm counts.
An impossible-phase `unreachable!()` is not a missing implementation, while an
identity/pass-through needs its applicability justification. Count a shared or
delegated owner for its consumers without pretending these are independent
implementations. Exhaust these paths before labeling a variant as having no
applicable consumer.

Group independently established backend results by the family declared in
`specs/backends/<name>.md`. Record capability-specific consumers separately;
one member's need for an annotation does not establish a family-wide need.
Keep source-consumer presence distinct from running corpus witnesses and
assertions. Source coverage does not prove execution or correct behavior;
[`audit-backend-completeness`](../audit-backend-completeness/SKILL.md) owns the
runtime value-shape and application-stage gate.

## 3. Read the family table

Read `specs/backends/README.md` § Language families. Report source-consumer
coverage across current families, with the applicable backend/occurrence
denominator and justified exclusions visible. A zero count for a deliberately
eliminated form or capability-specific annotation is not inherently a gap.

## 4. Report

Produce a table with these columns:

- **IR variant** — variant name (e.g., `LowAbsurdCall`).
- **Source** — the AST enum it belongs to.
- **Phase/applicability** — inhabited phases, consuming pass and applicable backends/occurrences.
- **Consumers / families** — direct, shared, delegated and serialized-runtime owner paths, aggregated by family only after the trace.
- **Source coverage** — covered applicable consumers versus the stated denominator; identify direct-arm N/A separately.
- **Runtime evidence** — actual executing witness and assertion, prior receipt or unassessed; source presence alone earns no runtime credit.
- **Surfaced concern** — broad source coverage, capability-specific use, unexplained single-family concentration, missing applicable consumer or missing runtime evidence, with reasons.

After the table, summarize:

- Unexplained single-family concentration, distinguishing a justified capability-specific use from a potentially coupled interface.
- Missing applicable consumers only after elimination, planning and delegation paths are exhausted; distinguish unreachable/dead implementation state from admitted behavior needing support.
- Source-coverage and runtime-evidence gaps separately, with the affected backend and owner; sparse direct match arms alone are not a finding.

## 5. Interpretive guidance

The audit produces data, not verdicts. Use these heuristics when reading the report:

- **Broad source coverage** establishes implementation structure, not a proof of family independence, runtime execution or correctness. Multiple consumers may share the same mistaken assumption.
- **Single-family concentration** warrants an applicability trace, not an automatic reshape. A capability-specific consumer may be legitimate even within a family whose other members do not need it.
- **No applicable consumer** is meaningful only after the phase/shared/delegated/runtime trace. Do not infer speculative intent, a missing language implementation or removal authority merely from absent emitter arms.

## 6. Cross-cutting touchpoints

Before suggesting a reshape, inventory actual mirrors and dependencies:
serialized `RuntimeExpr`, Java/Python tag dispatch, shared facade/value plans,
phase validators and backend translators as applicable. These consumers make
the impact larger than editing one enum. This informational audit identifies
supported concerns; it does not authorize an IR refactor.

## How to report

Group findings into:

1. **Concentrated consumers** — source coverage limited to one family/capability, with applicability and ownership justification.
2. **Missing applicable consumers** — supported only after the complete phase/consumer trace, distinct from eliminated forms.
3. **Family/backend coverage summary** — source consumer counts and runtime evidence limits separately, including shared/delegated owners.
4. **Supported reshape candidates** — concrete concerns and actual mirror/dependency impact, without implementation authority.

**Default: report only.** This audit is informational by design; the maintainer decides what to act on. If invoked with a fix-it directive, surface the candidates but do not autonomously reshape the IR (large change with cascading impact).
