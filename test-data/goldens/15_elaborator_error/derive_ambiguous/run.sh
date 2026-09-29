#!/bin/sh
# Two candidates both produce `Monad(Box)` and unify with the goal —
# resolution finds two derivations, the ambiguity coherence violation
# (the derive_ambiguous condition), an elaborator error (exit 15).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
