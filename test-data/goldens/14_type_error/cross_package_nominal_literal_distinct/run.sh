#!/bin/sh
# Cross-package nominal distinctness through a LITERAL ANNOTATION:
# the consumer and its `bdep` dependency each declare a
# same-named `String` host type. A string literal annotated with the
# consumer's own `String` — `"hi"(String)` — resolves to the consumer's
# `(main, String)`. The dependency's `dep_take` (qualified in its
# declaring module) wants `(bdep/text, String)`, so the annotated
# literal is a type error — exit 14. The annotation is load-bearing: a
# bare `"hi"` would resolve via tier 2 to the expected dependency type
# and be accepted; the `(String)` annotation forces the consumer's type
# and exposes the distinctness.
set -u
cd workdir || exit
"$KIO_BIN" check
