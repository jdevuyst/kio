#!/bin/sh
# A bare build recursively discovers each nested package and emits its complete
# specified Kio' image under that package's own output root.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/golden.multi-package.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir"
"$KIO_BIN" build

for package in pkg_a pkg_b; do
  out="$package/out/kio-prime"
  test -f "$out/main.kio" || {
    printf 'missing %s/main.kio — package %s was not discovered/built\n' "$out" "$package" >&2
    exit 1
  }
  test -f "$out/$package.pkg.kio" || {
    printf 'missing %s/%s.pkg.kio — package file not re-emitted\n' "$out" "$package" >&2
    exit 1
  }
done
