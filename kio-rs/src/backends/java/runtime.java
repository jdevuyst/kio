  @FunctionalInterface
  public interface KioFn {
    Object call(Object... args);
  }

  interface StageFactory {
    Object create(java.util.function.Supplier<Object> body);
    Object advance(Object value);
  }

  interface GuestStage {
    java.util.function.Supplier<Object> body();
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

  private record SignatureStage(boolean type, List<String> names, List<String> captures) {}

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
      List<String> captures = stage.containsKey("captures")
          ? asList(stage.get("captures")).stream().map(KioRuntime::asString).toList() : null;
      if (asString(stage.get("kind")).equals("type")) {
        out.add(new SignatureStage(true, List.of(), captures));
      } else {
        List<String> names = new ArrayList<>();
        for (Object name : asList(stage.get("params"))) names.add(asString(name));
        out.add(new SignatureStage(false, names, captures));
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
      if (index instanceof PlanRef ref) return ref;
      if (index instanceof Map<?, ?>) {
        Map<String, Object> selected = asMap(index);
        return new PlanRef(asInt(selected.get("plan")), asInt(selected.get("node")));
      }
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

  private record GuestEntry(PackageRuntime runtime, String module, Object body,
      List<SignatureStage> stages, int index, Map<String, Object> env, PlanRef plan)
      implements Shapes.Fn1<Object, Object>, java.util.function.Supplier<Object> {
    public Object apply(Object domain) {
      return runtime.eval(this, domain);
    }

    public Object get() {
      return apply(null);
    }
  }

  private record ControlFrame(PackageRuntime runtime, String module, Map<String, Object> graph,
      int block, String result, Map<String, Object> saved) {}

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
        return makeFn(null, asMap(proj.get("cps_entry")), Collections.singletonMap("payload", payload));
      });
      return result;
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
      return layer(module, def.get("body"), signatureRuntimeGroups(asMap(def.get("sig"))), 0,
          env, compiled.plan(def.get("value_plan")));
    }

    private static Map<String, Object> captureEnv(Map<String, Object> env, SignatureStage stage) {
      if (stage.captures() == null) throw new IllegalStateException("a guest stage has prepared captures");
      Map<String, Object> captures = new LinkedHashMap<>();
      for (String name : stage.captures()) {
        if (!env.containsKey(name)) throw new IllegalStateException("a prepared capture is available: " + name);
        captures.put(name, env.get(name));
      }
      return captures;
    }

    private Object layer(String module, Object body, List<SignatureStage> stages,
        int index, Map<String, Object> env, PlanRef plan) {
      GuestEntry entry = new GuestEntry(this, module, body, stages, index,
          captureEnv(env, stages.get(index)), plan);
      return stages.get(index).type() ? compiled.stage(plan).create(entry) : entry;
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

    private static Object slot(Map<String, Object> slots, Object key) {
      String name = asString(key);
      if (!slots.containsKey(name)) throw new IllegalStateException("a prepared control slot is available: " + name);
      return slots.get(name);
    }

    private static Object operand(Object expression, Map<String, Object> slots) {
      Tag tagged = tag(expression);
      if (!tagged.tag().equals("ControlSlot")) {
        throw new IllegalStateException("an atomic control operand is a prepared slot");
      }
      return slot(slots, asMap(tagged.data()).get("slot"));
    }

    private static Map<String, Object> block(Map<String, Object> graph, int index) {
      return asMap(asList(graph.get("blocks")).get(index));
    }

    private static void supply(Map<String, Object> graph, int target,
        Map<String, Object> slots, Object value) {
      Object input = block(graph, target).get("input");
      if (input == null) throw new IllegalStateException("a control edge targets a result input");
      slots.put(asString(input), value);
    }

    private static Map<String, Object> savedSlots(Map<String, Object> slots, List<Object> live) {
      Map<String, Object> saved = new LinkedHashMap<>();
      for (Object key : live) saved.put(asString(key), slot(slots, key));
      return saved;
    }

    private Object atom(String module, Map<String, Object> operation, Map<String, Object> slots) {
      String kind = asString(operation.get("atom"));
      Tag tagged = tag(operation.get("node"));
      Map<String, Object> data = asMap(tagged.data());
      if (kind.equals("named_callable")) {
        return fn(asString(data.get("module_path")), asString(data.get("mangled")));
      }
      if (kind.equals("cps_payload")) {
        return compiled.project(data.get("nominal"), operand(data.get("receiver"), slots));
      }
      switch (tagged.tag()) {
      case "Unit": return new Shapes.Unit();
      case "FnExpr": {
        Map<String, Object> captures = new LinkedHashMap<>();
        for (Object raw : asList(operation.get("bindings"))) {
          Map<String, Object> binding = asMap(raw);
          captures.put(asString(binding.get("name")), slot(slots, binding.get("slot")));
        }
        return makeFn(module, data, captures);
      }
      case "StrLit": return role(data.get("host_type"), data.get("value"), "__fromRole");
      case "IntLit": return role(data.get("host_type"),
          new BigInteger(asString(data.get("digits")).replace("_", "")), "__fromRole");
      case "FloatLit": return role(data.get("host_type"),
          Double.parseDouble(asString(data.get("digits")).replace("_", "")), "__fromRole");
      case "BoolLit": return role(data.get("host_type"), data.get("value"), "__fromRole");
      case "EnrichedTuple": {
        List<Object> values = new ArrayList<>();
        for (Object item : asList(data.get("items"))) values.add(operand(item, slots));
        return productFromSlots(values);
      }
      case "EnrichedProject":
        return productSlot(operand(data.get("target"), slots), asInt(data.get("index")), asInt(data.get("arity")));
      case "EnrichedFieldGet":
        return compiled.project(data.get("nominal"), productSlot(operand(data.get("target"), slots),
            asInt(data.get("index")), asInt(data.get("arity"))));
      case "EnrichedInject":
        return sumInject(operand(data.get("payload"), slots), asInt(data.get("variant")), asInt(data.get("variants")));
      case "LowHostFnValueRef":
        return hostFnValue(asString(data.get("name")), asString(data.get("module_path")),
            asMap(data.get("sig")), data.get("value_plan"));
      case "LowModuleFnValueRef":
        return fn(asString(data.get("module_path")), asString(data.get("mangled")));
      case "LowHostCall": {
        Object callee = hostFnValue(asString(data.get("name")), asString(data.get("module_path")),
            asMap(data.get("sig")), data.get("callee_plan"));
        int count = listOrEmpty(data.get("type_args")).size();
        if (count != 0) callee = advance(callee, compiled.plan(data.get("callee_plan")), count).value();
        if (count != 0 && !signatureHasValueGroup(asMap(data.get("sig")))) return callee;
        List<Object> values = new ArrayList<>();
        for (Object argument : asList(data.get("args"))) values.add(operand(argument, slots));
        return applyValue(callee, productFromSlots(values));
      }
      case "LowNewtypeCtor":
        return compiled.construct(data.get("nominal"), operand(data.get("payload"), slots));
      case "LowNewtypeProj":
        return compiled.project(data.get("nominal"), operand(data.get("target"), slots));
      case "LowQualifiedNewtypeMember": {
        Object payload = operand(data.get("payload"), slots);
        return asBool(data.get("constructor")) ? compiled.construct(data.get("nominal"), payload)
            : compiled.project(data.get("nominal"), payload);
      }
      case "LowAbsurdCall":
        operand(data.get("value_arg"), slots);
        throw new IllegalStateException("Kio: __absurd__ on bottom value");
      default: throw new IllegalStateException("a prepared atomic operation has no guest control: " + tagged.tag());
      }
    }

    private Object eval(GuestEntry entry, Object domain) {
      ArrayList<ControlFrame> frames = new ArrayList<>();
      PackageRuntime runtime = this;
      String module = null;
      Map<String, Object> graph = null;
      Map<String, Object> slots = new LinkedHashMap<>();
      int current = 0;
      Object result = null;
      boolean returning = false;
      try {
        while (true) {
          if (returning) {
            if (frames.isEmpty()) return result;
            int last = frames.size() - 1;
            ControlFrame frame = frames.set(last, null);
            frames.remove(last);
            runtime = frame.runtime();
            module = frame.module();
            graph = frame.graph();
            current = frame.block();
            slots = frame.saved();
            slots.put(frame.result(), result);
            result = null;
            returning = false;
          }
          if (entry != null) {
            runtime = entry.runtime();
            SignatureStage stage = entry.stages().get(entry.index());
            Map<String, Object> local = new LinkedHashMap<>(entry.env());
            if (!stage.type()) {
              for (int i = 0; i < stage.names().size(); i++) {
                if (i + 1 == stage.names().size()) {
                  local.put(stage.names().get(i), domain);
                } else {
                  Shapes.Product<?, ?> pair = (Shapes.Product<?, ?>) domain;
                  local.put(stage.names().get(i), pair.left());
                  domain = pair.right();
                }
              }
            }
            if (entry.index() + 1 < entry.stages().size()) {
              result = runtime.layer(entry.module(), entry.body(), entry.stages(), entry.index() + 1,
                  local, runtime.compiled.child(entry.plan(), "result"));
              entry = null;
              domain = null;
              returning = true;
              continue;
            }
            module = entry.module();
            Tag control = tag(entry.body());
            if (!control.tag().equals("Control")) throw new IllegalStateException("a guest body has a prepared control graph");
            graph = asMap(asMap(control.data()).get("graph"));
            slots.clear();
            for (Object raw : asList(graph.get("inputs"))) {
              Map<String, Object> input = asMap(raw);
              slots.put(asString(input.get("slot")), slot(local, input.get("name")));
            }
            current = asInt(graph.get("start"));
            local.clear();
            entry = null;
            domain = null;
          }
          Map<String, Object> block = block(graph, current);
          for (Object raw : asList(block.get("operations"))) {
            Map<String, Object> operation = asMap(raw);
            Object value = switch (asString(operation.get("kind"))) {
              case "bind" -> slot(slots, operation.get("input"));
              case "atom" -> runtime.atom(module, operation, slots);
              default -> throw new IllegalStateException("unknown prepared control operation");
            };
            slots.put(asString(operation.get("output")), value);
          }
          Map<String, Object> exit = asMap(block.get("exit"));
          switch (asString(exit.get("kind"))) {
          case "return":
            result = slot(slots, exit.get("slot"));
            slots.clear();
            returning = true;
            continue;
          case "jump": {
            Object value = slot(slots, exit.get("argument"));
            current = asInt(exit.get("block"));
            supply(graph, current, slots, value);
            continue;
          }
          case "branch": {
            Map<String, Object> node = asMap(tag(exit.get("node")).data());
            boolean condition = asBool(runtime.role(node.get("host_type"), slot(slots, exit.get("condition")), "__toRole"));
            current = asInt(exit.get(condition ? "then_block" : "else_block"));
            continue;
          }
          case "match": {
            List<Object> arms = asList(exit.get("arms"));
            SumPayload selected = sumPayload(slot(slots, exit.get("scrutinee")), arms.size());
            current = asInt(arms.get(selected.index()));
            supply(graph, current, slots, selected.payload());
            continue;
          }
          case "invoke": {
            Object callee = slot(slots, exit.get("callee"));
            boolean typeStage = asString(exit.get("stage")).equals("type");
            List<Object> arguments = new ArrayList<>();
            for (Object argument : asList(exit.get("arguments"))) arguments.add(slot(slots, argument));
            domain = typeStage ? null : productFromSlots(arguments);
            Object resumeIndex = exit.get("resume");
            if (resumeIndex != null) {
              Map<String, Object> resume = asMap(asList(graph.get("resumes")).get(asInt(resumeIndex)));
              int target = asInt(resume.get("block"));
              String input = asString(resume.get("result"));
              if (!input.equals(block(graph, target).get("input"))) {
                throw new IllegalStateException("a resume supplies its exact block input");
              }
              frames.add(new ControlFrame(runtime, module, graph, target, input,
                  savedSlots(slots, asList(resume.get("live")))));
            }
            Object candidate = typeStage && callee instanceof GuestStage stage ? stage.body() : callee;
            slots.clear();
            if (candidate instanceof GuestEntry guest) {
              if (guest.stages().get(guest.index()).type() != typeStage) {
                throw new IllegalStateException("checked call and guest stage disagree");
              }
              entry = guest;
              continue;
            }
            if (typeStage) {
              PlanRef plan = runtime.compiled.plan(exit.get("type_plan"));
              for (int i = 0; i < asInt(exit.get("type_index")); i++) plan = runtime.compiled.child(plan, "result");
              result = runtime.compiled.stage(plan).advance(callee);
            } else {
              result = applyValue(callee, domain);
            }
            domain = null;
            returning = true;
            continue;
          }
          default: throw new IllegalStateException("unknown prepared control exit");
          }
        }
      } finally {
        frames.clear();
        slots.clear();
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
