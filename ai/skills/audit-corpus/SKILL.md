---
name: audit-corpus
description: Audit the five behavioral test-data/ corpora — goldens, emissions, poc, castles, contrib — against the applicable shared anti-evasion sweeps and each corpus's own evidence or maturity contract
allowed-tools: Read, Grep, Glob, Bash
---

# Corpus audit

This skill audits the five behavioral Kio corpora under `test-data/`, each with its own contract but sharing one spine of anti-evasion rules:

- **`test-data/goldens/`** — hand-written, minimized cases pinning one observable behavior each, bucketed by exit-code category.
- **`test-data/emissions/`** — backend-first generated-host evidence: independently authored public-interface hosts or durable spec/measurement-backed artifact facts.
- **`test-data/poc/`** — adopter-grade reference libraries: a comprehensive API surface plus a battery of `equiv` law blocks, meant to be copied verbatim into a user's package.
- **`test-data/castles/`** — larger composed programs (algorithms, toy games, parsers, simulations, puzzle solvers) run end to end, exercising realistic composition.
- **`test-data/contrib/`** — community-contributed cases from the contribution lane, each named after its contributor and kept green from then on.

The skill has two halves. **Shared checks** (§§ 1–9) apply to each corpus where the section names it; the backend-specific emission contract deliberately differs from the cross-implementation language corpora. These checks are the dedicated cover for two hard AGENTS.md universal rules that no other audit sweeps for directly. **Per-corpus contracts** state the differences that make each corpus distinct — the value in spelling them out.

Read first: AGENTS.md § Universal rules (the four rules named in § Shared principles), [`TESTING.md`](../../../TESTING.md), [`test-data/README.md`](../../../test-data/README.md), and the per-corpus contracts [`test-data/emissions/README.md`](../../../test-data/emissions/README.md), [`test-data/poc/README.md`](../../../test-data/poc/README.md), [`test-data/castles/README.md`](../../../test-data/castles/README.md), and [`test-data/contrib/README.md`](../../../test-data/contrib/README.md). The cross-backend caveat-citation mechanics overlap [`audit-test-strategy`](../audit-test-strategy/SKILL.md) § 8 and [`audit-spec-drift`](../audit-spec-drift/SKILL.md) § 7 — this skill is the history-and-source-shape lens plus the per-corpus maturity contracts; defer the two-banner-plus-emitter enumeration to those.

Findings are bugs in the corpus, not in the audit — when a check fires, fix the case (or the missing golden / the underlying emitter), do not relax the check.

## Shared principles

### The two anti-evasion rules

- **AGENTS.md § Universal rules — "Bugs surface; never hide them."** The violation tests: a bug-exposing case deleted, skipped, or bucket-demoted instead of staying in place as a failing case (the error buckets, a `run.sh` comment naming the bug, or a contrib `KNOWN_FAILING` marker are the sanctioned forms); or a backend dropped from a case's `build { ... }` block instead of the backend being made capable.
- **AGENTS.md § Universal rules — "Goldens demonstrate behavior, not workarounds."** The violation test: a case's source carries scaffolding a user wouldn't naturally write — explicit annotations dodging an inference gap, a polymorphic newtype split monomorphic, monomorphisation skipping a codegen path, `alias` scaffolding peers don't carry — present only to make a failing backend pass.

This skill also enforces two further universal rules on the corpora it audits: **AGENTS.md § Universal rules — "No partial implementations"** (a castle- or POC-discovered bug is fixed in the same change and pinned by a focused regression golden) and **AGENTS.md § Universal rules — "Per-backend limitations: only the genuinely impossible, mutual-cited"** (a per-backend opt-out that defangs the cross-backend consistency promise without the required matching spec/guide banners and emitter citation is a finding).

### Exec-file discipline

Every case chooses exactly one execution contract:

- `run.args` for the standard harness-owned path: build `workdir/` for `$KIO_TARGET`, then invoke `$KIO_RUNNER` with the listed args followed by the output directory. Preferred for a program.
- `run.test-only` for a library (no `main`): the harness runs `kio test` + `kio build` with no runner. Preferred over a `run.sh` that only discharges laws.
- `run.sh` for custom language-corpus execution: CLI behavior, formatter assertions, specified Kio'-artifact round trips, multi-step package flows, or the POC contract (`kio check` + `kio test` + build + run) — behavior the standard paths cannot express.
- `run.sh` is mandatory for every emission because the case, rather than `$KIO_RUNNER`, owns its public-host or artifact-shape assertion. Emissions have no `run.args` or `run.test-only`.

Outside emissions and the POC workflow, the preference is `run.args` / `run.test-only` over `run.sh`; a `run.sh` should exist only when there is a real nonstandard assertion the declarative paths cannot express (§ 4). Castles never carry a `run.sh` at all (§ Castle 3); contrib `run.sh` cases are maintainer-review-gated.

### Path portability and self-containment

Checked-in corpus files are portable (no machine- or checkout-specific paths — the class list and sweep are § 6) and self-contained (each case ships its own package plus its declared, committed dependency closure — § 5, § POC 10).

### KNOWN_FAILING semantics

`KNOWN_FAILING` is an empty marker file that tells `ci/run-tests.sh` a language-corpus case is *expected* to fail — a bug reproducer that *should* work but doesn't. The gate stays green, the run prints a `warn` line, and CI fails if a marked case ever passes (a stale marker), forcing the marker's removal once the bug is fixed. It is the corpus form of "Bugs surface; never hide them": the failing case stays in place, visible, until the bug is fixed. The contrib corpus is its primary user. It is *tracked debt*, not permanent state (§ 8). Emissions are success-only and forbid `KNOWN_FAILING`.

## Shared checks (applicable corpora under `test-data/`)

### 1. Deleted or demoted cases

Walk recent history over the corpora and the emitter:

```bash
git log --since="3 months ago" --diff-filter=D --name-only -- \
  test-data/goldens/ test-data/emissions/ test-data/poc/ \
  test-data/castles/ test-data/contrib/
git log --since="3 months ago" --diff-filter=R --find-renames --name-status -- test-data/goldens/
```

For each **deleted** case (`--diff-filter=D`) and each golden **moved into a failure bucket** (a rename whose destination directory is a higher-numbered error tier — `00_success/foo` → `14_type_error/foo`, `40_build_error/`, `15_elaborator_error/`, …):

- Read the deleting / moving commit. A paired emitter / typer / elaborator fix is **not** a deletion rationale: fixing the bug is why the focused regression must remain. Incidental coverage in a broader case is not a substitute either. A rename or consolidation is admissible only when the same commit preserves equally focused coverage of the exact regression and the history makes that one-to-one replacement explicit. A diagnostic-selection fixture may retire when prior authority removes that selection promise and focused coverage of every constituent defect remains.
- A deletion whose commit message amounts to "fixed", "case was failing", "flaky on rust", "covered elsewhere", or "no longer needed" without that exact focused replacement is a finding. A case can retire as success coverage only when the effective authorized contract no longer admits the behavior; preserve focused negative coverage of the newly rejected spelling where the language-surface testing contract requires it.
- A `00_success` → error-tier move is admissible **only** when prior authority made the behavior an error and the paired spec change records that decision. The same commit's spec edit cannot authorize the demotion. A move whose justification is "the emitter can't do this yet" is a finding: that is exactly the failing-golden-stays case, but the golden must stay where it demonstrates the intended behavior with a `run.sh` comment naming the bug, not be relabeled as if the rejection were correct.

Goldens are the corpus where bucket-demotion is meaningful; for emissions and the flat POC / castle / contrib corpora the same signal is a deletion, or (outside emissions) a `KNOWN_FAILING` marker added to paper over a regression rather than to track a genuine new bug reproducer. For each finding, cite the commit and case path, then state whether exact regression or durable evidence was lost or the demotion lacked prior contract authority.

Also inspect negative goldens that combine independent defects capable of
producing different reported diagnostics or exit categories. A bucket and
`expected.exit` may select among those overlapping states only when a
command-specific spec explicitly defines that precedence. Otherwise the case
must isolate the defect whose diagnostic or category it pins; a multi-error
precedence fixture is a finding even when the current implementation reports
the expected result deterministically. This does not prohibit a test that
deliberately collects and reports several same-category errors together.

### 2. Per-backend opt-out guards without a three-site caveat

The cross-backend consistency promise is that a language-corpus case building for multiple backends runs identically under each. The classification ladder for a single-backend / backend-omitting case and the `|| exit 0`-per-case-guard check are stated once in [`audit-test-strategy`](../audit-test-strategy/SKILL.md) § 8 — apply them here over goldens, POCs, castles, and contrib. Target-independent compiler/front-end/CLI cases, including a debug probe that uses one routing target only to enter a shared compiler phase without consuming its artifact, and explicit Kio'-phase cases do not acquire a backend claim merely from that routing target; inspect the script and assertions before classifying it. Do not apply the backend-omission check to emissions: their `test-data/emissions/<backend>/<case>` layout deliberately selects exactly one backend, and their evidence never substitutes for a missing runtime backend.

