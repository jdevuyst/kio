---
name: audit-install
description: Audit checkout setup coherence, exact toolchain ownership and activation, supported platform sets, INSTALL.md/install-dev-env pairing, and distributed install instructions
allowed-tools: Read, Grep, Glob, Bash
---

# Install and setup coherence audit

Read [INSTALL.md](../../../INSTALL.md),
[install-dev-env](../install-dev-env/SKILL.md),
[local-tools.md](../../topics/local-tools.md), and
[install-from-source.md](../../../docs/guides/install-from-source.md).
Follow actual setup callers through `ci/impl-toolchain.sh` and
`tools/toolchains/`; prose agreement alone does not establish correctness.
This audit is read-only: do not install tools or mutate account configuration
unless the user separately requests setup or validation that requires it.

## 1. References and command contracts

Check every setup path/link and code-block command against the checkout.
Include root/optional mise configurations and locks, delegated manifests,
`ci/install-target-language-toolchains.sh`, container lifecycle scripts, and
native CI jobs. Confirm referenced environment IDs exist with matching
configuration/lock pairs. Exercise non-mutating help/list/plan operations where
prerequisites are available; identify unavailable prerequisites rather than
calling unexecuted commands verified.

```sh
sh ci/install-target-language-toolchains.sh
sh ci/install-target-language-toolchains.sh --list
sh ci/impl-toolchain.sh supported-languages
sh ci/impl-toolchain.sh portability-languages
sh ci/impl-toolchain.sh toolchain-environments
```

The no-argument facade displays concise choices without installation. Help/list
work before bootstrap. Plan inspection requires the pinned mise; the explicit
`toolchain-bootstrap` operation installs only that verified manager. A plan must
not silently download or install target tools. Explicit
language requests, supported-set, portability-set, and exact-environment selection
must agree between help, implementation, INSTALL.md, and install-dev-env.

## 2. Version and mechanism ownership

Trace exact tool requests to native mise config/lock files or explicit delegated
manifests, not a duplicated workflow/script version table. Inspect root and
`mise.<environment>.toml` overlays with their locks, `tools/toolchains/mise.version`
and checksums, `rustup.toml`, and `ghcup.toml`. Check both manager and child pins,
verified download identities, and required compiler options/components/targets.
A manager pin alone does not identify its compiler. Installation receipts are
observations, never editable pin authority.
Assess Swift's native archive/signature route against INSTALL.md's exception;
its required Linux signature check is not a checked-in archive digest.

General helper CLIs remain mise-owned. Package-manager-backed mise entries
(`cargo:`, `npm:`, pipx) are legitimate; direct global package-manager installs
in setup callers are findings unless part of an explicitly owned manager route.
Optional Cargo-backed report tools remain in the optional configuration,
reachable through `install-report-tools`, and absent from ordinary core setup.
Check inventory outputs against real mise entries; an inventory is not itself
an installer. Discover new manifest kinds rather than assuming only GHCup and
Rustup can delegate.

Dependency discovery and lock maintenance use the pinned native mise executable
against tracked online checkout configuration, outside verified execution's
offline snapshot. Check that maintenance instructions update the real config/lock
owners rather than a copied snapshot and work without account mise activation.

For installed package-manager-backed helpers, compare the selected executable
origin with its exact mise installation. Stale binaries in `CARGO_HOME/bin` or
another account directory must not shadow a pinned optional report/audit tool.

Inspect `install-core`: Linux/macOS shared Node.js/TypeScript/Python/Rust target
installations serve core development, while general helpers stay in ordinary
mise storage; Windows retains its Rust/compiler route. The human facade remains
thin; dependency closure, setup, verification, and execution have one shared
owner. Source builds and native SDK prerequisites are explicit.

## 3. Project storage, activation, and nonmutation

Trace mise data/cache/state/configuration roots and Rustup/Cargo/GHCup child
roots to `.toolchains/` or an explicitly selected absolute `KIO_TOOLCHAINS_ROOT`.
General helper storage stays separate. Check setup-writer coordination and
idle-store cleanup guidance. No `HOME` replacement, account-default selection,
shell-profile mutation, or discarded Cargo configuration/credentials/compiler
cache may manufacture isolation. Follow actual child-manager invocations;
putting a binary below `.toolchains/` alone proves none of these properties.

