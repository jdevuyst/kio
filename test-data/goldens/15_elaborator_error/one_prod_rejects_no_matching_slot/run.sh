#!/bin/sh
# `one_prod!` requires some source slot's type to equal the target
# T under spine equality. Source `(A & B)`, target `C` — no source
# slot has type `C`. R-Project-Prod can only keep types that
# already appear; it never invents factors. Reach for `fit!` or
# axis-specifics that reshape the spine. Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
