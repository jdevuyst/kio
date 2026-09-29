#!/bin/sh
# `onto!` accepts identity on every target shape (acceptance
# monotonicity — see specs/language.md § iso! / into! / onto! /
# align! mechanics). In particular, identity succeeds when the
# target's DNF has a branch whose factor multi-set is a subset
# of a later branch — `()`'s empty product is a subset of `[T]`,
# but the engine's saturating-first per-source-branch search
# lands the identity assignment rather than catching the broader
# source in the empty branch via projection.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
