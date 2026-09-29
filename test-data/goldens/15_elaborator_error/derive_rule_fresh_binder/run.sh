#!/bin/sh
# A candidate with a binder in a precondition that is absent from the
# result is rejected at the derive! site (the derive_rule_fresh_binder
# condition), an elaborator error (exit 15).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
