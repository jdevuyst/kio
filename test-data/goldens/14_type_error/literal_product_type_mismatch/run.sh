#!/bin/sh
# The full compiler checks the free-T product mismatch; Prime requires T first.
# Keep phase-specific assertions here so either diagnostic cannot mask the other.
set -u

diag=$(mktemp)
trap 'rm -f "$diag"' EXIT HUP INT TERM
cd workdir || exit
"$KIO_BIN" check 2>"$diag"
status=$?
if [ "$status" -ne 14 ]; then
  cat "$diag" >&2
  printf 'expected type error 14, got %s\n' "$status" >&2
  exit 1
fi
case "$(basename "$KIO_BIN")" in
  kio|kio.exe) expected='type mismatch: expected `T & Witness`, found `Local`' ;;
  kio-prime|kio-prime.exe) expected='Kio'"'"' requires an explicit type argument for `[T]`' ;;
  *) printf 'unexpected compiler phase: %s\n' "$KIO_BIN" >&2; exit 1 ;;
esac
if ! grep -F "$expected" "$diag" >/dev/null; then
  cat "$diag" >&2
  printf 'missing phase-specific diagnostic: %s\n' "$expected" >&2
  exit 1
fi
cat "$diag" >&2
exit 14
