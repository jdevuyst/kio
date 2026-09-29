#!/bin/sh
# Render only the documented item signature, including structural forall
# brackets and the distinct binder and type-use token roles.
set -eu

trap 'rm -rf out' EXIT

"$KIO_BIN" doc build --html
sed -n '/<pre class="sig">/p' out/sig.html
