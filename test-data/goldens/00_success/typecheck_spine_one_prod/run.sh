#!/bin/sh
# Subject: one_prod! — focused product-slot picker (specs/language.md
# § Spine-based elaborator palette, specs/formal/elaborator.md § 12.4 —
# R-Project-Prod restricted to a single kept slot).
#
# Pre-condition: source product-typed (spine length ≥ 1, non-atomic),
# target T is the type of at least one source slot. The form fires
# R-Project-Prod picking the leftmost source slot whose type equals
# T (source-order pinning). T can be any type — not just atomic.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
