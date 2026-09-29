#!/bin/sh
# A do-block's bind statement `let x <- e;` requires `e` to
# have type `M(_)` for the receiver's monad. A non-monadic
# expression on the RHS triggers a type error.
set -u
cd workdir || exit
"$KIO_BIN" check
