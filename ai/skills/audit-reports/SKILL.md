---
name: audit-reports
description: Run selected reporting harnesses or all four, interpret their evidence, and propose bounded follow-ups for mutation, fuzzing, generator coverage, and normalizer reachability
allowed-tools: Read, Grep, Glob, Bash
---

# Reporting audit

Own the informational harnesses under [`reports/`](../../../reports/). Select mutation, fuzz, kio-gen coverage, normalizer reachability, or all four according to the user's request; an unqualified invocation selects all four. Read only the selected procedures:

| Report | Procedure |
| --- | --- |
| Semantic-core mutation and optimizer firing | [references/mutation.md](references/mutation.md) |
| Parser and lexer fuzzing | [references/fuzz.md](references/fuzz.md) |
| kio-gen marginal coverage | [references/kio-gen-coverage.md](references/kio-gen-coverage.md) |
| Production normalizer reachability | [references/normalizer-reachability.md](references/normalizer-reachability.md) |

Anchors: [`AGENTS.md`](../../../AGENTS.md) § Universal rules — Optimizations are justified, not assumed; Bugs surface; never hide them; Findings do not broaden scope. Generator coverage also anchors [`TESTING.md`](../../../TESTING.md) § Coverage tracking. Reports locate missing evidence; they do not establish semantic conformance, performance wins, or correctness by themselves.

Before execution, read [`local-tools.md`](../../topics/local-tools.md) and [`local-ci.md`](../../topics/local-ci.md). Inspect selected harness flags, actual scope, prerequisites, and preservation behavior. Report missing pinned tools before installing them; follow existing session authority for provisioning. Use the configured temporary root and compiler-cache readiness rules. Run selected harnesses sequentially, recording the commit/worktree state, tool versions, commands, budgets, selected implementations/cases, and retained artifacts. Inspect failures before continuing dependent work. Do not substitute `reports/all.sh` when a subset or different budgets are requested.

Use bounded defaults from the procedures. A request to run all reports does not authorize an unbounded mutation campaign or a repository-wide full implementation matrix: check aggregate scope against AGENTS.md before launching. Preparation-only requests permit source inspection and planning, not instrumented execution. In the `audit` umbrella this skill remains Tier 7, explicitly opt-in.

Combine selected findings without losing raw evidence or scope: distinguish harness failures, incomplete runs, test gaps, proven equivalents, unresolved outcomes, and stale artifacts. Cite artifact paths and exact source/mutant/input evidence; report skipped reports and remaining uncertainty. Compare prior runs only when source, tool, configuration, and coverage selection make the comparison meaningful. Never turn a bounded sample or a green infrastructure smoke into an exhaustive claim.

Default: report only; report execution may create its normal ignored artifacts, but findings do not authorize source edits, artifact deletion, tracker changes, or commits. With an authorized fix directive, read [`audit-fix-mode.md`](../../topics/audit-fix-mode.md), classify each finding against the effective authorized contract, and repair only findings within that directive. Unresolved contract/design choices remain decisions for the user. Verify regression fail-before/pass-after at the appropriate layer and follow exact full-matrix golden coverage rules; rerun only affected reports with a stated budget. Preserve bug-exposing goldens and raw campaign evidence. Summarize fixes, validation, and unresolved findings without changing commit/sign-off/push policy.
