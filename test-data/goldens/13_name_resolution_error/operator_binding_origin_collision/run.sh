#!/bin/sh
# One operator name cannot select declarations from distinct providers.
set -u
cd workdir || exit
"$KIO_BIN" check
