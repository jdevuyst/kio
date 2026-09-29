#!/bin/sh
# A return-type mismatch: `convert` is annotated `-> Bar` but its body has
# type `Foo`. Beyond the primary `type mismatch` diagnostic (exit 14), the
# typer attaches a secondary label at the annotation that pins the expected
# type — `expected `Bar` because of this` — so the user sees both ends of
# the conflict at once. This case pins that secondary-label enrichment
# end-to-end (kio-rs/src/pass/typecheck_core/aliases.rs require_type_equiv).
set -u
cd workdir || exit
"$KIO_BIN" check
