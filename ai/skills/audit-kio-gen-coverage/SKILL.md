---
name: audit-kio-gen-coverage
description: Measure kio-gen's marginal line-coverage contribution over the goldens-alone baseline and interpret what the delta says about generator health
allowed-tools: Read, Grep, Glob, Bash
---

# kio-gen Coverage-Delta Audit

Anchor: [`TESTING.md`](../../../TESTING.md) § Coverage tracking — the optional kio-gen coverage audit described there is this skill; it measures the generative layer's marginal contribution over the goldens-alone baseline.

Run the checked-in reporting harness:

```sh
sh reports/kio-gen-coverage-delta.sh
```

Pre-req: `cargo-llvm-cov` (`sh ci/impl-toolchain.sh install-report-tools`).

The script computes the delta between two `cargo-llvm-cov` runs:

1. **Baseline** — the hand-written goldens harness alone.
2. **With kio-gen** — the same goldens harness plus a freshly generated kio-gen batch.

`delta = (with-kio-gen) - (baseline)` is the additional line coverage kio-gen contributes.

Useful flags:

```sh
sh reports/kio-gen-coverage-delta.sh --count=100 --seed=42
sh reports/kio-gen-coverage-delta.sh --impls=kio@js --count=25
sh reports/kio-gen-coverage-delta.sh --keep-work
```

The default count is 500 generated cases. Use a smaller count only to validate harness health; report the count and seed with the result.

## Interpretation

- **Large positive, around 5pp or more** — kio-gen is reaching code paths the hand-written goldens do not. The generator is earning its keep.
- **Small positive, under about 1pp** — the generator mostly overlaps the goldens. Consider expanding generator surface coverage or absorbing its unique value into focused goldens.
- **Zero or negative** — investigate the harness before drawing distribution conclusions. Likely causes are generated-case failures, stale kio-gen grammar support, no profile data, or the instrumented binary not being used.

If the delta is unexpectedly low, inspect the preserved logs with `--keep-work` and check:

- Did both goldens and generated batches pass?
- Did `cargo llvm-cov` produce nonzero `*.profraw` data?
- Is the generated batch typecheck-clean under the selected implementations?
- Did the selected implementation set cover the code areas under discussion?

## Report Shape

Report:

1. `seed`, generated `count`, and selected `impls`.
2. `goldens-alone` line coverage.
3. `goldens + kio-gen` line coverage.
4. Delta in percentage points.
5. Interpretation and suggested follow-ups.

**Default: report only.** If invoked with a fix directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md): use the delta as evidence for concrete generator or corpus fixes, and do not land speculative coverage padding.
