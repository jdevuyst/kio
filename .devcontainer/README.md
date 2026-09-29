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

## Two pipelines, one Dockerfile

The dev container feeds two pipelines, but only one of them
actually builds the Dockerfile. The other pulls the result.

| | Triggered by | Produces | Consumed by |
| --- | --- | --- | --- |
| `.github/workflows/devcontainer-publish.yml` | monthly schedule or `workflow_dispatch` after image inputs change | A tagged Docker image at `ghcr.io/jdevuyst/kio-devcontainer` containing the core toolchain **and** a pre-built vscode-kio `kio.vsix` at `/opt/kio-extension/kio.vsix` | The Pages build job (via `container:`); local "Reopen in Container"; Codespaces (both regular launches and prebuilds, via the `image:` field in `devcontainer.json`) |
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
