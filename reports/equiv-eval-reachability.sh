#!/bin/sh
# Informational production-source reachability locator for normalization.rs.
# Findings exit zero; incomplete or malformed evidence exits nonzero.
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
ANALYZER=$SCRIPT_DIR/lib/equiv-eval-reachability.mjs
MANIFEST=$SCRIPT_DIR/equiv-eval-reachability-goldens.txt
# shellcheck disable=SC1091
. "$REPO_ROOT/ci/checks/orchestrators/lib/common.sh"

usage() {
  printf 'Usage: sh %s [--self-test]\n' "$0"
  printf 'Locate unexecuted production normalization.rs functions and source-line ranges.\n'
}

case "${1:-}" in
  '') ;;
  --self-test)
    [ "$#" -eq 1 ] || { usage >&2; exit 2; }
    command -v node >/dev/null 2>&1 || { printf 'error: Node.js is required\n' >&2; exit 2; }
    node "$ANALYZER" self-test "$REPO_ROOT/kio-rs/src/normalization.rs"
    exit 0
    ;;
  -h|--help) usage; exit 0 ;;
  *) usage >&2; exit 2 ;;
esac
[ "$#" -eq 0 ] || { usage >&2; exit 2; }
command -v node >/dev/null 2>&1 || { printf 'error: Node.js is required\n' >&2; exit 2; }
command -v cargo-llvm-cov >/dev/null 2>&1 || {
  printf 'error: cargo-llvm-cov is absent; run sh ci/impl-toolchain.sh install-report-tools\n' >&2
  exit 2
}
CR=$(printf '\r')
case "$REPO_ROOT" in
  *','*|*"'"*|*'%'*|*"$CR"*|*'
'*) printf 'error: repository path contains an unsupported comma, quote, percent, or newline\n' >&2; exit 2 ;;
esac

RUN_ROOT=$REPO_ROOT/target/reports/equiv-eval-reachability
RUN_DIR=$RUN_ROOT/run-$$
mkdir -p "$RUN_ROOT"
mkdir "$RUN_DIR" || { printf 'error: evidence directory exists: %s\n' "$RUN_DIR" >&2; exit 1; }
init_orchestrator_tmp equiv-eval-reachability
COMPLETE=0
cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  if [ "$COMPLETE" = 1 ] && [ "$status" -eq 0 ]; then
    rm -rf "$ORCHESTRATOR_TMP"
  else
    printf 'equiv-eval reachability failed; isolated state kept at %s\n' "$ORCHESTRATOR_TMP" >&2
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
case "$ORCHESTRATOR_TMP" in
  *','*|*"'"*|*'%'*|*"$CR"*|*'
'*) printf 'error: profile/wrapper path contains an unsupported comma, quote, percent, or newline\n' >&2; exit 2 ;;
esac

windows=0
case "${OS:-}:${MSYSTEM:-}" in Windows_NT:*|*:MINGW*|*:MSYS*|*:UCRT*) windows=1 ;; esac
if [ "$windows" = 1 ]; then
  command -v cygpath >/dev/null 2>&1 || { printf 'error: Git Bash requires cygpath\n' >&2; exit 2; }
  native() { cygpath -aw "$1"; }
  posix() { cygpath -au "$1"; }
  exe=.exe
else
  native() { printf '%s\n' "$1"; }
  posix() { printf '%s\n' "$1"; }
  exe=
fi

