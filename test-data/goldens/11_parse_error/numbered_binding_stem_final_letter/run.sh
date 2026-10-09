#!/bin/sh
set -eu

# Numbered-family syntax is validated before name lookup.
cd workdir
"$KIO_BIN" fmt --check main.kio
