# Implementation evidence

Implementation arguments, measurements and decision rationale, organized by
subject. Each record states what its evidence establishes and where the claim
stops; historical measurements are not guarantees for another version or
workload.

- [Backend decisions](backend-decisions.md) — implementation choices and their correctness arguments.
- [Backend performance](backend-performance.md) — generated-code and backend mechanism measurements.
- [Call-stack safety](call-stack-safety.md) — staged experiment, depth limits, host-interface changes, and measured costs.
- [Compiler performance](compiler-performance.md) — checking, representation, preparation and tooling measurements.
- [Generation baselines](generation-baselines.md) — compiler selection, harness ownership, and bounded native compatibility validation.

These records support engineering decisions, not new behavioral authority.
The [specifications](../specs/) define the language and host contracts;
[formal companions](../specs/formal/) hold language meta-theory.
[POC case studies](../docs/poc/) explain library-specific arguments, and
[`reports/`](../reports/) contains executable reporting harnesses.
