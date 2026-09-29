#!/bin/sh
#
# Build the `kio` binary the editor's language server runs.
#
# `devcontainer.json` points $KIO_BIN at `kio-rs/target/debug/kio`, and the
# vscode-kio extension resolves its server as `kio.server.path` → $KIO_BIN →
# $PATH — so that file, and not whatever $PATH happens to find, is what
# serves `kio lsp`. Keeping it current is this script's whole job.
#
# It has to be built, because nothing else builds it. `target/` is
# git-ignored, so a `git pull` moves the sources and leaves the binary alone;
# the lifecycle's `cargo build --tests` builds test harnesses, not the
# binary. An artifact from a long-past build therefore survives
# indefinitely, and one old enough to predate a subcommand answers
# `kio lsp` with "unknown subcommand" — while one merely a few weeks old
# answers it with stale analysis and no error at all. Cargo rebuilds exactly
# when the sources moved and is a no-op otherwise, so running this on every
# attach is what makes the second case impossible.
#
# Debug, not release: a debug rebuild after a pull costs seconds where a
# release rebuild costs minutes, and that cost lands on every attach — while
# for analysing a single package the debug binary is fast enough. A
# developer who wants a release server points `kio.server.path` at one.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)

cd "$REPO_ROOT"

if ! command -v cargo >/dev/null 2>&1; then
  echo "refresh-kio: cargo not on PATH; skipping (the language server keeps whatever binary is there)"
  exit 0
fi

if [ ! -f kio-rs/Cargo.toml ]; then
  echo "refresh-kio: no kio-rs/Cargo.toml; skipping"
  exit 0
fi

echo "refresh-kio: cargo build --bin kio (the language server's binary)"
( cd kio-rs && sh "$REPO_ROOT/ci/cargo.sh" build --bin kio )
