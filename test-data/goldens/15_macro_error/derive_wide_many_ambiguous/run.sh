#!/bin/sh
# A wide candidate tuple where four candidates derive the requested
# brand. This keeps the ambiguity count stable while exercising the
# resolver's indexed lookup with multiple matching rules and
# irrelevant rules around them.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" --no-cache check
