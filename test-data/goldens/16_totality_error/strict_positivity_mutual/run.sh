#!/bin/sh
# Strict positivity ranges over the newtype SCC, not one declaration.
# `Na` and `Nb` mutually recur; each appears under the other's arrow
# LHS, so the pair unfolds to a self-reference in a negative position
# (enough to encode `Omega` and break strong normalization). A
# per-newtype check that treats the sibling nominal as opaque would
# wrongly accept this; the SCC check rejects it. See
# `specs/formal/prime.md` § 2.5.
set -u
cd workdir || exit
"$KIO_BIN" check
