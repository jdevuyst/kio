#!/bin/sh
# A same-named private target constructor cannot preserve a public constructor.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
