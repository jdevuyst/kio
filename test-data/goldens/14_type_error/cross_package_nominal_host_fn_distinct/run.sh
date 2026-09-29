#!/bin/sh
# Cross-package nominal distinctness through a HOST FN's SIGNATURE:
# the consumer and its `bdep` dependency each declare a
# same-named `String` host type. A cross-module host fn's signature is
# qualified in its *declaring* module, so the dependency's `dep_id`
# wants the dependency's `(bdep/hosted, String)`. Feeding the consumer's
# own `(main, String)` is therefore a type error — exit 14. Before the
# fix the typer requalified `dep_id`'s signature in the caller,
# conflating the two same-named `String`s by leaf name and wrongly
# accepting this.
set -u
cd workdir || exit
"$KIO_BIN" check
