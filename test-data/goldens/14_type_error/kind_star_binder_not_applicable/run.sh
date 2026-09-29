#!/bin/sh
# Applying a kind-`*` binder (`a(b)` where `[A]` is the default
# kind `*`) is a kind error at typecheck. The diagnostic must point
# at `specs/grammar.md` § Kind grammar — higher-kinded application
# requires a kind-`*→*` head (`[*F]`).
set -u
cd workdir || exit
"$KIO_BIN" check
