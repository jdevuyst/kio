#!/bin/sh
# Prefix-forbidden conflict rule, success case. Two `op`s
# sharing a leading op-token run coexist if their full sequences
# diverge before either runs out — `_ && ++ _` and `_ && -- _`
# both lead with `&&` but branch at the third token, so neither
# sequence is a prefix of the other.
set -u
cd workdir || exit
"$KIO_BIN" check
