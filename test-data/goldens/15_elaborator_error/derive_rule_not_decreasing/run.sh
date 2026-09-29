#!/bin/sh
# A candidate whose precondition is not structurally smaller than its
# result (a self-feeding loop) is rejected at the derive! site (the
# derive_rule_not_decreasing condition), an elaborator error (exit 15).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
