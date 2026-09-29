#!/bin/sh
#
# Kio test runner.
#
# Walks a directory of test cases and runs each case against one or
# more implementations. Each case directory contains expected.* golden
# files plus exactly one execution contract: run.args for the standard
# test+build+runner path, run.sh for a custom script, or run.test-only
# for a library (kio test + kio build, no runner). A KNOWN_FAILING marker
# inverts the verdict: the case is expected to fail (an expected failure
# warns and stays green; an unexpected pass fails, flagging a stale
# marker). The harness diffs actual output against those goldens and
# reports per-implementation pass/fail.
#
# A case directory looks like:
#
#   <cases-dir>/<NN_category>/<name>/
#     workdir/                   # directory where the case commands run
#       <name>.pkg.kio           # package file (entry point)
#       *.kio                    # additional modules
#     run.args                   # standard runner argv, whitespace-separated
#                                # simple arguments. Empty or whitespace-only
#                                # means the default runner invocation. The
#                                # harness runs `kio test` first unless
#                                # IS_KIO_PRIME is present, then builds and runs.
#     run.sh                     # custom script, mutually exclusive
#                                # with run.args / run.test-only.
#     run.test-only              # library marker (no main): static
#                                # validation only — kio test + kio build,
#                                # no runner, no protocol, nothing executed
#                                # (a case needing its program run uses
#                                # run.args). Mutually exclusive.
#     expected.stdout            # stdout golden
#     expected.stderr            # exact stderr golden; rare, whole-stream contract
#     expected.exit              # single line: integer exit code
#     expected.stderr.ignore     # skip the stderr assertion entirely.
#                                # Mutually exclusive with
#                                # expected.stderr and
#                                # expected.stderr.grep (case fails
#                                # if more than one is present). Default
#                                # for successful tool/build/run cases.
#     expected.stderr.grep       # optional; each non-empty, non-
#                                # comment line is a POSIX ERE that
#                                # must match somewhere in actual
#                                # stderr (`grep -E -q`). Blank lines
#                                # and lines beginning with `#` are
#                                # ignored. The file must carry at
#                                # least one regex line; an empty
#                                # `.grep` is a harness error.
#                                # Mutually exclusive with
#                                # expected.stderr and
#                                # expected.stderr.ignore. Use when
#                                # stable stderr content should be pinned
#                                # while allowing extra warnings or notes.
#     SKIP_KIO_FMT_CHECK         # optional marker; skip the
#                                # ci/checks/per-case/fmt-canonical.sh
#                                # canonical-form check. Use for cases
#                                # whose subject is deliberately non-
#                                # canonical input (fmt round-trip
#                                # fixtures, parser-acceptance fixtures).
#     IS_KIO_PRIME               # optional marker, asserted by
#                                # ci/checks/per-case/prime-marker.sh: when
#                                # present, every regular-module .kio
#                                # file in the case must parse as Kio'.
#                                # Also gates --prime-only runs (only
#                                # IS_KIO_PRIME-marked cases are visited).
#     SKIP_KIO_PRIME_RUN         # optional marker; opts the case out of
#                                # --prime-only runs even when its
#                                # sources parse as Kio'. Use for cases
#                                # whose execution depends on full-surface
#                                # behavior outside kio-prime's semantics
#                                # (for example, flat calls over multiple
#                                # function layers or custom scripts that
#                                # invoke `kio test`). The IS_KIO_PRIME
#                                # biconditional check still runs and
#                                # asserts the source-side property.
#     DYN_LOAD_PRIME             # optional marker, gating
#                                # --dyn-load-prime-only
#                                # runs: when present, the dyn-load-prime
#                                # differential runner
#                                # (kio-test-runner-dyn-load-prime) loads the
#                                # case's emitted Kio' image through
#                                # dyn_load_prime.
#                                # It then mirrors the compiled route: load only
#                                # for empty-main, exported `main` for ordinary
#                                # main protocols, or scripted exports for
#                                # export-* protocols.
#                                # The runner feeds the whole emitted tree
#                                # (all modules, host modules, manifest) to
#                                # load_package. Declared when the
#                                # case's host calls are within the
#                                # testapi-dyn-load vocabulary. Pairs with a
#                                # `target kio-prime` build block.
#     UNKNOWN_BUILD_TARGET       # custom run.sh expects build error 40 for
#                                # an unknown target. Routes independently
#                                # of target names; still requires a declared
#                                # target and retains ordinary sampling.
#     RUN_EARLY                  # optional scheduling marker. After filters,
#                                # target applicability, implementation
#                                # sampling, and case narrowing have frozen the
#                                # worklist, buffered parallel runs dispatch
#                                # every selected (case, binary) unit for this
#                                # case before unmarked units. It never changes
#                                # selection or canonical report order. One-
#                                # worker, single-unit, --show-output, and
#                                # --update-expected runs execute canonically.
#
# <NN_category> matches a row in specs/exit-codes.md (e.g.,
# 00_success, 11_parse_error). The runner walks the tree
# recursively, so any depth is fine; cases are identified by the
# presence of expected.exit.
#
# ---- Execution model -------------------------------------------------
#
# The outer loop is **(case, binary)-major**: for each pair of a case
# and a distinct compiler binary across the case's applicable impls,
# the harness:
#
#   1. Runs `<binary> cache clear` once with cwd = the case's package
#      root (where the package file lives). This wipes the package-local
#      Kio-semantic cache entries the build block's `cache ...`
#      directive points at. Typed modules with identical semantic keys may
#      still reuse the harness's run-scoped shared root; cache-specific checks
#      shadow that root when they require a genuinely cold first command.
#      Marked custom cases instead clear their private execution copies below.
#   2. Runs all impls that share that binary on this case
#      **sequentially**, so they hit the same warm cache as the unit
#      progresses. Parse/typecheck/lower work done by the first impl
#      is reused by the second (e.g., the kio@js → kio@rust pair
#      sharing the `kio` binary).
#
# JOBS-way parallelism iterates over `(case, binary)` units across the
# corpus: different cases, and different binaries on the same case,
# run in parallel; impls within the same `(case, binary)` unit
# serialize on the shared cache. The case directory itself is used as
# the working directory, except that custom `run.sh` cases marked
# SKIP_DEP_MATERIALIZED execute in a whole-case copy per implementation.
# Their dependency fetch may leave partial trees: only harness-owned scratch
# is removed, never untracked authored inputs in the source case. Checks and
# expected-result updates retain the source case; copies are made after checks
# so updated fixtures reach execution. Sibling path dependencies stay together.
# Build outputs, package-local caches, and runner caches remain isolated
# across `(case, binary)` units. Harness-owned standard and test-only paths
# share the invocation's typed-module cache so identical modules copied into
# distinct package roots are typechecked once. A custom `run.sh` receives a
# typed root under its own per-case, per-implementation scratch directory by
# default. A caller may give a separately reviewed custom cohort one isolated
# run-scoped root through the harness-owned path+content manifest selected by
# `--custom-typed-cache-cohort`; unlisted scripts stay isolated, while a
# listed script whose content no longer authenticates stops the run.
#
# Build artifacts that `kio build` writes into `<case>/workdir/out/<target>/`
# accumulate in the case tree across runs (gitignored). The cache
# clear is targeted at the Kio-semantic on-disk caches, not at the
# build output.
#
# Usage:
#   sh ci/run-tests.sh \
#     --cases-dir=<dir> \
#     --cache-base=<dir> \
#     --impl-def=name=<n>,kio=<bin>,runner=<bin> \
#     [--impl-def=name=<n>,kio=<bin>,runner=<bin> ...] \
#     [-u|--update-expected] [--show-output] [--impls=SAMPLE_IMPL] \
#     [--jobs=<N>|auto] [--compiler-jobs=<N>] [<filter>...]
#
# Each --impl-def= value is a comma-separated key=value list with three
# required keys and two optional keys (order-independent):
#   name    display label used in output
#   kio     path to the kio compiler binary
#   runner  path to the program that executes a `kio build` output
#   target  target id (default: js)
#   prime-kio  reduced Kio' compiler used by phase-path checks
#   runner-cache-kind  optional non-executable cache adapter identity
#
# Two impl rows whose `kio=` resolves to the same absolute path share
# a binary and are grouped into the same `(case, binary)` unit.
# Path-bearing relative `kio=`, `runner=`, and `prime-kio=` values are
# resolved against the harness invocation directory before any worker
# changes into a case directory.
#
# Comma-in-paths is unsupported; commas are the field separator.
# Tab characters in any field are also unsupported (used internally).
#
# Environment exported to each case's custom run.sh:
#   KIO_BIN     harness admission proxy for the kio binary set by --impl-def.
#   KIO_RUNNER  harness-owned target-runner proxy. For an exact single-package
#               case, it supplies the top-level package name only when the
#               invocation has no explicit split or `=` --package-name.
#   KIO_TEST_RUNNER_CACHE_KIND
#               optional non-executable cache adapter identity for checks.
#   KIO_TARGET  target id for this impl.
#   KIO_TEST_RUNNER_BUILD_CACHE_DIR
#               target-scoped cache directory for runner-built artifacts.
#   KIO_TEST_RUNNER_BUILD_CACHE_SIZE
#               optional max size for runner-built artifact caches.
#   KIO_TEST_RUNNER_COMPILER_WRAPPER
#               optional compiler wrapper for compatible target runners.
#   KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER
#               optional internal debug executable placed around actual native
#               compiler invocations by the Rust, Go, Java, Haskell, and Swift
#               runners. One opaque executable; no shell splitting.
#   KIO_TEST_RUNNER_CACHE_DISABLE
#               optional 1/0 cache kill switch for target runners.
#   KIO_TEST_RUNNER_PROFILE
#               optional optimization profile (unoptimized/default/
#               optimized) for the compiled runners; default `default`.
#   KIO_DEBUG_TYPED_CACHE_ROOT
#               absolute typed-module cache root. Custom scripts receive a
#               per-case, per-implementation scratch root unless their exact
#               path and content are in the harness-owned reviewed cohort;
#               that cohort receives its own run-scoped root.
#   PATH        prefixed with harness-local proxies for kio, kio-prime, cargo,
#               rustc, go, javac, swiftc, and ghc. Compiler-producing commands
#               enter shared compiler admission; cheap Kio management and
#               non-compiling go commands pass through directly. Custom scripts
#               must preserve this prefix, use KIO_BIN for Kio, and invoke host
#               tools by bare name.
#
# Under the shared scheduler, a custom run.sh already owns a case-unit slot.
# Its PATH-routed Kio, Cargo, and native compiler commands enter only the
# compiler resource: they do not enter ci/cargo.sh or reacquire inherited
# scheduler resources, which could make a case wait on its own process tree.
# Emitted and fixture crates keep their ordinary command spellings.
#
# Flags:
#   --cases-dir=<dir>      REQUIRED. Directory of test cases.
#   --cache-base=<dir>     REQUIRED. Root for per-target runner build caches.
#   --impl-def=<spec>      REQUIRED, repeatable. Defines one implementation.
#   -u, --update-expected  overwrite expected.* files with actual output.
#                          Use when observable behavior changes intentionally.
#   --show-output          stream each case's stdout/stderr to the
#                          terminal as it runs (still captured for diff).
#                          Pairs well with a filter to focus on one case;
#                          mass runs will interleave output with pass/FAIL.
#   --jobs=<N>|auto        number of `(case, binary)` units to run in
#                          parallel across the corpus. Default `auto`
#                          resolves through the native scheduler's available
#                          parallelism query. --jobs=1 forces serial.
#                          Auto-disabled to serial (with a note on stderr)
#                          under --show-output, --update-expected, or
#                          when xargs -P is missing; silently disabled
#                          when only one unit is selected. In parallel
#                          mode each unit's stdout/stderr/pass-fail lines
#                          are buffered and emitted in worklist order at
#                          the end of the run. A completed failing unit also
#                          emits a compact live FAIL marker immediately.
#   --compiler-jobs=<N>    shared cap for top-level Cargo invocations,
#                          compiler-producing Kio commands, and actual native
#                          compiler commands. Omission uses the conservative
#                          hard cap of 2; a numeric value is a fixed cap.
#   --exclude=<regex>      POSIX ERE matched against the case name; cases
#                          matching any --exclude are skipped. Repeatable.
#                          Excludes win over includes.
#   --check=<path>         per-case check script (repeatable). Each check
#                          declares its routing via a `# ROUTING: <kind>`
#                          marker comment near the top:
#                            `# ROUTING: case-binary` — runs once per
#                              (case, binary) before the first impl on
#                              that unit. Use for checks that depend on
#                              the case source (or KIO_BIN) but not on
#                              the specific impl's runner/target. Gets
#                              KIO_BIN, KIO_PRIME_CHECK_BIN, KIO_TEST_UPDATE
#                              exported.
#                            `# ROUTING: impl` (the default) — runs once
#                              per (case, impl) before that impl's case run.
#                              A `# REQUIRES: runner` marker skips the
#                              check for compile-only `runner=SKIP` rows.
#                              Gets KIO_BIN, KIO_PRIME_BIN, KIO_RUNNER,
#                              KIO_TARGET,
#                              KIO_TEST_RUNNER_BUILD_CACHE_DIR,
#                              KIO_TEST_RUNNER_BUILD_CACHE_SIZE,
#                              KIO_TEST_RUNNER_COMPILER_WRAPPER,
#                              KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER,
#                              KIO_TEST_RUNNER_CACHE_DISABLE,
#                              KIO_TEST_RUNNER_PROFILE,
#                              KIO_TEST_UPDATE
#                              exported.
#                          check cwd is the original case directory in either
#                          case. Mutating checks (e.g. kio-prime-roundtrip
#                          which builds an alternate target) are
#                          responsible for their own scratch isolation;
#                          read-only checks (fmt-canonical, prime-marker)
#                          operate on cwd directly. Failures roll up
#                          alongside case-run failures.
#                          See ci/checks/per-case/ for the
#                          pre-built per-case checks the bootstrap uses.
#   --prime-only           only visit cases that carry an IS_KIO_PRIME
#                          marker file. Pair with a Kio'-only --impl-def=
#                          (e.g. the kio-rs `kio-prime` binary) to
#                          assert the marked cases really are Kio'.
#                          Non-marked cases are silently skipped.
#   --impls=SAMPLE_IMPL    for each case, pick exactly one of the
#                          impls applicable to that case (target-gated
#                          per build block) and run only that impl —
#                          random per run, per case. The candidate
#                          pool is the set of --impl-def= entries on
#                          the command line; pass a single --impl-def=
#                          to scope to one backend (or, from the
#                          wrappers, use --impls=<name>). Trades
#                          backend-divergence coverage for ~½ the wall
#                          time; use targeted full-matrix or explicit
#                          impl runs when backend divergence matters.
#                          A case applicable to only one configured
#                          impl runs on that impl (sampling
#                          becomes a no-op for it). See TESTING.md
#                          § Local iteration for the full interaction.
#                          Performance comparisons may set the internal
#                          KIO_DEBUG_SAMPLE_IMPL_SEED documented in
#                          ai/topics/local-performance.md; ordinary runs
#                          remain randomly sampled.
#   --sample-cases=<N>|all how many CASES per top-level bucket run their
#                          impls. `all` (the default) runs every case.
#                          With a count, the lowest-ranked N cases per
#                          bucket run their impls; every other case still
#                          becomes a unit and still runs its
#                          `# ROUTING: case-binary` checks, so nothing a
#                          corpus-wide check would catch can hide in the
#                          unsampled tail. Ranking is a seeded hash of the
#                          case name, so a draw reproduces from its seed.
#                          Do not confuse with --impls=SAMPLE_IMPL, which
#                          samples IMPLS per case; the two compose.
#   --case-seed=<S>        seed the --sample-cases draw. Default: the
#                          GitHub Actions run context when present, else
#                          UTC epoch seconds. The effective seed is always
#                          printed. Requires --sample-cases.
#   --custom-typed-cache-cohort=<id>
#                          harness-owned reviewed custom run.sh cohort that may
#                          share one separate run-scoped typed root. The only
#                          accepted id is `exec-dyn-load-goldens-v1`.
#   -h, --help             show this help and exit.
#
# Filters:
#   Any non-flag positional arguments are POSIX EREs matched against case
#   names (relative path under --cases-dir). Cases matching at least one
#   include regex (or all cases, if none are given) are kept; cases
#   matching any --exclude regex are then dropped.
#
# Output:
#   Each case is reported as `pass [<impl>] <case>` or
#   `FAIL [<impl>] <case>`. At the end, a per-impl summary is printed;
#   if 2+ impls were run and any case has differing statuses across
#   impls, a divergent-cases table is printed too. In parallel mode, a
#   completed failing unit also emits a compact live marker to stderr, or
#   to ci/all.sh's progress stream when launched under that wrapper.
#
#   The same live channel carries a progress heartbeat every
#   $KIO_DEBUG_PROGRESS_INTERVAL seconds (default 300): units done /
#   running / queued, passing result rows, and elapsed minutes. Under
#   case narrowing it reports progress through the units whose impls
#   actually build and run, through the checks-only remainder, and through
#   all units. A tick where
#   nothing completed also names the in-flight cases. A failure count is
#   appended only when non-zero, so a `fail` hit in this stream is always
#   real. Without the heartbeat a long run is silent under
#   ci/all.sh (which buffers task stdout), so a healthy run and a wedged
#   one look the same until the end. Set the interval to 0 to disable.
#   Skipped for single-unit runs and under --show-output /
#   --update-expected, which already stream each unit.
#
# Exit status: 0 if every implementation passes every selected case;
# non-zero otherwise.
#
# POSIX sh only. Parallelism uses xargs -P, which is widely available
# but not strictly POSIX; the script falls back to serial when -P
# isn't supported by the local xargs.

set -u

TAB=$(printf '\t')

# A single literal newline. Command substitution strips trailing
# newlines, so `$(printf '\n')` would be empty; assign a real newline
# directly. Used by `compare` to strip at most one trailing newline.
NL='
'

emit_live_progress() {
  elp_line=$1
  if [ -n "${KIO_CI_TASK_NAME:-}" ]; then
    elp_line="ci/run-tests.sh: LIVE ${KIO_CI_TASK_NAME}: $elp_line"
  else
    elp_line="ci/run-tests.sh: LIVE $elp_line"
  fi

  if [ "${KIO_CI_PROGRESS_FD:-}" = 3 ]; then
    printf '%s\n' "$elp_line" 2>/dev/null >&3 || :
  else
    printf '%s\n' "$elp_line" >&2
  fi
}

