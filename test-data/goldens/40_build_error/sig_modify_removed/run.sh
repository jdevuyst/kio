#!/bin/sh
# A removed export remains in history but is not a live Modify target.
# `sig status` rejects the incoherent history rather than resurrecting it.
# The module is Kio'-shaped; the signature CLI requires the full compiler.
set -u
cd workdir || exit 1
exec "$KIO_BIN" sig status
