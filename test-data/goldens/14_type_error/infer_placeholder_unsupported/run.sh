#!/bin/sh
# `_` placeholders are legal only in `fn` signatures and call
# type-argument lists. Top-level binding positions (fn / host
# fn / fn / type / newtype / etc.) carry public
# contracts and must be fully explicit, so `_` in a `fn`'s
# value-parameter annotation is a permanent type error (exit
# code 14).
set -u
cd workdir || exit
"$KIO_BIN" check