usage() {
  cat <<EOF
Usage: sh $0 \\
         --cases-dir=<dir> \\
         --cache-base=<dir> \\
         --impl-def=name=<n>,kio=<bin>,runner=<bin> \\
         [--impl-def=...] \\
         [-u|--update-expected] [--show-output] [--prime-only] \\
         [--impls=SAMPLE_IMPL] [--jobs=<N>|auto] [--compiler-jobs=<N>] \\
         [--sample-cases=<N>|all] [--case-seed=<S>] \\
         [--custom-typed-cache-cohort=<id>] \\
         [--exclude=<regex>...] [--check=<path>...] \\
         [<filter>...]

Walks <cases-dir> and checks each case's stdout, stderr policy, and
exit status against its expected files, for each --impl-def= given.

--impl-def= takes a comma-separated key=value list with three required
keys and three optional keys (order-independent):
  name    display label used in output
  kio     path to the kio compiler binary
  runner  path to the program that executes a kio build output, or the
          literal SKIP for a compile-only impl (run kio test + kio build,
          then stop — never invoke a runner or diff program output; used by
          the macOS / Windows portability sweep). A SKIP impl skips run.sh
          (custom-execution) cases as inapplicable.
  target  target id for this implementation (default: js).
  prime-kio
          path to the reduced Kio' compiler. Required when the
          kio-prime-roundtrip impl check is configured.
  runner-cache-kind
          non-executable artifact-cache adapter identity for impl checks.

Repeat --impl-def= for multiple implementations. Comma-in-paths is
unsupported; tab characters in any field are unsupported.
Path-bearing relative executable values are resolved against the directory
from which ci/run-tests.sh was invoked.

Execution is (case, binary)-major: impls sharing a binary on the
same case serialize so they share a warm package-local Kio-semantic cache,
with \`<binary> cache clear\` run once at the start of each unit. Harness-owned
paths additionally share one run-scoped typed-module root. Each custom run.sh
uses its own per-case, per-implementation scratch root unless a trusted caller
binds its exact path and content into a separate shared root with
--custom-typed-cache-cohort.
Different cases (and different binaries on the same case) run in
parallel up to --jobs.

Environment exported to each custom run.sh:
  KIO_BIN     harness admission proxy for the kio binary set by --impl-def.
  KIO_RUNNER  harness-owned target-runner proxy. With exactly one discoverable
              package in a regular, non-symlink top-level manifest, it supplies
              that name only absent an explicit split or \`=\` --package-name;
              other scripts pass their ordered descriptors to the same handle.
  KIO_TEST_RUNNER_CACHE_KIND
              optional non-executable cache adapter identity for checks.
  KIO_TARGET  target id for this impl.
  KIO_TEST_RUNNER_BUILD_CACHE_DIR
              target-scoped cache directory for runner-built artifacts.
  KIO_TEST_RUNNER_BUILD_CACHE_SIZE
              optional max size for runner-built artifact caches.
  KIO_TEST_RUNNER_COMPILER_WRAPPER
              optional compiler wrapper for compatible target runners.
  KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER
              optional internal debug executable placed outermost around each
              actual native compile. One opaque executable, not shell syntax.
  KIO_TEST_RUNNER_CACHE_DISABLE
              optional 1/0 cache kill switch for target runners.
  KIO_TEST_RUNNER_PROFILE
              optional optimization profile for the compiled runners:
              unoptimized, default, or optimized. Unset selects default.
  KIO_DEBUG_TYPED_CACHE_ROOT
              absolute typed-module cache root. Custom scripts receive a
              per-case, per-implementation scratch root unless their exact
              path and content are in the harness-owned reviewed cohort;
              that cohort receives its own run-scoped root.
  PATH        prefixed with harness-local proxies for kio, kio-prime, cargo,
              rustc, go, javac, swiftc, and ghc. Compiler-producing commands
              enter --compiler-jobs admission; cheap Kio management and
              non-compiling go commands pass through directly. Custom scripts
              must preserve this prefix, use KIO_BIN for Kio, and invoke host
              tools by bare name.

Flags:
  --cases-dir=<dir>      REQUIRED. Directory of test cases.
  --cache-base=<dir>     REQUIRED. Root for per-target runner build caches.
  --impl-def=<spec>      REQUIRED, repeatable.
  -u, --update-expected  overwrite expected.* files with actual output.
  --show-output          stream each case's stdout/stderr live to the
                         terminal (still captured for diff). Pairs well
                         with a filter to focus on one case.
  --jobs=<N>|auto        parallel (case, binary) units. Default auto uses the
                         native scheduler's available parallelism.
                         Auto-disabled to serial under --show-output,
                         --update-expected, single-unit selection, or
                         missing xargs -P. In parallel mode, completed
                         failing units also emit compact live FAIL markers.
  --compiler-jobs=<N>    cap top-level Cargo invocations, compiler-producing
                         Kio commands, and actual native compiler commands
                         across worktrees. Omission uses the conservative hard
                         cap of 2; a numeric value is a fixed cap.
  --keep-cache           skip the per-unit \`<binary> cache clear\`, reusing
                         warm caches across units. For large compile-only
                         sweeps; cache correctness is covered by the
                         exec_rlib_cache_* goldens.
  --exclude=<regex>      POSIX ERE matched against case names; cases
                         matching any --exclude are skipped. Repeatable.
                         Excludes win over includes.
  --check=<path>         per-case check script (repeatable). Routing is
                         declared in the script via a \`# ROUTING:\`
                         marker (case-binary | impl, default impl). See
                         ci/checks/per-case/. An impl check declaring
                         \`# REQUIRES: runner\` skips runner=SKIP rows.
  --prime-only           only visit cases that carry an IS_KIO_PRIME
                         marker; non-marked cases are silently skipped.
  --dyn-load-prime-only
                         only visit cases that carry a DYN_LOAD_PRIME
                         marker; non-marked cases are silently skipped.
                         Pair with the dyn-load-prime --impl-def= so the
                         dyn_load_prime runner visits only the cases that
                         opt in. Marker coverage is enforced by
                         ci/checks/repo-lint/dyn-load-prime-coverage.sh.
  --impls=SAMPLE_IMPL    for each case, run exactly one applicable
                         impl, picked at random per run. Trades full
                         backend coverage for half the wall time; add
                         targeted full-matrix or explicit impl runs
                         when backend divergence matters. The
                         candidate pool is the --impl-def= set you pass;
                         the wrappers expose --impls=<name> to narrow
                         it. See TESTING.md § Local iteration.
  --sample-cases=<N>|all how many CASES per top-level bucket run their
                         impls (build + run + impl-routed checks). \`all\`
                         is the default. Sampled-out cases still run their
                         \`# ROUTING: case-binary\` checks, so corpus-wide
                         invariants stay gated on every case. Distinct
                         from --impls=SAMPLE_IMPL, which samples impls per
                         case; the two compose.
  --case-seed=<S>        seed the --sample-cases draw so it reproduces.
                         Default: the GitHub Actions run context, else UTC
                         epoch seconds. Always printed. Requires
                         --sample-cases.
  --custom-typed-cache-cohort=<id>
                         select a harness-owned reviewed custom run.sh cohort.
                         Listed scripts share one separate run-scoped typed
                         root only after exact content authentication; unlisted
                         scripts stay isolated. The only accepted id is
                         exec-dyn-load-goldens-v1. Internal configuration.
  -h, --help             show this help.

Filters:
  Non-flag positional args are POSIX EREs matched against case names;
  cases matching at least one include (or all cases, if none are given)
  are kept; --exclude regexes then drop matches.
EOF
}

# Parse a --impl-def= spec into PARSED_NAME, PARSED_KIO, PARSED_RUNNER,
# PARSED_TARGET, PARSED_PRIME_KIO, PARSED_RUNNER_CACHE_KIND. The `target=` key is optional and defaults to `js`
# for backward compatibility with the original single-target
# harness; specifying it lets a per-impl row name its target so
# `KIO_TARGET` reaches the case run and the matching runner
# binary is invoked.
parse_impl_spec() {
  spec=$1
  PARSED_NAME=
  PARSED_KIO=
  PARSED_RUNNER=
  PARSED_TARGET=js
  PARSED_PRIME_KIO=
  PARSED_RUNNER_CACHE_KIND=
  while IFS= read -r kv; do
    [ -n "$kv" ] || continue
    case "$kv" in
      name=*)   PARSED_NAME=${kv#name=} ;;
      kio=*)    PARSED_KIO=${kv#kio=} ;;
      runner=*) PARSED_RUNNER=${kv#runner=} ;;
      target=*) PARSED_TARGET=${kv#target=} ;;
      prime-kio=*) PARSED_PRIME_KIO=${kv#prime-kio=} ;;
      runner-cache-kind=*) PARSED_RUNNER_CACHE_KIND=${kv#runner-cache-kind=} ;;
      *) printf 'error: --impl-def: unknown key in spec %s: %s\n' "$spec" "$kv" >&2
         exit 2 ;;
    esac
  done <<HEREDOC
$(printf '%s' "$spec" | tr ',' '\n')
HEREDOC
  if [ -z "$PARSED_NAME" ]; then
    printf 'error: --impl-def=%s missing name=\n' "$spec" >&2
    exit 2
  fi
  if [ -z "$PARSED_KIO" ]; then
    printf 'error: --impl-def=%s missing kio=\n' "$spec" >&2
    exit 2
  fi
  if [ -z "$PARSED_RUNNER" ]; then
    printf 'error: --impl-def=%s missing runner=\n' "$spec" >&2
    exit 2
  fi
  for parsed_value in "$PARSED_NAME" "$PARSED_KIO" "$PARSED_RUNNER" \
    "$PARSED_TARGET" "$PARSED_PRIME_KIO" "$PARSED_RUNNER_CACHE_KIND"; do
    if [ "$parsed_value" = __KIO_EMPTY_IMPL_FIELD__ ]; then
      printf 'error: --impl-def=%s uses reserved internal field value\n' "$spec" >&2
      exit 2
    fi
  done
  case "$PARSED_RUNNER_CACHE_KIND" in
    ''|*[!A-Za-z0-9._-]*)
      if [ -n "$PARSED_RUNNER_CACHE_KIND" ]; then
        printf 'error: --impl-def=%s has invalid runner-cache-kind=\n' "$spec" >&2
        exit 2
      fi
      ;;
  esac
  case "$PARSED_NAME$PARSED_KIO$PARSED_RUNNER$PARSED_TARGET$PARSED_PRIME_KIO$PARSED_RUNNER_CACHE_KIND" in
    *"$TAB"*)
      printf 'error: --impl-def=%s contains a tab character\n' "$spec" >&2
      exit 2
      ;;
  esac
}

# index_case_targets cases_file package_paths_file symlink_paths_file
#                    generated_markers_file metadata_file
#
# Joins the corpus-wide package inventory to each selected case and writes one
# metadata row per case:
#   <case_dir><TAB><space-separated-targets><TAB><package-name-or-*><TAB><shape>
#
# `*` means target-agnostic (no root package file, NO_BUILD_BLOCK, or a
# declared target covered by UNKNOWN_BUILD_TARGET).
# An empty target list means a root package exists but declares no target;
# assert_every_case_has_build_block diagnoses that contract violation.
# `shape=exact` means the case has exactly one discoverable package and its
# manifest is a regular, non-symlink file directly under workdir. Standard
# run.args cases require that shape; custom cases may own nested or multiple
# packages and pass their runner identities explicitly.
#
# The worklist joins this index with the implementation table in bounded awk
# passes. Do not reintroduce per-(case, impl) filesystem probes here: the
# corpus-scale setup path is intentionally linear in cases plus output rows.
index_case_targets() {
  ict_cases_file=$1
  ict_package_paths=$2
  ict_symlink_paths=$3
  ict_generated_markers=$4
  ict_metadata_file=$5
  ict_workdir_symlinks=$TMPDIR_RUN/workdir-symlinks
  ict_sentinel_paths=$TMPDIR_RUN/target-routing-markers

  : >"$ict_workdir_symlinks"
  : >"$ict_sentinel_paths"
  while IFS= read -r ict_case_dir; do
    [ -n "$ict_case_dir" ] || continue
    if [ -L "$ict_case_dir/workdir" ]; then
      printf '%s\n' "$ict_case_dir/workdir" >>"$ict_workdir_symlinks"
    fi
    for ict_marker in NO_BUILD_BLOCK UNKNOWN_BUILD_TARGET; do
      if [ -f "$ict_case_dir/$ict_marker" ]; then
        printf '%s\n' "$ict_case_dir/$ict_marker" >>"$ict_sentinel_paths"
      fi
    done
  done <"$ict_cases_file"

  LC_ALL=C awk -F"$TAB" \
    -v cases_file="$ict_cases_file" \
    -v packages_file="$ict_package_paths" \
    -v symlinks_file="$ict_symlink_paths" \
    -v markers_file="$ict_generated_markers" \
    -v workdir_symlinks_file="$ict_workdir_symlinks" \
    -v sentinels_file="$ict_sentinel_paths" '
    function generated(path, discovery_root, dir) {
      dir = path
      sub(/\/[^\/]*$/, "", dir)
      while (dir != "" && dir != discovery_root) {
        if (dir in generated_dir) return 1
        if (!sub(/\/[^\/]*$/, "", dir)) break
      }
      return 0
    }
    function owning_case(path, dir, case_dir) {
      dir = path
      sub(/\/[^\/]*$/, "", dir)
      while (dir != "") {
        if (dir ~ /\/workdir$/) {
          case_dir = dir
          sub(/\/workdir$/, "", case_dir)
          if (case_dir in wanted) return case_dir
        }
        if (!sub(/\/[^\/]*$/, "", dir)) break
      }
      return ""
    }
    function record_package(path, is_symlink, case_dir, rel, dir) {
      case_dir = owning_case(path)
      if (case_dir == "") return
      rel = substr(path, length(case_dir "/workdir/") + 1)
      if (rel ~ /(^|\/)(out|target|[.][^\/]*)\// ||
          generated(path, case_dir "/workdir")) return

      package_count[case_dir]++
      if (is_symlink) symlink_count[case_dir]++
      dir = path
      sub(/\/[^\/]*$/, "", dir)
      if (!is_symlink && dir == case_dir "/workdir") {
        root_regular_count[case_dir]++
        if (!(case_dir in package_path)) package_path[case_dir] = path
      }
    }
    FILENAME == cases_file {
      order[++case_count] = $0
      wanted[$0] = 1
      next
    }
    FILENAME == markers_file {
      marker_dir = $0
      sub(/\/[.]kio-generated$/, "", marker_dir)
      generated_dir[marker_dir] = 1
      next
    }
    FILENAME == packages_file {
      record_package($0, 0)
      next
    }
    FILENAME == symlinks_file {
      record_package($0, 1)
      next
    }
    FILENAME == workdir_symlinks_file {
      case_dir = $0
      sub(/\/workdir$/, "", case_dir)
      if (case_dir in wanted) workdir_symlink[case_dir] = 1
      next
    }
    FILENAME == sentinels_file {
      marker = $0
      sub(/^.*\//, "", marker)
      case_dir = $0
      sub(/\/[^\/]*$/, "", case_dir)
      if (case_dir in wanted) {
        if (marker == "NO_BUILD_BLOCK") no_build_block[case_dir] = 1
        else unknown_build_target[case_dir] = 1
      }
      next
    }
    END {
      for (i = 1; i <= case_count; i++) {
        case_dir = order[i]
        package_name = "*"
        if (workdir_symlink[case_dir]) {
          shape = "workdir-symlink"
        } else if (package_count[case_dir] == 0) {
          shape = "missing"
        } else if (package_count[case_dir] != 1) {
          shape = "multiple"
        } else if (symlink_count[case_dir] != 0) {
          shape = "symlink"
        } else if (root_regular_count[case_dir] != 1) {
          shape = "nested"
        } else {
          shape = "exact"
          package_name = package_path[case_dir]
          sub(/^.*\//, "", package_name)
          sub(/[.]pkg[.]kio$/, "", package_name)
        }

        if (!(case_dir in package_path) ||
            ((case_dir in no_build_block) && !(case_dir in unknown_build_target))) {
          targets = (case_dir in unknown_build_target) ? "" : "*"
          print case_dir "\t" targets "\t" package_name "\t" shape
          continue
        }

        targets = ""
        while ((getline line < package_path[case_dir]) > 0) {
          if (line ~ /^[[:space:];]*target [A-Za-z_][A-Za-z0-9_-]* [{]/) {
            target = line
            sub(/^[[:space:];]*target /, "", target)
            sub(/ [{].*$/, "", target)
            targets = targets (targets == "" ? "" : " ") target
          }
        }
        close(package_path[case_dir])
        if (targets != "" && (case_dir in unknown_build_target)) targets = "*"
        print case_dir "\t" targets "\t" package_name "\t" shape
      }
    }
  ' "$ict_cases_file" "$ict_generated_markers" "$ict_package_paths" \
    "$ict_symlink_paths" "$ict_workdir_symlinks" "$ict_sentinel_paths" \
    >"$ict_metadata_file" || return 1
}

# assert_every_case_has_build_block metadata_file
# Pre-flight gate enforcing that every case carrying a root
# package file declares a `build { ... }` block (or opts out via a
# NO_BUILD_BLOCK sentinel). A package-bearing case with neither is a
# regression-hiding hole: the target join would match no implementation
# and the case would be silently skipped on every impl, so the golden
# never runs. Fail the whole run loudly instead. Reads case dirs (one
# per row) from metadata_file; returns 0 if all pass, 1 (with a
# diagnostic per offender on stderr) otherwise.
assert_every_case_has_build_block() {
  aebb_metadata_file=$1
  aebb_bad_file=$TMPDIR_RUN/missing-build-block-cases
  LC_ALL=C awk -F"$TAB" '$2 == "" { print $1 }' \
    "$aebb_metadata_file" >"$aebb_bad_file" || return 2
  [ ! -s "$aebb_bad_file" ] || {
    while IFS= read -r aebb_dir; do
      [ -n "$aebb_dir" ] || continue
      # shellcheck disable=SC2016 # %s is a printf placeholder and the backticks are literal text
      printf 'error: golden case %s declares no `build` block — every case must declare one so it is actually exercised (or place a NO_BUILD_BLOCK sentinel if the absence of a build block is the case'\''s subject)\n' \
        "$aebb_dir" >&2
    done <"$aebb_bad_file"
    return 1
  }
}

# assert_standard_case_package_shape metadata_file modes_file
#
# The standard runner protocol derives one package identity and one output
# tree from the case root. Validate that invariant once before worklist
# dispatch, including compile-only runner=SKIP rows. Custom run.sh cases own
# their package selection and may intentionally exercise nested/multi-package
# discovery; run.test-only invokes no runner and needs no injected identity.
assert_standard_case_package_shape() {
  ascps_metadata_file=$1
  ascps_modes_file=$2
  ascps_bad_file=$TMPDIR_RUN/standard-package-shape-errors
  LC_ALL=C awk -F"$TAB" -v modes_file="$ascps_modes_file" '
    FILENAME == modes_file { mode[$1] = $2; next }
    mode[$1] == "run.args" && $4 != "exact" { print $1 "\t" $4 }
  ' "$ascps_modes_file" "$ascps_metadata_file" >"$ascps_bad_file" || return 2
  [ ! -s "$ascps_bad_file" ] || {
    while IFS=$TAB read -r ascps_case ascps_shape; do
      printf 'error: %s: standard run.args case must contain exactly one discoverable package as a regular, non-symlink top-level workdir/*.pkg.kio file (found %s shape)\n' \
        "$ascps_case" "$ascps_shape" >&2
    done <"$ascps_bad_file"
    return 1
  }
}

# check_routing check_path
# Echoes the routing kind for a per-case check: `case-binary` if the
# script carries a `# ROUTING: case-binary` marker near the top,
# `impl` otherwise (the default). The marker grep is bounded to the
# first 40 lines so a stray match later in the script can't flip the
# routing.
check_routing() {
  cr_path=$1
  if head -n 40 "$cr_path" 2>/dev/null \
      | grep -Eq '^#[[:space:]]*ROUTING:[[:space:]]*case-binary[[:space:]]*$'
  then
    printf 'case-binary\n'
  else
    printf 'impl\n'
  fi
}

# check_synth_tag check_path
# Echoes the synthesized impl-tag prefix a case-binary check
# attributes to: the value of a `# SYNTH-TAG: <name>` marker near the
# top, or `invariants` (the default) when no marker is present. The tag
# names which `<tag>@<binary>` row the check's pass/FAIL lands in. Every
# current case-binary check uses the default, rolling into the shared
# `invariants@<binary>` tally; the marker lets a future distinct
# verification pass claim its own summary line instead. The marker grep
# is bounded to the first 40 lines, and the tag is restricted to a
# leading-alpha word so it cannot collide with the configured impl names
# or the `@`-bearing row encoding.
check_synth_tag() {
  cst_path=$1
  cst_tag=$(head -n 40 "$cst_path" 2>/dev/null \
    | sed -n 's/^#[[:space:]]*SYNTH-TAG:[[:space:]]*\([A-Za-z][A-Za-z0-9_-]*\)[[:space:]]*$/\1/p' \
    | head -n 1)
  if [ -n "$cst_tag" ]; then
    printf '%s\n' "$cst_tag"
  else
    printf 'invariants\n'
  fi
}

# prepare_run_args case_dir out_file target_id
# Copies run.args tokens to out_file after validating that each token is a
# simple runner argument. The parser uses whitespace splitting only; an empty
# or whitespace-only file means the default runner invocation.
#
# A run.args file stays portable across target rows by spelling a configured
# namespace as `--artifact-namespace <target-id>=<namespace>`. This harness
# consumes that map and passes only the current target's effective namespace
# to the target-local runner CLI. The runner therefore never mistakes a Kio
# package name or a target id for emitted-artifact identity.
prepare_run_args() {
  pra_dir=$1
  pra_out=$2
  pra_target=$3
  pra_file="$pra_dir/run.args"
  : >"$pra_out"
  if [ ! -f "$pra_file" ]; then
    return 0
  fi
  pra_raw=$pra_out.raw
  LC_ALL=C tr -s '[:space:]' '\n' <"$pra_file" | sed '/^$/d' >"$pra_raw"
  pra_bad_arg=$(LC_ALL=C grep -n '[^A-Za-z0-9._/@=+:-]' "$pra_raw" | sed -n '1p')
  if [ -n "$pra_bad_arg" ]; then
    rm -f "$pra_raw"
    printf 'run.args:%s: argument contains unsupported characters: %s' \
      "${pra_bad_arg%%:*}" "${pra_bad_arg#*:}"
    return 1
  fi
  pra_expect_namespace=0
  pra_namespace_error=0
  pra_package_name_error=0
  while IFS= read -r pra_arg || [ -n "$pra_arg" ]; do
    if [ "$pra_expect_namespace" = 1 ]; then
      pra_namespace_spec=$pra_arg
      pra_expect_namespace=0
    else
      case "$pra_arg" in
        --package-name|--package-name=*)
          pra_package_name_error=1
          break
          ;;
        --artifact-namespace)
          pra_expect_namespace=1
          continue
          ;;
        --artifact-namespace=*)
          pra_namespace_spec=${pra_arg#--artifact-namespace=}
          ;;
        *)
          printf '%s\n' "$pra_arg" >>"$pra_out"
          continue
          ;;
      esac
    fi
    case "$pra_namespace_spec" in
      *=*=*|'='*|*'=')
        pra_namespace_error=1
        break
        ;;
      *=*)
        pra_namespace_target=${pra_namespace_spec%%=*}
        pra_namespace=${pra_namespace_spec#*=}
        ;;
      *)
        pra_namespace_error=1
        break
        ;;
    esac
    if [ "$pra_namespace_target" = "$pra_target" ]; then
      printf '%s\n%s\n' --artifact-namespace "$pra_namespace" >>"$pra_out"
    fi
  done <"$pra_raw"
  rm -f "$pra_raw"
  if [ "$pra_package_name_error" = 1 ]; then
    printf 'run.args: --package-name is harness-owned; custom run.sh cases may pass explicit package descriptors'
    return 1
  fi
  if [ "$pra_namespace_error" = 1 ] || [ "$pra_expect_namespace" = 1 ]; then
    printf 'run.args: --artifact-namespace requires <target-id>=<namespace>'
    return 1
  fi
  return 0
}

