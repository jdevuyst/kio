#!/bin/sh
# Formatter parse failures retain the parser's help and replacement in both
# file and stdin modes, with and without --check. No failing mode writes output
# or changes the source; every invocation preserves the parse-error category.
set -eu
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
cp workdir/main.kio "$work/main.kio"
for mode in file stdin check-file check-stdin; do
    case "$mode" in
        file) set -- "$work/main.kio" ;;
        stdin) set -- - ;;
        check-file) set -- --check "$work/main.kio" ;;
        check-stdin) set -- --check - ;;
    esac
    status=0
    "$KIO_BIN" fmt "$@" <workdir/main.kio >"$work/stdout" 2>"$work/stderr" || status=$?
    cat "$work/stderr" >&2
    test "$status" -eq 11
    test ! -s "$work/stdout"
    cmp workdir/main.kio "$work/main.kio"
    grep -q 'help:.*unit type' "$work/stderr"
    # shellcheck disable=SC2016 # Backticks delimit the literal replacement text.
    grep -q 'suggestion:.*replace with `[[:space:]]*\.[[:space:]]*`' "$work/stderr"
    test "$(tail -c 1 "$work/stderr" | od -An -tu1 | tr -d '[:space:]')" = 10
done
exit 11
