#!/bin/sh
# One signature-version operation may target a declaration only once. The
# inline and exact-reference spellings below both modify `api.A`; replay must
# reject the duplicate before either operation can overwrite the other.
#
# `kio sig` is surface-only, so the Kio'-shaped source is checked by the
# independent verifier but the kio-prime runtime invocation is skipped.
set -u
cd workdir || exit 1
exec "$KIO_BIN" sig status
