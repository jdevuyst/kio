#!/bin/sh
set -eu
formatted=$("$KIO_BIN" fmt - <workdir/main.kio)
printf '%s\n' "$formatted" | "$KIO_BIN" fmt -
