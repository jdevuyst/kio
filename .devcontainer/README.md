# Dev container

This directory defines the Kio dev container: a core image consumed by
local VS Code "Reopen in Container", GitHub Codespaces, and the Pages build.
The Linux test gate instead installs the shared mise tool pins directly
on its hosted runner.
The Dockerfile pins mise; the repository root `mise.toml` and
`mise.lock` pin both the core tools the image installs and the target
extras CI installs on demand. The lifecycle hooks wired in
`devcontainer.json` (`on-create.sh`, `update-content.sh`,
`post-attach.sh`) trust the workspace mise config, warm Cargo
registries, install npm dependencies, build the `kio` binary the editor's
language server runs (`refresh-kio.sh`), and install the baked vscode-kio
extension into the editor (`find-code-cli.sh` locates the CLI that
performs the install).

## The language server's binary

`devcontainer.json` sets `KIO_BIN` to `kio-rs/target/debug/kio`, and the
vscode-kio extension resolves its server as `kio.server.path` → `KIO_BIN`
→ `PATH`. So the server is a binary the container names explicitly rather
than whatever `PATH` happens to find first — `target/release` is ahead of
`target/debug` on `PATH`, and `target/` is git-ignored, so an artifact from
any past build outlives every `git pull` and would shadow the current one
indefinitely. `refresh-kio.sh` builds that exact path on every attach;
cargo rebuilds only when the sources moved, so a warm container pays
nothing.

Debug rather than release: a debug rebuild after a pull costs seconds
against a release rebuild's minutes, and that cost would land on every
attach. A developer who wants a release server points `kio.server.path` at
one.

`ci/checks/orchestrators/devcontainer-lifecycle.sh` runs `post-attach.sh`
against a real VS Code Server and asserts the extension lands; it points
the hook at a `.vsix` it builds via `KIO_DEVCONTAINER_VSIX`, since it has
no image to read `/opt/kio-extension/` from.

## Published-image acceptance

Release and monthly/manual image publication use
`ci/infra/devcontainer-acceptance.sh` on a native Linux Docker host.
This check pulls an immutable published image, creates a clean
checkout at the image's source revision, runs its configured lifecycle
as `vscode`, and tests the installed baked VSIX in a real headless editor.
It repeats the editor checks after reconnect and after refreshing to an
explicit compiler commit. Removing the installed extension's executable
must cause an activation failure; restoring it must restore the editor
checks. It builds the compiler through the
lifecycle hooks; it does not build an image or replacement VSIX. Publication
tests the candidate by digest before promoting that same digest to its
source-specific tag and advancing `latest`. A proven newer `latest` is retained
when publishing an older accepted release; failure leaves it in place. The
release coordinator then allows registry publication, reusing the inspected
VSIX baked into the image.

Install the official consumer CLI from the optional mise pins:

```sh
mise -E optional install --locked 'npm:@devcontainers/cli'
```

Supply cached official VS Code Server and Linux Electron distributions
from the same editor commit, a full refresh commit that changes compiler
inputs, a fresh absolute output directory, and explicit Docker CPU and
memory limits:

```sh
sh ci/infra/devcontainer-acceptance.sh \
  'ghcr.io/jdevuyst/kio-devcontainer@sha256:<published-digest>' \
  '<full-refresh-commit>' "$SERVER_ARCHIVE" "$EDITOR_DIRECTORY" \
  "$ACCEPTANCE_OUTPUT" "$ACCEPTANCE_CPUS" "$ACCEPTANCE_MEMORY"
```

Set `TMPDIR` to an existing temporary directory before running the check.
The host also needs Docker Buildx, Git, Node, and the extension's locked
Node dependencies (`npm ci` in `tools/vscode-kio`). The editor directory
contains `code`, `bin/code`, and `resources/app/product.json`; the server
archive contains the official server's `bin/code-server` and
`product.json`. A missing tool, required hook, compiler or installed
extension is a failure, not a successful skip.
The host checkout owner must have the published image's `vscode` UID;
the check verifies this before creation and disables UID-adjustment image
builds so the tested image remains the published artifact.

The output retains phase logs, image and platform-manifest digests,
checkout commits, baked and installed payload hashes, editor provenance,
timings, cgroup resource observations and the final container state.
The script removes only its labelled container and retains its private
checkout and receipt for inspection. Cold creation fetches dependencies
and builds tests in five Rust workspaces, so reserve disk and memory for
those artifacts before launch. CPU and memory limits are experimental
inputs, not asserted minimum requirements. A SIGKILL without resource
evidence is not classified as OOM.

