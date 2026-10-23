#!/bin/sh
# `align!` rejects product projection — `(A & B) → A` drops the
# `B` factor (real payload data lost), which violates align!'s
# "payload preserved" guarantee. Projection is `onto!`-only per
# spec § iso! / into! / onto! / align! mechanics. Exit 15 (elaborator
# error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
