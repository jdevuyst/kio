#!/bin/sh
# Type-closure bridge check: the bridged export `app.wrap` reaches the
# `pub` type `lib.Widget` in its signature, but `lib` is not selected by
# any bridge glob — the generated interface would not be self-contained
# (exit 20, bridge error).
set -u
cd workdir || exit
"$KIO_BIN" check
