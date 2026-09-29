#!/bin/sh
#
# Dev container lifecycle: postAttachCommand.
#
# Runs after VS Code attaches to the container — every Codespace
# start (initial creation, restart, reconnect) and every local
# "Reopen in Container" attach. Lives in postAttach rather than
# postCreate because installing an extension needs the VS Code
# Server fully up, and postCreate runs *before* the server is
# ready: the install would silently no-op (verified empirically —
# `code --list-extensions` came back empty after a postCreate
# install). postAttach guarantees the server is online.
#
# find-code-cli.sh resolves the CLI that performs the install, and
# explains why the `code` wrapper on $PATH does not suffice. Because
# the server is already online here, a failure to resolve is not a
# race the hook can wait out: the short poll below covers only a
# server still being unpacked, and an environment with no editor at
# all (a headless `devcontainer up`) falls through to a clean no-op.
#
# Idempotent: `--force` lets repeated invocations (every restart)
# reinstall over an existing copy cheaply.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)

cd "$REPO_ROOT"

# Before the editor's language server can be spawned, the binary it
# spawns has to exist and match the checkout. See refresh-kio.sh.
sh "$SCRIPT_DIR/refresh-kio.sh"

# The .vsix is pre-built by the `Publish Dev Container` workflow on
# a GHA runner and baked into the image at /opt/kio-extension/kio.vsix
# (see Dockerfile and .devcontainer/README.md § Two pipelines). We
# install from there rather than from a workspace path so the install
# does not depend on the user having run on-create.sh / `npm run
# package` locally.
#
# $KIO_DEVCONTAINER_VSIX overrides the location. The lifecycle check at
# ci/checks/orchestrators/devcontainer-lifecycle.sh runs this hook for
# real — a VS Code Server it staged, a .vsix it built — and it has no
# image to read /opt from.
VSIX=${KIO_DEVCONTAINER_VSIX:-/opt/kio-extension/kio.vsix}
if [ ! -f "$VSIX" ]; then
  echo "post-attach: $VSIX not found; skipping extension install (image build skipped the bake step?)"
  exit 0
fi

attempts=0
while ! cli=$("$SCRIPT_DIR/find-code-cli.sh" \
  "${HOME:-}/.vscode-server" \
  "${HOME:-}/.vscode-server-insiders" \
  /vscode/vscode-server)
do
  attempts=$((attempts + 1))
  if [ "$attempts" -ge 15 ]; then
    echo "post-attach: no VS Code CLI found; skipping extension install (no editor attached?)"
    exit 0
  fi
  sleep 1
done

echo "post-attach: installing vscode-kio from $VSIX using $cli"
"$cli" --install-extension "$VSIX" --force
