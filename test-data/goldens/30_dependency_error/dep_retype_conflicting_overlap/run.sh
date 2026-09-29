#!/bin/sh
# Two selectors for one exact source newtype name distinct targets. Target
# choice cannot depend on source order: the second selector is invalid before
# the materializer writes dependency output. The path dependency and scratch
# copy keep the case hermetic and deterministic.
set -u

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

scratch=$(mktemp -d) || fail "cannot make scratch directory"
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_retype_conflicting_overlap() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$scratch"
  exit "$cleanup_status"
}
trap cleanup_retype_conflicting_overlap EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

cp -R workdir "$scratch/consumer" || fail "cannot copy consumer fixture"
cp -R library "$scratch/library" || fail "cannot copy dependency fixture"

cd "$scratch/consumer" || fail "cannot enter consumer fixture"
"$KIO_BIN" dep fetch --force
status=$?
if [ -e widget/store.kio ]; then
  fail "conflicting retypes wrote dependency output before rejection"
fi
exit "$status"