This check covers the native Linux image/lifecycle/installed-extension
composition. Docker Desktop/macOS, the desktop Remote Containers
transport, Codespaces orchestration, browser remote UI and other
architectures need separate integration evidence. Review the retained
receipt independently before treating it as release acceptance.

The orchestration assertions also have a lightweight local selftest:

```sh
node ci/infra/devcontainer-acceptance/driver.cjs selftest
```

## Two pipelines, one Dockerfile

The dev container feeds two pipelines, but only one of them
actually builds the Dockerfile. The other pulls the result.

| | Triggered by | Produces | Consumed by |
| --- | --- | --- | --- |
| `.github/workflows/devcontainer-candidate.yml`, called by release publication or `devcontainer-publish.yml` | a release, monthly schedule or `workflow_dispatch` after image inputs change | An accepted Docker image at `ghcr.io/jdevuyst/kio-devcontainer` containing the core toolchain **and** the inspected vscode-kio `kio.vsix` at `/opt/kio-extension/kio.vsix` | The Pages build job (via `container:`); local "Reopen in Container"; Codespaces (both regular launches and prebuilds, via the `image:` field in `devcontainer.json`) |
| Codespaces prebuild (configured in the UI — see below) | the path-filter list below, plus the schedule chosen in the UI | A Codespaces snapshot stored by GitHub's Codespaces service (not a Docker image) | Codespace launches |

`devcontainer.json` references the published image via `"image":
"ghcr.io/jdevuyst/kio-devcontainer:latest"` — it does **not**
re-build the local Dockerfile. So a change to `.devcontainer/Dockerfile`
flows: publish workflow rebuilds the image → next Codespaces
prebuild pulls the new `:latest` and re-runs the lifecycle scripts
on top of it. One Dockerfile build per change, not two.

The GHCR image carries the mise-managed core toolchain (`cargo`, `node`,
Python, tree-sitter, lints, etc.) **plus** the pre-built vscode-kio
extension (`.vsix`). Go, Swift, Java, and Haskell stay pinned in
`mise.toml` / `mise.lock` as backend extras; Linux CI selects them with
`ci/impl-toolchain.sh` before invoking `ci/all.sh`. When those extras
need distro shared libraries, the script installs them through
`mise bootstrap packages apply apt:...`.
Interactive terminals print a one-time hint with currently runnable
impl selectors, all accepted impl selectors, and the
`ci/impl-toolchain.sh install ...` command. Set
`KIO_DEVCONTAINER_HIDE_TARGET_HELP=1` to suppress that hint.
The Codespaces snapshot adds the repo cloned and the lifecycle scripts
already run (`cargo fetch` done, `npm ci` done, the extension installed
into the editor from the baked `.vsix`), so a Codespace boots in seconds
instead of waiting on the cold `on-create.sh` / `update-content.sh`
walk.

Why bake the `.vsix` into the image rather than build it in the
Codespaces prebuild? `vsce package` (the extension packager) hangs
on Codespaces infrastructure — the cause has not been pinned down,
but the symptom is a deterministic stall right after vsce's
`LICENSE not found` warning, regardless of `--no-dependencies` /
`--baseContentUrl` / other flags. On a GHA `ubuntu-latest` runner
the same command completes in seconds. Building in the publish
workflow sidesteps the whole class of vsce-in-Codespaces issues.

One race to know about: a single commit that changes **both** the
Dockerfile and `devcontainer.json` (or the lifecycle scripts) can
have the Codespaces prebuild start before a manually triggered publish
workflow finishes, in which case the prebuild pulls the stale `:latest`.
The next prebuild trigger picks up the fresh image. The case is rare
enough that it isn't worth coupling the two pipelines via
`workflow_run` chaining.

## Codespaces prebuild configuration

Codespaces prebuilds are configured through the GitHub UI at
`Settings → Codespaces → Prebuilds`; there is no in-repo prebuild
config file. The values below are the source of truth — re-apply
them through the UI after a Codespaces UI change or when adding a
new prebuild region.

Trigger — `File and directory paths`:

```
.devcontainer/**
**/Cargo.lock
**/package-lock.json
tools/vscode-kio/**
mise.toml
mise.lock
```

Branch: `main`.

Region: `US East` (default; expand to other regions if contributor
demand surfaces).

## Verifying a prebuild took

Open a fresh Codespace from `main` after the prebuild run completes.
The Codespaces creation dialog shows a `Prebuilt` badge when the
snapshot is in use; the Settings → Codespaces → Prebuilds status
table lists per-region completion times for the most recent runs.
