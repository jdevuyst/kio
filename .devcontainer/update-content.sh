#!/bin/sh
#
# Dev container lifecycle: updateContentCommand.
#
# Runs whenever the workspace content changes — and during a
# Codespaces prebuild, after `onCreateCommand`. The prebuild snapshot
# captures the filesystem right after this command finishes, so this
# is the place to warm artifacts a fresh Codespace should land on:
#
#   - `cargo build --tests` per Rust workspace pre-populates each
#     `target/` directory with stable + test-deps compiled.
#
# We do NOT package the vscode-kio extension here. vsce v3's
# `package` step hangs on Codespaces infrastructure. The .vsix is
# pre-built by the `Publish Dev Container` workflow on a GHA runner
# and baked into the image at `/opt/kio-extension/kio.vsix`;
# `post-attach.sh` installs from there. See
# `.devcontainer/README.md` § Two pipelines.
#
# Each work path is guarded so a partial workspace does not fail the
# hook. Idempotent: repeated invocations are no-ops once the build is
# warm.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)

cd "$REPO_ROOT"

if command -v mise >/dev/null 2>&1 && [ -f mise.toml ]; then
  mise trust mise.toml >/dev/null
fi

cargo_build_tests_workspace() {
  ws=$1
  if [ -f "$ws/Cargo.toml" ]; then
    echo "update-content: cargo build --tests in $ws"
    ( cd "$ws" && sh "$REPO_ROOT/ci/cargo.sh" build --tests )
  else
    echo "update-content: skipping $ws (no Cargo.toml)"
  fi
}

# Rust workspaces. The fuzz crate (kio-rs/fuzz) requires nightly Rust
# and is a reporting harness, not a dev-loop crate — skip it.
cargo_build_tests_workspace kio-rs
cargo_build_tests_workspace ci/infra/kio-ci-scheduler-rs
cargo_build_tests_workspace ci/infra/kio-gen-rs
cargo_build_tests_workspace ci/infra/kio-prime-check-rs
cargo_build_tests_workspace ci/infra/kio-test-runner-rs

# `cargo build --tests` builds test harnesses, not the `kio` binary the
# editor's language server spawns. Build it here too so a prebuild
# snapshot lands with it warm; `post-attach.sh` keeps it current from
# there. See refresh-kio.sh.
sh "$SCRIPT_DIR/refresh-kio.sh"

# vscode-kio packaging is intentionally NOT done here — see the
# header comment. The .vsix is baked into the image by the publish
# workflow and installed by `post-attach.sh`.
