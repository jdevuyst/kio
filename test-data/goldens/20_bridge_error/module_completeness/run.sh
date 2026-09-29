#!/bin/sh
# Module-completeness bridge check: `app` is bridged and reaches
# `helper` through its `use` graph, but `helper` declares a `host`
# item and is not itself selected by any bridge glob — the generated
# interface would hide a referenced host requirement from the host
# (exit 20, bridge error).
set -u
cd workdir || exit
"$KIO_BIN" check
