#!/bin/sh
# An equiv whose three terms residualize to three distinct explicit
# Unit-saturated host calls: `a(())`, `b(())`, and `c(())`. Pins the runner's `group A` /
# `group B` / `group C` labelling for ≥3 NF groups, exercising the
# group-letter loop past the 2-letter case. Exits 50 from the
# `5x` test-error tier.
set -u
cd workdir || exit
"$KIO_BIN" test
