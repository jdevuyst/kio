#!/bin/sh
# Self-chain (`__`) slots cannot admit a different operator inside
# their captured expression. This subject checks `x + y * z` without
# explicit parentheses; adding them selects the intended grouping.
set -u
cd workdir || exit
"$KIO_BIN" check
