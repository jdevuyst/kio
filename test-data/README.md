# Tests

Kio source code consumed as input data by CI. The harness (`ci/run-tests.sh`), the per-case checks (`ci/checks/per-case/`), the per-test-type orchestrators (`ci/checks/orchestrators/`), and the support crates (`ci/infra/kio-ci-scheduler-rs/`, `ci/infra/kio-gen-rs/`, `ci/infra/kio-prime-check-rs/`, `ci/infra/kio-test-runner-rs/`, `ci/infra/highlight-agreement-js/`) all live under `ci/`. For testing strategy (philosophy, layers, decision tree for "where does my new test go"), see [`TESTING.md`](../TESTING.md).

Seven corpora:

- **`test-data/goldens/`** — hand-written, version-controlled cases pinning specific observable behavior across implementations.
- **`test-data/emissions/`** — one-backend, success-only cases compiling independently authored public hosts or pinning durable specified/measured generated-artifact facts. Contract in [`test-data/emissions/README.md`](emissions/README.md).
- **`test-data/highlight-corpus/`** — lexer-level tokenization fixtures used by the three-way highlight-agreement check.
- **`test-data/kiodoc-cases/`** — markdown fixtures driven by `kio doc check` / `kio doc build`.
- **`test-data/poc/`** — adopter-grade Kio modules carrying a comprehensive API surface and a battery of `equiv` law blocks; CI runs `kio check` + `kio test` + per-backend `main` execution against each.
- **`test-data/castles/`** — larger composed Kio programs (algorithms, toy games, parsers, simulations, puzzle solvers) run end to end through the standard runner path; success-only, sampled per run.
- **`test-data/contrib/`** — community-contributed cases from the contribution lane ([`CONTRIBUTING.md`](../CONTRIBUTING.md) § Contribute an example program or library); small self-contained cases — a program (`run.args`), a library (`run.test-only`), or a bug reproducer (`KNOWN_FAILING`) — each directory named after its contributor. Case contract in [`test-data/contrib/README.md`](contrib/README.md).

The generative-test producer (`ci/infra/kio-gen-rs/`) emits a seeded batch of Kio test cases into a temp dir; the harness consumes them in the same shape as `test-data/goldens/`. Failures uncovered there get hand-promoted into the goldens corpus via the seed printed on every run.

## Running

The orchestrators in `ci/checks/orchestrators/` invoke the harness with the right flags for each corpus:

- `sh ci/checks/orchestrators/golden-tests.sh` — runs `test-data/goldens/` against every Kio impl.
- `sh ci/checks/orchestrators/emissions-tests.sh` — runs every `test-data/emissions/<backend>/<case>/` locally; case sampling is per backend.
- `sh ci/checks/orchestrators/generative-tests.sh` — generates a batch via `ci/infra/kio-gen-rs/` and runs it.
- `sh ci/checks/orchestrators/kiodoc-tests.sh` — runs `test-data/kiodoc-cases/` plus a smoke `kio doc check` on `docs/`.
- `sh ci/checks/orchestrators/poc-tests.sh` — runs `test-data/poc/` against every applicable Kio impl.
- `sh ci/checks/orchestrators/castle-tests.sh` — runs a reproducible sample of `test-data/castles/` against every applicable Kio impl (`--all-cases` runs the whole corpus).
- `sh ci/checks/orchestrators/contrib-tests.sh` — runs a reproducible sample of `test-data/contrib/` against every applicable Kio impl (`--all-cases` runs the whole corpus; an empty corpus is a clean pass).
- `sh ci/checks/orchestrators/highlight-tokens.sh` / `highlight-agreement.sh` — `test-data/highlight-corpus/`.

Pass `--impls=<name>[,<name>...]` to restrict to specific impls. Pass `-u` (or `--update-expected`) to refresh goldens. Pass any number of POSIX EREs as positional arguments to run only matching cases. Use `--exclude=<regex>` (repeatable) to drop matching cases; excludes win over includes.

