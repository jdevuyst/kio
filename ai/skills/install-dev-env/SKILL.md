---
name: install-dev-env
description: Bring a fresh or bare checkout to a working Kio dev environment — bootstrap mise if absent, install the pinned toolchains, warm caches, verify with a smoke build. Idempotent agent automation of INSTALL.md
allowed-tools: Read, Grep, Glob, Bash, Edit, Write
---

# Install the dev environment (kio)

Provision a checkout so the toolchain-dependent skills and `ci/all.sh` can run. This is the agent-run automation of [`INSTALL.md`](../../../INSTALL.md) — the human doc and this skill are the same steps, kept in sync by [`audit-install`](../audit-install/SKILL.md). The devcontainer already does all of this (its Dockerfile installs `mise`; the lifecycle scripts warm the caches); the gap this fills is a **bare host** where `mise` isn't present — the case that blocks `upgrade-deps` and `ci/all.sh` from running at all.

## 1. Detect what's already there

```sh
command -v mise >/dev/null 2>&1 && echo "mise: present" || echo "mise: absent"
```

- `mise` present and `sh ci/impl-toolchain.sh core-mise-tools | xargs mise install --locked -y` a no-op → core tools are already provisioned; jump to **5. Verify** unless backend extras are needed.
- `mise` present but tools missing → skip to **3. Provision**.
- `mise` absent → **2. Bootstrap** (the bare-host case).

## 2. Bootstrap mise (bare host only)

Per [`INSTALL.md`](../../../INSTALL.md) § Install mise, OS-aware. A freshly-installed `mise` isn't on `PATH` until activated, so activate it in the current shell before step 3:

- **Linux**: `curl https://mise.run | sh`, then `eval "$(~/.local/bin/mise activate bash)"`, and append that activation to `~/.bashrc` for future shells.
- **macOS**: `brew install mise`, then `eval "$(mise activate bash)"`.
- **Windows**: `scoop install mise` + the PowerShell activation INSTALL.md lists.

## 3. Provision the pinned toolchains

```sh
mise trust mise.toml
sh ci/impl-toolchain.sh core-mise-tools | xargs mise install --locked -y
mise exec -- python3 --version
```

On macOS, use the same `core-mise-tools` command for the baseline local setup; `mise install --locked -y` is reserved for intentionally installing every pinned optional backend-extra and audit tool. On Windows install the portability subset INSTALL.md names (`node npm:typescript python go java rust ghcup`). The mise configs also own package-manager-backed CLIs (`npm:typescript`, `npm:pyright`, `npm:markdownlint-cli2`, pipx-backed `yamllint`, and the optional Cargo-backed tools from `sh ci/impl-toolchain.sh install-report-tools`) plus the `aqua:wasm-bindgen/wasm-pack` entry; do not reintroduce direct `cargo install`, `npm install -g`, or `pipx install` setup steps for them.

The Cargo-backed report/audit tools (`cargo-fuzz`, `cargo-mutants`, `cargo-llvm-cov`, `cargo-audit`) are deliberately outside the core devcontainer and baseline local setup because they can source-build. Install them only when a deep report/audit or dependency-upgrade pass needs them:

```sh
sh ci/impl-toolchain.sh install-report-tools
```

For local backend-extra checks, use the same helpers CI uses:

- **Linux**: `sh ci/impl-toolchain.sh install kio@swift,kio@haskell` — installs backend-extra mise tools and routes backend distro libraries through `mise bootstrap packages apply`.
- **macOS**: `sh ci/impl-toolchain.sh extra-mise-tools <impls> | xargs mise install --locked -y` — installs the backend-extra mise tools directly; there's no distro-package step to route.

## 4. Warm the caches (optional, recommended)

Reuse the devcontainer's provisioning rather than re-deriving it — both scripts are POSIX sh, guarded, and idempotent:

```sh
sh .devcontainer/on-create.sh        # cargo fetch per crate + npm ci per package
sh .devcontainer/update-content.sh   # cargo build --tests per crate
```

## 5. Verify — the failure is the point

```sh
export SCCACHE_IDLE_TIMEOUT=0
sccache --dist-status >/dev/null
( cd kio-rs && sh ../ci/cargo.sh build )
sh ci/all.sh SAMPLE_IMPL
```

The exported idle policy and contact-or-start probe are the canonical lifecycle
from [`INSTALL.md`](../../../INSTALL.md) § Build and check and AGENTS.md §
Universal rules ("Don't bypass or break a configured compiler cache").

A green run means the environment is provisioned end to end. A *failure* is what verifying is *for* — read its cause and route it (step 6).

## 6. Close provisioning gaps (don't document around them)

A gap the verify finds is a provisioning *bug* to fix, not a limitation to note. Scope it tightly to what this skill owns — **a tool the build/CI needs that isn't on PATH** (`command not found`, or a check that silently skips for a missing binary):

- Add the tool to `mise.toml` (the source of truth — prefer a mise registry name or an `aqua:` / `github:` backend over a system package), then `mise lock` + `mise install` + re-verify.
- For the Python backend runner, `mise.toml` pins Python. If `python3` / `python` is missing, repair mise activation or installation rather than falling back to an OS package.
- If the provisioning *procedure* changed (a new step, not just a new pinned tool), reflect it in [`INSTALL.md`](../../../INSTALL.md) **and in this skill**, kept in sync ([`audit-install`](../audit-install/SKILL.md) guards the pairing).

Beyond missing tools, **any setup friction feeds back into the owning document** (AGENTS.md § Universal rules — Tooling friction feeds back into the instructions): a documented command that fails as written, a step that turned out to be missing, an error whose diagnosis was nonobvious. Route by owner — the human setup story to [`INSTALL.md`](../../../INSTALL.md) (a public artifact: the full no-leak bar applies), agent-shell and toolchain mechanics to [`ai/topics/local-tools.md`](../../topics/local-tools.md) (§ Toolchains on PATH, § Compiler cache), container provisioning to the `.devcontainer/` scripts, and this skill wherever its own steps said otherwise. Gate every addition on the portability filter in [`ai/topics/no-leak.md`](../../topics/no-leak.md): write only what holds on a fresh clone on any machine, phrased from symptoms ("if X fails with Y, check Z"); a problem specific to the current machine's state is a report item (step 7), not documentation.

A failure from a **code bug, a golden mismatch, or a missing network / service is *not* a provisioning gap** — report it (step 7), never paper over it by installing tools. The question is always "is the *environment* missing something," not "make `ci/all.sh` green by any means."

## 7. Report

Summarize what was already present, what got bootstrapped or installed, what provisioning gaps were closed (and the `mise.toml` / INSTALL.md / skill updates that closed them), any failure that *wasn't* a provisioning gap, and any machine-specific friction that failed the portability gate — that belongs here, not in the docs. If `mise` hit a GitHub API rate limit resolving release metadata, the escape hatch (per INSTALL.md) is `gh auth login` or a `MISE_GITHUB_TOKEN`.
