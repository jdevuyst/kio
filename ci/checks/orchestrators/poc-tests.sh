#!/bin/sh
# shellcheck disable=SC2034 # Capacity-explicitness is read by sourced common.sh.
#
# Validate the POC corpus contract, then build the implementations +
# verifier and run `test-data/poc/` through `ci/run-tests.sh`, with
# per-case checks (kio fmt idempotence, IS_KIO_PRIME biconditional,
# rlib-cache warm-hit) wired in via run-tests' `--check=` pipeline.
#
# The POC corpus carries adopter-grade Kio modules — one per
# data structure / topic — whose `run.sh` chains `kio check` +
# `kio test` + per-backend build + run. The corpus has a flat
# layout (`test-data/poc/<topic>/`), not the exit-code-bucketed
# shape `test-data/goldens/` uses. See `test-data/poc/README.md`.
#
# Single impl bucket: regular Kio compilers run against the full
# corpus. There is no kio-prime bucket — POC `run.sh` files
# invoke `kio test`, a surface-only CLI kio-prime rejects, so
# `--prime-only` would skip every case anyway. The Kio' phase-path
# artifact differential is independent of those custom execution
# scripts and runs whenever the POC's root package builds the target.
#
# Case sampling. Each POC is the worked example a docs/poc/ case study
# is written against, so the corpus runs WHOLE by default: it is small,
# and a POC that stops working invalidates a published page. Sampling is
# an opt-in for a gate that must be fast (the GitHub run). A sampled-out
# POC still runs every `# ROUTING: case-binary` check this orchestrator
# wires. See ci/run-tests.sh --sample-cases.
#
# Usage:
#   sh ci/checks/orchestrators/poc-tests.sh \
#     [--impls=FULL_IMPL_MATRIX|SAMPLE_IMPL|<name>[,<name>...]] [--jobs=<N>] \
#     [--compiler-jobs=<N>] \
#     [--all-cases | --sample-cases | --case-count=<N>] [--case-seed=<S>] \
#     [-- <run-tests args>...]
#
# --impls=FULL_IMPL_MATRIX
#                run all configured impls (default).
# --impls=SAMPLE_IMPL
#                for each case, run exactly one applicable impl,
#                 picked at random per run.
# --impls=<list>  restrict run-tests to the named comma-separated impls.
# --jobs=<N>      passed through to run-tests as --jobs; default is
#                 run-tests' native available-parallelism default.
# --compiler-jobs=<N>
#                 cap top-level Cargo invocations, compiler-producing Kio
#                 commands, and runner compiler commands; omission uses paced,
#                 best-effort CPU/memory feedback; a numeric value is a fixed cap.
# --all-cases     every POC runs its impls (default).
# --sample-cases  cap impl runs at $DEFAULT_CASE_COUNT POCs.
# --case-count=<N>
#                 cap at N instead.
# --case-seed=<S> pin the draw for reproduction.
#
# Anything after `--` is forwarded verbatim to ci/run-tests.sh.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

# Absolute-path source so the helpers resolve from any cwd; shellcheck
# can't follow the dynamic $SCRIPT_DIR path (SC1091).
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

TAB=$(printf '\t')

set_default_build_cache_size

# Per-bucket cap when this corpus samples. POC is a flat corpus (one
# bucket), so it is simply the number of POCs that build and run.
DEFAULT_CASE_COUNT=10

# POCs run whole by default: the corpus is small, and each one backs a
# published docs/poc/ case study.
#
# Consumed across the source boundary by lib/common.sh (SC2034).
# shellcheck disable=SC2034
CASE_MODE=all
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
# unless that harness deliberately scopes itself narrower. POC cases
# exercise the full backend pipeline (build + run), so the impl list
# matches `golden-tests.sh`'s regular bucket. The kio-prime impl is
# omitted because POC `run.sh` files
# invoke `kio test`, which kio-prime rejects (equiv is surface-only).
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
Usage: sh $0 [--impls=FULL_IMPL_MATRIX|SAMPLE_IMPL|<name>[,<name>...]] [--jobs=<N>] [--compiler-jobs=<N>] [--all-cases | --sample-cases | --case-count=<N>] [--case-seed=<S>] [-- <run-tests args>...]

Build the configured Kio implementations + the Kio' verifier, then
run ci/run-tests.sh over test-data/poc with per-case checks wired
in (kio fmt idempotence, IS_KIO_PRIME biconditional, rlib-cache
warm-hit).

POC cases chain \`kio check\` + \`kio test\` + per-backend build +
run inside each \`run.sh\`. All three must exit 0 for the case to
pass.

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

Case coverage (which POCs run their impls) — this corpus runs WHOLE by
default; it is small, and each POC backs a published docs/poc/ case
study:
--all-cases     every POC builds and runs (default).
--sample-cases  cap impl runs at $DEFAULT_CASE_COUNT POCs.
--case-count=<N>
                cap at N instead.
--case-seed=<S> pin the draw. run-tests prints the effective seed and
                the selected POCs.

A sampled-out POC still runs every \`# ROUTING: case-binary\` check:
sampling scopes the build+run tier only.

Anything after \`--\` is forwarded verbatim to ci/run-tests.sh.

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

# After this loop, "$@" holds the passthrough args (everything after --).
resolve_compiler_jobs

validate_poc_contract() {
  poc_dir=$REPO_ROOT/test-data/poc
  ok=1

  if [ ! -f "$poc_dir/README.md" ]; then
    printf 'error: POC corpus contract file is missing: test-data/poc/README.md\n' >&2
    ok=0
  fi

  for dir in "$poc_dir"/*/; do
    [ -d "$dir" ] || continue
    [ -n "$dir" ] || continue
    if [ ! -f "$dir/expected.exit" ]; then
      rel=${dir#"$REPO_ROOT"/}
      printf 'error: POC case is missing expected.exit: %s\n' "${rel%/}" >&2
      ok=0
    fi
  done

  expected_files=$(find "$poc_dir" -name expected.exit -type f | sort)
  while IFS= read -r expected; do
    [ -n "$expected" ] || continue
    if ! expected_exit_is_zero "$expected"; then
      printf 'error: POC expected.exit must contain exactly 0: %s\n' "${expected#"$REPO_ROOT"/}" >&2
      ok=0
    fi
  done <<EOF
$expected_files
EOF

  [ "$ok" = 1 ]
}

validate_poc_contract

# Save the passthrough args so run-tests.sh can prepend them to its argv.
passthrough_count=$#
init_orchestrator_tmp poc-tests
PASSTHROUGH_FILE="$ORCHESTRATOR_TMP/passthrough.args"
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_poc_tests() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$ORCHESTRATOR_TMP"
  exit "$cleanup_status"
}
trap cleanup_poc_tests EXIT
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

# Validate the --impls selection, then filter to the selected impls.
# Both helpers live in lib/common.sh.
resolve_case_seed
validate_impls_against_list "$ALL_IMPLS"
SELECTED=$(filter_impls "$ALL_IMPLS")

# Build phase. Same set of builds as `golden-tests.sh`: the kio /
# kio-prime binaries from the kio-rs crate, the standalone Kio'
# grammar verifier, and the per-backend test runners.
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

set -- "--cases-dir=test-data/poc" "--cache-base=$(shared_cache_base poc)"
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
while IFS= read -r arg; do
  set -- "$@" "$arg"
done <"$PASSTHROUGH_FILE"

printf '\n========== run-tests (poc) ==========\n'
sh ci/run-tests.sh "$@"
