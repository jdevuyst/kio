#!/bin/sh
# Crossing a closure boundary does not make unchanged type arguments polymorphic recursion.
set -u
cd workdir || exit
"$KIO_BIN" check
