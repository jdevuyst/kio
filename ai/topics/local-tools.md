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

`.devcontainer/` is the canonical core build and test environment for Kio. The `Dockerfile` pins mise and installs the core toolchain from the repository root [`mise.toml`](../../mise.toml) / [`mise.lock`](../../mise.lock), while the same root mise files pin backend-extra toolchains that CI installs on demand. Optional Cargo-backed audit/report tools are pinned in [`mise.optional.toml`](../../mise.optional.toml) so they stay out of the default active mise environment.

Three audiences consume the same image:

- **Local VS Code** — "Reopen in Container" against `.devcontainer/devcontainer.json`.
- **GitHub Codespaces** — opens a prebuilt environment when the prebuild snapshot is current; the prebuild trigger lives in the Codespaces UI, with the source-of-truth values mirrored in [`.devcontainer/README.md`](../../.devcontainer/README.md).
- **Pages build** — [`.github/workflows/pages.yml`](../../.github/workflows/pages.yml) builds the site inside `ghcr.io/jdevuyst/kio-devcontainer:latest`, published on a monthly schedule by [`.github/workflows/devcontainer-publish.yml`](../../.github/workflows/devcontainer-publish.yml). The Linux test gate in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) instead installs core tools and the complete eight-backend sampling pool directly on its hosted runner through `ci/impl-toolchain.sh`. macOS / Windows portability jobs run `jdx/mise-action` against the same root mise files for every tool with a proven runner route.

Tool-version bumps land in `mise.toml`, `mise.optional.toml`, `mise.lock`, and `mise.optional.lock`, and the Dockerfile changes only when the mise version, core tool set, system packages, or an escape-hatch install changes. The published image picks up the core tools from the root mise files at build time; hosted Linux, macOS and Windows test jobs install their core and selected backend tools directly from those pins. Dependency and tool-version *upgrades* — reason-driven (security / end-of-life / deprecation), not "a newer version exists" — are driven by the [`upgrade-deps`](../skills/upgrade-deps/SKILL.md) skill, not an automated bot.

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

Start every language runtime, compiler, package manager, runner tool, linter, formatter, and repo helper CLI from `mise.toml`. Kio does not support repo tooling that cannot be installed through mise. If a future tool genuinely requires a different route, revise this policy in the same change before adding the tool.

“Through mise” includes mise tool specs that deliberately delegate to a package manager installed by mise: `cargo:*` for Cargo-backed CLIs, `npm:*` for npm-backed CLIs, and pipx-backed Python CLIs such as `yamllint`. Those tools are allowed because the tool name, version, and install entry point are declared in the mise config and lock files. Do not add direct `cargo install`, `npm install -g`, or `pipx install` steps in the Dockerfile, CI, or setup docs.

Prefer mise core tools and registry shorthands first, then explicit binary-oriented backends (`http:`, `github:`, `aqua:`, `conda:`, `pkgx:`) before writing install glue. Toolchain-shaped targets use concrete tools rather than invented language names: `dotnet` for C# / F#, `ghcup` / `ghc` / `cabal` for Haskell-family work, `clang` / `cmake` / `ninja` for C/C++, and so on. When a backend-extra toolchain needs distro shared libraries, route those through `mise bootstrap packages apply` with explicit manager-qualified specs such as `apt:libgmp-dev`; that is still a system package install, but mise remains the orchestration entry point.

Source builds are visible cost, not the default. A source-building path such as `ruby-build`, `kerl`, opam compiler builds, or a Cargo plugin compile needs an explicit reason and should be represented as a mise entry or a clearly named mise postinstall hook so the cost is reviewable. The optional Cargo-backed report/audit tools use mise's `cargo:` backend; they may still compile from source, but `mise.optional.toml` owns their versions and install entry point. Because they are opt-in tools, `ci/impl-toolchain.sh core-mise-tools`, ordinary `mise exec`, and the core devcontainer image omit them; install them on demand with `sh ci/impl-toolchain.sh install-report-tools`.

