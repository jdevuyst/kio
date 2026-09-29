#!/bin/sh
# Lowercase identifiers are value syntax. A type expression path must end in a
# type-shaped name or a reserved compiler-internal type.
set -u
cd workdir || exit
"$KIO_BIN" check
