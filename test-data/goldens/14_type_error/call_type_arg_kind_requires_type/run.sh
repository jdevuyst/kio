#!/bin/sh
# An explicit call argument must have the kind declared by its
# corresponding type parameter. An unapplied constructor cannot
# instantiate a parameter that requires a saturated type.
set -u
cd workdir || exit
"$KIO_BIN" check
