#!/bin/sh
# A go `namespace` value naming an unimportable Go package (`main`,
# `internal`) is a build error (exit 40) saying why.
set -u
cd workdir || exit
"$KIO_BIN" build
