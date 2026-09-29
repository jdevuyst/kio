#!/bin/sh
# SUBJECT: Signature bytes partition Rust artifact-cache entries before sealed removals change emitted host declarations.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir/. "$scratch/"
rm -rf "$scratch/out"
cd "$scratch"

# Build 1: the sealed sig records no removal, so the Host trait carries
# no deprecated method. The artifact is cached under the sig-folded key.
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
if grep -q '#\[deprecated' out/rust/src/host.rs; then
  printf 'build 1: host.rs should carry no deprecated method yet\n' >&2
  exit 1
fi

# Change ONLY the sealed sig: record `log` added at v(1) and removed at a
# sealed v(2). No `.kio` module source changes, so the Prime bytes are
# identical to build 1.
cat > app.sig.kio <<'SIG'
signature app v(3);

v(1) {
  nonbreaking {
    add {
      module api {
        host type Str role(str);
        host fn open() -> Str;
        host fn log(p0: Str) -> .;
        pub fn echo(s: Str) -> Str;
      }
    }
  }
}

v(2) {
  nonbreaking {
    remove {
      module api {
        log;
      }
    }
  }
}
SIG

# Build 2: a Prime-only artifact-cache key would HIT here and serve the
# stale build-1 crate (no deprecated method). The sig-folded key MISSES
# and regenerates — the removed `log` must now re-emit as a #[deprecated]
# trait method.
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
# shellcheck disable=SC2016
grep -qF '#[deprecated(note = "host fn `api__log` removed at v(2)")]' \
  out/rust/src/host.rs || {
  printf 'build 2: sealed sig change did not bust the artifact cache\n' >&2
  printf '(the regenerated crate is missing the deprecated re-emit)\n' >&2
  exit 1
}
