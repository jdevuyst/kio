#!/bin/sh
#
# Dev container lifecycle: onCreateCommand.
#
# Runs ONCE when the dev container is first created and during a
# Codespaces prebuild. Heavy first-touch warm-up belongs here so the
# cost is paid in the snapshot instead of at every Codespace launch:
#
#   - `cargo fetch` populates the per-user cargo registry cache so
#     subsequent builds are offline-resolvable.
#   - `npm ci` installs the Node toolchains used by the highlight
#     agreement driver and the vscode-kio extension build.
#   - the baked `/opt/kio-extension/kio.vsix` artifact is installed by
#     post-attach.sh once the VS Code Server is available.
#
# Each work path is guarded with `[ -d <path> ]` so a partial workspace
# (mid-rebase, fresh checkout that skipped a submodule, etc.) does not
# fail the lifecycle hook.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)

cd "$REPO_ROOT"

if command -v mise >/dev/null 2>&1 && [ -f mise.toml ]; then
  mise trust mise.toml >/dev/null
fi

cargo_fetch_workspace() {
  ws=$1
  if [ -f "$ws/Cargo.toml" ]; then
    echo "on-create: cargo fetch in $ws"
    ( cd "$ws" && sh "$REPO_ROOT/ci/cargo.sh" fetch )
  else
    echo "on-create: skipping $ws (no Cargo.toml)"
  fi
}

npm_ci_dir() {
  dir=$1
  if [ -f "$dir/package-lock.json" ]; then
    echo "on-create: npm ci in $dir"
    ( cd "$dir" && npm ci )
  else
    echo "on-create: skipping $dir (no package-lock.json)"
  fi
}

# Rust workspaces. The fuzz crate (kio-rs/fuzz) requires nightly Rust
# and is a reporting harness, not a dev-loop crate — skip it.
cargo_fetch_workspace kio-rs
cargo_fetch_workspace ci/infra/kio-ci-scheduler-rs
cargo_fetch_workspace ci/infra/kio-gen-rs
cargo_fetch_workspace ci/infra/kio-prime-check-rs
cargo_fetch_workspace ci/infra/kio-test-runner-rs

# Node toolchains.
npm_ci_dir ci/infra/highlight-agreement-js
npm_ci_dir tools/vscode-kio

# Note: we deliberately do NOT run `npm run package` here. vsce v3's
# `package` step hangs on Codespaces infrastructure (~exactly where
# vsce starts walking the workspace after the LICENSE warning). The
# .vsix is instead pre-built by the `Publish Dev Container` workflow
# on a GHA runner and copied into the image at
# `/opt/kio-extension/kio.vsix`; `post-attach.sh` installs from
# there. Contributors who want to test local extension changes can
# run `cd tools/vscode-kio && npm run package` manually — the `npm ci`
# above keeps the build dependencies primed.
