#!/bin/sh
set -eu
cd workdir
case "$KIO_TARGET" in
  js) baseline=es2020 ;;
  ts) baseline=v5_0 ;;
  python) baseline=v3_10 ;;
  rust) baseline=v1_85 ;;
  go) baseline=v1_26 ;;
  java) baseline=v21 ;;
  swift) baseline=v6_0 ;;
  haskell) baseline=ghc9_10 ;;
  *) exit 1 ;;
esac
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM HUP
for selection in omitted unit latest concrete; do
  case "$selection" in
    omitted) setting= ;;
    unit) setting='; baseline ((()))' ;;
    latest) setting='; baseline ((latest))' ;;
    concrete) setting="; baseline (($baseline))" ;;
  esac
  printf 'package app; build { target %s { out "out/"%s } }\n' \
    "$KIO_TARGET" "$setting" >"$work/app.pkg.kio"
  (cd "$work" && "$KIO_BIN" build "$KIO_TARGET") >/dev/null
  "$KIO_BIN" fmt "$work/app.pkg.kio" >/dev/null
  "$KIO_BIN" fmt --check "$work/app.pkg.kio" >/dev/null
  if [ "$selection" = latest ]; then expected=latest; else expected=$baseline; fi
  grep -Fq "baseline $expected" "$work/app.pkg.kio"
  (cd "$work" && "$KIO_BIN" build "$KIO_TARGET") >/dev/null
done
printf '%s\n' 'baseline selection and formatter pinning agree'
