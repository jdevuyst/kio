#!/bin/sh
# A wide candidate tuple where two candidates derive the requested
# brand. This keeps ambiguity diagnostics stable while exercising the
# resolver's candidate scan with irrelevant candidates around the two
# matching rules.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" --no-cache check
