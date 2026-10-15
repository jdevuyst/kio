#!/bin/sh
# Delegated GHC installation from a verified upstream metadata snapshot.

toolchain_ghcup_version() {
  tc_ghc=$(toolchain_scalar "ghc.$TC_ENVIRONMENT.version" "$REPO_ROOT/tools/toolchains/ghcup.toml" 2>/dev/null || \
    toolchain_scalar ghc.development.version "$REPO_ROOT/tools/toolchains/ghcup.toml")
}

toolchain_ghcup_install() {
  toolchain_ghcup_version
  tc_url=$(toolchain_scalar metadata.url "$REPO_ROOT/tools/toolchains/ghcup.toml")
  tc_hash=$(toolchain_scalar metadata.sha256 "$REPO_ROOT/tools/toolchains/ghcup.toml")
  mkdir -p "$TC_ROOT/downloads"
  tc_metadata="$TC_ROOT/downloads/ghcup-$tc_hash.yaml"
  if [ ! -f "$tc_metadata" ] || [ "$(toolchain_hash "$tc_metadata")" != "$tc_hash" ]; then
    curl --fail --location --silent --show-error --retry 2 "$tc_url" >"$tc_metadata.partial"
    [ "$(toolchain_hash "$tc_metadata.partial")" = "$tc_hash" ] || toolchain_fail 'GHCup metadata checksum mismatch'
    mv "$tc_metadata.partial" "$tc_metadata"
  fi
  tc_manager=$("$TC_MISE" which ghcup --tool "ghcup@$(toolchain_version ghcup)")
  "$tc_manager" --url-source "file://$tc_metadata" --metadata-fetching-mode Strict --cache \
    install ghc "$tc_ghc" --no-set >&2
}

toolchain_ghcup_verify() {
  toolchain_ghcup_version
  tc_ghc_bin="$TC_ROOT/ghcup/.ghcup/ghc/$tc_ghc/bin"
  case "$TC_PLATFORM" in windows-*) tc_ghc_bin="$TC_ROOT/ghcup/ghcup/ghc/$tc_ghc/bin" ;; esac
  [ -x "$tc_ghc_bin/ghc" ] || [ -x "$tc_ghc_bin/ghc.exe" ] || toolchain_fail "GHC $tc_ghc is missing"
  tc_ghc_exe="$tc_ghc_bin/ghc"
  case "$TC_PLATFORM" in windows-*) tc_ghc_exe=$tc_ghc_exe.exe ;; esac
  tc_ghc_real=$(toolchain_real_path "$tc_ghc_exe")
  case "$tc_ghc_real" in "${tc_ghc_bin%/bin}/"*) ;; *) toolchain_fail 'GHC escaped its exact installation' ;; esac
  tc_observed=$("$tc_ghc_bin/ghc" --numeric-version)
  [ "$tc_observed" = "$tc_ghc" ] || toolchain_fail 'GHC version mismatch'
  tc_ghc_libdir=$(toolchain_native_path "$("$tc_ghc_bin/ghc" --print-libdir)")
  tc_ghc_libdir=$(CDPATH='' cd -- "$tc_ghc_libdir" && pwd -P)
  tc_ghc_libdir=$(toolchain_native_path "$tc_ghc_libdir")
  case "$tc_ghc_libdir" in "${tc_ghc_bin%/bin}/"*) ;; *) toolchain_fail 'GHC library directory escaped its exact installation' ;; esac
  TC_PATH="$(toolchain_shell_path "$tc_ghc_bin")${TC_PATH:+:$TC_PATH}"
  printf 'toolchain: ghc %s %s\n' "$tc_ghc" "$tc_ghc_bin/ghc" >&2
  printf 'toolchain-verified: ghc observed=%s\n' "$tc_observed" >&2
  "$tc_ghc_bin/ghc-pkg" field base version >&2
  "$tc_ghc_bin/ghc-pkg" field text version >&2
}
