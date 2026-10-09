import types


class _KioTypeStage:
    __slots__ = ()

    @classmethod
    def __class_getitem__(cls, _arguments):
        return cls

    def advance(self):
        raise NotImplementedError("a polymorphic host value implements advance")

    @staticmethod
    def lift(source):
        return _KioPoly(source.advance)

    def adapt(self, after):
        return self.lift(self).adapt(after)


class KioFn:
    __slots__ = ()

    @classmethod
    def __class_getitem__(cls, _arguments):
        return cls

    @staticmethod
    def lift(body):
        return _KioFunction(1, lambda args: body(args[0]), "host callable")

    def __call__(self, domain):
        raise NotImplementedError("a host callable implements __call__")

    def adapt(self, before, after):
        return self.lift(self).adapt(before, after)


class _KioTypeIndex:
    @classmethod
    def __class_getitem__(cls, _arguments):
        return cls


class KioApply1(_KioTypeIndex):
    def __new__(cls, *_arguments):
        raise TypeError("application values use their exact declaration operations")


class _KioLayer:
    __slots__ = ("before", "after", "inner")

    def __init__(self, before, after, inner):
        self.before = before
        self.after = after
        self.inner = inner


def kio_lift(body):
    return KioFn.lift(body)


def kio_lift_stage(source):
    return _KioTypeStage.lift(source)


def _identity(value):
    return value


def _run_layers(leaf, layers, value):
    pending = []
    while layers is not None:
        value = layers.before(value)
        pending.append(layers.after)
        layers = layers.inner
    value = leaf(value)
    while pending:
        value = pending.pop()(value)
    return value


def _variant_name(value):
    if isinstance(value, str):
        return value
    if isinstance(value, dict) and len(value) == 1:
        return next(iter(value))
    raise TypeError(f"invalid enum value: {value!r}")


def _tag(value):
    if not isinstance(value, dict) or len(value) != 1:
        raise TypeError(f"invalid tagged value: {value!r}")
    return next(iter(value.items()))


def _seg_name(segment):
    return segment["name"] if isinstance(segment, dict) else segment


def _host_name_core(source):
    leading = len(source) - len(source.lstrip("_"))
    trailing = len(source) - len(source.rstrip("_"))
    words = source.strip("_").split("_")
    core = words[0] + "".join(word[:1].upper() + word[1:] for word in words[1:])
    return "_" * leading + core + "_" * trailing


def _encode_host_identity(source):
    source = "/".join(_host_name_core(part) for part in source.split("/"))
    return source.replace("_", "_u").replace("/", "_s")


def _host_module_key(path):
    if "_" not in path:
        return path.replace("/", "_")
    return "KioModule_" + _encode_host_identity(path)


def _module_facade_path(path):
    segments = path.split("/")
    return [_host_name_core(segments[0])] + [
        "KioModule_" + _encode_host_identity(segment)
        for segment in segments[1:]
    ]


def _type_facade_name(name):
    return "KioType_" + _encode_host_identity(name)

def _role_adapter_identity(name):
    components = [_host_name_core(component) for component in name["module"] + [name["name"]]]
    if (not name["module"] or name["module"][0] == "V1"
            or any(not component or not component.isascii()
                   or not component.isalnum() for component in components)):
        return name["frame"]
    return "_".join(components)


def _get_member(value, key, default=None):
    if isinstance(value, dict):
        return value.get(key, default)
    return getattr(value, key, default)


def _namespace_set(root, path, value):
    cur = root
    for segment in path[:-1]:
        next_value = _get_member(cur, segment)
        if next_value is None:
            next_value = types.SimpleNamespace()
            setattr(cur, segment, next_value)
        elif not isinstance(next_value, types.SimpleNamespace):
            raise RuntimeError(f"facade path crosses value leaf: {'.'.join(path)}")
        cur = next_value
    if _get_member(cur, path[-1]) is not None:
        raise RuntimeError(f"duplicate facade path: {'.'.join(path)}")
    setattr(cur, path[-1], value)


def _product_from_slots(slots):
    if not slots:
        return None
    cur = slots[-1]
    for value in reversed(slots[:-1]):
        cur = {"left": value, "right": cur}
    return cur


def _product_slots(value, arity):
    slots = []
    cur = value
    for index in range(arity):
        if index + 1 == arity:
            slots.append(cur)
        else:
            slots.append(cur["left"])
            cur = cur["right"]
    return slots


def _product_slot(value, index, arity):
    if index < 0 or index >= arity:
        raise IndexError("product slot out of bounds")
    return _product_slots(value, arity)[index]


