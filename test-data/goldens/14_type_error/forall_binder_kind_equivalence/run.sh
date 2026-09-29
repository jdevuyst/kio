#!/bin/sh
# A common type cannot identify phantom forall binders of different kinds.
set -u
cd workdir || exit
"$KIO_BIN" check
