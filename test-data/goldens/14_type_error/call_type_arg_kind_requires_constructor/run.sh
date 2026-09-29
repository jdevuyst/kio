#!/bin/sh
# An explicit call argument must have the kind declared by its
# corresponding type parameter. A saturated type cannot instantiate
# a parameter that requires a one-argument type constructor.
set -u
cd workdir || exit
"$KIO_BIN" check
