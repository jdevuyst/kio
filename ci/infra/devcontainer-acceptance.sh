#!/bin/sh
# Release-only published-image acceptance; deliberately outside check discovery.
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo=$(CDPATH='' cd -- "$script_dir/../.." && pwd)
driver="$script_dir/devcontainer-acceptance/driver.cjs"

usage() {
  printf '%s\n' 'Usage: sh ci/infra/devcontainer-acceptance.sh IMAGE@sha256:DIGEST REFRESH_COMMIT SERVER_ARCHIVE EDITOR_DIRECTORY OUTPUT_DIRECTORY CPUS MEMORY' \
    'Requires a native Linux Docker host, cached official VS Code Server/Electron,' \
    'and the optional mise devcontainer CLI. OUTPUT_DIRECTORY must not exist.' \
    'Runs cold lifecycle hooks; retains logs and private checkout on failure.' \
    'Selftests: node ci/infra/devcontainer-acceptance/driver.cjs selftest'
}
case "${1-}" in -h|--help) usage; exit 0 ;; esac
[ "$#" -eq 7 ] || { usage >&2; exit 2; }
[ "$(uname -s)" = Linux ] || { printf 'acceptance requires Linux; no platform skip\n' >&2; exit 2; }
image=$1 refresh=$2 server=$3 editor=$4 output=$5 cpus=$6 memory=$7
toolchain_exports=$(sh "$repo/ci/impl-toolchain.sh" toolchain-env js --helpers 'npm:@devcontainers/cli')
eval "$toolchain_exports"
node "$driver" validate "$image" "$refresh" "$server" "$editor" "$output" "$cpus" "$memory"
for tool in docker devcontainer git node tar ps; do
  command -v "$tool" >/dev/null || { printf 'missing tool: %s\n' "$tool" >&2; exit 2; }
done
if [ -z "${TMPDIR-}" ] || [ ! -d "$TMPDIR" ]; then
  printf 'set TMPDIR to an existing temporary root\n' >&2; exit 2;
fi
mkdir "$output"
output=$(CDPATH='' cd -- "$output" && pwd)
control="$output/control"
workspace="$output/checkout"
mkdir "$control" "$control/tmp"
cp "$driver" "$control/driver.cjs"
name="kio-acceptance-$(node -e 'process.stdout.write(require("node:crypto").randomBytes(8).toString("hex"))')"
container=
finished=0
cleanup() {
  status=$?
  set +e
  trap - EXIT HUP INT TERM
  for identity in "$name" "$name-uid"; do
    target=
    if [ "$identity" = "$name" ]; then target=$container; fi
    if [ -z "$target" ]; then
      target=$(node "$driver" docker ps --all --quiet --filter "label=kio.acceptance=$identity") || status=1
    fi
    case "$target" in *[!a-f0-9]*) printf 'ambiguous cleanup ownership\n' >&2; target=; status=1 ;; esac
    if [ -n "$target" ]; then
      if [ "$identity" = "$name" ]; then
        # An unavailable final resource probe remains unknown, without preventing removal.
        node "$driver" docker exec --user vscode "$target" /bin/cat /sys/fs/cgroup/memory.peak /sys/fs/cgroup/memory.events /sys/fs/cgroup/memory.max /sys/fs/cgroup/cpu.max > "$output/resources-final-raw.log" 2>&1
        node "$driver" docker inspect "$target" > "$output/container-final.json" || status=1
      fi
      cleanup_result=$(node "$driver" docker rm -f "$target" 2>&1) || status=1
      printf '%s\n' "$cleanup_result" >> "$output/cleanup.log" || status=1
    fi
  done
  if [ "$status" -eq 0 ] && [ "$finished" -ne 1 ]; then status=1; fi
  if [ "$status" -eq 0 ]; then node "$driver" finish "$output" || status=1; fi
  printf '%s\n' "$status" > "$output/exit-status" || status=1
  if [ "$status" -eq 0 ]; then
    printf 'devcontainer-acceptance: PASS; retained receipt at %s/receipt.json\n' "$output"
  else
    printf 'devcontainer-acceptance: FAIL; retained evidence at %s\n' "$output" >&2
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

