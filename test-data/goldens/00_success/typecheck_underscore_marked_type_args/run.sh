#!/bin/sh
# `_`-marked type-args at a call site (surface-Kio sugar — not in
# Kio', see specs/prime.md § 41). The typer treats each `_` as an
# elided positional type-arg slot, solving it via the same
# return-type-driven inference path the fully-elided form uses.
# Surface coverage that the sibling Kio' golden
# `typecheck_return_type_inference` deliberately omits.
set -u
cd workdir || exit
"$KIO_BIN" check
