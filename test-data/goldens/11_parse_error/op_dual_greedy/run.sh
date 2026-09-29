#!/bin/sh
# Dual-greedy patterns are rejected: `___ OP ___` has no position
# info to pin associativity. At most one recursive slot (`__` or
# `___`) is allowed per op pattern.
set -u
cd workdir || exit
"$KIO_BIN" check