phase() {
  label=$1
  shift
  printf 'devcontainer-acceptance: %s\n' "$label"
  node "$driver" execute "$output" "$label" "$@"
}
dc() { devcontainer "$@"; }
phase pull docker pull "$image"
phase registry docker buildx imagetools inspect "$image" --raw
phase cli devcontainer --version
phase ownership docker run --rm --name "$name-uid" --label "kio.acceptance=$name-uid" --user vscode --entrypoint id "$image" -u
node "$driver" ownership "$output/ownership.log"
docker image inspect "$image" > "$control/image.json"
docker info --format '{{json .}}' > "$output/docker-host.json"
base=$(node "$driver" image-source "$control/image.json" "$image" "$output/registry.log" "$control/provenance.json")
git -C "$repo" cat-file -e "$base^{commit}"
git -C "$repo" cat-file -e "$refresh^{commit}"
[ -n "$(git -C "$repo" diff --name-only "$base" "$refresh" -- kio-rs/src kio-rs/build.rs kio-rs/Cargo.toml kio-rs/Cargo.lock)" ] || {
  printf 'refresh commit must change compiler inputs from image source\n' >&2; exit 2;
}
phase checkout git clone --local --no-checkout "$repo" "$workspace"
git -C "$workspace" checkout --detach "$base"
node "$driver" prepare "$repo" "$workspace" "$control" "$image" "$name" "$cpus" "$memory" "$server" "$editor" "$base" "$refresh"
config="$control/devcontainer.json"
phase create devcontainer up \
  --workspace-folder "$workspace" --config "$config" --skip-post-attach --log-format json
container=$(docker inspect --format '{{.Id}}' "$name")
docker inspect "$container" > "$control/container-created.json"
node "$driver" container-image "$control/image.json" "$control/container-created.json" "$control/provenance.json"
node "$driver" hooks "$output/create.log"
phase tools devcontainer exec \
  --container-id "$container" --workspace-folder "$workspace" --config "$config" \
  node /acceptance/driver.cjs tools "$base"
phase server devcontainer exec \
  --container-id "$container" --workspace-folder "$workspace" --config "$config" \
  node /acceptance/driver.cjs server

attach() {
  phase "$1-attach" devcontainer run-user-commands \
    --container-id "$container" --workspace-folder "$workspace" --config "$config" --log-format json
  phase "$1-editor" devcontainer exec \
    --container-id "$container" --workspace-folder "$workspace" --config "$config" \
    xvfb-run -a node /acceptance/driver.cjs editor "$1"
  docker inspect "$container" > "$output/$1-container.json"
  dc exec --container-id "$container" --workspace-folder "$workspace" --config "$config" \
    node /acceptance/driver.cjs resources "$1"
}
attach first
phase restart docker restart "$container"
phase reconnect devcontainer up \
  --workspace-folder "$workspace" --config "$config" --expect-existing-container --skip-post-attach --log-format json
attach reconnect

git -C "$workspace" checkout --detach "$refresh"
# --prebuild is the official CLI's explicit updateContent rerun path.
phase refresh devcontainer run-user-commands \
  --container-id "$container" --workspace-folder "$workspace" --config "$config" --prebuild --log-format json
node "$driver" hooks "$output/refresh.log" update
phase refresh-tools devcontainer exec \
  --container-id "$container" --workspace-folder "$workspace" --config "$config" \
  node /acceptance/driver.cjs tools "$refresh"
attach refreshed
phase injection devcontainer exec \
  --container-id "$container" --workspace-folder "$workspace" --config "$config" \
  xvfb-run -a node /acceptance/driver.cjs injection
finished=1
