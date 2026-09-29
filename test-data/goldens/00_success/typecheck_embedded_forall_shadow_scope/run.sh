#!/bin/sh
# An embedded forall binder keeps its local kind even when a higher-kinded
# nominal declaration has the same name.
set -u
cd workdir || exit
"$KIO_BIN" check
