#!/bin/sh
# A `derive!` candidate whose result applies the `Monad[*F]` brand to
# the kind-`*` type `Flat`, where a kind-`*→*` brand is required — a
# kind mismatch (exit 14). Per `specs/language.md` § The `derive!`
# elaborator, a candidate's return-type kind must match the goal's brand
# slot.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
