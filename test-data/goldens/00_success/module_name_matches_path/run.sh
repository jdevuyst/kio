#!/bin/sh
# `kio check` succeeds for a module at `workdir/util/list.kio` declaring
# `module util/list;`. Per rule 1 of `specs/package.md` § Module-Name
# Rules, the declared path must equal the file's path relative to the
# package root (`util/list` == `util/list.kio`); a module's name is not
# pinned under the package-name namespace.
set -u
cd workdir || exit
"$KIO_BIN" check
