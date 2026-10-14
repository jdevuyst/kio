import pathlib
import types
import unittest


def load_runtime():
    path = pathlib.Path(__file__).with_name("runtime.py")
    module = types.ModuleType("runtime_test")
    exec(compile(path.read_text(encoding="utf-8"), str(path), "exec"), module.__dict__)
    return module


def reference(name):
    return {"LowBoundRef": {"name": name}}


def call(name, *arguments, tail=True):
    data = {"name": name, "args": list(arguments), "type_args": []}
    if tail:
        data["tail"] = "value"
    return {"LowClosureCall": data}


def advance(name):
    return {"LowTypeApplication": {"callee": reference(name), "tail": "type"}}


def value_stage(*names, captures=()):
    return {"kind": "value", "params": list(names), "captures": list(captures)}


TYPE_STAGE = {"kind": "type", "params": [], "captures": []}
STRING_IDENTITY = {"module": ["api"], "name": "String", "frame": "String"}


def definition(stages, body):
    return {"sig": {"stages": stages}, "body": body}


def projector_entry(type_stages=0, arity=1):
    return definition(
        [value_stage("continuation", captures=("payload",) if arity else ())],
        {"LowClosureCall": {
            "name": "continuation", "type_args": [None] * type_stages,
            "args": [reference("payload")] if arity else [], "tail": "value",
        }},
    )


def guest(runtime, stages, body, captures=None):
    if captures:
        stages = [{**stages[0], "captures": list(captures)}, *stages[1:]]
    return runtime._make_fn("api", definition(stages, body), captures or {})


def package_runtime(module, log):
    compiled = types.SimpleNamespace(
        modules={}, boundary={"sites": [], "host_bindings": []}
    )
    host = {"KioHostOut_api_String": lambda value: log.append(value) or value}
    return module._PackageRuntime(compiled, host)


