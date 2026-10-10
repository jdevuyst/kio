#!/bin/sh
# Disposable hosted-runner adapter; the acceptance oracle remains unchanged.
set -eu
if [ "${GITHUB_ACTIONS-}" != true ] || [ "${RUNNER_ENVIRONMENT-}" != github-hosted ] ||
  [ "${RUNNER_OS-}" != Linux ] || [ "$(uname -m)" != x86_64 ]; then
    printf 'requires a disposable GitHub-hosted Linux x64 runner\n' >&2; exit 2;
fi
repo=$(pwd)
evidence="$RUNNER_TEMP/kio-container-evidence"
mkdir -p "$evidence"
df -h / > "$evidence/disk-before.log"
# Preserve mise/tools, and avoid daemon pruning or blanket hostedtoolcache removal.
sudo rm -rf /usr/share/dotnet /usr/local/lib/android /opt/ghc \
  /opt/hostedtoolcache/CodeQL /usr/share/swift
df -h / > "$evidence/disk-after.log"
available=$(df -Pk / | awk 'NR == 2 {print $4}')
[ "$available" -ge 52428800 ] || { printf 'acceptance requires 50 GiB free disk\n' >&2; exit 2; }
[ "$(getconf _NPROCESSORS_ONLN)" -ge 4 ] || { printf 'acceptance requires 4 host CPUs\n' >&2; exit 2; }
[ "$(awk '/MemTotal/ {print $2}' /proc/meminfo)" -ge 13631488 ] || {
  printf 'acceptance requires 13 GiB host memory for a 12g container\n' >&2; exit 2;
}
task=/home/kio-release-acceptance
[ ! -e "$task" ] || { printf 'acceptance workspace already exists\n' >&2; exit 2; }
account=$(getent passwd 1000 | cut -d: -f1)
if [ -z "$account" ]; then
  sudo useradd --uid 1000 --create-home kio-acceptance
  account=kio-acceptance
fi
# Match the published vscode UID on the host, never derive or rewrite the image.
sudo usermod -aG docker "$account"
group=$(id -gn "$account")
sudo install -d -o "$(id -un)" -g "$(id -gn)" "$task" "$task/home" "$task/tmp"
mise_bin=$(command -v mise)
mise_data=${MISE_DATA_DIR:-"$HOME/.local/share/mise"}
git clone --no-hardlinks "$repo" "$task/repo"
git -C "$task/repo" checkout --detach "$SOURCE_SHA"
export TMPDIR="$task/tmp"
for directory in tools/vscode-kio ci/infra/highlight-agreement-js; do
  (cd "$repo/$directory" && npm ci --no-fund --no-audit)
  cp -R "$repo/$directory/node_modules" "$task/repo/$directory/node_modules"
done
refresh=$(node ci/infra/release-container.cjs refresh)
mkdir "$task/editor"
curl --fail --location --proto '=https' --tlsv1.2 \
  https://update.code.visualstudio.com/latest/linux-x64/stable --output "$task/editor.tar.gz"
tar xzf "$task/editor.tar.gz" -C "$task/editor" --strip-components=1
commit=$(node -e 'const p=require(process.argv[1]); if(!/^[a-f0-9]{40}$/.test(p.commit)) throw Error("official editor commit missing"); process.stdout.write(p.commit)' "$task/editor/resources/app/product.json")
curl --fail --location --proto '=https' --tlsv1.2 \
  "https://update.code.visualstudio.com/commit:$commit/server-linux-x64/stable" --output "$task/server.tar.gz"
sudo chown -R "$account:$group" "$task"
as_owner() {
  sudo -u "$account" env HOME="$task/home" TMPDIR="$task/tmp" PATH="$PATH" \
    MISE_DATA_DIR="$mise_data" MISE_CACHE_DIR="$task/mise-cache" MISE_STATE_DIR="$task/mise-state" \
    MISE_CONFIG_DIR="$task/home/.config/mise" MISE_TRUSTED_CONFIG_PATHS="$task/repo" MISE_AUTO_INSTALL=0 \
    "$mise_bin" -C "$task/repo" -E optional exec "$@"
}
# Fail on pinned-tool accessibility before pulling the larger candidate image.
as_owner node -- node --version
as_owner npm:@devcontainers/cli -- devcontainer --version
status=0
as_owner node -- sh "$task/repo/ci/infra/devcontainer-acceptance.sh" \
  "ghcr.io/jdevuyst/kio-devcontainer@$IMAGE_DIGEST" "$refresh" "$task/server.tar.gz" \
  "$task/editor" "$task/result" 4 12g || status=$?
# Upload explicit evidence only, not private checkouts or container environments.
for phase in pull registry cli ownership checkout create tools server first-attach first-editor \
  restart reconnect reconnect-attach reconnect-editor refresh refresh-tools refreshed-attach \
  refreshed-editor injection; do
  if [ -f "$task/result/$phase.log" ]; then cp "$task/result/$phase.log" "$evidence/"; fi
done
for file in phases.jsonl exit-status resources-final-raw.log cleanup.log; do
  if [ -f "$task/result/$file" ]; then cp "$task/result/$file" "$evidence/"; fi
done
if [ "$status" -eq 0 ]; then
  node ci/infra/release-container.cjs receipt "$task/result/receipt.json" > "$evidence/receipt.json"
fi
df -h / > "$evidence/disk-final.log"
exit "$status"
