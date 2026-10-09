#!/bin/sh
#
# Measure kio-gen's marginal line-coverage contribution over the
# hand-written goldens harness.
#
# This reporting harness runs two instrumented kio-rs passes:
#   1. test-data/goldens alone
#   2. test-data/goldens plus a fresh kio-gen batch
#
# It reports line coverage for each pass and the delta. The number is
# informational: it is a generator-health signal, not a CI gate.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
# The file is repository-owned and resolved from this script; shellcheck
# cannot follow the dynamic $REPO_ROOT path (SC1091).
# shellcheck disable=SC1091
. "$REPO_ROOT/ci/checks/orchestrators/lib/common.sh"

COUNT=500
SEED=
IMPLS=FULL_IMPL_MATRIX
JOBS=auto
KEEP_WORK=0
COMPLETE=0

usage() {
  cat <<EOF
Usage: sh $0 [--count=<n>] [--seed=<n>] [--impls=FULL_IMPL_MATRIX|SAMPLE_IMPL|kio@js[,kio@rust]] [--jobs=<n>|auto] [--keep-work]

Run a cargo-llvm-cov coverage-delta report for kio-gen.

--count controls generated case count (default 500).
--seed makes the generated batch reproducible. Default uses current time.
--impls chooses regular implementations for both passes.
--jobs is forwarded to ci/run-tests.sh (default auto).
--keep-work preserves target/reports/kio-gen-coverage-delta/<run>/.
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --count=*) COUNT=${1#--count=} ;;
    --count)
      shift
      [ $# -gt 0 ] || { printf 'error: --count requires a value\n' >&2; exit 2; }
      COUNT=$1
      ;;
    --seed=*) SEED=${1#--seed=} ;;
    --seed)
      shift
      [ $# -gt 0 ] || { printf 'error: --seed requires a value\n' >&2; exit 2; }
      SEED=$1
      ;;
    --impls=*) IMPLS=${1#--impls=} ;;
    --impls)
      shift
      [ $# -gt 0 ] || { printf 'error: --impls requires a value\n' >&2; exit 2; }
      IMPLS=$1
      ;;
    --jobs=*) JOBS=${1#--jobs=} ;;
    --jobs)
      shift
      [ $# -gt 0 ] || { printf 'error: --jobs requires a value\n' >&2; exit 2; }
      JOBS=$1
      ;;
    --keep-work) KEEP_WORK=1 ;;
    -h|--help)
      usage
      exit 0
      ;;
    *) printf 'error: unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

case "$COUNT" in
  ''|*[!0-9]*) printf 'error: --count must be a non-negative integer\n' >&2; exit 2 ;;
esac

unset LLVM_PROFILE_FILE CARGO_LLVM_COV_TARGET_DIR CARGO_LLVM_COV CARGO_LLVM_COV_SHOW_ENV \
  RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTDOCFLAGS CARGO_ENCODED_RUSTDOCFLAGS

if ! sh "$REPO_ROOT/ci/cargo.sh" llvm-cov --version >/dev/null 2>&1; then
  printf 'error: cargo-llvm-cov is not installed.\n' >&2
  printf 'Install the pinned optional Cargo tools with:\n' >&2
  printf '  sh ci/impl-toolchain.sh install-report-tools\n' >&2
  exit 2
fi

if [ -z "$SEED" ]; then
  SEED=$(date +%s)
fi

RUN_ROOT="$REPO_ROOT/target/reports/kio-gen-coverage-delta"
RUN_DIR="$RUN_ROOT/run-$$"
init_orchestrator_tmp kio-gen-coverage-delta
CR=$(printf '\r')
case "$ORCHESTRATOR_TMP:$RUN_DIR" in
  *','*|*"'"*|*'%'*|*"$CR"*|*'
