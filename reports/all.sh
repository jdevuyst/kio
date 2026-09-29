#!/bin/sh
#
# Run every reporting harness in reports/ sequentially.
#
# Counterpart to ci/all.sh — same gating-vs-reporting split: ci/all.sh
# drives the gating checks (any failure blocks merge), this script
# drives the reporting harnesses (information for humans, never
# gating). The reports/ directory is deliberately a sibling of ci/,
# not a child, to reflect this split. Most harnesses are audit-skill-owned;
# evaluator reachability is a deliberate direct local diagnostic. This
# umbrella stays as the bulk-runner for ad-hoc local use.
#
# Reports run sequentially because each (cargo-fuzz, cargo-mutants,
# cargo-llvm-cov) is long enough that interleaving would muddle
# output more than it would save wall-clock. Graduate to ci/all.sh's
# parallel pattern if reports/ grows enough to make sequential
# painful.
#
# Each script's exit code is propagated unchanged: a reporting
# harness should exit 0 when it succeeds in producing its report
# (even if the report names findings) and non-zero only on
# infrastructure failure. This umbrella exits non-zero iff any
# script does.
#
# Individual scripts can be invoked directly with their own flags
# (e.g. `sh reports/kio-gen-coverage-delta.sh --count=100 --seed=42`);
# this umbrella runs each with no extra arguments.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

failures=
for script in "$SCRIPT_DIR"/*.sh; do
  [ -f "$script" ] || continue
  [ "${script##*/}" = "all.sh" ] && continue
  name=${script##*/}
  printf '\n========== %s ==========\n' "$name"
  if ! sh "$script"; then
    failures="$failures $name"
  fi
done

if [ -n "$failures" ]; then
  printf '\nFAILED:\n' >&2
  for n in $failures; do
    printf '  %s\n' "$n" >&2
  done
  exit 1
fi
