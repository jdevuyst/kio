# Local tools

Pointer: read when setting up the local development environment, troubleshooting tool versions, or checking local tool availability.

This page covers third-party tools and repository-script mechanics. It does not choose which checks to run or how to optimize a local work session; see [`local-ci.md`](local-ci.md) for Kio's check infrastructure and [`local-performance.md`](local-performance.md) for local performance tradeoffs.

Kio-owned debug, profiling, and timing-only switches are not third-party tool wiring. They use `KIO_DEBUG_...`, `kio debug ...`, or `--debug-...` surfaces and are catalogued in [`local-performance.md`](local-performance.md) § Debug probes.

## Script portability

Shared shell paths target the supported GNU/Linux and BSD/macOS environments; commands used by the Windows job must work under that job's declared shell, tools, path, and process semantics. Prefer POSIX `sh` and standardized utility syntax. Treat `/proc`, pidfd, GNU-only options, and util-linux-only behavior as platform-specific. Do not introduce or deepen such a dependency in a shared path; isolate a necessary mechanism behind capability detection and equivalent behavior, or keep a genuinely platform-specific check explicitly scoped at both script and caller. A Linux-only caller does not by itself narrow a shared script's contract.

The Windows scheduler route is deliberately a native binary behind the same
thin POSIX-shell facade. `ci/schedule.sh` and its bootstrap therefore require
the `sh` and `cygpath` environment supplied by Git for Windows/MSYS; they use
`cygpath` to bridge Git's shell paths to native Cargo and scheduler paths. This
is a prerequisite of the Windows shell entry point, not a second scheduling
implementation.

For each changed script, review its actual callers, state applicable platforms, and provide focused evidence there. ShellCheck verifies shell syntax, not the portability of external utilities. Existing non-portable paths remain repair work rather than authority for more platform-specific machinery.

`.devcontainer/` is the canonical core build and test environment for Kio.
The container and hosted CI use the shared provisioning owner described below.
Exact tool requests remain in root mise configuration/locks; optional Cargo-backed
audit/report tools use [`mise.optional.toml`](../../mise.optional.toml) so they
stay out of the default active environment.

Three audiences consume the same image:

- **Local VS Code** — "Reopen in Container" against `.devcontainer/devcontainer.json`.
- **GitHub Codespaces** — opens a prebuilt environment when the prebuild snapshot is current; the prebuild trigger lives in the Codespaces UI, with the source-of-truth values mirrored in [`.devcontainer/README.md`](../../.devcontainer/README.md).
- **Pages build** — standalone [`.github/workflows/pages.yml`](../../.github/workflows/pages.yml) uses `ghcr.io/jdevuyst/kio-devcontainer:latest`; release preparation instead uses [`.github/workflows/build-release-site.yml`](../../.github/workflows/build-release-site.yml) with the exact accepted image digest, retaining the built site for deployment. Release publication and monthly/manual [`.github/workflows/devcontainer-publish.yml`](../../.github/workflows/devcontainer-publish.yml) share candidate-image acceptance before promotion. The Linux test gate in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) instead installs core tools and the complete eight-backend sampling pool directly on its hosted runner through `ci/impl-toolchain.sh`. Native portability jobs consume the same pin and provisioning owners for their declared language sets.

Tool-version changes update their native mise configuration/locks or delegated
manager manifest under `tools/toolchains/`, never copied workflow constants.
Dependency upgrades follow [`upgrade-deps`](../skills/upgrade-deps/SKILL.md).

## Website browser tasks

The core mise tool `http:kio-firefox` installs one pinned Playwright Firefox
revision on Linux x64/arm64, macOS x64/arm64, and Windows x64. Its native ZIP
archives and SHA256 checksums live in `mise.toml`/`mise.lock`; the matching
`playwright-core` driver is pinned in `website/package.json`. Update that pair
together. There is no separate browser cache installer or system browser lookup.
See [`INSTALL.md`](../../INSTALL.md) for runtime floors and the mise-bootstrap
Linux library helper; macOS and Windows need no extra browser install step.

