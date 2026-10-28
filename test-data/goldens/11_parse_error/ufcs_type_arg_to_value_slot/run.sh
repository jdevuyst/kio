#!/bin/sh
# The receiver fills `take`'s only value slot; `String` is a type path
# in the following value-argument position. The parser rejects that
# syntax and directs type arguments to square brackets.
set -u
cd workdir || exit
"$KIO_BIN" check
