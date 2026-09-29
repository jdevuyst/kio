---
name: audit-local-running-guidance
description: Verify local running guidance stays split between local-tools.md, local-ci.md, and local-performance.md, and that the caches index (ai/topics/caches.md) stays a resolving, mechanics-free routing layer over them
allowed-tools: Read, Grep, Glob, Bash
---

# Local-running guidance audit

The local-running guidance is intentionally split across three files:

- `ai/topics/local-tools.md` — third-party/local tool mechanics: devcontainer, tool versions, sccache setup, scheduler-native Cargo serialization, and related environment variables.
- `ai/topics/local-ci.md` — Kio check infrastructure mechanics: what scripts do, selector syntax, logs, exit-code handling, cache-clearing behavior, and local coverage knobs.
- `ai/topics/local-performance.md` — advisory local performance strategy: warm caches, one-worktree and multi-worktree tradeoffs, cold-build coordination, and parallel local-session fan-in.

This skill keeps the split sharp and verifies the old standalone `parallel-agents.md` concept has not crept back as a topic file or source-of-truth.

Read AGENTS.md and the three local-running topic files before starting.

## 1. The topic set is complete and exclusive

`AGENTS.md` must reference all three local-running topics, and `ai/topics/parallel-agents.md` must not exist.

```sh
for f in ai/topics/local-tools.md ai/topics/local-ci.md ai/topics/local-performance.md; do
  [ -f "$f" ] || echo "missing local-running topic: $f"
  grep -qF "$f" AGENTS.md || echo "AGENTS.md does not reference: $f"
done
[ ! -e ai/topics/ci.md ] || echo "retired topic still exists: ai/topics/ci.md"
[ ! -e ai/topics/parallel-agents.md ] || echo "retired topic still exists: ai/topics/parallel-agents.md"
```

Also scan tracked files for stale links:

```sh
git grep -nE 'ai/topics/(dev-env|ci|parallel-agents)\.md|dev-env\.md|parallel-agents\.md' -- \
  AGENTS.md ai/ ci/ .github/ docs/ specs/ README.md ROADMAP.md CONTRIBUTING.md TESTING.md |
  grep -v 'ai/skills/audit-local-running-guidance/SKILL.md'
```

Findings:

- **Missing topic / pointer** — one of the three files is absent or not linked from AGENTS.md.
- **Retired topic** — `ai/topics/ci.md` or `ai/topics/parallel-agents.md` exists.
- **Stale link** — any tracked file points at `dev-env.md`, `ci.md`, or `parallel-agents.md` after the rename.

## 2. Local-running guidance anchors stay present

`local-ci.md` must keep the conservative scoping table that tells agents when a broad `ci/all.sh` run is not required, including the symlinked root module-file caveat for Kio corpus scoping.

