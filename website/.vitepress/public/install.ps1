# One-line install entry point for the `kio` command:
#
#     irm https://jdevuyst.github.io/kio/install.ps1 | iex
#
# A thin, stable alias that runs the latest release's installer (generated
# by dist), which downloads the matching prebuilt `kio` binary and adds it
# to your PATH.
irm https://github.com/jdevuyst/kio/releases/latest/download/kio-installer.ps1 | iex
