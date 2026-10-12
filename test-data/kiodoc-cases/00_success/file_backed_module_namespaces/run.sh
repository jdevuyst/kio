#!/bin/sh
set -eu
"$KIO_BIN" doc check
"$KIO_BIN" doc fmt --check
