import types


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
        cur = [value, cur]
    return cur


def _product_slots(value, arity):
    slots = []
    cur = value
    for index in range(arity):
        if index + 1 == arity:
            slots.append(cur)
        else:
            slots.append(cur[0])
            cur = cur[1]
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
        cur = [0, cur]
    for _ in range(variant):
        cur = [1, cur]
    return cur


def _sum_payload(value, variants):
    if variants <= 0:
        raise IndexError("sum arity out of bounds")
    cur = value
    for index in range(variants):
        if index + 1 == variants:
            return index, cur
        if cur[0] == 0:
            return index, cur[1]
        cur = cur[1]
    raise IndexError("sum payload out of bounds")


def _looks_like_product(value):
    return isinstance(value, list) and len(value) == 2


def _boundary_argument(plan, binder, bindings):
    for scope_plan, arguments in reversed(bindings):
        if scope_plan is plan:
            return next((argument for key, argument in arguments if key == binder), None)
    return None


def _boundary_nominal_application(plan, use_id, execution, bindings):
    use = plan["uses"][use_id]
    if use["kind"] == "nominal":
        return use["name"], ()
    if use["kind"] == "bound":
        argument = _boundary_argument(plan, use["binder"], bindings)
        return _boundary_nominal_application(*argument) if argument is not None else None
    if use["kind"] == "apply":
        head = _boundary_nominal_application(plan, use["constructor"], execution, bindings)
        if head is not None:
            name, args = head
            return name, args + tuple(
                (plan, child, execution, bindings) for child in use["args"]
            )
    return None


class _KioFunction:
    def __init__(self, arity, invoke, label):
        self.arity = arity
        self._invoke = invoke
        self.label = label

    def __call__(self, *args):
        args = list(args)
        if len(args) == self.arity:
            return self._invoke(args)
        # Exact application is the norm: recover_to_low gives every value
        # group and every curried layer its own single-layer call node, so a
        # well-typed call arrives with exactly `arity` arguments. The one
        # exception is a positional-arity function value — a host-fn value or
        # a lambda whose value group has several parameters — reached through
        # a product-domain arrow slot, which passes the whole domain as one
        # product value (specs/backends/README.md § Function-type FFI
        # canonicalization). Spread that product across the parameter slots;
        # this is the runtime counterpart of the compiled backends' static
        # function-value slot adaptation.
        if self.arity > 1 and len(args) == 1 and _looks_like_product(args[0]):
            return self._invoke(_product_slots(args[0], self.arity))
        raise TypeError(f"{self.label} expected {self.arity} args, got {len(args)}")


def _call_value(fn, *args):
    if isinstance(fn, _KioFunction):
        return fn(*args)
    return fn(*args)


# Type values erase, but their application boundaries do not: each type
# binder is one nullary runtime stage, while each value group stays one call.
def _signature_runtime_groups(sig):
    params = sig.get("params", [])
    groups = sig.get("groups", [])
    out = []
    offset = 0
    has_value_group = False
    if groups:
        for group in groups:
            tag, data = _tag(group)
            length = data["len"]
            chunk = params[offset : offset + length]
            offset += length
            if tag == "Type":
                out.extend((True, []) for _ in range(length))
            elif tag == "Value":
                has_value_group = True
                out.append((False, [_tag(param)[1]["name"] for param in chunk]))
    else:
        names = []
        for param in params:
            tag, data = _tag(param)
            if tag == "Type":
                if names:
                    out.append((False, names))
                    has_value_group = True
                    names = []
                out.append((True, []))
            elif tag == "Value":
                names.append(data["name"])
        if names:
            out.append((False, names))
            has_value_group = True
    if not has_value_group:
        out.append((False, []))
    return out


def _apply_type_stages(fn, type_args):
    for _ in type_args:
        fn = _call_value(fn)
    return fn