resolve_path_command() (
  rpc_name=$1
  set -f
  IFS=:
  for rpc_dir in $PATH; do
    [ -n "$rpc_dir" ] || rpc_dir=.
    case "$rpc_dir" in
      /*) rpc_abs_dir=$rpc_dir ;;
      *)
        rpc_abs_dir=$(CDPATH='' cd -- "$rpc_dir" 2>/dev/null && pwd -P) ||
          continue
        ;;
    esac
    rpc_path=$rpc_abs_dir/$rpc_name
    if [ -f "$rpc_path" ] && [ -x "$rpc_path" ]; then
      printf '%s\n' "$rpc_path"
      exit 0
    fi
  done
  exit 1
)

setup_native_compiler_proxy() {
  sncp_dir=$1
  sncp_ci_dir=$2
  mkdir -p "$sncp_dir/real" || return 1
  printf '%s\n' "$sncp_ci_dir/schedule.sh" \
    >"$sncp_dir/admission-path" || return 1
  for sncp_tool in cargo rustc go javac swiftc ghc; do
    sncp_real=$(resolve_path_command "$sncp_tool") || continue
    ln -s "$sncp_real" "$sncp_dir/real/$sncp_tool" || return 1
    ln -s "$sncp_ci_dir/infra/native-compiler-proxy.sh" \
      "$sncp_dir/$sncp_tool" || return 1
  done
}

setup_kio_compiler_proxy() {
  skcp_dir=$1
  skcp_real_kio=$2
  skcp_real_prime=${3:-}
  skcp_proxy=$RUN_TESTS_CI_DIR/infra/kio-compiler-proxy.sh
  skcp_main_name=${skcp_real_kio##*/}

  mkdir -p "$skcp_dir/bin" "$skcp_dir/real" || return 1
  printf '%s\n' "$RUN_TESTS_CI_DIR/schedule.sh" \
    >"$skcp_dir/admission-path" || return 1
  # Git Bash may implement ln -s by copying the executable. Keep only its
  # path in metadata; only the small fixed shell launcher is copied.
  printf '%s\n' "$skcp_real_kio" \
    >"$skcp_dir/real/$skcp_main_name.path" || return 1
  cp "$skcp_proxy" "$skcp_dir/bin/$skcp_main_name" || return 1

  # Custom run.sh files may invoke bare `kio`, while KIO_BIN must retain the
  # configured compiler's basename (notably `kio-prime`) so scripts can
  # distinguish the full and Prime-only implementations.
  if [ "$skcp_main_name" != kio ]; then
    printf '%s\n' "$skcp_real_kio" >"$skcp_dir/real/kio.path" || return 1
    # On MSYS, bare kio can already resolve the kio.exe launcher.
    if [ ! -e "$skcp_dir/bin/kio" ]; then
      cp "$skcp_proxy" "$skcp_dir/bin/kio" || return 1
    fi
  fi

  if [ -z "$skcp_real_prime" ]; then
    skcp_sibling=${skcp_real_kio%/*}/kio-prime
    [ -x "$skcp_sibling" ] && skcp_real_prime=$skcp_sibling
  fi
  if [ "$skcp_main_name" != kio-prime ]; then
    if [ -n "$skcp_real_prime" ]; then
      printf '%s\n' "$skcp_real_prime" \
        >"$skcp_dir/real/kio-prime.path" || return 1
    fi
    # Reserve the companion name even when it is unavailable. Otherwise a
    # custom run.sh can fall through to an unrelated ambient kio-prime.
    if [ ! -e "$skcp_dir/bin/kio-prime" ]; then
      cp "$skcp_proxy" "$skcp_dir/bin/kio-prime" || return 1
    fi
  fi
}

# assert_case_run_contract cases_file modes_file
# Enforces the case execution contract: exactly one of run.args, run.sh,
# or run.test-only must exist. run.args selects the standard harness-owned
# test+build+runner path; run.sh is for custom scripts; run.test-only is
# a library (no main) validated by kio test + kio build with no runner.
# Records the selected filename so applicability can apply the compile-only
# SKIP rule without probing the filesystem again for every implementation.
assert_case_run_contract() {
  acrc_cases_file=$1
  acrc_modes_file=$2
  acrc_bad=0
  : >"$acrc_modes_file"
  while IFS= read -r acrc_dir; do
    [ -n "$acrc_dir" ] || continue
    acrc_modes=0
    acrc_mode=
    if [ -f "$acrc_dir/run.args" ]; then
      acrc_modes=$((acrc_modes + 1))
      acrc_mode=run.args
    fi
    if [ -f "$acrc_dir/run.sh" ]; then
      acrc_modes=$((acrc_modes + 1))
      acrc_mode=run.sh
    fi
    if [ -f "$acrc_dir/run.test-only" ]; then
      acrc_modes=$((acrc_modes + 1))
      acrc_mode=run.test-only
    fi
    if [ "$acrc_modes" -gt 1 ]; then
      printf 'error: %s: case has multiple execution files (run.args, run.sh, run.test-only); keep exactly one\n' \
        "$acrc_dir" >&2
      acrc_bad=1
    elif [ "$acrc_modes" = 0 ]; then
      printf 'error: %s: case has no execution file; add run.args (standard test+build+runner), run.sh (custom), or run.test-only (library: kio test + kio build, no runner)\n' \
        "$acrc_dir" >&2
      acrc_bad=1
    else
      printf '%s\t%s\n' "$acrc_dir" "$acrc_mode" >>"$acrc_modes_file"
    fi
    if [ -f "$acrc_dir/UNKNOWN_BUILD_TARGET" ] &&
       { [ "$acrc_mode" != run.sh ] ||
         [ "$(tr -d '[:space:]' <"$acrc_dir/expected.exit")" != 40 ]; }; then
      printf 'error: %s: UNKNOWN_BUILD_TARGET requires custom run.sh and expected.exit 40\n' \
        "$acrc_dir" >&2
      acrc_bad=1
    fi
  done <"$acrc_cases_file"
  return "$acrc_bad"
}

execute_case() {
  ec_case_dir=$1
  ec_kio=$2
  ec_runner=$3
  ec_target=$4
  ec_run_args_file=$5
  ec_run_mode=$6
  ec_scratch=$7
  ec_run_script=$8
  ec_typed_cache_root=$9

  cd "$ec_case_dir" || exit 1
  if [ "$ec_run_mode" = "standard" ]; then
    cd workdir || exit 1
    if [ ! -f ../IS_KIO_PRIME ]; then
      ec_test_stdout="$ec_scratch/kio-test.stdout"
      ec_test_stderr="$ec_scratch/kio-test.stderr"
      "$ec_kio" test >"$ec_test_stdout" 2>"$ec_test_stderr"
      ec_test_status=$?
      if [ "$ec_test_status" != 0 ]; then
        cat "$ec_test_stdout" >&2
        cat "$ec_test_stderr" >&2
        exit "$ec_test_status"
      fi
    fi
    "$ec_kio" build "$ec_target" || exit
    # Compile-only impl (runner=SKIP): stop after a successful build — never
    # read run.args or invoke a runner.
    if [ "$ec_runner" = SKIP ]; then
      exit 0
    fi
    set --
    ec_arg=
    while IFS= read -r ec_arg || [ -n "$ec_arg" ]; do
      set -- "$@" "$ec_arg"
    done <"$ec_run_args_file"
    set -- "$@" "out/$ec_target"
    if [ -f ../input.stdin ]; then
      "$ec_runner" "$@" < ../input.stdin
    else
      "$ec_runner" "$@"
    fi
  elif [ "$ec_run_mode" = "test-only" ]; then
    # A library case: discharge equiv laws (kio test, unless the package
    # is Kio' — kio-prime rejects the surface `kio test`) and validate
    # codegen (kio build), but invoke no runner. There is no `main`.
    cd workdir || exit 1
    if [ ! -f ../IS_KIO_PRIME ]; then
      ec_test_stdout="$ec_scratch/kio-test.stdout"
      ec_test_stderr="$ec_scratch/kio-test.stderr"
      "$ec_kio" test >"$ec_test_stdout" 2>"$ec_test_stderr"
      ec_test_status=$?
      if [ "$ec_test_status" != 0 ]; then
        cat "$ec_test_stdout" >&2
        cat "$ec_test_stderr" >&2
        exit "$ec_test_status"
      fi
    fi
    "$ec_kio" build "$ec_target" || exit
    exit 0
  else
    ec_kio_proxy_dir=${ec_kio%/*}
    PATH="$ec_kio_proxy_dir:$COMPILER_PROXY_DIR:$PATH" \
      KIO_DEBUG_TYPED_CACHE_ROOT="$ec_typed_cache_root" \
      KIO_BIN="$ec_kio" KIO_RUNNER="$ec_runner" KIO_TARGET="$ec_target" \
      sh "$ec_run_script"
  fi
}

# compare actual_file expected_file label impl_name case_name update_flag
# Returns 0 if matched (or updated), 1 otherwise. Prints diff/notes to stdout.
compare() {
  c_actual=$1
  c_expected=$2
  c_label=$3
  c_impl=$4
  c_name=$5
  c_update=$6

  if [ ! -f "$c_expected" ]; then
    if [ "$c_update" = 1 ]; then
      cp "$c_actual" "$c_expected"
      printf '  [%s/%s] created %s\n' "$c_impl" "$c_name" "$c_label"
      return 0
    fi
    printf '  [%s/%s] missing expected file: %s\n' "$c_impl" "$c_name" "$(basename "$c_expected")"
    return 1
  fi

  # Compare with at most one trailing newline stripped from EACH side
  # (actual and expected). A single trailing-newline difference — the
  # editor/POSIX "files end with a newline" case — is tolerated; any
  # further difference (a second trailing newline, mid-content bytes)
  # still mismatches. We read each file with a sentinel byte appended so
  # command substitution doesn't eat *all* trailing newlines, then strip
  # the sentinel and exactly one `\n`. The raw files are left untouched
  # so the `--update` path stays byte-faithful and the mismatch report
  # below shows the true diff.
  c_expected_norm=$(cat "$c_expected" 2>/dev/null; printf x); c_expected_norm=${c_expected_norm%x}
  c_actual_norm=$(cat "$c_actual" 2>/dev/null; printf x); c_actual_norm=${c_actual_norm%x}
  c_expected_norm=${c_expected_norm%"$NL"}
  c_actual_norm=${c_actual_norm%"$NL"}
  if [ "$c_expected_norm" = "$c_actual_norm" ]; then
    return 0
  fi

  if [ "$c_update" = 1 ]; then
    cp "$c_actual" "$c_expected"
    printf '  [%s/%s] updated %s\n' "$c_impl" "$c_name" "$c_label"
    return 0
  fi

  printf '  [%s/%s] %s mismatch:\n' "$c_impl" "$c_name" "$c_label"
  diff -u -L "expected.$c_label" -L "actual.$c_label" "$c_expected" "$c_actual" | sed 's/^/    /'
  return 1
}

# run_case_binary_checks case_dir name binary_label binary_kio scratch \
#                        update_flag cb_checks_file results_file
# Run every `# ROUTING: case-binary` check once for this (case, binary)
# unit. $cb_checks_file holds one `<synth-tag><TAB><path>` record per
# check; checks are grouped by their synth-tag and each tag reports
# independently. For each tag with at least one check, the function
# prints a `pass`/`FAIL [<tag>@<binary_label>] <name>` line (mirroring
# the case-run output) and appends a matching row to results_file, so
# the summary's per-tag tally stays separate from the case-run
# outcomes. A check's stdout/stderr is captured and printed only on
# failure. Returns 0 if every check passed, 1 otherwise.
#
# Tags let a distinct verification pass report on its own line; every
# current case-binary check uses the default `invariants` tag, so their
# pass/FAIL rolls into a single `invariants@<binary>` row.
run_case_binary_checks() {
  rcbc_case_dir=$1
  rcbc_name=$2
  rcbc_binary_label=$3
  rcbc_binary_kio=$4
  rcbc_scratch=$5
  rcbc_update=$6
  rcbc_cb_checks_file=$7
  rcbc_results_file=$8

  if [ ! -s "$rcbc_cb_checks_file" ]; then
    return 0
  fi

  # De-duplicated, source-order-stable set of synth tags present in
  # this unit's case-binary checks.
  rcbc_tags=$(awk -F"$TAB" '!seen[$1]++ { print $1 }' "$rcbc_cb_checks_file")

  # A check may exit 77 to declare itself inapplicable to this
  # (case, binary) unit (the autotools "skip" convention). A skipped
  # check contributes no verdict; if every check carrying a tag skips,
  # the tag emits no summary row at all. This keeps a verification
  # pass off the board where it has nothing to assert — e.g.
  # dep-canonical skips a case that ships no committed `*.dep.kio`
  # dependency tree, and a CLI / parse-error fixture with no `workdir/`
  # package skips every source-level check, so no `invariants@<binary>`
  # row appears for it.
  rcbc_skip_code=77

  rcbc_all_ok=1
  rcbc_idx=0
  printf '%s\n' "$rcbc_tags" | while IFS= read -r rcbc_tag; do
    [ -n "$rcbc_tag" ] || continue
    # Run every check carrying this tag, in source order.
    rcbc_tag_checks=$(awk -F"$TAB" -v t="$rcbc_tag" '$1==t { print $2 }' \
      "$rcbc_cb_checks_file")
    printf '%s\n' "$rcbc_tag_checks" | while IFS= read -r rcbc_check; do
      [ -n "$rcbc_check" ] || continue
      rcbc_idx=$((rcbc_idx + 1))
      rcbc_check_name=$(basename "$rcbc_check" .sh)
      rcbc_log="$rcbc_scratch/cbcheck_${rcbc_tag}_${rcbc_idx}.log"
      (
        cd "$rcbc_case_dir" || exit 1
        KIO_BIN="$rcbc_binary_kio" \
          KIO_TEST_UPDATE="$rcbc_update" \
          sh "$rcbc_check"
      ) >"$rcbc_log" 2>&1
      rcbc_status=$?
      # The pipe runs this loop body in a subshell, so per-tag state
      # is carried through sentinel files rather than variables.
      if [ "$rcbc_status" = "$rcbc_skip_code" ]; then
        continue
      fi
      : >"$rcbc_scratch/tagran_$rcbc_tag"
      if [ "$rcbc_status" != 0 ]; then
        printf '  [%s@%s/%s] %s failed (exit %d):\n' \
          "$rcbc_tag" "$rcbc_binary_label" "$rcbc_name" \
          "$rcbc_check_name" "$rcbc_status"
        sed 's/^/    /' "$rcbc_log"
        : >"$rcbc_scratch/tagfail_$rcbc_tag"
      fi
    done
    # No row when every check carrying this tag skipped.
    if [ ! -f "$rcbc_scratch/tagran_$rcbc_tag" ]; then
      continue
    fi
    if [ ! -f "$rcbc_scratch/tagfail_$rcbc_tag" ]; then
      printf 'pass [%s@%s] %s\n' "$rcbc_tag" "$rcbc_binary_label" "$rcbc_name"
      printf '%s@%s\t%s\t%s\n' "$rcbc_tag" "$rcbc_binary_label" "PASS" "$rcbc_name" \
        >>"$rcbc_results_file"
    else
      printf 'FAIL [%s@%s] %s\n' "$rcbc_tag" "$rcbc_binary_label" "$rcbc_name"
      printf '%s@%s\t%s\t%s\n' "$rcbc_tag" "$rcbc_binary_label" "FAIL" "$rcbc_name" \
        >>"$rcbc_results_file"
    fi
  done

  # The per-tag loop runs in a pipe subshell; recover the unit-level
  # verdict from the sentinel files it may have left behind.
  if ls "$rcbc_scratch"/tagfail_* >/dev/null 2>&1; then
    rcbc_all_ok=0
  fi
  return $((1 - rcbc_all_ok))
}

# run_one_case impl_name impl_kio impl_runner impl_target impl_prime_kio impl_runner_cache_kind case_dir name
#              scratch update_flag show_output_flag results_file checks_file package_name clear_binary
# Runs one case, diffs against goldens, prints the pass/FAIL line (and
# any inline diff/notes) to stdout, and appends a record to results_file.
# Caller redirects stdout/stderr if buffering is desired.
#
# checks_file: path to a newline-separated list of executable scripts
# whose routing is `impl`. Each one runs once for this (case, impl)
# before the case run, with cwd = the case directory. KIO_BIN, KIO_RUNNER,
# KIO_TEST_RUNNER_CACHE_KIND, KIO_TARGET, KIO_PRIME_BIN, KIO_TEST_RUN_ARGS_FILE,
# KIO_TEST_RUNNER_BUILD_CACHE_DIR,
# KIO_TEST_RUNNER_BUILD_CACHE_SIZE,
# KIO_TEST_RUNNER_COMPILER_WRAPPER,
# KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER,
# KIO_TEST_RUNNER_CACHE_DISABLE, KIO_TEST_UPDATE are exported.
# KIO_TEST_RUN_ARGS_FILE names the harness-prepared, target-local argument
# stream: package-namespace mappings in raw run.args have already been
# selected, and checks must consume this file rather than reparsing run.args.
# Failures are reported alongside case-run failures.
run_one_case() {
  rc_impl_name=$1
  rc_impl_kio=$2
  rc_impl_runner=$3
  rc_impl_target=$4
  rc_impl_prime_kio=$5
  rc_runner_cache_kind=$6
  rc_case_dir=$7
  rc_name=$8
  rc_scratch=$9
  rc_update=${10}
  rc_show_output=${11}
  rc_results_file=${12}
  rc_checks_file=${13:-}
  rc_package_name=${14:-}
  rc_clear_binary=${15}
  [ "$rc_package_name" != "*" ] || rc_package_name=

  # Preserve the debug observer's set-vs-unset state for checks, custom
  # scripts, and standard runner launches. An explicitly empty value must
  # reach the runner so its nonempty-executable validation can reject it.
  if [ -n "${KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER+x}" ]; then
    export KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER
  else
    unset KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER
  fi

  rc_stdout="$rc_scratch/stdout"
  rc_stderr="$rc_scratch/stderr"
  rc_status="$rc_scratch/status"
  rc_kio_proxy_dir="$rc_scratch/kio-compiler-proxy"
  if ! setup_kio_compiler_proxy \
    "$rc_kio_proxy_dir" "$rc_impl_kio" "$rc_impl_prime_kio"; then
    printf '  [%s/%s] cannot prepare Kio compiler admission proxy\n' \
      "$rc_impl_name" "$rc_name"
    printf 'FAIL [%s] %s\n' "$rc_impl_name" "$rc_name"
    printf '%s\t%s\t%s\n' "$rc_impl_name" "FAIL" "$rc_name" >>"$rc_results_file"
    return 0
  fi
  rc_proxy_kio="$rc_kio_proxy_dir/bin/${rc_impl_kio##*/}"
  rc_proxy_prime_kio=
  if [ -n "$rc_impl_prime_kio" ]; then
    rc_proxy_prime_kio="$rc_kio_proxy_dir/bin/kio-prime"
  fi

  # Per-impl runner-build-cache directory: $CACHE_BASE/<impl-target>/.
  # The base is supplied by --cache-base on the parent harness
  # command line; carving by impl-target keeps wasm / native / etc.
  # siblings isolated once they exist. The directory is created
  # lazily on first cache write by the runner. The cache is content-
  # addressed and benefits from cross-case sharing — distinct from
  # the per-(case, binary) Kio-semantic cache that the outer loop
  # clears.
  rc_runner_build_cache_dir="$CACHE_BASE/$rc_impl_target"

  rc_ok=1

  rc_run_mode=custom
  rc_run_args_file=
  rc_run_script=./run.sh
  rc_typed_cache_root=$rc_scratch/typed-cache
  rc_trusted_oid=
  rc_trusted_master_script=
  if [ -f "$rc_case_dir/run.args" ]; then
    rc_run_mode=standard
    rc_run_args_file=$(CDPATH='' cd -- "$rc_scratch" && pwd)/run.args
    rc_run_args_error=$(prepare_run_args "$rc_case_dir" "$rc_run_args_file" "$rc_impl_target")
    rc_run_args_status=$?
    if [ "$rc_run_args_status" != 0 ]; then
      printf '  [%s/%s] %s\n' "$rc_impl_name" "$rc_name" "$rc_run_args_error"
      printf 'FAIL [%s] %s\n' "$rc_impl_name" "$rc_name"
      printf '%s\t%s\t%s\n' "$rc_impl_name" "FAIL" "$rc_name" >>"$rc_results_file"
      return 0
    fi
  elif [ -f "$rc_case_dir/run.test-only" ]; then
    # A library (no `main`): kio test + kio build, no runner. Like a
    # compile-only impl, but for every impl and with the source-level
    # checks still applied.
    rc_run_mode=test-only
  fi

  # Custom scripts remain isolated unless the harness caller authenticated
  # this exact relative path and content before dispatch. Execute the frozen
  # snapshot, not the mutable source-tree or copied file. The execution cwd
  # retains the case's contents and relative layout.
  if [ "$rc_run_mode" = custom ] &&
     [ -n "${CUSTOM_TYPED_CACHE_COHORT_FILE:-}" ]; then
    rc_trusted_script=$rc_name/run.sh
    rc_trusted_oid=$(awk -F"$TAB" -v p="$rc_trusted_script" \
      '$1 == p { print $2; exit }' "$CUSTOM_TYPED_CACHE_COHORT_FILE")
    if [ -n "$rc_trusted_oid" ]; then
      rc_trusted_master_script=$CUSTOM_TYPED_CACHE_SCRIPTS_ROOT/$rc_trusted_script
    fi
  fi

  # The runner receives package identity from the harness, independently of
  # emitted backend source. Pre-dispatch indexing has already proved that
  # every standard case has one safe top-level package. A custom case receives
  # the same default only when it has that exact shape; nested and
  # multi-package scripts pass repeated --package-name / configured namespace
  # arguments themselves.
  rc_effective_runner=$rc_impl_runner
  if [ "$rc_impl_runner" != SKIP ]; then
    rc_effective_runner=$rc_scratch/kio-runner
    cp "$TEST_RUNNER_IDENTITY_PROXY" "$rc_effective_runner"
    printf '%s\n%s\n' "$rc_impl_runner" "$rc_package_name" >"$rc_effective_runner.config"
  fi

  # A compile-only impl (runner=SKIP) runs kio test + kio build but never a
  # runner; the worklist excludes custom (run.sh) cases from it, so every
  # case reaching here in SKIP mode uses run.args or run.test-only. Impl checks
  # remain applicable unless they explicitly declare `# REQUIRES: runner`.
  # A run.test-only case is compile-only on every impl (rc_test_only).
  rc_skip_runner=0
  [ "$rc_impl_runner" = SKIP ] && rc_skip_runner=1
  rc_test_only=0
  [ "$rc_run_mode" = test-only ] && rc_test_only=1

  # A KNOWN_FAILING case pins a tracked bug: it is *expected* to fail
  # (build, run, or output). The per-case checks (fmt / prime-marker /
  # round-trip / cache) can't pass on a case that doesn't build, so they
  # are skipped; the case's own verdict is inverted below (an expected
  # failure warns and stays green; an unexpected pass fails, flagging a
  # stale marker). See CONTRIBUTING.md § Bug reproducers.
  rc_known_failing=0
  [ -f "$rc_case_dir/KNOWN_FAILING" ] && rc_known_failing=1

  # Per-(case, impl) checks use the source case directory. Checks (e.g.
  # fmt-canonical, prime-marker) carry a `# ROUTING: case-binary`
  # marker and run earlier in the unit; the checks reaching this
  # codepath are the impl-routed ones (kio-prime-roundtrip,
  # rlib-cache-second-run-hits), which manage their own scratch copies
  # for any mutations they need to make.
  # KIO_TEST_UPDATE=1 propagates the -u/--update-expected mode so
  # checks that can fix the corpus (writing markers, normalizing
  # source) update in place.
  if [ "$rc_known_failing" != 1 ] && [ -n "$rc_checks_file" ] && [ -s "$rc_checks_file" ]; then
    rc_check_idx=0
    while IFS= read -r rc_check; do
      [ -n "$rc_check" ] || continue
      if [ "$rc_skip_runner" = 1 ] && grep -q '^# REQUIRES: runner$' "$rc_check"; then
        continue
      fi
      rc_check_idx=$((rc_check_idx + 1))
      rc_check_name=$(basename "$rc_check" .sh)
      rc_check_log="$rc_scratch/check_${rc_check_idx}.log"
      (
        cd "$rc_case_dir" || exit 1
        PATH="$rc_kio_proxy_dir/bin:$COMPILER_PROXY_DIR:$PATH" \
          KIO_BIN="$rc_proxy_kio" KIO_RUNNER="$rc_effective_runner" \
          KIO_TEST_RUNNER_CACHE_KIND="$rc_runner_cache_kind" \
          KIO_TEST_RUN_ARGS_FILE="$rc_run_args_file" \
          KIO_PRIME_BIN="$rc_proxy_prime_kio" \
          KIO_TARGET="$rc_impl_target" \
          KIO_TEST_RUNNER_BUILD_CACHE_DIR="$rc_runner_build_cache_dir" \
          KIO_TEST_RUNNER_BUILD_CACHE_SIZE="${KIO_TEST_RUNNER_BUILD_CACHE_SIZE:-}" \
          KIO_TEST_RUNNER_COMPILER_WRAPPER="${KIO_TEST_RUNNER_COMPILER_WRAPPER:-}" \
          KIO_TEST_RUNNER_CACHE_DISABLE="${KIO_TEST_RUNNER_CACHE_DISABLE:-}" \
          KIO_TEST_RUNNER_PROFILE="${KIO_TEST_RUNNER_PROFILE:-}" \
          KIO_TEST_UPDATE="$rc_update" \
          sh "$rc_check"
      ) >"$rc_check_log" 2>&1
      rc_check_status=$?
      if [ "$rc_check_status" != 0 ]; then
        rc_ok=0
        printf '  [%s/%s] check %s failed (exit %d):\n' \
          "$rc_impl_name" "$rc_name" "$rc_check_name" "$rc_check_status"
        sed 's/^/    /' "$rc_check_log"
      fi
    done <"$rc_checks_file"
  fi

  # Stderr-assertion mode selection. Exactly one of the three
  # `expected.stderr*` files must be present per case:
  #   * expected.stderr        — byte-equal diff; rare, for whole-stream contracts.
  #   * expected.stderr.ignore — skip the stderr assertion entirely.
  #   * expected.stderr.grep   — POSIX ERE matcher; every non-empty,
  #                              non-comment line must match somewhere
  #                              in actual stderr.
  # If zero or more than one is present the case is misconfigured and we
  # fail it with a clear message instead of silently letting one
  # shadow a stale sibling.
  rc_have_stderr=0
  rc_have_ignore=0
  rc_have_grep=0
  [ -f "$rc_case_dir/expected.stderr" ]        && rc_have_stderr=1
  [ -f "$rc_case_dir/expected.stderr.ignore" ] && rc_have_ignore=1
  [ -f "$rc_case_dir/expected.stderr.grep" ]   && rc_have_grep=1
  rc_stderr_modes=$((rc_have_stderr + rc_have_ignore + rc_have_grep))
  if [ "$rc_stderr_modes" -ne 1 ]; then
    rc_present=
    [ "$rc_have_stderr" = 1 ] && rc_present="${rc_present:+$rc_present, }expected.stderr"
    [ "$rc_have_ignore" = 1 ] && rc_present="${rc_present:+$rc_present, }expected.stderr.ignore"
    [ "$rc_have_grep"   = 1 ] && rc_present="${rc_present:+$rc_present, }expected.stderr.grep"
    if [ -n "$rc_present" ]; then
      printf '  [%s/%s] case has multiple stderr-assertion files (%s); keep exactly one\n' \
        "$rc_impl_name" "$rc_name" "$rc_present"
    else
      printf '  [%s/%s] case has no stderr-assertion file; add expected.stderr.ignore, expected.stderr.grep, or rare exact expected.stderr\n' \
        "$rc_impl_name" "$rc_name"
    fi
    printf 'FAIL [%s] %s\n' "$rc_impl_name" "$rc_name"
    printf '%s\t%s\t%s\n' "$rc_impl_name" "FAIL" "$rc_name" >>"$rc_results_file"
    return 0
  fi
  # Pre-validate .grep: non-empty regex set required. We do this
  # before the case run so a malformed marker is caught even when the
  # actual stderr is empty.
  if [ "$rc_have_grep" = 1 ]; then
    # Count non-blank, non-comment lines. `grep -v` selects lines
    # matching neither the blank pattern nor the comment pattern;
    # `grep -c` counts the matches. The `|| true` keeps `set -u` happy
    # if the file has zero regex lines (grep exits 1).
    rc_grep_count=$(grep -Ecv -e '^[[:space:]]*$' -e '^[[:space:]]*#' \
      "$rc_case_dir/expected.stderr.grep" 2>/dev/null | tr -d ' ')
    if [ "${rc_grep_count:-0}" = 0 ]; then
      printf '  [%s/%s] expected.stderr.grep contains no regex lines (blank/comment-only); add at least one\n' \
        "$rc_impl_name" "$rc_name"
      printf 'FAIL [%s] %s\n' "$rc_impl_name" "$rc_name"
      printf '%s\t%s\t%s\n' "$rc_impl_name" "FAIL" "$rc_name" >>"$rc_results_file"
      return 0
    fi
  fi

  rc_execution_dir=$rc_case_dir
  if [ "$rc_run_mode" = custom ] && [ -e "$rc_case_dir/SKIP_DEP_MATERIALIZED" ]; then
    # Copy all inputs, including untracked/ignored files, after source checks.
    # Preserve links and sibling directories rather than flattening dependency
    # topology. This is fixture isolation, not a sandbox for external paths.
    rc_execution_dir=$rc_scratch/case
    if ! mkdir -p "$rc_execution_dir" ||
       ! cp -RP "$rc_case_dir/." "$rc_execution_dir"; then
      printf '  [%s/%s] cannot copy custom case into execution scratch\n' \
        "$rc_impl_name" "$rc_name"
      printf 'FAIL [%s] %s\n' "$rc_impl_name" "$rc_name"
      printf '%s\t%s\t%s\n' "$rc_impl_name" "FAIL" "$rc_name" >>"$rc_results_file"
      return 0
    fi
    if [ "$KEEP_CACHE" != 1 ]; then
      (
        cd "$rc_execution_dir" || exit 1
        if [ -d workdir ]; then cd workdir || exit 1; fi
        "$rc_clear_binary" cache clear
      ) >"$rc_scratch/cache-clear.log" 2>&1 || :
    fi
  fi

  # Refresh the authenticated master into this impl's scratch immediately
  # before launch, then verify its exact bytes at the final harness-controlled
  # step before `sh` opens it. This also keeps later source-tree edits from
  # changing the snapshotted cohort selected before dispatch.
  if [ -n "$rc_trusted_oid" ]; then
    rc_run_script=$rc_scratch/authenticated-run.sh
    rc_snapshot_ok=1
    if [ ! -f "$rc_trusted_master_script" ] ||
       [ -L "$rc_trusted_master_script" ]; then
      rc_snapshot_ok=0
    elif ! cp "$rc_trusted_master_script" "$rc_run_script"; then
      rc_snapshot_ok=0
    elif ! chmod 0444 "$rc_run_script"; then
      rc_snapshot_ok=0
    elif ! rc_snapshot_oid=$(git hash-object --no-filters \
      "$rc_run_script" 2>/dev/null); then
      rc_snapshot_ok=0
    elif [ "$rc_snapshot_oid" != "$rc_trusted_oid" ]; then
      rc_snapshot_ok=0
    fi
    if [ "$rc_snapshot_ok" != 1 ]; then
      printf '  [%s/%s] authenticated custom run.sh snapshot changed before execution\n' \
        "$rc_impl_name" "$rc_name"
      printf 'FAIL [%s] %s\n' "$rc_impl_name" "$rc_name"
      printf '%s\t%s\t%s\n' "$rc_impl_name" "FAIL" "$rc_name" >>"$rc_results_file"
      return 0
    fi
    rc_typed_cache_root=$CUSTOM_TYPED_CACHE_ROOT
  fi

  if [ "$rc_show_output" = 1 ]; then
    # Stream stdout/stderr to the terminal AND capture to files for
    # diffing. POSIX-clean approach: a FIFO per stream with a backgrounded
    # `tee` reading from it. The case's redirect blocks until the readers
    # are open; we wait on the tees afterward to flush before diffing.
    printf 'running [%s] %s\n' "$rc_impl_name" "$rc_name"
    rc_stdout_pipe="$rc_scratch/stdout.pipe"
    rc_stderr_pipe="$rc_scratch/stderr.pipe"
    rm -f "$rc_stdout_pipe" "$rc_stderr_pipe"
    mkfifo "$rc_stdout_pipe" "$rc_stderr_pipe"
    tee "$rc_stdout" <"$rc_stdout_pipe" &
    rc_tee_out_pid=$!
    tee "$rc_stderr" <"$rc_stderr_pipe" >&2 &
    rc_tee_err_pid=$!
    (
      # shellcheck disable=SC2030,SC2031 # exported only for this isolated case process tree
      export KIO_TEST_RUNNER_BUILD_CACHE_DIR="$rc_runner_build_cache_dir"
      # shellcheck disable=SC2030,SC2031 # exported only for this isolated case process tree
      export KIO_TEST_RUNNER_BUILD_CACHE_SIZE="${KIO_TEST_RUNNER_BUILD_CACHE_SIZE:-}"
      # shellcheck disable=SC2030,SC2031 # exported only for this isolated case process tree
      export KIO_TEST_RUNNER_COMPILER_WRAPPER="${KIO_TEST_RUNNER_COMPILER_WRAPPER:-}"
      # shellcheck disable=SC2030,SC2031 # exported only for this isolated case process tree
      export KIO_TEST_RUNNER_CACHE_DISABLE="${KIO_TEST_RUNNER_CACHE_DISABLE:-}"
      # shellcheck disable=SC2030,SC2031 # exported only for this isolated case process tree
      export KIO_TEST_RUNNER_PROFILE="${KIO_TEST_RUNNER_PROFILE:-}"
      # shellcheck disable=SC2030,SC2031 # exported only for this isolated case process tree
      export KIO_TEST_RUNNER_CACHE_KIND="$rc_runner_cache_kind"
      execute_case \
        "$rc_execution_dir" "$rc_proxy_kio" "$rc_effective_runner" "$rc_impl_target" \
        "$rc_run_args_file" "$rc_run_mode" "$rc_scratch" \
        "$rc_run_script" "$rc_typed_cache_root"
    ) \
      >"$rc_stdout_pipe" 2>"$rc_stderr_pipe"
    rc_actual_status=$?
    wait "$rc_tee_out_pid" "$rc_tee_err_pid"
  else
    (
      # shellcheck disable=SC2030,SC2031 # exported only for this isolated case process tree
      export KIO_TEST_RUNNER_BUILD_CACHE_DIR="$rc_runner_build_cache_dir"
      # shellcheck disable=SC2030,SC2031 # exported only for this isolated case process tree
      export KIO_TEST_RUNNER_BUILD_CACHE_SIZE="${KIO_TEST_RUNNER_BUILD_CACHE_SIZE:-}"
      # shellcheck disable=SC2030,SC2031 # exported only for this isolated case process tree
      export KIO_TEST_RUNNER_COMPILER_WRAPPER="${KIO_TEST_RUNNER_COMPILER_WRAPPER:-}"
      # shellcheck disable=SC2030,SC2031 # exported only for this isolated case process tree
      export KIO_TEST_RUNNER_CACHE_DISABLE="${KIO_TEST_RUNNER_CACHE_DISABLE:-}"
      # shellcheck disable=SC2030,SC2031 # exported only for this isolated case process tree
      export KIO_TEST_RUNNER_PROFILE="${KIO_TEST_RUNNER_PROFILE:-}"
      # shellcheck disable=SC2030,SC2031 # exported only for this isolated case process tree
      export KIO_TEST_RUNNER_CACHE_KIND="$rc_runner_cache_kind"
      execute_case \
        "$rc_execution_dir" "$rc_proxy_kio" "$rc_effective_runner" "$rc_impl_target" \
        "$rc_run_args_file" "$rc_run_mode" "$rc_scratch" \
        "$rc_run_script" "$rc_typed_cache_root"
    ) \
      >"$rc_stdout" 2>"$rc_stderr"
    rc_actual_status=$?
  fi
  printf '%s\n' "$rc_actual_status" >"$rc_status"

  # rc_ok is initialized to 1 at the start; per-check failures may have
  # already cleared it. Score the case run below, reporting all failures
  # together rather than short-circuiting.
  if [ "$rc_skip_runner" = 1 ] || [ "$rc_test_only" = 1 ]; then
    # No runner was invoked — either a compile-only impl (runner=SKIP)
    # or a run.test-only library case. Pass iff kio test + kio build
    # succeeded; the (empty) captured output is not diffed against the
    # case's expected.* program output.
    if [ "$rc_actual_status" != 0 ]; then
      rc_ok=0
      printf '  [%s/%s] compile-only (kio test + kio build) failed (exit %d):\n' \
        "$rc_impl_name" "$rc_name" "$rc_actual_status"
      sed 's/^/    /' "$rc_stderr"
    fi
  else
    compare "$rc_stdout" "$rc_case_dir/expected.stdout" stdout "$rc_impl_name" "$rc_name" "$rc_update" || rc_ok=0
    if [ "$rc_have_ignore" = 1 ]; then
      : # stderr assertion skipped on user request.
    elif [ "$rc_have_grep" = 1 ]; then
      # For each non-blank, non-comment regex line, the actual stderr
      # must contain at least one match. We print every failing regex
      # (not just the first) so a single iteration surfaces all the
      # assertions that need attention, then dump the captured stderr
      # once at the end so the user can see what the actual output was.
      rc_grep_failed=0
      while IFS= read -r rc_grep_pat; do
        case "$rc_grep_pat" in
          ''|'#'*) continue ;;
        esac
        # Strip leading whitespace before the blank/comment check so a
        # leading-indented `#` line is treated as a comment.
        rc_grep_stripped=$(printf '%s' "$rc_grep_pat" | sed -e 's/^[[:space:]]*//')
        case "$rc_grep_stripped" in
          ''|'#'*) continue ;;
        esac
        if ! grep -E -q -- "$rc_grep_pat" "$rc_stderr"; then
          if [ "$rc_grep_failed" = 0 ]; then
            printf '  [%s/%s] stderr.grep mismatch:\n' "$rc_impl_name" "$rc_name"
          fi
          printf '    unmatched regex: %s\n' "$rc_grep_pat"
          rc_grep_failed=1
        fi
      done <"$rc_case_dir/expected.stderr.grep"
      if [ "$rc_grep_failed" = 1 ]; then
        printf '    actual stderr:\n'
        sed 's/^/      /' "$rc_stderr"
        rc_ok=0
      fi
    else
      compare "$rc_stderr" "$rc_case_dir/expected.stderr" stderr "$rc_impl_name" "$rc_name" "$rc_update" || rc_ok=0
    fi
    compare "$rc_status" "$rc_case_dir/expected.exit" exit "$rc_impl_name" "$rc_name" "$rc_update" || rc_ok=0
    # A stderr.ignore case that fails on stdout/exit shows only the diff;
    # its program stderr (often the real cause — a panic, an uncaught
    # error) is captured but unshown. Surface it on failure so the case
    # self-diagnoses without a re-run. The .grep / exact stderr modes
    # already display stderr on a mismatch, and a compile-only SKIP failure
    # dumps its own above.
    if [ "$rc_ok" != 1 ] && [ "$rc_have_ignore" = 1 ] && [ -s "$rc_stderr" ]; then
      printf '  [%s/%s] captured stderr:\n' "$rc_impl_name" "$rc_name"
      sed 's/^/    /' "$rc_stderr"
    fi
  fi

  if [ "$rc_known_failing" = 1 ]; then
    if [ "$rc_ok" = 1 ]; then
      # The tracked bug no longer reproduces: the fix landed but the
      # KNOWN_FAILING marker wasn't removed. Fail so the stale marker
      # gets cleaned up (and the case becomes a normal passing case).
      printf 'FAIL [%s] %s (KNOWN_FAILING but now passes — remove the marker)\n' \
        "$rc_impl_name" "$rc_name"
      printf '%s\t%s\t%s\n' "$rc_impl_name" "FAIL" "$rc_name" >>"$rc_results_file"
    else
      # Expected failure: the tracked bug still reproduces. Warn so it
      # stays visible in the run, but don't fail the gate.
      printf 'warn [%s] %s (KNOWN_FAILING: tracked bug still reproduces)\n' \
        "$rc_impl_name" "$rc_name"
      printf '%s\t%s\t%s\n' "$rc_impl_name" "WARN" "$rc_name" >>"$rc_results_file"
    fi
  elif [ "$rc_ok" = 1 ]; then
    printf 'pass [%s] %s\n' "$rc_impl_name" "$rc_name"
    printf '%s\t%s\t%s\n' "$rc_impl_name" "PASS" "$rc_name" >>"$rc_results_file"
  else
    printf 'FAIL [%s] %s\n' "$rc_impl_name" "$rc_name"
    printf '%s\t%s\t%s\n' "$rc_impl_name" "FAIL" "$rc_name" >>"$rc_results_file"
  fi
}

