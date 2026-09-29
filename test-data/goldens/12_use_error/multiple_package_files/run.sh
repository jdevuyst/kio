#!/bin/sh
# A package may have at most one `*.pkg.kio` file at the package
# root. This case ships two (`pkg_a.pkg.kio`, `pkg_b.pkg.kio`)
# to trigger the multiple-package-file check; per `specs/exit-codes.md` this
# is a code-12 use/package-walk failure.
set -u
cd workdir || exit
"$KIO_BIN" check
