#!/bin/sh
# Different nominal heads remain observable even when their constructor
# members share a leaf spelling and their payloads are identical. A
# polymorphic opaque host sink gives both arms the same result type while
# retaining each constructed argument in the residual normal form.
set -u
cd workdir || exit
"$KIO_BIN" test
