#!/bin/sh
# shellcheck disable=SC2034 # Capacity-explicitness is read by sourced common.sh.
#
# Validate and run the backend-emissions corpus through ci/run-tests.sh.
# Every case owns its host/artifact assertion in run.sh; the configured runner
# is therefore an always-fail tripwire, not a built runner-crate feature.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/emissions-contract.sh"

TAB=$(printf '\t')
EMISSIONS_DIR=$REPO_ROOT/test-data/emissions
TRIPWIRE_RUNNER=$REPO_ROOT/ci/infra/emissions-runner-forbidden.sh

# Sampling takes one case from each top-level backend bucket. Direct local
# runs remain exhaustive; the GitHub gate opts into this cap.
DEFAULT_CASE_COUNT=1
# shellcheck disable=SC2034
CASE_MODE=all
# shellcheck disable=SC2034
CASE_COUNT=$DEFAULT_CASE_COUNT
# shellcheck disable=SC2034
CASE_SEED=

# Keep this implementation list aligned under TESTING.md § Harness impl-list
# parity. Each case declares exactly its bucket's one target, so every case has
# one applicable record even when the configured cohort contains several impls.
ALL_IMPLS="kio@js${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${TRIPWIRE_RUNNER}${TAB}js
kio@ts${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${TRIPWIRE_RUNNER}${TAB}ts
kio@python${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${TRIPWIRE_RUNNER}${TAB}python
kio@java${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${TRIPWIRE_RUNNER}${TAB}java
kio@rust${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${TRIPWIRE_RUNNER}${TAB}rust
kio@go${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${TRIPWIRE_RUNNER}${TAB}go
kio@swift${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${TRIPWIRE_RUNNER}${TAB}swift
kio@haskell${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${TRIPWIRE_RUNNER}${TAB}haskell"

# Read across the common.sh source boundary.
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
      regular=$(printf '%s\n' "$ALL_IMPLS" | awk -F"$TAB" 'NF > 0 {printf "%s ", $1}')
      cat <<EOF
Usage: sh $0 [--impls=FULL_IMPL_MATRIX|SAMPLE_IMPL|<name>[,<name>...]] [--jobs=<N>] [--compiler-jobs=<N>] [--all-cases | --sample-cases | --case-count=<N>] [--case-seed=<S>] [-- <filter>...]

Validate test-data/emissions and run its success-only custom run.sh cases.
Each case belongs to one backend bucket and declares exactly that build target.

Implementation coverage:
  --impls=FULL_IMPL_MATRIX  all eight backend records (default)
  --impls=SAMPLE_IMPL       one applicable impl per case
  --impls=<list>            named comma-separated impl cohort

Case coverage:
  --all-cases               every selected-backend case (default)
  --sample-cases            one case per selected backend
  --case-count=<N>          N cases per selected backend
  --case-seed=<S>           reproducible sample seed

Arguments after -- are forwarded verbatim as ci/run-tests.sh filters. Backend
buckets outside the selected impl cohort are excluded without adding include
filters, so an exact caller filter is never widened.

Impls: ${regular}
EOF
      exit 0
      ;;
    --) shift; break ;;
    -*) printf 'error: unknown argument: %s\n' "$1" >&2; exit 2 ;;
    *) printf 'error: unknown argument: %s (case filters go after --)\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

resolve_compiler_jobs
resolve_case_seed

passthrough_count=$#
init_orchestrator_tmp emissions-tests
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_emissions_tests() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$ORCHESTRATOR_TMP"
  exit "$cleanup_status"
}
trap cleanup_emissions_tests EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
PASSTHROUGH_FILE=$ORCHESTRATOR_TMP/passthrough.args
: >"$PASSTHROUGH_FILE"
emtf_i=0
while [ "$emtf_i" -lt "$passthrough_count" ]; do
  printf '%s\n' "$1" >>"$PASSTHROUGH_FILE"
  shift
  emtf_i=$((emtf_i + 1))
done

EMISSIONS_CONTRACT_PREFIX=emissions-tests
export EMISSIONS_CONTRACT_PREFIX
emissions_validate_corpus "$EMISSIONS_DIR" || exit 1

validate_impls_against_list "$ALL_IMPLS"
SELECTED=$(filter_impls "$ALL_IMPLS")

printf '\n========== build kio (emissions) ==========\n'
build_corpus_tool_binary \
  kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$ORCHESTRATOR_TMP/kio" \
  --all-features --bins
SELECTED=$(printf '%s\n' "$SELECTED" | awk -F"$TAB" -v OFS="$TAB" \
  -v original="$REPO_ROOT/kio-rs/target/debug/kio" -v kio="$ORCHESTRATOR_TMP/kio" '
    NF > 0 { if ($2 == original) $2 = kio; print }
  ')

cd "$REPO_ROOT"

# run-tests requires a cache base even though the tripwire runner must never
# consume it. Keep the empty/defensive directory inside this invocation's
# throwaway root rather than publishing a shared runner cache.
set -- \
  "--cases-dir=test-data/emissions" \
  "--cache-base=$ORCHESTRATOR_TMP/runner-cache"

IMPL_ARGS=$ORCHESTRATOR_TMP/impl.args
printf '%s\n' "$SELECTED" | while IFS=$TAB read -r emta_name emta_kio emta_runner emta_target; do
  [ -n "$emta_name" ] || continue
  printf -- '--impl-def=name=%s,kio=%s,runner=%s,target=%s\n' \
    "$emta_name" "$emta_kio" "$emta_runner" "$emta_target"
done >"$IMPL_ARGS"
while IFS= read -r emta_arg; do
  [ -n "$emta_arg" ] || continue
  set -- "$@" "$emta_arg"
done <"$IMPL_ARGS"

set -- "$@" "--check=$REPO_ROOT/ci/checks/per-case/fmt-canonical.sh"
[ -z "$JOBS" ] || set -- "$@" "--jobs=$JOBS"
[ "$COMPILER_JOBS" = adaptive ] || set -- "$@" "--compiler-jobs=$COMPILER_JOBS"
[ "$SAMPLE_IMPL" = 0 ] || set -- "$@" --impls=SAMPLE_IMPL

while IFS= read -r emta_arg; do
  [ -n "$emta_arg" ] || continue
  set -- "$@" "$emta_arg"
done <<EOF
$(case_sampling_args)
EOF

# Exclusions intersect the selected implementation cohort with any exact
# positional filters below. Adding positive backend filters would instead OR
# with those caller filters in run-tests and silently widen them.
SELECTED_TARGETS=$(printf '%s\n' "$SELECTED" | awk -F"$TAB" 'NF > 0 { print $4 }')
for emta_backend in $EMISSIONS_BACKENDS; do
  if ! printf '%s\n' "$SELECTED_TARGETS" | grep -qx "$emta_backend"; then
    set -- "$@" "--exclude=^$emta_backend/"
  fi
done

while IFS= read -r emta_filter; do
  [ -n "$emta_filter" ] || continue
  set -- "$@" "$emta_filter"
done <"$PASSTHROUGH_FILE"

printf '\n========== run-tests (emissions) ==========\n'
sh ci/run-tests.sh "$@"
