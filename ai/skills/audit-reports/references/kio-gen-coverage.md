# kio-gen Coverage-Delta Audit

Anchor: [`TESTING.md`](../../../../TESTING.md) § Coverage tracking — the optional kio-gen coverage audit described there is this skill; it measures the generative layer's marginal contribution over the goldens-alone baseline.

Run the checked-in reporting harness with a fixed implementation for comparable passes:

```sh
sh reports/kio-gen-coverage-delta.sh --impls=kio@js --keep-work
```

Pre-req: `cargo-llvm-cov` (`sh ci/impl-toolchain.sh install-report-tools`).

The script computes the delta between two `cargo-llvm-cov` runs:

1. **Baseline** — the hand-written goldens harness alone.
2. **With kio-gen** — the same goldens harness plus a freshly generated kio-gen batch.

`delta = (with-kio-gen) - (baseline)` is the additional line coverage kio-gen contributes.

Useful flags:

```sh
sh reports/kio-gen-coverage-delta.sh --impls=kio@js --count=100 --seed=42 --keep-work
sh reports/kio-gen-coverage-delta.sh --impls=kio@js --count=25
sh reports/kio-gen-coverage-delta.sh --impls=kio@js --seed=42 --keep-work
```

The default count is 500 generated cases. Use a smaller count only to validate harness health; report the count and seed with the result. The script currently configures only JS and Rust, even for its `FULL_IMPL_MATRIX` label; do not claim all-backend evidence. Its no-flag default runs both implementations across both broad workloads. Review aggregate cost and obtain explicit user direction for effectively corpus-wide full-matrix work under AGENTS.md; an unqualified report invocation does not authorize it.

`--seed` seeds the generator only. `SAMPLE_IMPL` independently chooses backend assignments in each pass, so its delta can include selection noise. Prefer a fixed `--impls=kio@js` (or another justified fixed supported subset), state that backend scope, and verify that both goldens passes have identical selections before attributing a delta to generation. Keep `--keep-work` so logs survive for interpretation.

Before accepting a real run, inspect the retained compiler identity, default-feature release build log, coverage environment, cohort wrappers, and raw profiles. The script builds one instrumented compiler in an isolated target, executes its retained snapshot in both passes, and requires profiles from each golden cohort and, for a nonempty generated batch, the generated cohort. Cohort `%m` merge pools bound raw-profile growth by binary signature rather than invocation count. It compares the golden execution rows and source/line coverage denominators before reporting a delta. The reduced Kio' compiler and host tools remain uninstrumented support. These checks establish workload identity and evidence integrity, not semantic conformance; source inspection or the stub self-test cannot replace the real run. Failed runs preserve their isolated state and evidence for diagnosis.

## Interpretation

These thresholds are rough workload heuristics, not acceptance criteria or evidence of semantic completeness. Interpret them only after validating the profiles, identical baseline selection, and denominator:

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

**Default: report only.** If invoked with a fix directive, follow [`ai/topics/audit-fix-mode.md`](../../../topics/audit-fix-mode.md): use the delta as evidence for concrete generator or corpus fixes, and do not land speculative coverage padding.
