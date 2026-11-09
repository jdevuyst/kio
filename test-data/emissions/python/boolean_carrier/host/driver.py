from dataclasses import dataclass

from boolean_carrier import create_booleanCarrier, kio_lift


@dataclass(frozen=True, eq=False)
class Flag:
    truth: bool
    tag: str


class Api:
    def __init__(self):
        self.events = []
        self.left_value = Flag(False, "initial-left")
        self.right_value = Flag(False, "initial-right")

    def left(self):
        self.events.append("L")
        return self.left_value

    def callback(self, _unit):
        self.events.append("R")
        return self.right_value

    def factory(self):
        self.events.append("F")
        return kio_lift(self.callback)


class Host:
    def __init__(self):
        self.api = Api()

    def KioHostIn_api_Flag(self, value):
        return value.truth

    def KioHostOut_api_Flag(self, value):
        return Flag(value, "literal")


host = Host()
package = create_booleanCarrier(host)
for left_truth in [False, True]:
    for right_truth in [False, True]:
        left = Flag(left_truth, "left")
        right = Flag(right_truth, "right")
        host.api.left_value = left
        host.api.right_value = right
        for conjunction in [False, True]:
            taken = left_truth if conjunction else not left_truth
            expected = right if taken else left
            inline = package.api.andInline if conjunction else package.api.orInline
            opaque = package.api.andCallback if conjunction else package.api.orCallback
            factory = package.api.andFactory if conjunction else package.api.orFactory

            result = inline(left, right)
            assert result is expected
            assert result.tag == expected.tag

            host.api.events.clear()
            result = opaque(left, kio_lift(host.api.callback))
            assert result is expected
            assert result.tag == expected.tag
            assert host.api.events == (["R"] if taken else [])

            host.api.events.clear()
            result = factory()
            assert result is expected
            assert result.tag == expected.tag
            assert host.api.events == (["L", "F", "R"] if taken else ["L", "F"])
