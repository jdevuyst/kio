#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - < workdir/main.kio)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
comments=$(printf '%s\n' "$formatted" | sed -n 's/^[[:space:]]*\/\/ */\/\/ /p')
test "$comments" = '// first
// second'
printf '%s\n' 'variadic operand comments retained'
