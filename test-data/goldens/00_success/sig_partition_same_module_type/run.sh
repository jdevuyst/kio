#!/bin/sh
# Host requirements and exports occupy different verdict partitions while
# referring to the same module-local type in one signature epoch.
set -u

scratch=$(mktemp -d) || exit 1
trap 'rm -rf "$scratch"' EXIT
cp workdir/app.pkg.kio workdir/api.kio "$scratch/" || exit 1
cd "$scratch" || exit 1

"$KIO_BIN" sig stage --force --stdout > history.txt || exit $?
cp history.txt app.sig.kio || exit 1

expect_exit() {
  want=$1
  shift
  "$@" > command.txt 2>&1
  got=$?
  if [ "$got" -ne "$want" ]; then
    printf 'expected exit %s, got %s: %s\n' "$want" "$got" "$*" >&2
    cat command.txt >&2
    exit 1
  fi
}

expect_exit 82 "$KIO_BIN" sig status
expect_exit 0 "$KIO_BIN" sig commit
expect_exit 0 "$KIO_BIN" sig status
expect_exit 0 "$KIO_BIN" sig log
printf 'partitioned signature replays\n'
