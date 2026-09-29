#!/bin/sh
# UFCS dispatch is type-equality on the whole receiver vs the
# callee's first parameter. A receiver wider than the first param
# is rejected; the user has to narrow with `narrow_prod!` first.
# Type error → exit code 14.
#
set -u
cd workdir || exit
"$KIO_BIN" check
