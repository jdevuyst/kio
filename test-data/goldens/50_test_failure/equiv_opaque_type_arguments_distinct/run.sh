#!/bin/sh
set -u
cd workdir || exit
"$KIO_BIN" test >&2
