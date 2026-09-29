#!/bin/sh
#
# Sourced by interactive shells in the Kio devcontainer image.

_kio_devcontainer_target_help() {
  case $- in
    *i*) ;;
    *) return 0 ;;
  esac

  [ -z "${KIO_DEVCONTAINER_HIDE_TARGET_HELP:-}" ] || return 0
  [ -z "${KIO_DEVCONTAINER_TARGET_HELP_SHOWN:-}" ] || return 0
  export KIO_DEVCONTAINER_TARGET_HELP_SHOWN=1

  cache_root=${XDG_CACHE_HOME:-${HOME:-}/.cache}
  if [ -n "$cache_root" ]; then
    cache_dir=$cache_root/kio
    marker=$cache_dir/devcontainer-target-help-shown
    [ ! -f "$marker" ] || return 0
    if mkdir -p "$cache_dir" 2>/dev/null; then
      : >"$marker" 2>/dev/null || true
    fi
  fi

  script=
  search_dir=${PWD:-.}
  while [ "$search_dir" != "/" ]; do
    if [ -f "$search_dir/ci/impl-toolchain.sh" ]; then
      script=$search_dir/ci/impl-toolchain.sh
      break
    fi
    search_dir=$(dirname -- "$search_dir")
  done

  installed=
  available=
  if [ -f "$script" ]; then
    installed=$(sh "$script" installed-impls 2>/dev/null || true)
    available=$(sh "$script" impls 2>/dev/null || true)
  fi

  [ -n "$installed" ] || installed="run: sh ci/impl-toolchain.sh installed-impls"
  [ -n "$available" ] || available="run: sh ci/impl-toolchain.sh impls"

  printf '\n'
  printf 'Kio devcontainer: core tooling is installed; backend extras are on demand.\n'
  printf 'Currently runnable implementation selectors: %s\n' "$installed"
  printf 'All accepted implementation selectors: %s\n' "$available"
  printf 'Install backend extras from the repo root, for example:\n'
  printf '  sh ci/impl-toolchain.sh install kio@go,kio@java\n'
  # shellcheck disable=SC2016 # Literal shell examples for the terminal banner.
  printf '  impls=$(sh ci/impl-toolchain.sh select --extra-count=3 --seed=local)\n'
  # shellcheck disable=SC2016 # Literal shell examples for the terminal banner.
  printf '  sh ci/impl-toolchain.sh install "$impls"\n'
  # shellcheck disable=SC2016 # Literal shell examples for the terminal banner.
  printf '  sh ci/all.sh "$impls"\n'
  printf 'Set KIO_DEVCONTAINER_HIDE_TARGET_HELP=1 to hide this message.\n'
  printf '\n'
}

_kio_devcontainer_target_help
