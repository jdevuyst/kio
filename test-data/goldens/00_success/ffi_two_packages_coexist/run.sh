#!/bin/sh
# Two packages, one host program — the executable witness for
# `specs/backends/README.md` § The package facade § Coexistence.
#
# `pkg_alpha` and `pkg_beta` have the same source surface: module path
# (`greeter`), host-fn leaf (`print`), export leaves (`main`, `pair`),
# and boundary shapes — including the same structural product
# `(I32 & String)` behind `pair`, so both artifacts expose the same
# positional shape through their backend's public facade. Their
# manifests differ by package identity, and `pkg_alpha` deliberately
# selects the Haskell namespace `KioRunnerHostA` to collide with the
# coexist runner's preferred helper module name. The runner must allocate
# a fresh helper name while the package namespaces still keep every
# emitted symbol isolated. The
# single host program — both artifacts loaded, each instantiated against
# its own prefixing host, greetings called first → second → first, then
# both packages' `pair()` products read in the same order — proves
# symbol-level coexistence, instance isolation, structural-facade coexistence,
# and helper allocation together. Different-compiler-version coexistence
# follows from the same structural property this case pins: every emitted
# symbol is either the package namespace itself or is scoped beneath it,
# so fixed member names remain package-qualified. One checkout has one
# compiler, so the in-tree witness is the symbol-level half.
#
# The run rewrites nothing but builds two out-trees, so it runs in a
# private scratch copy of `workdir`. The packages are natural surface
# Kio (an unannotated string literal; `use … from …`), so the case
# carries no IS_KIO_PRIME marker.
set -u

scratch=$(mktemp -d "${TMPDIR:?}/golden.two-packages-coexist.XXXXXX") || {
  printf 'cannot make scratch dir\n' >&2
  exit 1
}
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir/pkg_alpha workdir/pkg_beta "$scratch/" || exit 1

for pkg in pkg_alpha pkg_beta; do
  (
    cd "$scratch/$pkg" || exit 1
    "$KIO_BIN" test >/dev/null || exit
    "$KIO_BIN" build "$KIO_TARGET" || exit
  ) || exit 1
done

if [ "$KIO_TARGET" = haskell ]; then
  exec "$KIO_RUNNER" \
    --package-name pkg_alpha \
    --artifact-namespace KioRunnerHostA \
    --package-name pkg_beta \
    --protocol coexist \
    "$scratch/pkg_alpha/out/$KIO_TARGET" \
    "$scratch/pkg_beta/out/$KIO_TARGET"
fi

exec "$KIO_RUNNER" \
  --package-name pkg_alpha \
  --package-name pkg_beta \
  --protocol coexist \
  "$scratch/pkg_alpha/out/$KIO_TARGET" \
  "$scratch/pkg_beta/out/$KIO_TARGET"
