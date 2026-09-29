#!/bin/sh
# Residual applications, constructed data, and sequences compare
# recursively. Each block changes one reachable component beneath an
# otherwise shared outer structure and therefore exits through the test-
# failure tier.
set -u
cd workdir || exit
"$KIO_BIN" test
