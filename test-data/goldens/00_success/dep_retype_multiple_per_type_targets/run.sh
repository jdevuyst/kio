#!/bin/sh
# Independent per-type retypes from one source module retain their exact
# destinations when another dependency module imports the names together.
# HERMETIC + DETERMINISTIC.
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
  || fail "cannot materialize per-type retypes: $(cat fetch.err)"
"$KIO_BIN" check >check.out 2>check.err \
  || fail "per-type redirects produced invalid source: $(cat check.err)"

printf 'every per-type redirect preserved\n'
