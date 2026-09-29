#!/bin/sh
# A `literal` declaration stores one bare token; each annotated use
# supplies the concrete literal type, so `one(I32)` and `one(I64)`
# substitute to `1(I32)` and `1(I64)`.
set -u
cd workdir || exit
"$KIO_BIN" check
