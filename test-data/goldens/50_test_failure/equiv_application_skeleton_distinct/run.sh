#!/bin/sh
# Stuck applications compare their complete application skeleton
# (specs/formal/equiv.md sections 3 and 4.5). A polymorphic opaque sink
# gives the partial and flat over-application arms the same result type
# without erasing the one-layer versus two-layer application residual.
set -u
cd workdir || exit
"$KIO_BIN" test
