#!/bin/sh
# Tier 1 of literal resolution (specs/language.md § Literals): an
# explicit `(Type)` annotation on the literal pins its type
# directly, independent of the surrounding context. Exercises every
# literal kind — String, Bool, integer, float — plus an integer
# literal annotated as a float-shaped type (the admission
# relation lets an integer literal stand in for a float role).
set -u
cd workdir || exit
"$KIO_BIN" check