Inspect `toolchain-exec`, shell exports, and GitHub exports. Setup cannot alter
its parent's environment. Each process receives its own selection, including
PATH, manager homes, exact environment, native SDK settings, and nested tool
lookups. Confirm callers preserve cwd and use `--core` where general helpers
are required. Two installed endpoints must not share a mutable selection file
that makes concurrent jobs change each other's tools.

Execution verifies versions and origins before launching, rejects missing or
mismatched tools, and disables install-on-execution. General mise activation or
an account `~/.ghcup/bin` directory cannot substitute for owned child selection.
An ambient executable with the expected spelling is insufficient evidence.
With existing installations, non-mutating verification can provide local evidence:

```sh
sh ci/impl-toolchain.sh toolchain-verify rust
sh ci/impl-toolchain.sh toolchain-exec rust -- rustc --version
```

Use existing focused provisioning self-tests/receipts for configuration preservation,
cross-directory and concurrent selection, native paths, installation reuse, and
namespace/options handling. Separate source inspection from actual platform
execution; an audit does not silently commission a cold multi-platform install.

## 4. Platform sets and setup callers

Compare supported and portability definitions with local instructions, GitHub
jobs, and container setup. Linux admits all eight current languages; macOS
supports JS/TS/Rust/Swift, Windows Rust. Native requests outside those sets,
including implicit core dependencies such as Python on macOS, warn upfront that
setup is best-effort; set flags exclude that remainder.
The smaller portability option remains available on Linux.

Native setup and GitHub use the same compiler version authority and route.
Windows requires Git Bash/MSYS path conversion; macOS Swift requires its SDK
resolution. Explicit native SDK/image prerequisites do not authorize arbitrary
ambient compiler fallback. System libraries route through mise bootstrap with
manager-qualified specs and visible privilege requirements.

Full Linux sampling setup installs every configured backend, not only whichever
tools happen to be on PATH. A smaller native job does not imply the complete
POSIX integration suite is supported. Test coverage must not contract silently
because provisioning failed.

Trace ordinary CI restore/verify/save ordering: the cache key includes platform,
exact config/locks, provisioning recipes, selected environments/tools and store-root
compatibility. Include child stores, verify cache hits, and save after all requested
setup succeeds. Release preparation uses the same routes with fresh install/download
roots and tiny native witnesses; cached success is not cold-install evidence.
Inspect the release owner's outage handling without weakening existing gates.

## 5. Human and agent guidance agree

Compare INSTALL.md and install-dev-env procedures by outcome: bootstrap, core
setup, explicit language/set/environment choices, optional report tools,
activation, cache warming, and scoped verification. Commands name the same
operations. Detection and optional automation are allowed additions; a missing
or contradictory core step is a finding. Build commands in tracked workspaces
use `ci/cargo.sh`; public instructions for installed users or emitted projects
use ordinary Cargo. Compare cwd, subcommand, and arguments across that boundary.

## 6. Distributed source installs and offline manual

INSTALL.md and install-from-source.md cross-links, requirements, and source-build
commands agree about their distinct full-development and minimal-install scopes.
Distribution and public source-build routes expose only the `kio` binary;
internal compiler, oracle, generator, and runner binaries remain repository tools.

Every distributed route carries the complete offline manual. Inspect Git-free
packaged-crate builds and unrelated-directory installed-binary access. Require
actual native Windows tar.exe, macOS, and Linux extraction evidence, including
published pipe/named-file forms, byte parity, and cmd.exe/PowerShell >=7.4 transfer.
Check conventional alternatives without ignore-zeros as well as the default format.
Record versions/commands; upstream manuals or Linux bsdtar do not prove native
behavior. [audit-bundled-docs](../audit-bundled-docs/SKILL.md) owns detailed
inventory, freshness, and binary-size checks.

## Report

Report findings with file/line and concrete failed invariant, grouped by broken
references/commands, pin or mechanism ownership, storage/activation/nonmutation,
platform/cache integration, guidance drift, and distributed-install consistency.
State which conclusions came from source, existing receipts, or executed checks,
and identify missing platform evidence without treating it as a pass.

Default: report only. With a fix directive follow
[audit-fix-mode.md](../../topics/audit-fix-mode.md), re-sync paired owners, and
rerun affected checks.
