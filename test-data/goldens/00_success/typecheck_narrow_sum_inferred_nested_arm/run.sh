#!/bin/sh
# `narrow_sum!`'s forward DEFAULT type rule must infer a target that
# the spine elaborator accepts even when a source arm is itself a sum.
# The source right-spine is `[(), (. | .), ()]`; the canonical
# codiagonal target keeps the nested sum arm representable as a
# non-terminal arm: `((. | .) | .)`.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
