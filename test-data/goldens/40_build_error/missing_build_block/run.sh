#!/bin/sh
# `kio build` requires the package's `<name>.pkg.kio` to carry
# a `build { ... }` block naming the compilation targets; a package file
# file with no build block is a build error (exit 40 per
# specs/exit-codes.md). The package typechecks fine on its own —
# `kio check` would exit 0 here.
set -u
cd workdir || exit
"$KIO_BIN" build