Python is pinned through mise. The devcontainer includes it because
`yamllint` is installed through mise's `pipx:yamllint` backend, so
`kio@python` is part of the core implementation pool at no
additional image cost. The Python backend contract is still a Python
3.10+ floor: the runner command is `python3` on Linux/macOS and
`python` on Windows, and those commands should resolve through mise in
the devcontainer and in local checkouts.

When a tool needs GitHub release metadata, use the locked route where possible. `mise.lock` records URLs/checksums for the common Linux/macOS/Windows platforms and `jdx/mise-action` passes `${{ github.token }}` by default. Docker builds do not inherit that token automatically; if `mise install` needs GitHub API access, pass it as a BuildKit secret and expose it only for that `RUN` as `MISE_GITHUB_TOKEN`.

If a hosted-runner platform lacks a no-source-build binary route for a backend toolchain, skip that platform/tool cell explicitly and narrow the test impl list for that runner. This is a runner-tooling gap, not evidence that the backend is portable on that platform. Try mise backends, including `http:` / `github:` direct-binary routes, before skipping; do not use `continue-on-error` or a source build to make the matrix look green.

Homebrew, Scoop, Chocolatey, and native OS package managers are not repo-tooling routes. Use them for bootstrapping mise and for OS/runtime libraries required by mise-managed tools; on Linux backend-extra libraries should go through `mise bootstrap packages apply` where supported. Do not use them to install a linter, compiler, runner, formatter, or helper CLI unless this policy is deliberately revised in the same change.

## Toolchains on PATH: mise shims, ghcup, and agent shells

`mise activate` (equivalently, the mise shims directory) puts the pinned tools on PATH — with one exception: `ghc`. `mise.toml` pins `ghcup` (the Haskell toolchain *manager*), which installs `ghc` itself into `~/.ghcup/bin`, *outside* mise's shims, so `mise which ghc` returns "not a mise bin". The canonical PATH therefore carries **both** the mise shims **and** `~/.ghcup/bin`; the devcontainer `Dockerfile` is the source of truth and sets exactly this (`…/share/mise/shims:…/.ghcup/bin:…`). `go`, `java` / `javac`, `swift`, `node`, `rg`, and the rest are native mise tools and ride the shims with no extra step.

Mise trust is path-scoped. In each fresh checkout or sibling worktree, run `mise trust` at the repository root before using `mise exec`, `mise install`, or a tool provided by mise; otherwise mise rejects the local `mise.toml` even when another worktree for the same repository was already trusted.

**Agents (the Bash tool) need extra wiring beyond `INSTALL.md`'s `.bashrc` line.** The Bash tool spawns a *non-login, non-interactive* shell, which sources only `$BASH_ENV` — not `~/.bashrc` (it returns early when non-interactive) and not `~/.profile` (login-only). So a `mise activate` line in `~/.bashrc` (what `INSTALL.md` sets up for interactive users) never reaches an agent's tool shells: `go` / `swift` / `ghc` are absent and even `node` falls back to the system copy. Point `BASH_ENV` at a snippet that prepends the toolchain dirs — in whatever mechanism the agent harness provides for env vars; for Claude Code, for example, `settings.json`:

```jsonc
// settings.json (Claude Code)
"env": { "BASH_ENV": "/absolute/path/to/bash-env.sh" }
```

```sh
# bash-env.sh — sourced by every non-interactive Bash tool shell
case ":$PATH:" in
  *":$HOME/.local/share/mise/shims:"*) ;;
  *) export PATH="$HOME/.local/share/mise/shims:$HOME/.ghcup/bin:$PATH" ;;
esac
```

**Diagnostic — a wholesale runner failure is a PATH gap, not a broken runner.** If every `go` / `java` / `swift` / `haskell` golden fails the same way (empty output, exit 1), that is almost always this PATH gap. Verify the compiler resolves — `go version`, `javac -version`, `java -version`, `swift --version`, `ghc --numeric-version` — *before* concluding anything about the runner. Calling a backend runner "known broken" on the strength of a missing toolchain hides a real, fixable environment fault, which `AGENTS.md` § Universal rules — "Bugs surface; never hide them" — forbids.

