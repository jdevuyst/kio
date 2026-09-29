#!/bin/sh
# The module sources are Kio'-shaped, but this case exercises the full
# compiler's `repl` command, which the Kio'-only CLI does not expose.
set -eu
cd workdir || exit

printf '%s\n' \
  ':pure ()' \
  ':pure local_pure' \
  ':pure demo/provider.imported_pure' \
  ':pure imported_pure' \
  ':pure provider.imported_pure' \
  ':pure local_pure(())' \
  ':pure unrestricted' \
  ':pure unrestricted(())' \
  ':pure emit' \
  ':pure emit("seen")' \
  ':pure .(x: .) { x }' \
  ':pure (.(x: .) { x })(())' \
  ':pure .(_x: .) { emit("captured") }' \
  ':pure Alias' \
  ':quit' |
  "$KIO_BIN" repl demo/main
