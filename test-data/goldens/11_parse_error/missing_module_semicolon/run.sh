#!/bin/sh
# Module declaration is missing its terminating `;` — the parser
# expects one and rejects with exit 11.
set -u
cd workdir || exit
"$KIO_BIN" check
