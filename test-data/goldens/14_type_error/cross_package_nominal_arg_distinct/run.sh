#!/bin/sh
# Cross-package nominal distinctness in a fn ARGUMENT slot:
# the consumer and its `bdep` dependency each declare a same-named
# `Meters` newtype over a shared `Float`. A cross-module fn's signature
# is qualified in its *declaring* module, so `dep_len`'s parameter wants
# the dependency's `(bdep/meters, Meters)`. Feeding the consumer's own
# `(main, Meters)` is therefore a type error — exit 14. Before the fix
# the typer requalified the parameter in the caller, conflating the two
# same-named newtypes by leaf name and wrongly accepting this.
set -u
cd workdir || exit
"$KIO_BIN" check
