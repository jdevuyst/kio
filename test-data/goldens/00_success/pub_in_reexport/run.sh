#!/bin/sh
# A `pub(helper)` item re-exported by a module inside the `helper` subtree
# and consumed from outside via the public façade — builds clean (exit 0).
set -u
cd workdir || exit
"$KIO_BIN" check
