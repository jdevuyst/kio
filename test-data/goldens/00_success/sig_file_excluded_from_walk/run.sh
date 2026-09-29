#!/bin/sh
# A package shipping a `<name>.sig.kio` signature changelog alongside
# its modules must `check` / `build` / `fmt` / `doc` cleanly: the sig
# file shares the `.kio` extension but is NOT a module, so every
# `*.kio` walk must exclude it (else it parses as a module and fails
# path-coherence). This pins that exclusion through the central
# `file_kind` recognizer.
set -u
cd workdir || exit

"$KIO_BIN" check || exit
"$KIO_BIN" build "$KIO_TARGET" || exit
# On Haskell, GHC-compile the emitted module under `compile-only` so the
# build is type-checked, not merely emitted.
if [ "$KIO_TARGET" = haskell ]; then
  "$KIO_RUNNER" --protocol compile-only "out/$KIO_TARGET" || exit
fi
"$KIO_BIN" fmt --check || exit
"$KIO_BIN" doc check || exit

# An EXPLICIT-path `kio fmt <name>.sig.kio` must also be a no-op: the
# shared `format_one` recognizer skips sig files, so the explicit-path
# / stdin entry point (not just the dir walk) leaves the file
# byte-identical and exits 0. Snapshot before/after and compare.
cp app.sig.kio app.sig.kio.before || exit
"$KIO_BIN" fmt app.sig.kio || exit
cmp app.sig.kio.before app.sig.kio || exit
rm -f app.sig.kio.before
