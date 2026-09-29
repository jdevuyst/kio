#!/bin/sh
# A retype's semantic projection is limited to its source and target modules'
# transitive import closure. An unrelated module on disk therefore cannot make
# `dep fetch` fail merely because the retype payload checker has no reason to
# inspect it. HERMETIC + DETERMINISTIC.
set -u

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

scratch=$(mktemp -d) || fail "cannot make scratch directory"
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

cp -R workdir "$scratch/consumer" || fail "cannot copy consumer fixture"
cp -R library "$scratch/library" || fail "cannot copy dependency fixture"

cd "$scratch/consumer" || fail "cannot enter consumer fixture"
"$KIO_BIN" dep fetch --force >fetch.out 2>fetch.err \
  || fail "retype fetch inspected an unrelated module: $(cat fetch.err)"
test -f widget/store.kio || fail "dependency module was not materialized"

printf 'unrelated module ignored\n'