```sh
grep -qiE 'discovery-based|hand-maintained scheduler|hand-maintained .*list' ai/topics/local-ci.md ||
  echo 'local-ci.md missing ci/all.sh discovery-based broad-gate guidance'
grep -qiE 'early-error|early error|failure reporting channel|progress/failure' ai/topics/local-ci.md ||
  echo 'local-ci.md missing early-error/tidy-output reporting guidance'
grep -qF '### When `ci/all.sh` is not required' ai/topics/local-ci.md ||
  echo 'local-ci.md missing ci/all.sh scoping table'
grep -qF 'test-data/' ai/topics/local-ci.md ||
  echo 'local-ci.md scoping table missing test-data row'
grep -qiE 'symlink(ed)? root module|resolve symlink' ai/topics/local-ci.md ||
  echo 'local-ci.md scoping table missing symlinked root module-file caveat'
grep -qiE 'docs/.+specs/.+ai/|Prose-only.+docs/.+specs/.+ai/' ai/topics/local-ci.md ||
  echo 'local-ci.md scoping table missing prose-only docs/specs/ai guidance'
grep -qF 'repo-lint' ai/topics/local-ci.md ||
  echo 'local-ci.md scoping table missing repo-lint config guidance'
grep -qiE 'kio-rs/|orchestrators|per-case|ci/infra' ai/topics/local-ci.md ||
  echo 'local-ci.md scoping table missing harness/compiler escalation guidance'
grep -qiE 'boundary is unclear|choose broader coverage|unclear.*broader' ai/topics/local-ci.md ||
  echo 'local-ci.md scoping table missing uncertainty defaults broader guidance'
grep -qF 'emissions-tests.sh' ai/topics/local-ci.md ||
  echo 'local-ci.md missing emissions orchestrator guidance'
grep -qiE 'emissions?.*(all cases|default.*all)|default.*all.*emissions?' ai/topics/local-ci.md ||
  echo 'local-ci.md missing local-all emissions default'
grep -qiE '(one|1).*(case )?per (available )?backend|per (available )?backend.*(one|1)' ai/topics/local-ci.md ||
  echo 'local-ci.md missing GitHub one-emission-per-backend sampling contract'
grep -qiE 'emissions?.*(never|does not).*(runtime|backend-completeness)|runtime.*(never|does not).*emissions?' ai/topics/local-ci.md ||
  echo 'local-ci.md missing emissions-do-not-satisfy-runtime-coverage boundary'
```

`local-performance.md` must keep advisory machine/session guidance for warmup, contention, failure handling, and multi-session fan-in.

