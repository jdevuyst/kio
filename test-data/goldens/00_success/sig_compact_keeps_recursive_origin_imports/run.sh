#!/bin/sh
# A compact boundary must keep distinct recursive contexts from the same
# module in separate sections. These groups were introduced under incompatible
# `dep` aliases; merging their sections makes the compacted history invalid.
set -u

scratch=$(mktemp -d) || { printf 'cannot make scratch dir\n' >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT
cp workdir/app.pkg.kio workdir/app.sig.kio workdir/api.kio \
  workdir/left.kio workdir/right.kio "$scratch/" || exit 1
cd "$scratch" || exit 1

expect_exit() {
  want=$1
  shift
  "$@" >/dev/null 2>&1
  got=$?
  if [ "$got" -ne "$want" ]; then
    # shellcheck disable=SC2016
    printf 'expected exit %s from `%s`, got %s\n' "$want" "$*" "$got" >&2
    exit 1
  fi
}

expect_exit 0 "$KIO_BIN" sig status
expect_exit 0 "$KIO_BIN" sig compact 3
expect_exit 0 "$KIO_BIN" sig status

log=$("$KIO_BIN" sig log) || exit 1
api_sections=$(printf '%s\n' "$log" | grep -c '^    module api {$')
[ "$api_sections" -eq 2 ] || {
  printf 'compacted boundary must keep two recursive api origin sections\n' >&2
  printf '%s\n' "$log" >&2
  exit 1
}
printf '%s\n' "$log" | grep -q 'import left as dep;' || exit 1
printf '%s\n' "$log" | grep -q 'import right as dep;' || exit 1
