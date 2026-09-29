#!/bin/sh
# Cross-package nominal distinctness through a newtype's MEMBER PAYLOAD:
# the consumer and its `bdep` dependency each declare a
# same-named `String` host type. A cross-module newtype's constructor /
# projector payload is qualified in its *declaring* module, so the
# dependency's `Box.mk` wants the dependency's `(bdep/boxed, String)`.
# Feeding the consumer's own `(main, String)` is therefore a type error
# — exit 14. Before the fix the typer requalified `Box`'s payload in
# the caller, conflating the two same-named `String`s by leaf name and
# wrongly accepting this.
set -u
cd workdir || exit
"$KIO_BIN" check
