#!/bin/sh
# Literal normal forms retain float digits and string contents. Both
# blocks keep one role-bearing type in scope and change only the literal
# payload, so each disagreement exits through the test-failure tier.
set -u
cd workdir || exit
"$KIO_BIN" test
