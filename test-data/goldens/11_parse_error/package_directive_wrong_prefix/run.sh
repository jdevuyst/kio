#!/bin/sh
# The leading directive must match the file's extension: a `.pkg.kio`
# file requires `package <name>;`. Here `app.pkg.kio` carries
# `module app;` instead, so parsing fails (exit code 11).
set -u
cd workdir || exit
"$KIO_BIN" check
