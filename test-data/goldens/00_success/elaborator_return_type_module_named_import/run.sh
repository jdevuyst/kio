#!/bin/sh
# Subject: an elaborator whose declared result type is a newtype from
# the same module, where the consumer selectively imports that newtype
# alongside a same-named elaborator. The elaborator-reflection round
# trip re-enters the typer's pattern unifier with the already-qualified
# `(repmod, Rep)` result type; identity-exact canonicalization must be
# idempotent and not double the `repmod` head into `repmod.repmod.Rep`
# via the selective import of the elaborator named `repmod`.
set -u
cd workdir || exit
"$KIO_BIN" check
