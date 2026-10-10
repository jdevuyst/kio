#!/bin/sh
set -eu

(
    cd workdir
    "$KIO_BIN" test >/dev/null
)

first=$("$KIO_BIN" fmt - < workdir/main.kio)
second=$(printf '%s\n' "$first" | "$KIO_BIN" fmt -)
test "$first" = "$second"
markers=$(printf '%s\n' "$first" | sed -n 's/^[[:space:]]*\/\/ marker_//p')
expected=$(printf '%s\n' 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16)
if test "$markers" != "$expected"; then
    printf 'expected comment markers:\n%s\nfound:\n%s\n' "$expected" "$markers" >&2
    exit 1
fi
printf '%s\n' 'declaration header comments preserved'
