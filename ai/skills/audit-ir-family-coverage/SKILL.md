---
name: audit-ir-family-coverage
description: Report which language families exercise each universal-IR variant — surfaces single-family bias and aspirational (unexercised) variants before more backends calcify the shape
allowed-tools: Read, Grep, Glob, Bash
---

# IR family coverage audit

Kio's IR (the `Expr` / `Type` enums in `kio-rs/src/ast.rs`, plus the enriched / lowered variants) absorbs work that every backend would otherwise re-derive. If a given IR variant is exercised only by backends in one language family, the IR shape may have baked in assumptions specific to that family — assumptions that will surface when a backend from a different family lands.

This audit walks the IR variants and reports per-family coverage. The output is **informational**, not pass/fail: a maintainer reads it to spot IR constructs that lean on one family's properties and decide whether to refactor before more backends calcify the bias.

Read [`specs/backends/README.md`](../../../specs/backends/README.md) § Language families before starting. The family list (and current member backends) lives there as data; this skill reads it at run time rather than hardcoding family names.

## 1. Enumerate IR variants

For each of the following AST enums in `kio-rs/src/ast.rs`, list all variants:

- `Expr` — the surface-level expression variants.
- `Expr::Low*` — the Low IR variants (`LowHostCall`, `LowClosureCall`, `LowAbsurdCall`, `LowCpsProjectorApply`, `LowQualifiedModuleCall`, `LowQualifiedNewtypeMember`, `LowNewtypeCtor`, `LowNewtypeProj`, `LowIndirectCall`, …).
- `Expr::Enriched*` — the structurally-recovered variants (`EnrichedTuple`, `EnrichedRecord`, `EnrichedProject`, `EnrichedFieldGet`, `EnrichedInject`, `EnrichedMatch`, `EnrichedConditional`).
- `Type` — top-level type variants.
- The capability annotations on closure / fn nodes: `Lifetime` (on `Type::Function` via `FnTypeCapabilities`). `Capabilities.captured_from` on `Expr::FnExpr` is an internal cross-pass annotation no emitter consumes, and the Rust build-block `thread_safety` key is a post-emit transform, not an AST annotation — neither joins the coverage matrix.

The enumeration is mechanical — grep the `enum Expr` and `enum Type` declarations and collect variant names.

## 2. Map variants to families

For each variant, identify which shipping backend's emitter has a non-trivial match arm for it. "Non-trivial" means more than a `_ => unreachable!()` or pass-through. The handling code lives under `kio-rs/src/backends/<name>/`.

Group the per-backend hits by the family the backend declares near the top of `specs/backends/<name>.md`. The result is a `variant -> {family: [backend names]}` map.

## 3. Read the family table

Read `specs/backends/README.md` § Language families to enumerate the families that exist. This is the denominator: each variant's coverage is "covered in N of K families."

## 4. Report

Produce a table with these columns:

- **IR variant** — variant name (e.g., `LowAbsurdCall`).
- **Source** — the AST enum it belongs to.
- **Families covered** — which families have at least one backend with non-trivial handling.
- **Coverage** — `N / K` where K is the total family count.
- **Surfaced concern** — `OK` if coverage is broad, `single-family bias` if only one family covers it, `aspirational` if no family covers it (the variant exists in the IR but no shipping backend emits for it).

After the table, summarize:

- Variants with single-family bias — likely to surface gaps when a backend from a different family lands. Worth scrutinizing now: does the variant's interface make sense for other families, or is its design coupled to the one family that uses it?
- Variants with `aspirational` status — these are IR constructs the language admits but no backend currently handles. Either the variant is dead (and should be removed) or it's awaiting a backend that needs it.
- Families with sparse coverage overall — a family with no shipping backend naturally has zero variant coverage. Note which families are aspirational at the backend level.

## 5. Interpretive guidance

The audit produces data, not verdicts. Use these heuristics when reading the report:

- **`OK` (broad coverage)** — the variant has been exercised by multiple families' emitters, so its interface is family-agnostic by construction. No action.
- **`single-family bias`** — the variant exists because one family needed it. When a backend from another family lands, expect to either reshape the variant or add a parallel one. If the bias is fundamental (e.g., a variant only ever makes sense for static-typed backends), that's fine; surface it so the family taxonomy can reflect the constraint. If the bias is incidental (the variant could serve other families but happens not to be needed yet), refactor opportunities surface here.
- **`aspirational`** — the variant is exercised by no shipping backend; it was added speculatively. Cross-check against `ROADMAP.md` to see whether a settled roadmap thread is actively building toward needing it. If not, candidate for removal.

## 6. Cross-cutting touchpoints

Reshaping the IR is cheap while the variant set lives only in `kio-rs/src/ast.rs` — the cost is bounded to one Rust crate. Anything that mirrors the variant set (a serialized form, a second implementation, per-backend translators) multiplies that cost, so single-family-biased variants are cheapest to fix the moment this audit surfaces them.

## How to report

Group findings into:

1. **Single-family-biased variants** — variants covered by only one family, with notes on whether the bias is fundamental or incidental.
2. **Aspirational variants** — variants no shipping backend exercises.
3. **Family coverage summary** — per-family count of variants covered, surfaces families with sparse coverage overall.
4. **Reshape candidates** — variants that would benefit from reshaping while the variant set still lives in one Rust crate, before anything mirrors it.

**Default: report only.** This audit is informational by design; the maintainer decides what to act on. If invoked with a fix-it directive, surface the candidates but do not autonomously reshape the IR (large change with cascading impact).