## Compiler cache

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

> **⚠ Before any build or `ci/all.sh`, export `SCCACHE_IDLE_TIMEOUT=0` and contact or start the daemon with `sccache --dist-status >/dev/null`.** With the repository's pinned sccache, `--dist-status` contacts an existing daemon or starts one; `--show-stats` reports statistics and can succeed without a live daemon. If the first `rustc` instead spawns the daemon lazily inside admitted compiler work, the long-lived daemon can inherit and pin that compiler lease and **deadlock later work**. The fingerprint is a near-idle machine with the build/agent stalled and **no `rustc`/`cargo` running**.
>
> `SCCACHE_IDLE_TIMEOUT=0` is load-bearing: the default ~600s idle shutdown lets the daemon exit during any non-build gap, so the *next* build re-opens the lazy-spawn window the pre-start was meant to close. With the timeout disabled the daemon stays up for the whole session.
>
> Once a daemon is running, **do not stop or restart it mid-build.** A restart while a build is in flight makes that build respawn the daemon inside admitted work, re-triggering the exact deadlock above. Recover at most once, with no build in flight: run `sccache --stop-server`, export `SCCACHE_IDLE_TIMEOUT=0`, then contact or start it with `sccache --dist-status >/dev/null`; let no build start until that probe succeeds. This is a recurring trap — recognize it on sight rather than re-diagnosing. Full detail in the § Knobs note below and [`local-performance.md`](local-performance.md) § Fanning out building agents.

`RUSTC_WRAPPER` covers Cargo; `KIO_TEST_RUNNER_COMPILER_WRAPPER` covers compatible direct compiler invocations from the Rust test runner.

`sccache` does not require `CARGO_INCREMENTAL=0`. Choose Cargo incremental mode based on the work pattern:

- For repeated edits and repeated `ci/all.sh` runs in one warm worktree, leave Cargo incremental compilation at its default unless measurement says otherwise. The worktree-local `target/` directory is usually the main accelerator, and incremental rebuilds can beat sccache hits for first-party crates.
- For fresh/disposable worktrees or cross-worktree reuse, set `CARGO_INCREMENTAL=0` when using sccache. That lets sccache cache more first-party crate compilations and share them across worktrees, at the cost of giving up Cargo's local incremental artifacts.

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
- Export `SCCACHE_IDLE_TIMEOUT=0`, then contact or start the server with `sccache --dist-status >/dev/null` before `ci/all.sh`, rather than letting the first `rustc` spawn it lazily. Local CI entry points perform an early fail-fast check for wrappers configured explicitly through their environment and recheck at the last safe point after each newly acquired compiler admission. A nested compiler command carrying that same canonical inherited compiler lease reuses its already-established readiness; no result is persisted across independent leases. The session-level check remains required because those scripts cannot discover a wrapper hidden in Cargo configuration.

Inspect the effective local setup with:

```sh
printf 'RUSTC_WRAPPER=%s\n' "${RUSTC_WRAPPER-}"
printf 'CARGO_INCREMENTAL=%s\n' "${CARGO_INCREMENTAL-}"
printf 'KIO_TEST_RUNNER_COMPILER_WRAPPER=%s\n' "${KIO_TEST_RUNNER_COMPILER_WRAPPER-}"
printf 'KIO_CI_SERIALIZE_CARGO=%s\n' "${KIO_CI_SERIALIZE_CARGO-}"
command -v sccache >/dev/null && sccache --show-stats
for f in .cargo/config.toml .cargo/config "${CARGO_HOME:-$HOME/.cargo}/config.toml" "${CARGO_HOME:-$HOME/.cargo}/config"; do
  [ -f "$f" ] && printf 'Cargo config present: %s\n' "$f"
done
```

If a Cargo config file is present, inspect it before drawing conclusions about cache behavior: it can set `build.rustc-wrapper`, `build.incremental`, or `[env]` values such as `CARGO_INCREMENTAL` and `SCCACHE_DIR` even when the shell environment is empty. Prefer changing the shell environment for a session; edit or add Cargo config only as a deliberate local policy.

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
