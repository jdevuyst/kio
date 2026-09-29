#!/bin/sh
# Two `literal`s with the same name in the same module are a
# duplicate top-level declaration (exit 13). Same rule as two
# `fn`s with the same name.
set -u
cd workdir || exit
"$KIO_BIN" check