# run_one_unit unit_key — execute one (case, binary) worklist row.
#
# Reads the row from $UNITS_FILE (a tab-separated table keyed on
# unit_key). Each row carries: unit_key, case_dir, binary_kio, one or more impl
# tuples `name|runner|target|prime-kio` joined by `;`, then the pre-indexed
# package name (`*` when no default runner identity applies).
# The tuple encoding avoids tabs (xargs payload constraint) and colons
# (some impl names use `@`); `|` and `;` are reserved by the script and
# rejected at impl-parse time if present in any field.
#
# Sequence for the unit:
#   1. Run `<binary> cache clear` with cwd = case_dir/workdir if the case
#      has a workdir/ dir (package files live there), else cwd =
#      case_dir. Cases without a package file (or whose package
#      file has no build block) are no-ops for the cache (the `cache`
#      directive lives in the build block), so a failed clear there
#      is treated as harmless and logged silently. This clears only the
#      package-local roots; harness-owned paths retain the run-scoped
#      typed-module root exported by the parent harness. Custom scripts use
#      per-case scratch unless an authenticated reviewed cohort receives its
#      own separate run-scoped root.
#   1b. For a `run.args` case (the harness-owned path) that declares any
#       `*.dep.kio`, regenerate its dependencies (`<binary> dep fetch`)
#       before the run. The re-rooted tree is committed (the source of
#       truth) and already on disk, so build/check/test consume it without
#       materializing; fetching here re-derives it from current source so
#       the run exercises the latest materialization. `dep fetch` is
#       idempotent — it rewrites the same committed bytes when nothing
#       changed — and the dep-canonical per-case check independently
#       asserts committed == fetch (no drift). A `run.sh` case owns its own
#       `dep fetch` (the committed-tree cases run one too) and is skipped
#       here. The fetch is fatal: a non-zero exit FAILs the unit (every
#       committed-`.dep.kio` `run.args` case has a well-formed dependency,
#       so a failure is real).
#   2. Run every `# ROUTING: case-binary` check once.
#   3. Run each impl in the unit's impl list sequentially via
#      run_one_case.
run_one_unit() {
  rou_key=$1

  # Locate the row.
  rou_row=$(awk -F"$TAB" -v k="$rou_key" '$1==k {print; exit}' "$UNITS_FILE")
  if [ -z "$rou_row" ]; then
    printf 'error: worker: no row in worklist for key=%s\n' "$rou_key" >&2
    return 1
  fi
  rou_case_dir=$(printf '%s' "$rou_row" | awk -F"$TAB" '{print $2}')
  rou_binary_kio=$(printf '%s' "$rou_row" | awk -F"$TAB" '{print $3}')
  rou_impls_blob=$(printf '%s' "$rou_row" | awk -F"$TAB" '{print $4}')
  rou_package_name=$(printf '%s' "$rou_row" | awk -F"$TAB" '{print $5}')

  rou_name=${rou_case_dir#"$CASES_DIR"/}

  rou_safe=$(printf '%s' "$rou_key" | tr / _)
  rou_scratch="$UNIT_SCRATCH_BASE/$rou_safe"
  mkdir -p "$rou_scratch"
  rou_kio_proxy_dir="$rou_scratch/kio-compiler-proxy"
  if ! setup_kio_compiler_proxy \
    "$rou_kio_proxy_dir" "$rou_binary_kio" ""; then
    printf 'error: cannot prepare Kio compiler admission proxy for %s\n' \
      "$rou_name" >&2
    return 1
  fi
  rou_proxy_kio="$rou_kio_proxy_dir/bin/${rou_binary_kio##*/}"

  # Step 1: cache clear. If the binary doesn't support `cache clear`
  # (e.g., a kio-prime build without the cache subcommand), use the
  # fallback binary registered for it at startup. Both binaries
  # resolve the same `cache ...` directive in the case's build
  # file, so either correctly wipes the unit's cache.
  rou_clear_binary=$(awk -F"$TAB" -v b="$rou_binary_kio" '$1==b {print $2; exit}' \
    "$CLEAR_BINARY_FILE")
  if [ -z "$rou_clear_binary" ]; then
    rou_clear_binary=$rou_binary_kio
  fi
  rou_clear_log="$rou_scratch/cache-clear.log"
  if [ -d "$rou_case_dir/workdir" ]; then
    rou_clear_cwd="$rou_case_dir/workdir"
  else
    rou_clear_cwd="$rou_case_dir"
  fi
  # --keep-cache skips the per-unit clear so a large compile-only sweep
  # reuses warm caches across units; cache correctness is covered by the
  # exec_rlib_cache_* goldens, so skipping the clear here is safe.
  rou_clear_status=0
  # Marked custom cases clear each private execution copy after source checks.
  if [ "$KEEP_CACHE" != 1 ] &&
     { [ ! -f "$rou_case_dir/run.sh" ] || [ ! -e "$rou_case_dir/SKIP_DEP_MATERIALIZED" ]; }; then
    (
      cd "$rou_clear_cwd" || exit 1
      "$rou_clear_binary" cache clear
    ) >"$rou_clear_log" 2>&1
    rou_clear_status=$?
  fi
  if [ "$rou_clear_status" != 0 ]; then
    # `cache clear` is best-effort: a case without a package file
    # (or without a `cache ...` directive in its build block) has
    # nothing to clear, and the binary may report that as a non-zero
    # exit. We don't treat that as a unit-level failure here. If the
    # cache subcommand is genuinely broken, the case run will
    # fail later and the failure will surface there with a
    # readable error.
    :
  fi

  # Step 1.5: regenerate this package's dependencies before the run. The
  # re-rooted tree is committed (the source of truth), so build / check /
  # test consume it as on-disk source without materializing. Fetching here
  # once per unit — before the case-binary checks (`kio test` /
  # fmt-canonical / dep-canonical) and the impl runs that all consume the
  # tree — re-derives it from current source so the run exercises the
  # latest materialization; the dep-canonical check then asserts the
  # committed tree equals that fetch output (no drift). `dep fetch` honors
  # any lock and re-roots from the on-disk source; it is idempotent.
  #
  # This pre-fetch is part of the `run.args` contract: it serves the
  # harness-owned test+build+runner path (execute_case standard mode) and
  # the case-binary checks that path's cases share. A `run.sh` case opts
  # out of the harness entirely and runs its own `kio dep fetch` in-script
  # (its committed-tree cases regenerate before the run the same way; a few
  # git-dependency cases assemble and fetch a dependency at run time), so
  # it is not pre-fetched here. Gating on `run.args` (rather than on a
  # committed `.dep.kio`) is also what lets this fetch be fatal: every
  # committed-`.dep.kio` `run.args` case has a well-formed dependency that
  # materializes cleanly, so a non-zero `dep fetch` is a real failure to
  # surface, not a tolerated error-fixture quirk.
  set -- "$rou_case_dir"/run.args
  if [ -e "$1" ]; then
    set -- "$rou_clear_cwd"/*.dep.kio
    if [ -e "$1" ]; then
      rou_dep_log="$rou_scratch/dep-fetch.log"
      (
        cd "$rou_clear_cwd" || exit 1
        "$rou_binary_kio" dep fetch
      ) >"$rou_dep_log" 2>&1
      rou_dep_status=$?
      if [ "$rou_dep_status" != 0 ]; then
        printf '  dep fetch failed for %s (exit %d):\n' "$rou_name" "$rou_dep_status"
        sed 's/^/    /' "$rou_dep_log"
        if [ -n "$rou_impls_blob" ]; then
          printf '%s\n' "$rou_impls_blob" | tr ';' '\n' | while IFS= read -r rou_df_tuple; do
            [ -n "$rou_df_tuple" ] || continue
            rou_df_iname=$(printf '%s' "$rou_df_tuple" | awk -F'|' '{print $1}')
            printf 'FAIL [%s] %s\n' "$rou_df_iname" "$rou_name"
            printf '%s\t%s\t%s\n' "$rou_df_iname" "FAIL" "$rou_name" >>"$RESULTS_FILE"
          done
        else
          # A case-narrowed unit (sampled or outside a named set) runs no
          # impls, so there is no impl row to fail. The verdict still has to
          # land somewhere or the failure would be printed and then counted
          # as a pass: attribute it to the case-binary tags this unit was
          # about to run (the same `<tag>@<binary>` rows
          # run_case_binary_checks would have written), falling back to the
          # default tag when no check is configured.
          rou_df_binary_label=$(basename "$rou_binary_kio")
          rou_df_tags=$(awk -F"$TAB" '!seen[$1]++ { print $1 }' "$CASE_BINARY_CHECKS_FILE" 2>/dev/null)
          [ -n "$rou_df_tags" ] || rou_df_tags=invariants
          printf '%s\n' "$rou_df_tags" | while IFS= read -r rou_df_tag; do
            [ -n "$rou_df_tag" ] || continue
            printf 'FAIL [%s@%s] %s\n' "$rou_df_tag" "$rou_df_binary_label" "$rou_name"
            printf '%s@%s\t%s\t%s\n' "$rou_df_tag" "$rou_df_binary_label" "FAIL" "$rou_name" \
              >>"$RESULTS_FILE"
          done
        fi
        return 1
      fi
    fi
  fi

  # Step 2: case-binary-routed checks. Each check's pass/FAIL
  # attributes to a synthesized `<synth-tag>@<binary>` impl tag (the
  # default `invariants`) so the summary's per-tag tallies stay distinct
  # from case-run outcomes.
  # run_case_binary_checks prints those lines and records the rows
  # itself, one verdict per tag present in the unit's checks.
  # A KNOWN_FAILING case pins a tracked bug and is expected not to build
  # cleanly, so the case-binary checks (fmt / prime-marker / dep-canonical)
  # would fail on it — skip them; run_one_case inverts the case verdict.
  if [ ! -f "$rou_case_dir/KNOWN_FAILING" ]; then
    rou_binary_label=$(basename "$rou_binary_kio")
    run_case_binary_checks \
      "$rou_case_dir" "$rou_name" "$rou_binary_label" "$rou_proxy_kio" \
      "$rou_scratch" "$UPDATE_FLAG" \
      "$CASE_BINARY_CHECKS_FILE" "$RESULTS_FILE" || :
  fi

  # Step 3: sequential impls on this (case, binary) unit. Each impl
  # tuple is encoded as `name|runner|target|prime-kio|runner-cache-kind` and joined with ';' in
  # the worklist row's 4th column.
  rou_impl_idx=0
  printf '%s\n' "$rou_impls_blob" | tr ';' '\n' | while IFS= read -r rou_tuple; do
    [ -n "$rou_tuple" ] || continue
    rou_impl_idx=$((rou_impl_idx + 1))
    rou_iname=$(printf '%s' "$rou_tuple" | awk -F'|' '{print $1}')
    rou_irunner=$(printf '%s' "$rou_tuple" | awk -F'|' '{print $2}')
    rou_itarget=$(printf '%s' "$rou_tuple" | awk -F'|' '{print $3}')
    rou_iprime=$(printf '%s' "$rou_tuple" | awk -F'|' '{print $4}')
    rou_icache_kind=$(printf '%s' "$rou_tuple" | awk -F'|' '{print $5}')
    rou_impl_scratch="$rou_scratch/impl_$rou_impl_idx"
    mkdir -p "$rou_impl_scratch"
    run_one_case \
      "$rou_iname" "$rou_binary_kio" "$rou_irunner" "$rou_itarget" "$rou_iprime" "$rou_icache_kind" \
      "$rou_case_dir" "$rou_name" "$rou_impl_scratch" \
      "$UPDATE_FLAG" "$SHOW_OUTPUT_FLAG" \
      "$RESULTS_FILE" "$IMPL_CHECKS_FILE" "$rou_package_name" "$rou_clear_binary"
  done

  return 0
}

# ---- Worker mode (xargs -P invocation) ------------------------------
#
# When invoked with --__worker as the first arg, run a single
# `(case, binary)` unit identified by its unit key (the single
# argv element) and write its captured pass/FAIL output to a buffer
# file. All shared state (unit table path, cases-dir, buffer-dir,
# scratch base, results file, update flag) is passed via
# KIO_TEST_WORKER_* env vars. The unit key is a hashable
# fixed-shape token (`<safe_case>__<safe_binary>`); the worker
# looks up the row in $UNITS_FILE and dispatches to run_one_unit.
#
# The single-arg shape exists so the worker survives macOS BSD
# `xargs -I {}`, which normalizes tab characters in the substituted
# `{}` to single spaces before invoking utility. Don't reintroduce
# in-band whitespace into the xargs payload.

if [ "${1:-}" = "--__worker" ]; then
  shift
  _w_unit_key=$1
  _w_script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
  RUN_TESTS_CI_DIR=$_w_script_dir
  TEST_RUNNER_IDENTITY_PROXY=$_w_script_dir/infra/test-runner-identity-proxy.sh
  if [ "${KIO_CI_SCHEDULE:-}" != DISABLE ]; then
    case "${KIO_CI_SCHEDULE_HELD:-}" in
      work|work,cargo|work,compiler|work,cargo,compiler) ;;
      *)
        _w_script_path=$_w_script_dir/$(basename -- "$0")
        exec sh "$_w_script_dir/schedule.sh" -- \
          sh "$_w_script_path" --__worker "$_w_unit_key"
        ;;
    esac
  fi
  _w_buffer="$KIO_TEST_WORKER_BUFFER_DIR/$_w_unit_key"
  _w_done="$KIO_TEST_WORKER_DONE_DIR/$_w_unit_key"
  # Hydrate shared globals from the worker env.
  CASES_DIR="$KIO_TEST_WORKER_CASES_DIR"
  CACHE_BASE="$KIO_TEST_WORKER_CACHE_BASE"
  KEEP_CACHE="$KIO_TEST_WORKER_KEEP_CACHE"
  UNITS_FILE="$KIO_TEST_WORKER_UNITS_FILE"
  RESULTS_FILE="$KIO_TEST_WORKER_RESULTS_FILE"
  UNIT_SCRATCH_BASE="$KIO_TEST_WORKER_SCRATCH_BASE"
  CASE_BINARY_CHECKS_FILE="${KIO_TEST_WORKER_CB_CHECKS_FILE:-}"
  IMPL_CHECKS_FILE="${KIO_TEST_WORKER_IMPL_CHECKS_FILE:-}"
  COMPILER_PROXY_DIR="$KIO_TEST_WORKER_COMPILER_PROXY_DIR"
  CLEAR_BINARY_FILE="$KIO_TEST_WORKER_CLEAR_BINARY_FILE"
  UPDATE_FLAG="$KIO_TEST_WORKER_UPDATE"
  SHOW_OUTPUT_FLAG="$KIO_TEST_WORKER_SHOW_OUTPUT"
  _w_stream="${KIO_TEST_WORKER_STREAM:-0}"
  CUSTOM_TYPED_CACHE_COHORT_FILE="${KIO_TEST_WORKER_CUSTOM_TYPED_CACHE_COHORT_FILE:-}"
  CUSTOM_TYPED_CACHE_ROOT="${KIO_TEST_WORKER_CUSTOM_TYPED_CACHE_ROOT:-}"
  CUSTOM_TYPED_CACHE_SCRIPTS_ROOT="${KIO_TEST_WORKER_CUSTOM_TYPED_CACHE_SCRIPTS_ROOT:-}"
  # Worker variables transport parent-only paths into this shell. Do not leak
  # them into case checks or custom run.sh subprocesses: an unlisted case
  # receives only its documented case environment and isolated typed root.
  unset KIO_TEST_WORKER_BUFFER_DIR KIO_TEST_WORKER_DONE_DIR
  unset KIO_TEST_WORKER_CASES_DIR KIO_TEST_WORKER_CACHE_BASE
  unset KIO_TEST_WORKER_KEEP_CACHE KIO_TEST_WORKER_UNITS_FILE
  unset KIO_TEST_WORKER_RESULTS_FILE KIO_TEST_WORKER_SCRATCH_BASE
  unset KIO_TEST_WORKER_CB_CHECKS_FILE KIO_TEST_WORKER_IMPL_CHECKS_FILE
  unset KIO_TEST_WORKER_COMPILER_PROXY_DIR KIO_TEST_WORKER_CLEAR_BINARY_FILE
  unset KIO_TEST_WORKER_UPDATE KIO_TEST_WORKER_SHOW_OUTPUT
  unset KIO_TEST_WORKER_CUSTOM_TYPED_CACHE_COHORT_FILE
  unset KIO_TEST_WORKER_CUSTOM_TYPED_CACHE_ROOT
  unset KIO_TEST_WORKER_CUSTOM_TYPED_CACHE_SCRIPTS_ROOT
  unset KIO_TEST_WORKER_STREAM
  if [ "$_w_stream" = 1 ]; then
    run_one_unit "$_w_unit_key"
    _w_status=$?
    : >"$_w_done"
    exit "$_w_status"
  fi
  run_one_unit "$_w_unit_key" >"$_w_buffer" 2>&1
  _w_status=$?
  grep -E '^FAIL \[' "$_w_buffer" | while IFS= read -r _w_fail; do
    emit_live_progress "$_w_fail"
  done
  # Completion marker for the progress ticker. The buffer file is created
  # by the redirect above — i.e. when the unit STARTS — so it cannot stand
  # in for "finished". Written whatever the unit's status, since the
  # ticker counts units done, not units passed.
  : >"$_w_done"
  exit "$_w_status"
fi

# ---- Main mode ------------------------------------------------------

UPDATE=0
SHOW_OUTPUT=0
CASES_DIR=
CACHE_BASE=
KEEP_CACHE=
JOBS=auto
JOBS_SET=0
if [ "${KIO_CI_SCHEDULE_COMPILER_JOBS+x}" = x ]; then
  COMPILER_JOBS=$KIO_CI_SCHEDULE_COMPILER_JOBS
  COMPILER_JOBS_EXPLICIT=1
else
  COMPILER_JOBS=adaptive
  COMPILER_JOBS_EXPLICIT=0
fi
PRIME_ONLY=0
DYN_LOAD_PRIME_ONLY=0
SAMPLE_IMPL=0
SAMPLE_CASES=
SAMPLE_CASES_SET=0
CASE_SEED=
IMPL_CASE_SET_FILE=
IMPL_CASE_SET_SET=0
CUSTOM_TYPED_CACHE_COHORT=
CUSTOM_TYPED_CACHE_COHORT_SET=0

# Resolve the script path to absolute so workers spawned via xargs can
# find it regardless of the cwd they inherit.
SCRIPT_PATH=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)/$(basename -- "$0")
INVOCATION_DIR=$(pwd -P)
RUN_TESTS_CI_DIR=${SCRIPT_PATH%/*}
TEST_RUNNER_IDENTITY_PROXY=$(dirname -- "$SCRIPT_PATH")/infra/test-runner-identity-proxy.sh

run_tests_owns_tmp=1
run_tests_supervisor_pid=
run_tests_tree_drained=1
run_tests_pending_status=
run_tests_stdin_reserved=

reserve_run_tests_stdin() {
  if ! (: <&9) 2>/dev/null; then
    exec 9<&0; run_tests_stdin_reserved=9; return 0
  fi
  if ! (: <&8) 2>/dev/null; then
    exec 8<&0; run_tests_stdin_reserved=8; return 0
  fi
  if ! (: <&7) 2>/dev/null; then
    exec 7<&0; run_tests_stdin_reserved=7; return 0
  fi
  if ! (: <&6) 2>/dev/null; then
    exec 6<&0; run_tests_stdin_reserved=6; return 0
  fi
  if ! (: <&5) 2>/dev/null; then
    exec 5<&0; run_tests_stdin_reserved=5; return 0
  fi
  if ! (: <&4) 2>/dev/null; then
    exec 4<&0; run_tests_stdin_reserved=4; return 0
  fi
  if ! (: <&3) 2>/dev/null; then
    exec 3<&0; run_tests_stdin_reserved=3; return 0
  fi
  return 1
}

close_run_tests_stdin_reservation() {
  case "$run_tests_stdin_reserved" in
    9) exec 9<&- ;; 8) exec 8<&- ;; 7) exec 7<&- ;; 6) exec 6<&- ;;
    5) exec 5<&- ;; 4) exec 4<&- ;; 3) exec 3<&- ;;
    '') return 0 ;;
    *) return 2 ;;
  esac
  run_tests_stdin_reserved=
}

run_tests_help_requested() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      -h|--help) return 0 ;;
      --) return 1 ;;
      -u|--update-expected|--show-output|--prime-only|--dyn-load-prime-only|--keep-cache) ;;
      --impls=SAMPLE_IMPL|--impls=FULL_IMPL_MATRIX) ;;
      --cases-dir=*|--cache-base=*|--impl-def=*|--jobs=*|--compiler-jobs=*|--sample-cases=*|--impl-case-set-file=*|--custom-typed-cache-cohort=*|--case-seed=*|--exclude=*|--check=*) ;;
      --impls|--cases-dir|--cache-base|--impl-def|--jobs|--compiler-jobs|--sample-cases|--impl-case-set-file|--custom-typed-cache-cohort|--case-seed|--exclude|--check)
        shift
        [ "$#" -gt 0 ] || return 1
        ;;
      -*) return 1 ;;
      *) return 1 ;;
    esac
    shift
  done
  return 1
}

case "${1:-}" in
  --__supervised-main)
    [ -n "${KIO_RUN_TESTS_SUPERVISED_TMP:-}" ] || {
      printf 'error: invalid internal supervised run-tests invocation\n' >&2
      exit 2
    }
    TMPDIR_RUN=$KIO_RUN_TESTS_SUPERVISED_TMP
    [ -d "$TMPDIR_RUN" ] || {
      printf 'error: supervised run-tests root is not a directory: %s\n' \
        "$TMPDIR_RUN" >&2
      exit 2
    }
    run_tests_owns_tmp=0
    unset KIO_RUN_TESTS_SUPERVISED_TMP
    shift
    ;;
  *) TMPDIR_RUN=$(mktemp -d) ;;
esac
export KIO_DEBUG_TYPED_CACHE_ROOT="$TMPDIR_RUN/shared-typed-cache"
# Reap the progress ticker before the temp tree goes: it holds the
# live-progress fd, and ci/all.sh's reporter waits for that write end to
# close. This handler is defined before option validation because an early
# exit must not name stop_progress_ticker, which is defined much later.
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_run_tests() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  if [ -n "${progress_ticker_pid:-}" ]; then
    kill "$progress_ticker_pid" 2>/dev/null || true
    wait "$progress_ticker_pid" 2>/dev/null || true
    progress_ticker_pid=
  fi
  if [ "$run_tests_owns_tmp" = 1 ] &&
     [ -n "${run_tests_supervisor_pid:-}" ]; then
    if ! : >"$run_tests_cancel_file"; then
      # If the portable cooperative channel itself fails, terminating the
      # supervisor still closes its Unix process group or Windows Job. Without
      # the drained marker below, retain the state tree conservatively.
      kill -TERM "$run_tests_supervisor_pid" 2>/dev/null || true
    fi
    wait "$run_tests_supervisor_pid" 2>/dev/null || true
    run_tests_supervisor_pid=
  fi
  if [ "$run_tests_owns_tmp" = 1 ] &&
     [ -n "${run_tests_drained_marker:-}" ] &&
     [ -f "$run_tests_drained_marker" ]; then
    run_tests_tree_drained=1
  fi
  if [ "$run_tests_owns_tmp" = 1 ]; then
    if [ "$run_tests_tree_drained" = 1 ]; then
      rm -rf "$TMPDIR_RUN"
    else
      printf 'error: process-tree extinction was not established; preserved run state at %s\n' \
        "$TMPDIR_RUN" >&2
    fi
  fi
  exit "$cleanup_status"
}
trap cleanup_run_tests EXIT

if [ "$run_tests_owns_tmp" = 1 ]; then
  if run_tests_help_requested "$@"; then
    :
  else
    run_tests_drained_marker=$TMPDIR_RUN/process-tree.drained
    run_tests_cancel_file=$TMPDIR_RUN/process-tree.cancel
    trap '[ -n "$run_tests_pending_status" ] || run_tests_pending_status=130' INT
    trap '[ -n "$run_tests_pending_status" ] || run_tests_pending_status=143' TERM
    trap '[ -n "$run_tests_pending_status" ] || run_tests_pending_status=129' HUP
    if (exec 7<&0) 2>/dev/null; then
      reserve_run_tests_stdin || {
        printf 'error: no portable file descriptor is available to preserve standard input\n' >&2
        exit 1
      }
    fi
    run_tests_tree_drained=0
    if [ -n "$run_tests_stdin_reserved" ]; then
      (
        close_run_tests_stdin_reservation
        KIO_RUN_TESTS_SUPERVISED_TMP=$TMPDIR_RUN
        export KIO_RUN_TESTS_SUPERVISED_TMP
        exec sh "$RUN_TESTS_CI_DIR/schedule.sh" --supervise \
          --drained-marker "$run_tests_drained_marker" \
          --cancel-file "$run_tests_cancel_file" -- \
          sh "$SCRIPT_PATH" --__supervised-main "$@"
      ) <&"$run_tests_stdin_reserved" &
    else
      KIO_RUN_TESTS_SUPERVISED_TMP=$TMPDIR_RUN \
        sh "$RUN_TESTS_CI_DIR/schedule.sh" --supervise \
          --drained-marker "$run_tests_drained_marker" \
          --cancel-file "$run_tests_cancel_file" -- \
          sh "$SCRIPT_PATH" --__supervised-main "$@" 0<&- &
    fi
    run_tests_supervisor_pid=$!
    trap 'exit 130' INT
    trap 'exit 143' TERM
    trap 'exit 129' HUP
    close_run_tests_stdin_reservation
    [ -z "$run_tests_pending_status" ] || exit "$run_tests_pending_status"
    if wait "$run_tests_supervisor_pid"; then
      run_tests_supervisor_status=0
    else
      run_tests_supervisor_status=$?
    fi
    run_tests_supervisor_pid=
    if [ -f "$run_tests_drained_marker" ]; then
      run_tests_tree_drained=1
    elif [ "$run_tests_supervisor_status" -eq 0 ]; then
      run_tests_supervisor_status=1
    fi
    exit "$run_tests_supervisor_status"
  fi
fi

trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

impls_file="$TMPDIR_RUN/impls"
: >"$impls_file"
IMPL_EMPTY=__KIO_EMPTY_IMPL_FIELD__

excludes_file="$TMPDIR_RUN/excludes"
: >"$excludes_file"

checks_file="$TMPDIR_RUN/checks"
: >"$checks_file"

while [ $# -gt 0 ]; do
  case "$1" in
    -u|--update-expected) UPDATE=1 ;;
    --show-output)        SHOW_OUTPUT=1 ;;
    --prime-only)         PRIME_ONLY=1 ;;
    --dyn-load-prime-only) DYN_LOAD_PRIME_ONLY=1 ;;
    --impls=SAMPLE_IMPL)  SAMPLE_IMPL=1 ;;
    --impls=FULL_IMPL_MATRIX) SAMPLE_IMPL=0 ;;
    --impls=*)
      printf 'error: ci/run-tests.sh only accepts --impls=SAMPLE_IMPL or --impls=FULL_IMPL_MATRIX (got %s)\n' "${1#--impls=}" >&2
      exit 2
      ;;
    --impls)
      shift
      [ $# -gt 0 ] || { printf 'error: --impls requires SAMPLE_IMPL or FULL_IMPL_MATRIX\n' >&2; exit 2; }
      case "$1" in
        SAMPLE_IMPL) SAMPLE_IMPL=1 ;;
        FULL_IMPL_MATRIX) SAMPLE_IMPL=0 ;;
        *) printf 'error: ci/run-tests.sh only accepts --impls=SAMPLE_IMPL or --impls=FULL_IMPL_MATRIX (got %s)\n' "$1" >&2; exit 2 ;;
      esac
      ;;
    --cases-dir=*)        CASES_DIR=${1#--cases-dir=} ;;
    --cases-dir)
      shift
      [ $# -gt 0 ] || { printf 'error: --cases-dir requires a value\n' >&2; exit 2; }
      CASES_DIR=$1
      ;;
    --cache-base=*)       CACHE_BASE=${1#--cache-base=} ;;
    --cache-base)
      shift
      [ $# -gt 0 ] || { printf 'error: --cache-base requires a value\n' >&2; exit 2; }
      CACHE_BASE=$1
      ;;
    --keep-cache) KEEP_CACHE=1 ;;
    --impl-def=*)
      parse_impl_spec "${1#--impl-def=}"
      [ -n "$PARSED_RUNNER_CACHE_KIND" ] || PARSED_RUNNER_CACHE_KIND=$IMPL_EMPTY
      [ -n "$PARSED_PRIME_KIO" ] || PARSED_PRIME_KIO=$IMPL_EMPTY
      printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$PARSED_NAME" "$PARSED_KIO" "$PARSED_RUNNER" "$PARSED_TARGET" "$PARSED_RUNNER_CACHE_KIND" "$PARSED_PRIME_KIO" >>"$impls_file"
      ;;
    --impl-def)
      shift
      [ $# -gt 0 ] || { printf 'error: --impl-def requires a value\n' >&2; exit 2; }
      parse_impl_spec "$1"
      [ -n "$PARSED_RUNNER_CACHE_KIND" ] || PARSED_RUNNER_CACHE_KIND=$IMPL_EMPTY
      [ -n "$PARSED_PRIME_KIO" ] || PARSED_PRIME_KIO=$IMPL_EMPTY
      printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$PARSED_NAME" "$PARSED_KIO" "$PARSED_RUNNER" "$PARSED_TARGET" "$PARSED_RUNNER_CACHE_KIND" "$PARSED_PRIME_KIO" >>"$impls_file"
      ;;
    --jobs=*) JOBS=${1#--jobs=}; JOBS_SET=1 ;;
    --jobs)
      shift
      [ $# -gt 0 ] || { printf 'error: --jobs requires a value\n' >&2; exit 2; }
      JOBS=$1
      JOBS_SET=1
      ;;
    --compiler-jobs=*)
      COMPILER_JOBS=${1#--compiler-jobs=}
      COMPILER_JOBS_EXPLICIT=1
      ;;
    --compiler-jobs)
      shift
      [ $# -gt 0 ] || { printf 'error: --compiler-jobs requires a value\n' >&2; exit 2; }
      COMPILER_JOBS=$1
      COMPILER_JOBS_EXPLICIT=1
      ;;
    --sample-cases=*) SAMPLE_CASES=${1#--sample-cases=}; SAMPLE_CASES_SET=1 ;;
    --sample-cases)
      shift
      [ $# -gt 0 ] || { printf 'error: --sample-cases requires a value\n' >&2; exit 2; }
      SAMPLE_CASES=$1
      SAMPLE_CASES_SET=1
      ;;
    --impl-case-set-file=*) IMPL_CASE_SET_FILE=${1#--impl-case-set-file=}; IMPL_CASE_SET_SET=1 ;;
    --impl-case-set-file)
      shift
      [ $# -gt 0 ] || { printf 'error: --impl-case-set-file requires a value\n' >&2; exit 2; }
      IMPL_CASE_SET_FILE=$1
      IMPL_CASE_SET_SET=1
      ;;
    --custom-typed-cache-cohort=*)
      [ "$CUSTOM_TYPED_CACHE_COHORT_SET" = 0 ] || {
        printf 'error: --custom-typed-cache-cohort may be specified only once\n' >&2
        exit 2
      }
      CUSTOM_TYPED_CACHE_COHORT=${1#--custom-typed-cache-cohort=}
      CUSTOM_TYPED_CACHE_COHORT_SET=1
      [ -n "$CUSTOM_TYPED_CACHE_COHORT" ] || {
        printf 'error: --custom-typed-cache-cohort requires a value\n' >&2
        exit 2
      }
      ;;
    --custom-typed-cache-cohort)
      [ "$CUSTOM_TYPED_CACHE_COHORT_SET" = 0 ] || {
        printf 'error: --custom-typed-cache-cohort may be specified only once\n' >&2
        exit 2
      }
      shift
      [ $# -gt 0 ] || {
        printf 'error: --custom-typed-cache-cohort requires a value\n' >&2
        exit 2
      }
      CUSTOM_TYPED_CACHE_COHORT=$1
      CUSTOM_TYPED_CACHE_COHORT_SET=1
      ;;
    --case-seed=*)
      CASE_SEED=${1#--case-seed=}
      # An empty seed would be silently replaced by the epoch default, and
      # the run would then report a "reproduce with" seed the caller never
      # chose for a draw they believed they had pinned.
      [ -n "$CASE_SEED" ] || { printf 'error: --case-seed requires a value\n' >&2; exit 2; }
      ;;
    --case-seed)
      shift
      [ $# -gt 0 ] || { printf 'error: --case-seed requires a value\n' >&2; exit 2; }
      CASE_SEED=$1
      [ -n "$CASE_SEED" ] || { printf 'error: --case-seed requires a value\n' >&2; exit 2; }
      ;;
    --exclude=*) printf '%s\n' "${1#--exclude=}" >>"$excludes_file" ;;
    --exclude)
      shift
      [ $# -gt 0 ] || { printf 'error: --exclude requires a value\n' >&2; exit 2; }
      printf '%s\n' "$1" >>"$excludes_file"
      ;;
    --check=*) printf '%s\n' "${1#--check=}" >>"$checks_file" ;;
    --check)
      shift
      [ $# -gt 0 ] || { printf 'error: --check requires a value\n' >&2; exit 2; }
      printf '%s\n' "$1" >>"$checks_file"
      ;;
    -h|--help) usage; exit 0 ;;
    --) shift; break ;;
    -*) printf 'error: unknown flag: %s\n' "$1" >&2; exit 2 ;;
    *) break ;;
  esac
  shift
done

if [ -z "$CASES_DIR" ]; then
  printf 'error: --cases-dir=<dir> is required\n' >&2
  exit 2
fi

if [ ! -d "$CASES_DIR" ]; then
  printf 'error: --cases-dir=%s is not a directory\n' "$CASES_DIR" >&2
  exit 2
fi

if [ -z "$CACHE_BASE" ]; then
  printf 'error: --cache-base=<dir> is required\n' >&2
  exit 2
fi
# Ensure cache base is absolute so each custom run.sh's `cd workdir` doesn't
# break the path that gets passed to the runner.
case "$CACHE_BASE" in
  /*) ;;
  *)  CACHE_BASE=$(CDPATH='' cd -- "$(dirname -- "$CACHE_BASE")" 2>/dev/null && pwd)/$(basename -- "$CACHE_BASE") || {
        printf 'error: cannot resolve --cache-base=%s to an absolute path\n' "$CACHE_BASE" >&2
        exit 2
      }
      ;;
esac
mkdir -p "$CACHE_BASE" 2>/dev/null || true

if [ ! -s "$impls_file" ]; then
  printf 'error: at least one --impl-def= is required\n' >&2
  exit 2
fi

validate_debug_sample_impl_seed() {
  vdsis_value=$1
  case "$vdsis_value" in
    ''|*[!0-9]*|0?*) return 1 ;;
  esac
  [ "${#vdsis_value}" -le 10 ] || return 1
  if [ "${#vdsis_value}" -eq 10 ]; then
    LC_ALL=C awk -v value="$vdsis_value" \
      'BEGIN { exit !((value + 0) <= 4294967295) }' </dev/null || return 1
  fi
}

if [ "${KIO_DEBUG_SAMPLE_IMPL_SEED+x}" = x ]; then
  validate_debug_sample_impl_seed "$KIO_DEBUG_SAMPLE_IMPL_SEED" || {
    printf 'error: KIO_DEBUG_SAMPLE_IMPL_SEED must be canonical decimal in 0..4294967295\n' >&2
    exit 2
  }
  [ "$SAMPLE_IMPL" -eq 1 ] || {
    printf 'error: KIO_DEBUG_SAMPLE_IMPL_SEED requires --impls=SAMPLE_IMPL\n' >&2
    exit 2
  }
fi

case "$JOBS" in
  auto) ;;
  ''|*[!0-9]*)
    printf 'error: --jobs must be "auto" or a positive integer (got %s)\n' "$JOBS" >&2
    exit 2
    ;;
  *)
    if [ "$JOBS" -lt 1 ]; then
      printf 'error: --jobs must be >= 1 (got %s)\n' "$JOBS" >&2
      exit 2
    fi
    ;;
esac

case "$COMPILER_JOBS" in
  adaptive)
    [ "$COMPILER_JOBS_EXPLICIT" -eq 0 ] || {
      printf 'error: --compiler-jobs must be a positive integer (got adaptive)\n' >&2
      exit 2
    }
    ;;
  ''|*[!0-9]*)
    printf 'error: --compiler-jobs must be a positive integer (got %s)\n' \
      "$COMPILER_JOBS" >&2
    exit 2
    ;;
  *)
    [ "$COMPILER_JOBS" -ge 1 ] || {
      printf 'error: --compiler-jobs must be >= 1 (got %s)\n' "$COMPILER_JOBS" >&2
      exit 2
    }
    ;;
esac
if [ "$COMPILER_JOBS" = adaptive ]; then
  unset KIO_CI_SCHEDULE_COMPILER_JOBS
else
  KIO_CI_SCHEDULE_COMPILER_JOBS=$COMPILER_JOBS
  export KIO_CI_SCHEDULE_COMPILER_JOBS
fi

# Direct run-tests.sh entry points do not pass through an orchestrator.
# Configure wrappers before any possible bootstrap, then perform the early
# probe through native readiness isolation whenever a scheduler context exists.
# Cache-miss commands recheck after admission.
# shellcheck disable=SC1091
. "$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)/infra/sccache.sh"
kio_configure_sccache_environment

if [ "${KIO_CI_SCHEDULE:-}" != DISABLE ] ||
   [ -n "${KIO_CI_SCHEDULER_BIN:-}" ] ||
   [ -n "${KIO_CI_SCHEDULE_HELD:-}" ] ||
   [ -n "${KIO_CI_SCHEDULE_LEASE_FDS:-}" ]; then
  KIO_CI_SCHEDULER_BIN=$(sh "$RUN_TESTS_CI_DIR/schedule.sh" --prepare) || exit $?
  export KIO_CI_SCHEDULER_BIN
fi
sh "$RUN_TESTS_CI_DIR/schedule.sh" --readiness -- sh || exit $?
if [ "${KIO_CI_SCHEDULE:-}" != DISABLE ]; then
  KIO_CI_SCHEDULE_DIR=$(sh "$RUN_TESTS_CI_DIR/schedule.sh" --state-dir) || exit $?
  export KIO_CI_SCHEDULE_DIR
fi

# Native runner library calls do not re-enter the shell facade. Register the
# tooling-owned adapter only when a compatible runner compiler wrapper can
# actually start a daemon; otherwise ordinary native compilers need neither an
# extra shell nor Windows Job breakaway. The scheduler itself remains generic.
# shellcheck disable=SC2031 # the parent-level option value, not a worker subshell mutation
if kio_any_sccache_wrapper "${KIO_TEST_RUNNER_COMPILER_WRAPPER:-}"; then
  KIO_CI_SCHEDULE_READINESS_HOOK_PROGRAM='sh'
  KIO_CI_SCHEDULE_READINESS_HOOK_ARG_COUNT=1
  KIO_CI_SCHEDULE_READINESS_HOOK_ARG_0=$RUN_TESTS_CI_DIR/schedule.sh
  export KIO_CI_SCHEDULE_READINESS_HOOK_PROGRAM
  export KIO_CI_SCHEDULE_READINESS_HOOK_ARG_COUNT
  export KIO_CI_SCHEDULE_READINESS_HOOK_ARG_0
else
  unset \
    KIO_CI_SCHEDULE_READINESS_HOOK_PROGRAM \
    KIO_CI_SCHEDULE_READINESS_HOOK_ARG_COUNT \
    KIO_CI_SCHEDULE_READINESS_HOOK_ARG_0
fi

COMPILER_PROXY_DIR="$TMPDIR_RUN/native-compiler-proxy"
setup_native_compiler_proxy "$COMPILER_PROXY_DIR" "${SCRIPT_PATH%/*}" || {
  printf 'error: cannot prepare native compiler admission proxy\n' >&2
  exit 2
}

# --sample-cases=<N>: cap the number of cases per top-level bucket that
# run their impls (build + run + `# ROUTING: impl` checks). `all` — and
# the default, an unset flag — leaves every case running its impls.
# A cap of 0 would run no impls at all and still exit 0 — a green gate that
# built and ran nothing. That is the "opt a target out" shape AGENTS.md
# § Universal rules forbids, so the floor is 1.
case "${SAMPLE_CASES_SET:-0}${SAMPLE_CASES}" in
  # `--sample-cases=` with an empty value would otherwise fall into the
  # unset arm and silently mean "run every case" — the opposite of what the
  # caller asked for.
  1) printf 'error: --sample-cases requires "all" or a positive integer\n' >&2; exit 2 ;;
esac
case "$SAMPLE_CASES" in
  ''|all) SAMPLE_CASES= ;;
  *[!0-9]*)
    printf 'error: --sample-cases must be "all" or a positive integer (got %s)\n' "$SAMPLE_CASES" >&2
    exit 2
    ;;
  0)
    printf 'error: --sample-cases=0 would run no case on any impl and still pass; use a positive integer, or --sample-cases=all\n' >&2
    exit 2
    ;;
esac

if [ -n "$IMPL_CASE_SET_FILE" ] && [ "$SAMPLE_CASES_SET" = 1 ]; then
  printf 'error: --impl-case-set-file and --sample-cases are mutually exclusive\n' >&2
  exit 2
fi
if [ "$IMPL_CASE_SET_SET" = 1 ] && [ -z "$IMPL_CASE_SET_FILE" ]; then
  printf 'error: --impl-case-set-file requires a value\n' >&2
  exit 2
fi
if [ -n "$IMPL_CASE_SET_FILE" ]; then
  case "$IMPL_CASE_SET_FILE" in
    /*) ;;
    *) printf 'error: --impl-case-set-file requires an absolute path\n' >&2; exit 2 ;;
  esac
  if [ ! -f "$IMPL_CASE_SET_FILE" ] || [ -L "$IMPL_CASE_SET_FILE" ]; then
    printf 'error: --impl-case-set-file must name a regular non-symlink file\n' >&2
    exit 2
  fi
  if ! LC_ALL=C awk '
    $0 == "" || $0 !~ /^[A-Za-z0-9_][A-Za-z0-9_.\/-]*$/ ||
      $0 ~ /(^|\/)\.\.?(\/|$)/ || $0 ~ /\/\// || $0 ~ /\/$/ { exit 1 }
    previous != "" && ("x" $0) <= ("x" previous) { exit 1 }
    { previous=$0 }
    END { if (NR == 0) exit 1 }
  ' "$IMPL_CASE_SET_FILE"; then
    printf 'error: --impl-case-set-file must be non-empty, normalized, safe, sorted, and unique\n' >&2
    exit 2
  fi
fi

CUSTOM_TYPED_CACHE_COHORT_FILE=
CUSTOM_TYPED_CACHE_ROOT=
CUSTOM_TYPED_CACHE_SCRIPTS_ROOT=
if [ -n "$CUSTOM_TYPED_CACHE_COHORT" ]; then
  case "$CUSTOM_TYPED_CACHE_COHORT" in
    exec-dyn-load-goldens-v1)
      custom_typed_cache_cohort_source=$RUN_TESTS_CI_DIR/checks/orchestrators/custom-typed-cache/exec-dyn-load-goldens.tsv
      ;;
    *)
      printf 'error: unknown --custom-typed-cache-cohort: %s\n' \
        "$CUSTOM_TYPED_CACHE_COHORT" >&2
      exit 2
      ;;
  esac
  custom_typed_cache_expected_cases=$RUN_TESTS_CI_DIR/../test-data/goldens
  if [ ! -d "$custom_typed_cache_expected_cases" ]; then
    printf 'error: custom typed-cache cohort cannot resolve canonical goldens directory\n' >&2
    exit 2
  fi
  custom_typed_cache_cases=$(CDPATH='' cd -- "$CASES_DIR" && pwd -P) || exit 2
  custom_typed_cache_expected_cases=$(CDPATH='' cd -- \
    "$custom_typed_cache_expected_cases" && pwd -P) || exit 2
  if [ "$custom_typed_cache_cases" != "$custom_typed_cache_expected_cases" ]; then
    printf 'error: --custom-typed-cache-cohort is valid only for the canonical goldens directory\n' >&2
    exit 2
  fi
  if [ ! -f "$custom_typed_cache_cohort_source" ] ||
     [ -L "$custom_typed_cache_cohort_source" ]; then
    printf 'error: custom typed-cache cohort manifest must be a regular non-symlink file\n' >&2
    exit 2
  fi
  CUSTOM_TYPED_CACHE_COHORT_FILE=$TMPDIR_RUN/custom-typed-cache-cohort.tsv
  cp "$custom_typed_cache_cohort_source" \
    "$CUSTOM_TYPED_CACHE_COHORT_FILE" || exit 2
  chmod 0444 "$CUSTOM_TYPED_CACHE_COHORT_FILE" || exit 2
  custom_typed_cache_empty_oid=$(git hash-object --stdin </dev/null) || exit 2
  custom_typed_cache_oid_length=${#custom_typed_cache_empty_oid}
  if ! LC_ALL=C awk -F '\t' -v oid_length="$custom_typed_cache_oid_length" '
    NF != 2 || $1 == "" || $2 == "" ||
      $1 !~ /^[A-Za-z0-9_][A-Za-z0-9_.\/-]*$/ ||
      $1 ~ /(^|\/)\.\.?(\/|$)/ || $1 ~ /\/\// || $1 ~ /\/$/ ||
      $1 !~ /\/run\.sh$/ || length($2) != oid_length ||
      $2 ~ /[^0-9a-f]/ { exit 1 }
    previous != "" && ("x" $1) <= ("x" previous) { exit 1 }
    { previous=$1 }
    END { if (NR == 0) exit 1 }
  ' "$CUSTOM_TYPED_CACHE_COHORT_FILE"; then
    printf 'error: custom typed-cache cohort manifest must contain non-empty, normalized, safe, sorted, unique path<TAB>Git-object rows\n' >&2
    exit 2
  fi
  CUSTOM_TYPED_CACHE_ROOT=$TMPDIR_RUN/custom-typed-cache
  CUSTOM_TYPED_CACHE_SCRIPTS_ROOT=$TMPDIR_RUN/custom-typed-cache-scripts
fi

# A seed with no sampling is a no-op at every layer, not an error: ci/all.sh
# forwards one seed to every corpus orchestrator uniformly, including the
# corpora that run whole, so rejecting it here would break the ordinary
# path. Report it rather than erroring, so a caller who meant to sample and
# passed only a seed is not left believing a draw was pinned.
if [ -n "$CASE_SEED" ] && [ -z "$SAMPLE_CASES" ]; then
  printf 'note: --case-seed is inert here — every case runs; pass --sample-cases=<N> to draw a sample\n' >&2
  CASE_SEED=
fi

# The sample is reproducible from its seed alone, so a failing run can be
# replayed case-for-case. An explicit --case-seed wins; under GitHub
# Actions the run context makes each run's draw distinct but recoverable
# from the run page; otherwise UTC epoch seconds for an ad-hoc local run.
if [ -n "$SAMPLE_CASES" ] && [ -z "$CASE_SEED" ]; then
  if [ -n "${GITHUB_RUN_ID:-}" ]; then
    CASE_SEED="${GITHUB_RUN_ID}:${GITHUB_RUN_ATTEMPT:-1}"
  else
    CASE_SEED=$(date -u +%s)
  fi
fi

# Resolve each impl's kio and runner to absolute paths. Reject
# duplicates and reject '|' in any field (used as an in-row impl-tuple
# separator in the worklist; see run_one_unit).
path_probe_dir=$TMPDIR_RUN/path-probe
mkdir "$path_probe_dir"
resolve_impl_executable() {
  rie_value=$1
  rie_resolved=$(command -v "$rie_value" 2>/dev/null) || return 1
  case "${OS:-}:${MSYSTEM:-}" in
    Windows_NT:*|*:MINGW*|*:MSYS*|*:CYGWIN*)
      case "$rie_resolved" in
        [A-Za-z]:[\\/]*|\\\\*)
          # Proxy basename/sibling lookup uses shell path separators too.
          rie_resolved=$(cygpath -u "$rie_resolved") || return 1
          ;;
      esac
      ;;
  esac
  case "$rie_resolved" in
    /*) ;;
    */*) rie_resolved=$INVOCATION_DIR/$rie_resolved ;;
    *)
      # An empty PATH component finds an executable in the current
      # directory but dash reports that hit without a slash. Distinguish it
      # from a shell builtin/function by resolving once from an empty harness
      # scratch directory: builtins keep the same bare result;
      # a cwd-relative filesystem hit disappears or resolves elsewhere.
      rie_away=$(
        CDPATH='' cd -- "$path_probe_dir" || exit 1
        command -v "$rie_value" 2>/dev/null || true
      )
      if [ "$rie_away" != "$rie_resolved" ]; then
        rie_resolved=$INVOCATION_DIR/$rie_resolved
      fi
      ;;
  esac
  printf '%s\n' "$rie_resolved"
}

