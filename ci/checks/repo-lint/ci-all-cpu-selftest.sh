#!/bin/sh
set -eu
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
ALL_SH="$REPO_ROOT/ci/all.sh"
CPU_SUM="$REPO_ROOT/ci/infra/ci-time-cpu-sum.sh"
fail() {
  printf 'ci-all-cpu-selftest: %s\n' "$1" >&2
  exit 1
}
grep -Fq -- '-f "KIO_CI_CPU %U %S"' "$ALL_SH" ||
  fail 'GNU time CPU record is not tagged'
grep -Fq "sh \"\$CPU_SUM\" \"\$cpu_file\"" "$ALL_SH" ||
  fail 'per-task reporting bypasses the shared parser'
grep -Fq "sh \"\$CPU_SUM\" \"\$TMPDIR_LOGS\"/logs/*.cpu" "$ALL_SH" ||
  fail 'aggregate reporting bypasses the shared parser'
grep -Fq ": >\"\$cpu_file\"" "$ALL_SH" ||
  fail 'scheduled tasks do not publish CPU placeholders'
[ -f "$CPU_SUM" ] || fail 'shared parser is missing'
scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/ci-all-cpu-selftest.XXXXXX") ||
  fail 'cannot make scratch directory'
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
printf 'KIO_CI_CPU 1.25 0.75\n' >"$scratch/success"
printf 'Command exited with non-zero status 1\nKIO_CI_CPU 2.50 0.50\n' >"$scratch/failed"
printf 'Command terminated by signal 15\nKIO_CI_CPU 3.00 1.00\n' >"$scratch/signalled"
printf 'KIO_CI_CPU 4.25 2.75\n' >"$scratch/other"
printf 'KIO_CI_CPU broken 1.00\n' >"$scratch/malformed"
printf 'KIO_CI_CPU 1.00 1.00\nKIO_CI_CPU 2.00 2.00\n' >"$scratch/duplicate"
printf '1.00 2.00\n' >"$scratch/untagged"
: >"$scratch/missing"
[ "$(sh "$CPU_SUM" "$scratch/success")" = 2.0 ] || fail 'success sum is wrong'
[ "$(sh "$CPU_SUM" "$scratch/failed")" = 3.0 ] || fail 'failed-task sum is wrong'
[ "$(sh "$CPU_SUM" "$scratch/signalled")" = 4.0 ] || fail 'signalled-task sum is wrong'
[ "$(sh "$CPU_SUM" "$scratch/failed" "$scratch/other")" = 10.0 ] ||
  fail 'multi-file sum is wrong'
if sh "$CPU_SUM" "$scratch/malformed" >/dev/null 2>&1; then
  fail 'malformed tagged data was accepted'
fi
if sh "$CPU_SUM" "$scratch/duplicate" >/dev/null 2>&1; then
  fail 'duplicate tagged data was accepted'
fi
if sh "$CPU_SUM" "$scratch/untagged" >/dev/null 2>&1; then
  fail 'untagged data was accepted'
fi
if sh "$CPU_SUM" "$scratch/success" "$scratch/missing" >/dev/null 2>&1; then
  fail 'partially missing aggregate data was accepted'
fi
printf 'ci-all-cpu-selftest: ok\n'
