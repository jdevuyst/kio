#!/bin/sh
# A unit-returning host call inside an expression statement is still an
# effectful residual action for `equiv`. The first arm contains two
# `print("x")` calls and the second contains one; the extra unit result
# is unused, but the call itself must not be elided when normal forms
# are compared. The block exits 50 from the `5x` test-error tier.
set -u
cd workdir || exit
"$KIO_BIN" test
