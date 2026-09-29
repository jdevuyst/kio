#!/bin/sh
# Rule 1 of the new module-name rules: declared segments must
# equal the file path relative to the package root. A file at
# `workdir/main.kio` declaring `module wrong/name;` fails the check;
# exit 11.
set -u
cd workdir || exit
"$KIO_BIN" check