KIO_CI_SCHEDULER_BIN=$(
  unset LLVM_PROFILE_FILE CARGO_LLVM_COV_TARGET_DIR CARGO_LLVM_COV CARGO_LLVM_COV_SHOW_ENV RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTDOCFLAGS CARGO_ENCODED_RUSTDOCFLAGS
  sh "$REPO_ROOT/ci/schedule.sh" --prepare
) || exit $?
export KIO_CI_SCHEDULER_BIN
counts=$(node "$ANALYZER" inventory "$REPO_ROOT/test-data/goldens" "$MANIFEST" "$REPO_ROOT/test-data/poc" "$RUN_DIR")
GOLDENS=${counts%% *}
POCS=${counts#* }
[ "$counts" = "$GOLDENS $POCS" ] || { printf 'error: invalid inventory cardinalities: %s\n' "$counts" >&2; exit 1; }
printf 'equiv-eval reachability: %s focused goldens, %s POCs\n' "$GOLDENS" "$POCS"

SUPPORT=$ORCHESTRATOR_TMP/runner-target
SUPPORT_NATIVE=$(native "$SUPPORT")
mkdir -p "$SUPPORT" "$ORCHESTRATOR_TMP/bin"
if ! (
  unset LLVM_PROFILE_FILE CARGO_LLVM_COV_TARGET_DIR CARGO_LLVM_COV CARGO_LLVM_COV_SHOW_ENV RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTDOCFLAGS CARGO_ENCODED_RUSTDOCFLAGS
  cd "$REPO_ROOT/ci/infra/kio-test-runner-rs"
  CARGO_TARGET_DIR=$SUPPORT_NATIVE sh "$REPO_ROOT/ci/cargo.sh" build --no-default-features --features js --bin kio-test-runner-js
) >"$RUN_DIR/runner-build.log" 2>&1; then
  cat "$RUN_DIR/runner-build.log" >&2; exit 1
fi
RUNNER=$ORCHESTRATOR_TMP/bin/kio-test-runner-js$exe
cp "$SUPPORT/debug/kio-test-runner-js$exe" "$RUNNER"

COVERAGE_BASE=$ORCHESTRATOR_TMP/coverage
COVERAGE_BASE_NATIVE=$(native "$COVERAGE_BASE")
mkdir -p "$COVERAGE_BASE"
if ! (
  unset LLVM_PROFILE_FILE CARGO_LLVM_COV_TARGET_DIR CARGO_LLVM_COV CARGO_LLVM_COV_SHOW_ENV
  cd "$REPO_ROOT/kio-rs"
  CARGO_TARGET_DIR=$COVERAGE_BASE_NATIVE sh "$REPO_ROOT/ci/cargo.sh" llvm-cov --profile=dev --all-features show-env --export-prefix --remap-path-prefix
) >"$RUN_DIR/show-env.sh" 2>"$RUN_DIR/show-env.log"; then
  cat "$RUN_DIR/show-env.log" >&2; exit 1
fi
flags=$(grep -Ec '^export (RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS)=' "$RUN_DIR/show-env.sh" || :)
if [ "$flags" -ne 1 ] || ! grep -q 'instrument-coverage' "$RUN_DIR/show-env.sh" \
    || ! grep -q '^export LLVM_PROFILE_FILE=' "$RUN_DIR/show-env.sh" \
    || ! grep -q '^export CARGO_LLVM_COV_TARGET_DIR=' "$RUN_DIR/show-env.sh"; then
  printf 'error: cargo llvm-cov show-env did not emit one authenticated instrumentation family\n' >&2; exit 1
fi
coverage_env=$(command cat "$RUN_DIR/show-env.sh")
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTDOCFLAGS CARGO_ENCODED_RUSTDOCFLAGS
eval "$coverage_env"
: "${CARGO_LLVM_COV_TARGET_DIR:?show-env omitted coverage target}"
COV_NATIVE=$CARGO_LLVM_COV_TARGET_DIR
COV=$(posix "$COV_NATIVE")
[ "$COV" = "$COVERAGE_BASE" ] || {
  printf 'error: show-env coverage target differs from isolated target: %s\n' "$COV" >&2; exit 1
}
mkdir -p "$COV"
export CARGO_TARGET_DIR="$COV_NATIVE"

run_kio() {
  log=$1; shift
  if ! (cd "$REPO_ROOT/kio-rs" && "$@") >"$RUN_DIR/$log" 2>&1; then
    cat "$RUN_DIR/$log" >&2; return 1
  fi
}
run_kio lib-build.log sh "$REPO_ROOT/ci/cargo.sh" test --no-run --lib --profile=dev --all-features
run_kio bins-build.log sh "$REPO_ROOT/ci/cargo.sh" build --bin kio --bin kio-prime --profile=dev --all-features
mv "$COV/debug/kio$exe" "$ORCHESTRATOR_TMP/bin/coverage-kio$exe"
mv "$COV/debug/kio-prime$exe" "$ORCHESTRATOR_TMP/bin/coverage-kio-prime$exe"
run_kio profile-clean.log sh "$REPO_ROOT/ci/cargo.sh" llvm-cov clean --profraw-only
node "$ANALYZER" profiles "$COV" empty
run_kio unit.log env "LLVM_PROFILE_FILE=$COV_NATIVE/unit-%p-%m.profraw" \
  sh "$REPO_ROOT/ci/cargo.sh" test --lib --profile=dev --all-features
mv "$ORCHESTRATOR_TMP/bin/coverage-kio$exe" "$COV/debug/kio$exe"
mv "$ORCHESTRATOR_TMP/bin/coverage-kio-prime$exe" "$COV/debug/kio-prime$exe"

REAL_KIO=$COV/debug/kio$exe
REAL_PRIME=$COV/debug/kio-prime$exe
for binary in "$REAL_KIO" "$REAL_PRIME" "$RUNNER"; do
  if [ ! -f "$binary" ] || [ ! -x "$binary" ]; then
    printf 'error: missing executable: %s\n' "$binary" >&2; exit 1
  fi
done
# Corpus infrastructure must not inherit instrumentation. Only the wrappers
# around the already-built compiler binaries receive cohort profile paths.
unset LLVM_PROFILE_FILE CARGO_LLVM_COV_TARGET_DIR CARGO_LLVM_COV CARGO_LLVM_COV_SHOW_ENV \
  RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTDOCFLAGS CARGO_ENCODED_RUSTDOCFLAGS CARGO_TARGET_DIR

make_wrapper() {
  wrapper=$1 real=$2 profile=$3
  {
    printf '#!/bin/sh\n'
    printf "LLVM_PROFILE_FILE='%s'\nexport LLVM_PROFILE_FILE\n" "$profile"
    printf "exec '%s' \"\$@\"\n" "$real"
  } >"$wrapper"
  chmod +x "$wrapper"
}

run_cohort() {
  label=$1 cases=$2 selectors=$3 expected=$4 prefix=$5
  bindir=$ORCHESTRATOR_TMP/$prefix-bin
  mkdir "$bindir"
  make_wrapper "$bindir/kio" "$REAL_KIO" "$COV_NATIVE/$prefix-%p-%m.profraw"
  make_wrapper "$bindir/kio-prime" "$REAL_PRIME" "$COV_NATIVE/$prefix-%p-%m.profraw"
  set -- "--cases-dir=$cases" "--cache-base=$ORCHESTRATOR_TMP/$prefix-cache" \
    "--impl-def=name=kio@js,kio=$bindir/kio,runner=$RUNNER,target=js,prime-kio=$bindir/kio-prime"
  while IFS= read -r selector || [ -n "$selector" ]; do [ -n "$selector" ] && set -- "$@" "$selector"; done <"$selectors"
  if ! (cd "$REPO_ROOT" && sh ci/run-tests.sh "$@") >"$RUN_DIR/$prefix.log" 2>&1; then
    cat "$RUN_DIR/$prefix.log" >&2; return 1
  fi
  node "$ANALYZER" summary "$RUN_DIR/$prefix.log" "$expected" "$label"
}
run_cohort focused-equiv-goldens test-data/goldens "$RUN_DIR/golden-selectors.txt" "$GOLDENS" golden
run_cohort all-pocs test-data/poc "$RUN_DIR/poc-selectors.txt" "$POCS" poc
node "$ANALYZER" profiles "$COV" complete >"$RUN_DIR/profile-counts.txt"

report() {
  format=$1 output=$2
  if ! (cd "$REPO_ROOT/kio-rs" && CARGO_LLVM_COV_TARGET_DIR=$COV_NATIVE CARGO_TARGET_DIR=$COVERAGE_BASE_NATIVE \
    sh "$REPO_ROOT/ci/cargo.sh" llvm-cov --profile=dev --all-features report "--$format" --failure-mode=any
  ) >"$output" 2>"$RUN_DIR/$format-export.log"; then
    cat "$RUN_DIR/$format-export.log" >&2; return 1
  fi
  [ -s "$output" ] || { printf 'error: empty %s export\n' "$format" >&2; return 1; }
}
report lcov "$RUN_DIR/coverage-union.lcov"
report json "$RUN_DIR/coverage-union.json"
SOURCE=$REPO_ROOT/kio-rs/src/normalization.rs
SOURCE_NATIVE=$(native "$SOURCE")
node "$ANALYZER" analyze "$RUN_DIR/coverage-union.lcov" "$RUN_DIR/coverage-union.json" \
  "$SOURCE" "$SOURCE_NATIVE" "$RUN_DIR/equiv-eval-reachability.json" >"$RUN_DIR/locator.txt"
cat "$RUN_DIR/locator.txt"
printf 'machine-readable evidence: %s\n' "$RUN_DIR/equiv-eval-reachability.json"
printf 'complete evidence directory: %s\n' "$RUN_DIR"
COMPLETE=1
