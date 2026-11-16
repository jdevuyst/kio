#!/bin/sh
set -u

# The formatter keeps a record receiver before a distinct trailing block.
"$KIO_BIN" fmt - <<'KIO'
mod fmt_monadic_label_receiver; fn subject() -> . { do (record { Field.mk(value) }) { final_value } }
KIO
