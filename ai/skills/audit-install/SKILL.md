---
name: audit-install
description: Verify the install story stays coherent — every path and command INSTALL.md names resolves (incl. inside code blocks, which markdown-link-check can't see), tools are mise-owned, install-dev-env mirrors INSTALL.md, install docs agree, required PATH dirs documented
allowed-tools: Read, Grep, Glob, Bash
---

# Install / setup coherence audit

`INSTALL.md` is the human-facing setup doc; [`install-dev-env`](../install-dev-env/SKILL.md) is its agent-run automation; both anchor on `mise.toml`, `mise.optional.toml`, `mise.lock`, `mise.optional.lock`, `ci/impl-toolchain.sh`, and the devcontainer. This audit keeps the set coherent. The shape mirrors [`audit-readme`](../audit-readme/SKILL.md) (top-level doc integrity) and [`audit-agents-md`](../audit-agents-md/SKILL.md) §2 / §4 (references resolve). The weekly markdown-link-check workflow already covers `INSTALL.md`'s *links*; this covers what it can't — the path / command references inside code blocks, plus the skill pairing.

Read `INSTALL.md`, `ai/skills/install-dev-env/SKILL.md`, and `docs/guides/install-from-source.md` before starting.

## 1. References resolve

Every file, path, and command `INSTALL.md` names must still exist:

```sh
for p in .devcontainer mise.toml mise.optional.toml mise.lock mise.optional.lock ci/all.sh ci/impl-toolchain.sh; do
  [ -e "$p" ] && echo "ok   $p" || echo "MISS $p"
done
```

- The paths above (`.devcontainer/`, `mise.toml`, `mise.optional.toml`, `mise.lock`, `mise.optional.lock`, `ci/all.sh`, `ci/impl-toolchain.sh`) all resolve.
- Commands in code blocks name real entrypoints: `sh ci/all.sh SAMPLE_IMPL`, `sh ci/impl-toolchain.sh ...`, `sh ci/impl-toolchain.sh core-mise-tools | xargs mise install --locked -y`, `sh ci/impl-toolchain.sh install-report-tools`, `cargo build` in `kio-rs/`.
- The Windows toolchain subset `INSTALL.md` lists (`node npm:typescript python go java rust ghcup`) must match real `mise.toml [tools]` entries — a tool renamed or dropped in `mise.toml` leaves this list stale.

```sh
awk '/^\[tools\]/{f=1;next} /^\[/{f=0} f && NF' mise.toml
```

## 2. Mise-owned tool coverage

Every non-OS tool that Kio installs or depends on must be installed through mise, named or categorised in `INSTALL.md`, and represented in `mise.toml` or `mise.optional.toml`. This includes tools installed by mise-managed package-manager backends: `cargo:*`, `npm:*`, and pipx-backed Python CLI entries are acceptable because the mise config and lock files own the name, version, and install entry point. Non-mise setup is limited to the mise bootstrap itself and OS/runtime libraries required by mise-managed tools. If a change adds non-mise repo tooling, report it as unsupported unless the provisioning policy in `ai/topics/local-tools.md` is revised in the same change.

- Cargo-backed report/audit CLIs (`cargo:cargo-fuzz`, `cargo:cargo-mutants`, `cargo:cargo-llvm-cov`, `cargo:cargo-audit`) are mise entries in `mise.optional.toml`, not direct `cargo install` Dockerfile steps. They are optional tools and should be reachable through `ci/impl-toolchain.sh report-mise-tools` and installable through `ci/impl-toolchain.sh install-report-tools`, not present in `core-mise-tools`.
- npm-backed CLIs (`npm:typescript`, `npm:markdownlint-cli2`) are mise entries, not direct `npm install -g` Dockerfile or CI steps.
- pipx-backed Python CLIs (`yamllint` today) are mise entries, not direct `pipx install` Dockerfile or CI steps.
- `wasm-pack` is a mise entry (`aqua:wasm-bindgen/wasm-pack` today), not a direct `cargo install` Dockerfile step.
- `ci/impl-toolchain.sh core-mise-tools` should be a subset of real `mise.toml [tools]` entries; `ci/impl-toolchain.sh report-mise-tools` should be a subset of real `mise.optional.toml [tools]` entries.
- `INSTALL.md` must explain that package-manager-backed mise tools are allowed only when the mise config and lock files own the version and install entry point.
- Cargo-backed tools must not be shadowed by stale binaries in `$CARGO_HOME/bin`; their resolved path should sit under `mise where <tool>`.

```sh
if find .devcontainer .github ci -type f \
  \( -name Dockerfile -o -name '*.sh' -o -name '*.yml' -o -name '*.yaml' \) \
  -exec grep -nE 'cargo install|npm install -g|pipx install' {} +; then
  echo 'unexpected direct package-manager tool install'
fi
root_tool_keys=$(
  awk '
    /^\[tools\]/{f=1; next}
    /^\[/{f=0}
    f && /=/ {
      key=$0
      sub(/[[:space:]]+=[[:space:]].*/, "", key)
      gsub(/^"|"$/, "", key)
      print key
    }
  ' mise.toml
)
optional_tool_keys=$(
  awk '
    /^\[tools\]/{f=1; next}
    /^\[/{f=0}
    f && /=/ {
      key=$0
      sub(/[[:space:]]+=[[:space:]].*/, "", key)
      gsub(/^"|"$/, "", key)
      print key
    }
  ' mise.optional.toml
)
missing_core=$(
  sh ci/impl-toolchain.sh core-mise-tools |
    while IFS= read -r tool; do
      printf '%s\n' "$root_tool_keys" | grep -Fxq "$tool" || printf '%s\n' "$tool"
    done
)
missing_report=$(
  sh ci/impl-toolchain.sh report-mise-tools |
    while IFS= read -r tool; do
      printf '%s\n' "$optional_tool_keys" | grep -Fxq "$tool" || printf '%s\n' "$tool"
    done
)
[ -z "$missing_core" ] || printf 'core mise tools not in mise.toml:\n%s\n' "$missing_core"
[ -z "$missing_report" ] || printf 'report mise tools not in mise.optional.toml:\n%s\n' "$missing_report"

for spec in cargo:cargo-fuzz cargo:cargo-mutants cargo:cargo-llvm-cov cargo:cargo-audit; do
  exe=${spec#cargo:}
  path=$(MISE_AUTO_INSTALL=0 MISE_EXEC_AUTO_INSTALL=0 mise -E optional which "$exe" 2>/dev/null || true)
  if [ -z "$path" ]; then
    printf 'cargo-backed tool not installed; shadow check skipped: %s\n' "$exe"
    continue
  fi
  root=$(MISE_AUTO_INSTALL=0 MISE_EXEC_AUTO_INSTALL=0 mise -E optional where "$spec" 2>/dev/null || true)
  if [ -z "$root" ]; then
    # `which` resolved a binary but mise has no install for the spec:
    # that IS the shadow condition (an unmanaged binary on PATH), and
    # without the guard the empty $root would make the case below
    # match any absolute path and silently pass.
    printf 'cargo-backed tool shadowed: %s resolves to %s but mise has no install for %s\n' "$exe" "$path" "$spec"
    continue
  fi
  case "$path" in
    "$root"/*) ;;
    *) printf 'cargo-backed tool shadowed: %s resolves to %s, expected under %s\n' "$exe" "$path" "$root" ;;
  esac
done
```

## 3. Backend-extra bootstrap coverage

`ci/impl-toolchain.sh` is the selector-to-tool mapping that Linux CI uses after the core devcontainer image starts. `INSTALL.md` must document the same local route for selected backend extras.

- `INSTALL.md` names `ci/impl-toolchain.sh install <impls>` for Linux backend extras and `ci/impl-toolchain.sh extra-mise-tools <impls> | xargs mise install --locked -y` for macOS backend extras.
- `INSTALL.md` says Linux system packages are installed through `mise bootstrap packages apply` with manager-qualified specs such as `apt:...`, not by direct user-facing `apt-get install` instructions.
- The install-dev-env skill mentions the same helper and does not instruct agents to install Swift/Haskell libraries manually.

```sh
sh ci/impl-toolchain.sh extra-system-packages kio@swift,kio@haskell
grep -q 'mise bootstrap packages' INSTALL.md || echo 'INSTALL.md: missing mise bootstrap package route'
grep -q 'ci/impl-toolchain.sh install' INSTALL.md || echo 'INSTALL.md: missing impl-toolchain install route'
grep -q 'ci/impl-toolchain.sh extra-mise-tools' INSTALL.md || echo 'INSTALL.md: missing macOS backend-extra route'
```

## 4. INSTALL.md ↔ install-dev-env in sync

The skill automates the steps the doc documents; they must agree on the *setup procedure*:

- Each core step in `INSTALL.md` (install mise; `mise trust` + `sh ci/impl-toolchain.sh core-mise-tools | xargs mise install --locked -y` on Linux/macOS; optional `ci/impl-toolchain.sh install <impls>` for backend extras; optional `install-report-tools` for deep report/audit tools; build + `ci/all.sh SAMPLE_IMPL`) has a matching step in `install-dev-env`, and vice versa.
- The commands match (same `mise install --locked`, same verify). A command changed in one but not the other is the finding.
- The build entry point is a deliberate scope difference, not drift: `INSTALL.md` is public host-user guidance and uses ordinary Cargo, while the agent-run `install-dev-env` skill runs the same build through `ci/cargo.sh` because it operates in a tracked repository workspace. Compare the working directory, Cargo subcommand, and arguments rather than requiring the wrapper prefix to match.
- The skill may *additionally* automate or optimize (detection, cache-warming) — extra automation isn't a divergence; a contradicting or missing *core* step is.

This is a judgment pass (like `audit-docs-drift`): flag divergence for review, don't mechanically diff prose.

## 5. INSTALL.md ↔ install-from-source.md consistency

The two install docs cross-reference each other. Confirm they don't contradict — the Cargo build invocation, the toolchain expectations, and the cross-links between them still hold.

The supported distribution and source-build instructions expose only the
public `kio` binary. Confirm that the release wrapper declares only that binary
and that public source-build commands select it explicitly (for example,
`cargo build --release --bin kio`). Internal compiler, corpus-oracle,
generator, and runner binaries remain repository or CI tools rather than
public installation workflows.

## 6. Sub-managed tool PATH is complete for a local checkout

`mise activate` exposes mise's shims, but some mise-managed tools are managers that install a compiler or runner elsewhere. The standing case is `ghcup` installing `ghc` into `~/.ghcup/bin`, which is *not* a shim (`mise which ghc` reports "not a mise bin"). Future tools may have the same shape. The `.devcontainer/Dockerfile` is the source of truth for the runtime PATH; `INSTALL.md` must not leave a local checkout unable to reach a pinned compiler.

- Inspect `mise.toml` for manager-shaped tools or postinstall hooks (`postinstall`, `ghcup`, future equivalents).
- For every sub-managed compiler dir required by those tools, the Dockerfile's `PATH` carries both the mise shims and that dir.
- For every such dir, `INSTALL.md` documents adding it for a local checkout. A sub-managed compiler dir present in the Dockerfile PATH but absent from `INSTALL.md` is the finding — a local user can't build or run that backend.
- Do not stop at `ghcup`: if a future mise-managed tool installs its real compiler outside mise shims, add that path to the Dockerfile and to `INSTALL.md`, and update this audit's examples if useful.

```sh
awk '/^\[tools\]/{f=1;next} /^\[/{f=0} f && /postinstall|ghcup/ { print }' mise.toml
grep -oE '[^:"]*\.ghcup/bin' .devcontainer/Dockerfile | head -1
grep -q '\.ghcup/bin' INSTALL.md || echo 'INSTALL.md: missing ~/.ghcup/bin PATH note'
```

Agent-shell wiring (`BASH_ENV` → mise shims + `~/.ghcup/bin`, for the Bash tool's non-interactive shells) is an agent-environment concern owned by [`ai/topics/local-tools.md`](../../topics/local-tools.md) § Toolchains on PATH, not `INSTALL.md`; this audit checks only the human/local story.

## How to report

Group findings into:

1. **Broken references** — a path / command in `INSTALL.md` that no longer resolves, or a Windows-subset tool absent from `mise.toml`.
2. **Mise tool coverage gaps** — a non-OS tool installed outside mise, a mise-owned tool missing from `INSTALL.md`, or a package-manager-backed/aqua CLI no longer represented in `mise.toml`.
3. **Backend-extra bootstrap gaps** — `INSTALL.md` or `install-dev-env` missing the `ci/impl-toolchain.sh` / `mise bootstrap packages` route.
4. **Skill ↔ doc drift** — a setup step present in one of `INSTALL.md` / `install-dev-env` but missing or contradicted in the other.
5. **Cross-doc contradictions** — `INSTALL.md` vs `docs/guides/install-from-source.md`.
6. **PATH gaps** — a sub-managed compiler dir (e.g. `~/.ghcup/bin`, or a future equivalent) in the devcontainer Dockerfile's PATH that `INSTALL.md` doesn't tell a local checkout to add.

For each finding, cite the file and line.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md) — fix the stale reference, or re-sync the skill and doc together, then re-run.
