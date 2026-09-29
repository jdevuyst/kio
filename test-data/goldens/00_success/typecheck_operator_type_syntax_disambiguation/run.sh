#!/bin/sh
# User-defined `&` and `!` operators are expression syntax only.
# Type-shaped call arguments still parse as types, while `! value`
# remains the user-defined prefix operator.
set -u
cd workdir || exit
"$KIO_BIN" check
