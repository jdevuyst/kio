#!/bin/sh
# Build-time contract-staleness advisory. When a `*.sig.kio` is present
# AND the live surface breaks the last sealed contract (unrecorded),
# `kio check` / `kio build` print a WARNING on stderr but still SUCCEED
# (exit unchanged). The advisory is SILENT when there is no sig, when the
# unrecorded drift is only nonbreaking, and when the break is already
# recorded.
#
# The flow mutates the package (writes `app.sig.kio`, builds to `out/`),
# so it runs in a private scratch copy of `workdir`.
#
# `kio sig` is surface-only (kio-prime rejects it), so the case carries
# IS_KIO_PRIME plus SKIP_KIO_PRIME_RUN.
set -u

scratch=$(mktemp -d) || { printf 'cannot make scratch dir\n' >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT
cp workdir/app.pkg.kio workdir/api.kio "$scratch/" || exit 1
cd "$scratch" || exit 1

# True iff `$@`'s stderr mentions the contract-staleness warning.
warns() {
  "$@" 2>warn.err 1>/dev/null
  grep -qi 'breaks its last recorded contract' warn.err
}
# Assert exit code of `$@`.
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

# (silent) No sig file yet: open-world — a package with no changelog
# checks / builds with no advisory.
if warns "$KIO_BIN" check; then
  # shellcheck disable=SC2016
  printf 'no-sig package must not warn on `kio check`\n' >&2
  exit 1
fi
if warns "$KIO_BIN" build; then
  # shellcheck disable=SC2016
  printf 'no-sig package must not warn on `kio build`\n' >&2
  exit 1
fi

# Seal v(1) recording the export `serve`.
expect_exit 0 "$KIO_BIN" sig stage
expect_exit 0 "$KIO_BIN" sig commit

# (silent) Only a nonbreaking unrecorded drift (a new export): no
# advisory.
printf 'module api;\n\npub fn serve() -> . { () }\n\npub fn more() -> . { () }\n' > api.kio
if warns "$KIO_BIN" check; then
  printf 'a nonbreaking unrecorded drift must not warn\n' >&2
  cat warn.err >&2
  exit 1
fi

# (warns) An unrecorded BREAKING change (drop the sealed export
# `serve`): `kio check` AND `kio build` warn on stderr but still exit 0.
printf 'module api;\n\npub fn other() -> . { () }\n' > api.kio
warns "$KIO_BIN" check || {
  # shellcheck disable=SC2016
  printf '`kio check` must warn on an unrecorded breaking change\n' >&2
  exit 1
}
expect_exit 0 "$KIO_BIN" check
warns "$KIO_BIN" build || {
  # shellcheck disable=SC2016
  printf '`kio build` must warn on an unrecorded breaking change\n' >&2
  exit 1
}
expect_exit 0 "$KIO_BIN" build

# (silent) Once the break is RECORDED (force-staged), the advisory goes
# quiet — the author has acknowledged it (this is `kio sig status` 82,
# not 80).
expect_exit 0 "$KIO_BIN" sig stage --force
if warns "$KIO_BIN" check; then
  printf 'a recorded break must not warn\n' >&2
  cat warn.err >&2
  exit 1
fi
