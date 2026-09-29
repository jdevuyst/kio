#!/bin/sh
# Source admits one complete mutual recursive group. The sealed signature must
# not split that maximal component across two independently written contexts.
#
# `kio sig` is surface-only, so the Kio'-shaped source is checked by the
# independent verifier but the kio-prime runtime invocation is skipped.
set -u
cd workdir || exit 1
exec "$KIO_BIN" sig status
