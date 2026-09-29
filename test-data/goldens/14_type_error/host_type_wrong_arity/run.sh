#!/bin/sh
# A roleless host type has the kind declared by its type parameters, so a
# unary host type cannot consume a second type argument.
set -u
cd workdir || exit
"$KIO_BIN" check
