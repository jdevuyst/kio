#!/bin/sh
# A module that declares a local `newtype Wrap` and selectively imports a
# same-named `Wrap` has two source identities for one visible name. The
# package-wide origin check rejects that ambiguity before lookup can select
# the import on the type side and the local constructor on the value side.
set -u
cd workdir || exit
"$KIO_BIN" check
