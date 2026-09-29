#!/bin/sh
# A `derive!` candidate-tuple element that isn't of function type. Its
# signature can't be read as an inference rule, so it is a type error
# (exit 14) pointing at the offending element.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
