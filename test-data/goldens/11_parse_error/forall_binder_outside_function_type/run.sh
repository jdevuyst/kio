#!/bin/sh
# Forall binders are position-restricted to declaration parameter
# lists and to function-type expressions. A `type` body
# spelled as a bare `<U>` (no surrounding parens, no `.>`) is a
# parse error.
set -u
cd workdir || exit
"$KIO_BIN" check
