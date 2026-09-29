#!/bin/sh
# Retyping must not expose a counterpart beyond its declared visibility.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
