#!/bin/sh
# Both exposed chain-permitting slots are rejected for this
# operand-ending nonprefix pattern: `___ + ___` cannot select which
# side captures the operator chain.
set -u
cd workdir || exit
"$KIO_BIN" check
