#!/bin/sh
# A module at `workdir/main.kio` declaring `module main;` is admissible
# even though its single-segment name differs from the package name
# `pkg`. The only constraint on the declaration is rule 1 of
# `specs/package.md` § Module-Name Rules: the declared path must equal
# the file's path relative to the package root (`main` == `main.kio`).
# There is no rule pinning a module's name under the package-name
# namespace.
set -u
cd workdir || exit
"$KIO_BIN" check