After the website is built, `cd website && npm run browser:smoke` reuses its
dist directory and tests real page/worker/WASM startup and one keyboard REPL
command. `npm run browser:capture -- --output NEW_DIRECTORY --page
/kio/docs/guides/repl.html` captures desktop/narrow rendered viewports through
the same launcher. `--dist DIRECTORY` selects another existing build;
`--full-page` captures the document beyond the viewport. Inspect the resulting
images for the changed content and surrounding layout; producing them is not
a visual review. CI runs the automated smoke, not an agent visual review.

Runs inherit `TMPDIR`, use a fresh owned profile and localhost preview server,
and clean them on exit. Failures retain a bounded diagnostic report and rendered
screenshot under the reported temporary directory. Operational waits are
state-based and bounded; there is no elapsed-time performance assertion or
whole-test retry. Firefox uses upstream Playwright automation defaults, including
disabled cross-process iframes, while its OS content sandbox remains enabled.
If launch fails, report the setup error and repair the supported runtime;
never add a sandbox opt-out or turn a missing browser into a skipped gate.

`npm run browser:history` builds a private tiny Git-backed documentation fixture
once and checks its static output plus real rendered version navigation through
the same browser launcher. It compiles no Rust/WASM. For static-only focused
iteration, `npm run check:history-site` runs that fixture without a browser.

## Dev container Docker builds

`devcontainer.json` pulls the published GHCR image; it does not build the local Dockerfile. To test the Dockerfile itself, reproduce the publish workflow's build-context pre-step first: build `tools/vscode-kio/kio.vsix`, copy it to `.devcontainer/kio.vsix`, then run `docker buildx build` against `.devcontainer/Dockerfile`. Remove `.devcontainer/kio.vsix` after the build so the checkout stays clean.

```sh
cd tools/vscode-kio
npm ci --no-fund --no-audit
npm run package
cd ../..
cp tools/vscode-kio/kio.vsix .devcontainer/kio.vsix

docker buildx build --platform linux/amd64 --progress=plain --load \
  -f .devcontainer/Dockerfile \
  -t kio-devcontainer-local:test .

rm -f .devcontainer/kio.vsix
```

The publish workflow builds both `linux/amd64` and `linux/arm64` with QEMU; a local smoke test may target only the host architecture unless the change specifically needs the emulated architecture path.