def _make_nominal_carrier(label):
    seal = object()

    class Carrier:
        __slots__ = ("__payload",)

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

        def to_native(self):
            if type(self) is not Carrier:
                raise TypeError(f"expected {label}")
            return self.__native

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
        self.sites = {}
        for site in self.boundary["sites"]:
            site["_nominals"] = {
                nominal["name"]["frame"]: nominal
                for nominal in site["nominals"]
            }
            self.sites[_site_key(site["module"], site["owner"])] = site
        self.public_newtypes = {
            _qname_key(entry["name"]): entry
            for entry in self.boundary["public_newtypes"]
        }
        self.nominal_newtypes = {}
        for site in self.boundary["sites"]:
            for nominal in site["nominals"]:
                declaration = nominal["declaration"]
                if declaration["kind"] != "newtype":
                    continue
                surface = declaration["surface"]["kind"]
                nominal_shape = (
                    surface != "both"
                    or declaration["existential_params"]
                    or declaration.get("transparent_payload") is None
                )
                if nominal_shape:
                    frame = nominal["name"]["frame"]
                    self.nominal_newtypes.setdefault(
                        frame,
                        _make_nominal_carrier(
                            ".".join(nominal["name"]["module"] + [nominal["name"]["name"]])
                        ),
                    )
        self.host_type_carriers = {}
        for binding in self.boundary["host_bindings"]:
            if binding["type_params"]:
                public_name = "KioHostType_" + binding["name"]["frame"]
                self.host_type_carriers[public_name] = _make_host_type_carrier(public_name)

    def install_declarations(self, namespace):
        namespace.update(self.host_type_carriers)

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
            _namespace_set(root, path, self._direct_callable(site))
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
                        result = _call_value(result)
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
                result = values[0] if values else None
            elif owner["kind"] == "newtype_projector":
                payload = values[0] if values else None
                inventory = self.compiled.public_newtypes[
                    (tuple(site["module"]), owner["newtype"])
                ]
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

        return _KioFunction(expected, invoke, "public boundary")

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
            return _KioFunction(
                0,
                lambda _args: self._existential_projector_result(
                    site, use["result"], payload
                ),
                "newtype projector type stage",
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
                continuation = _call_value(continuation)
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

            arity = 0 if is_type else len(names)
            return _KioFunction(arity, invoke, f"{module_key}.{fn_def['name']}")

        return layer(0, dict(base_env))

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
            return data["value"]
        if tag == "IntLit":
            return int(data["digits"].replace("_", ""))
        if tag == "FloatLit":
            return float(data["digits"].replace("_", ""))
        if tag == "BoolLit":
            return data["value"]
        if tag == "EnrichedTuple":
            return _product_from_slots([self.eval(module_key, item, env) for item in data["items"]])
        if tag == "EnrichedRecord":
            return _product_from_slots(
                [self.eval(module_key, field["value"], env) for field in data["fields"]]
            )
        if tag in ("EnrichedProject", "EnrichedFieldGet"):
            return _product_slot(
                self.eval(module_key, data["target"], env), data["index"], data["arity"]
            )
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
            branch = data["then_branch"] if self.eval(module_key, data["cond"], env) else data["else_branch"]
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
            fn = _apply_type_stages(fn, data.get("type_args", []))
            args = [self.eval(module_key, arg, env) for arg in data["args"]]
            return _call_value(fn, *args)
        if tag == "LowModuleCall":
            fn = self.fn(data["module_path"], data["mangled"])
            fn = _apply_type_stages(fn, data["type_args"])
            args = [self.eval(module_key, arg, env) for arg in data["args"]]
            return _call_value(fn, *args)
        if tag == "LowQualifiedModuleCall":
            fn = self.fn(data["module_path"], data["mangled"])
            fn = _apply_type_stages(fn, data["type_args"])
            args = [self.eval(module_key, arg, env) for arg in data["args"]]
            return _call_value(fn, *args)
        if tag in ("LowQualifiedNewtypeMember", "LowNewtypeCtor", "LowNewtypeProj"):
            payload = data.get("payload", data.get("target"))
            return self.eval(module_key, payload, env)
        if tag == "LowClosureCall":
            callee = _apply_type_stages(env[data["name"]], data.get("type_args", []))
            args = [self.eval(module_key, arg, env) for arg in data["args"]]
            return _call_value(callee, *args)
        if tag == "LowIndirectCall":
            callee = self.eval(module_key, data["callee"], env)
            callee = _apply_type_stages(callee, data.get("type_args", []))
            args = [self.eval(module_key, arg, env) for arg in data["args"]]
            return _call_value(callee, *args)
        if tag == "LowTypeApplication":
            return _call_value(self.eval(module_key, data["callee"], env))
        if tag == "LowAbsurdCall":
            self.eval(module_key, data["value_arg"], env)
            raise RuntimeError("Kio: __absurd__ on bottom value")
        if tag == "LowCpsProjectorApply":
            receiver = self.eval(module_key, data["receiver"], env)
            continuation = self.eval(module_key, data["continuation"], env)
            continuation = _apply_type_stages(
                continuation,
                range(data["continuation_type_stages"]),
            )
            continuation_arity = data["continuation_abi_arity"]
            if continuation_arity == 0:
                return _call_value(continuation)
            if continuation_arity == 1:
                return _call_value(continuation, receiver)
            raise RuntimeError(
                "CPS projector continuation must have zero or one ABI argument, "
                f"got {continuation_arity}"
            )
        raise RuntimeError(f"python target cannot evaluate routed expression {tag}")

    def _make_lambda(self, module_key, data, env):
        fn_def = {"name": "<lambda>", "sig": data["sig"], "body": data["body"]}
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

            arity = 0 if execution["kind"] == "type" else execution["layout"]["body_abi_arity"]
            return _KioFunction(arity, invoke, f"host.{module_path}.{name}")

        return layer(0, [])

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
                shell = source["product_shell"]
                values = [
                    self._convert_use(
                        site,
                        plan,
                        use_id,
                        value[key],
                        "in",
                        execution_uses,
                        absorb_transparent=True,
                        bindings=bindings,
                    )
                    for key, use_id in zip(shell["keys"], slots[start:end])
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
                shell = source["product_shell"]
                values = _product_slots(value, end - start)
                public_args.append(
                    {
                        key: self._convert_use(
                            site,
                            plan,
                            use_id,
                            slot_value,
                            "out",
                            execution_uses,
                            absorb_transparent=True,
                            bindings=bindings,
                        )
                        for key, use_id, slot_value in zip(
                            shell["keys"], slots[start:end], values
                        )
                    }
                )
                continue
            raise RuntimeError(f"unknown prepared source adapter: {adapter}")
        return public_args

    def _convert_use(
        self,
        site,
        plan,
        use_id,
        value,
        direction,
        execution_uses,
        absorb_transparent=False,
        bindings=(),
    ):
        use = plan["uses"][use_id]
        kind = use["kind"]
        if kind in ("unit", "bottom"):
            return value
        if kind == "bound":
            argument = _boundary_argument(plan, use["binder"], bindings)
            if argument is None:
                return value
            argument_plan, argument_id, argument_execution, argument_bindings = argument
            return self._convert_use(
                site, argument_plan, argument_id, value, direction,
                argument_execution, bindings=argument_bindings,
            )
        if kind in ("nominal", "apply"):
            application = _boundary_nominal_application(plan, use_id, execution_uses, bindings)
            if application is not None:
                name, args = application
                original_head = use
                while original_head["kind"] == "apply":
                    original_head = plan["uses"][original_head["constructor"]]
                return self._convert_nominal(
                    site, name, value, direction,
                    absorb_transparent and original_head["kind"] == "nominal",
                    args,
                )
            return value
        if kind == "product":
            keys = use["shell"]["keys"]
            if direction == "out":
                values = _product_slots(value, len(use["args"]))
                return {
                    key: self._convert_use(
                        site,
                        plan,
                        child,
                        child_value,
                        "out",
                        execution_uses,
                        absorb_transparent=True,
                        bindings=bindings,
                    )
                    for key, child, child_value in zip(keys, use["args"], values)
                }
            return _product_from_slots(
                [
                    self._convert_use(
                        site,
                        plan,
                        child,
                        value[key],
                        "in",
                        execution_uses,
                        absorb_transparent=True,
                        bindings=bindings,
                    )
                    for key, child in zip(keys, use["args"])
                ]
            )
        if kind == "sum":
            keys = use["shell"]["keys"]
            if direction == "out":
                index, payload = _sum_payload(value, len(use["args"]))
                return {
                    keys[index]: self._convert_use(
                        site,
                        plan,
                        use["args"][index],
                        payload,
                        "out",
                        execution_uses,
                        absorb_transparent=True,
                        bindings=bindings,
                    )
                }
            for index, (key, child) in enumerate(zip(keys, use["args"])):
                if key in value:
                    payload = self._convert_use(
                        site,
                        plan,
                        child,
                        value[key],
                        "in",
                        execution_uses,
                        absorb_transparent=True,
                        bindings=bindings,
                    )
                    return _sum_inject(payload, index, len(use["args"]))
            raise RuntimeError("sum value has no recognized variant key")
        if kind == "function":
            execution = execution_uses[use_id]
            if execution["kind"] != "function":
                raise RuntimeError("prepared function has no execution layout")
            layout = execution["layout"]
            if direction == "out":
                arity = self._layout_public_arity(layout)

                def invoke(public_args):
                    source_args = self._public_to_source(
                        site,
                        plan,
                        use["slots"],
                        layout,
                        public_args,
                        execution_uses,
                        bindings,
                    )
                    result = _call_value(value, *source_args)
                    return self._convert_use(
                        site,
                        plan,
                        use["result"],
                        result,
                        "out",
                        execution_uses,
                        bindings=bindings,
                    )

                return _KioFunction(arity, invoke, "ffi function")

            def invoke(source_args):
                public_args = self._source_to_public(
                    site,
                    plan,
                    use["slots"],
                    layout,
                    source_args,
                    execution_uses,
                    bindings,
                )
                result = value(*public_args)
                return self._convert_use(
                    site,
                    plan,
                    use["result"],
                    result,
                    "in",
                    execution_uses,
                    bindings=bindings,
                )

            return _KioFunction(layout["body_abi_arity"], invoke, "ffi function")
        if kind == "forall":
            action = execution_uses[use_id]["kind"]
            if action == "declaration_binder":
                return self._convert_use(
                    site,
                    plan,
                    use["result"],
                    value,
                    direction,
                    execution_uses,
                    bindings=bindings,
                )
            if action != "invoke_forall":
                raise RuntimeError("prepared forall has no execution action")
            if direction == "out":
                value = _call_value(value)
                return self._convert_use(
                    site,
                    plan,
                    use["result"],
                    value,
                    "out",
                    execution_uses,
                    bindings=bindings,
                )
            return _KioFunction(
                0,
                lambda _args: self._convert_use(
                    site,
                    plan,
                    use["result"],
                    value,
                    "in",
                    execution_uses,
                    bindings=bindings,
                ),
                "ffi type stage",
            )
        raise RuntimeError(f"unknown prepared facade use: {kind}")

    def _convert_nominal(
        self, site, name, value, direction, absorb_transparent=False, args=()
    ):
        nominal = site["_nominals"].get(name["frame"])
        if nominal is None:
            raise RuntimeError(
                "prepared boundary omitted nominal dependency "
                + ".".join(name["module"] + [name["name"]])
            )
        declaration = nominal["declaration"]
        if declaration["kind"] == "host_type":
            if declaration["role"] is not None:
                return self._role_adapter(_role_adapter_identity(name), direction, value)
            return value
        payload = declaration.get("transparent_payload")
        nominal_shape = (
            declaration["surface"]["kind"] != "both"
            or declaration["existential_params"]
            or payload is None
        )
        if nominal_shape:
            _, wrap, unwrap = self.compiled.nominal_newtypes[name["frame"]]
            return wrap(value) if direction == "out" else unwrap(value)
        execution = site["execution"]["transparent_payloads"][name["frame"]]
        if len(args) != len(declaration["type_params"]):
            raise RuntimeError("prepared nominal application is not saturated")
        # Argument references retain their caller scopes. Escaping callable
        # adapters capture this immutable frame instead of a mutable binder stack.
        bindings = ((payload["facade"], tuple(zip(payload["declaration_binders"], args))),)
        if direction == "out":
            converted = self._convert_use(
                site,
                payload["facade"],
                payload["payload_root"],
                value,
                "out",
                execution,
                bindings=bindings,
            )
            return converted if absorb_transparent else {_host_name_core(name["name"]): converted}
        public_value = value if absorb_transparent else value[_host_name_core(name["name"])]
        return self._convert_use(
            site,
            payload["facade"],
            payload["payload_root"],
            public_value,
            "in",
            execution,
            bindings=bindings,
        )