```bash
grep -rL 'target rust {' test-data/goldens/00_success/*/workdir/*.pkg.kio 2>/dev/null
grep -rL 'target js {'   test-data/goldens/00_success/*/workdir/*.pkg.kio 2>/dev/null
```

(Adjust the glob and target tokens to the live package syntax — verify against `specs/package.md` § Build target files first; target ids are bare identifiers inside the `build { ... }` block. Run the same walk over `test-data/poc/*/workdir/*.pkg.kio`, `test-data/castles/*/workdir/*.pkg.kio`, and `test-data/contrib/*/workdir/*.pkg.kio`.)

Two corpus-owned refinements: a `KNOWN_FAILING` contrib reproducer deliberately declares only the backend(s) where the bug occurs (`test-data/contrib/README.md`) — not an opt-out finding; and an omitted backend plus a bare `run.sh` comment with **no** matching spec/guide banners and emitter citation stays a finding — a fixable shortcoming deferred by relabeling, per AGENTS.md § Universal rules — "Per-backend limitations: only the genuinely impossible, mutual-cited".

### 3. Sources reshaped to dodge an inference gap

The hardest signature to catch mechanically, and the most corrosive: a `.kio` source that carries defensive scaffolding its peers don't, present only to make a backend pass. Two signals — a structural reshape, and a workaround-narrating comment.

**Reshape tells.** Look for these, then read the case to judge whether the scaffolding is load-bearing for the case's *subject* or defensive cover for an emitter / inference gap:

- **Annotation asymmetry.** A case carrying explicit type-arguments / type ascriptions where structurally-similar sibling cases elide them. Inference-elision is the natural surface style (see `ai/topics/surface-forms.md` — elided type-args are preferred); a lone case that spells them out is a candidate for "added an annotation to dodge an inference gap." Cross-check with the case's intent: a `typecheck_*` case whose *subject* is explicit-annotation behavior is fine.
- **Monomorphization scaffolding.** A polymorphic newtype split into several monomorphic newtypes, or a generic function specialized to one concrete type, where the natural authoring would be a single polymorphic declaration — the "splitting one polymorphic newtype into several monomorphic ones" / "monomorphising on a single type to skip codegen paths" reshape named in the rule. Grep for clusters of near-duplicate monomorphic declarations:

  ```bash
  grep -rnE 'newtype +_?[A-Z][A-Za-z0-9]*(I32|I64|F64|String|Bool)\b' test-data/
  ```

  A case with `WrapI32` / `WrapString` / `WrapBool` siblings where a `Wrap[A]` would read naturally is a candidate finding.
- **Alias scaffolding asymmetry.** `alias` declarations a case introduces that its peers don't, used to side-step a path the emitter mishandles directly.

**Workaround-narrating comments.** A source comment that narrates a workaround, an inference-dodge, a monomorphisation-to-skip-codegen, or a "doesn't work yet so we…" is the signal of a hidden bug documented as intended behavior:

```bash
find test-data -name '*.kio' ! -path '*/out/*' -print0 \
  | xargs -0 grep -niE 'workaround|work around|TODO|FIXME|HACK|XXX|dodge|monomorph|annotat.* to (avoid|dodge|work)|doesn.?t (yet|work)|not yet (support|work)|compiler (bug|gap|limitation)|until .* (fix|support)' \
    2>/dev/null
```

A hit is a candidate finding — read the comment in context. A comment explaining a *domain* choice (why the PRNG constants are overflow-free, say) is fine; a comment explaining how the source was bent around the compiler is a finding, and the fix is to fix the compiler and let the source read naturally.

For each candidate, the finding is only confirmed if the scaffolding's purpose is to dodge a backend / inference shortcoming rather than to demonstrate the case's stated subject. When a build is available, the decisive test is: does removing the scaffolding produce an emitter / inference failure? If so, the scaffolding is masking a bug — the fix is to make the emitter capable and let the source read naturally, per "Goldens demonstrate behavior, not workarounds." List candidates you couldn't disprove statically separately so the user can confirm with a build.

### 4. Custom runner scripts that should be `run.args` or `run.test-only`

