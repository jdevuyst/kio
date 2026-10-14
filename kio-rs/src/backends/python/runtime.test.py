import pathlib
import types
import unittest


def load_runtime():
    path = pathlib.Path(__file__).with_name("runtime.py")
    module = types.ModuleType("runtime_test")
    exec(compile(path.read_text(encoding="utf-8"), str(path), "exec"), module.__dict__)
    return module


def slot(name):
    return {"ControlSlot": {"slot": name}}


def block(exit, operations=(), input=None):
    return {"input": input, "operations": list(operations), "exit": exit}


def control(*blocks, resumes=()):
    return {"Control": {"graph": {"start": 0, "inputs": [], "blocks": list(blocks), "resumes": list(resumes)}}}


def invocation(name, *arguments, type_stage=False, resume=None):
    return {"kind": "invoke", "callee": name, "arguments": list(arguments),
            "stage": "type" if type_stage else "value", "resume": resume}


def reference(name):
    return control(block({"kind": "return", "slot": name}))


def call(name, *arguments):
    return control(block(invocation(name, *arguments)))


def advance(name):
    return control(block(invocation(name, type_stage=True)))


def atom(kind, node, output="result", bindings=()):
    return {"kind": "atom", "atom": kind, "output": output, "node": node,
            "bindings": [{"slot": name, "name": name} for name in bindings]}


def returned_atom(kind, node, bindings=()):
    return control(block({"kind": "return", "slot": "result"}, [atom(kind, node, bindings=bindings)]))


def value_stage(*names, captures=()):
    return {"kind": "value", "params": list(names), "captures": list(captures)}


TYPE_STAGE = {"kind": "type", "captures": []}
STRING_IDENTITY = {"module": ["api"], "name": "String", "frame": "String"}


def definition(stages, body):
    inputs = dict.fromkeys(stages[-1]["captures"] + stages[-1].get("params", []))
    graph = {**body["Control"]["graph"], "inputs": [{"slot": name, "name": name} for name in inputs]}
    return {"sig": {"stages": stages}, "body": {"Control": {"graph": graph}}}


