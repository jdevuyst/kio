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

class KioFn:
    __slots__ = ()

    @classmethod
    def __class_getitem__(cls, _arguments):
        return cls

    @staticmethod
    def lift(body):
        return _KioFunction(body)

    def __call__(self, domain):
        raise NotImplementedError("a host callable implements __call__")

class _KioTypeIndex:
    @classmethod
    def __class_getitem__(cls, _arguments):
        return cls


class KioApply1(_KioTypeIndex):
    def __new__(cls, *_arguments):
        raise TypeError("application values use their exact declaration operations")


def kio_lift(body):
    return KioFn.lift(body)


def kio_lift_stage(source):
    return _KioTypeStage.lift(source)


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
    if (not name["module"]
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
    for _ in range(index):
        value = value["right"]
    return value if index + 1 == arity else value["left"]


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
    __slots__ = ("__leaf",)

    def __init__(self, invoke):
        self.__leaf = invoke

    def __setattr__(self, name, value):
        if hasattr(self, name):
            raise AttributeError("callable carriers are immutable")
        object.__setattr__(self, name, value)

    def __call__(self, domain):
        return self.__leaf(domain)

class _KioPoly(_KioTypeStage):
    __slots__ = ("__leaf",)

    def __init__(self, invoke):
        self.__leaf = invoke

    def __setattr__(self, name, value):
        if hasattr(self, name):
            raise AttributeError("type-stage carriers are immutable")
        object.__setattr__(self, name, value)

    def advance(self):
        return self.__leaf()

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


class _CompiledPackage:
    def __init__(self, ir):
        self.ir = ir
        self.modules = {entry["key"]: entry["module"] for entry in ir["modules"]}
        self.boundary = ir["boundary"]
        self.sites = {
            _site_key(site["module"], site["owner"]): site
            for site in self.boundary["sites"]
        }
        self.public_newtypes = {
            _qname_key(entry["name"]): entry
            for entry in self.boundary["public_newtypes"]
        }
        self.nominal_newtypes = {}
        for module in self.modules.values():
            for item in module.get("items", []):
                tag, declaration = _tag(item)
                if tag == "Newtype":
                    name = declaration["nominal"]
                    self.nominal_newtypes[name["frame"]] = _make_nominal_carrier(
                        ".".join(name["module"] + [name["name"]])
                    )
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
                    execution["layout"], public_args[offset : offset + count]
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
            return result

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

            def apply(domain):
                source_args = _product_slots(domain, layout["body_abi_arity"])
                continuation = source_args[0]
                continuation_use = use["slots"][0]
                return self._apply_existential_continuation(
                    site, continuation_use, continuation, payload
                )

            return _KioFunction(apply)
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

            def invoke(domain):
                local = dict(env)
                if not is_type:
                    for slot, name in enumerate(names):
                        if slot + 1 == len(names):
                            local[name] = domain
                        else:
                            local[name] = domain["left"]
                            domain = domain["right"]
                if index + 1 < len(groups):
                    return layer(index + 1, local)
                return self.eval(module_key, fn_def["body"], local)

            if is_type:
                return _KioPoly(lambda: invoke(None))
            return _KioFunction(invoke)

        return layer(0, dict(base_env))

    def eval(self, module_key, expr, env):
        while True:
            tag, data = _tag(expr)
            if tag == "Let":
                value = self.eval(module_key, data["value"], env)
                local = dict(env)
                local[data["name"]] = value
                env = local
                expr = data["body"]
            elif tag == "Seq":
                self.eval(module_key, data["value"], env)
                expr = data["body"]
            else:
                break
        if tag == "Unit":
            return None
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
            fn = _apply_type_stages(fn, data["type_args"])
            args = [self.eval(module_key, arg, env) for arg in data["args"]]
            return _call_value(fn, *args)
        if tag == "LowModuleCall":
            fn = self.fn(data["module_path"], data["mangled"])
            fn = _apply_type_stages(fn, data["type_args"])
            if data["type_args"] and not _signature_has_value_group(data["sig"]):
                assert not data["args"], "a routed type-only head has no value arguments"
                return fn
            args = [self.eval(module_key, arg, env) for arg in data["args"]]
            return _call_value(fn, *args)
        if tag == "LowQualifiedModuleCall":
            fn = self.fn(data["module_path"], data["mangled"])
            fn = _apply_type_stages(fn, data["type_args"])
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
            callee = _apply_type_stages(env[data["name"]], data["type_args"])
            args = [self.eval(module_key, arg, env) for arg in data["args"]]
            return _call_value(callee, *args)
        if tag == "LowIndirectCall":
            callee = self.eval(module_key, data["callee"], env)
            callee = _apply_type_stages(callee, data["type_args"])
            args = [self.eval(module_key, arg, env) for arg in data["args"]]
            return _call_value(callee, *args)
        if tag == "LowTypeApplication":
            return self.eval(module_key, data["callee"], env).advance()
        if tag == "LowAbsurdCall":
            self.eval(module_key, data["value_arg"], env)
            raise RuntimeError("Kio: __absurd__ on bottom value")
        if tag == "LowCpsProjectorApply":
            receiver = self.eval(module_key, data["receiver"], env)
            payload = self._nominal_unwrap(data["nominal"], receiver)
            continuation = self.eval(module_key, data["continuation"], env)
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

    def _literal(self, data, value):
        return self._role_adapter(_role_adapter_identity(data["host_type"]), "out", value)

    def _nominal_wrap(self, name, payload):
        return self.compiled.nominal_newtypes[name["frame"]][1](payload)

    def _nominal_unwrap(self, name, value):
        return self.compiled.nominal_newtypes[name["frame"]][2](value)

    def _make_lambda(self, module_key, data, env):
        fn_def = {
            "name": "<lambda>", "sig": data["sig"], "body": data["body"],
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

            def invoke(domain):
                next_stages = list(stages)
                if execution["kind"] == "value":
                    args = _product_slots(domain, execution["layout"]["body_abi_arity"])
                    next_stages.append((semantic, execution["layout"], args))
                if index + 1 < len(site["execution"]["head"]):
                    return layer(index + 1, next_stages)
                return self._call_prepared_host(site, next_stages)

            if execution["kind"] == "type":
                return _KioPoly(lambda: invoke(None))
            return _KioFunction(invoke)

        return layer(0, [])

    def _call_prepared_host(self, site, stages):
        owner = site["owner"]
        module_path = "/".join(site["module"])
        fn = self._host_fn(owner["name"], module_path)
        public_args = []
        for semantic, layout, source_args in stages:
            public_args.extend(
                self._source_to_public(layout, source_args)
            )
        return fn(*public_args)

    def _layout_public_arity(self, layout):
        return sum(
            source["adapter"] != "unit_value"
            for source in layout["source_params"]
        )

    def _public_to_source(self, layout, public_args):
        source_args = []
        public_index = 0
        for source in layout["source_params"]:
            adapter = source["adapter"]
            if adapter == "unit_value":
                source_args.append(None)
            elif adapter in ("identity", "right_nest"):
                source_args.append(public_args[public_index])
                public_index += 1
            else:
                raise RuntimeError(f"unknown prepared source adapter: {adapter}")
        if public_index != len(public_args):
            raise RuntimeError("prepared source adapter did not consume its public arguments")
        return source_args

    def _source_to_public(self, layout, source_args):
        if len(source_args) != layout["source_param_count"]:
            raise RuntimeError("prepared source adapter received the wrong runtime arity")
        public_args = []
        for source, value in zip(layout["source_params"], source_args):
            adapter = source["adapter"]
            if adapter == "unit_value":
                continue
            if adapter in ("identity", "right_nest"):
                public_args.append(value)
                continue
            raise RuntimeError(f"unknown prepared source adapter: {adapter}")
        return public_args
