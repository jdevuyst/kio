#!/bin/sh
# `into!` rejects product projection — `(A & B) → A` would drop
# the `B` component, which only `onto!` is allowed to do. Mirror
# of `project_sum_narrowing` (the one rule `onto!` doesn't
# include). Exit 15.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
