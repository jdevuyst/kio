# Publication wiring and evidence

Read the candidate versions of these owning files. This table is a routing map;
workflow/configuration files define their actual triggers, permissions, and commands.

| Channel | Owning files | Evidence and dependencies |
| --- | --- | --- |
| Tests and Checks | [ci.yml](../../../../.github/workflows/ci.yml), [local-ci.md](../../../topics/local-ci.md) | Monthly/manual; Linux gate and narrower macOS/Windows portability jobs; record requested coverage and exact head SHA. Linux provisions mise directly. |
| Credential preflight | [release-credentials.yml](../../../../.github/workflows/release-credentials.yml), [credential inventory](../../../../ci/infra/release-credentials.json), [checker](../../../../ci/infra/release-credentials.cjs) | Manual on trusted `main` and an automatic release prerequisite, protected by the `publish` environment; no publication. Resolve stored-secret scope and forwarding through every reusable caller. Run manually before any release tag or hosting-state creation. Missing/rejected credentials and unavailable verification fail; scope/access limits remain explicit. Exact generated workflow freshness runs before credential verification; planning can create hosting state in parallel with the automatic check. |
| Binary release | [release.yml](../../../../.github/workflows/release.yml), [dist-workspace.toml](../../../../dist-workspace.toml), [distribution wrapper](../../../../kio-rs/dist/Cargo.toml) | Version-looking tag; plan → local/global artifacts and preparation → host → registry publishers → announce → prepared Pages deploy; no CI dependency. Host is the immutable-publication point of no return and requires successful preparation plus an explicit ready output. Inspect planned targets, installers, hashes, and actual release assets. Tag planning can create hosting state before builds complete. |
| Preparation and publication | [prepare-release.yml](../../../../.github/workflows/prepare-release.yml), [publish-release.yml](../../../../.github/workflows/publish-release.yml), [package-vscode.yml](../../../../.github/workflows/package-vscode.yml), [devcontainer-candidate.yml](../../../../.github/workflows/devcontainer-candidate.yml) | Before host publication, packages one inspected VSIX, builds/accepts/promotes its candidate image, verifies the crate and builds/smoke-tests the site. Registry publishers consume those identities only after binary hosting. Record source SHA, VSIX artifact ID/hash, image index/platform digests and acceptance receipt. A failed acceptance or promotion blocks all registry leaves. |
| crates.io | [publish-crate.yml](../../../../.github/workflows/publish-crate.yml), [compiler manifest](../../../../kio-rs/Cargo.toml) | Coordinator invokes it after successful binary hosting and container acceptance/promotion; `releases/v*`, matching plan tag, canonical repository guard, `publish` environment, `CARGO_REGISTRY_TOKEN`. Publishes `kio-lang` from the caller's exact source SHA. Verify registry version, packaged source provenance, download and installation. |
| Marketplace / Open VSX | [publish-vscode.yml](../../../../.github/workflows/publish-vscode.yml), [extension manifest](../../../../tools/vscode-kio/package.json), [extension development](../../../../tools/vscode-kio/DEVELOPMENT.md) | Coordinator supplies the exact inspected VSIX artifact ID and hash used by the accepted container. Separate registry leaves publish those bytes, with the source/tag/repository guards intact. Publisher/namespace `kio-lang`, extension `kio`. `VSCE_PAT` and `OVSX_PAT` jobs independently warn and succeed when absent. Artifact availability is not registry proof. |
| Devcontainer | [devcontainer-publish.yml](../../../../.github/workflows/devcontainer-publish.yml), [devcontainer-candidate.yml](../../../../.github/workflows/devcontainer-candidate.yml), [accept-container.yml](../../../../.github/workflows/accept-container.yml), [Dockerfile](../../../../.devcontainer/Dockerfile), [consumer manifest](../../../../.devcontainer/devcontainer.json), [devcontainer documentation](../../../../.devcontainer/README.md) | Release and monthly/manual publication from `main` share candidate build → digest-pinned native acceptance → promotion. amd64/arm64 GHCR images use `GITHUB_TOKEN` package permissions; accepted candidates receive `sha-<full-source-SHA>` without rebuilding. Advance `latest` only when absent or its source is the same revision or an ancestor; retain a proven descendant, and fail promotion for divergent histories. Shared acceptance/promotion concurrency protects mutation. A pre-mutation promotion receipt preserves prior tag digests for guarded rollback. Record index and platform digests, source revision, installed/baked payload hashes and actual consumer image. |
| Website | [build-release-site.yml](../../../../.github/workflows/build-release-site.yml), [deploy-release-site.yml](../../../../.github/workflows/deploy-release-site.yml), [pages.yml](../../../../.github/workflows/pages.yml), [website package](../../../../website/package.json) | Release preparation builds from the exact source and accepted digest; after publication deploy that same artifact through `github-pages`. Its ref policy must admit release tags and main recovery. Standalone manual Pages builds use `latest`. Verify source, image, artifact, deployment URL, assets and links. |
| Installers | [website installer wrapper](../../../../website/.vitepress/public/install.sh), [README](../../../../README.md), [INSTALL.md](../../../../INSTALL.md), [source install guide](../../../../docs/guides/install-from-source.md) | Wrapper's first download is only one hop; it downloads the latest GitHub release installer, which downloads the binary. Run the actual released path in an isolated authorized environment; check installed executable path/version and useful CLI behavior. |

## Artifact and consumer checks

Inspect the actual VSIX archive as documented, including grammar/WASM/runtime
files, package identity/version, license bundle, and exclusion of local settings.
Keep the inspected/downloaded payload checksum and its source provenance with any
manual-upload handoff. Verify Marketplace and Open VSX independently; a catalog,
download, and installation result can differ from a delayed HTML listing page.

The native Linux candidate-image check is
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

Recovery preserves successful immutable publications and authenticates the
original source, run, VSIX and both-platform image identity. Check independent
registry occupancy before dispatching any remaining publishers.

Recheck publish-environment permitted refs and approvals against the tagged
workflow's `environment` declaration. Secret-name presence is not credential
validity. Check tag rules/immutable releases and registry version occupancy before
creation. Repository visibility does not establish GHCR anonymous access or Pages
configuration. Read-only inspection does not authorize changing any settings.

After partial publication, preserve the exact successful version/source/artifact
receipts and inspect which retry will run which steps. Retry the failed registry
leaf job, not its aggregate reusable-workflow caller, while retaining the
successful packaging job, immutable artifact ID/hash and accepted image digest;
rerunning the whole Release workflow can rebuild packages and repeat successful
publishers. A crate version may already be occupied even when another job failed.
Escalate an unavailable selective retry or a publication-order conflict as a
concrete decision. Never assume tag removal, yanking, or deleting a listing permits
reusing the published version.
