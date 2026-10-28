#!/bin/sh
set -eu
# Recursive header stages stay separate from ordinary result applications.
formatted=$("$KIO_BIN" fmt - <workdir/main.kio)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
printf '%s\n' "$formatted"
