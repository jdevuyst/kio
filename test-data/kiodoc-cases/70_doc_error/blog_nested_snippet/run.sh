#!/bin/sh
# An unfiltered docs check must validate Kio snippets in nested blog pages.
set -u
"$KIO_BIN" doc check
