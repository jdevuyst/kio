#!/bin/sh
# Composed inferred types retain each provider's written quantified names.
# The module is Kio'-shaped; the transcript needs the full compiler's REPL.
set -eu
cd workdir || exit
printf '%s\n' \
  ':signature l.make' \
  ':signature r.make' \
  ':signature l.suffix' \
  ':t (l.make(()), r.make(()))' \
  ':t (r.make(()), l.make(()))' \
  ':t (l.suffix(()), l.make(()))' \
  ':quit' | "$KIO_BIN" repl main
