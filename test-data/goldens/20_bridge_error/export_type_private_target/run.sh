#!/bin/sh
# Type-closure bridge check: the bridged export `pkg.reveal` reaches the
# `pub` type `pkg/types.Secret` in its signature, but `pkg/types` is not
# selected by any bridge glob, so the generated interface would not be
# self-contained (exit 20, bridge error).
set -u
cd workdir || exit
"$KIO_BIN" check
