#!/bin/sh
# One exact source nominal may be selected by only one `retype` statement,
# even when two statements spell the same target. The materializer must reject
# the later selector before writing dependency output. The path dependency and
# scratch copy keep the case hermetic and deterministic.
set -u

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

scratch=$(mktemp -d) || fail "cannot make scratch directory"
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_retype_duplicate_selector() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$scratch"
  exit "$cleanup_status"
}
trap cleanup_retype_duplicate_selector EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

cp -R workdir "$scratch/consumer" || fail "cannot copy consumer fixture"
cp -R library "$scratch/library" || fail "cannot copy dependency fixture"

cd "$scratch/consumer" || fail "cannot enter consumer fixture"
"$KIO_BIN" dep fetch --force
status=$?
if [ -e widget/store.kio ]; then
  fail "duplicate retypes wrote dependency output before rejection"
fi
exit "$status"
