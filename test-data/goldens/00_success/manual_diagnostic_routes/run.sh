#!/bin/sh
# Package diagnostics expose the same offline and release-pinned web routes as help.
set -eu
mkdir -p out
work=$(mktemp -d out/manual-routes.XXXXXX)
trap 'rm -rf "$work"' EXIT HUP INT TERM
check_route() {
  set +e
  (cd "$work" && "$KIO_BIN" "$@") >"$work/stdout" 2>"$work/stderr"
  status=$?
  set -e
  [ "$status" = 40 ] && [ ! -s "$work/stdout" ] || exit 1
  grep -F 'kio manual cat specs/package.md' "$work/stderr" >/dev/null
  grep -F 'https://github.com/jdevuyst/kio/blob/releases/v' "$work/stderr" >/dev/null
  grep -E '/specs/package.md#(package-file|build-target-files)' "$work/stderr" >/dev/null
}
check_route build
check_route build .
check_route cache path
check_route doc check
check_route doc check .
printf '%s\n' 'module main;' 'fn misapply[A][B](x: A(B)) -> A(B) { x }' >"$work/main.kio"
set +e
(cd "$work" && "$KIO_BIN" check) >"$work/stdout" 2>"$work/stderr"
status=$?
set -e
[ "$status" = 14 ] && [ ! -s "$work/stdout" ] || exit 1
grep -F 'kio manual cat specs/grammar.md' "$work/stderr" >/dev/null
grep -E 'https://github.com/jdevuyst/kio/blob/releases/v.*/specs/grammar.md#kind-grammar' "$work/stderr" >/dev/null
