#!/bin/sh
# `kio test` and `:normalize` discharge the same user-elaborator expansions
# against the same package-backed evaluator context. The three equiv laws
# overlap the REPL's direct, reordered, and cross-module reflection queries;
# a cold/warm pair also pins replay through the equiv cache. `rep_of_unit`
# keeps the cross-module shape: the elaborator is used only by `rne/other`,
# so its generated imports must resolve when `rne/other.rep_of_unit` is
# inlined into a query against `rne/main`. The printed residual names those
# cross-module newtypes through an ordinary import prelude instead of leaking
# the producer module's generated alias into the consumer.
# `marker_fn` pins the deliberately informational display used when
# normalization leaves a dependency-defined closure residual: its source
# module, original function name, and remaining value-parameter groups.
set -eu
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit

mkdir -p out
rm -rf out/.kio-cache/equiv
KIO_DEBUG_EQUIV_CACHE=1 "$KIO_BIN" test >out/parity-cold.out 2>out/parity-cold.err
KIO_DEBUG_EQUIV_CACHE=1 "$KIO_BIN" test >out/parity-warm.out 2>out/parity-warm.err
cmp out/parity-cold.out out/parity-warm.out >/dev/null

for law in \
  pair_has_i32_matches_true \
  reordered_pair_has_i32_matches_true \
  reflected_unit_matches_constructor
do
  if ! grep -E "equiv-cache: hit [0-9a-f]{64} rne/main\.${law}$" \
    out/parity-warm.err >/dev/null
  then
    printf 'missing warm equiv-cache hit for rne/main.%s\n' "$law" >&2
    exit 1
  fi
done

cat out/parity-warm.out
printf ':normalize pair_has_i32()\n:normalize contains!(((), 1), I32)\n:normalize rep_of_unit()\n:normalize marker_fn()\n' | "$KIO_BIN" repl rne/main
