#!/bin/sh
# Return-type-driven type-argument inference. language.md § Type
# system: elided / `_`-marked type-args at a call site are solved
# by unifying both the value-arg types AND the call's expected
# return type. Covers the canonical `__absurd__` case (binder
# reachable ONLY through the return), a mixed case where the
# return type contributes one binder while value-args contribute
# another, and the `_`-marked spelling.
set -u
cd workdir || exit
"$KIO_BIN" check
