#!/bin/sh
# Local annotations type bare literals; visible host types alone do not.
set -eu

(cd workdir && "$KIO_BIN" check)

probe=$(mktemp -d)
trap 'rm -rf "$probe"' EXIT HUP INT TERM
cp -R workdir "$probe/workdir"
cp unanchored.kio "$probe/workdir/lituniq/main.kio"

set +e
(cd "$probe/workdir" && "$KIO_BIN" check) >"$probe/stdout" 2>"$probe/stderr"
status=$?
set -e
if [ "$status" -ne 14 ]; then
  cat "$probe/stderr" >&2
  echo "unanchored literal should fail with type error 14, got $status" >&2
  exit 1
fi
grep -F 'cannot determine the type of this integer literal' "$probe/stderr" >/dev/null
grep -F 'let n = 100' "$probe/stderr" >/dev/null
