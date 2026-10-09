#!/bin/sh
# A visible uniquely nearest label repairs only its braced import identifier.
# JS only routes this frontend diagnostic; no backend artifact is consumed.
set -eu
cd workdir
"$KIO_BIN" check
