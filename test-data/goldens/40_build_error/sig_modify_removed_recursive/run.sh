#!/bin/sh
# An exact operation FQN selects its declaration from a complete recursive
# context, but that context cannot resurrect a removed member through Modify.
# The module is Kio'-shaped; the signature CLI requires the full compiler.
set -u
cd workdir || exit 1
exec "$KIO_BIN" sig status
