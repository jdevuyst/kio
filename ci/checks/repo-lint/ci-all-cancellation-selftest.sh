#!/bin/sh

# Exercise the actual abort handler with signal delivery replaced by recording.
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/ci-all-cancellation-selftest.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

fail() {
  printf 'ci-all-cancellation-selftest: %s\n' "$*" >&2
  exit 1
}

[ "$(grep -c '^on_exit() {$' "$REPO_ROOT/ci/all.sh")" -eq 1 ] ||
  fail 'expected exactly one abort handler'
awk '
  /^on_exit\(\) \{$/ { copying=1 }
  copying { print }
  copying && /^}$/ { complete=1; exit }
  END { if (!complete) exit 1 }
' "$REPO_ROOT/ci/all.sh" >"$scratch/on-exit.sh" ||
  fail 'cannot extract the complete abort handler'

mkdir -p "$scratch/records/logs"
printf '2\n' >"$scratch/records/logs/01.pgid"
printf '2147483647\n' >"$scratch/records/logs/02.pgid"
printf '10\n' >"$scratch/records/logs/03.pgid"
record=3
for invalid in '' 0 1 00 01 0002 -2 +2 ' 2' '2 ' '2
3' 2147483648 4294967295 4294967296 4294967297 \
  99999999999999999999999999999999999999999999999999 fixture
do
  record=$((record + 1))
  printf '%s\n' "$invalid" >"$scratch/records/logs/invalid-$record.pgid"
done

# Function lookup intercepts the shell builtin too; a PATH-only fake kill
# would leave the dangerous delivery path active. This child sources only the
# extracted function, never the gate's startup or task dispatch.
sh -c '
  set -eu
  signals=$1
  TMPDIR_LOGS=$2
  cleanup_source=$3
  kill() { printf "%s %s\n" "$1" "$2" >>"$signals"; }
  sleep() { :; }
  rm() { :; }
  emit_done() { printf "DONE %s\n" "$1"; }
  done_status=
  KEEP_LOGS=1
  PROG=ci-all-cancellation-selftest
  . "$cleanup_source"
  set +e
  (exit 143)
  on_exit
' sh "$scratch/signals" "$scratch/records" "$scratch/on-exit.sh" \
  >"$scratch/cleanup.log" 2>&1 || fail 'abort handler failed'

printf '%s\n' '-TERM -2' '-TERM -2147483647' '-TERM -10' \
  '-KILL -2' '-KILL -2147483647' '-KILL -10' >"$scratch/expected-signals"
if ! cmp -s "$scratch/expected-signals" "$scratch/signals"; then
  printf 'ci-all-cancellation-selftest: unexpected intercepted signal targets:\n' >&2
  cat "$scratch/signals" >&2
  fail 'abort handler accepted an unsafe group or rejected a valid group'
fi
grep -Fxq 'DONE aborted (exit 143)' "$scratch/cleanup.log" ||
  fail 'abort status was not preserved'

# The forwarding fixture does not own real process groups. Its fake /proc
# record must remain nonnumeric even if production validation regresses.
awk '
  /^cat >"\$fixture\/bin\/sed"/ { copying=1; next }
  copying && /^EOF$/ { complete=1; exit }
  copying { print }
  END { if (!complete) exit 1 }
' "$SCRIPT_DIR/ci-all-case-forwarding-selftest.sh" >"$scratch/fake-sed.sh" ||
  fail 'cannot extract the forwarding fixture process record'
fake_group=$(sh "$scratch/fake-sed.sh" /proc/fixture/stat | cut -d' ' -f3)
case "$fake_group" in
  *[!0-9]*) ;;
  *) fail 'forwarding fixture exposes a numeric process-group identifier' ;;
esac

printf 'ci-all-cancellation-selftest: ok (intercepted TERM/KILL targets; nonsignalable fixture groups)\n'
