#!/bin/sh
set -u
cd workdir || exit
"$KIO_BIN" fmt --check a/b/c.kio
