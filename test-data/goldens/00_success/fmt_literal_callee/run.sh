#!/bin/sh
set -u

# A bare literal needs grouping when it is the callee rather than type-annotated.
"$KIO_BIN" fmt - <<'KIO'
module fmt_literal_callee; fn subject() -> . { ("literal")(argument) }
KIO
