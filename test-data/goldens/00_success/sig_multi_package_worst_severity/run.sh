#!/bin/sh
# Multi-package `kio sig status` reports the *worst* severity across the
# discovered packages (the CI gate wants the loudest verdict), per the
# rank `80 > 82 > 81 > 0` in `more_severe_status` / `status_rank`.
#
# With no selector, `kio sig` fans out over every package in the cwd
# subtree (here `pkg_a` + `pkg_b`) and combines each per-package code.
# Two pairings pin the rank:
#
#   (1) A = 82 (recorded-unsealed-break) vs B = 81 (stale-but-
#       compatible) -> overall 82, NOT 81 (unsealed-break outranks
#       stale).
#   (2) A = 80 (unrecorded break) vs B = 82 (recorded-unsealed-break)
#       -> overall 80, NOT 82 (unrecorded incompatibility is the top of
#       the order).
#
# The flow rewrites each package's `*.sig.kio` + source to stand up the
# two pairings, so it runs in a private scratch copy of `workdir`.
#
# `kio sig` is surface-only (kio-prime rejects it), so the case carries
# IS_KIO_PRIME (its module sources are Kio'-shaped) plus SKIP_KIO_PRIME_RUN
# to opt out of the kio-prime run.
set -u

scratch=$(mktemp -d) || { printf 'cannot make scratch dir\n' >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT
cp -R workdir/pkg_a workdir/pkg_b "$scratch/" || exit 1
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

# Sealed baseline both packages share: v(1) records `api.serve`, header
# at v(2). The open v(2) draft (when present) records the break.
sealed_v1='signature %s v(2);

v(1) {
  nonbreaking {
    add {
      module api {
        pub fn serve() -> .;
      }
    }
  }
}
'
recorded_break='signature %s v(2);

v(1) {
  nonbreaking {
    add {
      module api {
        pub fn serve() -> .;
      }
    }
  }
}

v(2) {
  breaking {
    remove {
      module api {
        serve;
      }
    }
  };
  nonbreaking {
    add {
      module api {
        pub fn other() -> .;
      }
    }
  }
}
'

# === Pairing 1: A = 82, B = 81 -> overall 82 ===
# A: live dropped `serve` (exports `other`); the open v(2) draft records
# the break -> 82.
printf 'module api;\n\npub fn other() -> . { () }\n' > pkg_a/api.kio
# shellcheck disable=SC2059
printf "$recorded_break" alpha > pkg_a/alpha.sig.kio
# B: fresh package (one export, no sig) -> 81.
printf 'module api;\n\npub fn serve() -> . { () }\n' > pkg_b/api.kio
rm -f pkg_b/beta.sig.kio
expect_exit 82 "$KIO_BIN" sig status

# === Pairing 2: A = 80, B = 82 -> overall 80 ===
# A: live dropped `serve`, but only the sealed v(1) is recorded (no v(2)
# draft) -> the break is unrecorded -> 80.
printf 'module api;\n\npub fn other() -> . { () }\n' > pkg_a/api.kio
# shellcheck disable=SC2059
printf "$sealed_v1" alpha > pkg_a/alpha.sig.kio
# B: recorded unsealed break -> 82.
printf 'module api;\n\npub fn other() -> . { () }\n' > pkg_b/api.kio
# shellcheck disable=SC2059
printf "$recorded_break" beta > pkg_b/beta.sig.kio
expect_exit 80 "$KIO_BIN" sig status