'*) printf 'error: profile/wrapper path contains an unsupported character\n' >&2; exit 2 ;;
esac
cleanup_report() {
  cleanup_status=$?
  if [ "$COMPLETE" = 1 ] && [ "$cleanup_status" -eq 0 ]; then
    rm -rf "$ORCHESTRATOR_TMP"
  else
    printf 'incomplete coverage state kept at %s\n' "$ORCHESTRATOR_TMP" >&2
  fi
  if [ "$KEEP_WORK" = 0 ] && [ "$COMPLETE" = 1 ] && [ "$cleanup_status" -eq 0 ]; then
    rm -rf "$RUN_DIR"
  else
    printf 'coverage work kept at %s\n' "$RUN_DIR"
  fi
  return "$cleanup_status"
}
trap cleanup_report EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
mkdir -p "$RUN_ROOT"
mkdir "$RUN_DIR"
mkdir "$RUN_DIR/bin"
mkdir "$ORCHESTRATOR_TMP/bin"
KIO_CI_SCHEDULER_BIN=$(sh "$REPO_ROOT/ci/schedule.sh" --prepare)
export KIO_CI_SCHEDULER_BIN

TAB=$(printf '\t')
ALL_IMPLS="kio@js${TAB}${RUN_DIR}/bin/kio${TAB}${ORCHESTRATOR_TMP}/bin/kio-test-runner-js${TAB}js
kio@rust${TAB}${RUN_DIR}/bin/kio${TAB}${ORCHESTRATOR_TMP}/bin/kio-test-runner-rust${TAB}rust"

SELECTED_IMPLS=
SAMPLE_IMPL_MODE=0
case "$IMPLS" in
  FULL_IMPL_MATRIX)
    SELECTED_IMPLS=$ALL_IMPLS
    ;;
  SAMPLE_IMPL)
    SELECTED_IMPLS=$ALL_IMPLS
    SAMPLE_IMPL_MODE=1
    ;;
  ALL)
    printf 'error: selector ALL was renamed to FULL_IMPL_MATRIX\n' >&2
    exit 2
    ;;
  ANY)
    printf 'error: selector ANY was renamed to SAMPLE_IMPL\n' >&2
    exit 2
    ;;
  *)
    old_ifs=$IFS
    IFS=,
    for impl in $IMPLS; do
      case "$impl" in
        kio@js)
          SELECTED_IMPLS="${SELECTED_IMPLS:+$SELECTED_IMPLS
}kio@js${TAB}${RUN_DIR}/bin/kio${TAB}${ORCHESTRATOR_TMP}/bin/kio-test-runner-js${TAB}js"
          ;;
        kio@rust)
          SELECTED_IMPLS="${SELECTED_IMPLS:+$SELECTED_IMPLS
}kio@rust${TAB}${RUN_DIR}/bin/kio${TAB}${ORCHESTRATOR_TMP}/bin/kio-test-runner-rust${TAB}rust"
          ;;
        *) printf 'error: unknown impl in --impls: %s\n' "$impl" >&2; exit 2 ;;
      esac
    done
    IFS=$old_ifs
    ;;
esac

if [ -z "$SELECTED_IMPLS" ]; then
  printf 'error: --impls selected no implementations\n' >&2
  exit 2
fi

need_js=0
need_rust=0
printf '%s\n' "$SELECTED_IMPLS" | while IFS=$TAB read -r impl_name _impl_kio _impl_runner impl_target; do
  [ -n "$impl_name" ] || continue
  case "$impl_target" in
    js) : ;;
    rust) : ;;
    *) printf 'error: internal unknown target %s\n' "$impl_target" >&2; exit 2 ;;
  esac
done
if printf '%s\n' "$SELECTED_IMPLS" | awk -F"$TAB" '$4 == "js" { found = 1 } END { exit found ? 0 : 1 }'; then
  need_js=1
fi
if printf '%s\n' "$SELECTED_IMPLS" | awk -F"$TAB" '$4 == "rust" { found = 1 } END { exit found ? 0 : 1 }'; then
  need_rust=1