def _sum_inject(payload, variant, variants):
    if variants <= 0 or variant < 0 or variant >= variants:
        raise IndexError("sum variant out of bounds")
    cur = payload
    if variant + 1 < variants:
        cur = {"tag": "left", "value": cur}
    for _ in range(variant):
        cur = {"tag": "right", "value": cur}
    return cur


def _sum_payload(value, variants):
    if variants <= 0:
        raise IndexError("sum arity out of bounds")
    cur = value
    for index in range(variants):
        if index + 1 == variants:
            return index, cur
        if cur["tag"] == "left":
            return index, cur["value"]
        cur = cur["value"]
    raise IndexError("sum payload out of bounds")


class _KioFunction(KioFn):
    __slots__ = ("__leaf", "__layers")

    def __init__(self, arity, invoke, label):
        del label
        self.__leaf = lambda domain: invoke(_product_slots(domain, arity))
        self.__layers = None

    def __setattr__(self, name, value):
        if hasattr(self, name):
            raise AttributeError("callable carriers are immutable")
        object.__setattr__(self, name, value)

    def __call__(self, domain):
        return _run_layers(self.__leaf, self.__layers, domain)

    def adapt(self, before, after):
        value = object.__new__(_KioFunction)
        value.__leaf = self.__leaf
        value.__layers = _KioLayer(before, after, self.__layers)
        return value


class _KioPoly(_KioTypeStage):
    __slots__ = ("__leaf", "__layers")

    def __init__(self, invoke):
        self.__leaf = lambda _unit: invoke()
        self.__layers = None

    def __setattr__(self, name, value):
        if hasattr(self, name):
            raise AttributeError("type-stage carriers are immutable")
        object.__setattr__(self, name, value)

    def advance(self):
        return _run_layers(self.__leaf, self.__layers, None)

    def adapt(self, after):
        value = object.__new__(_KioPoly)
        value.__leaf = self.__leaf
        value.__layers = _KioLayer(_identity, after, self.__layers)
        return value


class _KioNamedFunction:
    def __init__(self, arity, invoke, label):
        self.arity = arity
        self._invoke = invoke
        self.label = label

    def __call__(self, *args):
        if len(args) != self.arity:
            raise TypeError(f"{self.label} expected {self.arity} args, got {len(args)}")
        return self._invoke(list(args))


def _call_value(fn, *args):
    return fn(_product_from_slots(args))


# Type values erase, but their application boundaries do not: each type
# binder is one explicit advance, while each value group stays one unary call.
def _signature_runtime_groups(sig):
    return [
        (stage["kind"] == "type", stage.get("params", []))
        for stage in sig["stages"]
    ]


def _signature_has_value_group(sig):
    return any(stage["kind"] == "value" for stage in sig["stages"])


def _apply_type_stages(fn, type_args):
    for _ in type_args:
        fn = fn.advance()
    return fn


def _make_nominal_carrier(label):
    seal = object()

    class Carrier:
        __slots__ = ("__payload",)

        @classmethod
        def __class_getitem__(cls, _arguments):
            return cls

        def __new__(cls, permit, payload):
            if permit is not seal:
                raise TypeError(f"{label} is not directly constructible")
            value = super().__new__(cls)
            value.__payload = payload
            return value

        def _kio_unwrap(self, permit):
            if permit is not seal:
                raise TypeError(f"{label} payload is not directly accessible")
            return self.__payload

        def __repr__(self):
            return f"<Kio newtype {label}>"

    def wrap(payload):
        return Carrier(seal, payload)

    def unwrap(value):
        if type(value) is not Carrier:
            raise TypeError(f"expected {label}")
        return value._kio_unwrap(seal)

    return Carrier, wrap, unwrap


def _make_host_type_carrier(label):
    seal = object()

    class Carrier:
        __slots__ = ("__native",)

        def __new__(cls, permit, native):
            if permit is not seal:
                raise TypeError(f"{label} values use from_native")
            value = super().__new__(cls)
            value.__native = native
            return value

        @classmethod
        def __class_getitem__(cls, _args):
            return cls

        @classmethod
        def from_native(cls, value):
            return cls(seal, value)

        @staticmethod
        def to_native(value):
            if type(value) is not Carrier:
                raise TypeError(f"expected {label}")
            return value.__native

        def __repr__(self):
            return f"<{label}>"

    Carrier.__name__ = label
    Carrier.__qualname__ = label
    return Carrier


def _site_key(module, owner):
    kind = owner["kind"]
    if kind in ("host_function", "exported_function"):
        leaf = owner["name"]
    else:
        leaf = owner["newtype"] + "\0" + owner["member"]
    return (tuple(module), kind, leaf)


def _qname_key(name):
    return (tuple(name["module"]), name["name"])


