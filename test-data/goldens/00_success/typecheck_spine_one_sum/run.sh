#!/bin/sh
# Subject: one_sum! — focused sum-arm picker (specs/language.md
# § Spine-based elaborator palette, specs/formal/elaborator.md § 12.4 —
# R-Comm + R-Collapse-Sum + R-Identity-Sum-elim, with target's
# sum-spine length pinned to 1).
#
# Pre-condition: source sum-typed (spine length ≥ 1, non-atomic),
# target has sum-spine length exactly 1; every non-`!` source arm
# has type structurally equal to T. The form forwards whichever arm
# is inhabited; `!` arms drop via R-Identity-Sum-elim. Distinct
# from `atom!`: target need not be atomic — only the sum-spine
# length is constrained.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
