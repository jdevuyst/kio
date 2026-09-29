#!/bin/sh
# One-line install entry point for the `kio` command:
#
#     curl -fsSL https://jdevuyst.github.io/kio/install.sh | sh
#
# A thin, stable alias: it runs the latest release's installer (generated
# by dist), which downloads the matching prebuilt `kio` binary for your
# platform and puts it on your PATH. Anything after `sh -s --` is
# forwarded to that installer.
set -eu
curl -fsSL https://github.com/jdevuyst/kio/releases/latest/download/kio-installer.sh | sh -s -- "$@"
