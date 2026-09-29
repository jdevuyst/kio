#!/bin/sh
set -u

# The formatter keeps a grouped label-value receiver before a distinct trailing block.
"$KIO_BIN" fmt - <<'KIO'
module fmt_monadic_label_receiver; fn subject() -> . { do! ({field = value}) { final_value } }
KIO
