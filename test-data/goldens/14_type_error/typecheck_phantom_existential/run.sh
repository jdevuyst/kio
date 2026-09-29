#!/bin/sh
# Each existential binder declared on a newtype header must occur at
# least once in the payload — a phantom existential is information-
# free and rejected as a type error (exit 14).
set -u
cd workdir || exit
"$KIO_BIN" check
