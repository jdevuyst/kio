#!/bin/sh
# Intra-draft net-out advisory. When an item is added then removed within
# the OPEN (unsealed) draft before sealing, `kio sig stage` names it in a
# warning. The net-out itself is correct (the recompute is
# diff(sealed, live)); the recorded draft holds no spurious change for
# the item. The case asserts: the warning fires and names the item, the
# recomputed draft records no change for it, and `kio sig stage` still
# exits 0.
#
# The flow mutates the package, so it runs in a private scratch copy of
# `workdir`.
#
# `kio sig` is surface-only (kio-prime rejects it), so the case carries
# IS_KIO_PRIME plus SKIP_KIO_PRIME_RUN.
set -u

scratch=$(mktemp -d) || { printf 'cannot make scratch dir\n' >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT
cp workdir/app.pkg.kio workdir/api.kio "$scratch/" || exit 1
cd "$scratch" || exit 1

# Seal v(1) recording the export `serve` -> open draft v(2).
"$KIO_BIN" sig stage >/dev/null 2>&1 || exit 1
"$KIO_BIN" sig commit >/dev/null 2>&1 || exit 1

# Within the open v(2) draft, add a NEW export `temp` and record it.
printf 'module api;\n\npub fn serve() -> . { () }\n\npub fn temp() -> . { () }\n' > api.kio
"$KIO_BIN" sig stage >/dev/null 2>&1 || {
  # shellcheck disable=SC2016
  printf 'recording the intra-draft add of `temp` should succeed\n' >&2
  exit 1
}
grep -q 'temp' app.sig.kio || {
  # shellcheck disable=SC2016
  printf 'the open draft should record `temp` after the first stage\n' >&2
  exit 1
}

# Now remove `temp` from source again (still within the open draft) and
# re-stage: it nets out. `kio sig stage` warns, naming `temp`, and exits
# 0.
printf 'module api;\n\npub fn serve() -> . { () }\n' > api.kio
"$KIO_BIN" sig stage 2>stage.err 1>/dev/null
got=$?
if [ "$got" -ne 0 ]; then
  printf 'a net-out re-stage should exit 0, got %s\n' "$got" >&2
  cat stage.err >&2
  exit 1
fi
grep -q 'added then removed' stage.err || {
  # shellcheck disable=SC2016
  printf '`kio sig stage` should warn that `temp` was added then removed\n' >&2
  cat stage.err >&2
  exit 1
}
grep -q 'temp' stage.err || {
  # shellcheck disable=SC2016
  printf 'the net-out warning should name `temp`\n' >&2
  cat stage.err >&2
  exit 1
}

# The recomputed draft records NO spurious change for `temp` — it is in
# neither the sealed baseline nor the live surface, so it nets out
# entirely.
if grep -q 'temp' app.sig.kio; then
  # shellcheck disable=SC2016
  printf 'the recomputed draft must not record any change for the netted-out `temp`; file:\n' >&2
  cat app.sig.kio >&2
  exit 1
fi