```sh
grep -qF '## Machine shape and contention' ai/topics/local-performance.md ||
  echo 'local-performance.md missing machine/contention guidance'
grep -qiE 'constrained CPU|constrained .*memory|many-core' ai/topics/local-performance.md ||
  echo 'local-performance.md missing constrained/many-core scenario guidance'
{ grep -qF 'KIO_CI_SERIALIZE_CARGO' ai/topics/local-performance.md &&
    grep -qF 'CARGO_BUILD_JOBS' ai/topics/local-performance.md; } ||
  echo 'local-performance.md missing scheduler-native Cargo serialization / CARGO_BUILD_JOBS guidance'
grep -qiE 'CPU.*wall|wall.*CPU' ai/topics/local-performance.md ||
  echo 'local-performance.md missing CPU/wall contention-reading guidance'
grep -qF 'ci/watch-builds.sh' ai/topics/local-performance.md ||
  echo 'local-performance.md missing watch-builds warmup guidance'
grep -qiE 'broad hygiene.*warmup|warmup.*broad hygiene' ai/topics/local-performance.md ||
  echo 'local-performance.md missing broad-hygiene warmup warning'
grep -qF '## Failure handling' ai/topics/local-performance.md ||
  echo 'local-performance.md missing failure-handling guidance'
grep -qiE 'process tree you launched|specific process you launched' ai/topics/local-performance.md ||
  echo 'local-performance.md missing owned-process termination guidance'
agents_authority_scope='(^|[.!?][[:space:]]+)a missing approval blocks only actions that depend on that approval:'
agents_authority_safe_work='a missing approval blocks only actions that depend on that approval:[[:space:]]+continue safe read-only work'
performance_authority_scope='(^|[.!?][[:space:]]+)an authority wait is scoped to the action that needs the unanswered decision[.]'
performance_authority_safe_work='record that blocked action and the decision or event that releases it,[[:space:]]+then[[:space:]]+continue work that does not prejudge the answer:[[:space:]]+read-only investigation'
tr '\n' ' ' < AGENTS.md | grep -qiE "$agents_authority_scope" ||
  echo 'AGENTS.md missing scoped authority-wait rule'
tr '\n' ' ' < AGENTS.md | grep -qiE "$agents_authority_safe_work" ||
  echo 'AGENTS.md missing safe work during authority waits'
tr '\n' ' ' < ai/topics/local-performance.md | grep -qiE "$performance_authority_scope" ||
  echo 'local-performance.md missing scoped authority-wait rule'
tr '\n' ' ' < ai/topics/local-performance.md | grep -qiE "$performance_authority_safe_work" ||
  echo 'local-performance.md missing safe work during authority waits'
if printf '%s\n' 'It is false that a missing approval blocks only actions that depend on that approval.' |
    grep -qiE "$agents_authority_scope"; then
  echo 'authority-wait scope anchor accepts inverted AGENTS.md wording'
fi
if printf '%s\n' 'A missing approval blocks only actions that depend on that approval: do not continue safe read-only work.' |
    grep -qiE "$agents_authority_safe_work"; then
  echo 'authority-wait safe-work anchor accepts inverted AGENTS.md wording'
fi
if printf '%s\n' 'An authority wait is never scoped to the action that needs the unanswered decision.' |
    grep -qiE "$performance_authority_scope"; then
  echo 'authority-wait scope anchor accepts inverted local-performance.md wording'
fi
if printf '%s\n' 'Record that blocked action and the decision or event that releases it, then do not continue work that does not prejudge the answer: read-only investigation.' |
    grep -qiE "$performance_authority_safe_work"; then
  echo 'authority-wait safe-work anchor accepts inverted local-performance.md wording'
fi
grep -qiE 'integration worktree|final local gate|scoped checks' ai/topics/local-performance.md ||
  echo 'local-performance.md missing multi-session scoped-then-integration guidance'
grep -qiE '`git worktree add`.*does not change.*(working directory|cwd)' ai/topics/local-performance.md ||
  echo 'local-performance.md missing worktree-add cwd guidance'
grep -qF '`git -C <worktree>`' ai/topics/local-performance.md ||
  echo 'local-performance.md missing explicit worktree mutation context'
worktree_mutation_prohibition='(never|do not|must not|may not)[[:space:]]+chain[^.]*implicit-context'
matches_worktree_mutation_prohibition() {
  grep -qiE "$worktree_mutation_prohibition"
}
tr '\n' ' ' < ai/topics/local-performance.md | matches_worktree_mutation_prohibition ||
  echo 'local-performance.md missing implicit-context worktree mutation prohibition'
if printf '%s\n' 'You may chain worktree creation with an implicit-context `git cherry-pick`, `git commit`, or `git merge`.' |
    matches_worktree_mutation_prohibition; then
  echo 'worktree mutation prohibition anchor accepts affirmative permission wording'
fi
for command in '`git cherry-pick`' '`git commit`' '`git merge`'; do
  grep -qF "$command" ai/topics/local-performance.md ||
    echo "local-performance.md missing worktree mutation command: $command"
done

# The single native scheduler contract stays portable and visible.
grep -qF 'std::thread::available_parallelism' ai/topics/local-ci.md ||
  echo 'local-ci.md missing native available-parallelism work-capacity guidance'
grep -qF 'work -> cargo -> compiler' ai/topics/local-ci.md ||
  echo 'local-ci.md missing scheduler resource-order guidance'
grep -qF 'KIO_CI_SERIALIZE_CARGO=1' ai/topics/local-ci.md ||
  echo 'local-ci.md missing scheduler-native Cargo serialization guidance'
grep -qiE 'native `?self-test`?' ai/topics/local-ci.md ||
  echo 'local-ci.md missing native scheduler self-test guidance'
grep -qiE 'macOS.+Windows|Windows.+macOS' ai/topics/local-ci.md ||
  echo 'local-ci.md missing macOS/Windows native scheduler evidence'
{ grep -qiF 'Cross-target compilation' ai/topics/local-ci.md &&
    grep -qiF 'cannot prove' ai/topics/local-ci.md; } ||
  echo 'local-ci.md missing cross-target evidence limitation'
grep -qiE 'generic.+readiness hook|readiness hook.+generic' ai/topics/local-ci.md ||
  echo 'local-ci.md missing generic readiness-hook contract'
{ grep -qiF 'sccache' ai/topics/local-ci.md &&
    grep -qiF 'adapter' ai/topics/local-ci.md; } ||
  echo 'local-ci.md missing explicit sccache-adapter ownership'
grep -nE 'KIO_CARGO_LOCK|flock|`nproc`|nproc slots|Windows[^.]+(direct route|bypass)' \
  AGENTS.md ai/topics/local-tools.md ai/topics/local-ci.md \
  ai/topics/local-performance.md TESTING.md test-data/README.md &&
  echo 'stale scheduler/cargo-lock claim remains in local-running guidance'
```

