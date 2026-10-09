from __future__ import annotations

from typing import TYPE_CHECKING, Callable, NoReturn

from qualifier_hygiene import create_qualifierHygiene

if TYPE_CHECKING:
    from qualifier_hygiene import QualifierHygieneHost


class StopError(Exception):
    pass


class Capabilities:
    def __init__(self, events: list[str]) -> None:
        self.events = events

    def typing(self) -> None:
        self.events.append("typing")

    def zStop(self) -> NoReturn:
        self.events.append("stop")
        raise StopError("host stop")


class Host:
    def __init__(self, events: list[str]) -> None:
        self.capabilities = Capabilities(events)


def requires_bottom(callback: Callable[[], NoReturn]) -> None:
    _ = callback


events: list[str] = []
host: QualifierHygieneHost = Host(events)
package = create_qualifierHygiene(host)
package.api.typing()
requires_bottom(package.api.zStop)
try:
    package.api.zStop()
except StopError as error:
    assert str(error) == "host stop"
else:
    raise AssertionError("bottom-returning call returned")
assert events == ["typing", "stop"]
print("typing qualifiers: ok")
