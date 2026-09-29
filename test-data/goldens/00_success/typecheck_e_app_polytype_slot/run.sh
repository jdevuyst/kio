#!/bin/sh
# Subject: E-App § 5.2 (`specs/formal/elaboration.md`) —
# value-argument checking against a polytype slot, for the
# **non-lambda-literal** branch.
#
#   When a callee parameter slot is itself a polytype (rank-2-
#   or-higher position) and the value at that slot is *not* a
#   lambda literal (so E-FnCheck does not fire), the typer
#   checks structural equivalence between the value's
#   synthesized polytype and the parameter's expected polytype.
#   Shallow skolemization is not needed because the value
#   already has a polytype shape; alpha-equivalence on
#   `Forall` decides admissibility.
#
# The canonical exemplar is a path expression referring to a
# top-level polymorphic `fn` passed into a rank-2 parameter
# slot:
#
#     fn id[U](v: U) -> U { v }
#     fn use_id(f: [T] T -> T) -> . { ... }
#     pub fn main() -> . { use_id(id) }
#
# `id`'s declared scheme `∀U. (U) → U` is alpha-equivalent to
# `use_id`'s parameter polytype `∀T. (T) → T`; the typer
# accepts via structural equivalence — no instantiation, no
# skolemization. Discriminating vs. typecheck_rank_n: this
# golden's subject is the non-`fn`-literal branch of E-App
# § 5.2 (path → polytype slot); typecheck_rank_n covers the
# `fn`-literal branch (E-FnCheck).
set -u
cd workdir || exit
"$KIO_BIN" check
