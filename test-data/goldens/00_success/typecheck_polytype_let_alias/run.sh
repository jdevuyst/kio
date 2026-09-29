#!/bin/sh
# Subject: E-Let polytype carry-through (specs/formal/elaboration.md
# § 6 — "A polymorphic RHS yields a polymorphic local only when
# the RHS itself synthesizes a polytype").
#
# `let f = id;` where `id : [A] A -> A` is a top-level
# polymorphic fn binds `f` at `id`'s polytype. The local can
# then be applied at any type-arg through standard E-App
# dispatch. Aliasing chains carry the polytype forward.
#
# Negative twin: 14_type_error/unannotated_fn_literal_in_let
# pins that `let g = .(x) { x };` (un-annotated lambda literal,
# no expected polytype) still rejects — Hindley-Milner-style
# let-generalization stays excluded.
set -u
cd workdir || exit
"$KIO_BIN" check
