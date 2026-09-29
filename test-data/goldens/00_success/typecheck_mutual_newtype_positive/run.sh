#!/bin/sh
# Strictly-positive mutual recursion stays accepted. `Na` and `Nb`
# mutually recur, but each only ever holds the other in a product
# (covariant) position -- never under an arrow LHS -- so the SCC-wide
# strict-positivity check admits the pair. This is the companion
# accept-case to the `16_totality_error` mutual/wrapper rejections: the SCC
# check must not over-reject benign mutual recursion. See
# `specs/formal/prime.md` § 2.5.
set -u
cd workdir || exit
"$KIO_BIN" check
