#!/bin/sh
# SUBJECT: TypeScript renders the complete representable retained closure as optional and deprecated while its JS stays current-only.
# CONTRACT: specs/backends/ts.md § Deprecated host items
# Literal backticks below are part of the asserted generated documentation.
# shellcheck disable=SC2016
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.ts-deprecated-shape.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
rm -rf "$scratch/workdir/out"

cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

dts=$(cat out/ts/app.d.ts)
# Host functions and host-type bindings have distinct optionality contracts.
host_dts=$(awk '
  /^interface __AppHost</ { found++; inside = 1; next }
  inside && /^}/ { closed++; inside = 0; next }
  inside { if (NF) body++; print }
  END { if (found != 1 || closed != 1 || body == 0) exit 1 }
' out/ts/app.d.ts) || {
  printf 'app.d.ts: expected one complete nonempty __AppHost declaration\n' >&2
  exit 1
}
printf '%s' "$host_dts" | grep -qF '/** @deprecated Host fn `api.old` was removed at v(2). */' || {
  printf 'app.d.ts: retained api.old lacks recognized deprecation JSDoc\n' >&2
  exit 1
}
printf '%s' "$host_dts" | grep -qF 'readonly old?:' || {
  printf 'app.d.ts: retained api.old is not optional\n' >&2
  exit 1
}
printf '%s' "$host_dts" | grep -qF 'readonly api: {' || {
  printf 'app.d.ts: mixed live/history api namespace is not required\n' >&2
  exit 1
}
if printf '%s' "$host_dts" | grep -qF 'readonly api?: {'; then
  printf 'app.d.ts: mixed live/history api namespace became optional\n' >&2
  exit 1
fi
printf '%s' "$host_dts" | grep -qF 'readonly open: () => string;' || {
  printf 'app.d.ts: live api.open is not required\n' >&2
  exit 1
}
printf '%s' "$host_dts" | grep -qF '/** @deprecated Host module `legacy` is retained for removed host functions. */' || {
  printf 'app.d.ts: retained-only legacy namespace lacks deprecation JSDoc\n' >&2
  exit 1
}
printf '%s' "$host_dts" | grep -qF 'readonly legacy?: {' || {
  printf 'app.d.ts: retained-only legacy namespace is not optional\n' >&2
  exit 1
}
printf '%s' "$host_dts" | grep -qF '/** @deprecated Host fn `legacy.retired` was removed at v(2). */' || {
  printf 'app.d.ts: retained legacy.retired lacks recognized deprecation JSDoc\n' >&2
  exit 1
}
printf '%s' "$host_dts" | grep -qF 'readonly retired?:' || {
  printf 'app.d.ts: retained legacy.retired is not optional\n' >&2
  exit 1
}
printf '%s' "$dts" | grep -qF '/** @deprecated Host type `legacy.Gone` was removed at v(2). */' || {
  printf 'app.d.ts: retained legacy.Gone binding lacks recognized deprecation JSDoc\n' >&2
  exit 1
}
printf '%s' "$dts" | grep -qF '/** @deprecated Type `legacy.Gone` is retained only for host declarations removed at v(2). */' || {
  printf 'app.d.ts: retained legacy.Gone carrier lacks recognized deprecation JSDoc\n' >&2
  exit 1
}
printf '%s' "$dts" | grep -qF '/** @deprecated Host type `api.Box` was removed at v(2). */' || {
  printf 'app.d.ts: retained api.Box binding lacks deprecation JSDoc\n' >&2
  exit 1
}
printf '%s' "$dts" | grep -qF 'readonly Box?: AppTypeLambda<' || {
  printf 'app.d.ts: retained api.Box binding is not optional\n' >&2
  exit 1
}
printf '%s' "$dts" | grep -qF '/** @deprecated Host type carrier `api.Token` was removed at v(2). */' || {
  printf 'app.d.ts: a retained transitive declaration lacks deprecation JSDoc\n' >&2
  exit 1
}
printf '%s' "$dts" | grep -qF '/** @deprecated Internal compatibility helper for removed host declarations. */' || {
  printf 'app.d.ts: retained compatibility helper lacks deprecation JSDoc\n' >&2
  exit 1
}
if printf '%s' "$dts" | grep -qF 'Unused'; then
  printf 'app.d.ts: unreachable retained api.Unused leaked into the compatibility closure\n' >&2
  exit 1
fi

app_js=$(cat out/ts/app.js)
if printf '%s' "$app_js" | grep -Eq 'api\.old|legacy|retired'; then
  printf 'app.js: retained TypeScript declarations leaked into the current JS runtime\n' >&2
  exit 1
fi