Per § Exec-file discipline, a language-corpus case chooses exactly one contract. A `run.sh` whose whole body is `cd workdir` then `kio test` (or `kio check` + `kio test`), with no `$KIO_RUNNER` invocation, is a library discharge that `run.test-only` expresses declaratively — flag it as a `run.test-only` candidate (existing cases needn't be converted, but new ones should prefer it). Emissions are excluded from this preference because every emission requires its own `run.sh`; audit them under § Emissions instead. Then audit every non-emission `run.sh` that invokes `$KIO_RUNNER`:

```bash
find test-data -name run.sh ! -path 'test-data/emissions/*' ! -path '*/out/*' \
  -exec grep -Hn 'KIO_RUNNER' {} \;
```

For each hit, read the script header and body:

- **Plain standard path only** — `cd workdir`, the standard `run.args` pre-test/build/run sequence (possibly with runner args or `input.stdin` redirection the standard path already supports) is a finding. Replace it with `run.args` and let the harness own the plumbing; use a zero-byte `run.args` when no runner argv is needed.
- **Custom assertion plus runner** — admissible when the script first asserts something outside the standard path and then runs the output to preserve the case's observable result. Examples: `kio fmt --check`, a specified Kio' build/rebuild round trip, or `kio check` / `kio test` behavior. The script should have a leading comment naming that custom assertion; otherwise list it as a documentation candidate. Generated host-backend greps or native validation are never admissible in a golden; move durable evidence to emissions.
- **POC workflow script** — admissible when it drives the POC contract (`kio check`, `kio test`, build, then run) rather than a single golden's plain compiled-output path.

This is a hygiene heuristic, not a harness law: a custom script may legitimately invoke `$KIO_RUNNER`, but the invocation should not be the only reason the script exists. A `run.sh` found under `test-data/castles/` is a § Castle 3 finding independently (castles are `run.args`-only).

Also inspect every custom `run.sh` that invokes `cargo`, `rustc`, `go`, `javac`,
`swiftc`, or `ghc`, and every one that assigns `PATH`. The harness prefixes
`PATH` with compiler-admission proxies; a script must preserve that prefix and
invoke these tools by bare command name. An absolute compiler path, a tool path
captured before the harness prefix, or a `PATH` replacement that drops the
prefix is a finding because it bypasses the shared compiler resource. This is
a manual authoring-contract review: do not add a syntax-only gate that mistakes
comments or wrapper variables for an executable compiler call.

Any such native compiler in a golden that consumes generated host-backend files
is a corpus-boundary finding even when compiler admission is preserved. An
emission `HOST_INTERFACE` may compile or load the public artifact in the
ordinary documented way, and an `ARTIFACT_SHAPE` script may inspect only its
durable named fact; those cases still obey the bare-command / inherited-`PATH`
resource rule.

### 5. elab modules included instead of depended on

The shared POC elaborator library — `elaborator_util.kio`, `algebraic_elaborators.kio`, `spine_elaborators.kio`, `match.kio`, `derive.kio` (and the rest of the `elab` package) — has a single canonical home under `test-data/poc/elab/workdir/` (package `elab`). A test case that needs the elaborators **depends on `elab`**: a `<local>.dep.kio` (`dependency elab; source { path "<rel>/elab.pkg.kio"; }`) at the package root, importing the re-rooted modules (`use elab/derive …;`, `use elab/match …;`). Depending on elab materializes its modules under an `elab/` tree at the case root, and that re-rooted tree is **committed** (the case ships its dependency's materialized closure). The committed modules are re-rooted derivatives (`module derive;` → `module elab/derive;`), not byte copies of the canonical source — so this section's byte-identical-copy check (b) never flags a committed materialized tree.

The two pre-dependency workarounds are forbidden: a tracked filesystem link into `test-data/poc/elab/workdir/`, and a byte-identical real copy of a canonical module. The gate-enforced counterpart is [`ci/checks/repo-lint/golden-reference-deps.sh`](../../../ci/checks/repo-lint/golden-reference-deps.sh), which fails CI on either. This section is the manual backstop and explainer; run it the same way to confirm:

```bash
git ls-files -s test-data | awk '$1=="120000"{print $4}' \
  | while read -r f; do case "$(readlink "$f")" in *elab/workdir*) echo "TRACKED LINK: $f";; esac; done

for name in elaborator_util.kio algebraic_elaborators.kio spine_elaborators.kio match.kio derive.kio; do
  canon="test-data/poc/elab/workdir/$name"
  find test-data -type f -name "$name" ! -path "test-data/poc/elab/workdir/*" \
    -exec sh -c 'cmp -s "$1" "$2" && echo "BYTE-IDENTICAL COPY: $1"' _ {} "$canon" \;
done
```

Each `TRACKED LINK` / `BYTE-IDENTICAL COPY` hit is a finding: replace it by depending on `elab` and importing the re-rooted module (remove the included file, add `<local>.dep.kio`, `use elab/<name-stem> …;`, and a `elab/testapi;` glob to the consumer's `bridge` for the role types the elaborators import). A reference-module-named file that is not byte-identical and not a tracked filesystem link (a genuine purpose-built variant a single case ships) is admissible and not flagged. If you suspect a "variant" is actually stale drift of the canonical module, that is a separate spec/POC-sync concern; record it but do not auto-convert it.

### 6. No absolute or checkout-local paths, no stray symlinks

Goldens, emissions, POCs, castles, and contrib cases are checked-in corpus fixtures. They must not name a developer checkout, literal temp scratch location, home directory, sibling worktree, Windows drive path, or local `file://` URL in `.kio` source, package metadata, dependency metadata, `run.sh`, `run.args`, expected output, fixtures, or README files. The portable harness-provided `$TMPDIR` variable is expected in emission scripts. Dependency `source { path "..." }` entries are relative from the package root; an absolute `.dep.kio` `path` is never portable. If a custom git-dependency script needs a per-run local repository, generate the `.dep.kio` with the local path syntax accepted by git rather than a `file://` URL.

```bash
git grep -nE 'file://|path[[:space:]]+"(/|~|[A-Za-z]:[\\/])|/home/[[:alnum:]_.-]+|/Users/[[:alnum:]_.-]+|/tmp/|kio-worktrees|(^|[[:space:]])~/' -- test-data
```

Every hit is a candidate finding; classify only true domain-fixture strings as legitimate. A `.dep.kio` absolute path or local `file://` URL is always a finding.

Symlinks in the corpus are the other portability hazard. List every tracked symlink under `test-data`:

```bash
git ls-files -s test-data | awk '$1=="120000"{print $4}'
```

Any symlink under `test-data/contrib/` is a finding — the contrib contract forbids symlinks outright (`test-data/contrib/README.md`). A symlink pointing into `elab/workdir/` is a § 5 finding (the pre-dependency reuse workaround). Other symlinks need judgment against the corpus contract.

### 7. `run.sh` equiv cases that never discharge (`kio test`)

The standard `run.args` path runs `kio test` as a build prerequisite (`ci/run-tests.sh` `execute_case`) before it reads `run.args`, so every `run.args` case discharges its `equiv` blocks automatically. A `run.sh` case opts out of that path and owns its own tool invocations — so a `run.sh` golden whose package declares `equiv` but never runs `kio test` ships those `equiv` laws **unexercised** (see [`TESTING.md`](../../../TESTING.md) § Test layers, per-case checks).

The gate-enforced counterpart is [`ci/checks/repo-lint/equiv-discharge.sh`](../../../ci/checks/repo-lint/equiv-discharge.sh), which fails CI on a `run.sh` golden that declares `equiv` (at statement position) in a discharge-reachable bucket yet never invokes `kio test`. This section is the manual backstop and explainer; run it to confirm:

```bash
sh ci/checks/repo-lint/equiv-discharge.sh
```

For each finding the gate reports, the fix is to discharge the equiv in the case's `run.sh` — add a `kio test` line. Worked examples: `00_success/typecheck_onto_collapse_alpha_equiv` (`kio dep fetch` → `kio check` → `kio test`) and `00_success/check_module_tree_file_selector`, whose subject is `kio check`'s selector and which discharges with `"$KIO_BIN" test >/dev/null` so its snapshot stays empty. A case whose subject is another subcommand should still discharge — `kio test` runs fine and is cheap, and the corpus re-discharges freely anyway. If the package's only `equiv` lives in an imported dependency, `kio test` still asserts a clean load with nothing to discharge, so it still belongs. If `kio test` genuinely cannot pass (it fails before discharge), the case belongs in an error-before-discharge bucket, not a reachable one — surface that as a bucket mismatch rather than working around it.

A case whose `expected.exit` is a pre-discharge error (parse / type / elaborator / dependency / …) is exempt automatically; `kio test` can't reach discharge there. This is the golden-corpus form; the POC and castle corpora carry the same obligation through their own `kio test` lines (§ POC 6 actually runs it; § Castle 7's orchestrator runs it).

### 8. `KNOWN_FAILING` cases are tracked open bugs

Per § KNOWN_FAILING semantics (tracked debt, not permanent state), list every marker across the whole corpus and check they aren't accumulating:

```bash
find test-data -name KNOWN_FAILING | sed 's:/KNOWN_FAILING::'
```

For each, the underlying Kio bug should eventually be fixed — at which point a maintainer removes the marker (the harness's stale-marker failure forces this) and pins the fix with a focused regression golden, per "Bugs surface; never hide them." Report the full list so it stays on the radar; flag any marker that has lingered across many releases or whose reproducer no longer looks like a real bug (in which case the case, not just the marker, should go). The marker is not a way to land a permanently-red case and forget it.

A `KNOWN_FAILING` below `test-data/emissions/` is immediately a corpus-contract finding: emissions are success-only and carry `expected.exit` exactly `0`.

### 9. Focused claims reach and discriminate their subject

A focused regression case is valid only when it reaches the phase, path, or
assertion it advertises and fails for that reason on the unfixed code. A
nonzero exit, matching bucket, or broad stderr regex is not sufficient when an
earlier independent error can satisfy the same oracle.

For each recently added or materially revised focused regression, first state
its advertised subject from the case name, header, README, and introducing
commit. Then inspect the fixture as one composed program: the `run.sh` control
flow or declarative execution marker, all marker files, committed materialized
dependencies and package manifests, bridge declarations, expected-exit bucket,
and stderr policy. Individually valid pieces can still compose into a case that
stops before its subject.

Require evidence that distinguishes the advertised path:

- A positive or control input reaches past every earlier prerequisite and
  discriminates the named phase or branch from a nearby path.
- The exact command used for the regression is observed **red on the unfixed
  code**, and its phase, exit category, diagnostic subject, or explicit
  assertion sentinel matches the claim. Reject a red caused by setup,
  dependency, parsing, or another earlier failure.
- The same command is green after the fix. Do not change selectors, fixtures,
  environment, or assertions between the red and green observations.
- A success or forwarding-path claim has a counterexample or mutation that
  disables the named path and makes the focused test fail. Merely observing the
  unmodified program succeed does not prove that path ran.

This is a semantic review, not a syntax lint: do not add fixture metadata or a
partial parser that guesses intent from names or shell text. Test-layer
placement, redundancy, runner independence, and backend parity remain owned by
[`audit-test-strategy`](../audit-test-strategy/SKILL.md); this check owns whether
the corpus fixture honestly proves the claim assigned to that layer.

## Goldens: exit-code buckets and minimized shape

`test-data/goldens/` is the minimized-regression corpus. The shared checks above cover its anti-evasion surface; what is distinct is the bucketed layout and per-case markers. Anchor against `test-data/README.md` § Golden test case layout.

### Goldens 1. Exit-code buckets

Cases live under `test-data/goldens/<NN_category>/` where `NN` is the expected exit code and `category` matches a row in [`specs/exit-codes.md`](../../../specs/exit-codes.md). A case's `expected.exit` must match its `<NN_category>` bucket. A case whose `expected.exit` disagrees with its bucket, or whose bucket `category` names no row in `specs/exit-codes.md`, is a finding. The `90_runtime_exit` tier is the exception: its `NN` is a tier label, not an asserted category (`specs/exit-codes.md` § Runtime exit codes) — runtime exit codes are the program's own, so its cases carry per-case `expected.exit` and a mismatch there is not a finding (`ci/checks/repo-lint/golden-bucket-exit.sh` encodes the same exemption).

### Goldens 2. Minimized shape

A golden is the **smallest** source that pins one behavior or one regression. A golden carrying the scale and composition of a real program belongs in `test-data/castles/`; a golden carrying an adopter-grade API surface plus a law battery belongs in `test-data/poc/`. A case misfiled in `goldens/` that markets itself as (or is shaped like) one of the other corpora is a finding — the fix is to move it, not to reshape the corpus. Minimization is never an excuse to reshape source around a compiler gap (§ 3).

### Goldens 3. `IS_KIO_PRIME` biconditional

`IS_KIO_PRIME` is an empty marker asserting that every regular-module `*.kio` source under `workdir/` parses against the formal Kio' grammar in [`specs/prime.md`](../../../specs/prime.md). The contract is **biconditional**: the marker exists iff every regular-module file parses as Kio'. It is verified mechanically by [`ci/checks/per-case/prime-marker.sh`](../../../ci/checks/per-case/prime-marker.sh), which uses the standalone parser at [`ci/infra/kio-prime-check-rs/`](../../../ci/infra/kio-prime-check-rs/) as an oracle. A marker present on a case whose sources are not all Kio', or absent from a case whose sources all are, is a finding — bring markers into agreement with `-u`/`--update-expected` after a parser change, never by hand-editing a case to dodge the oracle.

### Goldens 4. `ffi_*` protocol cases

`ffi_*` is the convention for FFI-protocol goldens. They keep `workdir/` as the Kio package **only** and carry `run.args` selecting a non-default runner protocol (for example `--protocol bridge-product-roundtrip`); the protocol runner code owns the backend-native host and bridge driving. Protocol names and behavior are documented in [`ci/infra/kio-test-runner-rs/README.md`](../../../ci/infra/kio-test-runner-rs/README.md). An `ffi_*` case that embeds a backend-native host or bridge inside `workdir/` instead of routing through a protocol runner, or that selects a `--protocol` name the runner doesn't define, is a finding.

### Goldens 5. `DYN_LOAD_PRIME` derived coverage

`DYN_LOAD_PRIME` gates the `dyn_load_prime` interpreter differential ([`TESTING.md`](../../../TESTING.md) § Test layers). Placement is **derived, not curated** — [`ci/checks/repo-lint/dyn-load-prime-coverage.sh`](../../../ci/checks/repo-lint/dyn-load-prime-coverage.sh) recomputes eligibility mechanically and CI fails on drift — so the audit owns only the judgment half the lint cannot decide:

- **Every `SKIP_DYN_LOAD_PRIME` reason still holds.** The skip file must state its reason; a reason that no longer applies (the vocabulary grew, the case's shape changed) is a finding — remove the skip and let the lint demand the marker.
- **Stress exclusions preserve semantic coverage.** Apply [Stress and semantic coverage](../../../TESTING.md#stress-and-semantic-coverage): an excessive-cost reason needs bounded measurement evidence, retained full stress coverage on applicable backends, and named compact dynamic cases covering the same semantic shapes. Check those cases actually run through the differential and assert the relevant results; a smaller input alone does not prove deep-stack safety. Flag an exclusion that conceals an interpreter defect, loses distinct semantic checks, or leaves independently maintained copies drifting apart. Do not flag a justified stress-only exclusion merely because the compact and stress workloads differ.
- **The lint's eligibility derivation matches the prose contract.** The rules in the lint's header, the tier description in `TESTING.md`, and the marker entries in `test-data/README.md` § Golden test case layout describe one contract; drift between script and prose is a finding.
- **Exclusions stay principled.** The vocabulary rule keys on the `testapi-dyn-load` env list in `ci/infra/kio-test-runner-rs/src/shared/protocol.rs`; when the loader host's capabilities grow, the lint mechanically demands markers on newly-eligible cases — but a *systemic* exclusion class (a host-fn family many cases share, a scalar kind the host could carry) is a coverage-gap finding to surface, not merely a set of unmarked cases.

### Goldens 6. Generated-host boundary and Kio' exception

Golden-owned `*.kio`, `run.sh`, `expected.*`, fixtures, and helper files never
read, copy, grep, patch, import, or native-compile generated host-backend files.
They may pass an output directory opaquely to a fixed ordinary runner protocol;
the harness and protocol implementation own ordinary artifact loading. Review
every golden reference to `workdir/out`, backend extensions and manifests, host
compiler commands, and shell text-processing commands. A case that asserts a
generated public facade or filesystem shape is misfiled and belongs under
`test-data/emissions/<backend>/` if the assertion is durable.

Kio' is the explicit exception. It is a specified backend-neutral phase
artifact, so a golden may read or assemble Kio' when its grammar, round trip,
verifier, evaluator, or dynamic-load boundary is the case's subject. Generic
harness-owned phase checks also remain valid. Treat neither as precedent for
direct host-backend artifact handling.

This boundary is a manual placement review. Inspect the references above in
context; do not add a syntax-only gate that tries to infer shell dataflow,
working-directory state, or whether a host artifact is consumed from spelling
alone.

## Emissions: generated-host evidence contract

`test-data/emissions/` is backend-first supplemental evidence. It proves one
public host-interface fact or one durable artifact-shape fact at a time; it is
never language/runtime coverage and never satisfies an
`audit-backend-completeness` runtime cell. Read
[`test-data/emissions/README.md`](../../../test-data/emissions/README.md) before
starting.

The fixed independent runner is the default public-interface oracle. A
`HOST_INTERFACE` emission is justified only when that runner cannot naturally
and independently demonstrate the backend-specific host fact; an emission
that merely repeats a runner-proven contract is a placement finding.

Run `sh ci/checks/repo-lint/emissions-corpus.sh` before the case-quality pass.
A failure is a structural corpus finding; a green result does not replace the
manual public-interface/durable-shape judgment below.

### Emissions 0. Backend-first layout; the filesystem is the registry

Every case is exactly `test-data/emissions/<backend>/<case>/`, where `<backend>`
is one of `js`, `ts`, `python`, `java`, `rust`, `go`, `swift`, or `haskell`.
There is no side registry, allowlist, or marker catalogue to keep in sync. A
case outside those buckets, nested below another case, or duplicated into a
language-neutral bucket is a finding.

### Emissions 1. Exactly one subject marker and a success-only case

Each case has exactly one empty `HOST_INTERFACE` or `ARTIFACT_SHAPE` marker,
one `run.sh`, one `workdir/`, `expected.stdout`, `expected.exit` containing
exactly `0`, and exactly one stderr-policy file. It has no `run.args`,
`run.test-only`, `KNOWN_FAILING`, or `oracles/`. `workdir/` has exactly one root
`*.pkg.kio`, and that package has exactly one build target matching its backend
bucket. Run the orchestrator's static contract validation and manually inspect
any disagreement rather than creating a second registry.

```sh
for case in test-data/emissions/*/*/; do
  [ -d "$case" ] || continue
  markers=0
  for marker in HOST_INTERFACE ARTIFACT_SHAPE; do
    if [ -e "$case$marker" ]; then
      [ -f "$case$marker" ] && [ ! -s "$case$marker" ] ||
        printf '%s: %s must be an empty file\n' "$case" "$marker"
      markers=$((markers + 1))
    fi
  done
  [ "$markers" = 1 ] || printf '%s: expected exactly one subject marker\n' "$case"
  find "$case" -maxdepth 1 \( -name run.args -o -name run.test-only \
    -o -name KNOWN_FAILING -o -name oracles \) -print
done
```

### Emissions 2. `HOST_INTERFACE` is an independent public-spec host

`HOST_INTERFACE` requires a checked-in `host/`. Normally its host source is
fixed and independently authored from `specs/backends/<backend>.md`. A
mechanically-wide capacity witness may generate repeated source solely from
fixed checked-in public-spec parameters when checking in the expansion would
obscure review. In either form the host is never copied, patched, discovered,
or extended by scraping current output. The ordinary host
compiler/runtime may import, compile, link, or load the generated artifact by
the documented public API. The host must not reach private symbols. An
incompatible facade change should make the unchanged host fail: that
independence is the oracle.

### Emissions 3. `ARTIFACT_SHAPE` pins only durable facts

`ARTIFACT_SHAPE` forbids `host/`. Its `# SUBJECT:` and optional `# CONTRACT:`
identify either a public backend-spec fact (such as a required path, manifest,
language mode, or named public file) or the portable artifact proxy retained
with a recorded optimization/resource measurement. Private helper spelling,
formatting, declaration order, and other incidental output are findings.
Private invariants that need no filesystem belong in unit or mutation tests;
an incidental old assertion with no durable role is deleted rather than
migrated.

### Emissions 4. Scratch-only build and compiler admission

Each `run.sh` has a non-empty leading `# SUBJECT:` comment. It creates
`scratch=$(mktemp -d "${TMPDIR:?}/...XXXXXX")`, installs a removal trap, copies
the immutable `workdir/` into scratch, changes into that copy before every
`"${KIO_BIN:?}" build "${KIO_TARGET:?}"`, and never changes into or builds the
tracked `workdir/`. A `HOST_INTERFACE` case copies its fixed `host/` to scratch
too. It preserves the harness-provided `PATH`, calls native compilers by bare
name, and never invokes `$KIO_RUNNER`; the orchestrator provides a rejecting
runner as a tripwire. The orchestrator pins `--cache-base` below its temporary
root, so no tracked output or shared runner cache is involved.

### Emissions 5. Local-all and GitHub one-per-backend coverage

The direct emissions orchestrator runs all cases by default. Local broad CI
keeps that default. Normal GitHub sampled CI caps emissions at one case per
available backend through the corpus default selected by `--sample-cases`;
`--case-coverage=emissions:1` is the equivalent explicit override. Sampling is
backend-first, not one global case. Verify the docs, orchestrator defaults, and
workflow call agree. An audit run uses `--all-cases`; a sampled green run is
not full corpus evidence.

## POC: adopter-grade library contract

`test-data/poc/` is the corpus of adopter-grade Kio packages. Each `test-data/poc/<topic>/workdir/` carries copyable root library sources, and library POCs may carry a nested `workdir/demo/` package that imports the root package and provides the demonstrative `main`. The corpus's promise is **load-bearing for adopters**: a module that fails the contract leaves a user with subtly broken or incomplete code. Verify `test-data/poc/README.md` exists, then read it and `test-data/README.md` § POC case layout before starting. The shared sweeps (§§ 1–9) apply; the checks below are the POC-specific contract.

### POC 0. Corpus contract file, success exits, and the two flavors

`test-data/poc/README.md` is the shared contract for the corpus. Its absence is a finding before any case-level audit starts.

Every POC is a `00_success` shape by construction. Each immediate `test-data/poc/<topic>/` directory must carry `expected.exit`, and every `expected.exit` under `test-data/poc/` must contain exactly `0`, with or without a final newline. Non-zero expected exits belong in `test-data/goldens/`, not in the adopter-grade POC corpus.

```sh
test -f test-data/poc/README.md

for dir in test-data/poc/*/; do
  test -f "$dir/expected.exit"
done

find test-data/poc -name expected.exit -type f -exec sh -c '
  for path do
    size=$(wc -c <"$path" | tr -d "[:space:]")
    case "$size" in
      1) printf "0" | cmp -s - "$path" ;;
      2) printf "0\n" | cmp -s - "$path" ;;
      *) false ;;
    esac || printf "%s\n" "$path"
  done
' sh {} +
```

**The two POC flavors.** A POC is either a **library** (the default) or a **worked example** (an explicit carve-out for a POC whose subject is a language *mechanism* demonstrated end-to-end — the construction itself is the point — rather than a drop-in library API). Because the mechanism it demonstrates can live below the surface library vocabulary, a worked example is exempt from the library API-surface guardrails (comprehensive API, operator DSL) and from the no-`__intrinsics__` rule. The carve-out is signaled by an empty `WORKED_EXAMPLE` marker file at the case root (sibling of `run.sh` and `workdir/`), and **that marker — not any hardcoded directory list — is the sole trigger**: the audit's contract diverges between the two flavors by the marker's presence alone.

```sh
ls test-data/poc/<topic>/WORKED_EXAMPLE 2>/dev/null  # zero exit ⇒ worked-example; nonzero ⇒ library
```

A POC without a `WORKED_EXAMPLE` marker is audited against the library contract; a POC with one is audited against the worked-example contract.

### POC 1. Header comment

Every POC's headline source starts with a header comment that names its flavor. For library POCs, this is the copyable root library module or modules that define the public API (`core.kio`, `list.kio`, `result.kio`, `optics.kio`, and so on). For worked examples, this is the demonstrative entry module.

- **Library** — the header explicitly states the module is ready to drop into a user's project: "ready-to-use", "ready to drop in", "drop into your own Kio package", or close synonyms. No backing-out language ("not yet", "incomplete", "may not work in …").
- **Worked example** — the header explicitly states the module is **NOT** a drop-in library and names *why*: typically that the module's subject is a language mechanism (intrinsics, HKT machinery, …) that the library carve-out exists to demonstrate.

A library POC missing the ready-to-drop-in claim, or a worked-example POC missing the NOT-a-library disclaimer, is a finding. The disclaimer needs to be in the header (top of file, above the first declaration) — a buried mid-file caveat doesn't count.

### POC 2. Module `///` documentation

Every real root library `*.kio` file under `test-data/poc/*/workdir/` must have module-level `///` docs immediately above the `module` directive, and every exported top-level declaration must have attached `///` docs: `pub fn`, `pub type`, `pub literal`, `pub alias`, `pub newtype`, `pub labels`, `pub op`, `pub fold`, and `pub elab`. Demo modules, host adapters, `testapi` host-API modules, and materialized dependency roots are exempt from this root-library doc scan (see [`test-data/poc/README.md`](../../../test-data/poc/README.md) § Module documentation).

Private helpers do not need `///`. If an item is not worth documenting for copyable library users, it should not be `pub` unless another module genuinely needs it across the module boundary. Public constructors and projectors inside a documented `newtype` are covered by the `newtype` docs unless they need extra semantic explanation.

Deduplicate symlinked shared files by canonical realpath before reporting, so one shared module produces one finding:

```sh
python3 - <<'PY'
from pathlib import Path
import os
import re
import sys

dependency_roots = set()
for dep in Path("test-data/poc").glob("*/workdir/**/*.dep.kio"):
    dependency_roots.add(dep.parent / dep.name.removesuffix(".dep.kio"))

paths = []
seen = set()
for path in sorted(Path("test-data/poc").glob("*/workdir/**/*.kio")):
    if path.name.endswith((".pkg.kio", ".sig.kio", ".dep.kio")):
        continue
    if "/demo/" in path.as_posix():
        continue
    if path.name == "testapi.kio" or "/testapi/" in path.as_posix():
        continue
    if any(path.is_relative_to(root) for root in dependency_roots):
        continue
    real = Path(os.path.realpath(path))
    if real in seen:
        continue
    seen.add(real)
    paths.append(path)

public_decl = re.compile(r"^pub\s+(fn|type|literal|alias|newtype|labels|op|fold|elab)\b")
failures = []

for path in paths:
    lines = path.read_text().splitlines()
    for index, line in enumerate(lines):
        if re.match(r"^module\b", line):
            if index == 0 or not lines[index - 1].lstrip().startswith("///"):
                failures.append((path, index + 1, "missing module-level /// docs"))
            break
    else:
        failures.append((path, 1, "missing module directive"))

    for index, line in enumerate(lines):
        if public_decl.match(line):
            if index == 0 or not lines[index - 1].lstrip().startswith("///"):
                failures.append((path, index + 1, line.strip()))

for path, line, message in failures:
    print(f"{path}:{line}: {message}")

sys.exit(1 if failures else 0)
PY
```

### POC 3. No `__intrinsics__` (library POCs)

Library POCs must not import `__intrinsics__` from their own root library or demo-owned modules. Surface forms only — imported `if!`, `scope!`, `do!`, and `match!` block calls, the algebraic palette, the spine palette, and UFCS. The library promise is "copy this into your own code"; an `__intrinsics__` import drags the spec's escape-hatch surface into the user's file, defeating the promise.

Materialized dependency roots named by `*.dep.kio` are out of scope for this POC-owned scan; those modules are audited at their source package. For example, a library POC may carry a materialized `elab/` dependency whose own worked-example package imports `__intrinsics__`.

```sh
python3 - <<'PY'
from pathlib import Path
import sys

workdir = Path("test-data/poc/<topic>/workdir")
if (workdir.parent / "WORKED_EXAMPLE").exists():
    sys.exit(0)

dependency_roots = {dep.parent / dep.name.removesuffix(".dep.kio") for dep in workdir.rglob("*.dep.kio")}
failures = []

for path in sorted(workdir.rglob("*.kio")):
    if path.name.endswith((".pkg.kio", ".sig.kio", ".dep.kio")):
        continue
    if path.name == "testapi.kio" or "/testapi/" in path.as_posix():
        continue
    if any(path.is_relative_to(root) for root in dependency_roots):
        continue
    for line_no, line in enumerate(path.read_text().splitlines(), start=1):
        if line == "use __intrinsics__;":
            failures.append((path, line_no))

for path, line_no in failures:
    print(f"{path}:{line_no}: imports __intrinsics__")

sys.exit(1 if failures else 0)
PY
```

Worked-example POCs are exempt from the library-surface contract — their subject is a language mechanism demonstrated end-to-end, not a drop-in API. A worked example may still import `__intrinsics__` when the intrinsics surface is its subject; finding an `__intrinsics__` import in a marked case is expected, not a violation. (A worked example need not import intrinsics — the current `hkt` POC demonstrates the HKT vocabulary entirely in surface forms.)

### POC 4. API surface comprehensiveness (library POCs)

Apply [`test-data/poc/README.md` § General-purpose abstraction ownership](../../../test-data/poc/README.md#general-purpose-abstraction-ownership) to the POC's own library and demo modules. Identify the owner of each public general-purpose abstraction and literal syntax, and inspect its actual API and direct consumers rather than deciding from a type's spelling or recursive shape.

- A duplicate public general-purpose collection or optional-value abstraction is a finding, including a renamed clone or duplicate literal. Routine absence uses ordinary `A | .` and `()` outside the dedicated Option abstraction. Ordinary products/sums and private domain-specific state are not duplicate libraries.
- Where public APIs exchange a shared nominal collection, trace the declared dependency and any explicit retype edges to its owner. A same-spelled local type or alias does not establish that identity. Where the dependency is disproportionate, a fold, visitor, or callback seam lets the caller choose its collection without another public clone.
- Exclude materialized dependency roots identified by actual `*.dep.kio` declarations from the authored-duplicate count; inspect those modules at their source owner instead. This does not waive canonical-materialization checks. A public domain-specific raw representation is not a duplicate merely because it is public, and this check does not authorize privatizing an existing adopter boundary.

Record each candidate's source owner, public API/literal, dependency or consumer evidence, and disposition. Reclassification needs a concrete domain-role or ownership argument, not a rename. For a change strengthening this policy or audit, require the README's corpus-wide remediation/reclassification condition; the known examples are not an exhaustive inventory. This is a manual semantic check, not a new name-based lint or registry.

The shared contract sets size guardrails:

- 3 to 6 `op _ … _ { impl …, };` operator declarations.
- 20 to 40 top-level `fn` declarations.
- One `newtype` or `alias` for the data structure itself (one optional small domain-specific helper type is allowed; this allowance does not permit a duplicate public general-purpose abstraction or a local named Option for routine absence).
- 15 to 30 `equiv` blocks (see § POC 5 below).
- Primary library source size 400 to 800 lines (1000 ceiling for tree-backed structures like `vec` / `dict`).

```sh
python3 - <<'PY'
from pathlib import Path
import re

workdir = Path("test-data/poc/<topic>/workdir")
dependency_roots = {dep.parent / dep.name.removesuffix(".dep.kio") for dep in workdir.rglob("*.dep.kio")}

def owned_root_source(path: Path) -> bool:
    if path.name.endswith((".pkg.kio", ".sig.kio", ".dep.kio")):
        return False
    if "/demo/" in path.as_posix():
        return False
    return not any(path.is_relative_to(root) for root in dependency_roots)

sources = [path for path in sorted(workdir.rglob("*.kio")) if owned_root_source(path)]
text = "\n".join(path.read_text() for path in sources)
print("ops", len(re.findall(r"^pub op ", text, flags=re.M)))
print("fns", len(re.findall(r"^(pub fn|fn) ", text, flags=re.M)))
print("equivs", len(re.findall(r"^equiv ", text, flags=re.M)))
for path in sources:
    print(path, len(path.read_text().splitlines()))
PY
```

A count outside the guardrails is a finding, but not a hard one — the contract says "stop when the next entry wouldn't add anything new." A module 50 lines over the ceiling on a single justified addition is fine; a module hitting the floor because half the obvious operations are missing is a finding.

### POC 5. `equiv` blocks discharging the module's laws

This is the load-bearing piece. The POC corpus is the only place that exercises `equiv`-driven law discharge at library scale; the modules are simultaneously the corpus's `kio test` coverage. Every library POC must use `equiv` where the data structure's algebra naturally admits laws:

- **Functor laws** when the structure carries a `map` (`map(id) ≡ id`, `map(f . g) ≡ map(f) . map(g)`).
- **Monad laws** when the structure carries a `bind` / `>>=` (left identity, right identity, associativity).
- **Bifunctor / monoid / semigroup laws** where applicable.
- **Structure-specific identities** — `reverse . reverse ≡ id` on lists; FIFO discipline on queues; a successful vec update reads back the replacement at its index; last-write-wins on dicts; lens get-set / set-get / set-set on optics; prism review-preview round-trip on optics.
- **Round-trip laws** when the module provides paired conversions (`to_sum . from_sum ≡ id`, `to_list . from_list ≡ id` on small concrete spines).

The audit's check: for each non-trivial library fn (typically anything beyond the constructors and primitive accessors), is the most obvious law on it stated as an `equiv` block? A library missing the functor laws on its `map`, or the monad laws on its `bind`, is a clear finding — those are exactly the laws an adopter most needs to trust.

Concrete-spine restatements (when a law doesn't discharge on a fully-opaque parameter — typically because the reduction needs to walk a recursive structure) are admissible per `test-data/poc/README.md`'s `equiv` discipline note; the audit checks the law is stated *somewhere*, not the precise statement style.

A blanket absence of `equiv` blocks (a library POC with 0 of them) is the most severe finding — the module then provides no machine-checked guarantee of its own behavior.

### POC 6. `kio test` exits cleanly on every POC that uses `equiv`

A POC's `equiv` blocks are only meaningful if they actually discharge. For every package directory under `test-data/poc/<topic>/workdir/` whose owned source contains an `equiv` block, the audit invokes `kio test` in that package directory and asserts exit 0. This includes nested demo packages when a worked example keeps its laws in the demo entry. A non-zero exit — runtime panic, type error surfaced only at `kio test` time, an equiv whose two sides reduce to different values — is a finding: the module's stated law is wrong, or the implementation no longer satisfies it.

The check uses the locally-built `kio` binary at `kio-rs/target/debug/kio`, matching the path `ci/checks/orchestrators/poc-tests.sh` resolves. Build it (`( cd kio-rs && sh ../ci/cargo.sh build )`) before running the audit if it isn't present.

```sh
python3 - <<'PY'
from pathlib import Path
import subprocess
import sys

repo = Path.cwd()
kio = repo / "kio-rs/target/debug/kio"
failures = []

def package_dirs():
    for pkg in sorted(Path("test-data/poc").glob("*/workdir/**/*.pkg.kio")):
        yield pkg.parent

def owned_sources(pkg_dir: Path):
    dependency_roots = {dep.parent / dep.name.removesuffix(".dep.kio") for dep in pkg_dir.rglob("*.dep.kio")}
    nested_packages = {pkg.parent for pkg in pkg_dir.glob("**/*.pkg.kio") if pkg.parent != pkg_dir}
    for path in sorted(pkg_dir.rglob("*.kio")):
        if path.name.endswith((".pkg.kio", ".sig.kio", ".dep.kio")):
            continue
        if any(path.is_relative_to(root) for root in dependency_roots):
            continue
        if any(path.is_relative_to(nested) for nested in nested_packages):
            continue
        yield path

for pkg_dir in package_dirs():
    if not any(any(line.startswith("equiv ") for line in path.read_text().splitlines()) for path in owned_sources(pkg_dir)):
        continue
    result = subprocess.run([str(kio), "test"], cwd=pkg_dir, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    if result.returncode != 0:
        failures.append((pkg_dir, result.returncode, result.stdout))

for pkg_dir, code, output in failures:
    print(f"FAIL {pkg_dir} (exit {code}):\n{output}")

sys.exit(1 if failures else 0)
PY
```

A POC package without any `equiv` block is skipped by this check — § POC 5 already flags the absence for library POCs. A POC carrying a `WORKED_EXAMPLE` marker is included if its owned source contains an `equiv` block; the worked-example carve-out exempts the module from API-surface guardrails, not from the discharge-must-succeed contract.

**Goldens are out of scope for this check.** The goldens corpus contains `.kio` files with `equiv` blocks under `test-data/goldens/00_success/test_equiv_*/`, `14_type_error/equiv_*/`, and `50_test_failure/equiv_*/`, but those cases already invoke `kio test` from their own `run.sh` files (the test harness asserts the expected exit code per bucket — 0 for `00_success`, 14 for `14_type_error`, 50 for `50_test_failure`). Re-running `kio test` from the audit would duplicate that coverage with no extra signal.

### POC 7. Demonstrative `main`

`main` is the worked tour an adopter reads first. The audit checks:

- `main` is not a no-op (`pub fn main() -> . { () }` and equivalents).
- `main`'s output uses both the named-function API and the operator DSL — adopters see both call styles.
- `main`'s output is captured in `expected.stdout` (the harness asserts byte-for-byte agreement).
- The output threads concrete values through the API in a way that reads as a worked example — concrete pairs, lists, dicts, the operators applied to them, output separated for readability.

The optics POC's demo `main` (in `test-data/poc/optics/workdir/demo/testapi/main.kio`) is the shape to compare against. A no-op `main`, or a `main` that exercises only one call style, is a finding.

### POC 8. `run.sh` chains all three commands

Every POC's `run.sh` must chain `kio check` + `kio test` + per-backend build + run, in that order, with the first two redirected so `expected.stdout` snapshots only `main`'s output.

```sh
grep -c 'kio.*check'                test-data/poc/<topic>/run.sh   # expect ≥1
grep -c 'kio.*test'                 test-data/poc/<topic>/run.sh   # expect ≥1
grep -c '"$KIO_BIN" build'          test-data/poc/<topic>/run.sh   # expect ≥1
grep -c '"$KIO_RUNNER"'             test-data/poc/<topic>/run.sh   # expect ≥1
```

A POC missing any of the three commands is a finding — the corpus's whole point is that all three pass against every POC on every applicable emitter.

### POC 9. Path portability

Path portability is a shared check (§ 6); its `git grep` covers `test-data/poc` along with the other corpora. A `.dep.kio` absolute `path` or local `file://` URL under `test-data/poc` is always a finding.

### POC 10. Self-containment

A POC has one reusable root package under `workdir/` and may have a nested runnable package under `workdir/demo/`. A root package may import another POC only through a committed materialized `*.dep.kio` dependency; direct `use ... from <sibling-poc>` imports are forbidden. The drop-in promise rests on copying the package's own library files plus its declared dependency closure, not on ambient sibling directories.

```sh
sibs=$(ls test-data/poc | paste -sd '|' -)
grep -nE "use .* from ($sibs)" \
  $(find test-data/poc/<topic>/workdir -name '*.kio' ! -path '*/library/*' -print)
```

(Derive the sibling-POC alternation from the live `test-data/poc/` listing — a hardcoded name list rots as POCs land.)

Direct cross-POC imports are a finding. Imports through the local name created by a `*.dep.kio` file (`elab/...`, `library/...`) are expected, as are imports of host names declared by the package's own `<pkg>.pkg.kio`.

## Castle: composed-program contract

`test-data/castles/` is the corpus of larger composed Kio programs — an algorithm, a toy game, a parser, a planner, a simulation, a puzzle solver — each run end to end through the standard runner path. Castles exist to exercise *realistic composition*: source that looks like Kio someone would actually write for a small real project, the integration layer that catches regressions minimized goldens and generated Kio' programs miss. The corpus's promise is **diversity through composition**: every castle is a success case (`expected.exit` exactly `0`), uses natural Kio, and is never reshaped to dodge a compiler gap. Verify `test-data/castles/README.md` exists, then read it and `test-data/README.md` § Castle case layout before starting. The procedure for adding a castle is the [`build-castle`](../build-castle/SKILL.md) skill; this audit verifies the result. The shared sweeps (§§ 1–9) apply; the checks below are the castle-specific contract.

### Castle 1. Corpus contract file and flat layout

`test-data/castles/README.md` is the shared contract for the corpus. Its absence is a finding before any case-level audit starts.

Every castle is a **direct child** of `test-data/castles/`. There are no exit-code buckets and no nested case directories — a nested `expected.exit` below the top level means the flat-corpus contract has eroded.

```sh
test -f test-data/castles/README.md

for dir in test-data/castles/*/; do
  [ -d "$dir" ] || continue
  # A castle is a direct child; reject any nested case directory.
  find "$dir" -mindepth 2 -name expected.exit -type f \
    ! -path '*/out/*' 2>/dev/null | head -n 1
done
```

A nested `expected.exit` (outside the build-artifact `out/` tree) is a finding.

### Castle 2. Required file set

Each `test-data/castles/<name>/` must carry, at the case root:

- `README.md`,
- `workdir/` (the Kio package directory, with `<pkg>.pkg.kio` carrying the `build { ... }` block and the regular-module tree that provides `main`),
- `run.args` (the only execution file),
- `expected.stdout`,
- exactly one stderr-policy file — `expected.stderr`, `expected.stderr.ignore`, or `expected.stderr.grep`,
- `expected.exit`.

```sh
for dir in test-data/castles/*/; do
  [ -d "$dir" ] || continue
  name=$(basename "$dir")
  for f in README.md run.args expected.stdout expected.exit; do
    test -f "$dir/$f" || printf '%s: missing %s\n' "$name" "$f"
  done
  test -d "$dir/workdir" || printf '%s: missing workdir/\n' "$name"
  n=0
  for f in expected.stderr expected.stderr.ignore expected.stderr.grep; do
    test -f "$dir/$f" && n=$((n + 1))
  done
  [ "$n" = 1 ] || printf '%s: must have exactly one stderr-policy file (got %d)\n' "$name" "$n"
done
```

A missing required file, a missing `workdir/`, or zero / more-than-one stderr-policy file is a finding.

The corpus must use the actual landed shape: the package tree lives **directly under `workdir/`**, not under a `src/` subdirectory, and there is no `<pkg>.downstream.kio` file — those are not Kio conventions. A castle whose package lives under `workdir/src/` or that ships a `*.downstream.kio` is a finding (it diverges from `maze_replay/workdir/` and from the `build-castle` skill's documented layout).

### Castle 3. `run.args`-only execution

`run.args` is the **only** execution file a castle uses. A castle never carries a `run.sh` — that is the goldens/POC custom-runner shape, not the castle standard-runner-path shape.

```sh
find test-data/castles -mindepth 2 -maxdepth 2 -name run.sh -type f
```

Any `run.sh` is a finding. `run.args` is normally empty (selecting the standard build-then-run path); a non-empty `run.args` supplies plain whitespace-separated runner tokens (e.g. a `--protocol <tier>` selector). Both shapes are admissible — the audit checks only that `run.sh` is absent and `run.args` is present (§ Castle 2).

### Castle 4. `expected.exit` is exactly `0`

Every castle is a success case. `expected.exit`'s normalized content (whitespace stripped) must be exactly `0`. A non-zero expected exit means the case is a goldens-bucket regression or a bug to fix — by definition not a castle.

```sh
find test-data/castles -mindepth 2 -maxdepth 2 -name expected.exit -type f \
  -exec sh -c '
    for path do
      content=$(tr -d "[:space:]" <"$path")
      [ "$content" = 0 ] || printf "%s: expected.exit must be exactly 0 (got %s)\n" "$path" "$content"
    done
  ' sh {} +
```

Any non-`0` normalized `expected.exit` is a finding.

### Castle 5. `input.stdin` iff the package reads stdin

A castle that reads input declares the canonical runner host fn `read_ascii_line()` and **must** ship an `input.stdin` fixture (the standard `run.args` path redirects it into the runner). A castle that reads no input **must not** ship `input.stdin`. The biconditional matters both ways: a declared `read_ascii_line()` with no fixture hits EOF on the first read; a stray `input.stdin` with no reader is dead fixture data.

```sh
for dir in test-data/castles/*/; do
  [ -d "$dir/workdir" ] || continue
  name=$(basename "$dir")
  reads_stdin=$(find "$dir/workdir" -type f -name '*.kio' ! -path '*/out/*' -print0 2>/dev/null \
    | xargs -0 grep -lE 'read_ascii_line[[:space:]]*\(' 2>/dev/null | head -n 1)
  has_fixture=$( [ -f "$dir/input.stdin" ] && echo 1 || echo 0 )
  if [ -n "$reads_stdin" ] && [ "$has_fixture" = 0 ]; then
    printf '%s: declares read_ascii_line() but has no input.stdin fixture\n' "$name"
  fi
  if [ -z "$reads_stdin" ] && [ "$has_fixture" = 1 ]; then
    printf '%s: ships input.stdin but never calls read_ascii_line()\n' "$name"
  fi
done
```

Either direction of the biconditional broken is a finding. (Path portability under `test-data/castles` and workaround-shaped source comments are the shared checks § 6 and § 3.)

### Castle 6. README quality

Each castle's `README.md` is for a reader who has not read the source. The audit checks it carries, in substance:

- **the project shape** — what the program models or computes (an algorithm, a game, a parser, a simulation, a puzzle solver, a utility);
- **the corpus diversity** — a "What this adds to the corpus" section naming where this castle sits that the rest of the corpus does not (domain / data shape / execution shape / host surface / compiler stress);
- **the fixture grammar** — for a castle that reads input, enough to identify any seed line and the input echoed or summarized by stdout; the README must let a reader understand `expected.stdout` without manually merging it with `input.stdin`.

```sh
for dir in test-data/castles/*/; do
  [ -f "$dir/README.md" ] || continue
  name=$(basename "$dir")
  body=$(cat "$dir/README.md")
  printf '%s' "$body" | grep -qiE 'what this adds|adds to the corpus|diversity' \
    || printf '%s: README has no corpus-diversity section ("What this adds to the corpus")\n' "$name"
  # Fixture grammar only required when the castle reads input.
  if [ -f "$dir/input.stdin" ]; then
    printf '%s' "$body" | grep -qiE 'input|fixture|seed|stdin|line' \
      || printf '%s: README of an input-reading castle does not describe the fixture grammar\n' "$name"
  fi
done
```

A README missing the diversity section, or an input-reading castle's README that never describes the fixture grammar, is a finding. The grep is a floor; the substance check (does the README actually let a cold reader understand stdout?) is a manual read.

Additionally, a castle README must **not** claim to be a minimized regression golden or an adopter-grade POC reference library — those are the *other* corpora's contracts, and a castle that markets itself as one of them has misfiled itself.

```sh
grep -rniE 'minimized regression|regression golden|adopter-grade|drop-in|ready to drop|copy this into|ready-to-use' \
  test-data/castles/*/README.md
```

A hit needs judgment: a README that *contrasts* castles with goldens/POCs (as the corpus README does) is fine; a README that *claims to be* a minimized golden or a drop-in POC is a finding.

### Castle 7. Every castle runs under the orchestrator

The corpus is only meaningful if every castle actually builds and runs to a `0` exit through the standard runner path. For each castle the audit invokes the orchestrator on that castle with `--impls=SAMPLE_IMPL` (one applicable impl, the cheapest confirmation that the case is live) and asserts a clean exit. A castle that fails to build or run, or whose output drifts from `expected.stdout`, is a finding — a real regression surfaced.

```sh
# Run each castle once on one applicable impl. The orchestrator builds
# the compilers + runners on first use; subsequent castles reuse them.
logdir=$(mktemp -d)
fail=0
for dir in test-data/castles/*/; do
  [ -f "$dir/expected.exit" ] || continue
  name=$(basename "$dir")
  if ! sh ci/checks/orchestrators/castle-tests.sh --impls=SAMPLE_IMPL -- "^${name}\$" >"$logdir/$name.log" 2>&1; then
    printf 'FAIL %s:\n' "$name"
    tail -n 40 "$logdir/$name.log"
    fail=1
  fi
done
[ "$fail" = 0 ] || printf 'one or more castles failed the orchestrator\n'
```

Before running, use the repo's configured compiler cache per [`ai/topics/local-tools.md`](../../topics/local-tools.md) § Compiler cache rather than routing into a throwaway cache. The orchestrator's own `validate_castle_contract` re-checks the layout, file set, `expected.exit`, `run.sh`-absence, and `input.stdin` biconditional (§§ Castle 1–5) at run time, so a green orchestrator pass is independent corroboration of those structural checks. The orchestrator also discharges each castle's `kio test` load (the castle form of § 7). For a pre-merge confirmation the corpus is green on every impl, run `sh ci/checks/orchestrators/castle-tests.sh --impls=FULL_IMPL_MATRIX --all-cases`.

### Castle 8. Castle-discovered bugs have focused regression goldens

When a castle exposes a Kio bug, AGENTS.md § Universal rules requires the fix to land in the same change with a **focused regression golden** under `test-data/goldens/` that exercises the bug's shape directly — the castle is never the only coverage for the bug, and the bug is never papered over in the castle source. This is the hardest check to mechanize (it asks about history, not current files), so it is a manual review backed by signals:

- For any castle whose `README.md` § "What this adds to the corpus" names a compiler shape it was the first to stress (e.g. `maze_replay` cites `rec(loop)` recursion in a `match!` clause body), check that a focused golden under `test-data/goldens/00_success/` (or the relevant exit-code bucket) exercises that shape directly. `maze_replay`'s `rec`-in-`match!`-clause shape is pinned by `test-data/goldens/00_success/exec_rec_in_match_clause/`.
- Walk the castle-track commits (`git log -- test-data/castles/`) and confirm any "Fix …" commit that landed alongside a castle also added a `test-data/goldens/` case.

A castle whose composition is known to have surfaced a bug with no corresponding focused golden is a finding — the regression can silently return.

## Contrib: the community quality bar

`test-data/contrib/` is the contribution lane described in [`CONTRIBUTING.md`](../../../CONTRIBUTING.md) § Contribute an example program or library — the one place external pull requests are accepted. The **structural** contract (directory naming, the file allowlist, the exec-file set, `KNOWN_FAILING`, stderr policy, `<pkg>.pkg.kio`, owner naming) is a CI gate — `contrib-tests.sh`'s `validate_contrib_contract` and `pr-policy-check.yml` — and the `.github/` freshness of that gate is [`audit-github`](../audit-github/SKILL.md) § 2. **Do not re-implement either here.** This section owns the **quality bar** (judgment, like the castle README read), not the structural gate. Read `test-data/contrib/README.md` first.

### Contrib 1. The quality bar

A contrib case is natural Kio a user would actually write. Its bar is *deliberately relaxed* relative to the other corpora — it does **not** need the minimized shape of a golden, the adopter-grade API surface of a POC, or the scale of a castle, and it is not subject to those corpora's contracts. What it must not contain:

- **Absolute or machine/user-specific paths** — the shared § 6 sweep covers `test-data/contrib`; symlinks are forbidden outright.
- **Compiler-bug workarounds, outside a `KNOWN_FAILING` reproducer** — a contrib case that reshapes its source to dodge a compiler gap (rather than filing the bug as a `KNOWN_FAILING` reproducer) is a finding, the shared § 3 rule applied to contributions. A genuine bug reproducer carries a `KNOWN_FAILING` marker and declares only the backend(s) where the bug occurs (§ Contrib 2, § 8).
- **A missing or empty `README.md`** — each case carries a short description of what it demonstrates. Judge it as you would a castle README: could a reader who has not seen the source tell what the case is for?

### Contrib 2. Three case shapes and reference cases

A contrib case is one of three shapes, each with a distinct exec-file (§ Exec-file discipline):

- **a program** — runnable Kio with a `main`; `run.args` (normally empty).
- **a library** — an exported API and no `main`, proven by `equiv` laws; `run.test-only` (which discharges the laws via `kio test`).
- **a bug reproducer** — a program or library that *should* work but doesn't; `KNOWN_FAILING` marks the expected failure.

Two maintainer-owned reference cases anchor the corpus and must stay: `example-0` (a minimal program, `run.args`) and `example-1` (a minimal library, `run.test-only` with `equiv` laws). They are the copy-templates a contributor starts from and they keep the `run.args` and `run.test-only` contrib paths exercised on every CI pass even when no real contribution is open. Every other case directory is owner-scoped: named `<github-username>-<issue-number>` (all lowercase), and only that contributor opens follow-ups against it (`pr-policy-check.yml` enforces the match). A non-reference directory whose name is not a plausible `<github-username>-<issue-number>` slot is a quality flag to surface (the structural gate owns the hard enforcement).

## How to report

Group findings by scope. **Shared-check findings** (§§ 1–9) can fire on any corpus; the per-corpus findings follow. For each finding, cite the file (and the commit, for deleted/demoted cases), the corpus, the check § it fired from, and the AGENTS.md rule by name.

### Shared-check findings

One bucket per shared check, numbered 1–9 to mirror §§ 1–9: hidden bugs (deleted / demoted cases); per-backend opt-outs without a three-site caveat; workaround-shaped sources (list statically-unconfirmable candidates separately); standard-path logic or compiler-admission bypasses hidden in `run.sh`; direct reference-module inclusion; path portability / symlink violations; undischarged `equiv` in `run.sh` goldens; `KNOWN_FAILING` accumulation — for § 8, always report the full marker list (each is a tracked open Kio bug); accumulation is the signal; focused claims whose fixture never reaches or discriminates its advertised subject.

### Per-corpus findings

One bucket per per-corpus check, keyed by its § label (Goldens 1–6, Emissions 0–5, POC 0–10, Castle 1–8, Contrib 1–2), each finding citing the check it fired from. Severity signals to carry into the ranking: direct generated-host handling in a golden (§ Goldens 6), an adaptive host or incidental artifact assertion (§§ Emissions 2–3), and treating emissions as runtime evidence (§ Emissions 5) are hard boundary failures; a missing law-discharging `equiv` (§ POC 5) is the single most load-bearing POC finding type; a failing `kio test` (§ POC 6) or an orchestrator failure (§ Castle 7) is a real surfaced regression — fix the law, the implementation, or the emitter, never reshape the case; a misfiled case (§ Goldens 2, § Castle 6's misfiling clause) moves corpora rather than being reshaped in place.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md). The fix is always to restore the surfaced bug or bring the case up to contract — re-add the deleted golden as a failing case, add the omitted backend (or, for an already-shipping backend with a genuinely impossible limitation, supply the matching spec/guide banners and emitter citation), strip the defensive scaffolding, add the missing `equiv` / `kio test` line, write the missing regression golden — and fix the underlying emitter / inference gap, never to re-hide it. Per-finding fixes for missing `equiv`, a `kio test` failure, a no-op `main`, an orchestrator failure, and a workaround reshape involve real authoring or engineering; surface those for human authorship rather than mechanically generating placeholders. If the underlying fix is out of session scope, leave the case in place demonstrating the intended behavior with a `run.sh` comment naming the bug.
