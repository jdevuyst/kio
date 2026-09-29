# Castles: large composed Kio program corpus

Driven by [`ci/checks/orchestrators/castle-tests.sh`](../../ci/checks/orchestrators/castle-tests.sh).

A **castle** is a larger, coherent Kio program — an algorithm, a toy
game, a parser, a planner, a simulation, a puzzle solver, a
utility-shaped program — run end to end through the standard runner
path. Castles exist to exercise *realistic composition*: source that
looks like Kio someone would actually write for a small real project,
rather than a minimized regression test, an adopter-grade reference
library, or a workaround for today's compiler.

## How castles differ from the other corpora

- **Goldens** (`test-data/goldens/`) pin minimized observable contracts
  bucketed by exit-code category. A golden is the smallest source that
  pins one behavior or one regression.
- **POCs** (`test-data/poc/`) are adopter-grade reference modules: a
  comprehensive API surface plus a battery of `equiv` law blocks
  discharged by `kio test`. A POC is meant to be copied verbatim into a
  user's package.
- **Generated tests** (`ci/infra/kio-gen-rs/`) supply randomized Kio'
  coverage of the core language.
- **Castles** are composed whole programs. They are *not*
  adopter-grade reference libraries and they do *not* chain
  `kio check` / `kio test` law suites. Their job is the integration
  layer: realistic composition that catches the regressions that
  hand-minimized goldens and generated Kio' programs miss.

The point is diversity through composition. When a castle exposes a Kio
bug, the castle keeps its natural source shape, the bug gets fixed in
the same change, and the fix lands with a focused regression golden
under `test-data/goldens/` — bugs get fixed, not hidden, and a golden
that exposes one is never deleted or skipped to keep the corpus looking
green. A castle is never reshaped to dodge a compiler gap.

## Corpus shape

Castles are POC-like in layout but not in purpose: a flat directory of
success-only case directories, every one of which exits `0`. The corpus
directory is `test-data/castles/`, and each castle is a **direct child**
of it — there are no exit-code buckets and no nested case directories.

```text
test-data/castles/<name>/
  README.md
  workdir/
    <pkg>.pkg.kio        # package file, carrying the build { ... } block
    ...                  # the regular-module tree that provides main
  run.args
  expected.stdout
  expected.stderr.ignore
  expected.exit
  input.stdin            # present iff the package reads stdin
```

The case goes through the same [`ci/run-tests.sh`](../../ci/run-tests.sh)
harness as goldens and POCs, so the full per-case marker menu
(`IS_KIO_PRIME`, the three mutually-exclusive `expected.stderr*` policy
files, `SKIP_KIO_FMT_CHECK`, …) and the per-case-check pipeline
(`fmt-canonical`, `prime-marker`, `kio-prime-roundtrip`, the rlib-cache
warm-hit check) apply identically. See
[`test-data/README.md`](../README.md) § Golden test case layout for the
shared file contract.

### `run.args`-only execution

`run.args` is the **only** execution file a castle uses; a castle never
carries a `run.sh`. `run.args` is normally empty, which selects the
standard build-then-run path: the harness runs `kio test` (a no-op load
on a castle, which declares no `equiv` law suite), builds `workdir` for
the impl's target, then invokes the backend runner with the arguments
from `run.args` followed by `out/<target>`. The harness supplies the source
package name and selects any target-qualified artifact-namespace mapping as
described in [`test-data/README.md`](../README.md) § Golden test case layout;
a non-empty `run.args` otherwise supplies plain runner arguments
(whitespace-separated simple tokens).

### `expected.exit`

Every castle is a success case. `expected.exit` contains exactly:

```text
0
```

A castle whose program exits non-zero is, by definition, not a castle —
it is either a goldens-bucket regression case or a bug to fix.

### `input.stdin` fixture semantics

A castle that reads input declares the canonical runner host fn
`read_ascii_line()` (and the `string_*` helpers documented in
[`ci/infra/kio-test-runner-rs/README.md`](../../ci/infra/kio-test-runner-rs/README.md)),
and ships an `input.stdin` fixture. The standard `run.args` path
redirects `input.stdin` into the runner automatically. `read_ascii_line()`
consumes normalized lines, returns `()` at EOF, resets per execution,
and grants no other host I/O. Castle parser fixtures are ASCII-only.

A castle that declares `read_ascii_line()` **must** ship an
`input.stdin`; a castle that reads no input omits the fixture.

### Seed as fixture

Randomness is fixture data, not orchestrator state. A castle that needs
a seed reads it from `input.stdin`, documents the line shape in its
README, and implements any PRNG in Kio itself. The orchestrator's
sampling controls only *which* checked-in castles run; it never injects
a program seed. There is no `--program-seed` flag and no runner-provided
`seed()` host fn.

### Output readability

An interactive or replay-style castle should print enough context that
`expected.stdout` is understandable on its own, without manually merging
it with `input.stdin`. Prefer a compact transcript or summary in domain
terms — dimensions, the seed read from the fixture, a command summary, a
checksum, the final state, a score, a status. The runner does not echo
`read_ascii_line()` input as a side effect; expected stdout is entirely
the Kio program's own output.

## Running

```sh
sh ci/checks/orchestrators/castle-tests.sh --all-cases    # every castle
sh ci/checks/orchestrators/castle-tests.sh                # default sample
sh ci/checks/orchestrators/castle-tests.sh --case-count=5 --case-seed=123
sh ci/checks/orchestrators/castle-tests.sh -- maze ledger # named castles
```

The orchestrator samples which castles *build and run*, and prints the
effective seed plus the selected castle names, so a sampled failure
reproduces from its seed. `--all-cases` runs the whole corpus. A
sampled-out castle still runs every case-binary check — `fmt-canonical`,
`prime-marker`, and the `dep-canonical` drift check on its committed
materialized dependency tree — so sampling trades away build-and-run
coverage and nothing else. See `castle-tests.sh --help` for the full flag
set and the default cap.

## Authoring

Each castle's `README.md` should be useful to a reader who has not read
the source: what the program models or computes, the shape of
`input.stdin` and where any fixture seed appears, what stdout means, and
what this castle adds relative to the rest of the corpus.

Adding a new castle follows a documented authoring workflow: survey the
existing corpus, choose a diversity target, reuse POC packages through
path dependencies where natural, ship the success-only `run.args`
contract, and fix any discovered bug with a focused regression golden
plus a doc update. This file states the corpus contract; that workflow
is the procedure for meeting it. The worked reference example is
[`maze_replay/`](maze_replay/).
