#!/bin/sh
# User-defined operators have no precedence — different operators
# in one expression require explicit parentheses. `x + y * z` is a
# parse error per spec; the user disambiguates with `(x + y) * z`
# or `x + (y * z)`.
set -u
cd workdir || exit
"$KIO_BIN" check
