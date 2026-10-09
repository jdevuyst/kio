#!/bin/sh
# Synthesized expression types use the shared diagnostic/editor printer.
# The module is Kio'-shaped; the transcript needs the full compiler's REPL.
set -eu
cd workdir || exit
printf '%s\n' \
  ':t ((), (), ())' \
  ':t (((), ()), ())' \
  ':t .(x: . | ! | .) { x }' \
  ':t .(x: (. | !) | .) { x }' \
  ':t .(x: . & (! | .)) { x }' \
  ':quit' | "$KIO_BIN" repl display
