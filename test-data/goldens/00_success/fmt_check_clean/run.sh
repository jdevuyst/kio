#!/bin/sh
# `kio fmt --check` on already-canonical input prints nothing to
# stdout and exits 0. Pairs with `60_fmt_check/fmt_check_dirty`
# (which exits 60 and lists the dirty path).
set -u
cd workdir || exit
"$KIO_BIN" fmt --check main.kio
