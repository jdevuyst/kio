#!/bin/sh
# A bare-infix `;` would collide with the statement terminator at use
# sites, so the parser rejects its declaration (exit 11).
set -u
cd workdir || exit
"$KIO_BIN" check
