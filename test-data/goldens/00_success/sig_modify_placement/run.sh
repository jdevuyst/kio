#!/bin/sh
# Verdict-based `modify` placement, end-to-end through the CLI. A
# retained-but-reshaped export (same name, different signature) must land
# in a `modify` block under `breaking`, and the recorded draft must
# round-trip (parse -> replay -> compare clean).
#
# Seal v(1) with `pub fn f(x: H) -> H`. Then change the live source to
# `pub fn f() -> H` (SAME name, parameter dropped) — a breaking signature
# change. Recording it (`stage --force`) must:
#
#   (1) refuse without `--force` (the change is breaking) -> exit 40;
#   (2) write a `modify { module api { ... pub fn f() -> H } }` block
#       under v(2)'s `breaking` section (NOT an add/remove pair);
#   (3) round-trip: `commit` seals it and the next `kio sig status`
#       returns 0 — the sealed history replays to the live surface and
#       compares clean.
#
# A fresh-package (all-`Added`) flow can't reach the `Modified` placement;
# this is the only path that drives `change_set_for`'s modify filter
# through a non-empty sealed baseline.
#
# The flow mutates (rewrites `api.kio`, writes `app.sig.kio`), so it runs
# in a private scratch copy of `workdir`.
#
# `kio sig` is surface-only (kio-prime rejects it), so the case carries
# IS_KIO_PRIME (its module source is Kio'-shaped) plus SKIP_KIO_PRIME_RUN
# to opt out of the kio-prime run.
set -u

scratch=$(mktemp -d) || { printf 'cannot make scratch dir\n' >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT
cp workdir/app.pkg.kio workdir/api.kio "$scratch/" || exit 1
cd "$scratch" || exit 1

expect_exit() {
  want=$1
  shift
  "$@" >/dev/null 2>&1
  got=$?
  if [ "$got" -ne "$want" ]; then
    # shellcheck disable=SC2016
    printf 'expected exit %s from `%s`, got %s\n' "$want" "$*" "$got" >&2
    exit 1
  fi
}

# Seal v(1) with `pub fn f(x: H) -> H` (the host items are breaking env
# adds against the empty baseline; the export is a compatible add).
expect_exit 0 "$KIO_BIN" sig stage --force
expect_exit 0 "$KIO_BIN" sig commit
expect_exit 0 "$KIO_BIN" sig status

# Reshape `f`: same name, drop the parameter. A breaking signature change.
cat > api.kio <<'API'
module api;

host type H role(i32);

host fn make() -> H;

pub fn f() -> H { make() }
API

# The break is unrecorded -> 80; `stage` refuses without `--force`.
expect_exit 80 "$KIO_BIN" sig status
expect_exit 40 "$KIO_BIN" sig stage

# `--force` records the reshape. It must be a `modify` (a retained name
# whose signature changed), placed under `breaking`.
expect_exit 0 "$KIO_BIN" sig stage --force
grep -q 'modify' app.sig.kio || {
  # shellcheck disable=SC2016
  printf 'a reshaped export must be recorded as a `modify` block\n' >&2
  cat app.sig.kio >&2
  exit 1
}
# The reshaped signature `pub fn f() -> H` (no parameter) is the body of
# the modify; it must not have been recorded as a remove of `f`.
grep -q '^[[:space:]]*pub fn f() -> H$' app.sig.kio || {
  # shellcheck disable=SC2016
  printf 'the modify block must carry the reshaped `pub fn f() -> H`\n' >&2
  cat app.sig.kio >&2
  exit 1
}

# Recorded break, not yet sealed -> 82.
expect_exit 82 "$KIO_BIN" sig status

# Round-trip: sealing the modify and re-checking status must replay the
# history to the live surface and compare clean (exit 0).
expect_exit 0 "$KIO_BIN" sig commit
expect_exit 0 "$KIO_BIN" sig status