class TailEntryTests(unittest.TestCase):
    def setUp(self):
        self.module = load_runtime()
        self.log = []
        self.runtime = package_runtime(self.module, self.log)

    def test_package_instances_share_value_and_type_entries(self):
        other = package_runtime(self.module, [])
        value = guest(self.runtime, [value_stage("x")], reference("x"))
        poly = guest(self.runtime, [TYPE_STAGE], reference("answer"), {"answer": 37})
        for index in range(2048):
            owner = self.runtime if index % 2 else other
            value = guest(owner, [value_stage("x")], call("next", reference("x")), {"next": value})
            poly = guest(owner, [TYPE_STAGE], advance("next"), {"next": poly})
        self.assertEqual(value(41), 41)
        self.assertEqual(value(43), 43)
        self.assertEqual(poly.advance(), 37)
        self.assertEqual(poly.advance(), 37)

    def test_independent_artifacts_transfer_value_and_type_stages(self):
        foreign_log = []
        foreign = package_runtime(load_runtime(), foreign_log)
        literal = {"StrLit": {"host_type": STRING_IDENTITY, "value": "foreign"}}
        value = guest(foreign, [value_stage()], literal)
        poly = guest(foreign, [TYPE_STAGE], literal)
        for index in range(20000):
            owner = self.runtime if index % 2 else foreign
            value = guest(owner, [value_stage()], call("next"), {"next": value})
            poly = guest(owner, [TYPE_STAGE], advance("next"), {"next": poly})
        self.assertEqual(value(None), "foreign")
        self.assertEqual(poly.advance(), "foreign")
        self.assertEqual(value(None), "foreign")
        self.assertEqual(poly.advance(), "foreign")
        self.assertEqual(foreign_log, ["foreign"] * 4)
        self.assertEqual(self.log, [])

    def test_opaque_callables_do_not_expose_metadata(self):
        class OpaqueMetaclass(type):
            def __getattribute__(cls, name):
                raise AssertionError("metaclass attribute read: " + name)

        class OpaqueCallable(metaclass=OpaqueMetaclass):
            __slots__ = ()

            def __getattribute__(self, name):
                if name == "advance":
                    return object.__getattribute__(self, name)
                raise AssertionError("instance attribute read: " + name)

            def __call__(self, value):
                return value

            def advance(self):
                return 47

        opaque = OpaqueCallable()
        value = guest(self.runtime, [value_stage("x")], call("opaque", reference("x")), {"opaque": self.module.kio_lift(opaque)})
        poly = guest(self.runtime, [TYPE_STAGE], advance("opaque"), {"opaque": self.module.kio_lift_stage(opaque)})
        self.assertEqual(value(53), 53)
        self.assertEqual(poly.advance(), 47)

    def test_host_method_names_do_not_grant_guest_entry_authority(self):
        events = []

        class HostCallable:
            def _kio_tail_entry_v1(self):
                raise AssertionError("an ordinary host method is not a guest entry")

            @property
            def __class__(self):
                raise AssertionError("an ordinary host descriptor must not be read")

            def __call__(self, value):
                events.append("value")
                return value

            def advance(self):
                events.append("type")
                return 47

        host = HostCallable()
        value = guest(self.runtime, [value_stage("x")], call("host", reference("x")), {"host": self.module.kio_lift(host)})
        poly = guest(self.runtime, [TYPE_STAGE], advance("host"), {"host": self.module.kio_lift_stage(host)})
        self.assertEqual(value(53), 53)
        self.assertEqual(poly.advance(), 47)
        self.assertEqual(events, ["value", "type"])

    def test_guest_owner_and_residual_captures_survive_transfer(self):
        foreign_log = []
        foreign = package_runtime(self.module, foreign_log)
        literal = {"StrLit": {"host_type": STRING_IDENTITY, "value": "owner"}}
        value = guest(foreign, [value_stage()], literal)
        poly = guest(foreign, [TYPE_STAGE], literal)
        value_caller = guest(self.runtime, [value_stage()], call("target"), {"target": value})
        type_caller = guest(self.runtime, [value_stage()], advance("target"), {"target": poly})
        self.assertEqual(value_caller(None), "owner")
        self.assertEqual(type_caller(None), "owner")
        self.assertEqual(foreign_log, ["owner", "owner"])
        self.assertEqual(self.log, [])
        mixed = guest(foreign, [TYPE_STAGE, value_stage("x"), {**TYPE_STAGE, "captures": ["x"]}], reference("x"))
        left, right = mixed.advance()(59), mixed.advance()(61)
        returned = guest(self.runtime, [value_stage()], reference("saved"), {"saved": left})
        self.assertIs(returned(None), left)
        self.assertEqual(left.advance(), 59)
        self.assertEqual(right.advance(), 61)
        self.assertEqual(left.advance(), 59)

    def test_effect_order_callbacks_and_host_reentry(self):
        events = []
        callback = self.module.KioFn.lift(lambda value: events.append(value) or value)
        type_stage = self.module._KioPoly(lambda: events.append("type") or callback)
        argument = self.module.KioFn.lift(lambda _: events.append("argument") or 67)
        staged_call = {"LowClosureCall": {
            "name": "target", "type_args": [None], "tail": "value",
            "args": [call("argument", {"Unit": {}}, tail=False)],
        }}
        staged = guest(self.runtime, [value_stage()], staged_call, {"target": type_stage, "argument": argument})
        self.assertEqual(staged(None), 67)
        self.assertEqual(events, ["type", "argument", 67])
        twice = guest(self.runtime, [value_stage("callback")], {"Seq": {
            "value": call("callback", {"Unit": {}}, tail=False),
            "body": call("callback", {"Unit": {}}),
        }})
        twice(callback)
        self.assertEqual(events[-2:], [None, None])
        identity = guest(self.runtime, [value_stage("x")], reference("x"))
        retained = guest(self.runtime, [value_stage("target")], {"FnExpr":
            definition([value_stage("x", captures=("target",))], call("target", reference("x")))
        })(identity)
        self.assertEqual(retained(71), 71)
        self.assertEqual(retained(73), 73)
        reentrant = self.module.KioFn.lift(lambda value: identity(value + 1) + 1)
        reentry = guest(self.runtime, [value_stage("x")], call("host", reference("x")), {"host": reentrant})
        self.assertEqual(reentry(79), 81)
        marker = RuntimeError("host exception")

        def fail(_):
            raise marker

        broken = guest(self.runtime, [value_stage()], call("host"), {"host": self.module.kio_lift(fail)})
        with self.assertRaises(RuntimeError) as raised:
            broken(None)
        self.assertIs(raised.exception, marker)
        self.assertEqual(retained(83), 83)

    def test_returned_projector_entries_forward_continuations(self):
        entry = projector_entry()
        value = guest(self.runtime, [value_stage("x")], reference("x"))
        continuation = {"FnExpr": definition([value_stage("next", captures=("x",))], call("next", reference("x")))}
        for _ in range(2048):
            adapter = self.runtime._make_fn(None, entry, {"payload": value})
            value = guest(self.runtime, [value_stage("x")], call("adapter", continuation), {"adapter": adapter})
        self.assertEqual(value(89), 89)
        self.assertEqual(value(97), 97)

    def test_projector_type_advancement_and_unit_domain(self):
        for arity in (0, 1):
            events = []
            payload = object()
            entry = projector_entry(type_stages=2, arity=arity)
            adapter = self.runtime._make_fn(None, entry, {"payload": payload})
            self.assertEqual(adapter.env, {"payload": payload} if arity else {})

            def invoke(value):
                self.assertIs(value, payload if arity else None)
                events.append("value")
                return 101

            callback = self.module._KioPoly(lambda: events.append("first") or
                self.module._KioPoly(lambda: events.append("second") or
                    self.module.KioFn.lift(invoke)))
            self.assertEqual(events, [])
            self.assertEqual(adapter(callback), 101)
            self.assertEqual(adapter(callback), 101)
            self.assertEqual(events, ["first", "second", "value"] * 2)

    def test_entry_and_residual_environments_follow_exact_capture_plan(self):
        kept, ignored = object(), object()
        stages = [
            value_stage("first", "unused", captures=("kept",)),
            {**TYPE_STAGE, "captures": ["kept", "first"]},
            value_stage("last", captures=("kept", "first")),
        ]
        body = {"EnrichedTuple": {"items": [reference("kept"), reference("first")]}}
        fn = self.runtime._make_fn(
            "api", definition(stages, body),
            {"kept": kept, "ignored": ignored},
        )
        self.assertEqual(fn.env, {"kept": kept})
        first = fn({"left": 103, "right": ignored})
        second = fn({"left": 107, "right": ignored})
        self.assertEqual(first.env, {"kept": kept, "first": 103})
        self.assertEqual(second.env, {"kept": kept, "first": 107})
        selected = first.advance()
        self.assertEqual(selected.env, {"kept": kept, "first": 103})
        self.assertEqual(selected(ignored), {"left": kept, "right": 103})
        self.assertEqual(selected(ignored), {"left": kept, "right": 103})
        self.assertEqual(second.advance()(None), {"left": kept, "right": 107})
        self.assertEqual(fn.env, {"kept": kept})

    def test_projector_result_type_stage_keeps_payload_without_invoking_callback(self):
        events = []
        payload = object()
        entry = projector_entry()
        entry["sig"]["stages"].insert(0, {**TYPE_STAGE, "captures": ["payload"]})
        result = self.runtime._make_fn(None, entry, {"payload": payload, "unused": object()})
        self.assertEqual(result.env, {"payload": payload})
        first, second = result.advance(), result.advance()
        self.assertEqual(events, [])
        self.assertEqual(first.env, {"payload": payload})
        callback = self.module.kio_lift(lambda value: events.append(value) or 113)
        self.assertEqual(first(callback), 113)
        self.assertEqual(second(callback), 113)
        self.assertEqual(events, [payload, payload])

    def test_nested_capture_selection_does_not_retain_activation_locals(self):
        inner = definition([value_stage(captures=("saved",))], reference("saved"))
        outer = guest(self.runtime, [value_stage("saved", "unused")], {"FnExpr": inner})
        captured = object()
        returned = outer({"left": captured, "right": object()})
        self.assertEqual(returned.env, {"saved": captured})
        self.assertIs(returned(None), captured)
        self.assertIs(returned(None), captured)

    def test_required_capture_is_not_recovered_from_an_ambient_environment(self):
        fn = definition([value_stage(captures=("required",))], reference("required"))
        with self.assertRaises(KeyError):
            self.runtime._make_fn("api", fn, {"unrelated": 109})
        omitted = definition([value_stage()], reference("required"))
        broken = self.runtime._make_fn("api", omitted, {"required": 109})
        with self.assertRaises(KeyError):
            broken(None)


if __name__ == "__main__":
    unittest.main()
