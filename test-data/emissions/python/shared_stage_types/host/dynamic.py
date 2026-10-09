from types import SimpleNamespace
from stages_first import create_stagesFirst, kio_lift, kio_lift_stage

events = []
package = create_stagesFirst(SimpleNamespace(api=SimpleNamespace(mark=lambda: None)))


class Identity:
    def advance(self):
        events.append("stage")

        def value(argument):
            events.append("value")
            return argument

        return kio_lift(value)


poly = package.api.relayPoly(kio_lift_stage(Identity()))
assert events == []
selected = poly.advance()
assert events == ["stage"]
assert selected(37) == 37
assert events == ["stage", "value"]
package.api.call(kio_lift(lambda _unit: events.append("ordinary")))
assert events == ["stage", "value", "ordinary"]
print("dynamic stages ok")
