#!/bin/sh
# The explicit literal annotation names the rigid type binder, not the
# same-spelled host type. This compiler-phase check needs no host artifact.
set -eu
cd workdir
exec "$KIO_BIN" check
