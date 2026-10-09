#!/bin/sh
# Only an exact target name is selected, never a backend group.
set -u
cd workdir || exit
"$KIO_BIN" build js
