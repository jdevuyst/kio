#!/bin/sh
# SUBJECT: Haskell's public facade preserves generic, higher-rank, existential, structural, and nominal boundary types.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.generic-public-abi.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"
mkdir -p "$scratch/ghc"

cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

expect_public_rejection() {
  epr_host=$1
  epr_identity=$2
  epr_log=$scratch/ghc/$epr_host.stderr
  if ghc -v0 -fforce-recomp -fno-code \
    -odir "$scratch/ghc" \
    -hidir "$scratch/ghc" \
    -i"$scratch/host" \
    -iout/haskell \
    "$scratch/host/$epr_host.hs" >"$scratch/ghc/$epr_host.stdout" 2>"$epr_log"; then
    printf 'generic_public_abi: %s unexpectedly compiled\n' "$epr_host" >&2
    exit 1
  fi
  if ! grep -Fq "$epr_identity" "$epr_log"; then
    printf 'generic_public_abi: %s failed for an unrelated reason\n' "$epr_host" >&2
    cat "$epr_log" >&2
    exit 1
  fi
}

for host in Wrong PrivatePack PrivateTree PackConstructor TreeConstructor; do
  case "$host" in
    Wrong) identity=__api__Pair__mkPair ;;
    PrivatePack) identity=KioExists_privatePack__Pack ;;
    PrivateTree) identity=KioType_privateTree__Tree ;;
    PackConstructor) identity=KioExists_api__Pack ;;
    TreeConstructor) identity=KioType_tree__Tree ;;
  esac
  expect_public_rejection "$host" "$identity"
done
ghc -v0 -fforce-recomp \
  -odir "$scratch/ghc" \
  -hidir "$scratch/ghc" \
  -iout/haskell \
  -main-is Host.main \
  -o "$scratch/host-check" \
  "$scratch/host/Host.hs" \
  "$scratch/host/PublicTypes.hs"
"$scratch/host-check"