resolved_impls_file="$TMPDIR_RUN/impls.resolved"
: >"$resolved_impls_file"
seen_names_file="$TMPDIR_RUN/impls.names"
: >"$seen_names_file"

while IFS=$TAB read -r impl_name impl_kio impl_runner impl_target impl_runner_cache_kind impl_prime_kio; do
  [ -n "$impl_name" ] || continue
  [ "$impl_runner_cache_kind" != "$IMPL_EMPTY" ] || impl_runner_cache_kind=
  [ "$impl_prime_kio" != "$IMPL_EMPTY" ] || impl_prime_kio=
  if grep -Fxq -- "$impl_name" "$seen_names_file"; then
    printf 'error: duplicate impl name: %s\n' "$impl_name" >&2
    exit 2
  fi
  printf '%s\n' "$impl_name" >>"$seen_names_file"
  case "$impl_name$impl_kio$impl_runner$impl_target$impl_prime_kio$impl_runner_cache_kind" in
    *'|'*|*';'*)
      printf 'error: impl %s: field contains "|" or ";", reserved as in-row separators\n' \
        "$impl_name" >&2
      exit 2
      ;;
  esac

  if ! resolved_kio=$(resolve_impl_executable "$impl_kio"); then
    printf 'error: impl %s: kio=%s is not executable or not on PATH\n' \
      "$impl_name" "$impl_kio" >&2
    exit 2
  fi
  if [ "$impl_runner" = SKIP ]; then
    # Compile-only impl: no runner is invoked (kio test + kio build only),
    # so there is no runner binary to resolve.
    resolved_runner=SKIP
  elif ! resolved_runner=$(resolve_impl_executable "$impl_runner"); then
    printf 'error: impl %s: runner=%s is not executable or not on PATH\n' \
      "$impl_name" "$impl_runner" >&2
    exit 2
  fi
  resolved_prime_kio=
  if [ -n "$impl_prime_kio" ]; then
    if ! resolved_prime_kio=$(resolve_impl_executable "$impl_prime_kio"); then
      printf 'error: impl %s: prime-kio=%s is not executable or not on PATH\n' \
        "$impl_name" "$impl_prime_kio" >&2
      exit 2
    fi
  fi
  resolved_cache_field=${impl_runner_cache_kind:-$IMPL_EMPTY}
  resolved_prime_field=${resolved_prime_kio:-$IMPL_EMPTY}
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$impl_name" "$resolved_kio" "$resolved_runner" "$impl_target" "$resolved_cache_field" "$resolved_prime_field" \
    >>"$resolved_impls_file"
