#!/bin/sh
# A do-block's final expression must have type `M(_)` for the
# same `M` the receiver's bind operates on. A non-monadic final
# expression — here, a bare `String` — triggers a type error.
set -u
cd workdir || exit
"$KIO_BIN" check
