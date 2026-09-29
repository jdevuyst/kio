#!/bin/sh
# Prefix-forbidden conflict rule: two `op`s in the same module
# may share leading op-token sequences as long as neither's
# sequence is a strict prefix of the other's. `_ && _` is a strict
# prefix of `_ && ++ _`, so the pair is a name conflict — exit 11.
set -u
cd workdir || exit
"$KIO_BIN" check