done <"$impls_file"

# Probe each distinct binary across the resolved impls for `cache
# clear` support. Build a binary→clear-binary map (`$CLEAR_BINARY_FILE`):
# binaries that support the subcommand map to themselves; binaries
# that don't fall back to another resolved binary that does (same
# build block → same `cache ...` directive → either binary clears
# the same on-disk path).
clear_binary_file="$TMPDIR_RUN/clear_binary.tsv"
: >"$clear_binary_file"
distinct_binaries_file="$TMPDIR_RUN/binaries.distinct"
: >"$distinct_binaries_file"
awk -F"$TAB" '{print $2}' "$resolved_impls_file" | sort -u >"$distinct_binaries_file"

cache_clear_fallback=
while IFS= read -r bin; do
  [ -n "$bin" ] || continue
  if "$bin" cache --help >/dev/null 2>&1; then
    printf '%s\t%s\n' "$bin" "$bin" >>"$clear_binary_file"
    if [ -z "$cache_clear_fallback" ]; then
      cache_clear_fallback=$bin
    fi
  fi
done <"$distinct_binaries_file"

while IFS= read -r bin; do
  [ -n "$bin" ] || continue
  if ! grep -Fq -- "$bin$TAB" "$clear_binary_file"; then
    if [ -n "$cache_clear_fallback" ]; then
      printf '%s\t%s\n' "$bin" "$cache_clear_fallback" >>"$clear_binary_file"
    else
      # shellcheck disable=SC2016 # backticks are literal text
      printf 'error: no configured binary supports `cache clear` (probed: %s)\n' \
        "$bin" >&2
      exit 2
    fi
  fi
done <"$distinct_binaries_file"

# Print configured impls.
printf 'configured implementations:\n'
while IFS=$TAB read -r impl_name impl_kio impl_runner impl_target _impl_cache_kind impl_prime_kio; do
  [ -n "$impl_name" ] || continue
  [ "$impl_prime_kio" != "$IMPL_EMPTY" ] || impl_prime_kio=
  printf '  %s\n    KIO_BIN=%s\n    KIO_RUNNER=%s\n    KIO_TARGET=%s\n' \
    "$impl_name" "$impl_kio" "$impl_runner" "$impl_target"
  if [ -n "$impl_prime_kio" ]; then
    printf '    KIO_PRIME_BIN=%s\n' "$impl_prime_kio"
  fi
done <"$resolved_impls_file"
printf '\n'

# Partition the --check= list into case-binary-routed and impl-routed
# files. Routing is declared inline in each check script via a
# `# ROUTING: case-binary` marker comment; absence of a marker
# defaults to per-(case, impl) routing. The two files are passed to
# the worker via separate env vars so the inner loop can dispatch
# each check at the right level without re-parsing the markers.
#
# Each case-binary record is `<synth-tag><TAB><path>`: the tag (from
# the check's `# SYNTH-TAG:` marker, default `invariants`) names which
# `<tag>@<binary>` summary row the check's pass/FAIL rolls into, so a
# distinct verification pass can report on its own line. The summary
# needs no registry of those tags: it identifies a non-impl row by
# complement against the configured impl names, so a row bearing any tag
# is counted whether or not a wired check declared it. Impl-routed
# records stay path-only.
cb_checks_file="$TMPDIR_RUN/checks.case-binary"
impl_checks_file="$TMPDIR_RUN/checks.impl"
: >"$cb_checks_file"
: >"$impl_checks_file"
if [ -s "$checks_file" ]; then
  while IFS= read -r check_path; do
    [ -n "$check_path" ] || continue
    kind=$(check_routing "$check_path")
    if [ "$kind" = "case-binary" ]; then
      synth_tag=$(check_synth_tag "$check_path")
      printf '%s\t%s\n' "$synth_tag" "$check_path" >>"$cb_checks_file"
    else
      printf '%s\n' "$check_path" >>"$impl_checks_file"
    fi
  done <"$checks_file"
fi

prime_kio_required=0
while IFS= read -r check_path; do
  [ -n "$check_path" ] || continue
  if grep -q '^# REQUIRES: prime-kio$' "$check_path"; then
    prime_kio_required=1
    break
  fi
done <"$impl_checks_file"
if [ "$prime_kio_required" = 1 ]; then
  while IFS=$TAB read -r impl_name _impl_kio _impl_runner _impl_target _impl_cache_kind impl_prime_kio; do
    [ -n "$impl_name" ] || continue
    [ "$impl_prime_kio" != "$IMPL_EMPTY" ] || impl_prime_kio=
    if [ -z "$impl_prime_kio" ]; then
      printf 'error: impl %s: an enabled check requires prime-kio=<bin>\n' \
        "$impl_name" >&2
      exit 2
    fi
  done <"$resolved_impls_file"
fi

# Inventory every case marker and package-discovery input in one corpus walk,
# then apply the include/exclude filters once. The filtered case list and the
# package inventory are reused when building the (case, binary) worklist below.
# Cases are at any depth under CASES_DIR; the relative path (e.g.,
# 00_success/parse_id) becomes the case name in test output.
all_cases_file="$TMPDIR_RUN/cases.all"
all_cases_unsorted_file="$TMPDIR_RUN/cases.all.unsorted"
package_paths_file="$TMPDIR_RUN/package-paths"
symlink_package_paths_file="$TMPDIR_RUN/package-symlink-paths"
generated_markers_file="$TMPDIR_RUN/generated-markers"
discovery_paths_file="$TMPDIR_RUN/discovery-paths"
discovery_paths_unsorted_file="$TMPDIR_RUN/discovery-paths.unsorted"
find "$CASES_DIR" \
  \( -name 'expected.exit' -type f -o -name '*.pkg.kio' -o -name '.kio-generated' \) \
  -print >"$discovery_paths_unsorted_file" 2>/dev/null
LC_ALL=C sort "$discovery_paths_unsorted_file" >"$discovery_paths_file"
: >"$all_cases_unsorted_file"
: >"$package_paths_file"
: >"$symlink_package_paths_file"
: >"$generated_markers_file"
while IFS= read -r discovery_path; do
  case "$discovery_path" in
    */expected.exit)
      case_dir=${discovery_path%expected.exit}
      while [ "${case_dir%/}" != "$case_dir" ]; do
        case_dir=${case_dir%/}
      done
      [ -n "$case_dir" ] || case_dir=/
      printf '%s\n' "$case_dir" >>"$all_cases_unsorted_file"
      ;;
    */.kio-generated)
      [ -e "$discovery_path" ] || continue
      printf '%s\n' "$discovery_path" >>"$generated_markers_file"
      ;;
    *.pkg.kio)
      # Match compiler discovery: a symlink contributes only when following
      # it reaches a regular file. Broken links and links to directories are
      # not package manifests; a regular-file link remains discoverable but
      # is kept separate so standard cases reject its non-regular shape.
      if [ -L "$discovery_path" ] && [ -f "$discovery_path" ]; then
        printf '%s\n' "$discovery_path" >>"$symlink_package_paths_file"
      elif [ -f "$discovery_path" ]; then
        printf '%s\n' "$discovery_path" >>"$package_paths_file"
      fi
      ;;
  esac
done <"$discovery_paths_file"
LC_ALL=C sort "$all_cases_unsorted_file" >"$all_cases_file"

# A reviewed custom-cache entry is authority supplied by the harness caller,
# not by the case. Authenticate the snapshotted manifest against the complete
# discovered custom-case set, then snapshot every authenticated script before
# dispatch. Workers execute only those immutable snapshots with the original
# case cwd; a later source-tree mutation therefore cannot change cohort code.
if [ -n "$CUSTOM_TYPED_CACHE_COHORT_FILE" ]; then
  mkdir -p "$CUSTOM_TYPED_CACHE_SCRIPTS_ROOT" || exit 2
  while IFS=$TAB read -r trusted_script trusted_oid; do
    trusted_name=${trusted_script%/run.sh}
    trusted_case_dir=$CASES_DIR/$trusted_name
    if ! grep -Fxq -- "$trusted_case_dir" "$all_cases_file" ||
       [ ! -f "$trusted_case_dir/run.sh" ] ||
       [ -L "$trusted_case_dir/run.sh" ] ||
       [ -e "$trusted_case_dir/run.args" ] ||
       [ -e "$trusted_case_dir/run.test-only" ]; then
      printf 'error: trusted custom typed-cache entry is not an exact custom case: %s\n' \
        "$trusted_script" >&2
      exit 2
    fi
    trusted_script_snapshot=$CUSTOM_TYPED_CACHE_SCRIPTS_ROOT/$trusted_script
    mkdir -p "$(dirname -- "$trusted_script_snapshot")" || exit 2
    cp "$trusted_case_dir/run.sh" "$trusted_script_snapshot" || exit 2
    chmod 0444 "$trusted_script_snapshot" || exit 2
    trusted_actual_oid=$(git hash-object --no-filters \
      "$trusted_script_snapshot") || exit 2
    if [ "$trusted_actual_oid" != "$trusted_oid" ]; then
      printf 'error: trusted custom typed-cache script identity changed: %s\n' \
        "$trusted_script" >&2
      exit 2
    fi
  done <"$CUSTOM_TYPED_CACHE_COHORT_FILE"
fi

# Named manifests are checked against the mode cohort before positional
# include/exclude filters, so a focused invocation intersects a valid set
# instead of redefining its canonical membership.
if [ -n "$IMPL_CASE_SET_FILE" ]; then
  canonical_scope_names="$TMPDIR_RUN/cases.canonical-scope"
  : >"$canonical_scope_names"
  while IFS= read -r canonical_case_dir; do
    [ -n "$canonical_case_dir" ] || continue
    if [ "$PRIME_ONLY" = 1 ] && {
      [ ! -f "$canonical_case_dir/IS_KIO_PRIME" ] ||
        [ -f "$canonical_case_dir/SKIP_KIO_PRIME_RUN" ];
    }; then continue; fi
    if [ "$DYN_LOAD_PRIME_ONLY" = 1 ] &&
       [ ! -f "$canonical_case_dir/DYN_LOAD_PRIME" ]; then
      continue
    fi
    printf '%s\n' "${canonical_case_dir#"$CASES_DIR"/}" >>"$canonical_scope_names"
  done <"$all_cases_file"
  outside_named="$TMPDIR_RUN/cases.named-outside"
  if ! awk 'FILENAME == ARGV[1] { canonical[$0]=1; next }
      !($0 in canonical) { print; bad=1 }
      END { exit bad }' \
      "$canonical_scope_names" "$IMPL_CASE_SET_FILE" >"$outside_named"; then
    printf 'error: --impl-case-set-file contains member outside the canonical pass cohort: %s\n' \
      "$(sed -n '1p' "$outside_named")" >&2
    exit 2
  fi
fi

filters_file="$TMPDIR_RUN/filters"
: >"$filters_file"
for arg in "$@"; do
  printf '%s\n' "$arg" >>"$filters_file"
done
filter_count=$#
requested_filter_count=$filter_count

# A tightly anchored case name has no ERE operators between its anchors, so
# grep can plan every such include in one pass. Keep the original matcher as
# the fallback for all other selectors and for any suspect grep execution.
case_plan_file=$all_cases_file
exact_classifier_stderr="$TMPDIR_RUN/cases.exact-classifier.stderr"
exact_classifier_status=1
if [ "$filter_count" -gt 0 ]; then
  LC_ALL=C awk -v expected="$filter_count" '
    {
      if (length($0) < 3 || substr($0, 1, 1) != "^" ||
          substr($0, length($0), 1) != "$") exit 1
      name = substr($0, 2, length($0) - 2)
      if (name == "" || name !~ "^[A-Za-z0-9_/-]+$") exit 1
    }
    END { if (NR != expected) exit 1 }
  ' "$filters_file" 2>"$exact_classifier_stderr"
  exact_classifier_status=$?
fi
if [ "$exact_classifier_status" -eq 0 ] &&
   [ ! -s "$exact_classifier_stderr" ]
