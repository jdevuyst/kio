#!/bin/sh
# `onto!` accepts identity even when target DNF has duplicate
# branches — `A | A` has two identical DNF branches, and the
# engine's per-source-branch search picks distinct targets for
# the two source arms (kth source DNF branch → kth target slot
# of the same type, the source-order rule lifted to sums).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
