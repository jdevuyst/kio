#!/bin/sh
# Changelog version coherence. The `*.sig.kio` header `signature <pkg>
# v(N);` names the current contract generation (the open draft), so no
# version block may exceed it. Here the header says `v(2)` but a block
# claims `v(3)`; `kio sig` rejects the file at the parse/load tier
# (exit 11) before any replay or draft recompute can disagree about the
# same file. (Duplicate `v(N)` blocks and non-contiguous version runs
# are rejected the same way; see the parser unit tests.)
#
# `kio sig` is surface-only (kio-prime rejects it), so the case carries
# IS_KIO_PRIME (its module sources are Kio'-shaped) plus
# SKIP_KIO_PRIME_RUN to opt out of the kio-prime run.
set -u
cd workdir || exit 1
"$KIO_BIN" sig status
