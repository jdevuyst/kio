#!/bin/sh
# A non-negative occurrence smuggled through a wrapper newtype.
# `P` puts `Q` under an arrow LHS; `Q` is a transparent wrapper of
# `P`. The pair unfolds to `P = . | (P -> .)` -- the direct form
# strict positivity already rejects -- so the wrapper indirection
# must not let it slip past. `P` and `Q` share an SCC, and `Q` occurs
# negatively in `P`'s payload, so the SCC check rejects it. See
# `specs/formal/prime.md` § 2.5.
set -u
cd workdir || exit
"$KIO_BIN" check
