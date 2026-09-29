#!/bin/sh
# An unrecognized target id is a build error (exit 40).
set -u
cd workdir || exit
"$KIO_BIN" build
