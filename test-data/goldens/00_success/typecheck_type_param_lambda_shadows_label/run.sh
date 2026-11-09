#!/bin/sh
# Subject: a lambda type-parameter shadows the module-level nominal `A`.
# The lambda's `[A]` binder must scope over its parameter and return
# annotations.
set -u
cd workdir || exit
"$KIO_BIN" check
