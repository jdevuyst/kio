#!/bin/sh
# A label declaration does not generate a lowercase `label.member` path.
# Its projector is reached through field access (`h.?{hed}`) or the
# generated newtype member `Hed.get`. Label rewriting only fires in label
# syntax, so `hed.hed(h)` reaches the name resolver as an ordinary path
# with an unrecognised head and is rejected in this module.
set -u
cd workdir || exit
"$KIO_BIN" check