then
  exact_case_names_file="$TMPDIR_RUN/cases.exact-names"
  exact_case_map_file="$TMPDIR_RUN/cases.exact-map.tsv"
  exact_map_stderr="$TMPDIR_RUN/cases.exact-map.stderr"
  exact_map_failed=0
  {
    : >"$exact_case_names_file" && : >"$exact_case_map_file" ||
      exact_map_failed=1
    if [ "$exact_map_failed" -eq 0 ]; then
      while IFS= read -r exact_case_dir; do
        [ -n "$exact_case_dir" ] || continue
        exact_case_name=${exact_case_dir#"$CASES_DIR"/}
        case "$exact_case_name" in *"$TAB"*) continue ;; esac
        if ! printf '%s\n' "$exact_case_name" >>"$exact_case_names_file" ||
           ! printf '%s\t%s\n' "$exact_case_name" "$exact_case_dir" \
             >>"$exact_case_map_file"
        then
          exact_map_failed=1
          break
        fi
      done <"$all_cases_file"
    fi
    [ "$exact_map_failed" -eq 0 ]
  } 2>"$exact_map_stderr"
  exact_map_status=$?

  if [ "$exact_map_status" -eq 0 ] && [ ! -s "$exact_map_stderr" ]
  then
    exact_plan_stderr=
    exact_matches_tmp="$TMPDIR_RUN/cases.exact-matches.tmp"
    exact_grep_stderr="$TMPDIR_RUN/cases.exact-grep.stderr"
    grep -E -f "$filters_file" "$exact_case_names_file" \
      2>"$exact_grep_stderr" >"$exact_matches_tmp"
    exact_grep_status=$?
    if { [ "$exact_grep_status" -eq 0 ] ||
         { [ "$exact_grep_status" -eq 1 ] && [ ! -s "$exact_matches_tmp" ]; }; } &&
       [ ! -s "$exact_grep_stderr" ]
    then
      exact_plan_tmp="$TMPDIR_RUN/cases.exact-plan.tmp"
      exact_plan_stderr="$TMPDIR_RUN/cases.exact-plan.stderr"
      exact_plan_file="$TMPDIR_RUN/cases.exact-plan"
      if awk -F "$TAB" '
           FILENAME == ARGV[1] { selected[$0] = 1; next }
           {
             name = $1
             if (name in selected) {
               print substr($0, length(name) + 2)
             }
           }
         ' "$exact_matches_tmp" "$exact_case_map_file" \
           2>"$exact_plan_stderr" >"$exact_plan_tmp" &&
         [ ! -s "$exact_plan_stderr" ] &&
         mv "$exact_plan_tmp" "$exact_plan_file" \
           2>>"$exact_plan_stderr" &&
         [ ! -s "$exact_plan_stderr" ]
      then
        case_plan_file=$exact_plan_file
        filter_count=0
      else
        rm -f "$exact_plan_tmp" "$exact_plan_file" 2>/dev/null || :
      fi
    fi
    rm -f "$exact_matches_tmp" "$exact_grep_stderr" 2>/dev/null || :
    [ -z "${exact_plan_stderr:-}" ] ||
      rm -f "$exact_plan_stderr" 2>/dev/null || :
  fi
  rm -f "$exact_map_stderr" 2>/dev/null || :
fi
[ "$requested_filter_count" -eq 0 ] ||
  rm -f "$exact_classifier_stderr" 2>/dev/null || :

filtered_cases_file="$TMPDIR_RUN/cases.filtered"
: >"$filtered_cases_file"

while IFS= read -r case_dir; do
  [ -n "$case_dir" ] || continue
  name=${case_dir#"$CASES_DIR"/}

  if [ "$filter_count" -gt 0 ]; then
    matched=0
    while IFS= read -r filter; do
      if printf '%s' "$name" | grep -Eq -- "$filter"; then
        matched=1
        break
      fi
    done <"$filters_file"
    [ "$matched" = 1 ] || continue
  fi

  if [ -s "$excludes_file" ]; then
    excluded=0
    while IFS= read -r exclude; do
      if printf '%s' "$name" | grep -Eq -- "$exclude"; then
        excluded=1
        break
      fi
    done <"$excludes_file"
    [ "$excluded" = 0 ] || continue
  fi

  # --prime-only: skip cases without an IS_KIO_PRIME marker. The marker
  # indicates the case's regular-module sources are Kio'-shaped, so a
  # Kio'-only --impl-def= (e.g. `kio-prime`) can run them; any non-marked
  # case would, by construction, hit a parse-error and pollute the
  # results without telling us anything new. The SKIP_KIO_PRIME_RUN
  # marker is the inverse opt-out: a case may be Kio'-shaped at the
  # source level but exercise full-surface behavior that the Kio'-only
  # implementation deliberately rejects, so we still skip it under
  # prime-only.
  if [ "$PRIME_ONLY" = 1 ]; then
    if [ ! -f "$case_dir/IS_KIO_PRIME" ]; then
      continue
    fi
    if [ -f "$case_dir/SKIP_KIO_PRIME_RUN" ]; then
      continue
    fi
  fi

  # --dyn-load-prime-only: skip cases without a DYN_LOAD_PRIME marker.
  # The
  # marker opts a case into the dyn-load-prime differential runner
  # (kio-test-runner-dyn-load-prime), which loads the case's emitted Kio'
  # image through dyn_load_prime and mirrors the
  # case's compiled-runner route: load only, exported `main`, or scripted
  # exports. It is the explicit, greppable opt-in (parallel to
  # IS_KIO_PRIME + --prime-only): a case declares it when its host calls
  # are within dyn_load_prime's testapi-dyn-load vocabulary, so the
  # interpreter can run it. Non-marked cases are silently skipped by
  # this impl group only; they still run on the regular js / rust impls.
  if [ "$DYN_LOAD_PRIME_ONLY" = 1 ]; then
    if [ ! -f "$case_dir/DYN_LOAD_PRIME" ]; then
      continue
    fi
  fi

  printf '%s\n' "$case_dir" >>"$filtered_cases_file"
done <"$case_plan_file"
filter_count=$requested_filter_count

# Pre-flight: every case carrying a root package file must declare a
# `build { ... }` block (or opt out via NO_BUILD_BLOCK). A
# package-bearing case with neither would match no target in the
# applicability join and be silently skipped on every impl — a
# regression-hiding hole. Fail loudly here rather than quietly drop the
# case from every worklist.
case_modes_file="$TMPDIR_RUN/case-modes.tsv"
if ! assert_case_run_contract "$filtered_cases_file" "$case_modes_file"; then
  exit 1
fi

case_targets_file="$TMPDIR_RUN/case-targets.tsv"
if ! index_case_targets "$filtered_cases_file" "$package_paths_file" \
  "$symlink_package_paths_file" "$generated_markers_file" \
  "$case_targets_file"; then
  printf 'error: cannot index case build targets\n' >&2
  exit 2
fi
if ! assert_standard_case_package_shape \
  "$case_targets_file" "$case_modes_file"; then
  exit 1
fi
if ! assert_every_case_has_build_block "$case_targets_file"; then
  # shellcheck disable=SC2016 # backticks are literal text
  printf 'error: one or more golden cases declare no `build` block; aborting (see messages above)\n' >&2
  exit 1
fi

# Case narrowing decides which cases run their impls this pass. A named set
# selects exact canonical names; --sample-cases=<N> selects a deterministic
# per-bucket subset.
#
# A corpus pass spends nearly all its wall time in the impl tier: per case
# per impl, a `kio build <target>` plus a host-language compile and a run.
# The `# ROUTING: case-binary` tier — fmt idempotence, dependency-tree
# canonicality, the IS_KIO_PRIME biconditional — is pure-compiler and
# cheap. So narrowing scopes the impl tier and nothing else: a narrowed-out
# case still becomes a unit, still gets its `cache clear` and `dep fetch`,
# and still runs every case-binary check its corpus wires. It simply runs no
# impls.
#
# That split is load-bearing. It is what keeps drift a case-binary check
# would catch from hiding in the unsampled tail until its case happens to
# be drawn, so a corpus may be sampled without weakening any invariant the
# case-binary tier asserts. Named sets and sampling both retain every
# KNOWN_FAILING case, whose inverted impl verdict is itself a corpus gate.
#
# The count is per top-level bucket, not per corpus. An exit-code-bucketed
# corpus (test-data/goldens/) concentrates its cost in 00_success, whose
# cases reach a backend; the error buckets stop inside the compiler and
# cost almost nothing. A per-bucket count keeps every cheap bucket whole
# while capping the expensive one. Cases directly under the corpus root
# share the unbucketed bucket; a flat corpus (castles, poc, contrib) is
# entirely that, so the count is just how many of its cases run.
#
# Selection is deterministic given the seed: rank each case by a seeded
# hash of its name, break ties by name, take the lowest-ranked N per
# bucket. Same seed + same corpus => same draw on any machine. The hash is
# computed in awk over integer arithmetic bounded well inside the
# exactly-representable double range, so no libc, locale, or awk
# implementation can move a case; LC_ALL=C pins the sort's collation for
# the same reason.
sampled_names_file="$TMPDIR_RUN/cases.sampled"
: >"$sampled_names_file"
if [ -n "$IMPL_CASE_SET_FILE" ]; then
  while IFS= read -r named_case_dir; do
    [ -n "$named_case_dir" ] || continue
    named_case=${named_case_dir#"$CASES_DIR"/}
    if grep -Fqx "$named_case" "$IMPL_CASE_SET_FILE" ||
       [ -f "$named_case_dir/KNOWN_FAILING" ]; then
      printf '%s\n' "$named_case" >>"$sampled_names_file"
    fi
  done <"$filtered_cases_file"
  LC_ALL=C sort -u -o "$sampled_names_file" "$sampled_names_file"
elif [ -n "$SAMPLE_CASES" ]; then
  ranked_file="$TMPDIR_RUN/cases.ranked"
  CASE_SEED_VALUE=$CASE_SEED LC_ALL=C awk -v cases_dir="$CASES_DIR" '
    BEGIN {
      seed = ENVIRON["CASE_SEED_VALUE"]
      for (i = 32; i < 127; i++) ord[sprintf("%c", i)] = i
    }
    {
      name = substr($0, length(cases_dir) + 2)
      s = seed name
      h = 5381
      n = length(s)
      for (i = 1; i <= n; i++) {
        c = ord[substr(s, i, 1)]
        if (c == "") c = 1
        h = (h * 33 + c) % 2147483647
      }
      bucket = ""
      if (index(name, "/") > 0) {
        bucket = name
        sub(/\/.*$/, "", bucket)
      }
      printf "%s\t%010d\t%s\n", bucket, h, name
    }
  ' "$filtered_cases_file" | LC_ALL=C sort -t"$TAB" -k1,1 -k2,2 -k3,3 >"$ranked_file"

  LC_ALL=C awk -F"$TAB" -v n="$SAMPLE_CASES" '
    $1 != prev { prev = $1; k = 0 }
    { k++; if (k <= n) print $3 }
  ' "$ranked_file" >"$sampled_names_file"

  # A KNOWN_FAILING case pins a tracked bug, and its gate is that it starts
  # FAILING to fail — run_one_case inverts the verdict, so a reproducer whose
  # bug got fixed fails the run and the marker gets cleaned up. That gate
  # lives entirely in the impl tier, and run_one_unit skips the case-binary
  # checks for such a case (it is expected not to build cleanly). A
  # narrowed-out KNOWN_FAILING case would therefore run nothing at all and
  # its marker could go stale unnoticed. Pin them in: they are few, they are
  # cheap, and a corpus that samples them is a corpus whose bug-reproducer
  # gate silently stops working.
  while IFS= read -r pin_case_dir; do
    [ -n "$pin_case_dir" ] || continue
    [ -f "$pin_case_dir/KNOWN_FAILING" ] || continue
    printf '%s\n' "${pin_case_dir#"$CASES_DIR"/}" >>"$sampled_names_file"
  done <"$filtered_cases_file"
  LC_ALL=C sort -u "$sampled_names_file" -o "$sampled_names_file"

  sampled_total=$(wc -l <"$filtered_cases_file" | tr -d ' ')
  sampled_kept=$(wc -l <"$sampled_names_file" | tr -d ' ')
  printf 'sample-cases: seed = %s\n' "$CASE_SEED"
  printf 'sample-cases: cap = %s case(s) per bucket\n' "$SAMPLE_CASES"
  printf 'sample-cases: %s of %s case(s) run their impls; the other %s run case-binary checks only\n' \
    "$sampled_kept" "$sampled_total" "$((sampled_total - sampled_kept))"
  printf 'sample-cases: reproduce this draw with --sample-cases=%s --case-seed=%s\n' \
    "$SAMPLE_CASES" "$CASE_SEED"
  # Counted from the selection itself, not from min(total, cap): a bucket
  # holding a pinned KNOWN_FAILING case can exceed the cap.
  printf 'sample-cases: per bucket (impl-run/total):\n'
  LC_ALL=C awk -F"$TAB" '
    NR == FNR { selected[$0] = 1; next }
    {
      total[$1]++
      if ($3 in selected) kept[$1]++
    }
    END {
      for (b in total) {
        printf "  %s\t%d/%d\n", (b == "" ? "(unbucketed)" : b), kept[b] + 0, total[b]
      }
    }
  ' "$sampled_names_file" "$ranked_file" | LC_ALL=C sort
  printf 'sample-cases: selected (impl runs):\n'
  while IFS= read -r sampled_name; do
    [ -n "$sampled_name" ] || continue
    printf '  %s\n' "$sampled_name"
  done <"$sampled_names_file"
  printf '\n'
fi

# Join cases to configured implementations once. Each output row is:
#   <case-order><TAB><impl-order><TAB><case-dir><TAB><impl-name><TAB>
#   <kio><TAB><runner><TAB><target><TAB><prime-kio><TAB><runner-cache-kind><TAB>
#   <package-name-or-*>
#
# This is the only place that applies target eligibility and the
# compile-only/custom-run exclusion. Sampling and unit construction consume
# this canonical relation instead of re-probing package files.
applicability_rows_file="$TMPDIR_RUN/applicability-rows.tsv"
LC_ALL=C awk -F"$TAB" \
  -v targets_file="$case_targets_file" \
  -v modes_file="$case_modes_file" \
  -v empty_impl_field="$IMPL_EMPTY" \
  -v impls_file="$resolved_impls_file" '
  FILENAME == targets_file {
    case_dir = $1
    cases[++case_count] = case_dir
    target_list = $2
    package_name[case_dir] = $3
    if (target_list == "*") {
      target_agnostic[case_dir] = 1
    } else {
      n = split(target_list, targets, " ")
      for (i = 1; i <= n; i++) {
        if (targets[i] != "") supports[case_dir, targets[i]] = 1
      }
    }
    next
  }
  FILENAME == modes_file {
    mode[$1] = $2
    next
  }
  FILENAME == impls_file {
    impl_count++
    impl_name[impl_count] = $1
    impl_kio[impl_count] = $2
    impl_runner[impl_count] = $3
    impl_target[impl_count] = $4
    impl_cache_kind[impl_count] = ($5 == empty_impl_field ? "" : $5)
    impl_prime[impl_count] = ($6 == empty_impl_field ? "" : $6)
    next
  }
  END {
    for (case_index = 1; case_index <= case_count; case_index++) {
      case_dir = cases[case_index]
      for (impl_index = 1; impl_index <= impl_count; impl_index++) {
        target = impl_target[impl_index]
        if (!(case_dir in target_agnostic) && !((case_dir SUBSEP target) in supports)) {
          continue
        }
        if (impl_runner[impl_index] == "SKIP" &&
            mode[case_dir] != "run.args" && mode[case_dir] != "run.test-only") {
          continue
        }
        printf "%d\t%d\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n",
          case_index, impl_index, case_dir, impl_name[impl_index],
          impl_kio[impl_index], impl_runner[impl_index], target,
          impl_prime[impl_index], impl_cache_kind[impl_index], package_name[case_dir]
      }
    }
  }
' "$case_targets_file" "$case_modes_file" "$resolved_impls_file" \
  >"$applicability_rows_file" || {
    printf 'error: cannot construct case/implementation applicability\n' >&2
    exit 2
  }

# --impls=SAMPLE_IMPL: for each case, pick exactly one applicable impl at
# random for an ordinary run. The worklist build below uses this pick to trim
# each case's canonical applicability rows down to the selected implementation.
#
# Cases applicable to only one configured impl are routed there
# regardless; sampling is a no-op for them.
sample_impl_file="$TMPDIR_RUN/sample_impl.tsv"
: >"$sample_impl_file"
if [ "$SAMPLE_IMPL" = 1 ]; then
  if [ "${KIO_DEBUG_SAMPLE_IMPL_SEED+x}" = x ]; then
    printf 'impls=SAMPLE_IMPL mode: deterministically picking one applicable impl per case\n'
  else
    printf 'impls=SAMPLE_IMPL mode: picking one applicable impl per case at random\n'
  fi
  applicability_file="$TMPDIR_RUN/applicability.tsv"
  awk -F"$TAB" '
    function flush() {
      if (case_dir != "") print case_dir "\t" names
    }
    {
      if ($3 != case_dir) {
        flush()
        case_dir = $3
        names = ""
      }
      names = names (names == "" ? "" : " ") $4
    }
    END { flush() }
  ' "$applicability_rows_file" >"$applicability_file" || {
    printf 'error: cannot construct SAMPLE_IMPL candidate pool\n' >&2
    exit 2
  }

  if [ "${KIO_DEBUG_SAMPLE_IMPL_SEED+x}" = x ]; then
    sample_impl_phase=regular
    [ "$PRIME_ONLY" -eq 0 ] || sample_impl_phase=direct-prime
    [ "$DYN_LOAD_PRIME_ONLY" -eq 0 ] || sample_impl_phase=dyn-load-prime
    sample_impl_scope=${KIO_DEBUG_SAMPLE_IMPL_SCOPE:-direct}
    case "$sample_impl_scope" in
      checks/orchestrators/golden-tests) sample_impl_corpus=goldens ;;
      checks/orchestrators/emissions-tests) sample_impl_corpus=emissions ;;
      checks/orchestrators/generative-tests) sample_impl_corpus=generative ;;
      checks/orchestrators/poc-tests) sample_impl_corpus=poc ;;
      checks/orchestrators/castle-tests) sample_impl_corpus=castles ;;
      checks/orchestrators/contrib-tests) sample_impl_corpus=contrib ;;
      direct)
        sample_impl_corpus=${CASES_DIR%/}
        sample_impl_corpus=${sample_impl_corpus##*/}
        ;;
      *)
        printf 'error: invalid internal SAMPLE_IMPL scope: %s\n' \
          "$sample_impl_scope" >&2
        exit 2
        ;;
    esac

    KIO_DEBUG_SAMPLE_IMPL_SEED_VALUE=$KIO_DEBUG_SAMPLE_IMPL_SEED LC_ALL=C \
      awk -F'\t' -v cases_dir="$CASES_DIR" \
        -v sample_scope="$sample_impl_scope" \
        -v sample_phase="$sample_impl_phase" \
        -v sample_corpus="$sample_impl_corpus" '
        BEGIN {
          seed = ENVIRON["KIO_DEBUG_SAMPLE_IMPL_SEED_VALUE"]
          for (i = 32; i < 127; i++) ord[sprintf("%c", i)] = i
        }
        {
          name = substr($1, length(cases_dir) + 2)
          s = sample_scope SUBSEP sample_phase SUBSEP sample_corpus SUBSEP seed SUBSEP name
          h = 5381
          for (i = 1; i <= length(s); i++) {
            c = ord[substr(s, i, 1)]
            if (c == "") c = 1
            h = (h * 33 + c) % 2147483647
          }
          n = split($2, impls, " ")
          if (n > 0) print $1 "\t" impls[(h % n) + 1]
        }
      ' "$applicability_file" >"$sample_impl_file" || {
        printf 'error: cannot select seeded SAMPLE_IMPL candidates\n' >&2
        exit 2
      }

    while IFS="$TAB" read -r sample_impl_case sample_impl_name; do
      [ -n "$sample_impl_case" ] || continue
      sample_impl_relative=${sample_impl_case#"$CASES_DIR"/}
      printf 'sample-impl-map:\t%s/%s\t%s\t%s\t%s\n' \
        "$sample_impl_scope" "$sample_impl_phase" "$sample_impl_corpus" \
        "$sample_impl_relative" "$sample_impl_name"
    done <"$sample_impl_file"
  else
    # Single awk run so srand() seeds once and rand() draws are
    # independent across cases. The seed comes from /dev/urandom
    # rather than awk's default time(0); back-to-back invocations
    # within the same second would otherwise pick identically.
    sample_impl_seed=$(od -An -N4 -tu4 /dev/urandom 2>/dev/null | tr -d ' \n')
    : "${sample_impl_seed:=$$}"  # fallback if /dev/urandom is unavailable
    awk -F'\t' -v seed="$sample_impl_seed" 'BEGIN { srand(seed) }
      { n = split($2, impls, " ")
        if (n > 0) {
          idx = int(rand() * n) + 1
          print $1 "\t" impls[idx]
        }
      }' "$applicability_file" >"$sample_impl_file" || {
        printf 'error: cannot select SAMPLE_IMPL candidates\n' >&2
        exit 2
      }
  fi
fi

# Apply the implementation pick in one join. Keeping the untrimmed canonical
# rows above is important: their case-major, configured-implementation order
# is the input contract for SAMPLE_IMPL's random draw.
selected_applicability_rows_file="$TMPDIR_RUN/applicability-selected.tsv"
if [ "$SAMPLE_IMPL" = 1 ]; then
  awk -F"$TAB" -v picks_file="$sample_impl_file" '
    FILENAME == picks_file {
      pick[$1] = $2
      next
    }
    ($3 in pick) && $4 == pick[$3] { print }
  ' "$sample_impl_file" "$applicability_rows_file" \
    >"$selected_applicability_rows_file" || {
      printf 'error: cannot apply SAMPLE_IMPL selection\n' >&2
      exit 2
    }
else
  awk '{ print }' "$applicability_rows_file" \
    >"$selected_applicability_rows_file" || {
      printf 'error: cannot retain implementation matrix\n' >&2
      exit 2
    }
fi

# Build the (case, binary) worklist.
#
# Each row in $UNITS_FILE has the shape
#   <unit_key>\t<case_dir>\t<binary_kio>\t<impls_blob>\t<package-name-or-*>
# where <unit_key> = <safe_case>__<safe_binary> and <impls_blob> is a
# `;`-joined list of
# `<impl_name>|<runner>|<target>|<prime-kio>|<runner-cache-kind>` tuples. The
# worker uses unit_key to look the row up; tabs and spaces are
# avoided inside the impls blob so xargs / shell quoting can't
# reshape the payload.
#
# A case's applicable impls (target-gated; further trimmed by
# --impls=SAMPLE_IMPL if active) are partitioned by their kio_binary_path.
# Each distinct binary on the case becomes one unit row; the impls
# on that binary form its impls_blob.
units_file="$TMPDIR_RUN/units.tsv"
: >"$units_file"
keys_file="$TMPDIR_RUN/units.keys"
: >"$keys_file"
dispatch_keys_file="$TMPDIR_RUN/units.dispatch.keys"
: >"$dispatch_keys_file"

# Normalize the selected applicability rows into the historical ordering:
# filtered-case order, then lexical binary path, then configured impl order.
# The last field marks case-narrowed rows (sampled or outside a named set)
# whose unit must retain its case-binary checks while carrying no
# implementation tuple.
unit_parts_file="$TMPDIR_RUN/unit-parts.tsv"
unit_parts_unsorted_file="$TMPDIR_RUN/unit-parts.unsorted.tsv"
case_sampling=0
[ -n "$SAMPLE_CASES$IMPL_CASE_SET_FILE" ] && case_sampling=1
awk -F"$TAB" \
  -v sampled_file="$sampled_names_file" \
  -v sampling="$case_sampling" \
  -v cases_dir="$CASES_DIR" '
  FILENAME == sampled_file {
    sampled[$0] = 1
    next
  }
  {
    name = substr($3, length(cases_dir) + 2)
    sampled_out = sampling && !(name in sampled)
    printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%d\n",
      $1, $3, $5, $2, $4, $6, $7, $8, $9, $10, sampled_out
  }
' "$sampled_names_file" "$selected_applicability_rows_file" \
  >"$unit_parts_unsorted_file" || {
    printf 'error: cannot construct worklist unit parts\n' >&2
    exit 2
  }
sort -t"$TAB" -k1,1n -k3,3 -k4,4n "$unit_parts_unsorted_file" \
  >"$unit_parts_file" || {
    printf 'error: cannot order worklist unit parts\n' >&2
    exit 2
  }

