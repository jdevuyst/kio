#!/bin/sh
# `kio sig status` must report 82 (recorded-break-pending), not a false
# 80 (unrecorded incompatibility), in two design-sanctioned cases:
#
#   (1) a recorded break whose draft was hand-edited / merge-reordered
#       into non-canonical module-section order — the `recorded` flag
#       must be order-insensitive (the emitter canonicalizes both sides
#       before the text compare);
#   (2) a fully-recorded break with a *later, still-unrecorded
#       compatible* add — the 80-vs-82 discriminator must key on the
#       breaking portion specifically, not whole-draft equality.
#
# Both pinned bugs flipped 82 -> 80, a wrong CI gate on realistic input.
# The case also asserts `commit` succeeds on the reordered recorded break.
#
# `kio sig` is surface-only (kio-prime rejects it), so the case carries
# IS_KIO_PRIME (its module sources are Kio'-shaped) plus
# SKIP_KIO_PRIME_RUN to opt out of the kio-prime run.
set -u

scratch=$(mktemp -d) || { printf 'cannot make scratch dir\n' >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT
cp workdir/app.pkg.kio workdir/api.kio workdir/core.kio "$scratch/" || exit 1
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

# Record the fresh surface and seal v(1) -> open v(2).
expect_exit 0 "$KIO_BIN" sig stage --force
expect_exit 0 "$KIO_BIN" sig commit
expect_exit 0 "$KIO_BIN" sig status

# Break: remove `alpha` (api) AND `gamma` (core) — two sibling sections.
printf 'module api;\n\npub fn beta() -> . { () }\n' > api.kio
printf 'module core;\n\npub fn delta() -> . { () }\n' > core.kio

# Hand-write a recorded break draft in NON-CANONICAL order: the v(2)
# remove block lists `core` before `api`. A semantic (order-insensitive)
# recorded-equality check reads this as recorded -> 82.
cat > app.sig.kio <<'SIG'
signature app v(2);

v(1) {
  nonbreaking {
    add {
      module api {
        pub fn alpha() -> .;
        pub fn beta() -> .;
      };
      module core {
        pub fn delta() -> .;
        pub fn gamma() -> .;
      }
    }
  }
}

v(2) {
  breaking {
    remove {
      module core {
        gamma;
      };
      module api {
        alpha;
      }
    }
  }
}
SIG

# The reordered recorded break must read 82, not 80.
expect_exit 82 "$KIO_BIN" sig status
# And `commit` must accept the reordered-but-equivalent recorded draft.
expect_exit 0 "$KIO_BIN" sig commit
expect_exit 0 "$KIO_BIN" sig status

# Case (2): seal a fresh baseline, force-record a break, then append a
# compatible export WITHOUT recording it. The recorded break outranks the
# unrecorded compatible drift -> 82, not 80.
rm -f app.sig.kio
printf 'module api;\n\npub fn alpha() -> . { () }\n\npub fn beta() -> . { () }\n' > api.kio
printf 'module core;\n\npub fn gamma() -> . { () }\n\npub fn delta() -> . { () }\n' > core.kio
expect_exit 0 "$KIO_BIN" sig stage --force
expect_exit 0 "$KIO_BIN" sig commit
# Break: drop `alpha`; force-record it.
printf 'module api;\n\npub fn beta() -> . { () }\n' > api.kio
expect_exit 0 "$KIO_BIN" sig stage --force
expect_exit 82 "$KIO_BIN" sig status
# Now ALSO add a compatible export, unrecorded.
printf 'module api;\n\npub fn beta() -> . { () }\n\npub fn extra() -> . { () }\n' > api.kio
expect_exit 82 "$KIO_BIN" sig status
