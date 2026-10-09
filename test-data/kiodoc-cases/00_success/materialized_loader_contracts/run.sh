#!/bin/sh
set -eu

cd workdir
"$KIO_BIN" doc check input/contracts.md
"$KIO_BIN" doc fmt --check input/contracts.md