Cases run in parallel by default (one process per case, up to the native
scheduler's `std::thread::available_parallelism` result). Override with
`--jobs=<N>`; `--jobs=1` forces serial.

For direct invocation of the harness:

```bash
sh ci/run-tests.sh \
  --cases-dir=test-data/goldens \
  --cache-base=<cache-dir> \
  --impl-def=name=<n>,kio=<bin>,runner=<bin>,target=<target>,prime-kio=<reduced-bin>
```

`--cases-dir=<dir>`, `--cache-base=<dir>`, and at least one `--impl-def=` are required. `--impl-def=` takes a comma-separated `key=value` list: `name` (display label), `kio` (compiler binary path), `runner` (executes a `kio build` output), `target` (selects the runner among the per-backend bins), and optional `prime-kio` (the separately-built reduced Kio' compiler). A check bearing `# REQUIRES: prime-kio` makes that field mandatory; an impl check bearing `# REQUIRES: runner` skips a compile-only `runner=SKIP` row. Repeat `--impl-def=` for multiple implementations in one run.

## Golden / emission boundary

Goldens own cross-implementation language, diagnostic, and runtime behavior. A golden case's `*.kio`, `run.sh`, fixtures, and expected files never directly read, copy, grep, patch, import, or native-compile files emitted by a host-language backend. The standard runner protocols receive generated output directories opaquely, and generic harness-owned phase checks remain allowed.

Kio' is the deliberate exception: it is a specified backend-neutral phase artifact, so a golden may read or assemble emitted Kio' when Kio' itself, its independent verifier, or the `dyn_load_prime` boundary is the subject.

The fixed runner is the default public-interface oracle. Only backend-specific generated-host evidence that the runner cannot naturally and independently demonstrate lives in [`test-data/emissions/`](emissions/README.md): `HOST_INTERFACE` compiles a checked-in fixed host authored independently from the public backend spec; `ARTIFACT_SHAPE` pins only a durable specified fact or a portable proxy justified by a recorded measurement. An emission that merely duplicates a runner-proven contract is misplaced. Exact private helper spellings and other no-filesystem invariants belong in unit or mutation tests. Do not migrate incidental private assertions; delete them.

## Golden test case layout (`test-data/goldens/`)

Cases are grouped under `test-data/goldens/<NN_category>/` directories where `NN` is the expected exit code and `category` matches a row in [`specs/exit-codes.md`](../specs/exit-codes.md). Filters match against the relative path. The one carve-out is the `90_runtime_exit` tier: its prefix is a tier label, not an asserted compiler category, because it holds cases that build and run and then assert the **program's own** runtime exit code ([`specs/exit-codes.md`](../specs/exit-codes.md) § Runtime exit codes). Cases there carry per-case `expected.exit` values — a program that builds cleanly and then exits `1` or `90` both belong — so `NN` there does not constrain a case's `expected.exit`.

Each case is a directory containing:

- `workdir/` — the directory where the case's Kio commands run. It may
  contain one package or an intentionally invalid package layout. When a
  case needs the shared POC elaborator library (`derive!`, `match!`, the
  spine palette, …), it declares a **path dependency** on the canonical
  `elab` package — a `<local>.dep.kio` at the package root:

  ```kio
  // elab.dep.kio
  dependency elab;
  source { path "<rel>/elab.pkg.kio"; }
  ```

  and imports the re-rooted modules (`use elab/derive …;`,
  `use elab/match …;`). Depending on elab materializes its modules under
  an `elab/` tree at the case package root, and that **re-rooted tree is
  committed** — the case ships its dependency's materialized closure, so a
  fresh checkout runs without a fetch step. The committed modules are
  re-rooted derivatives (`module derive;` → `module elab/derive;`), not
  copies of the canonical source. Symlinking or byte-copying a *canonical*
  elab module into a case is still forbidden (it predates dependencies);
  CI rejects both via
  [`ci/checks/repo-lint/golden-reference-deps.sh`](../ci/checks/repo-lint/golden-reference-deps.sh),
  while [`ci/checks/repo-lint/dep-materialization.sh`](../ci/checks/repo-lint/dep-materialization.sh)
  asserts the committed materialized tree is present and canonical.
- Exactly one of:
  - `run.args` — standard harness-owned execution. `ci/run-tests.sh`
    runs `cd workdir`, discharges `equiv` blocks with `"$KIO_BIN" test`
    unless the case carries `IS_KIO_PRIME`, then builds with
    `"$KIO_BIN" build "$KIO_TARGET"` and invokes the target runner through
    the harness's identity proxy. This path requires exactly one discoverable
    package, whose manifest is a regular, non-symlink `workdir/*.pkg.kio`
    file; an extra nested package root requires a custom `run.sh` that owns
    package selection. The proxy supplies `--package-name` from that filename;
    the source package name is only the backend's default-namespace input. A
    portable
    `--artifact-namespace <target-id>=<namespace>` entry in `run.args`
    overrides that default for one target: the harness consumes the mapping
    and forwards plain `--artifact-namespace <namespace>` to that target's
    runner. All other prepared `run.args` tokens follow, then
    `out/$KIO_TARGET`.
    Successful `kio test` output is suppressed; on failure the captured
    stdout/stderr are replayed to stderr and build/run are skipped.
    Arguments are whitespace-separated simple tokens; quoting, escapes,
    comments, shell operators, globs, and variable expansion are not
    supported. Empty, blank, and whitespace-only `run.args` files mean
    the default runner invocation.
  - `run.test-only` — a library (no `main`): the harness-owned path minus
    the runner. `ci/run-tests.sh` runs `cd workdir`, discharges `equiv`
    blocks with `"$KIO_BIN" test` (unless `IS_KIO_PRIME`) and builds with
    `"$KIO_BIN" build "$KIO_TARGET"`, but invokes no `$KIO_RUNNER` —
    nothing is executed. For a case whose subject is `kio test` + codegen
    validation, with no program to run.
  - `run.sh` — custom execution script for cases that are not expressible
    as the standard test+build+runner path. The harness exports `$KIO_BIN`,
    `$KIO_RUNNER`, and `$KIO_TARGET`; `$KIO_BIN` is the configured Kio
    implementation routed through the harness's shared compiler admission.
    `$KIO_RUNNER` is the identity proxy described above. It supplies a default
    only for one discoverable package in a regular, non-symlink top-level
    manifest and only when the invocation has no explicit split or `=`
    `--package-name`; other scripts supply their ordered `--package-name` /
    optional plain `--artifact-namespace` descriptors to that same handle. Its
    `PATH` is
    prefixed with harness-local proxies for `kio`, `kio-prime`, `cargo`,
    `rustc`, `go`, `javac`, `swiftc`, and `ghc`: compiler-producing commands
    enter the shared compiler resource without reacquiring the case's work
    slot or the optional capacity-one `cargo` resource, while cheap Kio
    management commands and non-compiling `go` commands pass through directly.
    Scripts use `$KIO_BIN`
    when they need the configured implementation rather than an ambient
    spelling. The
    script must preserve the inherited `PATH` prefix and invoke those tools by
    bare command name; absolute or previously captured tool paths bypass the
    harness proxy and are not valid corpus authoring. A case with more than one
    execution file, or none, is a harness error. Prefer
    `run.args` for plain compiled-output execution. A custom `run.sh` may still
    invoke `$KIO_RUNNER` when the script also asserts behavior outside the
    standard path, such as CLI, formatter, or a Kio'-artifact round trip.
    A golden `run.sh` must not inspect or native-compile generated
    host-language files; move that subject to the emissions corpus.
- `input.stdin` *(optional)* — checked-in fixture stream fed to the
  executable test runner's stdin. Standard `run.args` cases receive it
  automatically; custom `run.sh` cases redirect it themselves.
- `expected.stdout` — exact stdout oracle.
- `expected.exit` — success/failure oracle; for goldens this must match
  the case's `<NN_category>` bucket, except in the `90_runtime_exit` tier,
  whose cases assert the program's own runtime exit code per case (see
  § Golden test case layout above and [`specs/exit-codes.md`](../specs/exit-codes.md)
  § Runtime exit codes).
- `run.args` FFI protocols — FFI-protocol goldens select non-default
  runner protocols here, for example:

  ```text
  --protocol bridge-product-roundtrip
  ```

  Protocol names and behavior are documented in
  [`ci/infra/kio-test-runner-rs/README.md`](../ci/infra/kio-test-runner-rs/README.md).
- `ffi_*` cases *(convention)* — FFI-protocol goldens. They keep `workdir/`
  as the Kio package only and carry `run.args`; protocol runner code owns
  the host-language-native host and bridge driving.
- `IS_KIO_PRIME` *(optional)* — empty marker file asserting that every regular-module `*.kio` source under `workdir/` parses against the formal Kio' grammar in [`specs/prime.md`](../specs/prime.md). The contract is **biconditional**: the marker exists iff every regular-module file parses as Kio'. Verified mechanically by the per-case check at [`ci/checks/per-case/prime-marker.sh`](../ci/checks/per-case/prime-marker.sh), which uses the standalone parser at [`ci/infra/kio-prime-check-rs/`](../ci/infra/kio-prime-check-rs/) as an oracle. Pass `-u`/`--update-expected` to bring markers into agreement after a parser change.
- `SKIP_KIO_PRIME_RUN` *(optional)* — empty marker opting out of `--prime-only` runs (i.e. it tells the runner to skip the case under Kio'-only impls like `kio-prime@js`) even when its sources parse as Kio'. Use for parseable cases whose execution depends on full-surface behavior that the Kio'-only impl deliberately rejects, such as flat calls over multiple function layers or custom scripts that invoke a surface-only CLI.
- `DYN_LOAD_PRIME` *(optional)* — empty marker opting a `00_success` case into the `dyn_load_prime` interpreter differential (the `dyn-load-prime@kio-prime` bucket; [`TESTING.md`](../TESTING.md) § Test layers). The marker is **derived, not curated**: [`ci/checks/repo-lint/dyn-load-prime-coverage.sh`](../ci/checks/repo-lint/dyn-load-prime-coverage.sh) recomputes each case's eligibility from its standard `run.args` path, successful expected exit, root manifest build block, and the shared protocol registry's exact dyn-load-prime support classification. `compile-only` needs only a loadable image; `construct-only` instantiates the exact empty-host package without an export; main protocols instantiate the selected exact host contract and invoke `main` in its exact declaring module; supported export protocols instantiate the same way and drive their surface scripts. Exact entry and contract compatibility are checked at runtime. The lint fails when an eligible case lacks the marker, an ineligible case carries one, or a marked case's manifest lacks a `target kio-prime` block.
- `SKIP_DYN_LOAD_PRIME` *(optional)* — opts an **eligible** case out of the differential. Unlike the other skip markers it must not be empty: its content states the reason, and the coverage lint rejects an empty one. Excessively expensive interpreted stress workloads may use this exception while retaining compact dynamic semantic coverage and full stress coverage on applicable backends; name that coverage in the reason and follow [Stress and semantic coverage](../TESTING.md#stress-and-semantic-coverage). It is not a way to hide an interpreter defect. A case that is mechanically ineligible needs no skip file — ineligibility is derived.
- `UNKNOWN_BUILD_TARGET` *(optional)* — empty marker for a custom `run.sh`
  whose subject is the build error from an unrecognized target. Requires
  expected exit 40 and at least one declared target. The case is target-agnostic
  for implementation routing because its invalid target cannot name a runner;
  it asserts the compiler diagnostic, not host execution. It retains ordinary
  implementation/case sampling, case-binary checks, and the custom-script
  exclusion from compile-only implementations. Never use it to bypass a valid
  target's applicability or a missing build block.
- `RUN_EARLY` *(optional)* — scheduling-only marker for a case whose work would
  otherwise become the tail of a buffered parallel corpus run. Filters, target
  applicability, implementation sampling, and case sampling first freeze the
  canonical worklist; the harness then dispatches every selected
  `(case, binary)` unit for marked cases before unmarked units, preserving
  canonical order within each tier. The marker never adds a case or
  implementation, never promotes a sampled-out case into the build-and-run
  tier, and never changes canonical buffered reports. It applies to every
  parallel `ci/run-tests.sh` pass in which the case is selected. One-worker,
  single-unit, interactive `--show-output`, and `--update-expected` runs
  execute canonically.
- `SKIP_KIO_FMT_CHECK` *(optional)* — empty marker opting out of the
  per-case canonical-format check. Use only when the case's subject is
  deliberately non-canonical input, such as formatter round-trip or
  parser-acceptance fixtures.
- `expected.stderr.ignore` — empty marker file that skips the stderr
  assertion. This is the default for successful tool, build, and run
  cases: `expected.exit=0` already proves success, and stderr remains
  available for future warnings, diagnostics, progress, or other
  implementation-specific notes.
- `expected.stderr.grep` — POSIX ERE matcher. Each non-empty,
  non-comment line is a regex passed to `grep -E`; every regex must
  match somewhere in the actual stderr for the case to pass. Blank
  lines and lines beginning with `#` (after optional leading whitespace)
  are ignored, so the file can carry header comments. **The file must
  contain at least one regex line** — an empty or comment-only `.grep`
  is a harness error, not a free pass. Use this whenever stable stderr
  subject facts matter. Prefer one or two regexes pinning facts the case
  actually exercises, not full diagnostic prose. The case's bucket plus
  `expected.exit` already assert the exit-code category.
- `expected.stderr` — exact byte-for-byte stderr oracle. This is rare
  and reserved for cases where the whole stderr stream is intentionally
  under test, such as CLI UX silence, panic text, or program-authored
  stderr. The case must document why extra stderr would be a regression,
  either in the custom `run.sh` header or with a tight program-local
  source comment.

Exactly one stderr policy file (`expected.stderr.ignore`,
`expected.stderr.grep`, or `expected.stderr`) is required per case. If
more than one is present, or none is present, the runner fails the case
as misconfigured.

**Writing a `.grep` file.** One or two regex lines is usually right.
Pin the load-bearing stderr subject facts the test exercises, not full
diagnostic prose. Do not normally pin exact line/column positions, full
error sentences, category phrases such as `type mismatch` or `parse
error`, rendered source snippets, or complete help text; the case's
bucket plus `expected.exit` already assert the exit-code category.
Filenames, syntax tokens, names, roles, target names, and required
`help:` / `note:` presence are appropriate only when they are the case's
actual subject.

### Where case commentary goes

Custom case commentary — what the case asserts, the exit-code rationale, links to the bug or spec section it pins — lives in the comment header of `run.sh`. Keep it tight; one paragraph is usually enough.

Per-case `*.kio` files carry **only program-local notes**, and only where the WHY is non-obvious. Don't restate a standard `run.args` case's harness mechanics in the source. Default to no comments in `*.kio` if the code speaks for itself.

**Surface forms in non-`IS_KIO_PRIME` cases.** When a case is not marked `IS_KIO_PRIME`, its `*.kio` sources should use the surface forms described in [`specs/language.md`](../specs/language.md) (operators, tuple literals, `if`/`else`, `match!`, spine elaborators such as `widen_sum!` / `fit!`, label-generated sugar, and algebraic elaborators when the case is specifically about them) rather than directly calling the underlying Kio' intrinsics. The exception is cases whose *subject* is the intrinsic itself.

### Invoking cases

`ci/run-tests.sh` either runs the standard `run.args` path or invokes a
custom `run.sh` from the case directory. In both modes stdout, stderr,
and exit status are captured and checked against the expected files.
Custom `run.sh` cases keep the same aggregate contract: their expected
files describe the script's observable result as a whole. Do not add
phase-scoped `build.*` / `run.*` files; if a phase-specific assertion
matters, encode it in the script and keep the oracle at the aggregate
stdout/stderr/exit level.

## Emission case layout (`test-data/emissions/`)

Driven by [`ci/checks/orchestrators/emissions-tests.sh`](../ci/checks/orchestrators/emissions-tests.sh). Cases live at `test-data/emissions/<backend>/<case>/`, where `<backend>` is a shipping host backend and the single build target in the case's root package matches it. A justified multi-artifact `HOST_INTERFACE` case may also build nested packages with that same sole target; the root package remains a real built artifact and the applicability record. Every case is success-only, uses a custom `run.sh`, and carries exactly one empty marker: `HOST_INTERFACE` or `ARTIFACT_SHAPE`. `HOST_INTERFACE` requires `host/`; `ARTIFACT_SHAPE` forbids it. The complete structural and evidence contract is [`test-data/emissions/README.md`](emissions/README.md).

The orchestrator runs all emission cases by default. Explicit sampling uses the backend directories as buckets, so `--case-count=1` chooses one case per available backend. Positional filters match `<backend>/<case>` and follow `--`; for example:

```sh
sh ci/checks/orchestrators/emissions-tests.sh \
  --impls=FULL_IMPL_MATRIX -- '^rust/public_facade$'
```

Emission evidence never satisfies a runtime backend-completeness cell. Keep the corresponding cross-backend runtime behavior in goldens and use emissions only for the generated-host boundary itself.

## POC case layout (`test-data/poc/`)

Driven by [`ci/checks/orchestrators/poc-tests.sh`](../ci/checks/orchestrators/poc-tests.sh). The corpus carries adopter-grade Kio modules — one per topic — whose source files are shaped so a user can copy them verbatim into their own package. See [`test-data/poc/README.md`](poc/README.md) for the corpus contract.

Cases live at `test-data/poc/<topic>/` (flat layout — no exit-code buckets, since every POC is a `00_success` shape by construction). Each case directory carries:

- `workdir/` — the reusable root Kio package, with `<topic>.pkg.kio` (carrying its `build { ... }` block), root library modules, and any committed materialized `*.dep.kio` dependencies. Library package names use the topic name (`optics`, `hkt`, `list`, …), not abbreviations.
- `workdir/demo/` *(library POCs)* — a separate runnable package that imports the root package through `library.dep.kio`, binds host requirements with small adapter modules, and keeps runner-facing `testapi` modules out of the reusable library. Worked examples whose subject is the runnable mechanism itself may keep the entry package directly under `workdir/`.
- `run.sh` — chains root-package `kio check`, root-package `kio test`, then demo-package or worked-example build + run, all of which must exit `0`. `kio check` / `kio test` output is redirected so `expected.stdout` snapshots `main`'s output only.
- `expected.stdout`, `expected.exit`, and exactly one stderr policy file
  — checked against the actual run output, same as `test-data/goldens/`;
  `expected.exit` must contain `0`, with or without a final newline.

The full menu of per-case markers (`IS_KIO_PRIME`, `expected.stderr.ignore`, …) and the per-case-check pipeline apply identically to POC cases — they go through the same `ci/run-tests.sh` harness as goldens. The Kio' phase-path check uses a successful direct target build as its eligibility test, independent of the case's custom `run.sh`, so buildable POC root packages participate too.

## Castle case layout (`test-data/castles/`)

Driven by [`ci/checks/orchestrators/castle-tests.sh`](../ci/checks/orchestrators/castle-tests.sh). The corpus carries larger composed Kio programs — algorithms, toy games, parsers, simulations, puzzle solvers — run end to end through the standard runner path. They sit between minimized goldens and adopter-grade POCs: realistic composition, success-only, never reshaped to dodge a compiler gap. See [`test-data/castles/README.md`](castles/README.md) for the corpus contract.

Cases live at `test-data/castles/<name>/` as direct children (flat layout — no exit-code buckets and no nested case directories; every castle is a `00_success` shape by construction). Each castle directory carries:

- `README.md` — what the program models, the shape of `input.stdin` and where any fixture seed appears, what stdout means, what this castle adds.
- `workdir/` — the Kio package, with `<pkg>.pkg.kio` (carrying its `build { ... }` block) and the regular-module tree that provides `main`.
- `run.args` — the only execution file; normally empty (standard build-then-run path). A castle never carries a `run.sh`.
- `input.stdin` *(optional)* — present iff the package reads stdin via `read_ascii_line()`. Randomness is fixture data: a castle that needs a seed reads it from `input.stdin` and implements any PRNG in Kio.
- `expected.stdout`, `expected.exit`, and exactly one stderr policy file
  — checked against the actual run output, same as `test-data/goldens/`;
  `expected.exit` must contain exactly `0`.

The full per-case marker menu and the per-case-check pipeline (`fmt-canonical`, `prime-marker`, `kio-prime-roundtrip`, rlib-cache warm-hit) apply identically — castles go through the same `ci/run-tests.sh` harness as goldens. The orchestrator samples which checked-in castles *build and run* (printing the effective seed and selection); `--all-cases` runs the whole corpus. Sampling scopes the build+run tier only: a sampled-out castle still runs every case-binary check, including the `dep-canonical` drift check on its committed materialized dependency tree. When a castle exposes a Kio bug, the bug is fixed in the same change and covered by a focused regression golden under `test-data/goldens/`.

Because the default run is a reproducible sample, a failure reports the seed it ran under (`sample-cases: seed = <s>`) and the selected castle names. Reproduce a sampled failure by pinning that seed, or by naming the castle directly:

```sh
sh ci/checks/orchestrators/castle-tests.sh --case-seed=<s>
sh ci/checks/orchestrators/castle-tests.sh --all-cases -- <castle-name>
```

## The kiodoc corpus (`test-data/kiodoc-cases/`)

Driven by [`ci/checks/orchestrators/kiodoc-tests.sh`](../ci/checks/orchestrators/kiodoc-tests.sh), not `golden-tests.sh`. Exercises `kio doc`. Because `kio doc check` / `kio doc build` are package-rooted (they read the `docs` field of the `build { ... }` block in the package file — see [`specs/cli.md`](../specs/cli.md) § `kio doc`), **each case directory is itself a one-package fixture**: it carries a `pkg.pkg.kio` whose build block declares a `docs` field, plus the markdown / `.kio` content the case exercises, and its `run.sh` invokes `kio doc check` (or `kio doc build`) from the case root. The standard `expected.stdout` / stderr-policy / `expected.exit` golden contract applies. Most error-category cases use `expected.stderr.ignore` since the diagnostic wording is implementation-specific.

## Highlight corpus (`test-data/highlight-corpus/`)

Lexer-level tokenization fixtures used by two CI checks:

- `ci/checks/orchestrators/highlight-tokens.sh` diffs `kio debug tokens` over each fixture's `source.kio` against `expected.tokens.json`. Pass `-u` / `--update-expected` to refresh after a vocabulary change.
- `ci/checks/orchestrators/highlight-agreement.sh` extends that to a three-way agreement check: kio-rs reference ↔ tree-sitter grammar (`tools/tree-sitter-kio/`) ↔ TextMate grammar (`tools/textmate-kio/`). The Node.js driver lives at `ci/infra/highlight-agreement-js/check.mjs`, runs fixtures concurrently, and loads tree-sitter in-process from a freshly generated WASM parser. Every positive classification must match the reference. Provider-dependent bracket runs and bracket-led call arguments may remain silent under an exact aggregate source span because the independent grammars do not carry the compiler's selected operator scope.