def _value_application(plan, use_id, bindings):
    chunks = []
    while True:
        application = plan["uses"][use_id]["application"]
        chunks.append([
            (plan, argument, bindings) for argument in application["args"]
        ])
        head = application["head"]
        if head["kind"] == "nominal":
            arguments = []
            while chunks:
                arguments.extend(chunks.pop())
            return head["name"], arguments
        reference = bindings.get(head["binder"]) if bindings else None
        if reference is None:
            return None
        plan, use_id, bindings = reference


class _CompiledPackage:
    def __init__(self, ir):
        self.ir = ir
        self.modules = {entry["key"]: entry["module"] for entry in ir["modules"]}
        self.boundary = ir["boundary"]
        self.sites = {}
        for site in self.boundary["sites"]:
            site["callable"]["facade"]["_values"] = site["callable"]["values"]
            site["_nominals"] = {
                nominal["name"]["frame"]: nominal
                for nominal in site["nominals"]
            }
            for nominal in site["nominals"]:
                payload = nominal["declaration"].get("transparent_payload")
                if payload is not None:
                    payload["facade"]["_values"] = payload["values"]
            self.sites[_site_key(site["module"], site["owner"])] = site
        self.public_newtypes = {
            _qname_key(entry["name"]): entry
            for entry in self.boundary["public_newtypes"]
        }
        self.nominal_newtypes = {}
        self.nominal_declarations = {}
        for module in self.modules.values():
            for item in module.get("items", []):
                tag, declaration = _tag(item)
                if tag == "HostType":
                    self.nominal_declarations[declaration["nominal"]["frame"]] = {
                        "kind": "host_type"
                    }
                    continue
                if tag != "Newtype":
                    continue
                name = declaration["nominal"]
                frame = name["frame"]
                self.nominal_declarations[frame] = {
                    "kind": "newtype", "surface": {"kind": "opaque"},
                    "existential_params": [], "transparent_payload": None,
                }
                self.nominal_newtypes[frame] = _make_nominal_carrier(
                    ".".join(name["module"] + [name["name"]])
                )
        for site in self.boundary["sites"]:
            for nominal in site["nominals"]:
                declaration = nominal["declaration"]
                self.nominal_declarations[nominal["name"]["frame"]] = declaration
                if declaration["kind"] != "newtype":
                    continue
                frame = nominal["name"]["frame"]
                if frame not in self.nominal_newtypes:
                    raise RuntimeError("a live boundary names an absent newtype declaration")
        # Internal declarations grant exact identity checks only. Payload access
        # above comes solely from the prepared public boundary descriptors.
        self.value_site = {"_nominals": {
            frame: {"declaration": declaration}
            for frame, declaration in self.nominal_declarations.items()
        }}
        self.host_type_carriers = {}
        for binding in self.boundary["host_bindings"]:
            if binding["type_params"]:
                public_name = "KioHostType_" + binding["name"]["frame"]
                self.host_type_carriers[public_name] = _make_host_type_carrier(public_name)

    def install_declarations(self, namespace):
        namespace.update(self.host_type_carriers)
        for binding in self.boundary["host_bindings"]:
            if binding["type_params"]:
                marker = "KioHostMk_" + binding["name"]["frame"]
                namespace[marker] = type(marker, (_KioTypeIndex,), {})
        for declaration in self.boundary["public_newtypes"]:
            frame = declaration["name"]["frame"]
            namespace["KioNewtype_" + frame] = self.nominal_newtypes[frame][0]
            if declaration["type_params"]:
                marker = "KioNewtypeMk_" + frame
                namespace[marker] = type(marker, (_KioTypeIndex,), {})

    def create(self, host=None):
        return _PackageRuntime(self, host).exports()


