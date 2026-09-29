#!/bin/sh
# A single item after a leading product operator is valid and
# collapses to the item, matching the unary chain rule.
set -u
cd workdir || exit
"$KIO_BIN" check
