#!/bin/sh
set -u
"$KIO_BIN" fmt - <<'KIO'
module fmt_label_forms; type { Ready: . { mk; get } } fn make() -> Ready { record { Ready.mk(()) } } fn echo(ready: .) -> Ready { record { Ready.mk(ready) } } fn empty() -> . { record {} } fn read(r: Ready) -> . { r.>select { Ready.get }; () } fn write(r: Ready) -> Ready { r.>with { Ready.mk(()) } } fn write_echo(r: Ready, ready: .) -> Ready { r.>with { Ready.mk(ready) } }
KIO
