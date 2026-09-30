#!/bin/sh
# shellcheck disable=SC2034 # Capacity-explicitness is read by sourced common.sh.
#
# Build the implementations + verifier and run the castle corpus
# through `ci/run-tests.sh`, with per-case checks (kio fmt
# idempotence, IS_KIO_PRIME biconditional, Kio' round-trip,
# TypeScript-strict typecheck, rlib-cache warm-hit) wired in via
# run-tests' `--check=` pipeline.
#
# Castles are larger, coherent Kio programs — algorithms, toy games,
# parsers, simulations, puzzle solvers — that exercise realistic
# composition through the standard runner path. They are success-only
# standard runner cases: each carries a `run.args` (normally empty,
# selecting the standard build-then-run path) and an `expected.exit`
# of exactly `0`. The corpus has a flat layout
# (`test-data/castles/<name>/`), not the exit-code-bucketed shape
# `test-data/goldens/` uses. See `test-data/castles/README.md`.
#
# Single impl bucket: regular Kio compilers run against selected
# castles. Unlike `golden-tests.sh`, there is no separate
# `kio-prime@js` source-execution bucket: castles are natural surface
# packages, so a Kio'-only compiler has nothing dedicated to run here.
# The per-case prime-marker / kio-prime-roundtrip checks still assert
# the source-level Kio' biconditional and generated-artifact parity
# where their contracts fit.
#
# Case sampling. Running every castle on every push is unnecessary; the
# corpus exists to catch composition regressions over time, not on each
# commit. So this orchestrator's default is to sample (see
# $DEFAULT_CASE_COUNT) — unlike regular/direct Prime goldens and poc, which
# default to whole.
# `--all-cases` runs the full corpus; `--case-count=N` sets the size;
# `--case-seed=S` pins the draw. The selection itself lives in
# ci/run-tests.sh (`--sample-cases`), which prints the effective seed and
# the selected castles.
#
# Sampling is safe for this corpus only because a sampled-out castle still
# runs its `# ROUTING: case-binary` checks. dep-canonical is one of them,
# so a committed materialized dependency tree that drifts is caught on
# every castle on every pass rather than waiting for its castle to be
# drawn — which is the one artifact here that would otherwise rot in the
# unsampled tail.
#
# Program seeds are fixture data, not orchestrator state. A castle that
# needs randomness reads its seed from its checked-in `input.stdin`
# fixture and implements any PRNG in Kio. There is no `--program-seed`,
# no runner-provided `seed()` host fn, and no seed injection: sampling
# controls only which checked-in castles run.
#
# Usage:
#   sh ci/checks/orchestrators/castle-tests.sh \
#     [--impls=FULL_IMPL_MATRIX|SAMPLE_IMPL|<name>[,<name>...]] [--jobs=<N>] \
#     [--compiler-jobs=<N>] \
#     [--all-cases | --sample-cases | --case-count=<N>] [--case-seed=<S>] \
#     [-- <name>...]
#
# --impls=FULL_IMPL_MATRIX
#                  run all configured impls (default).
# --impls=SAMPLE_IMPL
#                  for each case, run exactly one applicable impl,
#                  picked at random per run.
# --impls=<list>   restrict run-tests to the named comma-separated impls.
# --jobs=<N>       passed through to run-tests as --jobs; default is
#                  run-tests' native available-parallelism default.
# --compiler-jobs=<N>
#                  cap top-level Cargo invocations, compiler-producing Kio
#                  commands, and runner compiler commands; omission uses paced,
#                  best-effort CPU/memory feedback; a numeric value is a fixed cap.
# --all-cases      every castle runs its impls.
# --sample-cases   sample at the default cap ($DEFAULT_CASE_COUNT). This
#                  is already the default for this corpus; the flag is
#                  accepted so ci/all.sh can pass it uniformly.
# --case-count=<N> sample at N castles instead.
# --case-seed=<S>  pin the draw. Default: the GitHub Actions run context
#                  when present, else local UTC epoch seconds.
#
# Anything after `--` is forwarded verbatim to ci/run-tests.sh as
# positional case-name filters (e.g. `-- maze ledger`). Filters narrow
# the corpus before the draw, so `-- maze ledger` with a cap of 2 or
# more runs exactly those.
#
# POSIX sh only.

set -eu

# Older POSIX shells can lose a parse failure's status once EXIT is trapped.
sh -n "$0"

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