Findings:

- **Missing scoping guidance** — `local-ci.md` lost the decision table or one of its required rows/caveats.
- **Missing performance guidance** — `local-performance.md` lost the warmup, contention, failure-handling, authority-wait, multi-session scenario, or worktree-mutation safety guidance.
- **Scheduler-contract drift** — local guidance loses the one native three-resource model, portable work-capacity query, native macOS/Windows evidence, explicit generic readiness hook, or cross-target evidence limit; or it revives an external Cargo lock, `flock`, `nproc` fallback, shell policy engine, or Windows bypass.

## 3. Ownership stays sharp

Each file owns a distinct kind of information:

- `local-tools.md` explains third-party tooling and local entry-point mechanics. It may show how to configure `sccache`, inspect wrapper state, or enable the scheduler-native Cargo resource; it should not advise when a workflow should use those tools beyond brief cross-links.
- `local-ci.md` explains check infrastructure mechanics. It may say `ci/all.sh FULL_IMPL_MATRIX` is heavier than `SAMPLE_IMPL` and explain selector semantics; it should not carry broader local performance strategy.
- `local-performance.md` carries the local strategy and coordination advice. It should link back to the tools and CI mechanics rather than restating setup or selector details at length.

Detection — surface likely ownership drift:

```sh
grep -rniE 'fan[ -]out|integration worktree|parallel local|long-lived .*worktree|warm `?target|cold `?kio-rs|redundant broad|serialize cold' \
  ai/topics/local-tools.md ai/topics/local-ci.md
grep -niE 'RUSTC_WRAPPER|KIO_TEST_RUNNER_COMPILER_WRAPPER|SCCACHE_DIR|KIO_CI_SERIALIZE_CARGO|--impls=|--gen-count|FAILED:|PASSED:' \
  ai/topics/local-performance.md
```

For each hit, classify:

- **Mechanism / cross-link** — the file briefly names an adjacent concept to route the reader. Pass.
- **Ownership drift** — a file explains another file's subject in detail. Finding: move the detail to the owning topic and leave a cross-link.

## 4. Performance guidance stays advisory and machine-agnostic

`local-performance.md` must read as guidance, not law. Hedged phrasing ("prefer", "usually", "tends to", "normally", "tradeoff") is correct. Imperative mandates presented as the only valid procedure are the failure. The delegated/background lifecycle and worktree mutation-context safeguard are correctness rules rather than performance heuristics, so their imperative form is expected.

```sh
grep -niE '\b(must|always|never|required|mandatory|forbidden)\b' \
  ai/topics/local-performance.md
```

For each hit, classify:

- **Advice / contract pointer** — hedged, or pointing to a rule owned by AGENTS.md or `local-ci.md`. Pass.
- **Dictation** — the doc itself elevates a performance heuristic to a mandate with no "prefer / usually / judgment call" escape. Finding: soften to advisory framing.

None of the three local topics may pin a developer machine's specs: no core counts, RAM figures, named box specs, disk sizes, or concrete local checkout/worktree paths.

```sh
grep -niE '[0-9]+ *-?(cores?|cpus?|threads?|gb|gib|mb|mib|tb)\b' \
  ai/topics/local-tools.md ai/topics/local-ci.md ai/topics/local-performance.md
grep -niE '[0-9]+(\.[0-9]+)? *(gb|gib|mb|tb)?\s*(free|of ram|memory|disk)' \
  ai/topics/local-tools.md ai/topics/local-ci.md ai/topics/local-performance.md
grep -niE '(^|[[:space:]`"])(/tmp/|/home/|~/|~[[:alnum:]_-]+/)' \
  ai/topics/local-tools.md ai/topics/local-ci.md ai/topics/local-performance.md
