#!/bin/sh
# Rank-N polymorphism positive case. Exercises:
#   - `[U] U -> U` as a parameter type (rank-2).
#   - Passing an inline polymorphic `fn` against a forall-shaped
#     expected type.
#   - Binding the result of an `id` instantiation (whose type is a
#     forall) to a `let` and applying it.
#   - Multiple binders in one function type (`[A], [B]`).
# All declarations are well-typed; `kio check` exits 0.
set -u
cd workdir || exit
"$KIO_BIN" check