class _PackageRuntime:
    def __init__(self, compiled, host):
        self.compiled = compiled
        self.host = host
        self.contexts = {}
        self.fn_cache = {}
        for key, module in compiled.modules.items():
            self.contexts[key] = self._build_context(module)
        self._validate_host()

    def _build_context(self, module):
        ctx = {
            "fns": {},
        }
        for item in module.get("items", []):
            tag, data = _tag(item)
            if tag == "FnDef":
                ctx["fns"][data["name"]] = data
        return ctx

    def _validate_host(self):
        missing = []
        for site in self.compiled.boundary["sites"]:
            owner = site["owner"]
            if owner["kind"] == "host_function":
                module_path = "/".join(site["module"])
                if self._host_fn(owner["name"], module_path, required=False) is None:
                    missing.append(f"{_host_module_key(module_path)}.{owner['name']}")
        for binding in self.compiled.boundary["host_bindings"]:
            if binding["role"] is None:
                continue
            identity = _role_adapter_identity(binding["name"])
            for prefix in ("KioHostIn_", "KioHostOut_"):
                name = prefix + identity
                if _get_member(self.host, name) is None:
                    missing.append(name)
        if missing:
            raise RuntimeError("missing host item: " + ", ".join(missing))

    def _host_fn(self, name, module_path, required=True):
        name = _host_name_core(name)
        ns = _host_module_key(module_path)
        nested = _get_member(self.host, ns)
        fn = _get_member(nested, name) if nested is not None else None
        if fn is None and required:
            raise RuntimeError(f"missing host item: {ns}.{name}")
        return fn

    def _role_adapter(self, identity, direction, value):
        name = ("KioHostIn_" if direction == "in" else "KioHostOut_") + identity
        adapter = _get_member(self.host, name)
        if adapter is None:
            raise RuntimeError(f"missing host item: {name}")
        return adapter(value)

    def exports(self):
        root = types.SimpleNamespace()
        for entry in self.compiled.boundary["public_newtypes"]:
            path = _module_facade_path("/".join(entry["name"]["module"]))
            path.append(_type_facade_name(entry["name"]["name"]))
            _namespace_set(root, path, types.SimpleNamespace())
        for site in self.compiled.boundary["sites"]:
            owner = site["owner"]
            if owner["kind"] == "host_function":
                continue
            path = _module_facade_path("/".join(site["module"]))
            if owner["kind"] == "exported_function":
                path.append(_host_name_core(owner["name"]))
            else:
                path.extend([_type_facade_name(owner["newtype"]), _host_name_core(owner["member"])])
            function = self._direct_callable(site)
            if "doc" in site:
                function.__doc__ = site["doc"]
            _namespace_set(root, path, function)
        return root

    def _direct_callable(self, site):
        expected = sum(
            self._layout_public_arity(stage["layout"])
            for stage in site["execution"]["head"]
            if stage["kind"] == "value"
        )

        def invoke(public_args):
            if len(public_args) != expected:
                owner = site["owner"]
                name = owner.get("name", owner.get("member", "boundary callable"))
                raise TypeError(f"{name} expected {expected} args, got {len(public_args)}")
            owner = site["owner"]
            if owner["kind"] == "exported_function":
                result = self.fn("/".join(site["module"]), owner["name"])
            else:
                result = None
            offset = 0
            values = []
            for semantic, execution in zip(
                site["callable"]["head"], site["execution"]["head"]
            ):
                if execution["kind"] == "type":
                    if owner["kind"] == "exported_function":
                        result = result.advance()
                    continue
                count = self._layout_public_arity(execution["layout"])
                source_args = self._public_to_source(
                    site,
                    site["callable"]["facade"],
                    semantic["slots"],
                    execution["layout"],
                    public_args[offset : offset + count],
                    site["execution"]["root_uses"],
                )
                offset += count
                if owner["kind"] == "exported_function":
                    result = _call_value(result, *source_args)
                else:
                    values.extend(source_args)
            if owner["kind"] == "newtype_constructor":
                name = self.compiled.public_newtypes[
                    (tuple(site["module"]), owner["newtype"])
                ]["name"]
                result = self._nominal_wrap(name, values[0] if values else None)
            elif owner["kind"] == "newtype_projector":
                inventory = self.compiled.public_newtypes[
                    (tuple(site["module"]), owner["newtype"])
                ]
                payload = self._nominal_unwrap(
                    inventory["name"], values[0] if values else None
                )
                if inventory["existential_params"]:
                    result = self._existential_projector_result(
                        site, site["callable"]["returned"], payload
                    )
                else:
                    result = payload
            return self._convert_use(
                site,
                site["callable"]["facade"],
                site["callable"]["returned"],
                result,
                "out",
                site["execution"]["root_uses"],
            )

        return _KioNamedFunction(expected, invoke, "public boundary")

    def _existential_projector_result(self, site, use_id, payload):
        use = site["callable"]["facade"]["uses"][use_id]
        if use["kind"] == "forall":
            action = site["execution"]["root_uses"][use_id]["kind"]
            if action == "declaration_binder":
                return self._existential_projector_result(
                    site, use["result"], payload
                )
            if action != "invoke_forall":
                raise RuntimeError("prepared forall has no execution action")
            return _KioPoly(
                lambda: self._existential_projector_result(
                    site, use["result"], payload
                ),
            )
        if use["kind"] == "function":
            layout = site["execution"]["root_uses"][use_id]["layout"]

            def apply(source_args):
                continuation = source_args[0]
                continuation_use = use["slots"][0]
                return self._apply_existential_continuation(
                    site, continuation_use, continuation, payload
                )

            return _KioFunction(layout["body_abi_arity"], apply, "newtype projector")
        raise RuntimeError("existential projector has no prepared continuation stage")

    def _apply_existential_continuation(
        self, site, use_id, continuation, payload
    ):
        plan = site["callable"]["facade"]
        execution_uses = site["execution"]["root_uses"]
        use = plan["uses"][use_id]
        while use["kind"] == "forall":
            action = execution_uses[use_id]["kind"]
            if action == "invoke_forall":
                continuation = continuation.advance()
            elif action != "declaration_binder":
                raise RuntimeError("prepared forall has no execution action")
            use_id = use["result"]
            use = plan["uses"][use_id]
        if use["kind"] != "function":
            raise RuntimeError("existential continuation has no function stage")
        execution = execution_uses[use_id]
        if execution["kind"] != "function":
            raise RuntimeError("prepared function has no execution layout")
        arity = execution["layout"]["body_abi_arity"]
        if arity == 0:
            return _call_value(continuation)
        if arity == 1:
            return _call_value(continuation, payload)
        raise RuntimeError(
            f"existential continuation must have zero or one ABI argument, got {arity}"
        )

    def fn(self, module_key, name):
        cache_key = (module_key, name)
        if cache_key in self.fn_cache:
            return self.fn_cache[cache_key]
        fn_def = self.contexts[module_key]["fns"][name]
        fn = self._make_fn(module_key, fn_def, {})
        self.fn_cache[cache_key] = fn
        return fn

    def _make_fn(self, module_key, fn_def, base_env):
        groups = _signature_runtime_groups(fn_def["sig"])

        def layer(index, env):
            is_type, names = groups[index]

            def invoke(args):
                local = dict(env)
                if not is_type:
                    for name, value in zip(names, args):
                        local[name] = value
                if index + 1 < len(groups):
                    return layer(index + 1, local)
                return self.eval(module_key, fn_def["body"], local)

            if is_type:
                return _KioPoly(lambda: invoke([]))
            return _KioFunction(len(names), invoke, f"{module_key}.{fn_def['name']}")

        return self._protect_applied_value(layer(0, dict(base_env)), fn_def["value_plan"])

    def eval(self, module_key, expr, env):
        tag, data = _tag(expr)
        if tag == "Unit":
            return None
        if tag == "Let":
            value = self.eval(module_key, data["value"], env)
            local = dict(env)
            local[data["name"]] = value
            return self.eval(module_key, data["body"], local)
        if tag == "Seq":
            self.eval(module_key, data["value"], env)
            return self.eval(module_key, data["body"], env)
        if tag == "FnExpr":
            return self._make_lambda(module_key, data, env)
        if tag == "StrLit":
            return self._literal(data, data["value"])
        if tag == "IntLit":
            return self._literal(data, int(data["digits"].replace("_", "")))
        if tag == "FloatLit":
            return self._literal(data, float(data["digits"].replace("_", "")))
        if tag == "BoolLit":
            return self._literal(data, data["value"])
        if tag == "EnrichedTuple":
            return _product_from_slots([self.eval(module_key, item, env) for item in data["items"]])
        if tag == "EnrichedProject":
            return _product_slot(
                self.eval(module_key, data["target"], env), data["index"], data["arity"]
            )
        if tag == "EnrichedFieldGet":
            carrier = _product_slot(
                self.eval(module_key, data["target"], env), data["index"], data["arity"]
            )
            return self._nominal_unwrap(data["nominal"], carrier)
        if tag == "EnrichedInject":
            return _sum_inject(
                self.eval(module_key, data["payload"], env), data["variant"], data["variants"]
            )
        if tag == "EnrichedMatch":
            variant, payload = _sum_payload(
                self.eval(module_key, data["scrutinee"], env), len(data["arms"])
            )
            arm = data["arms"][variant]
            local = dict(env)
            local[arm["param"]] = payload
            return self.eval(module_key, arm["body"], local)
        if tag == "EnrichedConditional":
            condition = self.eval(module_key, data["cond"], env)
            native = self._role_adapter(_role_adapter_identity(data["host_type"]), "in", condition)
            branch = data["then_branch"] if native else data["else_branch"]
            return self.eval(module_key, branch, env)
        if tag == "LowBoundRef":
            return env[data["name"]]
        if tag == "LowHostFnValueRef":
            return self._host_fn_value(data["name"], data["module_path"], data["sig"], data["ret_ty"])
        if tag == "LowModuleFnValueRef":
            return self.fn(data["module_path"], data["mangled"])
        if tag == "LowHostCall":
            fn = self._host_fn_value(
                data["name"], data["module_path"], data["sig"], data["ret_ty"]
            )
            fn = self._apply_planned_type_stages(fn, data)
            args = [self.eval(module_key, arg, env) for arg in data["args"]]
            return _call_value(fn, *args)
        if tag == "LowModuleCall":
            fn = self.fn(data["module_path"], data["mangled"])
            fn = self._apply_planned_type_stages(fn, data)
            if data["type_args"] and not _signature_has_value_group(data["sig"]):
                assert not data["args"], "a routed type-only head has no value arguments"
                return fn
            args = [self.eval(module_key, arg, env) for arg in data["args"]]
            return _call_value(fn, *args)
        if tag == "LowQualifiedModuleCall":
            fn = self.fn(data["module_path"], data["mangled"])
            fn = self._apply_planned_type_stages(fn, data)
            if data["type_args"] and not _signature_has_value_group(data["sig"]):
                assert not data["args"], "a routed type-only head has no value arguments"
                return fn
            args = [self.eval(module_key, arg, env) for arg in data["args"]]
            return _call_value(fn, *args)
        if tag in ("LowQualifiedNewtypeMember", "LowNewtypeCtor", "LowNewtypeProj"):
            payload = data.get("payload", data.get("target"))
            value = self.eval(module_key, payload, env)
            constructor = tag == "LowNewtypeCtor" or (
                tag == "LowQualifiedNewtypeMember" and data["constructor"]
            )
            operation = self._nominal_wrap if constructor else self._nominal_unwrap
            return operation(data["nominal"], value)
        if tag == "LowClosureCall":
            callee = self._apply_planned_type_stages(env[data["name"]], data)
            args = [self.eval(module_key, arg, env) for arg in data["args"]]
            return _call_value(callee, *args)
        if tag == "LowIndirectCall":
            callee = self.eval(module_key, data["callee"], env)
            callee = self._apply_planned_type_stages(callee, data)
            args = [self.eval(module_key, arg, env) for arg in data["args"]]
            return _call_value(callee, *args)
        if tag == "LowTypeApplication":
            value = self.eval(module_key, data["callee"], env).advance()
            return self._protect_applied_value(value, data["applied_plan"])
        if tag == "LowAbsurdCall":
            self.eval(module_key, data["value_arg"], env)
            raise RuntimeError("Kio: __absurd__ on bottom value")
        if tag == "LowCpsProjectorApply":
            receiver = self.eval(module_key, data["receiver"], env)
            payload = self._nominal_unwrap(data["nominal"], receiver)
            continuation = self.eval(module_key, data["continuation"], env)
            continuation = self._protect_applied_value(continuation, data["continuation_plan"])
            continuation = _apply_type_stages(
                continuation,
                range(data["continuation_type_stages"]),
            )
            continuation_arity = data["continuation_abi_arity"]
            if continuation_arity == 0:
                return _call_value(continuation)
            if continuation_arity == 1:
                return _call_value(continuation, payload)
            raise RuntimeError(
                "CPS projector continuation must have zero or one ABI argument, "
                f"got {continuation_arity}"
            )
        raise RuntimeError(f"python target cannot evaluate routed expression {tag}")

    def _apply_planned_type_stages(self, value, data):
        arguments = data.get("type_args", [])
        value = _apply_type_stages(value, arguments)
        if not arguments:
            return value
        return self._protect_applied_value(value, data["applied_plan"])

    def _protect_applied_value(self, value, plan_id):
        plan = self.compiled.ir["value_plans"][plan_id]
        return self._adapt_value(
            self.compiled.value_site, plan, plan["root"], value, "out"
        )

    def _literal(self, data, value):
        return self._role_adapter(_role_adapter_identity(data["host_type"]), "out", value)

    def _nominal_wrap(self, name, payload):
        declaration = self.compiled.nominal_declarations[name["frame"]]
        if declaration["kind"] != "newtype":
            raise RuntimeError("a routed newtype operation named a host type")
        return self.compiled.nominal_newtypes[name["frame"]][1](payload)

    def _nominal_unwrap(self, name, value):
        declaration = self.compiled.nominal_declarations[name["frame"]]
        if declaration["kind"] != "newtype":
            raise RuntimeError("a routed newtype operation named a host type")
        return self.compiled.nominal_newtypes[name["frame"]][2](value)

    def _make_lambda(self, module_key, data, env):
        fn_def = {
            "name": "<lambda>", "sig": data["sig"], "body": data["body"],
            "value_plan": data["value_plan"],
        }
        return self._make_fn(module_key, fn_def, env)

    def _host_fn_value(self, name, module_path, sig, ret_ty):
        del sig, ret_ty
        key = (tuple(module_path.split("/")), "host_function", name)
        site = self.compiled.sites.get(key)
        if site is None:
            raise RuntimeError(f"missing prepared host boundary: {module_path}.{name}")

        def layer(index, stages):
            semantic = site["callable"]["head"][index]
            execution = site["execution"]["head"][index]

            def invoke(args):
                next_stages = list(stages)
                if execution["kind"] == "value":
                    next_stages.append((semantic, execution["layout"], args))
                if index + 1 < len(site["execution"]["head"]):
                    return layer(index + 1, next_stages)
                return self._call_prepared_host(site, next_stages)

            if execution["kind"] == "type":
                return _KioPoly(lambda: invoke([]))
            return _KioFunction(execution["layout"]["body_abi_arity"], invoke, f"host.{module_path}.{name}")

        plan = site["callable"]["values"]
        return self._adapt_value(site, plan, plan["root"], layer(0, []), "out")

    def _call_prepared_host(self, site, stages):
        owner = site["owner"]
        module_path = "/".join(site["module"])
        fn = self._host_fn(owner["name"], module_path)
        public_args = []
        for semantic, layout, source_args in stages:
            public_args.extend(
                self._source_to_public(
                    site,
                    site["callable"]["facade"],
                    semantic["slots"],
                    layout,
                    source_args,
                    site["execution"]["root_uses"],
                )
            )
        result = fn(*public_args)
        return self._convert_use(
            site,
            site["callable"]["facade"],
            site["callable"]["returned"],
            result,
            "in",
            site["execution"]["root_uses"],
        )

    def _layout_public_arity(self, layout):
        return sum(
            source["adapter"] != "unit_value"
            for source in layout["source_params"]
        )

    def _public_to_source(
        self, site, plan, slots, layout, public_args, execution_uses, bindings=()
    ):
        source_args = []
        public_index = 0
        for source in layout["source_params"]:
            start = source["start"]
            end = source["end"]
            adapter = source["adapter"]
            if adapter == "unit_value":
                source_args.append(None)
            elif adapter == "identity":
                source_args.append(
                    self._convert_use(
                        site,
                        plan,
                        slots[start],
                        public_args[public_index],
                        "in",
                        execution_uses,
                        bindings=bindings,
                    )
                )
                public_index += 1
            elif adapter == "right_nest":
                value = public_args[public_index]
                public_index += 1
                original = _product_slots(value, end - start)
                values = [
                    self._convert_use(
                        site,
                        plan,
                        use_id,
                        slot_value,
                        "in",
                        execution_uses,
                        bindings=bindings,
                    )
                    for slot_value, use_id in zip(original, slots[start:end])
                ]
                source_args.append(_product_from_slots(values))
            else:
                raise RuntimeError(f"unknown prepared source adapter: {adapter}")
        if public_index != len(public_args):
            raise RuntimeError("prepared source adapter did not consume its public arguments")
        return source_args

    def _source_to_public(
        self, site, plan, slots, layout, source_args, execution_uses, bindings=()
    ):
        if len(source_args) != layout["source_param_count"]:
            raise RuntimeError("prepared source adapter received the wrong runtime arity")
        public_args = []
        for source, value in zip(layout["source_params"], source_args):
            start = source["start"]
            end = source["end"]
            adapter = source["adapter"]
            if adapter == "unit_value":
                continue
            if adapter == "identity":
                public_args.append(
                    self._convert_use(
                        site,
                        plan,
                        slots[start],
                        value,
                        "out",
                        execution_uses,
                        bindings=bindings,
                    )
                )
                continue
            if adapter == "right_nest":
                values = _product_slots(value, end - start)
                adapted = [self._convert_use(
                            site,
                            plan,
                            use_id,
                            slot_value,
                            "out",
                            execution_uses,
                            bindings=bindings,
                        )
                        for use_id, slot_value in zip(
                            slots[start:end], values
                        )
                ]
                public_args.append(
                    value if all(left is right for left, right in zip(values, adapted))
                    else _product_from_slots(adapted)
                )
                continue
            raise RuntimeError(f"unknown prepared source adapter: {adapter}")
        return public_args

    def _convert_use(
        self, site, plan, use_id, value, direction, execution_uses, bindings=()
    ):
        del execution_uses
        values = plan["_values"]
        return self._adapt_value(
            site, values, values["source_uses"][use_id], value, direction, bindings
        )

    def _nominal_payload_reference(self, declaration, arguments):
        payload = declaration["transparent_payload"]
        binders = payload["declaration_binders"]
        if len(arguments) != len(binders):
            raise RuntimeError("a value-level nominal application is not saturated")
        captured = dict(zip(binders, arguments))
        plan = payload["values"]
        return plan, plan["source_uses"][payload["payload_root"]], captured

    def _value_nominal(self, site, reference):
        application = _value_application(*reference)
        if application is None:
            return None
        name, arguments = application
        nominal = site["_nominals"].get(name["frame"])
        if nominal is None:
            raise RuntimeError("prepared boundary omitted an exact nominal dependency")
        return name, nominal["declaration"], arguments

    def _needs_protection(self, site, reference, direction):
        # The shared projection supplies the policy. This pass reads descriptors,
        # not host values, and recursive nominal dependencies conservatively act.
        pending = [("enter", reference, direction)]
        active = set()
        while pending:
            task = pending.pop()
            if task[0] == "leave":
                active.remove(task[1])
                continue
            _, current, flow = task
            plan, index, environment = current
            node = plan["protection"][index]
            kind = node["kind"]

            def child(use_id, direction=flow):
                return ("enter", (plan, use_id, environment), direction)

            if kind == "identity":
                continue
            if kind == "bound":
                bound = environment.get(node["binder"]) if environment else None
                if bound is not None:
                    pending.append(("enter", bound, flow))
            elif kind in ("product", "sum"):
                if node["snapshot"]:
                    raise RuntimeError("Python uses validation, not snapshot protection")
                pending.extend((child(node["right"]), child(node["left"])))
            elif kind == "function":
                opposite = "out" if flow == "in" else "in"
                pending.extend((child(node["result"]), child(node["domain"], opposite)))
            elif kind == "forall":
                pending.append(child(node["result"]))
            elif kind == "application":
                application = self._value_nominal(site, current)
                if application is None:
                    continue
                name, declaration, arguments = application
                if declaration["kind"] == "host_type":
                    continue
                # Every Python newtype is sealed, including both-public values.
                if flow == "in":
                    return True
                if declaration["surface"]["kind"] != "both" or declaration["existential_params"]:
                    continue
                key = (name["frame"], flow)
                if key in active:
                    return True
                active.add(key)
                pending.append(("leave", key))
                pending.append(("enter", self._nominal_payload_reference(declaration, arguments), flow))
            else:
                raise RuntimeError("unknown prepared protection operation: " + kind)
        return False

    def _adapt_value(self, site, plan, use_id, value, direction, bindings=()):
        reference = (plan, use_id, bindings)
        if not self._needs_protection(site, reference, direction):
            return value
        # One applicability pass precedes one value pass. Once protection is
        # needed, materialize concrete shells once; opaque leaves stay atomic.
        result = [None]
        pending = [("enter", reference, value, result, 0)]
        while pending:
            task = pending.pop()
            if task[0] == "nominal":
                _, original, carrier, payload, adapted, target, key = task
                target[key] = original if adapted[0] is payload else carrier[1](adapted[0])
                continue
            action, current, item, target, key = task
            if action == "read":
                item = item[0][item[1]]
            source, index, environment = current
            node = source["protection"][index]
            kind = node["kind"]

            def read(child, original, source_key, destination, destination_key):
                return ("read", (source, child, environment),
                        (original, source_key), destination, destination_key)

            if kind == "identity":
                target[key] = item
            elif kind == "bound":
                argument = environment.get(node["binder"]) if environment else None
                if argument is None:
                    target[key] = item
                else:
                    pending.append(("enter", argument, item, target, key))
            elif kind == "application":
                application = self._value_nominal(site, current)
                if application is None:
                    target[key] = item
                    continue
                name, declaration, arguments = application
                if declaration["kind"] == "host_type":
                    target[key] = item
                    continue
                carrier = self.compiled.nominal_newtypes[name["frame"]]
                if direction == "in" and type(item) is not carrier[0]:
                    raise TypeError("expected " + ".".join(name["module"] + [name["name"]]))
                if declaration["surface"]["kind"] != "both" or declaration["existential_params"]:
                    target[key] = item
                    continue
                contents = carrier[2](item)
                adapted = [None]
                pending.append(("nominal", item, carrier, contents, adapted, target, key))
                pending.append(("enter", self._nominal_payload_reference(declaration, arguments),
                                contents, adapted, 0))
            elif kind == "product":
                copy = {}
                target[key] = copy
                pending.append(read(node["right"], item, "right", copy, "right"))
                pending.append(read(node["left"], item, "left", copy, "left"))
            elif kind == "sum":
                tag = item["tag"]
                if tag not in ("left", "right"):
                    raise TypeError("a canonical sum tag is left or right")
                copy = {"tag": tag}
                target[key] = copy
                pending.append(read(node[tag], item, "value", copy, "value"))
            elif kind == "function":
                opposite = "in" if direction == "out" else "out"

                def before(argument, p=source, child=node["domain"], flow=opposite, env=environment):
                    return self._adapt_value(site, p, child, argument, flow, env)

                def after(output, p=source, child=node["result"], flow=direction, env=environment):
                    return self._adapt_value(site, p, child, output, flow, env)

                target[key] = item.adapt(before, after)
            elif kind == "forall":
                def after_stage(output, p=source, child=node["result"], flow=direction, env=environment):
                    return self._adapt_value(site, p, child, output, flow, env)

                target[key] = item.adapt(after_stage)
            else:
                raise RuntimeError("unknown prepared protection operation: " + kind)
        return result[0]
