---
name: audit-mutation
description: Run cargo-mutants over the kio-rs semantic core (typer / elaborator registry / substitute / normalizer / optimizer / Prime path) and classify every non-caught mutant as a test gap, semantic equivalent, or unresolved
allowed-tools: Read, Grep, Glob, Bash
---

# Mutation audit

Drive [`reports/mutation.sh`](../../../reports/mutation.sh) over the kio-rs **semantic core**: the typer (`typecheck_core` + `typecheck_full` and its production submodules), the elaborator registry, the substitute pass, the normalizer (`normalization`), the backend-neutral optimizer, and the Prime path (`src/prime/`). cargo-mutants applies small syntactic edits ("mutants") to the source and runs the test suite; a mutant that survives — passes the suite unchanged — names a place where the test corpus is too weak to catch a behavioral change.

Anchors: AGENTS.md § Universal rules — "Optimizations are justified, not assumed" names this audit as the firing / no-op sweep for optimization coverage, and the harness charter lives above `MUTATION_TARGETS` in `reports/mutation.sh`.

Every non-caught mutant is assigned one of three classifications:

- **Test gaps** — the mutant changes behavior in a way the suite doesn't observe. Fix: add a regression test that distinguishes the original from the mutant.
- **Semantic equivalents** — the mutant is syntactically different but produces the same observable behavior (dead branches, redundant guards, names that are pun-equivalent under the typer's equivalence). Fix: record an exclusion in cargo-mutants's config so future runs skip them.
- **Unresolved** — the available evidence does not yet distinguish a test gap from an equivalent mutation. Keep the audit open and record the concrete question.

The audit's job is to walk the survivor list, classify each, and propose the appropriate fix.

Pre-req: `cargo-mutants` (`sh ci/impl-toolchain.sh install-report-tools`).

## 1. Scope review — the target list still matches the charter

The harness's reach is `MUTATION_TARGETS` in [`reports/mutation.sh`](../../../reports/mutation.sh), governed by the charter comment directly above it: mutate the **semantic core** — the passes and algorithms where a subtle behavioral change is a soundness or normalization bug that fuzz, the golden corpus, the Prime verifier, and the backend coverage matrix would not reliably catch. The target list rots silently (a pass is added, a file moves out from under a glob), so each audit run starts by re-deriving it:

1. Run `sh reports/mutation.sh --scope-self-test`. It derives the claimed production source inventory, excluding the explicit test-only include below `typecheck_full/`, proves every source is reached by a target, and compares the optimizer firing inventory with the actual catalog in `optimize_expr_in_mode`.
2. Inspect the inventories with `sh reports/mutation.sh --print-targets`, `--print-production-sources`, and `--print-firing-sites`. Also enumerate the surrounding pass/algorithm modules (`ls kio-rs/src/pass/ kio-rs/src/prime/`) so a new semantic component outside the existing claimed roots is still classified deliberately.
3. Apply the charter to the diff, in both directions:
   - **Gap** — a module that fits the charter but no glob reaches it (a new pass, a file that moved out from under its glob). Finding: propose adding a glob.
   - **Cruft** — a glob matching zero mutants (renamed/deleted module — `smoke-reports.sh`'s mutation smoke also fails loudly on this) or matching a module the charter excludes. Finding: propose removing it.

Settled sub-decisions, recorded so they are not re-litigated each run:

- Desugaring (`src/pass/desugar/`, `label_elab/`, `op_fold.rs`) — **out**. Surface-form removal is owned by the dedicated label/desugar goldens across backends plus the Prime verifier.
- Backends (`src/backends/**`) — **out**. Owned by the golden corpus + `audit-backend-completeness`; mutating them would drown the semantic core and duplicate that audit.
- Lexer + parser — **out**. `reports/fuzz.sh` owns the front end.
- Name resolution (`src/pass/resolve.rs`) — **out**. Binding changes surface as type errors or changed output across the golden corpus.
- Backend-neutral optimization (`src/pass/optimize.rs`) — **in**. Its catalog is behavior-preserving performance machinery, and its private firing can disappear without changing the behavioral output that other layers observe. The scope self-test derives the exact catalog arms, while the report-tool smoke checks those functions against cargo-mutants' real candidate list.

A scope change edits `reports/mutation.sh` (target list, claimed production roots, firing inventory, and charter comment together, so they never disagree). `ci/checks/orchestrators/smoke-reports.sh` always runs the dependency-free scope check and, when cargo-mutants is installed, validates one real combined candidate manifest. Report scope drift as a finding; under a fix-it directive, land it.

## 2. Choose the run mode

### Bounded exploratory run

```sh
sh reports/mutation.sh
```

Default wall-clock cap is 15 min — enough to sample mutants across the harness's target globs (`sh reports/mutation.sh --print-targets` lists them). The `--shuffle` flag inside the script randomizes mutant order so successive bounded runs sample different subsets; over weeks the picture fills in.

For a deeper run during triage, pass `--timeout=0` to disable the cap: `sh reports/mutation.sh --timeout=0`.

The broad wrapper treats cargo-mutants outcomes 0 (all caught), 2 (missed), 3 (timed out), and 124 (wall-clock cap reached) as report data and exits zero. Any other non-zero status is a real harness failure (cargo-mutants install drift, kio-rs build error, etc.) — surface it and stop. This legacy mode retains its existing external `timeout` dependency.

### Reproducible complete campaign

Use the deterministic modes when the result must prove exact coverage rather than accumulate shuffled samples:

1. From a clean working tree, authenticate the candidate set for the current commit:

   ```sh
   sh reports/mutation.sh --equiv-manifest="$TMPDIR/equiv-mutation-manifest"
   ```

   The directory contains `manifest.txt` and `manifest.json` plus metadata recording `schema`, `commit`, `cargo_mutants_version`, `config_blob`, `source_blob`, `features=default`, `manifest_blob`, `manifest_json_blob`, and `count`. Treat the directory as immutable campaign input; do not change commits, tool version, configuration, evaluator source, or features before running its shards.

2. Run every zero-based shard `K/N`, using distinct output directories. For example, one shard of a four-way campaign is:

   ```sh
   sh reports/mutation.sh --equiv-shard=0/4 \
     --manifest="$TMPDIR/equiv-mutation-manifest" \
     --output="$TMPDIR/equiv-mutation-shard-0"
   ```

   A shard re-authenticates the clean commit, tool, configuration, source, features, and full candidate list. It runs in place, without shuffling, and independently checks that cargo-mutants selected exactly the manifest lines whose zero-based positions belong to `K/N`. Bound deterministic work by choosing fine-grained shards; cargo-mutants owns its portable per-test timeout and process-tree cleanup. The script's `--timeout` option belongs only to the broad exploratory mode and is rejected here. Treat the generated `equiv-shard.meta` and four outcome files as immutable raw-run evidence; edit only `triage.tsv`.

3. In each output's `mutants.out/triage.tsv`, classify every missed, timed-out, or unviable mutant using three tab-separated fields:

   ```text
   equivalent<TAB><exact mutant line><TAB><non-empty rationale>
   test-gap<TAB><exact mutant line><TAB><non-empty rationale>
   unresolved<TAB><exact mutant line><TAB><non-empty rationale>
   ```

   Caught mutants need no triage entry. Never classify an uncertain outcome as equivalent merely to complete the campaign.

4. Validate all shards offline, repeating `--shard-output` for the same directories passed to `--output`:

   ```sh
   sh reports/mutation.sh --equiv-validate \
     --manifest="$TMPDIR/equiv-mutation-manifest" \
     --shard-output="$TMPDIR/equiv-mutation-shard-0" \
     --shard-output="$TMPDIR/equiv-mutation-shard-1"
   ```

   Supply the complete `0/N` through `(N-1)/N` set. Validation rejects missing, duplicate, foreign, stale, or partial selections and outcomes, authenticates the four category files (`caught`, `missed`, `timeout`, `unviable`) through `outcomes_blob`, checks `command_status` against the missed/timeout categories, and requires an exact triage entry for every non-caught mutant. Exit 0 means exact coverage with every non-caught mutant classified equivalent; exit 3 means at least one test gap or unresolved mutant remains, so the audit stays open. Any other non-zero status means the artifacts are malformed or inconsistent.

## 3. Locate the survivor report

cargo-mutants writes the broad report to `kio-rs/mutants.out/`; a deterministic shard writes the corresponding report under its explicit output directory:

- `mutants.out/missed.txt` — mutants that the test suite did NOT catch (survivors). One mutant per line: `<file>:<line>:<column>: <mutant-description>`.
- `mutants.out/caught.txt` — mutants that were caught. Not interesting for this audit.
- `mutants.out/timeout.txt` — mutants whose test run exceeded the per-mutant timeout. Treat as unresolved until analysis distinguishes a slow failure, a surviving behavior change, or a genuine timeout-class change such as an infinite loop.
- `mutants.out/unviable.txt` — mutants that did not compile or otherwise could not run. They are non-caught outcomes and need classification in a deterministic campaign.
- `mutants.out/diff/<n>.diff` — per-mutant diff showing the source edit. Use this for classification.

Deterministic shard reports also retain their authenticated selection and campaign metadata. The offline validator, not a hand count, is authoritative for whether the shard union exactly covers the manifest.

## 4. Classify each survivor

For each line in `missed.txt`, and for every timeout or unviable outcome in a deterministic campaign:

1. **Read the mutant diff.** `cat kio-rs/mutants.out/diff/<n>.diff` shows what cargo-mutants changed in the source.
2. **Read the surrounding code.** Understand what the original code is supposed to do.
3. **Decide:**
   - **Test gap** — the mutant changes observable behavior (a different code path runs, a different type is produced, a different error is raised) but no test asserts on that difference. Fix: a new test that distinguishes them.
   - **Semantic equivalent** — the mutant is provably equivalent. Examples:
     - Replacing a redundant guard (`if cond && cond { ... }` → `if cond { ... }`).
     - Reordering commutative operations under value semantics.
     - Removing dead branches the typer's exhaustiveness check has already proven unreachable.
     - Renaming a local binder. (cargo-mutants usually doesn't do this, but variant-pattern reorderings sometimes are.)
   - **Unresolved** — needs deeper investigation. Record the concrete uncertainty and surface it for human review; don't speculate.

## 5. Propose follow-ups

For each non-caught mutant, propose one of:

- **Add a regression test** (test gap): name the test and describe what input distinguishes original from mutant, citing the mutant diff. The killer must be an **in-crate kio-rs test** (the module's `tests` block or the `typecheck_full.rs` harness) — the harness runs only `cargo test`, so a golden can never kill a mutant. When the pinned behavior is also **user-visible through the public surface** (an exit category, a diagnostic, an `equiv` verdict, runtime output), propose a **golden twin** under `test-data/goldens/` alongside the killer: the golden pins the same behavior across implementations and pipelines, which no in-crate test does. Check the corpus for an existing case already pinning the shape before proposing one — the twin rule adds missing cross-implementation coverage, not duplicates. Internal-invariant gaps (slot arithmetic, memo internals, interner state) stay unit-test-only per [`TESTING.md`](../../../TESTING.md).
- **Exclude in cargo-mutants's config** (semantic equivalent): the config is [`kio-rs/.cargo/mutants.toml`](../../../kio-rs/.cargo/mutants.toml) (`reports/mutation.sh` runs from `kio-rs/`, where cargo-mutants reads `.cargo/mutants.toml`). Each entry carries a comment explaining *why* it's equivalent — bare exclusions rot — and must obey the file's anchoring discipline: function name plus exact rewrite, never line numbers, and never a pattern that sweeps sibling mutants the review didn't classify (watch prefix collisions between function names; and since mutant names differ only by position, a same-shaped rewrite at several sites of one function excludes all sites or none — admissible only when every site is reviewed-equivalent).
- **Investigate further** (unresolved): name the file:line, paste the diff, and flag for human review.

## 6. Stale-survivor sweep

cargo-mutants doesn't clean its `mutants.out/` between broad runs by default. A finding that was real last week may have been fixed since — the file path / line might have shifted or the code might have been removed entirely. Cross-check each survivor's file:line against the current source. A deterministic campaign instead rejects metadata or selections that no longer match its authenticated commit, tool, configuration, evaluator source, features, and manifest.

- If the file or line no longer exists, the finding is stale. Note for cleanup but don't propose a fix.
- If the file/line still exists but the mutant description doesn't match the current code, also likely stale (the line was edited).

## 7. Report cumulative trends, not just this run

Because each bounded run samples a subset, the *interesting* number is the cumulative survivor count across runs, not this single run's count. If `mutants.out/missed.txt` from a prior run is still around, merge: which survivors persist across runs (deep test gaps), which are new (recent code without coverage), which dropped out (fixed).

## How to report

Group findings into:

1. **Test gaps with proposed regression tests** — survivors that admit a clear new test. Cite mutant file:line, mutant diff, proposed test.
2. **Semantic equivalents with proposed `mutants.toml` exclusions** — survivors that are provably equivalent. Cite mutant file:line, the equivalence argument, the exclusion line.
3. **Unresolved non-caught mutants** — need human review. Cite the outcome, diff, and concrete question.
4. **Stale survivors** — entries pointing at code that no longer exists. Propose cleanup of `mutants.out/`.
5. **Cross-run patterns** — survivors that persist across multiple weeks point at structural gaps. Surface those above per-run noise.
6. **Harness regressions** — if `reports/mutation.sh` itself failed (cargo-mutants install drift, kio-rs build error), name the symptom.
7. **Scope drift** — gaps or cruft the scope review (§ 1) surfaced in `MUTATION_TARGETS`, each with its charter argument.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md) — for each finding, land the proposed regression test, `mutants.toml` exclusion, or `reports/mutation.sh` scope change. Unresolved mutants stay as report-only findings; don't guess.
