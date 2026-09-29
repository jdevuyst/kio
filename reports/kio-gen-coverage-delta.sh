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
cleanup_report() {
  cleanup_status=$?
  rm -rf "$ORCHESTRATOR_TMP"
  if [ "$KEEP_WORK" = 0 ]; then
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
rm -rf "$RUN_DIR"
mkdir -p "$RUN_DIR"

TAB=$(printf '\t')
ALL_IMPLS="kio@js${TAB}${REPO_ROOT}/kio-rs/target/release/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/release/kio-test-runner-js${TAB}js
kio@rust${TAB}${REPO_ROOT}/kio-rs/target/release/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/release/kio-test-runner-rust${TAB}rust"

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
}kio@js${TAB}${REPO_ROOT}/kio-rs/target/release/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/release/kio-test-runner-js${TAB}js"
          ;;
        kio@rust)
          SELECTED_IMPLS="${SELECTED_IMPLS:+$SELECTED_IMPLS
}kio@rust${TAB}${REPO_ROOT}/kio-rs/target/release/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/release/kio-test-runner-rust${TAB}rust"
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
fi
if [ "$need_rust" = 1 ]; then
  ( cd "$REPO_ROOT/ci/infra/kio-test-runner-rs" && sh "$REPO_ROOT/ci/cargo.sh" build --release --no-default-features --features rust )
fi

BATCH="$RUN_DIR/kio-gen-batch"
"$REPO_ROOT/ci/infra/kio-gen-rs/target/release/kio-gen" \
  --output "$BATCH" --count "$COUNT" --seed "$SEED" >/dev/null

write_impl_args() {
  out=$1
  : >"$out"
  printf '%s\n' "$SELECTED_IMPLS" | while IFS=$TAB read -r impl_name impl_kio impl_runner impl_target; do
    [ -n "$impl_name" ] || continue
    printf '%s\n' "--impl-def=name=${impl_name},kio=${impl_kio},runner=${impl_runner},target=${impl_target},prime-kio=${KIO_PRIME_BIN}" >>"$out"
  done
}

run_tests_for_cases() {
  cases_dir=$1
  cache_dir=$2
  log_file=$3
  include_full_checks=$4
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

  KIO_PRIME_CHECK_BIN="$REPO_ROOT/ci/infra/kio-prime-check-rs/target/release/kio-prime-check" \
    sh "$REPO_ROOT/ci/run-tests.sh" "$@" >"$log_file" 2>&1
}

build_instrumented_kio() {
  cd "$REPO_ROOT/kio-rs"
  sh "$REPO_ROOT/ci/cargo.sh" llvm-cov clean --workspace >/dev/null 2>&1 || true
  sh "$REPO_ROOT/ci/cargo.sh" clean -p kio-lang --release >/dev/null 2>&1 || true
  # `--export-prefix` prepends `export ` so the RUSTFLAGS / LLVM_PROFILE_FILE
  # / CARGO_LLVM_COV lines are sourceable. It replaces the old `--sh`, which
  # cargo-llvm-cov removed; the previous invocation swallowed the resulting
  # "invalid option" error via `2>/dev/null`, so instrumentation was never
  # set, no profraw was written, and the run failed 15 minutes later at
  # `report` with a bare "no input files". Capture the env explicitly and
  # fail loudly if show-env ever breaks again, rather than silently
  # producing an uninstrumented build. (show-env prints an advisory line to
  # stderr, which is left visible and does not enter the eval.)
  cov_env=$(sh "$REPO_ROOT/ci/cargo.sh" llvm-cov show-env --export-prefix) || {
    printf 'coverage-delta: cargo llvm-cov show-env failed — cannot instrument\n' >&2
    exit 1
  }
  eval "$cov_env"
  sh "$REPO_ROOT/ci/cargo.sh" build --release --bins >/dev/null
  cd "$REPO_ROOT"
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
  ( cd "$REPO_ROOT/kio-rs" && sh "$REPO_ROOT/ci/cargo.sh" llvm-cov report --release --lcov --summary-only ) >"$out_lcov"
  line_coverage_from_lcov "$out_lcov"
}

run_pass() {
  label=$1
  include_gen=$2
  printf 'coverage-delta: running %s pass\n' "$label" >&2
  build_instrumented_kio
  rm -rf "$RUN_DIR/cache-$label-goldens" "$RUN_DIR/cache-$label-gen"
  run_tests_for_cases \
    "$REPO_ROOT/test-data/goldens" \
    "$RUN_DIR/cache-$label-goldens" \
    "$RUN_DIR/$label-goldens.log" \
    1
  if [ "$include_gen" = 1 ]; then
    run_tests_for_cases \
      "$BATCH" \
      "$RUN_DIR/cache-$label-gen" \
      "$RUN_DIR/$label-kio-gen.log" \
      0
  fi
  coverage_report "$label"
}

baseline=$(run_pass baseline 0)
with_gen=$(run_pass with-kio-gen 1)
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
