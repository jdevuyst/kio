#!/bin/sh
# Both diagnostic consumers retain actionable parse details without turning
# a broken package into a failed REPL process or changing the source file.
set -eu
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
cp workdir/main.kio "$work/main.kio"
(
    cd "$work"
    printf '%s\n' ':load main' ':quit' | "$KIO_BIN" repl >stdout 2>stderr
)
cmp workdir/main.kio "$work/main.kio"
for stream in stdout stderr; do
    grep -q 'help:.*unit type' "$work/$stream"
    # shellcheck disable=SC2016 # Backticks delimit the literal replacement text.
    grep -q 'suggestion:.*replace with `[[:space:]]*\.[[:space:]]*`' "$work/$stream"
    grep -q 'fn value(x: ())' "$work/$stream"
done
printf '%s\n' 'REPL startup and load preserve diagnostic details'
