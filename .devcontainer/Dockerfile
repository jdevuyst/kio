# syntax=docker/dockerfile:1.7
#
# Kio dev container image.
#
# The image uses the same repository provisioning owner and native mise
# configuration as local development and hosted CI.
#
# Scope: linux/amd64 and linux/arm64, matching the publish workflow's
# multi-arch build.
#
# Layer ordering goes most-cacheable first (apt) and most-volatile
# last (specific pinned tool versions) so an unrelated tool bump
# invalidates the smallest tail of layers.

FROM mcr.microsoft.com/devcontainers/base:ubuntu-24.04 AS core

# Microsoft base ships: non-root `vscode` user with passwordless sudo,
# git, curl, gnupg, ca-certificates, build-essential, locale wiring,
# and the dev-container lifecycle entrypoint. We add only Kio-specific
# tooling below.

# ----- git safe.directory ---------------------------------------------
#
# CI jobs run the container as root (see trailing USER directive); the
# workspace mounted at `/__w/kio/kio` is owned by the runner host UID,
# not root. Modern git refuses to operate on a repo whose top-level
# dir is owned by a different user with "dubious ownership in
# repository". `safe.directory '*'` tells git to trust any directory
# at the system-config level — safe enough inside a build container,
# and unblocks tools like `codeowners-validator` that call git
# directly without actions/checkout's `--safe-directory` plumbing.

RUN git config --system --add safe.directory '*'

