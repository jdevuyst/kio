#!/bin/sh
# Kiodoc formatting preserves parser repairs and identifies their virtual
# source without changing a Markdown file containing an invalid snippet.
set -eu
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
cp pkg.pkg.kio "$work/pkg.pkg.kio"
for source in direct harness accumulate file_backed placeholder variant; do
    cp "$source.md" "$work/$source.md"
    for mode in check rewrite; do
        if [ "$mode" = check ]; then
            set -- --check "$source.md"
        else
            set -- "$source.md"
        fi
        status=0
        (cd "$work" && "$KIO_BIN" doc fmt "$@") >"$work/stdout" 2>"$work/stderr" || status=$?
        cat "$work/stderr" >&2
        test "$status" -eq 70
        test ! -s "$work/stdout"
        cmp "$source.md" "$work/$source.md"
        grep -q "$source.md:[0-9].*snippet at line" "$work/stderr"
        grep -q 'fn value(x: ())' "$work/stderr"
        grep -q '[|].*\^' "$work/stderr"
        grep -q 'help:.*unit type' "$work/stderr"
        # shellcheck disable=SC2016 # Backticks delimit literal suggested text.
        grep -q 'suggestion:.*replace with `[[:space:]]*\.[[:space:]]*`' "$work/stderr"
    done
done
exit 70
