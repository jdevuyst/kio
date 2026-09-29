#!/bin/sh
# The dictionary's raw node helper is private so callers cannot forge tree nodes.
set -u
cd workdir || exit
"$KIO_BIN" check