# Absolute-path source so the helpers resolve from any cwd; shellcheck
# can't follow the dynamic $SCRIPT_DIR path (SC1091).
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

TAB=$(printf '\t')

CASTLES_DIR=$REPO_ROOT/test-data/castles

set_default_build_cache_size

# Default per-bucket cap when this corpus samples. Castles are a flat
# corpus (one bucket), so it is simply the number of castles that build
# and run. Sized so an ad-hoc local run and each CI pass exercise a
# meaningful slice without paying for every castle's full build+run.
DEFAULT_CASE_COUNT=10

# Castles sample by default: the corpus is large and slow, and its job is
# to catch composition regressions over time rather than on every commit.
#
# Consumed across the source boundary by lib/common.sh (SC2034).
# shellcheck disable=SC2034
CASE_MODE=sample
# shellcheck disable=SC2034
CASE_COUNT=$DEFAULT_CASE_COUNT
# shellcheck disable=SC2034
CASE_SEED=

# Configured implementations: one record per line, tab-separated as
# name<TAB>kio<TAB>runner<TAB>target. Names must not contain
# whitespace or commas.
#
# IMPL-LIST PARITY RULE — every test harness that defines a run-tests
# impl list keeps the same shape across harnesses: every kio compiler /
# backend pair the repo ships shows up in each harness's $ALL_IMPLS
# unless that harness deliberately scopes itself narrower. Castles
# exercise the full backend pipeline (build + run), so the impl list
# matches `poc-tests.sh`'s regular bucket. The kio-prime impl is
# omitted: castles are natural surface packages, so a
# Kio'-only compiler has no dedicated source-execution bucket here (the
# per-case kio-prime-roundtrip check still verifies the Kio' boundary).
# See `TESTING.md` § Harness impl-list parity for the rule.
ALL_IMPLS="kio@js${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-js${TAB}js
kio@ts${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-ts${TAB}ts
kio@python${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-python${TAB}python
kio@java${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-java${TAB}java
kio@rust${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-rust${TAB}rust
kio@go${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-go${TAB}go
kio@swift${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-swift${TAB}swift
kio@haskell${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-haskell${TAB}haskell"

# ONLY is read by the sourced lib helpers (apply_impls_spec sets it;
# filter_impls / validate_impls_against_list consume it), not in this
# file, so shellcheck sees only the write.
# shellcheck disable=SC2034
ONLY=
JOBS=
if [ "${KIO_CI_SCHEDULE_COMPILER_JOBS+x}" = x ]; then
  COMPILER_JOBS=$KIO_CI_SCHEDULE_COMPILER_JOBS
  COMPILER_JOBS_EXPLICIT=1
else
  COMPILER_JOBS=adaptive
  COMPILER_JOBS_EXPLICIT=0
fi
SAMPLE_IMPL=0

while [ $# -gt 0 ]; do
  case "$1" in
    --impls=*) apply_impls_spec "${1#--impls=}" ;;
    --impls)
      shift
      [ $# -gt 0 ] || { printf 'error: --impls requires a value\n' >&2; exit 2; }
      apply_impls_spec "$1"
      ;;
    --jobs=*) JOBS=${1#--jobs=} ;;
    --jobs)
      shift
      [ $# -gt 0 ] || { printf 'error: --jobs requires a value\n' >&2; exit 2; }
      JOBS=$1
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
    --all-cases) apply_all_cases ;;
    --sample-cases) apply_sample_cases ;;
    --case-count=*) apply_case_count "${1#--case-count=}" ;;
    --case-count)
      shift
      [ $# -gt 0 ] || { printf 'error: --case-count requires a value\n' >&2; exit 2; }
      apply_case_count "$1"
      ;;
    --case-seed=*) apply_case_seed "${1#--case-seed=}" ;;
    --case-seed)
      shift
      [ $# -gt 0 ] || { printf 'error: --case-seed requires a value\n' >&2; exit 2; }
      apply_case_seed "$1"
      ;;
    -h|--help)
      regular=$(printf '%s\n' "$ALL_IMPLS" | awk -F"$TAB" 'NF>0 {printf "%s ", $1}')
      cat <<EOF
Usage: sh $0 [--impls=FULL_IMPL_MATRIX|SAMPLE_IMPL|<name>[,<name>...]] [--jobs=<N>] [--compiler-jobs=<N>] [--all-cases | --sample-cases | --case-count=<N>] [--case-seed=<S>] [-- <name>...]

Build the configured Kio implementations + the Kio' verifier, then run
ci/run-tests.sh over test-data/castles with per-case checks wired in
(kio fmt idempotence, IS_KIO_PRIME biconditional, Kio' round-trip,
TypeScript-strict typecheck, rlib-cache warm-hit).

Castles are larger composed Kio programs run through the standard
runner path. Each is a success case: empty run.args, expected.exit 0.

Implementation coverage (which IMPLS run):
--impls=FULL_IMPL_MATRIX runs all configured impls (default).
--impls=SAMPLE_IMPL forwards to run-tests so each case runs on one
applicable impl, picked at random per run. Use for local iteration.
--impls=<list> restricts to the named impls (comma-separated).
See TESTING.md § Local iteration.

--jobs is forwarded to run-tests as --jobs; default is run-tests'
auto = native scheduler available parallelism.
--compiler-jobs caps top-level Cargo invocations, compiler-producing Kio
commands, and actual native compiler commands across participating worktrees;
omission uses paced, best-effort CPU/memory feedback; a numeric value is a fixed cap.

Case coverage (which CASTLES run their impls) — this corpus SAMPLES by
default, because it is large and slow and exists to catch composition
regressions over time rather than on every commit:
--all-cases     every castle builds and runs.
--sample-cases  sample at the default cap ($DEFAULT_CASE_COUNT); already
                the default here, accepted so ci/all.sh can pass it
                uniformly across corpora.
--case-count=<N>
                sample at N castles instead. If fewer than N remain after
                filtering, all of them run.
--case-seed=<S> pin the draw. Default: the GitHub Actions run context
                (GITHUB_RUN_ID:GITHUB_RUN_ATTEMPT) when present, else
                local UTC epoch seconds. run-tests always prints the
                effective seed and the selected castles.

A sampled-out castle still runs every \`# ROUTING: case-binary\` check
(fmt idempotence, dependency-tree canonicality, the IS_KIO_PRIME
biconditional) — sampling scopes the build+run tier only, so those
invariants stay gated on the whole corpus every pass.

Program seeds are fixture data, not orchestrator state: a castle that
needs randomness reads its seed from its checked-in input.stdin. There
is no --program-seed and no seed injection.

Anything after \`--\` is forwarded to ci/run-tests.sh as positional
case-name filters (e.g. \`-- maze ledger\`); filters narrow the corpus
before the draw.

Impls: ${regular}
EOF
      exit 0
      ;;
    --) shift; break ;;
    -*) printf 'error: unknown argument: %s\n' "$1" >&2; exit 2 ;;
    *) printf 'error: unknown argument: %s (case-name filters go after --)\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

# After this loop, "$@" holds the passthrough case-name filters
# (everything after --).
resolve_compiler_jobs

# Save the passthrough filters so run-tests.sh can prepend them to its
# argv.
passthrough_count=$#
init_orchestrator_tmp castle-tests
PASSTHROUGH_FILE="$ORCHESTRATOR_TMP/passthrough.args"
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_castle_tests() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$ORCHESTRATOR_TMP"
  exit "$cleanup_status"
}
trap cleanup_castle_tests EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
: >"$PASSTHROUGH_FILE"
i=0
while [ "$i" -lt "$passthrough_count" ]; do
  printf '%s\n' "$1" >>"$PASSTHROUGH_FILE"
  shift
  i=$((i + 1))
done

validate_castle_contract() {
  ok=1

  if [ ! -d "$CASTLES_DIR" ]; then
    printf 'error: castle corpus directory is missing: test-data/castles\n' >&2
    return 1
  fi

  if [ ! -f "$CASTLES_DIR/README.md" ]; then
    printf 'error: castle corpus contract file is missing: test-data/castles/README.md\n' >&2
    ok=0
  fi

  for dir in "$CASTLES_DIR"/*/; do
    [ -d "$dir" ] || continue
    rel=${dir#"$REPO_ROOT"/}
    rel=${rel%/}

    # A castle is a direct child of test-data/castles/. Reject any
    # nested case directory (identified by an expected.exit below the
    # top level) so the flat-corpus contract can't silently erode.
    nested=$(find "$dir" -mindepth 2 -name expected.exit -type f 2>/dev/null | head -n 1)
    if [ -n "$nested" ]; then
      printf 'error: castle %s has a nested case directory (%s); castles are direct children of test-data/castles/\n' \
        "$rel" "${nested#"$REPO_ROOT"/}" >&2
      ok=0
    fi

    if [ ! -f "$dir/README.md" ]; then
      printf 'error: castle %s is missing README.md\n' "$rel" >&2
      ok=0
    fi
    if [ ! -f "$dir/run.args" ]; then
      printf 'error: castle %s is missing run.args (the only execution file castles use)\n' "$rel" >&2
      ok=0
    fi
    if [ -f "$dir/run.sh" ]; then
      printf 'error: castle %s carries run.sh; castles use only run.args for the standard runner path\n' "$rel" >&2
      ok=0
    fi
    for f in expected.stdout expected.exit; do
      if [ ! -f "$dir/$f" ]; then
        printf 'error: castle %s is missing %s\n' "$rel" "$f" >&2
        ok=0
      fi
    done
    if [ ! -f "$dir/expected.stderr" ] && [ ! -f "$dir/expected.stderr.ignore" ] && [ ! -f "$dir/expected.stderr.grep" ]; then
      printf 'error: castle %s is missing a stderr-policy file (expected.stderr, expected.stderr.ignore, or expected.stderr.grep)\n' "$rel" >&2
      ok=0
    fi
    if [ -f "$dir/expected.exit" ] && ! expected_exit_is_zero "$dir/expected.exit"; then
      printf 'error: castle %s expected.exit must contain exactly 0 (every castle is a success case)\n' "$rel" >&2
      ok=0
    fi

    # A castle that reads stdin via read_ascii_line() must ship the
    # input.stdin fixture; otherwise the standard runner path has
    # nothing to redirect and the program hits EOF on the first read.
    # Materialized dependency trees may carry their own unused protocol
    # host declarations; only the consumer's own source decides whether
    # this castle reads stdin.
    if [ -d "$dir/workdir" ]; then
      dep_roots=$(find "$dir/workdir" -maxdepth 1 -type f -name '*.dep.kio' -exec basename {} .dep.kio \; 2>/dev/null || true)
      reads_stdin=$(
        find "$dir/workdir" -type f -name '*.kio' ! -path '*/out/*' -print 2>/dev/null \
          | while IFS= read -r src; do
              skip=0
              for dep in $dep_roots; do
                case "$src" in
                  ("$dir/workdir/$dep"/*)
                    skip=1
                    break
                    ;;
                esac
              done
              [ "$skip" = 1 ] && continue
              if grep -qE 'read_ascii_line[[:space:]]*\(' "$src" 2>/dev/null; then
                printf '%s\n' "$src"
                break
              fi
            done \
          | head -n 1
      )
      if [ -n "$reads_stdin" ] && [ ! -f "$dir/input.stdin" ]; then
        printf 'error: castle %s declares read_ascii_line() but has no input.stdin fixture\n' "$rel" >&2
        ok=0
      fi
    fi
  done

  [ "$ok" = 1 ]
}

validate_castle_contract

# Validate the --impls selection, then filter to the selected impls.
# Both helpers live in lib/common.sh.
resolve_case_seed
validate_impls_against_list "$ALL_IMPLS"
SELECTED=$(filter_impls "$ALL_IMPLS")

# Build phase. Same set of builds as `poc-tests.sh`: the kio /
# kio-prime binaries from the kio-rs crate, the standalone Kio' grammar
# verifier, and the per-backend test runners.
needs_compiler_build=0
case "$SELECTED" in
  *kio@*) needs_compiler_build=1 ;;
esac
if [ "$needs_compiler_build" = 1 ]; then
  build_corpus_tool_binary \
    kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$ORCHESTRATOR_TMP/kio" \
    --all-features --bins
  SELECTED=$(printf '%s\n' "$SELECTED" | awk -F"$TAB" -v OFS="$TAB" \
    -v original="$REPO_ROOT/kio-rs/target/debug/kio" -v kio="$ORCHESTRATOR_TMP/kio" '
      NF > 0 { if ($2 == original) $2 = kio; print }
    ')
fi

build_corpus_tool_binary \
  kio-prime "$REPO_ROOT/kio-rs" kio-prime "$ORCHESTRATOR_TMP/kio-prime" \
  --no-default-features --features prime,cli,parallel --bin kio-prime
KIO_PRIME_BIN="$ORCHESTRATOR_TMP/kio-prime"

build_corpus_tool_binary \
  kio-prime-check "$REPO_ROOT/ci/infra/kio-prime-check-rs" \
  kio-prime-check "$ORCHESTRATOR_TMP/kio-prime-check"

build_runner_feature() {
  feature=$1
  ( cd "$REPO_ROOT/ci/infra/kio-test-runner-rs" && sh "$REPO_ROOT/ci/cargo.sh" build --no-default-features --features "$feature" )
}

selected_runner_targets=$(printf '%s\n' "$SELECTED" | awk -F"$TAB" 'NF > 0 { print $4 }')
for target in js ts python java rust go swift haskell; do
  if printf '%s\n' "$selected_runner_targets" | grep -qx "$target"; then
    build_runner_feature "$target"
  fi
done

KIO_PRIME_CHECK_BIN="$ORCHESTRATOR_TMP/kio-prime-check"
export KIO_PRIME_CHECK_BIN

cd "$REPO_ROOT"

if [ -z "$SELECTED" ]; then
  exit 0
fi

# Castles may depend on POC packages, and several POCs depend on the
# elaborator package. Nested-dependency support is materialized-only, but
# a depended-on POC ships its own committed materialized closure, so its
# nested trees are already present in a clean checkout — no prefetch is
# needed. The per-unit `dep fetch` (run-tests.sh Step 1.5) re-roots each
# castle's own dependency from that committed source.

# Build the run-tests.sh argv. run-tests owns case selection: it applies
# the caller's positional filters first, then draws the sample from what
# survives them (see its --sample-cases block).
set -- "--cases-dir=test-data/castles" "--cache-base=$(shared_cache_base castles)"
TMPDIR_RT_ARGS="$ORCHESTRATOR_TMP/run-tests.args"
echo "$SELECTED" | while IFS=$TAB read -r impl_name impl_kio impl_runner impl_target; do
  [ -n "$impl_name" ] || continue
  printf -- '--impl-def=name=%s,kio=%s,runner=%s,target=%s,prime-kio=%s\n' \
    "$impl_name" "$impl_kio" "$impl_runner" "$impl_target" "$KIO_PRIME_BIN"
done >"$TMPDIR_RT_ARGS"
while IFS= read -r arg; do
  [ -n "$arg" ] || continue
  set -- "$@" "$arg"
done <"$TMPDIR_RT_ARGS"
# Per-case checks whose contracts fit castles: fmt idempotence and the
# IS_KIO_PRIME biconditional on the source, artifact parity between
# the source pipeline and the Kio'-roundtripped build, the
# TypeScript-strict typecheck, and the rlib-cache warm-hit. Castles
# declare no `equiv` law suites, so there is no equiv discharge to
# assert here. See TESTING.md § Harness impl-list parity.
set -- "$@" \
  "--check=$REPO_ROOT/ci/checks/per-case/fmt-canonical.sh" \
  "--check=$REPO_ROOT/ci/checks/per-case/dep-canonical.sh" \
  "--check=$REPO_ROOT/ci/checks/per-case/prime-marker.sh" \
  "--check=$REPO_ROOT/ci/checks/per-case/kio-prime-roundtrip.sh" \
  "--check=$REPO_ROOT/ci/checks/per-case/rlib-cache-second-run-hits.sh" \
  "--check=$REPO_ROOT/ci/checks/per-case/tsc-strict.sh" \
  "--check=$REPO_ROOT/ci/checks/per-case/pyright-strict.sh"
if [ -n "$JOBS" ]; then
  set -- "$@" "--jobs=$JOBS"
fi
if [ "$COMPILER_JOBS" != adaptive ]; then
  set -- "$@" "--compiler-jobs=$COMPILER_JOBS"
fi
if [ "$SAMPLE_IMPL" = 1 ]; then
  set -- "$@" "--impls=SAMPLE_IMPL"
fi
while IFS= read -r arg; do
  [ -n "$arg" ] || continue
  set -- "$@" "$arg"
done <<EOF
$(case_sampling_args)
EOF
# Caller-supplied positional case-name filters (after `--`).
while IFS= read -r filter; do
  [ -n "$filter" ] || continue
  set -- "$@" "$filter"
done <"$PASSTHROUGH_FILE"

printf '\n========== run-tests (castles) ==========\n'
sh ci/run-tests.sh "$@"
