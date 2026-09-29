#!/bin/sh
# A fence's harness reference opens its attribute list. `{@main
# check_exit_code=14}` is the spelling; `{check_exit_code=14 @main}` is
# a runner error, and so is the bare `@`'s trailing form
# `{check_exit_code=14 @}` — the position rule covers both references.
#
# The attribute parser reports this, so the error surfaces before any
# snippet reaches `kio check`. Flags and key-value attributes remain
# order-free among themselves.
set -u
"$KIO_BIN" doc check
