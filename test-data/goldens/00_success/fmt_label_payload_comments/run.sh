#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - <<'KIO'
module fmt_label_payload_comments;
labels { ready: . };
fn construct(ready: .) -> Ready { {ready = (// construction leading
ready // construction trailing
)} }
fn construct_unit() -> Ready { {ready = (// construction unit
)} }
fn update(row: Ready, other: .) -> Ready { row.!{ready = // update leading
(other // update trailing
)} }
fn update_unit(row: Ready) -> Ready { row.!{ready = (// update unit
)} }
fn canonical(ready: .) -> Ready & Ready { ({ready = ready}, {ready = ()}) }
KIO
)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
printf '%s\n' "$formatted"
