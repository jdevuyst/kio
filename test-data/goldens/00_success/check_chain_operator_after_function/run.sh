#!/bin/sh
# JS is only a routing target: this case checks source parsing and typing.
set -eu
cd workdir
"$KIO_BIN" check >/dev/null
