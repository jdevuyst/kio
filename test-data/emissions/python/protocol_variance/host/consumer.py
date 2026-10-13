from __future__ import annotations

from typing import TYPE_CHECKING

from protocol_variance import create_protocolVariance

if TYPE_CHECKING:
    from protocol_variance import KioViewHost_api__run_Arg0, ProtocolVarianceHost


class Token:
    pass


class Api:
    def __init__(self) -> None:
        self.calls = 0

    def run(self, callback: KioViewHost_api__run_Arg0[Token], /) -> None:
        callback({"left": Token(), "right": Token()})
        self.calls += 1


class Host:
    def __init__(self) -> None:
        self.api = Api()


def preserve(host: ProtocolVarianceHost[Token]) -> ProtocolVarianceHost[Token]:
    return host


host = Host()
package = create_protocolVariance(preserve(host))
package.api.invoke()
assert host.api.calls == 1
print("invariant callback host: ok")