def projector_entry(type_stages=0, arity=1):
    blocks, resumes = [], []
    current = "continuation"
    captures = ["payload"] if arity else []
    for index in range(type_stages):
        result = f"selected{index}"
        blocks.append(block(invocation(current, type_stage=True, resume=index),
                            input=current if index else None))
        resumes.append({"block": index + 1, "result": result, "live": captures})
        current = result
    blocks.append(block(invocation(current, *captures), input=current if type_stages else None))
    return definition(
        [value_stage("continuation", captures=("payload",) if arity else ())],
        control(*blocks, resumes=resumes),
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


class CooperativeFunction:
    def __init__(self, module, entry):
        self.module = module
        self.entry = entry

    def __call__(self, domain):
        return self.module._kio_run(self, domain, False)

    def kio_step(self, domain, transfer):
        return self.entry(domain, transfer)


class CooperativeStage:
    def __init__(self, module, entry):
        self.module = module
        self.entry = entry

    def advance(self):
        return self.module._kio_run(self, None, True)

    def kio_step(self, transfer):
        return self.entry(transfer)


def legacy_run(target, domain, type_stage=False, lookup=lambda: None):
    class Transfer:
        pending = True

        @property
        def resume(self):
            lookup()
            return None

        def call(self, next_target, value):
            nonlocal target, domain, type_stage
            target, domain, type_stage = next_target, value, False
            self.pending = True

        def advance(self, next_target):
            nonlocal target, domain, type_stage
            target, domain, type_stage = next_target, None, True
            self.pending = True

    transfer = Transfer()
    try:
        while transfer.pending:
            transfer.pending = False
            result = target.kio_step(transfer) if type_stage else target.kio_step(domain, transfer)
        return result
    finally:
        target = None
        domain = None


class ResumeDriverTests(unittest.TestCase):
    def test_checked_control_graph_resumes_with_only_its_planned_live_slots(self):
        module = load_runtime()
        runtime = package_runtime(module, [])
        graph = {
            "start": 0,
            "inputs": [{"slot": name, "name": name} for name in ("next", "record", "kept", "x")],
            "blocks": [
                {"input": None, "operations": [],
                 "exit": {"kind": "invoke", "callee": "next", "stage": "value", "arguments": ["x"], "resume": 0}},
                {"input": "child", "operations": [
                    {"kind": "atom", "atom": "tuple", "output": "packet", "bindings": [],
                     "node": {"EnrichedTuple": {"items": [
                         {"ControlSlot": {"slot": "kept"}}, {"ControlSlot": {"slot": "child"}}]}}}],
                 "exit": {"kind": "invoke", "callee": "record", "stage": "value", "arguments": ["packet"], "resume": None}},
            ],
            "resumes": [{"block": 1, "result": "child", "live": ["record", "kept"]}],
        }
        events = []
        record = module.kio_lift(lambda pair: events.append(pair["left"]) or pair["right"] + 1)
        chain = module.kio_lift(lambda value: value)
        for index in range(2048):
            chain = guest(runtime, [value_stage("x")], {"Control": {"graph": graph}},
                {"next": chain, "record": record, "kept": index})
        self.assertEqual(chain(23), 2071)
        self.assertEqual(events, list(range(2048)))
        events.clear()
        self.assertEqual(chain(29), 2077)
        self.assertEqual(events, list(range(2048)))
        resume = module._KioBlockResume(runtime, "api", graph, graph["resumes"][0],
            {"record": record, "kept": 7, "unused": object()})
        self.assertEqual(set(resume.saved), {"record", "kept"})
        with self.assertRaises(KeyError):
            module._KioBlockResume(runtime, "api", graph, graph["resumes"][0], {"record": record})
        with self.assertRaisesRegex(RuntimeError, "evaluated slot"):
            runtime._control_operand({"LowClosureCall": {"name": "next", "args": []}}, {"next": chain})

    def test_control_resume_can_suspend_again_and_return_unit_or_unwind(self):
        module = load_runtime()
        runtime = package_runtime(module, [])
        events = []
        marker = RuntimeError("second pending child failure")
        should_fail = False

        def first(value):
            events.append(("first", value))
            return value + 1

        def second(value):
            events.append(("second", value))
            if should_fail:
                raise marker
            return None

        body = control(
            block(invocation("first", "x", resume=0)),
            block(invocation("second", "child", resume=1), input="child"),
            block({"kind": "return", "slot": "unit"}, input="unit"),
            resumes=[{"block": 1, "result": "child", "live": ["second"]},
                     {"block": 2, "result": "unit", "live": []}],
        )
        entry = guest(runtime, [value_stage("x")], body,
                      {"first": module.kio_lift(first), "second": module.kio_lift(second)})
        self.assertIsNone(entry(7))
        self.assertEqual(events, [("first", 7), ("second", 8)])
        should_fail = True
        with self.assertRaises(RuntimeError) as raised:
            entry(11)
        self.assertIs(raised.exception, marker)
        should_fail = False
        self.assertIsNone(entry(13))
        self.assertEqual(events, [("first", 7), ("second", 8), ("first", 11),
                                  ("second", 12), ("first", 13), ("second", 14)])

    def test_pending_frames_cross_artifacts_and_return_in_reverse_call_order(self):
        modules = [load_runtime(), load_runtime()]
        events = []
        returned = modules[0].kio_lift(lambda value: value)
        chain = modules[0].kio_lift(lambda value: (value, returned))
        for index in range(4096):
            owner, next_entry = modules[index % 2], chain

            def invoke(value, transfer, owner=owner, next_entry=next_entry, index=index):
                def resume(result, _next_transfer):
                    events.append(index)
                    return (result[0] + 1, result[1])
                return owner._kio_call_with_resume(transfer, next_entry, value, resume)

            chain = CooperativeFunction(owner, invoke)
        for value in (7, 11):
            result = chain(value)
            self.assertEqual(result[0], value + 4096)
            self.assertIs(result[1], returned)
            self.assertEqual(events, list(range(4096)))
            events.clear()
        lookups = []
        self.assertEqual(legacy_run(chain, 13, lookup=lambda: lookups.append(True))[0], 4109)
        self.assertEqual(lookups, [True])
        self.assertEqual(events, list(range(4096)))

    def test_tail_transfers_never_read_resume_capability(self):
        module = load_runtime()
        cursors = set()
        chain = module.kio_lift(lambda value: value + 1)
        for _ in range(4096):
            next_entry = chain

            def invoke(value, transfer, next_entry=next_entry):
                cursors.add(transfer)
                return transfer.call(next_entry, value)

            chain = CooperativeFunction(module, invoke)

        def unexpected_lookup():
            self.fail("a tail-only entry looked up the optional resume capability")

        self.assertEqual(legacy_run(chain, 17, lookup=unexpected_lookup), 18)
        self.assertEqual(len(cursors), 1)
        cursors.clear()
        self.assertEqual(chain(19), 20)
        self.assertEqual(len(cursors), 1)

    def test_type_results_are_data_and_exception_discards_frames(self):
        module = load_runtime()
        events, controls = [], []
        returned = module.kio_lift(lambda value: value)
        stage = module._KioPoly(lambda: returned)
        for index in range(2048):
            next_stage = stage

            def advance(transfer, next_stage=next_stage, index=index):
                def resume(result, next_transfer):
                    events.append(index)
                    if index == 2047:
                        self.assertEqual(next_transfer.frames, [])
                    return result
                return module._kio_advance_with_resume(transfer, next_stage, resume)

            stage = CooperativeStage(module, advance)
        self.assertIs(stage.advance(), returned)
        self.assertEqual(events, list(range(2048)))
        events.clear()
        self.assertIs(legacy_run(stage, None, True), returned)
        self.assertEqual(events, list(range(2048)))
        marker = RuntimeError("pending child failure")

        def fail(_):
            raise marker

        def broken(value, transfer):
            controls.append(transfer)
            return module._kio_call_with_resume(transfer, module.kio_lift(fail), value,
                lambda result, _cursor: self.fail("failed child resumed"))

        with self.assertRaises(RuntimeError) as raised:
            CooperativeFunction(module, broken)(None)
        self.assertIs(raised.exception, marker)
        self.assertEqual(controls[0].frames, [])
        self.assertIsNone(controls[0].target)
        self.assertIsNone(controls[0].domain)
        self.assertIs(stage.advance(), returned)


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
            value = guest(owner, [value_stage("x")], call("next", "x"), {"next": value})
            poly = guest(owner, [TYPE_STAGE], advance("next"), {"next": poly})
        self.assertEqual(value(41), 41)
        self.assertEqual(value(43), 43)
        self.assertEqual(poly.advance(), 37)
        self.assertEqual(poly.advance(), 37)

    def test_independent_artifacts_transfer_value_and_type_stages(self):
        foreign_log = []
        foreign = package_runtime(load_runtime(), foreign_log)
        literal = returned_atom("leaf", {"StrLit": {"host_type": STRING_IDENTITY, "value": "foreign"}})
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
        value = guest(self.runtime, [value_stage("x")], call("opaque", "x"), {"opaque": self.module.kio_lift(opaque)})
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
        value = guest(self.runtime, [value_stage("x")], call("host", "x"), {"host": self.module.kio_lift(host)})
        poly = guest(self.runtime, [TYPE_STAGE], advance("host"), {"host": self.module.kio_lift_stage(host)})
        self.assertEqual(value(53), 53)
        self.assertEqual(poly.advance(), 47)
        self.assertEqual(events, ["value", "type"])

    def test_guest_owner_and_residual_captures_survive_transfer(self):
        foreign_log = []
        foreign = package_runtime(self.module, foreign_log)
        literal = returned_atom("leaf", {"StrLit": {"host_type": STRING_IDENTITY, "value": "owner"}})
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
        staged_call = control(
            block(invocation("target", type_stage=True, resume=0)),
            block(invocation("argument", resume=1), input="selected"),
            block(invocation("selected", "value"), input="value"),
            resumes=[{"block": 1, "result": "selected", "live": ["argument"]},
                     {"block": 2, "result": "value", "live": ["selected"]}],
        )
        staged = guest(self.runtime, [value_stage()], staged_call, {"target": type_stage, "argument": argument})
        self.assertEqual(staged(None), 67)
        self.assertEqual(events, ["type", "argument", 67])
        twice = guest(self.runtime, [value_stage("callback")], control(
            block(invocation("callback", resume=0)),
            block(invocation("callback"), input="ignored"),
            resumes=[{"block": 1, "result": "ignored", "live": ["callback"]}],
        ))
        twice(callback)
        self.assertEqual(events[-2:], [None, None])
        identity = guest(self.runtime, [value_stage("x")], reference("x"))
        retained = guest(self.runtime, [value_stage("target")], returned_atom("closure", {"FnExpr":
            definition([value_stage("x", captures=("target",))], call("target", "x"))
        }, bindings=["target"]))(identity)
        self.assertEqual(retained(71), 71)
        self.assertEqual(retained(73), 73)
        reentrant = self.module.KioFn.lift(lambda value: identity(value + 1) + 1)
        reentry = guest(self.runtime, [value_stage("x")], call("host", "x"), {"host": reentrant})
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
        continuation = {"FnExpr": definition([value_stage("next", captures=("x",))], call("next", "x"))}
        for _ in range(2048):
            adapter = self.runtime._make_fn(None, entry, {"payload": value})
            value = guest(self.runtime, [value_stage("x")], control(block(
                invocation("adapter", "continuation"),
                [atom("closure", continuation, "continuation", bindings=["x"])],
            )), {"adapter": adapter})
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
        body = returned_atom("tuple", {"EnrichedTuple": {"items": [slot("kept"), slot("first")]}})
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
        outer = guest(self.runtime, [value_stage("saved", "unused")], returned_atom("closure", {"FnExpr": inner}, bindings=["saved"]))
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

    def test_executing_entry_rejects_an_unplanned_ast_body(self):
        fn = {"sig": {"stages": [value_stage()]}, "body": {"Unit": {}}}
        broken = self.runtime._make_fn("api", fn, {})
        with self.assertRaisesRegex(RuntimeError, "checked control"):
            broken(None)


if __name__ == "__main__":
    unittest.main()
