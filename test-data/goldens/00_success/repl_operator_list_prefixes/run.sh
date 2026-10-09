#!/bin/sh
# The inspector renders operator visibility as a separate declaration token.
set -eu
cd workdir || exit
printf '%s\n' ':ls' ':scope' ':quit' | "$KIO_BIN" repl demo
