# Publication wiring and evidence

Read the candidate versions of these owning files. This table is a routing map;
workflow/configuration files define their actual triggers, permissions, and commands.

| Channel | Owning files | Evidence and dependencies |
| --- | --- | --- |
| Tests and Checks | [ci.yml](../../../../.github/workflows/ci.yml), [local-ci.md](../../../topics/local-ci.md) | Monthly/manual; Linux gate and narrower macOS/Windows portability jobs; record requested coverage and exact head SHA. Linux provisions mise directly. |
| Binary release | [release.yml](../../../../.github/workflows/release.yml), [dist-workspace.toml](../../../../dist-workspace.toml), [distribution wrapper](../../../../kio-rs/dist/Cargo.toml) | Version-looking tag; plan → local/global artifacts → host → registry publishers → announce; no CI dependency. Successful host creates GitHub release assets before registry publishers start. Inspect planned targets, installers, hashes, and actual release assets. Tag planning can create hosting state before builds complete. |
| crates.io | [publish-crate.yml](../../../../.github/workflows/publish-crate.yml), [compiler manifest](../../../../kio-rs/Cargo.toml) | Reusable cargo-dist publisher after successful host; `releases/v*`, matching plan tag, canonical repository guard, `publish` environment, `CARGO_REGISTRY_TOKEN`. Publishes `kio-lang` from the caller's exact source SHA. Verify registry version, packaged source provenance, download and installation. |
| Marketplace / Open VSX | [publish-vscode.yml](../../../../.github/workflows/publish-vscode.yml), [extension manifest](../../../../tools/vscode-kio/package.json), [extension development](../../../../tools/vscode-kio/DEVELOPMENT.md) | Reusable cargo-dist publisher after successful host, with the same source/tag/repository gates. One packaging job inspects and uploads `kio-vsix`; separate registry jobs download that artifact by ID. Publisher/namespace `kio-lang`, extension `kio`. `VSCE_PAT` and `OVSX_PAT` jobs independently warn and succeed when absent. Artifact availability is not registry proof. |
| Devcontainer | [devcontainer-publish.yml](../../../../.github/workflows/devcontainer-publish.yml), [Dockerfile](../../../../.devcontainer/Dockerfile), [consumer manifest](../../../../.devcontainer/devcontainer.json), [devcontainer documentation](../../../../.devcontainer/README.md) | Monthly/manual; builds baked VSIX and amd64/arm64 GHCR image using `GITHUB_TOKEN` package permissions. Writes `latest` and `sha-<short>`; reruns can overwrite either tag. Record index and platform digests, source revision, installed/baked payload hashes and actual consumer image. |
| Website | [pages.yml](../../../../.github/workflows/pages.yml), [website package](../../../../website/package.json) | Manual; build consumes GHCR `latest`, then deploys through `github-pages` environment. Verify dispatch SHA, actual image input, deployment URL, representative assets and links. |
| Installers | [website installer wrapper](../../../../website/.vitepress/public/install.sh), [README](../../../../README.md), [INSTALL.md](../../../../INSTALL.md), [source install guide](../../../../docs/guides/install-from-source.md) | Wrapper's first download is only one hop; it downloads the latest GitHub release installer, which downloads the binary. Run the actual released path in an isolated authorized environment; check installed executable path/version and useful CLI behavior. |

## Artifact and consumer checks

Inspect the actual VSIX archive as documented, including grammar/WASM/runtime
files, package identity/version, license bundle, and exclusion of local settings.
Keep the inspected/downloaded payload checksum and its source provenance with any
manual-upload handoff. Verify Marketplace and Open VSX independently; a catalog,
download, and installation result can differ from a delayed HTML listing page.

The native Linux published-image check is
[`ci/infra/devcontainer-acceptance.sh`](../../../../ci/infra/devcontainer-acceptance.sh).
Read its prerequisites and interpretation in
[`.devcontainer/README.md`](../../../../.devcontainer/README.md) before running it.
It requires a digest-pinned published image, official editor/server artifacts,
explicit resource limits, and a distinct compiler-refresh commit. It retains
provenance and tests lifecycle/editor behavior, but does not establish Docker
Desktop transport, other architectures, or hosted Codespaces/browser acceptance.
If an input or platform is unavailable, report that gap rather than inventing an
equivalent result or changing the script's contract.

For manual desktop and Codespaces checks, establish the checkout, image,
installed VSIX, and compiler actually used. A browser UI connected to a remote
Node workspace can run `kio lsp`; browser-only extension hosts provide highlighting.
Test attach, reconnect, and compiler discovery against the installed artifact;
a stale baked extension or unrelated compiler on PATH can make an apparent
successful test concern different code.

## Partial publication and live readiness

Recheck publish-environment permitted refs and approvals against the tagged
workflow's `environment` declaration. Secret-name presence is not credential
validity. Check tag rules/immutable releases and registry version occupancy before
creation. Repository visibility does not establish GHCR anonymous access or Pages
configuration. Read-only inspection does not authorize changing any settings.

After partial publication, preserve the exact successful version/source/artifact
receipts and inspect which retry will run which steps. Retry the failed registry
job while retaining the successful packaging job and immutable artifact ID;
rerunning the whole Release workflow can rebuild packages and repeat successful
publishers. A crate version may already be occupied even when another job failed.
Escalate an unavailable selective retry or a publication-order conflict as a
concrete decision. Never assume tag removal, yanking, or deleting a listing permits
reusing the published version.
