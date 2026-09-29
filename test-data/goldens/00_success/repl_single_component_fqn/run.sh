#!/bin/sh
# Root-module item FQNs execute through every named-item REPL view. These
# Kio'-shaped sources use the full compiler's inspector-only CLI.
set -eu
cd workdir || exit

printf '%s\n' \
  ':signature list.item' \
  ':source list.item' \
  ':doc list.item' \
  ':which list.item' \
  ':references list.item' \
  ':type list.item' \
  ':pure list.item' \
  ':quit' |
  "$KIO_BIN" repl list
