#!/bin/sh
set -eu
formatted=$("$KIO_BIN" fmt - <workdir/main.kio)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
retained=$(printf '%s\n' "$formatted" | sed -n 's/^[[:space:]]*\/\/ payload boundary$/payload boundary/p')
test "$retained" = 'payload boundary'
printf '%s\n' 'nominal payload boundary comment preserved'
