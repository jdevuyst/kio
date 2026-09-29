#!/bin/sh
# A `build { ... }` block whose target ids are not unique is a build error
# (exit 40 per specs/exit-codes.md; specs/package.md § Build target files).
# The package typechecks fine — `kio check` would exit 0 — but `kio build`
# rejects the duplicate `target js` blocks.
set -u
cd workdir || exit
"$KIO_BIN" build
