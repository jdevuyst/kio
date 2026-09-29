#!/bin/sh
# A nonempty packet containing only a type argument performs type application
# and leaves the selected Unit-domain function residual. It does not
# synthesize an unwritten Unit value merely because the signature lowers to a
# Unit domain.
set -u
cd workdir || exit
"$KIO_BIN" check