# ----- apt packages ---------------------------------------------------
#
# - xvfb: replace the `tests / vscode-e2e` workflow step
#   (`apt-get install xvfb`); @vscode/test-electron needs a virtual
#   display on headless Linux. Python and optional Cargo-backed tools
#   are installed through mise when selected, not through apt.

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        xvfb \
    && rm -rf /var/lib/apt/lists/*

# ----- VS Code Electron and Firefox runtime libraries -----------------
#
# `@vscode/test-electron` (run by `ci/checks/orchestrators/vscode-e2e.sh`) downloads
# the upstream VS Code binary into the workspace and exec's it inside
# Xvfb. VS Code is an Electron app and links against the GUI stack
# below; the minimal devcontainer base ships almost none of these,
# so the test bin dies at startup with
# `libglib-2.0.so.0: cannot open shared object file`.
#
# Package list mirrors the `Depends:` of the official VS Code .deb
# (so installing the .deb on a clean Ubuntu would have pulled the
# same set). Pinning is to whatever noble ships — these are GUI
# libraries whose ABI is governed by Ubuntu's stability promises,
# not something we want to track in our own version-mirror list.

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        libglib2.0-0t64 \
        libnss3 \
        libnspr4 \
        libgtk-3-0t64 \
        libgbm1 \
        libdrm2 \
        libxkbcommon0 \
        libxkbfile1 \
        libasound2t64 \
        libatk1.0-0t64 \
        libatk-bridge2.0-0t64 \
        libcups2t64 \
        libpango-1.0-0 \
        libcairo2 \
        libsecret-1-0 \
        libxss1 \
        libxshmfence1 \
        libxcb-dri3-0 \
        libxcomposite1 \
        libxdamage1 \
        libxext6 \
        libxfixes3 \
        libxrandr2 \
        libdbus-1-3 \
        libdbus-glib-1-2 \
        libxtst6 \
        fonts-liberation \
    && rm -rf /var/lib/apt/lists/*

# ----- mise-managed toolchains ---------------------------------------
#
# The image installs only core development tools. Target-language storage
# is separate from general helpers, and caller Cargo configuration remains
# in CARGO_HOME rather than the provisioning manager's private Cargo home.
#
# The build-push workflow passes the GitHub token as a BuildKit secret
# so GitHub-backed release metadata can authenticate without baking a
# token into an image layer.

ENV KIO_TOOLCHAINS_ROOT=/opt/kio-toolchain/.toolchains \
    KIO_GENERAL_MISE_DATA_DIR=/usr/local/share/mise \
    MISE_DATA_DIR=/usr/local/share/mise \
    CARGO_HOME=/usr/local/cargo \
    BASH_ENV=/opt/kio-toolchain/activate.sh

WORKDIR /opt/kio-toolchain
COPY mise*.toml mise*.lock ./
COPY ci/impl-toolchain.sh ci/impl-toolchain.sh
COPY tools/toolchains tools/toolchains
COPY .devcontainer/activate.sh activate.sh
RUN --mount=type=secret,id=github_token,required=false \
    set -eu; \
    if [ -f /run/secrets/github_token ]; then \
        export MISE_GITHUB_TOKEN="$(cat /run/secrets/github_token)"; \
    fi; \
    sh ci/impl-toolchain.sh install-core; \
    sh ci/impl-toolchain.sh toolchain-env js,ts,python,rust --core > /opt/kio-toolchain/environment.sh; \
    mkdir -p "$CARGO_HOME"; \
    chmod -R a+rwX "$KIO_TOOLCHAINS_ROOT" /usr/local/share/mise "$CARGO_HOME"

# ----- interactive terminal guidance ---------------------------------
#
# VS Code / Codespaces terminals start after the workspace is mounted,
# so the sourced helper can discover `ci/impl-toolchain.sh` from the
# checkout and print the current impl selector list without duplicating
# it here. It is interactive-shell-only and self-suppresses after the
# first terminal for a given container user.

COPY .devcontainer/terminal-help.sh /usr/local/share/kio-devcontainer/terminal-help.sh
RUN set -eu; \
    printf '%s\n' \
        '. /opt/kio-toolchain/activate.sh' \
        '[ -r /usr/local/share/kio-devcontainer/terminal-help.sh ] && . /usr/local/share/kio-devcontainer/terminal-help.sh' \
        > /etc/profile.d/kio-backend-extras.sh; \
    printf '\n%s\n%s\n' \
        '. /opt/kio-toolchain/activate.sh' \
        '[ -r /usr/local/share/kio-devcontainer/terminal-help.sh ] && . /usr/local/share/kio-devcontainer/terminal-help.sh' \
        >> /etc/bash.bashrc; \
    if [ -d /etc/zsh ]; then \
        printf '\n%s\n%s\n' \
            '. /opt/kio-toolchain/activate.sh' \
            '[ -r /usr/local/share/kio-devcontainer/terminal-help.sh ] && . /usr/local/share/kio-devcontainer/terminal-help.sh' \
            >> /etc/zsh/zshrc; \
    fi

# ----- vscode-kio extension (.vsix) -----------------------------------
#
# The publish workflow at `.github/workflows/devcontainer-publish.yml`
# builds the .vsix on its GHA runner (where vsce works in seconds —
# unlike inside Codespaces, where vsce v3's `package` step hangs)
# and copies it into the docker build context as `.devcontainer/kio.vsix`.
# The Codespaces / local-VS-Code lifecycle's `post-attach.sh` installs
# from `/opt/kio-extension/kio.vsix` rather than rebuilding the
# extension from source.

FROM core AS development
COPY .devcontainer/kio.vsix /opt/kio-extension/kio.vsix

# Image default user is root (no trailing USER directive). Two
# consumers, two contexts:
#
#   - Dev containers (Codespaces / local "Reopen in Container") —
#     the dev container CLI honours `remoteUser: vscode` in
#     `.devcontainer/devcontainer.json` and attaches as the vscode
#     user. The Microsoft base image already creates that user with
#     passwordless sudo, so interactive work runs as vscode without
#     a Dockerfile USER directive.
#
#   - CI jobs (`.github/workflows/ci.yml`'s `container:` directive)
#     — no devcontainer.json applies, so the image default user is
#     used. Running as root lets `actions/checkout` and the rest of
#     the runner tooling write to GitHub's `/__w/_temp/` mounts
#     (owned by the runner host UID, not 1000); a trailing
#     `USER vscode` directive made `actions/checkout` fail with
#     `EACCES: permission denied`.
#
# An explicit trailing `USER root` makes this unambiguous to both
# BuildKit (cache-key includes USER directives) and to a reader.

USER root