```

Classify per hit: a tool's *own documented location* (`~/.ghcup/bin`, `~/.cache/sccache`, an `$XDG_CACHE_HOME` default) is the file's subject matter and portable — pass; a maintainer's checkout path, worktree layout, or hardware quantity is a finding. See [`ai/topics/no-leak.md`](../../topics/no-leak.md) § The portability filter for the litmus.

Findings:

- **Prescriptive drift** — `local-performance.md` dictates a performance procedure as the single correct way instead of advising.
- **Machine leak** — a local-* topic pins a concrete hardware quantity or a maintainer's local path.

## 5. The caches index stays a resolving, mechanics-free routing layer

[`ai/topics/caches.md`](../../topics/caches.md) is a routing index over every cache in the dev loop; the local-running topics, the [`clear-caches`](../clear-caches/SKILL.md) skill, [`ai/topics/implementation.md`](../../topics/implementation.md), and `specs/cli.md` own the mechanics.

- **Rows resolve** — every owning-doc citation in the index (file § heading) points at a file and heading that exist; verify with anchor greps like § 2's.
- **No orphan caches** — every cache the owning homes document has a row: the two cache layers plus sccache in `local-tools.md` § Compiler cache, `local-ci.md` § Kio-semantic caches, the on-disk rows of `implementation.md` § Compiler performance architecture's inventory, the `clear-caches` skill's § What it clears table, and `kio cache` in `specs/cli.md`. A cache documented in an owning home but missing from the index is a finding; so is an index row no owning home still documents.
- **The index stays an index** — no env-var recipes, no clearing procedures, no keying mechanics beyond naming a knob. A row that starts teaching is a finding; the content belongs in the owning doc (the same routing discipline [`audit-kio-guide`](../audit-kio-guide/SKILL.md) enforces for `kio-authoring.md`).
- **Runner toolchain caches stay pinned and accounted-for** — the doc-to-reality half the index alone can't see. A cache-backed runner adapter that runs a compiler pins that compiler's default toolchain caches under its staging tempdir (Go's `GOCACHE`, swiftc's `-module-cache-path`); left unpinned it writes to a machine-shared default (`~/.cache/go-build`, `~/.cache/clang/ModuleCache`) — the leak class the swiftc clang module cache was, invisible to a doc-to-doc audit because it lived in no doc. The index's "Not persistent dev-loop caches" note must account for each such pin, and the mechanical sweep [`ci/checks/orchestrators/runner-cache-hermeticity.sh`](../../../ci/checks/orchestrators/runner-cache-hermeticity.sh) must probe every cache-backed target.
- **Emission scratch stays temporary** — emission scripts copy their immutable `workdir/` below the harness-provided `TMPDIR`, build only from that copy, and the orchestrator pins `--cache-base` below its temporary root. The caches index must account for that state as non-persistent and the [`clear-caches`](../clear-caches/SKILL.md) skill must not invent a checked-in emission `out/` tree or claim emissions use the shared runner artifact cache.

```sh
grep -qF 'GOCACHE' ci/infra/kio-test-runner-rs/src/go/bin_cache/mod.rs ||
  echo 'go runner adapter no longer pins GOCACHE'
grep -qF 'module-cache-path' ci/infra/kio-test-runner-rs/src/swift/bin_cache/mod.rs ||
  echo 'swift runner adapter no longer pins -module-cache-path'
grep -qiE 'GOCACHE|module cache' ai/topics/caches.md ||
  echo 'caches.md no longer accounts for the pinned runner toolchain caches'
for t in rust go haskell swift; do
  grep -qF "probe_target $t" ci/checks/orchestrators/runner-cache-hermeticity.sh ||
    echo "runner-cache-hermeticity.sh does not probe cache-backed target: $t"
