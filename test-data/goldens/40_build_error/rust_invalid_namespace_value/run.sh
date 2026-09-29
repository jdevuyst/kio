#!/bin/sh
# A rust `namespace` value outside the crate-name grammar is a build
# error (exit 40) naming the accepted grammar.
set -u
cd workdir || exit
"$KIO_BIN" build
