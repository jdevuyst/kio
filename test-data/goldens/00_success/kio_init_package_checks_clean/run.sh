#!/bin/sh
# `kio init <name>` scaffolds a package whose generated source checks
# clean and is already canonically formatted. The inverse of a normal
# golden: there is no pre-existing package in workdir/; run.sh CREATES
# one with `kio init` into a fresh temp directory and then checks it.
#
# This guards the init template against drifting out of sync with the
# language. A freshly-init'd package must use the host/bridge model
# (package `bridge { … }` glob list, `host type` / `host fn` in a
# module) and check clean. The old `env {}` / `bridge <name> {}` /
# `export {}` model that `kio check` now rejects would make
# `kio check` exit non-zero and fail this golden.
#
# `kio init` / `kio check` / `kio fmt --check` are backend-agnostic
# (check and fmt emit no target), so the case carries no workdir/ and
# runs on every configured impl — the same `kio` binary answers
# identically regardless of KIO_TARGET.
#
# Behavior, not bytes: `kio init` prints the absolute path of each
# generated file to stdout, which is machine-specific, so the case
# pins the assertions (files created, both checks exit 0) rather than
# init's stdout, and emits one stable success line.
#
# POSIX sh only.
set -u

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

pkgname=demo
tmpdir=$(mktemp -d) || fail "mktemp -d failed"
trap 'rm -rf "$tmpdir"' EXIT INT TERM HUP

# `kio init` scaffolds <name>.pkg.kio + main.kio in cwd.
( cd "$tmpdir" && "$KIO_BIN" init "$pkgname" ) >/dev/null \
  || fail "kio init $pkgname: non-zero exit"

[ -f "$tmpdir/$pkgname.pkg.kio" ] \
  || fail "kio init: expected $pkgname.pkg.kio to be written"
[ -f "$tmpdir/main.kio" ] \
  || fail "kio init: expected main.kio to be written"

# The generated package must check clean under the current language.
( cd "$tmpdir" && "$KIO_BIN" check ) \
  || fail "kio check on a freshly-init'd package must exit 0"

# The generated source must already be in canonical form.
( cd "$tmpdir" && "$KIO_BIN" fmt --check ) \
  || fail "kio fmt --check on freshly-init'd source must exit 0"

printf 'kio init package checks clean\n'
