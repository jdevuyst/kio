#!/bin/sh
# An unconstrained integer literal needs an explicitly selected default;
# merely importing two role-bearing integer types selects neither one.
set -u
cd workdir || exit
"$KIO_BIN" check