awk -F"$TAB" -v cases_dir="$CASES_DIR" '
  function flush() {
    if (!have_unit) return
    name = substr(case_dir, length(cases_dir) + 2)
    safe_case = name
    gsub(/\//, "_", safe_case)
    safe_bin = binary
    sub(/^.*\//, "", safe_bin)
    gsub(/[^[:alnum:]._]/, "_", safe_bin)
    # The historical `basename | tr -c ... _` maps basename'\''s trailing
    # newline to `_`; preserve the resulting key spelling exactly.
    safe_bin = safe_bin "_"
    unit_key = safe_case "__" safe_bin
    print unit_key "\t" case_dir "\t" binary "\t" blob "\t" package_name
  }
  {
    if (!have_unit || $2 != case_dir || $3 != binary) {
      flush()
      have_unit = 1
      case_dir = $2
      binary = $3
      package_name = $10
      sampled_out = $11
      blob = ""
    }
    if (!sampled_out) {
      tuple = $5 "|" $6 "|" $7 "|" $8 "|" $9
      blob = blob (blob == "" ? "" : ";") tuple
    }
  }
  END { flush() }
' "$unit_parts_file" >"$units_file" || {
  printf 'error: cannot assemble worklist units\n' >&2
  exit 2
}
awk -F"$TAB" '{ print $1 }' "$units_file" >"$keys_file" || {
  printf 'error: cannot index worklist units\n' >&2
  exit 2
}

# Preserve the canonical worklist for execution with one worker and for every
# report, summary, and diagnostic. The parallel path may instead dispatch an
# internal RUN_EARLY tier first; stable source order breaks ties within each
# tier. The marker cannot add, remove, or reorder reported results.
dispatch_rows_file="$TMPDIR_RUN/units.dispatch.tsv"
: >"$dispatch_rows_file"
dispatch_index=0
while IFS=$TAB read -r dispatch_key dispatch_case_dir _; do
  [ -n "$dispatch_key" ] || continue
  dispatch_index=$((dispatch_index + 1))
  dispatch_tier=1
  [ ! -f "$dispatch_case_dir/RUN_EARLY" ] || dispatch_tier=0
  printf '%s\t%s\t%s\n' "$dispatch_tier" "$dispatch_index" "$dispatch_key" \
    >>"$dispatch_rows_file"
done <"$units_file"
LC_ALL=C sort -t"$TAB" -k1,1n -k2,2n "$dispatch_rows_file" \
  | awk -F"$TAB" '{ print $3 }' >"$dispatch_keys_file" || {
    printf 'error: cannot construct dispatch order\n' >&2
    exit 2
  }

unit_count=$(wc -l <"$units_file" | tr -d ' ')
impl_unit_keys_file="$TMPDIR_RUN/units.impl.keys"
awk -F"$TAB" '$4 != "" { print $1 }' "$units_file" >"$impl_unit_keys_file"
impl_unit_count=$(wc -l <"$impl_unit_keys_file" | tr -d ' ')
checks_only_unit_keys_file="$TMPDIR_RUN/units.checks-only.keys"
awk -F"$TAB" '$4 == "" { print $1 }' "$units_file" \
  >"$checks_only_unit_keys_file"
checks_only_unit_count=$(wc -l <"$checks_only_unit_keys_file" | tr -d ' ')

# Resolve --jobs auto and apply auto-disable rules.
if [ "$JOBS" = "auto" ]; then
  JOBS=$(sh "$RUN_TESTS_CI_DIR/schedule.sh" --available-parallelism) || exit $?
fi

if [ "$JOBS" -gt 1 ] && [ "$SHOW_OUTPUT" = 1 ]; then
  printf '(note: --show-output forces serial execution)\n' >&2
  JOBS=1
fi
if [ "$JOBS" -gt 1 ] && [ "$UPDATE" = 1 ]; then
  printf '(note: --update-expected forces serial execution)\n' >&2
  JOBS=1
fi
if [ "$JOBS" -gt 1 ] && ! echo '' | xargs -P 1 -I {} true 2>/dev/null; then
  printf '(note: xargs -P unavailable; falling back to serial execution)\n' >&2
  JOBS=1
fi

# A standalone run, or one with an explicit local override, publishes its
# resolved work capacity before any worker enters the Git-common scheduler.
# A nested corpus run without --jobs keeps the outer ci/all.sh capacity:
# replacing it with this process's auto capacity would silently defeat the
# broad gate's shared limit. Forced serial modes narrow standalone and
# explicitly overridden runs; a one-unit worklist does not change the shared
# capacity merely because its local dispatch is necessarily serial.
if [ "$JOBS_SET" = 1 ] || [ -z "${KIO_CI_SCHEDULE_JOBS:-}" ]; then
  KIO_CI_SCHEDULE_JOBS=$JOBS
  export KIO_CI_SCHEDULE_JOBS
fi

if [ "$JOBS" -gt 1 ] && [ "$unit_count" -le 1 ]; then
  JOBS=1
fi

if [ "$JOBS" -gt 1 ]; then
  printf 'jobs: %s parallel\n\n' "$JOBS"
fi

# Run cases.
results_file="$TMPDIR_RUN/results"
: >"$results_file"

if [ "$unit_count" = 0 ]; then
  if [ "$filter_count" -gt 0 ] || [ -s "$excludes_file" ]; then
    printf 'no cases matched the filter(s)\n'
  else
    printf 'no cases found under %s\n' "$CASES_DIR"
  fi
fi

# Record per-impl applicable counts (used by the summary's
# "produced no result" arithmetic). For each impl, count the
# applicable cases — i.e. rows in $units_file whose impls_blob
# contains this impl's name.
: >"$TMPDIR_RUN/applicable.tsv"
while IFS=$TAB read -r impl_name _ _ _ _ _; do
  [ -n "$impl_name" ] || continue
  applicable_count=$(awk -F"$TAB" -v i="$impl_name" '
    {
      n = split($4, tuples, ";")
      for (j = 1; j <= n; j++) {
        split(tuples[j], rec, "|")
        if (rec[1] == i) { print; next }
      }
    }
  ' "$units_file" | wc -l | tr -d ' ')
  printf '%s\t%s\n' "$impl_name" "$applicable_count" \
    >>"$TMPDIR_RUN/applicable.tsv"
done <"$resolved_impls_file"

# Globals consumed by run_one_unit / worker.
UNITS_FILE=$units_file
RESULTS_FILE=$results_file
UNIT_SCRATCH_BASE="$TMPDIR_RUN/scratch"
mkdir -p "$UNIT_SCRATCH_BASE"
CASE_BINARY_CHECKS_FILE=$cb_checks_file
IMPL_CHECKS_FILE=$impl_checks_file
CLEAR_BINARY_FILE=$clear_binary_file
UPDATE_FLAG=$UPDATE
SHOW_OUTPUT_FLAG=$SHOW_OUTPUT

buffer_dir="$TMPDIR_RUN/buffers"
mkdir -p "$buffer_dir"

# A unit's buffer file is created when the unit STARTS (the worker's
# output redirect); its done file is written when the unit FINISHES. The
# ticker below reads the difference to tell running from queued from done.
done_dir="$TMPDIR_RUN/done"
mkdir -p "$done_dir"

export KIO_TEST_WORKER_RESULTS_FILE="$results_file"
export KIO_TEST_WORKER_SCRATCH_BASE="$UNIT_SCRATCH_BASE"
export KIO_TEST_WORKER_BUFFER_DIR="$buffer_dir"
export KIO_TEST_WORKER_DONE_DIR="$done_dir"
export KIO_TEST_WORKER_CASES_DIR="$CASES_DIR"
export KIO_TEST_WORKER_CACHE_BASE="$CACHE_BASE"
export KIO_TEST_WORKER_KEEP_CACHE="$KEEP_CACHE"
export KIO_TEST_WORKER_UNITS_FILE="$units_file"
export KIO_TEST_WORKER_CLEAR_BINARY_FILE="$clear_binary_file"
export KIO_TEST_WORKER_CB_CHECKS_FILE="$cb_checks_file"
export KIO_TEST_WORKER_IMPL_CHECKS_FILE="$impl_checks_file"
export KIO_TEST_WORKER_COMPILER_PROXY_DIR="$COMPILER_PROXY_DIR"
export KIO_TEST_WORKER_UPDATE="$UPDATE"
export KIO_TEST_WORKER_SHOW_OUTPUT="$SHOW_OUTPUT"
export KIO_TEST_WORKER_CUSTOM_TYPED_CACHE_COHORT_FILE="$CUSTOM_TYPED_CACHE_COHORT_FILE"
export KIO_TEST_WORKER_CUSTOM_TYPED_CACHE_ROOT="$CUSTOM_TYPED_CACHE_ROOT"
export KIO_TEST_WORKER_CUSTOM_TYPED_CACHE_SCRIPTS_ROOT="$CUSTOM_TYPED_CACHE_SCRIPTS_ROOT"
KIO_TEST_WORKER_STREAM=0
if [ "$SHOW_OUTPUT" = 1 ] || [ "$UPDATE" = 1 ]; then
  KIO_TEST_WORKER_STREAM=1
fi
export KIO_TEST_WORKER_STREAM

# Progress heartbeat.
#
# A corpus task runs for tens of minutes, and ci/all.sh buffers a task's
# stdout until the task ends — so between `TASK start` and `TASK pass` a
# healthy run and a wedged one look identical: silence. The heartbeat goes
# out on the live-progress channel (the one the FAIL markers already use),
# which reaches ci/all.sh's stream instead of the buffered log.
#
# It ticks on a timer rather than printing on each unit completion,
# because a wedged run completes nothing: a completion-triggered line
# would fall silent exactly when the stall it would reveal is happening.
# On a tick where nothing finished, the ticker says so and names the
# in-flight units — the `0.0s cpu` hang diagnosis that ai/topics/local-ci.md
# otherwise asks a reader to perform by hand, delivered live.
#
# `running` and `queued` are reported separately because a unit can sit
# waiting on a ci/schedule.sh slot rather than being stuck; without that
# split, normal scheduler contention would read as a hang.
#
# Skipped for a single unit (nothing to summarize) and under --show-output
# / --update-expected, which are interactive and already stream per-unit.
PROGRESS_INTERVAL=${KIO_DEBUG_PROGRESS_INTERVAL:-300}
case "$PROGRESS_INTERVAL" in
  ''|*[!0-9]*)
    printf 'error: KIO_DEBUG_PROGRESS_INTERVAL must be a non-negative integer (got %s)\n' \
      "$PROGRESS_INTERVAL" >&2
    exit 2
    ;;
esac
run_start_epoch=$(date +%s)

# Name the cases behind the units that started but have not finished, at
# most $1 of them, space-separated.
inflight_case_names() {
  icn_limit=$1
  icn_shown=0
  for icn_buffer in "$buffer_dir"/*; do
    [ -f "$icn_buffer" ] || continue
    icn_key=${icn_buffer##*/}
    [ -f "$done_dir/$icn_key" ] && continue
    icn_case=$(awk -F"$TAB" -v k="$icn_key" '$1==k {print $2; exit}' "$units_file")
    [ -n "$icn_case" ] || continue
    printf '%s ' "${icn_case#"$CASES_DIR"/}"
    icn_shown=$((icn_shown + 1))
    [ "$icn_shown" -ge "$icn_limit" ] && { printf '... '; break; }
  done
}

completed_unit_count() {
  cuc_keys_file=$1
  cuc_done=0
  while IFS= read -r cuc_key; do
    [ -n "$cuc_key" ] || continue
    if [ -f "$done_dir/$cuc_key" ]; then
      cuc_done=$((cuc_done + 1))
    fi
  done <"$cuc_keys_file"
  printf '%s\n' "$cuc_done"
}

progress_ticker_pid=
if [ "$unit_count" -gt 1 ] && [ "$PROGRESS_INTERVAL" -gt 0 ] \
   && [ "$SHOW_OUTPUT" != 1 ] && [ "$UPDATE" != 1 ]; then
  (
    # Starts at 0, not -1: a run that wedges before completing anything must
    # report on its FIRST tick. Seeding it below any reachable count would
    # make the first tick compare unequal and defer the wedge line to 2x the
    # interval — silence in exactly the case the heartbeat exists to cover.
    tick_prev_done=0
    while :; do
      # Sleep in one-second steps. A single `sleep $PROGRESS_INTERVAL` is a
      # foreground child of this subshell: killing the subshell does not kill
      # it, so it is reparented to init and keeps every inherited descriptor
      # open — stdout, stderr, and under ci/all.sh the progress fd. A piped
      # invocation (`... 2>&1 | tail`, which ai/topics/local-ci.md teaches)
      # would then block on the pipe's write end for the rest of the interval
      # after the run had already finished: a run that looks wedged when it
      # is done, which is the failure this heartbeat exists to prevent.
      # Stepping bounds any orphan to one second.
      tick_waited=0
      while [ "$tick_waited" -lt "$PROGRESS_INTERVAL" ]; do
        sleep 1
        tick_waited=$((tick_waited + 1))
      done
      if [ -n "$SAMPLE_CASES$IMPL_CASE_SET_FILE" ]; then
        # Count the disjoint tiers directly so a completion between reads
        # cannot be attributed to the wrong tier.
        tick_impl_done=$(completed_unit_count "$impl_unit_keys_file")
        tick_checks_done=$(completed_unit_count "$checks_only_unit_keys_file")
        tick_done=$((tick_impl_done + tick_checks_done))
      else
        tick_done=$(find "$done_dir" -mindepth 1 -maxdepth 1 -type f 2>/dev/null | wc -l | tr -d ' ')
      fi
      # A done marker implies its persistent buffer marker. Snapshot started
      # after done so a fast newly dispatched unit cannot make running negative.
      tick_started=$(find "$buffer_dir" -mindepth 1 -maxdepth 1 -type f 2>/dev/null | wc -l | tr -d ' ')
      tick_pass=$(grep -c "${TAB}PASS${TAB}" "$results_file" 2>/dev/null)
      tick_fail=$(grep -c "${TAB}FAIL${TAB}" "$results_file" 2>/dev/null)
      tick_elapsed=$(( $(date +%s) - run_start_epoch ))
      # The failure count is appended ONLY when it is non-zero, so the word
      # never appears on a healthy run's heartbeat. This stream is stdout
      # under ci/all.sh, i.e. what a reader (or an agent) greps for failure
      # markers; a line reading "0 failed" every interval would make every
      # such grep a false positive on a green run. A hit here is real.
      #
      # The counts are result ROWS, not units — a unit contributes one row
      # per impl plus one per case-binary tag — so they are labelled as rows
      # and do not sum to `units done`.
      if [ -n "$SAMPLE_CASES$IMPL_CASE_SET_FILE" ]; then
        tick_line=$(printf 'progress: %s/%s impl-run units done; %s/%s checks-only units done; %s/%s total units done, %s running, %s queued; %s passing rows; %dm elapsed' \
          "$tick_impl_done" "$impl_unit_count" \
          "$tick_checks_done" "$checks_only_unit_count" \
          "$tick_done" "$unit_count" \
          "$((tick_started - tick_done))" "$((unit_count - tick_started))" \
          "${tick_pass:-0}" "$((tick_elapsed / 60))")
      else
        tick_line=$(printf 'progress: %s/%s units done, %s running, %s queued; %s passing rows; %dm elapsed' \
          "$tick_done" "$unit_count" "$((tick_started - tick_done))" \
          "$((unit_count - tick_started))" "${tick_pass:-0}" \
          "$((tick_elapsed / 60))")
      fi
      if [ "${tick_fail:-0}" -gt 0 ]; then
        tick_line=$(printf '%s; %s FAILING rows' "$tick_line" "$tick_fail")
      fi
      emit_live_progress "$tick_line"
      # Nothing finished this interval. Distinguishing a wedge from slow
      # work is the reader's job, but naming the in-flight cases is ours.
      if [ "$tick_done" = "$tick_prev_done" ] && [ "$tick_started" -gt "$tick_done" ]; then
        emit_live_progress "$(printf 'progress: no unit completed in the last %ss; in flight: %s' \
          "$PROGRESS_INTERVAL" "$(inflight_case_names 6)")"
      fi
      tick_prev_done=$tick_done
    done
  ) &
  progress_ticker_pid=$!
fi

stop_progress_ticker() {
  if [ -n "${progress_ticker_pid:-}" ]; then
    kill "$progress_ticker_pid" 2>/dev/null || true
    wait "$progress_ticker_pid" 2>/dev/null || true
    progress_ticker_pid=
  fi
}

if [ "$unit_count" -gt 0 ]; then
  if [ "$JOBS" -le 1 ]; then
    # With one worker, priority dispatch cannot shorten the run. Preserve the
    # historical canonical execution and report order instead of maintaining
    # a second serial-only scheduling lifecycle. Materialize the keys into
    # shell state so a worker keeps the harness caller's stdin rather than
    # inheriting a loop's file descriptor.
    serial_keys=$(cat "$keys_file")
    serial_keys_ifs=$IFS
    IFS='
'
    serial_restore_glob=0
    case $- in
      *f*) ;;
      *) set -f; serial_restore_glob=1 ;;
    esac
    # Unit keys contain no newlines. Newline-only splitting plus disabled
    # pathname expansion preserves every key exactly.
    # shellcheck disable=SC2086
    set -- $serial_keys
    [ "$serial_restore_glob" -eq 0 ] || set +f
    IFS=$serial_keys_ifs
    for unit_key do
      sh "$SCRIPT_PATH" --__worker "$unit_key" || true
      if [ "$KIO_TEST_WORKER_STREAM" != 1 ]; then
        buffer="$buffer_dir/$unit_key"
        [ -f "$buffer" ] && cat "$buffer"
      fi
    done
  else
    # Parallel path. The worklist is one unit_key per line — no
    # in-band whitespace, no tab-separated triples — so macOS BSD
    # `xargs -I {}` (which normalizes tabs to spaces in the
    # substitution) can't mangle the payload. The worker looks the
    # full row up in $UNITS_FILE on its own.
    xargs -P "$JOBS" -I {} sh "$SCRIPT_PATH" --__worker {} <"$dispatch_keys_file"

    while IFS= read -r unit_key; do
      [ -n "$unit_key" ] || continue
      buffer="$buffer_dir/$unit_key"
      [ -f "$buffer" ] && cat "$buffer"
    done <"$keys_file"
  fi
fi

# Every unit is finished; a further heartbeat would only race the summary.
stop_progress_ticker

# Per-impl summary.
printf '\nsummary:\n'
overall_fail=0

# Every dispatched unit must have finished. The per-impl arithmetic below
# catches a worker that vanished (OOM, an xargs quirk) only for units that
# carry impl rows — it compares recorded results against applicable cases.
# A case-narrowed unit carries no impl rows at all: it is applicable to no
# impl, so it is in no impl's denominator, and its `<tag>@<binary>` rows are
# counted but never compared against an expected count. Without this check
# a worker that died on such a unit would take
# its case-binary checks — fmt idempotence, dependency-tree canonicality,
# the IS_KIO_PRIME biconditional — down with it silently, and the run would
# still exit 0. Under a sampled corpus that is most of the units, i.e.
# precisely the tier the sampling design leans on.
#
# The worker writes its done marker after run_one_unit returns, whatever
# the unit's status, so a shortfall means a worker never reached the end —
# a harness fault, not a failing case.
if [ "$unit_count" -gt 0 ]; then
  units_done=$(find "$done_dir" -mindepth 1 -maxdepth 1 -type f 2>/dev/null | wc -l | tr -d ' ')
  if [ "$units_done" -lt "$unit_count" ]; then
    printf '  !! %d of %d units produced no completion marker (finished=%d, expected=%d) — a worker died without recording a result; bug in the test harness, not in the cases\n' \
      $((unit_count - units_done)) "$unit_count" "$units_done" "$unit_count" >&2
    overall_fail=1
  fi
fi
while IFS=$TAB read -r impl_name _impl_kio _impl_runner _impl_target _impl_cache_kind _impl_prime; do
  [ -n "$impl_name" ] || continue
  impl_pass=$(awk -F"$TAB" -v i="$impl_name" '$1==i && $2=="PASS"' "$results_file" | wc -l)
  impl_fail=$(awk -F"$TAB" -v i="$impl_name" '$1==i && $2=="FAIL"' "$results_file" | wc -l)
  # WARN rows are KNOWN_FAILING cases whose tracked bug still reproduces
  # (see run_one_case): expected failures that don't fail the gate but
  # are surfaced so they aren't forgotten.
  impl_warn=$(awk -F"$TAB" -v i="$impl_name" '$1==i && $2=="WARN"' "$results_file" | wc -l)
  if [ "$impl_warn" -gt 0 ]; then
    printf '  %s: %d passed, %d failed, %d known-failing\n' "$impl_name" "$impl_pass" "$impl_fail" "$impl_warn"
  else
    printf '  %s: %d passed, %d failed\n' "$impl_name" "$impl_pass" "$impl_fail"
  fi
  if [ "$impl_fail" -gt 0 ]; then
    overall_fail=1
  fi
  # A successful run must report one PASS or FAIL per applicable case
  # for this impl — i.e. per case whose root build block declared this
  # impl's target (or which has no build block at all). If passes+fails
  # fall short, some workers exited without recording a result —
  # typically a host-shell or xargs quirk in the parallel path that
  # we'd otherwise miss because no case explicitly FAILed. Surface it
  # as a hard failure rather than letting the summary read like a
  # clean run.
  impl_recorded=$((impl_pass + impl_fail + impl_warn))
  impl_applicable=$(awk -F"$TAB" -v i="$impl_name" '$1==i {print $2; exit}' \
    "$TMPDIR_RUN/applicable.tsv" 2>/dev/null)
  : "${impl_applicable:=0}"
  if [ "$impl_recorded" -lt "$impl_applicable" ]; then
    printf '  !! %s: %d of %d cases produced no result (passes+fails=%d, expected=%d) — bug in the test harness, not in the cases\n' \
      "$impl_name" \
      $((impl_applicable - impl_recorded)) "$impl_applicable" \
      "$impl_recorded" "$impl_applicable" >&2
    overall_fail=1
  fi
done <"$resolved_impls_file"

# Roll every non-impl row into the summary so its failures land in the
# overall fail tally. The population is defined by complement — any row
# whose first field is not one of this run's configured impl names — and
# not by matching $synth_tag_pattern. Pattern-matching would tally only
# the tags that some wired `# ROUTING: case-binary` check declared, and
# silently drop a FAIL row bearing any other tag: the harness writes one
# such row itself when a case-narrowed unit's `dep fetch` fails with no
# case-binary check configured (run_one_unit's fallback tag). A FAIL that
# no tally reads is a FAIL the exit code never sees, so the complement is
# the safe direction — a row we did not anticipate gets counted, rather
# than dropped on the floor.
awk -F"$TAB" '
  NR == FNR { impl[$1] = 1; next }
  NF && !($1 in impl) { print $1 }
' "$resolved_impls_file" "$results_file" 2>/dev/null \
  | sort -u >"$TMPDIR_RUN/cb_impls"
while IFS= read -r cb_name; do
  [ -n "$cb_name" ] || continue
  cb_pass=$(awk -F"$TAB" -v i="$cb_name" '$1==i && $2=="PASS"' "$results_file" | wc -l)
  cb_fail=$(awk -F"$TAB" -v i="$cb_name" '$1==i && $2=="FAIL"' "$results_file" | wc -l)
  printf '  %s: %d passed, %d failed\n' "$cb_name" "$cb_pass" "$cb_fail"
  if [ "$cb_fail" -gt 0 ]; then
    overall_fail=1
  fi
done <"$TMPDIR_RUN/cb_impls"

# Divergent-cases table, only when 2+ impls and there's heterogeneity.
# The table is built from the case-run outcome rows in $results_file; a
# row that is not an impl's is excluded so it can't spuriously populate a
# divergent-case column. Non-impl rows are identified the same way the
# rollup above identifies them — by complement against the configured impl
# names, not by matching the tags some wired check declared — so a row
# bearing an unanticipated tag is excluded here and counted there, rather
# than the two sites disagreeing about what a non-impl row is.
impl_count=$(wc -l <"$resolved_impls_file")
if [ "$impl_count" -ge 2 ]; then
  divergent_file="$TMPDIR_RUN/divergent"
  awk -F"$TAB" '
    NR == FNR { impl[$1] = 1; next }
    NF && ($1 in impl) { print $3 "\t" $2 }
  ' "$resolved_impls_file" "$results_file" \
    | sort -u \
    | awk -F"$TAB" '{ count[$1]++ } END { for (c in count) if (count[c] > 1) print c }' \
    | sort >"$divergent_file"

  if [ -s "$divergent_file" ]; then
    printf '\ndivergent cases:\n'
    # Header
    printf '  %-50s' 'case'
    while IFS=$TAB read -r impl_name _ _ _ _ _; do
      [ -n "$impl_name" ] || continue
      printf '  %-12s' "$impl_name"
    done <"$resolved_impls_file"
    printf '\n'

    # Rows
    while IFS= read -r case_name; do
      [ -n "$case_name" ] || continue
      printf '  %-50s' "$case_name"
      while IFS=$TAB read -r impl_name _ _ _ _ _; do
        [ -n "$impl_name" ] || continue
        status=$(awk -F"$TAB" -v i="$impl_name" -v c="$case_name" \
          '$1==i && $3==c {print $2; exit}' "$results_file")
        printf '  %-12s' "${status:--}"
      done <"$resolved_impls_file"
      printf '\n'
    done <"$divergent_file"
  fi
fi

[ "$overall_fail" = 0 ]
