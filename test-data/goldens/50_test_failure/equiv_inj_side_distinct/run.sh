#!/bin/sh
# Two arms at the same sum type `(. | .)`, injecting the same
# unit payload into different branches: `__left__` vs
# `__right__`. The arms share a type (so the type-check step
# passes), but residualize to stuck applications whose head atoms
# differ (`__left__` vs `__right__`), distinguishing them
# per specs/formal/equiv.md § 4.5 "different injected branches".
# Exits 50 from the `5x` test-error tier.
set -u
cd workdir || exit
"$KIO_BIN" test
