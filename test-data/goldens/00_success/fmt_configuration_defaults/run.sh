#!/bin/sh
set -eu

# Formatting preserves omitted and explicit optional settings and removes
# ordinary grouping without evaluating configuration expressions.
# The routing target is incidental: this case observes only the compiler CLI.
"$KIO_BIN" test >/dev/null
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM HUP

cp thread_safety.input "$work/pkg.pkg.kio"
"$KIO_BIN" fmt "$work/pkg.pkg.kio" >/dev/null
"$KIO_BIN" fmt --check "$work/pkg.pkg.kio" >/dev/null

for kind in pkg dep lock; do
  cp "$kind.input" "$work/pkg.$kind.kio"
  "$KIO_BIN" fmt "$work/pkg.$kind.kio" >/dev/null
  cmp "$kind.expected" "$work/pkg.$kind.kio"
  "$KIO_BIN" fmt --check "$work/pkg.$kind.kio" >/dev/null
done

for mode in send sync send_sync; do
  sed "s/send_sync/$mode/g" pkg.input > "$work/pkg.pkg.kio"
  "$KIO_BIN" fmt "$work/pkg.pkg.kio" >/dev/null
  grep -q "thread_safety $mode" "$work/pkg.pkg.kio"
  # These closed choices are bare configuration words, not strings.
  sed "s/send_sync/\"$mode\"/g" pkg.input > "$work/pkg.pkg.kio"
  status=0
  "$KIO_BIN" fmt "$work/pkg.pkg.kio" >/dev/null 2>&1 || status=$?
  test "$status" -eq 11
done

printf '%s\n' 'configuration presence and grouping are canonical'
