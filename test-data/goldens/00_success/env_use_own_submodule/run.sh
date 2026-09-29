#!/bin/sh
# A root module's `host fn` signatures may name a Kio-defined type
# declared in the root's OWN submodule, brought into scope by a
# `import env_use_own_submodule/sub(Int, Wrap);` clause. The submodule
# declares both the host type `Int` and the `Wrap` newtype over it, so
# the dependency runs root -> submodule only; the submodule does not
# import the root back, so no module cycle forms.
set -u
cd workdir || exit
"$KIO_BIN" build "$KIO_TARGET" || exit

# Real Haskell coverage: GHC-compile the emitted module under the
# `compile-only` contract; an emitted-Haskell type error fails the build.
# The other targets remain emit-checked (`kio build`), while the Rust-specific
# white-box assertions below stay gated to Rust.
if [ "$KIO_TARGET" = haskell ]; then
  "$KIO_RUNNER" --protocol compile-only "out/$KIO_TARGET" || exit
fi
