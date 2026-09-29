#!/bin/sh
#
# Assert the cache-backed test runners keep machine-shared toolchain
# state inside their own cache root. A runner must write machine-shared
# caches only under KIO_TEST_RUNNER_BUILD_CACHE_DIR and its staging
# tempdirs — never a compiler's default machine-shared cache location
# (see ci/infra/kio-test-runner-rs/README.md § Runner build cache and
# compiler wrappers). This is the doc-to-reality half of that norm:
# ai/topics/caches.md claims to index *every* cache in the dev loop, but
# a leak nobody documented (swiftc's clang module cache, which defaulted
# to ~/.cache/clang/ModuleCache) is invisible to a doc-to-doc audit.
#
# Method: point XDG_CACHE_HOME at a fresh scratch dir, then build and run
# one tiny golden per cache-backed target through the real runner. The
# scratch dir starts empty, so the runner artifact cache misses and the
# compiler actually fires (a warm hit would skip it and hide a leak). The
# harness's own cache lands under <scratch>/kio/ (the --cache-base below);
# any file that appears elsewhere under <scratch> is a compiler default
# cache the runner failed to redirect. We redirect XDG_CACHE_HOME only,
# never HOME: clang's ModuleCache and Go's os.UserCacheDir both honor it
# on Linux, while a HOME redirect would break mise-shim and ~/.ghcup
# toolchain discovery and fail the run for the wrong reason.
#
# Only targets whose compiler is installed are probed, so a partial
# toolchain shard skips the absent ones rather than failing.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

usage() {
  cat <<EOF
Usage: sh $0

Run one tiny golden per cache-backed target (rust / go / haskell /
swift) with XDG_CACHE_HOME redirected to a scratch dir, and fail if any
runner wrote a compiler default cache outside its own <scratch>/kio/
cache root. Targets whose compiler is not installed are skipped.
EOF
}

case "${1:-}" in
  -h|--help) usage; exit 0 ;;
  "") ;;
  *) printf 'error: unknown argument: %s\n' "$1" >&2; exit 2 ;;
esac

# The tiny golden: an empty-main case that builds and runs on every
# cache-backed target, so one compile per target is enough to populate
# (and so expose) that target's toolchain caches.
CASE_FILTER='^00_success/exec_smoke$'

# mise resolves its own cache dir from XDG_CACHE_HOME too, so the redirect
# below drags mise's tool-resolution cache (a bin_paths-*.msgpack.z per
# installed tool) into the scratch wherever mise reaches the compiler
# through a shim that recomputes it — a leak that surfaces on a shims-on-PATH
# runner but not where the toolchain bin dirs are already on PATH. That
# cache is mise's, not a compiler default cache the adapter should redirect,
# so pin it to its real (pre-redirect) location — the same masquerade the
# sccache wrapper is blanked for below — leaving only genuine compiler
# caches able to land under the scratch.
MISE_CACHE_DIR="${MISE_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/mise}"
export MISE_CACHE_DIR

tmp=$(mktemp -d)
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_runner_cache_hermeticity() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$tmp"
  exit "$cleanup_status"
}
trap cleanup_runner_cache_hermeticity EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

ORCHESTRATOR_TMP=$tmp
export ORCHESTRATOR_TMP
KIO="$tmp/kio"
build_corpus_tool_binary \
  kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$KIO" \
  --all-features --bins

probed=0
failures=0

# probe_target <target> <compiler>: build the target's runner, run the
# golden with XDG_CACHE_HOME at a fresh scratch dir, and report any file
# that landed under the scratch outside kio/.
probe_target() {
  pt_target=$1
  pt_compiler=$2

  if ! command -v "$pt_compiler" >/dev/null 2>&1; then
    printf 'runner-cache-hermeticity: skip %-8s (%s not installed)\n' "$pt_target" "$pt_compiler"
    return 0
  fi
  probed=$((probed + 1))

  (
    cd "$REPO_ROOT/ci/infra/kio-test-runner-rs" &&
      sh "$REPO_ROOT/ci/cargo.sh" build --no-default-features --features "$pt_target"
  )
  pt_runner="$REPO_ROOT/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-$pt_target"

  pt_xdg="$tmp/$pt_target/xdg"
  rm -rf "${tmp:?}/$pt_target"
  mkdir -p "$pt_xdg"

  # A compiler wrapper (sccache) reads XDG_CACHE_HOME for its own store;
  # blank it so a wrapper's legitimate cache can't masquerade as a leak.
  if ! XDG_CACHE_HOME="$pt_xdg" \
    KIO_TEST_RUNNER_COMPILER_WRAPPER='' RUSTC_WRAPPER='' \
    sh "$REPO_ROOT/ci/run-tests.sh" \
      --cases-dir="$REPO_ROOT/test-data/goldens" \
      --cache-base="$pt_xdg/kio/hermetic" \
      --impl-def="name=kio@$pt_target,kio=$KIO,runner=$pt_runner,target=$pt_target" \
      "$CASE_FILTER" >"$tmp/$pt_target.run.log" 2>&1
  then
    printf 'runner-cache-hermeticity: FAIL %s — the golden run itself failed:\n' "$pt_target" >&2
    sed 's/^/  /' "$tmp/$pt_target.run.log" >&2
    failures=$((failures + 1))
    return 0
  fi

  # Everything the harness legitimately places lives under <scratch>/kio/.
  # Anything else is a compiler default cache the runner didn't redirect.
  pt_leaks=$(find "$pt_xdg" -type f -not -path "$pt_xdg/kio/*")
  if [ -n "$pt_leaks" ]; then
    printf 'runner-cache-hermeticity: FAIL %s — runner wrote machine-shared cache state outside its cache root:\n' "$pt_target" >&2
    printf '%s\n' "$pt_leaks" | sed "s|$pt_xdg|  <xdg>|" | sort | head -20 >&2
    pt_dirs=$(printf '%s\n' "$pt_leaks" | sed "s|$pt_xdg/||;s|/.*||" | sort -u | tr '\n' ' ')
    printf '  leaked under: %s\n' "$pt_dirs" >&2
    printf '  fix: pin the toolchain cache per-compile (e.g. -module-cache-path / GOCACHE) under the staging tempdir, or into KIO_TEST_RUNNER_BUILD_CACHE_DIR; see ci/infra/kio-test-runner-rs/README.md § Runner build cache and compiler wrappers.\n' >&2
    failures=$((failures + 1))
    return 0
  fi

  printf 'runner-cache-hermeticity: pass %s\n' "$pt_target"
}

# The four cache-backed runners and the compiler each drives. The JS / TS
# / Python / Java runners have no persistent compiled artifact cache, so
# they are out of scope here.
probe_target rust rustc
probe_target go go
probe_target haskell ghc
probe_target swift swiftc

if [ "$probed" = 0 ]; then
  printf 'error: no cache-backed target toolchain installed; nothing probed\n' >&2
  exit 1
fi

if [ "$failures" != 0 ]; then
  printf '\nrunner-cache-hermeticity: %s target(s) leaked\n' "$failures" >&2
  exit 1
fi

printf 'runner-cache-hermeticity: pass (%s target(s) probed)\n' "$probed"
