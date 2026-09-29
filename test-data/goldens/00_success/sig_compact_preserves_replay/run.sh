#!/bin/sh
# `kio sig compact` must preserve replay-equivalence: the compacted
# changelog replays to the same interface as the pre-compact one. The
# pinned (CRITICAL) bug synthesized the boundary sections with empty
# `import` clauses, so a cross-module reference (`app.wrap` returning
# `core.Id`) re-qualified differently after the compact — breaking
# replay-equivalence. The fix carries each item's originating section's
# `import` clauses through to the boundary block.
#
# The case builds a two-version changelog (v(1) adds a cross-module
# surface + a host fn; v(2) removes the host fn), compacts before v(2),
# and asserts `kio sig status` on the unchanged source still exits 0 —
# i.e. the compacted file replays to the identical live interface — and
# that the removed host fn's frozen signature survives in the boundary.
#
# `kio sig` is surface-only (kio-prime rejects it), so the case carries
# IS_KIO_PRIME (its module sources are Kio'-shaped) plus
# SKIP_KIO_PRIME_RUN to opt out of the kio-prime run.
set -u

scratch=$(mktemp -d) || { printf 'cannot make scratch dir\n' >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT
cp workdir/app.pkg.kio workdir/core.kio workdir/app.kio "$scratch/" || exit 1
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

# v(1): record the full surface (host items are breaking adds, exports
# compatible adds — `--force` records the breaking host adds) and seal.
expect_exit 0 "$KIO_BIN" sig stage --force
expect_exit 0 "$KIO_BIN" sig commit
expect_exit 0 "$KIO_BIN" sig status

# v(2): remove the host fn `boot` (a compatible removal); record + seal.
printf 'module app;\n\nimport core(Id);\n\npub fn wrap(x: Id) -> Id { x }\n' > app.kio
expect_exit 0 "$KIO_BIN" sig stage
expect_exit 0 "$KIO_BIN" sig commit
expect_exit 0 "$KIO_BIN" sig status

# Compact everything before v(2) into a boundary block. The cross-module
# `import core(Id);` reference and the removed host fn must survive.
expect_exit 0 "$KIO_BIN" sig compact 2

# Replay-equivalence: status on the unchanged source must still be clean
# (the compacted file replays to the identical live interface). If the
# boundary dropped the `import` clauses, `wrap`'s `Id` would re-qualify
# differently and this would report a spurious break.
expect_exit 0 "$KIO_BIN" sig status

# The removed host fn `boot` must still be recoverable in the compacted
# changelog (its frozen signature survives the collapse — no GC).
"$KIO_BIN" sig log | grep -q 'boot' || {
  # shellcheck disable=SC2016
  printf 'compacted changelog must retain the removed host fn `boot`\n' >&2
  "$KIO_BIN" sig log >&2
  exit 1
}
# And the cross-module import survives in the boundary block.
"$KIO_BIN" sig log | grep -q 'import core(Id)' || {
  # shellcheck disable=SC2016
  printf 'compacted boundary must carry the cross-module `import core(Id)`\n' >&2
  "$KIO_BIN" sig log >&2
  exit 1
}
