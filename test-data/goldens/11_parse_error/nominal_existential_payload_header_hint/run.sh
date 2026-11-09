#!/bin/sh
# Invalid existential payloads point to the current nominal header syntax.
set -u
cd workdir || exit
"$KIO_BIN" check
