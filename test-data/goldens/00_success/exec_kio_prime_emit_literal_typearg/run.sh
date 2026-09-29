#!/bin/sh
# Round-trip pipeline: build to Kio' source, then re-build that
# Kio' source to the impl's primary target, then run it through
# the impl's runner. The Kio' source the kio-prime backend
# produces is required to be syntactically Kio' and to compose
# behaviorally with the rest of the toolchain -- a regression
# golden whose subject is an `id(String, "literal")`-shape call
# whose explicit type-arg used to leak as a `(String)` annotation
# onto the literal value-arg.
set -u
cd workdir || exit
"$KIO_BIN" build kio-prime || exit
cd out/kio-prime || exit
"$KIO_BIN" build "$KIO_TARGET" || exit
"$KIO_RUNNER" --package-name exec_kio_prime_emit_literal_typearg \
  --protocol testapi-print out/"$KIO_TARGET"