fi

printf 'coverage-delta: seed=%s count=%s impls=%s\n' "$SEED" "$COUNT" "$IMPLS"

printf 'coverage-delta: building support binaries\n'
( cd "$REPO_ROOT/ci/infra/kio-gen-rs" && sh "$REPO_ROOT/ci/cargo.sh" build --release )
( cd "$REPO_ROOT/ci/infra/kio-prime-check-rs" && sh "$REPO_ROOT/ci/cargo.sh" build --release )
cp "$REPO_ROOT/ci/infra/kio-gen-rs/target/release/kio-gen" "$ORCHESTRATOR_TMP/bin/kio-gen"
cp "$REPO_ROOT/ci/infra/kio-prime-check-rs/target/release/kio-prime-check" "$ORCHESTRATOR_TMP/bin/kio-prime-check"
KIO_PRIME_TARGET="$ORCHESTRATOR_TMP/kio-prime-target"
(
  cd "$REPO_ROOT/kio-rs"
  sh "$REPO_ROOT/ci/cargo.sh" build \
    --release --target-dir "$KIO_PRIME_TARGET" \
    --no-default-features --features prime,cli,parallel --bin kio-prime
)
cp "$KIO_PRIME_TARGET/release/kio-prime" "$ORCHESTRATOR_TMP/kio-prime"
KIO_PRIME_BIN="$ORCHESTRATOR_TMP/kio-prime"
if [ "$need_js" = 1 ]; then
  ( cd "$REPO_ROOT/ci/infra/kio-test-runner-rs" && sh "$REPO_ROOT/ci/cargo.sh" build --release --no-default-features --features js )
  cp "$REPO_ROOT/ci/infra/kio-test-runner-rs/target/release/kio-test-runner-js" "$ORCHESTRATOR_TMP/bin/kio-test-runner-js"
fi
if [ "$need_rust" = 1 ]; then
  ( cd "$REPO_ROOT/ci/infra/kio-test-runner-rs" && sh "$REPO_ROOT/ci/cargo.sh" build --release --no-default-features --features rust )
  cp "$REPO_ROOT/ci/infra/kio-test-runner-rs/target/release/kio-test-runner-rust" "$ORCHESTRATOR_TMP/bin/kio-test-runner-rust"
fi

BATCH="$RUN_DIR/kio-gen-batch"
"$ORCHESTRATOR_TMP/bin/kio-gen" \
  --output "$BATCH" --count "$COUNT" --seed "$SEED" >/dev/null

write_impl_args() {
  out=$1
  : >"$out"
  printf '%s\n' "$SELECTED_IMPLS" | while IFS=$TAB read -r impl_name _impl_kio impl_runner impl_target; do
    [ -n "$impl_name" ] || continue
    printf '%s\n' "--impl-def=name=${impl_name},kio=${COHORT_COMPILER},runner=${impl_runner},target=${impl_target},prime-kio=${KIO_PRIME_BIN}" >>"$out"
  done
}

