#!/bin/sh
#
# Check that every golden case's `expected.exit` agrees with the exit-code
# prefix of the bucket directory it lives under.
#
# Goldens are grouped under `test-data/goldens/<NN_category>/` where `NN`
# is the case's expected exit code and `<category>` matches a row in
# `specs/exit-codes.md` (see `test-data/README.md` § Golden test case
# layout). A case's `expected.exit` must therefore equal its bucket's `NN`
# prefix; a mismatch means the case is filed under the wrong exit-code
# category, so a portable cross-implementation run asserts the wrong error
# tier. A bucket directory with no leading-digit prefix (an unbucketed
# case) is likewise a finding — every case lives under an `NN_` bucket.
#
# One tier is exempt: `90_runtime_exit` collects built-and-run cases that
# assert the *program's own* runtime exit code per case, so its `NN`
# prefix is a tier label rather than a single asserted category
# (`specs/exit-codes.md` § Runtime exit codes). Its cases carry per-case
# `expected.exit` values, so the value check is skipped for that bucket —
# but it still must carry an `NN` prefix. Every other bucket holds exactly
# one exit-code category.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT"

GOLDENS="test-data/goldens"

# Buckets whose NN prefix is a tier label, not a per-case asserted exit
# category (their cases assert the program's own runtime exit code). The
# value match is skipped for these; the prefix requirement still applies.
# Space-separated bucket names.
EXEMPT_BUCKETS="90_runtime_exit"

# Strip leading zeros to a canonical decimal string ("00" -> "0",
# "02" -> "2", "16" -> "16"); empty stays empty. Avoids the octal trap of
# a bare $(( )) on values like 08/09 in POSIX sh.
canon() {
  c=${1#"${1%%[!0]*}"}
  [ -n "$c" ] || { [ -n "$1" ] && c=0; }
  printf '%s' "$c"
}

findings=$(
  git ls-files "$GOLDENS" | grep '/expected\.exit$' | while IFS= read -r f; do
    rel=${f#"$GOLDENS"/}
    bucket=${rel%%/*}

    # Bucket's leading-digit NN prefix (empty if the dir has none).
    nn=${bucket%%[!0-9]*}

    if [ -z "$nn" ]; then
      printf '%s\t(bucket "%s" has no NN exit-code prefix)\n' "$rel" "$bucket"
      continue
    fi

    # Exempt tiers keep their prefix but skip the value match.
    for b in $EXEMPT_BUCKETS; do
      if [ "$bucket" = "$b" ]; then
        continue 2
      fi
    done

    # expected.exit value (first line, whitespace-trimmed).
    val=""
    IFS= read -r val <"$f" || :
    val=$(printf '%s' "$val" | tr -d '[:space:]')

    if [ "$(canon "$val")" != "$(canon "$nn")" ]; then
      printf '%s\texpected.exit=%s but bucket prefix is %s\n' "$rel" "$val" "$nn"
    fi
  done
)

if [ -n "$findings" ]; then
  echo "golden-bucket-exit: goldens whose expected.exit disagrees with the" >&2
  echo "NN exit-code prefix of their bucket directory:" >&2
  printf '%s\n' "$findings" | sed 's/^/  /' >&2
  echo >&2
  echo "Each golden lives under test-data/goldens/<NN_category>/ where NN equals" >&2
  echo "its expected.exit (test-data/README.md § Golden test case layout). Move" >&2
  echo "the case to the bucket matching its exit code, or correct the exit code." >&2
  echo "The 90_runtime_exit tier is the sole value-match exemption." >&2
  exit 1
fi

echo "golden-bucket-exit: OK (every golden's expected.exit matches its bucket prefix)"
