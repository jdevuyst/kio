#!/bin/sh
# A module-qualified reference to an imported `pub type` alias --
# `scale.Kelvin`, reached through `import scale as scale;` -- must
# survive the kio-prime backend as valid Kio'. Build the package to
# Kio', then re-check the emitted regular modules with the kio-prime
# binary (the kio-rs sibling of `kio`, next to `$KIO_BIN`). A cross-
# module requalifier gap here once forced a castle to be reshaped to
# a redundant `import <module>(Reading);` selective import; this pins the
# natural qualified form so the gap can't return unnoticed.
set -u
cd workdir || exit
"$KIO_BIN" build kio-prime || exit
kio_prime="$(dirname "$KIO_BIN")/kio-prime"
cd out/kio-prime || exit
"$kio_prime" check