done
grep -qiE 'emission.*(temporary|TMPDIR)|temporary.*emission' ai/topics/caches.md ||
  echo 'caches.md no longer accounts for transient emission builds'
grep -qiE 'emission.*(no|never|leave no).*(out/|shared runner artifact cache)' ai/skills/clear-caches/SKILL.md ||
  echo 'clear-caches skill no longer excludes transient emission state'
```

## 6. The CI command-line surface stays paired with the instructions

`ci/all.sh` is the command agents are *instructed* to run, so its flags are documentation ([`ai/topics/local-ci.md`](../../topics/local-ci.md) § Changing the CI command-line surface updates the instructions with it). A flag that exists in the script but in none of the guidance is a flag an agent will never use; a flag documented but no longer accepted is worse — it makes a documented command exit 2.

Both halves are mechanically checkable. Extract the long flags from `ci/all.sh`'s usage text and require each to appear in `TESTING.md` or `local-ci.md`; then take every long flag the guidance and the corpus READMEs *name* for an orchestrator and require the script to still accept it.

`--help` is excluded throughout: every script takes it and no script prints it in its own body.

```sh
# Every ci/all.sh flag reaches the guidance.
for f in $(sh ci/all.sh --help | grep -oE '^  --[a-z-]+' | tr -d ' ' | grep -v '^--help$'); do
  grep -qF -- "$f" TESTING.md ai/topics/local-ci.md ||
    echo "ci/all.sh flag undocumented in TESTING.md / local-ci.md: $f"
done

# Every orchestrator flag the docs hand a reader is still accepted. A flag
# the script rejects makes a copy-pasteable documented command exit 2.
for o in golden emissions poc castle contrib; do
  accepted=$(sh "ci/checks/orchestrators/$o-tests.sh" --help)
  for f in $(grep -ohE -- "$o-tests\.sh[^\`]*" TESTING.md ai/topics/local-ci.md \
               test-data/README.md test-data/emissions/README.md \
               test-data/castles/README.md ai/skills/*/SKILL.md 2>/dev/null |
             grep -oE -- '--[a-z-]+' | grep -v '^--help$' | sort -u); do
    printf '%s' "$accepted" | grep -qF -- "$f" ||
      echo "$o-tests.sh: documented flag no longer accepted: $f"
  done
done
```

Report a hit either way. Then read the prose around each surviving reference: a mechanical check confirms a flag *exists*, never that the sentence describing it is still true (a corpus whose default flipped from whole to sampled keeps every flag name valid while every claim about its coverage goes stale).

## How to report

Group findings by severity:

1. **Broken split** — missing topic, missing AGENTS.md pointer, retired `ci.md` / `parallel-agents.md` topic still present, or stale topic links.
2. **Missing guidance anchors** — the scoped-coverage table, symlink caveat, warmup, contention, failure-handling, or multi-session guidance is absent.
3. **Ownership drift** — one local-running topic explains another topic's subject in detail.
4. **Prescriptive drift** — `local-performance.md` hardens an advisory performance heuristic into a mandate.
5. **Machine leak** — a local-* topic pins concrete machine/worktree detail.
6. **Caches-index drift** — an index row whose owning-doc citation doesn't resolve, a cache documented in an owning home with no row, index content teaching mechanics instead of routing, or a runner toolchain cache left unpinned / unaccounted-for / unprobed (the hermeticity static half).
7. **CI CLI / instruction drift** — a `ci/all.sh` flag no guidance names, a documented orchestrator flag the script no longer accepts, or a prose claim about coverage (including local-all / GitHub one-per-backend emissions) that the scripts' current defaults contradict.
8. **Scheduler-contract drift** — more than one scheduling authority, a stale external-lock/platform-bypass claim, a hidden tool-name special case, or native platform behavior credited only from cross-target compilation.

For each finding, cite `file:line` and quote the offending text. Distinguish genuine findings from benign keyword/reference hits; the greps over-match by design.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).
