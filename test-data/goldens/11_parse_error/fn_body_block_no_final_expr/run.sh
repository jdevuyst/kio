#!/bin/sh
# A fn body is a statement-bearing block but must end with an
# expression. A block whose only content is a `let` statement (no
# trailing expression) hits the "block must end with an expression"
# parse error (exit 11).
set -u
cd workdir || exit
"$KIO_BIN" check
