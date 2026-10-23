#!/bin/sh
# No candidate rule derives the goal `Monad(List)` from the tuple —
# resolution finds zero derivations (the derive_no_rule_applies
# condition), an elaborator error (exit 15).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
