#!/bin/sh
# `kio check` accepts the same positional selectors as `kio test`
# (specs/cli.md § kio check). A selector naming a module that does not
# exist is a CLI usage error (exit 2 per `specs/exit-codes.md`), with a
# diagnostic naming the available modules — mirroring `kio test`.
set -u
cd workdir || exit
"$KIO_BIN" check nope
