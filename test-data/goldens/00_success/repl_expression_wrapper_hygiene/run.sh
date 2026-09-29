#!/bin/sh
# The module source is Kio'-shaped, but this case exercises the full compiler's
# expression-query adapter through the `repl` command.
set -eu
cd workdir || exit

printf '%s\n' \
  ':pure () // trailing comment' \
  ':pure (); () } fn injected() { emit("seen")' \
  ':quit' |
  "$KIO_BIN" repl demo/main
