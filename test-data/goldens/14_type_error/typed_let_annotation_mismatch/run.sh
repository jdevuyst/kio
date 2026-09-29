#!/bin/sh
# A fully concrete typed `let` checks its RHS against the annotation
# (specs/language.md § Bindings and expressions). `let .(x: W) = ();`
# checks `()` (unit) against the nominal newtype `W`; the types differ,
# so the binding is a type error (exit 14). Pins the annotation-vs-
# initializer clash the other typed_let_* goldens (which turn on
# inference / backward flow) do not cover.
set -u
cd workdir || exit
"$KIO_BIN" check
