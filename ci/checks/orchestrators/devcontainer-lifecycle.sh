#!/bin/sh
#
# Dev container lifecycle, end to end: does attaching an editor actually
# leave the vscode-kio extension installed?
#
# The hermetic half of this question is already answered — the resolution
# `post-attach.sh` uses to find a VS Code CLI is pinned by
# ci/checks/repo-lint/devcontainer-code-cli-selftest.sh against fixture
# server trees. What no fixture can answer is whether the CLI it picks
# accepts the install: `remote-cli/code` refuses outside a VS Code terminal
# and *exits 0* while refusing, so a hook wired to it reports success on
# every attach and installs nothing. That is a live VS Code Server's answer
# to give, and this check goes and gets it.
#
# It stages a real server, builds a real .vsix, runs the real hook, and then
# asks the server what it has installed — the last step being the one that
# matters, since the hook's exit status is exactly what lied before.
#
# Linux-only: the server tarball we stage is a linux build, and the dev
# container the hook targets is Linux. Skips cleanly elsewhere.
#
# POSIX sh only.

set -eu

if [ $# -gt 0 ]; then
  case "$1" in
    -h|--help)
      cat <<'EOF'
Usage: sh ci/checks/orchestrators/devcontainer-lifecycle.sh

Run .devcontainer/post-attach.sh against a real VS Code Server and assert
the vscode-kio extension is installed as a result.
EOF
      exit 0
      ;;
    *) printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
fi

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

# The extension id the .vsix installs under: `<publisher>.<name>` from
# tools/vscode-kio/package.json.
EXT_ID=kio-lang.kio

case "$(uname -s)" in
  Linux) ;;
  *)
    printf 'devcontainer-lifecycle: skipping — %s is not a supported test platform\n' "$(uname -s)" >&2
    exit 0
    ;;
esac

case "$(uname -m)" in
  x86_64) SERVER_ARCH=x64 ;;
  aarch64|arm64) SERVER_ARCH=arm64 ;;
  *)
    printf 'devcontainer-lifecycle: skipping — no VS Code server build for %s\n' "$(uname -m)" >&2
    exit 0
    ;;
esac

for tool in node npm curl tar tree-sitter; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    # shellcheck disable=SC2016 # backticks are literal text
    printf 'devcontainer-lifecycle: `%s` not found on PATH.\n' "$tool" >&2
    exit 2
  fi
done

scratch=$(mktemp -d) || {
  printf 'devcontainer-lifecycle: cannot make scratch dir\n' >&2
  exit 2
}
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_devcontainer_lifecycle() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$scratch"
  exit "$cleanup_status"
}
trap cleanup_devcontainer_lifecycle EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

# ----- the VS Code Server -------------------------------------------------
#
# Cached across runs under the repo's shared cache base: the tarball is
# ~50MB and does not change between runs on the same day.
cache_base=${XDG_CACHE_HOME:-$HOME/.cache}/kio/devcontainer
mkdir -p "$cache_base"
tarball="$cache_base/vscode-server-linux-$SERVER_ARCH.tar.gz"

if [ ! -f "$tarball" ]; then
  printf 'devcontainer-lifecycle: downloading VS Code Server (linux-%s)\n' "$SERVER_ARCH" >&2
  if ! curl -fsSL -o "$tarball.partial" \
      "https://update.code.visualstudio.com/latest/server-linux-$SERVER_ARCH/stable"; then
    rm -f "$tarball.partial"
    printf 'devcontainer-lifecycle: could not download the VS Code Server\n' >&2
    exit 1
  fi
  mv "$tarball.partial" "$tarball"
fi

# The hook globs `<root>/bin/*/bin/code-server`, so the directory standing
# in for a commit id can be named anything.
server_root="$scratch/home/.vscode-server"
mkdir -p "$server_root/bin/e2e"
tar xzf "$tarball" -C "$server_root/bin/e2e" --strip-components=1

if [ ! -x "$server_root/bin/e2e/bin/code-server" ]; then
  printf 'devcontainer-lifecycle: staged server has no bin/code-server\n' >&2
  exit 1
fi

# ----- the .vsix ----------------------------------------------------------
#
# Packaged from a private copy of the extension, its two sibling build tools,
# and the root license texts read by `build:license`, never in place.
# `ci/all.sh` runs orchestrators
# concurrently, and vscode-e2e.sh builds in `tools/vscode-kio` too — an
# `npm ci` here would delete `node_modules` out from under its Electron run.
# An orchestrator that mutates a tree another orchestrator is reading has no
# business being in the parallel gate.
printf 'devcontainer-lifecycle: packaging the extension\n' >&2
work="$scratch/tools"
mkdir -p "$work"
for tool in vscode-kio textmate-kio tree-sitter-kio; do
  if [ ! -d "$REPO_ROOT/tools/$tool" ]; then
    printf 'devcontainer-lifecycle: missing tools/%s\n' "$tool" >&2
    exit 2
  fi
  # `node_modules` and prior build output stay behind: the copy installs its
  # own, and a stale `.vsix` copied in would make a failed package look fine.
  tar -C "$REPO_ROOT/tools" \
    --exclude=node_modules --exclude=.vscode-test --exclude='*.vsix' \
    -cf - "$tool" | tar -C "$work" -xf -
done
cp "$REPO_ROOT/LICENSE-MIT" "$REPO_ROOT/LICENSE-APACHE" "$scratch/"

cd "$work/vscode-kio"
if [ -f package-lock.json ]; then
  npm ci --no-fund --no-audit --silent
else
  npm install --no-fund --no-audit --silent
fi
npm run package >/dev/null

VSIX="$work/vscode-kio/kio.vsix"
if [ ! -f "$VSIX" ]; then
  # shellcheck disable=SC2016 # backticks are literal text
  printf 'devcontainer-lifecycle: `npm run package` produced no kio.vsix\n' >&2
  exit 1
fi

# ----- the hook -----------------------------------------------------------
#
# $HOME is the whole point: the hook resolves its CLI under `$HOME`, so
# pointing it at the staged tree is what makes this the real code path
# rather than a re-implementation of it.
printf 'devcontainer-lifecycle: running post-attach.sh\n' >&2
if ! HOME="$scratch/home" KIO_DEVCONTAINER_VSIX="$VSIX" \
    sh "$REPO_ROOT/.devcontainer/post-attach.sh"; then
  printf 'devcontainer-lifecycle: post-attach.sh failed\n' >&2
  exit 1
fi

# ----- the server's own account of what it has ---------------------------
#
# Not the hook's exit status. A hook that installs nothing and exits 0 is
# the bug this check exists to catch.
installed=$(HOME="$scratch/home" \
  "$server_root/bin/e2e/bin/code-server" --list-extensions 2>/dev/null) || {
  printf 'devcontainer-lifecycle: could not list the server extensions\n' >&2
  exit 1
}

if ! printf '%s\n' "$installed" | grep -qi "^$EXT_ID\$"; then
  printf 'devcontainer-lifecycle: FAIL — post-attach.sh did not install %s\n' "$EXT_ID" >&2
  printf '  the server reports these extensions:\n' >&2
  printf '%s\n' "$installed" | sed 's/^/    /' >&2
  exit 1
fi

printf 'devcontainer-lifecycle: ok (%s installed by post-attach.sh into a live VS Code Server)\n' "$EXT_ID"
exit 0
