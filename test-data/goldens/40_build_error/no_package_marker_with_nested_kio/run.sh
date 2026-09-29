#!/bin/sh
# `kio build` invoked from a directory that has no
# `<name>.pkg.kio` at its root must fail at the build-error
# tier (exit 40 per specs/exit-codes.md) with a "no package marker"
# diagnostic — and must do so *before* walking nested subdirectories,
# otherwise a stray `.kio` file in some unrelated subdir surfaces as a
# parse error from arbitrary nested content (the bug that landed this
# golden: running `kio build` from the repo root used to parse
# every file under `test-data/goldens/` and report whichever
# parse error fired first).
#
# `workdir/` here is the directory `kio build` runs in; it deliberately
# carries an `.kio` file in a subdirectory but no package file at
# the root, so the bug — were it to regress — would surface as exit 11
# (parse error) rather than exit 40 (build error).
set -u
cd workdir || exit
"$KIO_BIN" build
