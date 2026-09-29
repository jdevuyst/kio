#!/bin/sh
# UFCS dispatch over a callee that doesn't resolve to a function
# value (here `not_a_function` is bound to a String literal).
# Exit 14 — type error.
set -u
cd workdir || exit
"$KIO_BIN" check
