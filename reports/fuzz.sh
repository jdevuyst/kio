#!/bin/sh
#
# Layer-(3c) fuzz signal for the parser and lexer entry points.
#
# Runs libFuzzer (via cargo-fuzz) against each target in
# kio-rs/fuzz/fuzz_targets/ for a bounded wall-clock budget. New
# crashes name parser or lexer bugs to investigate; surviving
# crashes get promoted to regression cases under
# test-data/goldens/<NN_category>/ keyed by the exit-code category the
# input eventually settles on.
#
# The fuzz crate lives at kio-rs/fuzz/ (separate workspace, see its
# Cargo.toml). cargo-fuzz requires nightly Rust because libFuzzer
# uses sanitizer-coverage instrumentation that's only available
# under nightly's `-Z` flags.
#
# Reporting semantics: each target run exits 0 on a clean run and
# non-zero when libFuzzer detects a crash. We treat both as
# expected outcomes and exit 0 from this script (a crash is data
# for the human reviewer, not a CI failure). Real harness errors
# (cargo-fuzz can't build, nightly missing) propagate. Each
# crashing input is saved by libFuzzer under
# kio-rs/fuzz/artifacts/<target>/; CI doesn't upload them as
# artifacts today, so reproduce locally via `sh reports/fuzz.sh
# --target=<name> --timeout=<longer>` if a finding looks
# promotable.
#
# Pre-req: nightly Rust toolchain and `cargo-fuzz` (install via
# `sh ci/impl-toolchain.sh install-report-tools`).
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)

# Per-target wall-clock budget in seconds. Two targets default
# to 180s each = 6 min total — short enough to fit comfortably
# inside an on-demand skill invocation while still catching most
# shallow bugs.
TIMEOUT=180
TARGET=

while [ $# -gt 0 ]; do
  case "$1" in
    --timeout=*) TIMEOUT=${1#--timeout=} ;;
    --timeout)
      shift
      [ $# -gt 0 ] || { printf 'error: --timeout requires a value\n' >&2; exit 2; }
      TIMEOUT=$1
      ;;
    --target=*) TARGET=${1#--target=} ;;
    --target)
      shift
      [ $# -gt 0 ] || { printf 'error: --target requires a value\n' >&2; exit 2; }
      TARGET=$1
      ;;
    -h|--help)
      cat <<EOF
Usage: sh $0 [--target=<name>] [--timeout=<seconds>]

Run libFuzzer (via cargo-fuzz) over the kio-rs parser and lexer
fuzz targets.

--target restricts the run to a single target (parse or lex).
Default runs every target in kio-rs/fuzz/fuzz_targets/.
--timeout caps per-target wall-clock at <seconds> (default 180).
EOF
      exit 0
      ;;
    *) printf 'error: unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

if ! sh "$REPO_ROOT/ci/cargo.sh" fuzz --version >/dev/null 2>&1; then
  printf 'error: cargo-fuzz is not installed.\n' >&2
  printf 'Install the pinned optional Cargo tools with:\n' >&2
  printf '  sh ci/impl-toolchain.sh install-report-tools\n' >&2
  exit 2
fi

cd "$REPO_ROOT/kio-rs"

# Enumerate targets. cargo-fuzz lists them based on the [[bin]]
# entries in kio-rs/fuzz/Cargo.toml.
if [ -n "$TARGET" ]; then
  targets=$TARGET
else
  targets=$(sh "$REPO_ROOT/ci/cargo.sh" +nightly fuzz list)
fi

overall_exit=0
for target in $targets; do
  printf '\n---- fuzz target: %s (budget %ds) ----\n' "$target" "$TIMEOUT"
  exit_code=0
  sh "$REPO_ROOT/ci/cargo.sh" +nightly fuzz run "$target" -- -max_total_time="$TIMEOUT" \
    || exit_code=$?
  case "$exit_code" in
    0)
      printf 'fuzz/%s: no crashes within budget\n' "$target"
      ;;
    77)
      # libFuzzer's "crash found" exit. cargo-fuzz forwards it.
      printf 'fuzz/%s: crash detected (see artifacts/ for input)\n' "$target"
      ;;
    *)
      printf 'fuzz/%s: cargo-fuzz failed with exit %d\n' "$target" "$exit_code" >&2
      exit "$exit_code"
      ;;
  esac
done

exit "$overall_exit"
