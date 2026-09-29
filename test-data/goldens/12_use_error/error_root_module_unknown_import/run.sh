#!/bin/sh
# A `*.kio` file cannot import a module that is not part
# of the local package's root module/module graph.
set -u
cd workdir || exit
"$KIO_BIN" check