Published composition acceptance is a separate release-only check:
[`ci/infra/devcontainer-acceptance.sh`](../../ci/infra/devcontainer-acceptance.sh).
Its official `npm:@devcontainers/cli` pin belongs to the optional mise
environment, outside the default tool pool. Run it with an immutable
published image and cached official editor/server distributions as
described in [`.devcontainer/README.md`](../../.devcontainer/README.md#published-image-acceptance).
It tests the baked VSIX rather than an extension rebuilt from source;
the ordinary lifecycle/editor orchestrators retain their distinct
source-build checks. Reserve a cold-build resource slot and retain the
receipt's explicit architecture and integration gaps.

Docker image and BuildKit state is daemon-wide, not worktree-local. Inspect it before and after a devcontainer build with:

```sh
docker system df
docker image ls 'kio-devcontainer-local'
```

Prune Docker state only under disk pressure, with no active `docker build` / `docker buildx build` running, and never as part of the default `clear-caches` worktree wipe. Remove known local test tags explicitly (`docker image rm kio-devcontainer-local:test`) before using broader daemon-wide commands. If the remaining BuildKit cache is the pressure point, use `docker builder prune` deliberately; it drops warm layers for every Docker build sharing that daemon.

## Toolchain provisioning policy

Linux CI installs all eight backend toolchains for both per-case sampling and
explicit full-matrix execution. `SAMPLE_IMPL` chooses from every configured
implementation, so omitting a toolchain would leave an invalid sampling pool.
`ci/impl-toolchain.sh tooling-impls` closes requested tool lists over ambient
host compilers used by availability-driven checks. An ambient system compiler
or executable shim is not a substitute for installation from the root mise
pins. Tool availability and per-case execution multiplicity are separate axes.

### Ownership and installation

Use `sh ci/impl-toolchain.sh install-core` for ordinary core development. On
Linux/macOS it installs the shared Node.js/TypeScript/Python/Rust environment and
general helpers; Windows selects its Rust/compiler route. It does not implicitly
install all target languages. The human entry point
`ci/install-target-language-toolchains.sh` accepts explicit language IDs,
`--supported-set`, or `--portability-set`; no arguments display help. These sets
are distinct on Linux as well as native portability hosts. Linux supports all
eight languages; macOS supports JS/TS/Rust/Swift; Windows supports Rust. Requests
outside the native supported sets warn before setup that it is best-effort,
including macOS core setup's Python helper dependency.
Do not infer full-suite platform support from a native compiler/portability job.

General runtimes, package managers, linters, formatters, and helper CLIs start
from tracked mise configuration. Target-language toolchains use mise directly
where suitable and bounded ecosystem-manager handlers where needed. Exact
requests and route-specific download identities belong to root `mise.toml`/`mise.lock`, native
`mise.<environment>.toml`/lock overlays, and the delegated manifests under
`tools/toolchains/`. `development` selects the root configuration. The shared
`ci/impl-toolchain.sh` owner maps languages and implementation selectors to
dependencies; its handlers own setup and verification. Do not introduce another
language/version table in a facade, workflow, or documentation.

`tools/toolchains/mise.version` and `mise.sha256` pin and verify the provisioning
bootstrap. `rustup.toml` owns the exact Rustup bootstrap; the mise Rust request
owns compiler version/options. `ghcup.toml` owns exact child GHC requests plus a
commit-pinned, checksummed upstream metadata document; mise owns GHCup itself.
Pinning a manager alone is insufficient. Native SDKs and system libraries are
explicit external prerequisites, with the same selected compiler version locally
and in GitHub jobs. Do not silently substitute an ambient compiler or download a
different version because a platform route fails.

Swift archive selection and Linux signature verification follow the
[native-route exception in INSTALL.md](../../INSTALL.md#tool-ownership-and-storage).

“Through mise” includes mise tool specs that deliberately delegate to a package manager installed by mise: `cargo:*` for Cargo-backed CLIs, `npm:*` for npm-backed CLIs, and pipx-backed Python CLIs such as `yamllint`. Those tools are allowed because the tool name, version, and install entry point are declared in the mise config and lock files. Do not add direct `cargo install`, `npm install -g`, or `pipx install` steps in the Dockerfile, CI, or setup docs.

Prefer maintained binary routes and ordinary mise tools before adding glue.
Delegation uses a bounded upstream-manager handler, not arbitrary executable
recipes in a data manifest. When distro shared libraries are needed, route them
through `mise bootstrap packages apply` with explicit manager-qualified specs
such as `apt:libgmp-dev`; privileged system-package work remains visible.

Source builds are visible cost, not the default. An intentional source-building
route needs an explicit reason and tracked mechanism/pins. Optional Cargo-backed
report/audit tools may compile from source; `mise.optional.toml` owns their
versions and `sh ci/impl-toolchain.sh install-report-tools` installs them on
demand. They stay out of ordinary core setup.

Python is shared by core checks and Python-backend execution. Its exact mise
pin differs from its documented runtime floor. Select it through the
project execution owner (`python3` on Linux/macOS; `python` on Windows).

When a tool needs GitHub release metadata, use its locked route where possible.
CI supplies a token explicitly when needed. Docker builds do not inherit one;
pass it as a BuildKit secret and expose it only for that install command as
`MISE_GITHUB_TOKEN`.

If an explicitly requested supported route fails, report the setup failure.
Do not narrow a requested implementation set to installed tools, silently skip
it, or use `continue-on-error`. Best-effort native requests remain visible gaps,
not conformance evidence. A deliberate platform coverage change follows the
ordinary contract/scope gates.

Homebrew, Scoop, Chocolatey, and OS package managers bootstrap mise or explicit
OS/SDK prerequisites; they do not silently replace tracked toolchain routes.

## Toolchains on PATH: selected execution environments

The default target-language store is `<checkout>/.toolchains/`, including mise
data/cache/state/configuration and child Rustup/Cargo/GHCup stores. General
helpers retain ordinary mise storage. `KIO_TOOLCHAINS_ROOT` permits an explicitly
chosen absolute shared root; use it consistently for setup and execution.
Setup writers serialize within that store. Remove or change it only while its
users are idle; no automatic cleanup policy makes active compilers safe to delete.
The store contains installed payloads and regenerable download caches; those
are different cleanup scopes. The default `clear-caches` workflow removes neither.
Deliberate download-cache cleanup leaves compiler payloads and manager selection
state intact; whole-store removal requires reinstalling the selected environments.
Do not replace `HOME`, change account defaults, edit shell profiles, clear Cargo
configuration/credentials, or bypass configured compiler caches to achieve
isolation.

Use the shared execution operation in interactive and non-interactive shells:

```sh
sh ci/impl-toolchain.sh toolchain-verify rust
sh ci/impl-toolchain.sh toolchain-exec rust --core -- sh ci/cargo.sh build --manifest-path kio-rs/Cargo.toml
sh ci/impl-toolchain.sh toolchain-exec python --environment ENVIRONMENT -- python3 --version
```

`toolchain-environments` lists valid IDs; `development` is the default. Setup
and execution take the same `--environment` argument. Execution verifies exact
versions and origins before launching, preserves cwd and required SDK/manager
state, disables install-on-execution, and carries selection into nested mise
execution. `--core` also verifies/activates the general helper pool for repository
builds and gates; omit it for a plain tool witness or Windows's Rust-only route.
Use separate per-process selections for concurrent endpoints. A child
installer cannot mutate its parent's PATH; ordinary mise activation or adding
an account GHCup directory is insufficient to select this project environment.
Agents need no `BASH_ENV` or profile changes when using this operation.

If an entire backend fails similarly, verify the selected tools before diagnosing
the runner. A missing compiler or wrong origin is a provisioning error, never
evidence that the backend is unsupported. `toolchain-plan` inspects exact
selection without downloading; `toolchain-verify` checks existing setup. A bare
checkout can run help/list immediately; plan inspection needs the pinned mise,
obtainable separately with `sh ci/impl-toolchain.sh toolchain-bootstrap`.

Verified execution uses an offline configuration snapshot. Upstream version
discovery and native lock regeneration instead run the pinned bootstrap
executable from an ordinary shell against the tracked checkout configuration;
see [upgrade-deps](../skills/upgrade-deps/SKILL.md) for that maintenance path.
Do not update pins or locks inside an activated snapshot.

Ordinary CI restores the actual owned stores, verifies them even on cache hits,
and saves only after successful complete setup. `toolchain-key` derives a key
from platform, exact configuration, provisioning recipes, store root, and
selection. Include delegated child stores; general mise caching alone omits
them. Receipts record observations and never become another editable pin source.
Release preparation exercises fresh install/download roots and tiny native
witnesses through these same routes; cached tools do not prove cold download
success. See the release skill for outage handling and publication ordering.

## Compiler cache

Run the tool commands below inside the selected execution environment, for example
`sh ci/impl-toolchain.sh toolchain-exec rust --core -- sccache --dist-status`.
The target-language installer does not put general helpers on its parent's PATH.

Two cache layers accelerate the compiled backends, stacked: the **test-runner artifact cache** (content-addressed compiled binaries, keyed by source bytes + toolchain) sits above the **sccache compiler wrapper** (object-level rustc caching). A warm artifact cache skips the compile entirely; sccache only speeds up the artifact cache's *misses*, and only for rust.

### Test-runner artifact cache

The Rust, Go, Haskell, and Swift test runners cache the final compiled per-golden binary content-addressed under a cache root, so a warm run skips the compiler entirely (the Rust runner caches two artifacts — package rlib + driver bin; Go / Haskell / Swift cache one final binary). The cache machinery, its keying, the path-neutral artifacts, and the env-var grammar live in [`ci/infra/kio-test-runner-rs/README.md`](../../ci/infra/kio-test-runner-rs/README.md) § Runner build cache and compiler wrappers.

The orchestrators (`ci/checks/orchestrators/*-tests.sh`) default this cache to a **machine-stable shared location**, so sibling worktrees reuse one cache — the cache key is path-normalized and the cached artifacts are path-neutral, so a hit warmed from worktree A serves worktree B. The location mirrors how sccache picks `~/.cache/sccache`:

- `$XDG_CACHE_HOME/kio/<suite>/` when `XDG_CACHE_HOME` is set, else `~/.cache/kio/<suite>/`, where `<suite>` is `goldens` / `castles` / `poc` / `kio-gen` / `kiodoc`. Each impl-target writes its own `…/<suite>/<target>/` subtree. Emissions are absent deliberately: their custom fixed-host/artifact scripts do not use the synthesized per-backend runner or its persistent artifact cache.

Because the location is shared, **clearing it is machine-wide** — it affects every parallel worktree, like clearing sccache. It is content-addressed (a stale entry is never *wrong*, only disk) and self-bounding (size-LRU below), so manual clearing is rarely needed; reach for it only under disk pressure. The [`clear-caches`](../skills/clear-caches/SKILL.md) skill leaves this shared cache alone by default for exactly this reason and treats it as a separate, explicit step.

`KIO_TEST_RUNNER_BUILD_CACHE_SIZE` bounds it. The orchestrators default it to a per-impl size-LRU cap (sccache-shaped; no max-age sweep) sized to roughly two generations of the largest target's golden corpus, so a single `ci/all.sh` run never evicts its own early entries mid-run. The cap is **per impl-target**: each runner prunes only its own `…/<suite>/<target>/` subtree on write, so one target filling its corpus can't evict another target's or another suite's entries. Override it (e.g. to tighten under disk pressure) by exporting `KIO_TEST_RUNNER_BUILD_CACHE_SIZE` before the orchestrator; an external value wins. To pin the cache worktree-local for a hermetic job, append `-- --cache-base=<dir>` to the orchestrator (run-tests.sh takes the last `--cache-base`).

`KIO_TEST_RUNNER_COMPILER_WRAPPER` ties the two layers together for rust: it wraps the artifact cache's *miss* compile as `<wrapper> rustc …`, so `KIO_TEST_RUNNER_COMPILER_WRAPPER=sccache` makes a cold rust artifact cheaper without changing the artifact key (the key uses the real rustc identity). The Go, Java, Haskell, and Swift runners ignore the wrapper — sccache wraps C/C++/rustc-shaped compilers and rejects `go` / `javac` / `ghc` / `swiftc` (it passes `-E`) — so for those backends the artifact cache or direct compiler invocation is the only acceleration layer.

The debug-only `KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER` is not another cache or toolchain wrapper: it observes actual native compile invocations for all five compiled runners and remains active when the artifact cache is disabled. Its usage and exact scope live in [`local-performance.md`](local-performance.md#debug-probes).

This is the test-runner artifact cache only. The kio BUILD cache (`out/.kio-cache/`, cleared by `kio cache clear`) is a separate, worktree-local cache — see [`local-ci.md`](local-ci.md) § Kio-semantic caches.

### sccache compiler wrapper

[sccache](https://github.com/mozilla/sccache) **v0.15.0** is provisioned by mise in the image (on `PATH` through mise shims); the image only makes the binary available, and enabling it for local Cargo remains opt-in. Once configured, repository entry points use the explicit `ci/infra/sccache.sh` adapter to register the scheduler's generic readiness hook only for commands that can use the wrapper. Semantic Kio compiler commands carry a generic private one-invocation skip proof from their harness proxy because typechecking and source emission cannot invoke the native test-runner wrapper; the facade consumes that proof before target launch. The native scheduler itself never recognizes the tool by name.

To enable it for Cargo builds and Rust-backend golden/test-runner compiles, set the wrapper environment once in your shell profile / a sourced env file:

```sh
export RUSTC_WRAPPER=sccache
export KIO_TEST_RUNNER_COMPILER_WRAPPER=sccache
```

> **⚠ When sccache is configured as a compiler wrapper, before any build or `ci/all.sh` export `SCCACHE_IDLE_TIMEOUT=0` and contact or start the daemon with `sccache --dist-status >/dev/null`.** Check the Cargo wrapper environment variables below, `KIO_TEST_RUNNER_COMPILER_WRAPPER`, and effective Cargo configuration; a build with no sccache wrapper needs no daemon. With the repository's pinned sccache, `--dist-status` contacts an existing daemon or starts one; `--show-stats` reports statistics and can succeed without a live daemon. If the first `rustc` instead spawns the daemon lazily inside admitted compiler work, the long-lived daemon can inherit and pin that compiler lease and **deadlock later work**. The fingerprint is a near-idle machine with the build/agent stalled and **no `rustc`/`cargo` running**.
>
> `SCCACHE_IDLE_TIMEOUT=0` is load-bearing: the default ~600s idle shutdown lets the daemon exit during any non-build gap, so the *next* build re-opens the lazy-spawn window the pre-start was meant to close. With the timeout disabled the daemon stays up for the whole session.
>
> Once a daemon is running, **do not stop or restart it mid-build.** A restart while a build is in flight makes that build respawn the daemon inside admitted work, re-triggering the exact deadlock above. Recover at most once, with no build in flight: run `sccache --stop-server`, export `SCCACHE_IDLE_TIMEOUT=0`, then contact or start it with `sccache --dist-status >/dev/null`; let no build start until that probe succeeds. This is a recurring trap — recognize it on sight rather than re-diagnosing. Full detail in the § Knobs note below and [`local-performance.md`](local-performance.md) § Fanning out building agents.

`RUSTC_WRAPPER` covers Cargo; `KIO_TEST_RUNNER_COMPILER_WRAPPER` covers compatible direct compiler invocations from the Rust test runner.

`sccache` does not require `CARGO_INCREMENTAL=0`. That variable disables Cargo's local incremental artifacts and lets sccache cache more first-party crate compilations. Choose the tradeoff for the work pattern described in [`local-performance.md`](local-performance.md#one-worktree) and [Multiple worktrees](local-performance.md#multiple-worktrees).

Prefer environment variables over creating new local Cargo config files. They cover both direct shell commands and the `ci/` scripts, are easy to inspect in `ci/all.sh` startup output, and avoid per-worktree config drift:

```sh
export RUSTC_WRAPPER=sccache
export KIO_TEST_RUNNER_COMPILER_WRAPPER=sccache
# Optional: choose a shared cache location before the sccache server starts.
export SCCACHE_DIR="<shared-worktree-cache-dir>"
# Optional: use only for a sccache-first profile aimed at fresh worktrees.
export CARGO_INCREMENTAL=0
```

Set the chosen setup once, not as a per-command prefix, so every `cargo`, `ci/cargo.sh`, and test-runner invocation inherits it. Knobs:

- `SCCACHE_DIR` relocates the cache; `SCCACHE_CACHE_SIZE` bounds it. Both are read by the sccache **server** when it starts — it runs as a per-machine daemon, so a value exported *after* the server is already running takes effect only after a restart. Restart the daemon only with **no build in flight** (§ sccache compiler wrapper warning) — never to pick up a knob mid-build. `sccache --show-stats` (read-only) reports available cache statistics; it is not a daemon-liveness probe.
- Default cache location is `~/.cache/sccache` (or `$XDG_CACHE_HOME/sccache`), ~10G.
- `SCCACHE_IDLE_TIMEOUT=0` keeps the daemon alive for the whole session. The default ~600s idle shutdown lets it exit during a non-build gap, re-opening the lazy-spawn deadlock window the explicit start was meant to close; set it whenever you pre-start the server.
- With sccache configured, export `SCCACHE_IDLE_TIMEOUT=0`, then contact or start the server with `sccache --dist-status >/dev/null` before `ci/all.sh`, rather than letting the first `rustc` spawn it lazily. Local CI entry points perform an early fail-fast check for wrappers configured explicitly through their environment and recheck at the last safe point after each newly acquired compiler admission. A nested compiler command carrying that same canonical inherited compiler lease reuses its already-established readiness; no result is persisted across independent leases. The session-level check remains required because those scripts cannot discover a wrapper hidden in Cargo configuration.

Inspect the effective local setup with:

```sh
printf 'RUSTC_WRAPPER=%s\n' "${RUSTC_WRAPPER-}"
printf 'RUSTC_WORKSPACE_WRAPPER=%s\n' "${RUSTC_WORKSPACE_WRAPPER-}"
printf 'CARGO_BUILD_RUSTC_WRAPPER=%s\n' "${CARGO_BUILD_RUSTC_WRAPPER-}"
printf 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER=%s\n' "${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER-}"
printf 'CARGO_INCREMENTAL=%s\n' "${CARGO_INCREMENTAL-}"
printf 'KIO_TEST_RUNNER_COMPILER_WRAPPER=%s\n' "${KIO_TEST_RUNNER_COMPILER_WRAPPER-}"
printf 'KIO_CI_SERIALIZE_CARGO=%s\n' "${KIO_CI_SERIALIZE_CARGO-}"
command -v sccache >/dev/null && sccache --show-stats
```

For each Cargo invocation, inspect `.cargo/config.toml` and `.cargo/config` in
its actual working directory and every ancestor, plus `config.toml` and
`config` in `CARGO_HOME` (`~/.cargo` by default), and any explicit `--config`
override. These files may set `build.rustc-wrapper`,
`build.rustc-workspace-wrapper`, `build.incremental`, or `[env]` values even
when the shell variables above are empty. The printout alone cannot establish
that no wrapper is configured. Prefer changing the shell environment for a
session; edit or add Cargo config only as a deliberate local policy.

Rust runtime cases compile generated host code through the fixed Rust test runner outside Cargo's normal `rustc-wrapper` path. For those, set `KIO_TEST_RUNNER_COMPILER_WRAPPER=sccache` alongside the Cargo wrapper when running the test runner locally; it wraps compatible direct compiler invocations from that runner. Emission `run.sh` files instead invoke their host toolchain by bare command name through the harness's compiler-admission proxy; the test-runner wrapper does not apply to them. GitHub CI still relies on `Swatinem/rust-cache` for Cargo artifacts, and Linux test-runner jobs additionally set `KIO_TEST_RUNNER_COMPILER_WRAPPER=sccache` when the mise-provisioned binary is present.

## tree-sitter WebAssembly builds

The `highlight-agreement` and `vscode-e2e` orchestrators compile the `tools/tree-sitter-kio` grammar to WebAssembly with `tree-sitter build --wasm`. On first use `tree-sitter` downloads a wasi-sdk toolchain (LLVM plus the `wasm32-wasi` sysroot, a few hundred MB) into `~/.cache/tree-sitter/wasi-sdk` and reuses it thereafter. The image provisions the `tree-sitter` CLI but does not pre-populate that per-machine cache.

On a fresh machine or after the cache is cleared, a full parallel gate (`ci/all.sh`) starts both wasm-building orchestrators against the cold cache at once; they race to download the same sdk into one directory, and a build that observes the half-written sysroot fails with `'stdlib.h' file not found`. Warm the cache once, serially, before the first parallel gate:

```sh
( cd tools/tree-sitter-kio && tree-sitter build --wasm -o kio-prewarm.wasm && rm -f kio-prewarm.wasm )
```

The single build completes the download; the concurrent orchestrators then reuse the cache and the race is gone. Broad-gate scheduling does not reliably avoid it — the two wasm tasks can still be scheduled in the same window — so pre-warming is the dependable fix.

## Scheduler-native Cargo serialization

Use `sh <repo-root>/ci/cargo.sh <cargo-args...>` for a top-level Cargo command
launched by an agent or operator from a tracked repository workspace, including
commands issued on their behalf by local-development and report scripts. It is
a transparent passthrough: it keeps the caller's working directory and forwards
every Cargo argument and environment variable. Its value is coordination: it
discovers the Git-common scheduler even outside `ci/all.sh` and takes the
generic compiler resource. Standalone Cargo deliberately takes no work slot,
so queued builds cannot block fresh corpus work. Set
`KIO_CI_SERIALIZE_CARGO=1` only when whole-Cargo serialization is useful; that
adds the scheduler's capacity-one `cargo` resource before compiler admission.

The wrapper neither locates a manifest nor changes directory. Invoke it from
the Cargo workspace that owns the command, or pass
`--manifest-path <workspace>/Cargo.toml` to the Cargo subcommand. For compiler
work, `<workspace>` is `<repo-root>/kio-rs`; `<repo-root>` itself has no
`Cargo.toml`.

The wrapper is intentionally a top-level entry point, not a recursive Cargo
shim. A subprocess launched by Cargo itself must use Cargo's supplied
executable: the parent can already hold scheduler resources and
reacquiring them would deadlock. A top-level repository
Cargo command reached from an already-scheduled `ci/run-tests.sh` worker may
still use `ci/cargo.sh`: the wrapper recognizes the inherited `work` lease and
takes only the missing compiler resource. Commands inside emitted or fixture
crates stay direct because those workspaces are test inputs rather than tracked
repository workspaces. Public instructions for hosts installing Kio or building
generated host projects are outside this local coordination contract. Apart
from those boundaries, bare Cargo in a tracked-workspace top-level command is
reserved for diagnosing or repairing the wrapper itself.

`ci/cargo.sh` discovers `<git-common-dir>/kio-ci-schedule` when the caller did
not inherit `KIO_CI_SCHEDULE_DIR`. The native scheduler owns all three resource
classes, and their only valid order is `work -> cargo -> compiler`. A
standalone invocation therefore takes `cargo -> compiler` when serialization
is enabled and only `compiler` otherwise. A caller already inside a scheduled
worker retains `work` first. No path acquires an earlier resource after a later
one.
One Cargo invocation consumes one compiler permit; the resource coordinates
Cargo with compiler-producing Kio commands and actual native compiler commands
but does not count or cap Cargo's individual `rustc` children.
Omitting `--compiler-jobs` uses paced, best-effort CPU/memory feedback within
live CPU and fixed-capacity limits. See
[Shared work, Cargo, and compiler admission](local-ci.md#shared-work-cargo-and-compiler-admission)
for the policy and its limits.
`--compiler-jobs=<N>` on `ci/all.sh`, `ci/run-tests.sh`, and corpus orchestrators
instead fixes that shared capacity. Sibling
worktrees have separate `target/` directories, while Cargo coordinates access
to the shared package cache under `CARGO_HOME`.

`KIO_CI_SCHEDULE=DISABLE` is the sole explicit scheduler bypass. It skips all
three resources (`work`, `cargo`, and `compiler`); it does not retain
serialization independently.
The internal `KIO_CI_SCHEDULE_HELD` and lease-descriptor inventory prevent
nested wrappers from reacquiring resources and are not user-facing bypasses.

The capacity-one Cargo resource is automatically shared by sibling worktrees
because its state lives below the Git common directory. There is no arbitrary
lock path to configure and no external lock utility. Enable it for launched
commands that should serialize whole Cargo invocations:

```sh
KIO_CI_SERIALIZE_CARGO=1 sh ci/watch-builds.sh
KIO_CI_SERIALIZE_CARGO=1 sh ci/all.sh SAMPLE_IMPL
```

This resource changes scheduling only; Cargo still runs and its fingerprints
remain the stale-artifact guard. A waiter consumes no compiler permit because
`cargo` is acquired first. `CARGO_BUILD_JOBS=1` instead serializes one Cargo
invocation's own codegen; it does not coordinate separate Cargo processes.

### Readiness hooks and process lifetime

Admission remains owned by the complete process tree on every supported host,
but completion is realized differently. Unix children inherit private lease
descriptors: the scheduled invocation waits only for the leader and returns its
status, while a surviving descendant keeps the resource occupied until its
last descriptor closes. A genuinely closed stdin stays closed. Windows children
are assigned to a kill-on-close Job Object before they begin running, and the
scheduler waits for that Job to drain after preserving the leader's status. A
closed stdin is mapped to `NUL`, giving the child EOF without an invalid native
handle. In both cases, a short-lived wrapper cannot free capacity while its
compiler remains active.

For resource-free whole-corpus supervision, a Unix child process group that
inherits the caller's controlling-terminal stdin receives foreground ownership
in its pre-exec hook and returns it after the complete group drains. This keeps
stdin a real terminal rather than replacing it with a byte relay. The transfer
is conditional on the supervisor still owning the terminal foreground and the
restore never overwrites a different live owner. Windows keeps the inherited
console handle while the native Job provides descendant ownership.

A long-lived compiler-cache daemon is the exception that must outlive the
admitted command without pinning it. The scheduler exposes one generic,
explicit post-admission readiness hook. On Unix the hook closes every validated
lease descriptor before `exec` and enters a distinct process group so its
descendants do not pin an enclosing corpus invocation's process group. On
Windows only the hook launches with Job breakaway semantics. Both paths wait
for the hook leader. `ci/infra/sccache.sh` is the named sccache adapter that
registers and implements this hook. The Rust scheduler never recognizes a tool
by basename, and another daemon-aware wrapper requires its own explicit adapter
and documentation.

Platform-neutral policy, queue, crash-transition, and readiness tests run on
every host. The same native scheduler self-test runs on Linux, macOS, and
Windows. On Windows, `scheduler_job_nests_inside_an_outer_job` creates a real
outer Job before exercising the scheduler-owned inner Job. Target-only checks
prove compilation but cannot prove advisory-lock, descriptor inheritance,
suspended-spawn/Job assignment, breakaway, or complete descendant-drain
behavior.
