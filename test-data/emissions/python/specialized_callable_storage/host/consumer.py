from __future__ import annotations

from typing import TYPE_CHECKING, Callable, TypeVar, cast

from callable_peer import create_callablePeer, kio_lift as peer_lift

from specialized_callable import (
    KioApply1,
    KioFn,
    KioHostMk_api__Box,
    KioHostType_api__Box,
    create_specializedCallable,
)

if TYPE_CHECKING:
    from specialized_callable import SpecializedCallableHost
    from specialized_callable import KioNewtype_api__N as Nominal
    from callable_peer import CallablePeerHost
    from callable_peer import KioNewtype_api__Hidden as PeerHidden

T = TypeVar("T")


class ApiHost:
    def __init__(self) -> None:
        self.packed = 0
        self.calls = 0

    def pack(self, value: T) -> KioApply1[KioHostMk_api__Box, T]:
        self.packed += 1
        return KioHostType_api__Box[T].from_native(value)

    def invoked(self) -> None:
        self.calls += 1


class Host:
    def __init__(self) -> None:
        self.api = ApiHost()

    def KioHostIn_api_Number(self, value: int) -> int:
        return value

    def KioHostOut_api_Number(self, value: int) -> int:
        return value


def unbox(value: KioApply1[KioHostMk_api__Box, T]) -> T:
    return cast(T, KioHostType_api__Box[T].to_native(value))


host = Host()
typed_host: SpecializedCallableHost[int] = host
pkg = create_specializedCallable(typed_host)
callbacks = [unbox(pkg.api.boxed()), unbox(pkg.api.boxedNamed()), unbox(pkg.api.boxedLiteral())]
assert host.api.packed == 3
assert host.api.calls == 0
valid = pkg.api.KioType_N.wrap(17)
foreign = pkg.other.KioType_N.wrap(29)
assert pkg.other.KioType_N.unwrap(foreign) == 29
for count, callback in enumerate(callbacks, 1):
    assert pkg.api.KioType_N.unwrap(callback(valid)) == 17
    assert host.api.calls == count
assert host.api.packed == 3

peer_host = Host()
typed_peer_host: CallablePeerHost[int] = peer_host
peer = create_callablePeer(typed_peer_host)
own_hidden = pkg.api.makeHidden(53)
peer_hidden = peer.api.makeHidden(67)
own_packed = pkg.api.makePacked(71)
peer_packed = peer.api.makePacked(89)
assert pkg.api.readHidden(own_hidden) == 53
assert peer.api.readHidden(peer_hidden) == 67
assert pkg.api.readPacked(own_packed) == 71
assert peer.api.readPacked(peer_packed) == 89
assert host.api.calls == 5
assert peer_host.api.calls == 2

for reader, wrong, selected, other in [
    (cast(Callable[[object], int], pkg.api.readHidden), peer_hidden, host, peer_host),
    (cast(Callable[[object], int], peer.api.readHidden), own_hidden, peer_host, host),
    (cast(Callable[[object], int], pkg.api.readPacked), peer_packed, host, peer_host),
    (cast(Callable[[object], int], peer.api.readPacked), own_packed, peer_host, host),
]:
    before_selected, before_other = selected.api.calls, other.api.calls
    try:
        reader(wrong)
    except TypeError:
        pass
    else:
        raise AssertionError("independent package opened a foreign nominal")
    # The Kio reader calls invoked before its private projector accesses storage.
    assert selected.api.calls == before_selected + 1
    assert other.api.calls == before_other

depth_calls = [0, 0]


def foreign_transform(value: Nominal[int]) -> PeerHidden[int]:
    depth_calls[0] += 1
    assert pkg.api.KioType_N.unwrap(value) == 17
    return peer_hidden


foreign_callback = peer_lift(foreign_transform)
deep_callback: KioFn[Nominal[int], PeerHidden[int]] = foreign_callback
for _ in range(4000):
    deep_callback = pkg.api.relayCallback(deep_callback)
    deep_callback = peer.api.relayCallback(deep_callback)
assert depth_calls == [0, 0]
assert peer.api.readHidden(deep_callback(valid)) == 67
assert depth_calls == [1, 0]


def foreign_factory(_unit: None) -> KioFn[Nominal[int], PeerHidden[int]]:
    depth_calls[1] += 1
    return foreign_callback


deep_factory: KioFn[None, KioFn[Nominal[int], PeerHidden[int]]] = peer_lift(foreign_factory)
for _ in range(4000):
    deep_factory = pkg.api.relayFactory(deep_factory)
    deep_factory = peer.api.relayFactory(deep_factory)
assert depth_calls == [1, 0]
returned_callback = deep_factory(None)
assert depth_calls == [1, 1]
assert peer.api.readHidden(returned_callback(valid)) == 67
assert depth_calls == [2, 1]
assert host.api.calls == 7
assert peer_host.api.calls == 6
print("specialized callable storage and privacy ok")
