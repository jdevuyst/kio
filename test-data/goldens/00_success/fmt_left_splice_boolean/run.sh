#!/bin/sh
set -u

# A leading-dot boolean needs grouping after a left-splice token.
"$KIO_BIN" fmt - <<'KIO'
module fmt_left_splice_boolean; fn subject() -> . { context_callee.<(.t) }
KIO
