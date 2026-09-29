#!/bin/sh
# `one_sum!` requires source arms to all match the target type
# under spine equality (uniform-arm collapse). Source `(A | B)`,
# target `A` — the B arm has no path to A, so the picker can't
# forward whichever arm is inhabited as the single target type.
# Partial picking is `match!`'s territory. Exit 15.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
