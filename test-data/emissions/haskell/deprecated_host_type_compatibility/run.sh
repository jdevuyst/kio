#!/bin/sh
# SUBJECT: Haskell retains a removed host-type family as optional while every live family remains required and kind-correct.
# Literal backticks below are part of the asserted generated diagnostic.
# shellcheck disable=SC2016
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.deprecated-host-type.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"
mkdir -p "$scratch/ghc"

cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

legacy_deprecation='host type `api/Legacy` removed at signature v2'

ghc -v0 -fforce-recomp -fno-code -Wdeprecations \
  -odir "$scratch/ghc" \
  -hidir "$scratch/ghc" \
  -iout/haskell \
  "$scratch/host/OldHost.hs" \
  2>"$scratch/ghc/OldHost.stderr"
grep -qF "$legacy_deprecation" "$scratch/ghc/OldHost.stderr" || {
  printf 'deprecated_host_type_compatibility: old host equation did not trigger the retained family deprecation\n' >&2
  cat "$scratch/ghc/OldHost.stderr" >&2
  exit 1
}

ghc -v0 -fforce-recomp -fno-code -Werror=deprecations \
  -odir "$scratch/ghc" \
  -hidir "$scratch/ghc" \
  -iout/haskell \
  "$scratch/host/NewHost.hs"

ghc -v0 -fforce-recomp -fno-code \
  -odir "$scratch/ghc" \
  -hidir "$scratch/ghc" \
  -iout/haskell \
  "$scratch/host/ParametricHost.hs" \
  "$scratch/host/StuckFamilyHost.hs"

expect_missing_contract() {
  emc_host=$1
  emc_identity=$2
  emc_log=$scratch/ghc/$emc_host.stderr
  if ghc -v0 -fforce-recomp -fno-code \
    -odir "$scratch/ghc" \
    -hidir "$scratch/ghc" \
    -iout/haskell \
    "$scratch/host/$emc_host.hs" >"$scratch/ghc/$emc_host.stdout" 2>"$emc_log"; then
    printf 'deprecated_host_type_compatibility: %s unexpectedly compiled\n' "$emc_host" >&2
    exit 1
  fi
  if ! grep -Fq "$emc_identity" "$emc_log"; then
    printf 'deprecated_host_type_compatibility: %s failed for an unrelated reason\n' "$emc_host" >&2
    cat "$emc_log" >&2
    exit 1
  fi
}

for host in MissingScalarHost MissingArrayHost MissingTransformerHost HigherKindedHost; do
  case "$host" in
    MissingScalarHost) identity='api/Scalar' ;;
    MissingArrayHost) identity='api/Array' ;;
    MissingTransformerHost) identity='api/Transformer' ;;
    HigherKindedHost) identity=HigherKindedTypes ;;
  esac
  expect_missing_contract "$host" "$identity"
done
