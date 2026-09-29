#!/bin/sh
# A type error in a non-first declaration: `wrong` returns the function
# value `id` where a `Foo` is expected. The stderr-grep checks the
# diagnostic names the expected type (`Foo`) and the found function
# type (`Foo -> Foo`).
set -u
cd workdir || exit
"$KIO_BIN" check
