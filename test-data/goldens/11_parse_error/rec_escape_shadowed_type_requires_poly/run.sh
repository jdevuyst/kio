#!/bin/sh
# The inner type binder is distinct from the recursive member's same-spelled binder.
set -u
cd workdir || exit
"$KIO_BIN" check
