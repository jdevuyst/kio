#!/bin/sh
# `kio check <file>` accepts the same file-path / module-name selectors
# as `kio test` (specs/cli.md § kio check). With two package-less
# modules, selecting `a.kio` checks the package and validates the
# selector against its module set; a valid selector exits 0 with no
# output, mirroring `kio check` with no arguments.
set -u
cd workdir || exit
"$KIO_BIN" check a.kio || exit
# Also discharge the modules' equiv blocks (ea, eb) so they are
# exercised here; stdout discarded to keep the selector snapshot empty.
"$KIO_BIN" test >/dev/null