run_tests_for_cases() {
  cases_dir=$1
  cache_dir=$2
  log_file=$3
  include_full_checks=$4
  cohort=$5
  mkdir "$RUN_DIR/bin/$cohort"
  COHORT_COMPILER="$RUN_DIR/bin/$cohort/kio"
  # Online merging bounds each cohort's storage by binary signature.
  {
    printf '#!/bin/sh\n'
    printf "LLVM_PROFILE_FILE='%s/%s-%%m.profraw'\nexport LLVM_PROFILE_FILE\n" "$COV" "$cohort"
    printf "exec '%s' \"\$@\"\n" "$RUN_DIR/bin/kio"
  } >"$COHORT_COMPILER"
  chmod +x "$COHORT_COMPILER"
  impl_args="$RUN_DIR/impl-args.txt"
  write_impl_args "$impl_args"

  set -- "--cases-dir=$cases_dir" "--cache-base=$cache_dir"
  while IFS= read -r arg; do
    set -- "$@" "$arg"
  done <"$impl_args"
  set -- "$@" "--check=$REPO_ROOT/ci/checks/per-case/prime-marker.sh"
  if [ "$include_full_checks" = 1 ]; then
    set -- "$@" \
      "--check=$REPO_ROOT/ci/checks/per-case/fmt-canonical.sh" \
      "--check=$REPO_ROOT/ci/checks/per-case/kio-prime-roundtrip.sh"
  fi
  if [ "$SAMPLE_IMPL_MODE" = 1 ]; then
    set -- "$@" "--impls=SAMPLE_IMPL"
  fi
  if [ -n "$JOBS" ]; then
    set -- "$@" "--jobs=$JOBS"
  fi

  KIO_PRIME_CHECK_BIN="$ORCHESTRATOR_TMP/bin/kio-prime-check" \
    sh "$REPO_ROOT/ci/run-tests.sh" "$@" >"$log_file" 2>&1
}

build_instrumented_kio() {
  COV="$ORCHESTRATOR_TMP/coverage"
  mkdir "$COV"
  (
    cd "$REPO_ROOT/kio-rs"
    CARGO_TARGET_DIR="$COV" sh "$REPO_ROOT/ci/cargo.sh" llvm-cov --release show-env --export-prefix
  ) >"$RUN_DIR/show-env.sh" 2>"$RUN_DIR/show-env.log"
  flags=$(grep -Ec '^export (RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS)=' "$RUN_DIR/show-env.sh" || :)
  if [ "$flags" -ne 1 ] || ! grep -q 'instrument-coverage' "$RUN_DIR/show-env.sh" \
      || ! grep -q '^export LLVM_PROFILE_FILE=' "$RUN_DIR/show-env.sh" \
      || ! grep -q '^export CARGO_LLVM_COV_TARGET_DIR=' "$RUN_DIR/show-env.sh"; then
    printf 'error: show-env omitted authenticated coverage settings\n' >&2; exit 1
  fi
  if ! (
    cd "$REPO_ROOT/kio-rs"
    coverage_env=$(command cat "$RUN_DIR/show-env.sh")
    eval "$coverage_env"
    [ "$CARGO_LLVM_COV_TARGET_DIR" = "$COV" ] || {
      printf 'error: coverage target differs from isolated target\n' >&2; exit 1
    }
    CARGO_TARGET_DIR="$COV" sh "$REPO_ROOT/ci/cargo.sh" build --release --bin kio
  ) >"$RUN_DIR/compiler-build.log" 2>&1; then
    cat "$RUN_DIR/compiler-build.log" >&2; exit 1
  fi
  cp "$COV/release/kio" "$RUN_DIR/bin/kio"
  git hash-object "$COV/release/kio" >"$RUN_DIR/compiler.hash"
  cmp "$COV/release/kio" "$RUN_DIR/bin/kio"
  printf 'profile=release\nfeatures=default\ncompiler=%s\n' "$RUN_DIR/bin/kio" >"$RUN_DIR/compiler.meta"
  clear_profiles
}

