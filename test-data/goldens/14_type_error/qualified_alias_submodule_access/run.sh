#!/bin/sh
# Sub-module access through a qualified-import alias
# (`<alias>.<sub>.<fn>`) is rejected: an `import m as m;` binds one
# module's surface, not a tree of sub-modules. The only valid
# three-segment dotted-path through an alias is
# `<alias>.<TypeName>.<member>` (see
# `00_success/exec_qualified_newtype_member`).
set -u
cd workdir || exit
"$KIO_BIN" check
