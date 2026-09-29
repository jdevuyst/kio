#!/bin/sh
# A callable newtype member cannot bind through a declaration below its use.
set -u
cd workdir || exit
"$KIO_BIN" check
