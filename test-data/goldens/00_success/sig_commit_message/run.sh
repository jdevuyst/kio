#!/bin/sh
# `kio sig commit -m "<message>"` records the message as a `///`
# doc-comment on the sealed version block. This case asserts: (1) the
# sealed block carries `/// drop legacy auth`, (2) `kio sig log` shows
# the message, and (3) the changelog round-trips (a follow-up
# `kio sig stage` leaves the file byte-identical, so parse -> emit is
# stable with the version-block doc present).
#
# The flow *mutates* the package (writes `app.sig.kio`), so it runs in a
# private scratch copy of `workdir` rather than in `workdir` itself.
#
# `kio sig` is a surface-only command (kio-prime rejects it), so the
# case carries IS_KIO_PRIME (its module sources are Kio'-shaped) plus
# SKIP_KIO_PRIME_RUN to opt out of the kio-prime run.
set -u

scratch=$(mktemp -d) || { printf 'cannot make scratch dir\n' >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT
cp workdir/app.pkg.kio workdir/api.kio "$scratch/" || exit 1
cd "$scratch" || exit 1

# Record the compatible export addition into the draft, then seal it
# with a changelog message.
"$KIO_BIN" sig stage >/dev/null 2>&1 || {
  # shellcheck disable=SC2016
  printf '`kio sig stage` should succeed on the fresh export add\n' >&2
  exit 1
}
"$KIO_BIN" sig commit -m "drop legacy auth" >/dev/null 2>&1 || {
  # shellcheck disable=SC2016
  printf '`kio sig commit -m` should seal v(1)\n' >&2
  exit 1
}

# The sealed v(1) block carries the message as a `///` doc-comment.
grep -q '^/// drop legacy auth$' app.sig.kio || {
  # shellcheck disable=SC2016
  printf 'sealed block should carry `/// drop legacy auth`; file:\n' >&2
  cat app.sig.kio >&2
  exit 1
}

# `kio sig log` displays the message.
"$KIO_BIN" sig log | grep -q 'drop legacy auth' || {
  # shellcheck disable=SC2016
  printf '`kio sig log` should show the changelog message\n' >&2
  exit 1
}

# The changelog round-trips: a follow-up `kio sig stage` re-parses and
# re-emits the file (the open v(2) draft is empty, so nothing changes),
# leaving it byte-identical — parse -> emit is stable with the
# version-block doc present.
cp app.sig.kio app.sig.kio.before
"$KIO_BIN" sig stage >/dev/null 2>&1
if ! cmp -s app.sig.kio app.sig.kio.before; then
  printf 'changelog should round-trip byte-identically; diff:\n' >&2
  diff app.sig.kio.before app.sig.kio >&2
  exit 1
fi
