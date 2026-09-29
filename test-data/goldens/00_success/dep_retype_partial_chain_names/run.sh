#!/bin/sh
# A module-form retype may feed a later per-type retype. Only that named
# newtype continues through the later edge; its siblings remain bound to the
# intermediate module. HERMETIC + DETERMINISTIC.
set -u

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

scratch=$(mktemp -d) || fail "cannot make scratch directory"
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

cp -R workdir "$scratch/consumer" || fail "cannot copy consumer fixture"
cp -R library "$scratch/library" || fail "cannot copy dependency fixtures"

cd "$scratch/consumer" || fail "cannot enter consumer fixture"
"$KIO_BIN" dep fetch --force >fetch.out 2>fetch.err \
  || fail "cannot materialize the retype chain: $(cat fetch.err)"
"$KIO_BIN" check >check.out 2>check.err \
  || fail "mixed retype chain produced invalid source: $(cat check.err)"

printf 'partial chain preserves each destination\n'
