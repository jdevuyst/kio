#!/bin/sh
# A package mixing one passing equiv with one failing one. The
# failing block has terms that residualize to distinct explicit
# Unit-saturated calls, `foo(())` and `bar(())`, exercising the NF-class
# breakdown report and the `50` exit code from the `5x` test-error
# tier.
set -u
cd workdir || exit
"$KIO_BIN" test