clear_profiles() {
  for raw in "$COV"/*.profraw; do
    [ -e "$raw" ] || continue
    if [ ! -f "$raw" ] || [ -L "$raw" ]; then
      printf 'error: invalid raw profile\n' >&2; exit 1
    fi
    rm "$raw"
  done
}

preserve_profiles() {
  label=$1
  cohort=$2
  found=0
  mkdir -p "$RUN_DIR/$label-profiles"
  for raw in "$COV/$cohort-"*.profraw; do
    [ -e "$raw" ] || continue
    if [ ! -f "$raw" ] || [ -L "$raw" ] || [ ! -s "$raw" ]; then
      printf 'error: invalid %s profile\n' "$cohort" >&2; exit 1
    fi
    cp "$raw" "$RUN_DIR/$label-profiles/"
    found=1
  done
  [ "$found" = 1 ] || { printf 'error: missing %s profiles\n' "$cohort" >&2; exit 1; }
}

line_coverage_from_lcov() {
  file=$1
  awk -F: '
    /^LF:/ { total += $2 }
    /^LH:/ { hit += $2 }
    END {
      if (total <= 0) exit 1
      printf "%.2f", (hit * 100.0) / total
    }
  ' "$file"
}

coverage_report() {
  label=$1
  out_lcov="$RUN_DIR/$label.lcov"
  ( cd "$REPO_ROOT/kio-rs" && CARGO_TARGET_DIR="$COV" CARGO_LLVM_COV_TARGET_DIR="$COV" \
    sh "$REPO_ROOT/ci/cargo.sh" llvm-cov --release report --lcov --failure-mode=any ) >"$out_lcov"
  awk '/^SF:/ { source=$0 } /^LF:/ { print source "\t" $0 }' "$out_lcov" | LC_ALL=C sort >"$RUN_DIR/$label-denominator.txt"
  [ -s "$RUN_DIR/$label-denominator.txt" ] || { printf 'error: empty coverage denominator\n' >&2; exit 1; }
  line_coverage_from_lcov "$out_lcov"
}

run_pass() {
  label=$1
  include_gen=$2
  printf 'coverage-delta: running %s pass\n' "$label" >&2
  run_tests_for_cases \
    "$REPO_ROOT/test-data/goldens" \
    "$RUN_DIR/cache-$label-goldens" \
    "$RUN_DIR/$label-goldens.log" \
    1 \
    "$label-golden"
  preserve_profiles "$label" "$label-golden"
  awk '/^(pass|warn) \[/ { print }' "$RUN_DIR/$label-goldens.log" | LC_ALL=C sort >"$RUN_DIR/$label-goldens-selected.txt"
  [ -s "$RUN_DIR/$label-goldens-selected.txt" ] || { printf 'error: no successful golden rows\n' >&2; exit 1; }
  if [ "$include_gen" = 1 ] && [ "$COUNT" -gt 0 ]; then
    run_tests_for_cases \
      "$BATCH" \
      "$RUN_DIR/cache-$label-gen" \
      "$RUN_DIR/$label-kio-gen.log" \
      0 \
      "$label-gen"
    preserve_profiles "$label" "$label-gen"
  fi
  coverage_report "$label"
}

build_instrumented_kio
baseline=$(run_pass baseline 0)
clear_profiles
with_gen=$(run_pass with-kio-gen 1)
cmp "$RUN_DIR/baseline-goldens-selected.txt" "$RUN_DIR/with-kio-gen-goldens-selected.txt" || {
  printf 'error: golden execution selections differ between passes\n' >&2; exit 1
}
cmp "$RUN_DIR/baseline-denominator.txt" "$RUN_DIR/with-kio-gen-denominator.txt" || {
  printf 'error: coverage denominators differ between passes\n' >&2; exit 1
}
[ "$(git hash-object "$RUN_DIR/bin/kio")" = "$(command cat "$RUN_DIR/compiler.hash")" ] || {
  printf 'error: executed compiler identity changed\n' >&2; exit 1
}
delta=$(awk -v a="$baseline" -v b="$with_gen" 'BEGIN { printf "%.2f", b - a }')

printf '\ncoverage-delta report\n'
printf '  seed: %s\n' "$SEED"
printf '  generated cases: %s\n' "$COUNT"
printf '  impls: %s\n' "$IMPLS"
printf '  goldens-alone line coverage: %s%%\n' "$baseline"
printf '  goldens + kio-gen line coverage: %s%%\n' "$with_gen"
printf '  kio-gen delta: %+0.2f%%\n' "$delta"
if [ "$KEEP_WORK" = 1 ]; then
  printf '  logs: %s\n' "$RUN_DIR"
fi
COMPLETE=1
