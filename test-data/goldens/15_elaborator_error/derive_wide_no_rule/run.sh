#!/bin/sh
# A wide candidate tuple where no candidate can derive the requested
# brand. This keeps the no-rule diagnostic stable while exercising the
# resolver's candidate scan on an intentionally wider set.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" --no-cache check
