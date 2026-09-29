#!/bin/sh
set -eu

# This source form is rejected by the grammar, independently of name lookup.
cd workdir
"$KIO_BIN" fmt --check main.kio
