#!/bin/sh
# Subject: reorder_prod! — product-axis permutation, no shape change
# (specs/language.md § Spine-based elaborator palette, specs/formal/
# elaborator.md § 12.4 — R-Comm + source-order pinning).
#
# Pre-condition: μ(spine_prod(S)) = μ(spine_prod(T)). The rule fires
# R-Comm with source-order pinning for like-typed groups: source
# slot i of type U lands at the kth target slot of type U iff i is
# the kth source slot of type U. No projection, no diagonal, no
# `()` insertion or removal.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
