#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - < workdir/main.kio)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
printf '%s\n' "$formatted"
