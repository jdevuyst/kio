  @FunctionalInterface
  public interface KioFn {
    Object call(Object... args);
  }

  interface StageFactory {
    Object create(java.util.function.Supplier<Object> body);
    Object advance(Object value);
  }

  static long stageKey(int plan, int node) {
    return ((long) plan << 32) | Integer.toUnsignedLong(node);
  }

  public static final class KioObject {
    private final Map<String, Object> fields = new LinkedHashMap<>();

    public Object get(String key) {
      return fields.get(key);
    }

    public Object getOrDefault(String key, Object defaultValue) {
      return fields.getOrDefault(key, defaultValue);
    }

    public boolean has(String key) {
      return fields.containsKey(key);
    }

    public KioObject set(String key, Object value) {
      fields.put(key, value);
      return this;
    }

    public Map<String, Object> asMap() {
      return Collections.unmodifiableMap(fields);
    }

    @Override
    public String toString() {
      return fields.toString();
    }
  }

  private record Tag(String tag, Object data) {}

  private record SignatureStage(boolean type, List<String> names) {}

  private static String variantName(Object value) {
    if (value instanceof String s) {
      return s;
    }
    if (value instanceof Map<?, ?> m && m.size() == 1) {
      return String.valueOf(m.keySet().iterator().next());
    }
    throw new IllegalArgumentException("invalid enum value: " + value);
  }

  private static Tag tag(Object value) {
    if (!(value instanceof Map<?, ?> m) || m.size() != 1) {
      throw new IllegalArgumentException("invalid tagged value: " + value);
    }
    Map.Entry<?, ?> entry = m.entrySet().iterator().next();
    return new Tag(String.valueOf(entry.getKey()), entry.getValue());
  }

  private static String encodeSourceIdentity(String source) {
    return source.replace("_", "_u").replace("/", "_s");
  }

  private static String moduleNs(String path) {
    if (!path.contains("_")) {
      return path.replace("/", "_");
    }
    return "KioModule_" + encodeHostIdentity(path);
  }

  private static String hostNameCore(String source) {
    int leading = 0;
    int end = source.length();
    while (leading < end && source.charAt(leading) == '_') leading++;
    while (end > leading && source.charAt(end - 1) == '_') end--;
    StringBuilder rendered = new StringBuilder(source.substring(0, leading));
    boolean uppercase = false;
    for (int i = leading; i < end; i++) {
      char ch = source.charAt(i);
      if (ch == '_') {
        uppercase = true;
      } else {
        rendered.append(uppercase ? Character.toUpperCase(ch) : ch);
        uppercase = false;
      }
    }
    return rendered.append(source.substring(end)).toString();
  }

  private static String encodeHostIdentity(String source) {
    return encodeSourceIdentity(Arrays.stream(source.split("/"))
        .map(KioRuntime::hostNameCore).collect(java.util.stream.Collectors.joining("/")));
  }

  private static List<String> moduleFacadePath(String path) {
    List<String> source = new ArrayList<>(Arrays.asList(path.split("/")));
    source.set(0, hostNameCore(source.get(0)));
    for (int i = 1; i < source.size(); i++) {
      source.set(i, "KioModule_" + encodeHostIdentity(source.get(i)));
    }
    return source;
  }

  private static String typeFacadeName(String name) {
    return "KioType_" + encodeHostIdentity(name);
  }

  @SuppressWarnings("unchecked")
  private static Map<String, Object> asMap(Object value) {
    return (Map<String, Object>) value;
  }

  @SuppressWarnings("unchecked")
  private static List<Object> asList(Object value) {
    return (List<Object>) value;
  }

  private static List<Object> listOrEmpty(Object value) {
    if (value == null) {
      return List.of();
    }
    return asList(value);
  }

  private static String asString(Object value) {
    return (String) value;
  }

  private static int asInt(Object value) {
    return ((Number) value).intValue();
  }

  private static boolean asBool(Object value) {
    return (Boolean) value;
  }

  private static void namespaceSet(KioObject root, List<String> path, Object value) {
    KioObject cur = root;
    for (int i = 0; i + 1 < path.size(); i++) {
      String segment = path.get(i);
      Object next = cur.get(segment);
      if (next == null) {
        next = new KioObject();
        cur.set(segment, next);
      } else if (!(next instanceof KioObject)) {
        throw new IllegalStateException("facade path crosses a value leaf: " + path);
      }
      cur = (KioObject) next;
    }
    String leaf = path.get(path.size() - 1);
    if (cur.get(leaf) != null) {
      throw new IllegalStateException("duplicate facade path: " + path);
    }
    cur.set(leaf, value);
  }

  private static Object productFromSlots(List<Object> values) {
    if (values.isEmpty()) return new Shapes.Unit();
    Object value = values.get(values.size() - 1);
    for (int i = values.size() - 2; i >= 0; i--) {
      value = new Shapes.Product<>(values.get(i), value);
    }
    return value;
  }

  private static List<Object> productSlots(Object value, int count) {
    List<Object> values = new ArrayList<>(count);
    for (int i = 0; i < count; i++) {
      if (i + 1 == count) {
        values.add(value);
      } else {
        Shapes.Product<?, ?> pair = (Shapes.Product<?, ?>) value;
        values.add(pair.left());
        value = pair.right();
      }
    }
    return values;
  }

  private static Object productSlot(Object value, int index, int count) {
    if (index < 0 || index >= count) throw new IllegalStateException("invalid prepared product slot");
    for (int i = 0; i < index; i++) value = ((Shapes.Product<?, ?>) value).right();
    return index + 1 == count ? value : ((Shapes.Product<?, ?>) value).left();
  }

  private static Object sumInject(Object value, int index, int count) {
    if (index < 0 || index >= count) throw new IllegalStateException("invalid prepared sum arm");
    Object sum = index + 1 == count ? value : new Shapes.Left<>(value);
    for (int i = 0; i < index; i++) sum = new Shapes.Right<>(sum);
    return sum;
  }

  private record SumPayload(int index, Object payload) {}

  private static SumPayload sumPayload(Object value, int count) {
    for (int i = 0; i + 1 < count; i++) {
      if (value instanceof Shapes.Left<?, ?> left) return new SumPayload(i, left.value());
      value = ((Shapes.Right<?, ?>) value).value();
    }
    return new SumPayload(count - 1, value);
  }

  @SuppressWarnings("unchecked")
  private static Object applyValue(Object value, Object domain) {
    return ((Shapes.Fn1<Object, Object>) value).apply(domain);
  }

  private static List<SignatureStage> signatureRuntimeGroups(Map<String, Object> sig) {
    List<SignatureStage> out = new ArrayList<>();
    for (Object raw : asList(sig.get("stages"))) {
      Map<String, Object> stage = asMap(raw);
      if (asString(stage.get("kind")).equals("type")) {
        out.add(new SignatureStage(true, List.of()));
      } else {
        List<String> names = new ArrayList<>();
        for (Object name : asList(stage.get("params"))) names.add(asString(name));
        out.add(new SignatureStage(false, names));
      }
    }
    if (out.isEmpty()) throw new IllegalStateException("a prepared callable has a stage");
    return out;
  }

  private static boolean signatureHasValueGroup(Map<String, Object> sig) {
    return signatureRuntimeGroups(sig).stream().anyMatch(stage -> !stage.type());
  }

  private static boolean isExported(Object vis) {
    return variantName(vis).equals("Public");
  }

  private static String nominalKey(Object raw) {
    Map<String, Object> name = asMap(raw);
    return String.join("/", asList(name.get("module")).stream().map(KioRuntime::asString).toList())
        + "\u0000" + asString(name.get("name"));
  }

  interface NominalFactory {
    Object create(Object payload);
    Object project(Object value);
  }

  private record PrivateNominal(String identity, Object payload) {}

  private static final class CompiledPackage {
    private final Map<String, Map<String, Object>> modules = new LinkedHashMap<>();
    private final Set<String> bridged = new LinkedHashSet<>();
    private final Map<String, Map<String, Object>> newtypes = new LinkedHashMap<>();
    private final List<Object> plans;
    private final Map<Long, StageFactory> stageFactories = Shapes.stageFactories();
    private final Map<String, NominalFactory> nominalFactories = Shapes.nominalFactories();

    private CompiledPackage(Map<String, Object> ir) {
      plans = asList(ir.get("value_plans"));
      for (Object raw : asList(ir.get("modules"))) {
        Map<String, Object> entry = asMap(raw);
        modules.put(asString(entry.get("key")), asMap(entry.get("module")));
      }
      for (Object key : asList(ir.get("bridged"))) bridged.add(asString(key));
      for (Map<String, Object> module : modules.values()) {
        for (Object raw : listOrEmpty(module.get("items"))) {
          Tag item = tag(raw);
          if (item.tag().equals("Newtype")) {
            Map<String, Object> data = asMap(item.data());
            newtypes.put(nominalKey(data.get("nominal")), data);
          }
        }
      }
    }

    private KioObject create(Object host) {
      return new PackageRuntime(this, host).exports();
    }

    private PlanRef plan(Object index) {
      int plan = asInt(index);
      return new PlanRef(plan, asInt(asMap(plans.get(plan)).get("root")));
    }

    private Map<String, Object> node(PlanRef ref) {
      return asMap(asList(asMap(plans.get(ref.plan())).get("uses")).get(ref.node()));
    }

    private PlanRef child(PlanRef ref, String key) {
      return new PlanRef(ref.plan(), asInt(node(ref).get(key)));
    }

    private StageFactory stage(PlanRef ref) {
      StageFactory stage = stageFactories.get(stageKey(ref.plan(), ref.node()));
      if (stage == null) throw new IllegalStateException("missing prepared forall factory " + ref);
      return stage;
    }

    private Object construct(Object name, Object payload) {
      String key = nominalKey(name);
      Map<String, Object> declaration = newtypes.get(key);
      if (declaration == null) throw new IllegalStateException("unknown prepared newtype " + key);
      NominalFactory factory = nominalFactories.get(key);
      if (factory != null) return factory.create(payload);
      if (!asList(declaration.get("type_params")).isEmpty()) {
        return Shapes.createAppliedNominal(key, payload);
      }
      return new PrivateNominal(key, payload);
    }

    private Object project(Object name, Object value) {
      String key = nominalKey(name);
      Map<String, Object> declaration = newtypes.get(key);
      if (declaration == null) throw new IllegalStateException("unknown prepared newtype " + key);
      NominalFactory factory = nominalFactories.get(key);
      if (factory != null) return factory.project(value);
      if (!asList(declaration.get("type_params")).isEmpty()) {
        return Shapes.projectAppliedNominal(key, value);
      }
      PrivateNominal nominal = (PrivateNominal) value;
      if (!nominal.identity().equals(key)) throw new IllegalStateException("wrong private newtype identity");
      return nominal.payload();
    }
  }

  private record PlanRef(int plan, int node) {}
  private record Applied(Object value, PlanRef plan) {}

  private static final class ModuleContext {
    private final Map<String, Map<String, Object>> fns = new LinkedHashMap<>();
  }

  private static final class PackageRuntime {
    private final CompiledPackage compiled;
    private final KioObject host;
    private final Map<String, ModuleContext> contexts = new LinkedHashMap<>();
    private final Map<String, Object> fnCache = new HashMap<>();

    private PackageRuntime(CompiledPackage compiled, Object host) {
      this.compiled = compiled;
      this.host = host == null ? new KioObject() : (KioObject) host;
      for (Map.Entry<String, Map<String, Object>> entry : compiled.modules.entrySet()) {
        ModuleContext context = new ModuleContext();
        for (Object raw : listOrEmpty(entry.getValue().get("items"))) {
          Tag item = tag(raw);
          if (item.tag().equals("FnDef")) {
            Map<String, Object> data = asMap(item.data());
            context.fns.put(asString(data.get("name")), data);
          }
        }
        contexts.put(entry.getKey(), context);
      }
      for (String key : compiled.bridged) {
        Map<String, Object> module = compiled.modules.get(key);
        if (module == null) continue;
        for (Object raw : listOrEmpty(module.get("items"))) {
          Tag item = tag(raw);
          if (item.tag().equals("HostFn")) hostFn(asString(asMap(item.data()).get("name")), key);
        }
      }
    }

    private KioFn hostFn(String name, String modulePath) {
      Object nested = host.get(moduleNs(modulePath));
      Object fn = nested instanceof KioObject ns ? ns.get(name) : null;
      if (!(fn instanceof KioFn)) {
        throw new IllegalStateException("missing host item: " + modulePath + "." + name);
      }
      return (KioFn) fn;
    }

    private Object role(Object name, Object value, String direction) {
      Object raw = host.get(direction);
      if (!(raw instanceof KioObject adapters)) {
        throw new IllegalStateException("missing exact host-role adapter table");
      }
      Object adapter = adapters.get(nominalKey(name));
      if (!(adapter instanceof KioFn fn)) {
        throw new IllegalStateException("missing exact host-role adapter " + nominalKey(name));
      }
      return fn.call(value);
    }

    private Applied advance(Object value, PlanRef plan, int count) {
      for (int i = 0; i < count; i++) {
        value = compiled.stage(plan).advance(value);
        plan = compiled.child(plan, "result");
      }
      return new Applied(value, plan);
    }

    private KioObject exports() {
      KioObject root = new KioObject();
      List<String> keys = new ArrayList<>(compiled.bridged);
      Collections.sort(keys);
      for (String key : keys) {
        Map<String, Object> module = compiled.modules.get(key);
        if (module == null) continue;
        List<String> prefix = key.isEmpty() ? new ArrayList<>() : moduleFacadePath(key);
        for (Object raw : listOrEmpty(module.get("items"))) {
          Tag item = tag(raw);
          Map<String, Object> data = asMap(item.data());
          if (item.tag().equals("FnDef") && isExported(data.get("vis"))) {
            List<String> path = new ArrayList<>(prefix);
            path.add(asString(data.get("name")));
            namespaceSet(root, path, exportFn(key, data));
          } else if (item.tag().equals("Newtype") && isExported(data.get("vis"))) {
            List<String> path = new ArrayList<>(prefix);
            path.add(typeFacadeName(asString(data.get("name"))));
            namespaceSet(root, path, exportNewtype(data));
          }
        }
      }
      return root;
    }

    private KioFn exportFn(String module, Map<String, Object> def) {
      List<SignatureStage> stages = signatureRuntimeGroups(asMap(def.get("sig")));
      return args -> {
        Object value = fn(module, asString(def.get("name")));
        PlanRef plan = compiled.plan(def.get("value_plan"));
        int offset = 0;
        for (SignatureStage stage : stages) {
          if (stage.type()) {
            value = compiled.stage(plan).advance(value);
          } else {
            int count = stage.names().size();
            value = applyValue(value, productFromSlots(Arrays.asList(args).subList(offset, offset + count)));
            offset += count;
          }
          plan = compiled.child(plan, "result");
        }
        if (offset != args.length) throw new IllegalStateException("prepared export argument count drift");
        return value;
      };
    }

    private KioObject exportNewtype(Map<String, Object> data) {
      KioObject result = new KioObject();
      Map<String, Object> ctor = asMap(data.get("constructor"));
      Map<String, Object> proj = asMap(data.get("projector"));
      result.set(asString(ctor.get("name")), (KioFn) args ->
          compiled.construct(data.get("nominal"), productFromSlots(Arrays.asList(args))));
      result.set(asString(proj.get("name")), (KioFn) args -> {
        if (args.length != 1) throw new IllegalStateException("newtype projector takes one receiver");
        Object payload = compiled.project(data.get("nominal"), args[0]);
        if (!asBool(data.get("has_existentials"))) return payload;
        PlanRef plan = compiled.plan(proj.get("value_plan"));
        while (asString(compiled.node(plan).get("kind")).equals("forall")) {
          plan = compiled.child(plan, "result");
        }
        return existentialResult(compiled.child(plan, "result"), payload);
      });
      return result;
    }

    private Object existentialResult(PlanRef plan, Object payload) {
      Map<String, Object> node = compiled.node(plan);
      if (asString(node.get("kind")).equals("forall")) {
        return compiled.stage(plan).create(() -> existentialResult(compiled.child(plan, "result"), payload));
      }
      if (!asString(node.get("kind")).equals("function")) {
        throw new IllegalStateException("prepared existential projector result is callable");
      }
      PlanRef continuation = compiled.child(plan, "domain");
      return (Shapes.Fn1<Object, Object>) value -> {
        PlanRef current = continuation;
        while (asString(compiled.node(current).get("kind")).equals("forall")) {
          value = compiled.stage(current).advance(value);
          current = compiled.child(current, "result");
        }
        return applyValue(value, payload);
      };
    }

    private Object fn(String module, String name) {
      String key = module + "\u0000" + name;
      Object cached = fnCache.get(key);
      if (cached != null) return cached;
      Map<String, Object> def = contexts.get(module).fns.get(name);
      if (def == null) throw new IllegalStateException("missing routed function " + key);
      Object value = makeFn(module, def, Map.of());
      fnCache.put(key, value);
      return value;
    }

    private Object makeFn(String module, Map<String, Object> def, Map<String, Object> env) {
      return layer(module, def, signatureRuntimeGroups(asMap(def.get("sig"))), 0,
          new LinkedHashMap<>(env), compiled.plan(def.get("value_plan")));
    }

    private Object layer(String module, Map<String, Object> def, List<SignatureStage> stages,
        int index, Map<String, Object> env, PlanRef plan) {
      SignatureStage stage = stages.get(index);
      if (stage.type()) {
        return compiled.stage(plan).create(() ->
            nextLayer(module, def, stages, index, env, compiled.child(plan, "result")));
      }
      return (Shapes.Fn1<Object, Object>) domain -> {
        Map<String, Object> local = new LinkedHashMap<>(env);
        List<Object> values = productSlots(domain, stage.names().size());
        for (int i = 0; i < values.size(); i++) local.put(stage.names().get(i), values.get(i));
        return nextLayer(module, def, stages, index, local, compiled.child(plan, "result"));
      };
    }

    private Object nextLayer(String module, Map<String, Object> def, List<SignatureStage> stages,
        int index, Map<String, Object> env, PlanRef result) {
      return index + 1 == stages.size() ? eval(module, def.get("body"), env)
          : layer(module, def, stages, index + 1, env, result);
    }

    private Object hostFnValue(String name, String module, Map<String, Object> sig, Object plan) {
      return hostLayer(name, module, signatureRuntimeGroups(sig), 0, List.of(), compiled.plan(plan));
    }

    private Object hostLayer(String name, String module, List<SignatureStage> stages,
        int index, List<Object> values, PlanRef plan) {
      SignatureStage stage = stages.get(index);
      if (stage.type()) {
        return compiled.stage(plan).create(() ->
            nextHostLayer(name, module, stages, index, values, compiled.child(plan, "result")));
      }
      return (Shapes.Fn1<Object, Object>) domain -> {
        List<Object> all = new ArrayList<>(values);
        all.addAll(productSlots(domain, stage.names().size()));
        return nextHostLayer(name, module, stages, index, all, compiled.child(plan, "result"));
      };
    }

    private Object nextHostLayer(String name, String module, List<SignatureStage> stages,
        int index, List<Object> values, PlanRef result) {
      return index + 1 == stages.size() ? hostFn(name, module).call(values.toArray())
          : hostLayer(name, module, stages, index + 1, values, result);
    }

    private Object call(String module, Map<String, Object> data, Map<String, Object> env,
        Object callee, boolean named) {
      List<Object> types = listOrEmpty(data.get("type_args"));
      if (!types.isEmpty()) {
        callee = advance(callee, compiled.plan(data.get("callee_plan")), types.size()).value();
      }
      List<Object> arguments = new ArrayList<>();
      for (Object argument : asList(data.get("args"))) arguments.add(eval(module, argument, env));
      if (named && !types.isEmpty() && !signatureHasValueGroup(asMap(data.get("sig")))) {
        if (!arguments.isEmpty()) throw new IllegalStateException("type-only call has value arguments");
        return callee;
      }
      return applyValue(callee, productFromSlots(arguments));
    }

    private Object eval(String module, Object expression, Map<String, Object> env) {
      Tag tagged = tag(expression);
      Map<String, Object> data = tagged.data() instanceof Map<?, ?> ? asMap(tagged.data()) : Map.of();
      switch (tagged.tag()) {
        case "Unit": return new Shapes.Unit();
        case "Let": {
          Object value = eval(module, data.get("value"), env);
          Map<String, Object> local = new LinkedHashMap<>(env);
          local.put(asString(data.get("name")), value);
          return eval(module, data.get("body"), local);
        }
        case "Seq":
          eval(module, data.get("value"), env);
          return eval(module, data.get("body"), env);
        case "FnExpr": return makeFn(module, data, env);
        case "StrLit": return role(data.get("host_type"), data.get("value"), "__fromRole");
        case "IntLit": return role(data.get("host_type"),
            new BigInteger(asString(data.get("digits")).replace("_", "")), "__fromRole");
        case "FloatLit": return role(data.get("host_type"),
            Double.parseDouble(asString(data.get("digits")).replace("_", "")), "__fromRole");
        case "BoolLit": return role(data.get("host_type"), data.get("value"), "__fromRole");
        case "EnrichedTuple": {
          List<Object> values = new ArrayList<>();
          for (Object item : asList(data.get("items"))) values.add(eval(module, item, env));
          return productFromSlots(values);
        }
        case "EnrichedProject":
          return productSlot(eval(module, data.get("target"), env),
              asInt(data.get("index")), asInt(data.get("arity")));
        case "EnrichedFieldGet":
          return compiled.project(data.get("nominal"),
              productSlot(eval(module, data.get("target"), env),
                  asInt(data.get("index")), asInt(data.get("arity"))));
        case "EnrichedInject":
          return sumInject(eval(module, data.get("payload"), env),
              asInt(data.get("variant")), asInt(data.get("variants")));
        case "EnrichedMatch": {
          List<Object> arms = asList(data.get("arms"));
          SumPayload value = sumPayload(eval(module, data.get("scrutinee"), env), arms.size());
          Map<String, Object> arm = asMap(arms.get(value.index()));
          Map<String, Object> local = new LinkedHashMap<>(env);
          local.put(asString(arm.get("param")), value.payload());
          return eval(module, arm.get("body"), local);
        }
        case "EnrichedConditional":
          return eval(module, asBool(role(data.get("host_type"),
              eval(module, data.get("cond"), env), "__toRole"))
              ? data.get("then_branch") : data.get("else_branch"), env);
        case "LowBoundRef": return env.get(asString(data.get("name")));
        case "LowHostFnValueRef":
          return hostFnValue(asString(data.get("name")), asString(data.get("module_path")),
              asMap(data.get("sig")), data.get("value_plan"));
        case "LowModuleFnValueRef":
          return fn(asString(data.get("module_path")), asString(data.get("mangled")));
        case "LowHostCall":
          return call(module, data, env, hostFnValue(asString(data.get("name")),
              asString(data.get("module_path")), asMap(data.get("sig")), data.get("callee_plan")), true);
        case "LowModuleCall":
        case "LowQualifiedModuleCall":
          return call(module, data, env,
              fn(asString(data.get("module_path")), asString(data.get("mangled"))), true);
        case "LowClosureCall":
          return call(module, data, env, env.get(asString(data.get("name"))), false);
        case "LowIndirectCall":
          return call(module, data, env, eval(module, data.get("callee"), env), false);
        case "LowTypeApplication":
          return advance(eval(module, data.get("callee"), env),
              compiled.plan(data.get("callee_plan")), 1).value();
        case "LowNewtypeCtor":
          return compiled.construct(data.get("nominal"), eval(module, data.get("payload"), env));
        case "LowNewtypeProj":
          return compiled.project(data.get("nominal"), eval(module, data.get("target"), env));
        case "LowQualifiedNewtypeMember": {
          Object payload = eval(module, data.get("payload"), env);
          return asBool(data.get("constructor")) ? compiled.construct(data.get("nominal"), payload)
              : compiled.project(data.get("nominal"), payload);
        }
        case "LowAbsurdCall":
          eval(module, data.get("value_arg"), env);
          throw new IllegalStateException("Kio: __absurd__ on bottom value");
        case "LowCpsProjectorApply": {
          Object payload = compiled.project(data.get("nominal"), eval(module, data.get("receiver"), env));
          Object continuation = eval(module, data.get("continuation"), env);
          int count = asInt(data.get("continuation_type_stages"));
          if (count != 0) continuation = advance(continuation,
              compiled.plan(data.get("continuation_plan")), count).value();
          int arity = asInt(data.get("continuation_abi_arity"));
          if (arity != 0 && arity != 1) throw new IllegalStateException("invalid prepared CPS arity");
          return applyValue(continuation, arity == 0 ? new Shapes.Unit() : payload);
        }
        default: throw new IllegalStateException("java target cannot evaluate routed expression " + tagged.tag());
      }
    }
  }

  private static final class Json {
    private final String input;
    private int index = 0;

    private Json(String input) {
      this.input = input;
    }

    private static Map<String, Object> parseObject(String input) {
      Json parser = new Json(input);
      Object value = parser.parseValue();
      parser.skipWhitespace();
      if (parser.index != parser.input.length()) {
        throw new IllegalArgumentException("trailing data in JSON input");
      }
      return asMap(value);
    }

    private Object parseValue() {
      skipWhitespace();
      if (index >= input.length()) {
        throw new IllegalArgumentException("unexpected end of JSON input");
      }
      char ch = input.charAt(index);
      if (ch == '{') {
        return parseMap();
      }
      if (ch == '[') {
        return parseArray();
      }
      if (ch == '"') {
        return parseString();
      }
      if (input.startsWith("true", index)) {
        index += 4;
        return Boolean.TRUE;
      }
      if (input.startsWith("false", index)) {
        index += 5;
        return Boolean.FALSE;
      }
      if (input.startsWith("null", index)) {
        index += 4;
        return null;
      }
      return parseNumber();
    }

    private Map<String, Object> parseMap() {
      expect('{');
      Map<String, Object> out = new LinkedHashMap<>();
      skipWhitespace();
      if (peek('}')) {
        index++;
        return out;
      }
      while (true) {
        String key = parseString();
        skipWhitespace();
        expect(':');
        out.put(key, parseValue());
        skipWhitespace();
        if (peek('}')) {
          index++;
          return out;
        }
        expect(',');
      }
    }

    private List<Object> parseArray() {
      expect('[');
      List<Object> out = new ArrayList<>();
      skipWhitespace();
      if (peek(']')) {
        index++;
        return out;
      }
      while (true) {
        out.add(parseValue());
        skipWhitespace();
        if (peek(']')) {
          index++;
          return out;
        }
        expect(',');
      }
    }

    private String parseString() {
      expect('"');
      StringBuilder out = new StringBuilder();
      while (index < input.length()) {
        char ch = input.charAt(index++);
        if (ch == '"') {
          return out.toString();
        }
        if (ch != '\\') {
          out.append(ch);
          continue;
        }
        if (index >= input.length()) {
          throw new IllegalArgumentException("unterminated JSON escape");
        }
        char escaped = input.charAt(index++);
        switch (escaped) {
          case '"':
          case '\\':
          case '/':
            out.append(escaped);
            break;
          case 'b':
            out.append('\b');
            break;
          case 'f':
            out.append('\f');
            break;
          case 'n':
            out.append('\n');
            break;
          case 'r':
            out.append('\r');
            break;
          case 't':
            out.append('\t');
            break;
          case 'u':
            if (index + 4 > input.length()) {
              throw new IllegalArgumentException("short JSON unicode escape");
            }
            out.append((char) Integer.parseInt(input.substring(index, index + 4), 16));
            index += 4;
            break;
          default:
            throw new IllegalArgumentException("invalid JSON escape: \\" + escaped);
        }
      }
      throw new IllegalArgumentException("unterminated JSON string");
    }

    private Object parseNumber() {
      int start = index;
      if (peek('-')) {
        index++;
      }
      while (index < input.length() && Character.isDigit(input.charAt(index))) {
        index++;
      }
      boolean floating = false;
      if (peek('.')) {
        floating = true;
        index++;
        while (index < input.length() && Character.isDigit(input.charAt(index))) {
          index++;
        }
      }
      if (index < input.length() && (input.charAt(index) == 'e' || input.charAt(index) == 'E')) {
        floating = true;
        index++;
        if (index < input.length() && (input.charAt(index) == '+' || input.charAt(index) == '-')) {
          index++;
        }
        while (index < input.length() && Character.isDigit(input.charAt(index))) {
          index++;
        }
      }
      String digits = input.substring(start, index);
      return floating ? Double.parseDouble(digits) : Long.parseLong(digits);
    }

    private void skipWhitespace() {
      while (index < input.length() && Character.isWhitespace(input.charAt(index))) {
        index++;
      }
    }

    private boolean peek(char expected) {
      return index < input.length() && input.charAt(index) == expected;
    }

    private void expect(char expected) {
      skipWhitespace();
      if (!peek(expected)) {
        throw new IllegalArgumentException("expected '" + expected + "' in JSON input");
      }
      index++;
    }
  }
