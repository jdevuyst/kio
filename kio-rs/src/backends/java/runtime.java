  @FunctionalInterface
  public interface KioFn {
    Object call(Object... args);
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

  public static List<Object> list(Object... values) {
    return new ArrayList<>(Arrays.asList(values));
  }

  public static Map<String, Object> map(Object... keysAndValues) {
    if (keysAndValues.length % 2 != 0) {
      throw new IllegalArgumentException("map requires an even number of key/value arguments");
    }
    Map<String, Object> out = new LinkedHashMap<>();
    for (int i = 0; i < keysAndValues.length; i += 2) {
      out.put((String) keysAndValues[i], keysAndValues[i + 1]);
    }
    return out;
  }

  private record Tag(String tag, Object data) {}

  private record SignatureStage(boolean type, List<String> names) {}

  private interface FnInvoke {
    Object invoke(List<Object> args);
  }

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

  private static String segName(Object segment) {
    if (segment instanceof Map<?, ?> m) {
      return String.valueOf(m.get("name"));
    }
    return String.valueOf(segment);
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

  private static Object getMember(Object value, String key) {
    return getMember(value, key, null);
  }

  private static Object getMember(Object value, String key, Object defaultValue) {
    if (value == null) {
      return defaultValue;
    }
    if (value instanceof KioObject obj) {
      return obj.getOrDefault(key, defaultValue);
    }
    if (value instanceof Map<?, ?> m) {
      return m.containsKey(key) ? m.get(key) : defaultValue;
    }
    try {
      return value.getClass().getField(key).get(value);
    } catch (ReflectiveOperationException ignored) {
    }
    try {
      return value.getClass().getMethod(key).invoke(value);
    } catch (ReflectiveOperationException ignored) {
      return defaultValue;
    }
  }

  private static boolean hasMember(Object value, String key) {
    if (value instanceof KioObject obj) {
      return obj.has(key);
    }
    if (value instanceof Map<?, ?> m) {
      return m.containsKey(key);
    }
    return getMember(value, key, null) != null;
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

  private static List<Object> rightSpineProduct(Object ty) {
    List<Object> out = new ArrayList<>();
    Object cur = ty;
    while (true) {
      Tag tagged = tag(cur);
      if (tagged.tag().equals("Product")) {
        Map<String, Object> data = asMap(tagged.data());
        out.add(data.get("left"));
        cur = data.get("right");
      } else {
        out.add(cur);
        return out;
      }
    }
  }

  private static List<Object> rightSpineSum(Object ty) {
    List<Object> out = new ArrayList<>();
    Object cur = ty;
    while (true) {
      Tag tagged = tag(cur);
      if (tagged.tag().equals("Sum")) {
        Map<String, Object> data = asMap(tagged.data());
        out.add(data.get("left"));
        cur = data.get("right");
      } else {
        out.add(cur);
        return out;
      }
    }
  }

  private static List<Object> rightSpineTake(Object param, int abiArity) {
    if (abiArity == 0) {
      return List.of();
    }
    List<Object> out = new ArrayList<>();
    Object cur = param;
    for (int index = 0; index < abiArity; index++) {
      if (index + 1 == abiArity) {
        out.add(cur);
        return out;
      }
      Tag tagged = tag(cur);
      if (tagged.tag().equals("Product")) {
        Map<String, Object> data = asMap(tagged.data());
        out.add(data.get("left"));
        cur = data.get("right");
      } else {
        out.add(cur);
        return out;
      }
    }
    return out;
  }

  private static List<Object> polymorphicPayloadParamSlots(Object param, int abiArity) {
    if (abiArity == 0) {
      return List.of();
    }
    return rightSpineProduct(param);
  }

  private static Object productFromSlots(List<Object> slots) {
    if (slots.isEmpty()) {
      return null;
    }
    Object cur = slots.get(slots.size() - 1);
    for (int i = slots.size() - 2; i >= 0; i--) {
      cur = list(slots.get(i), cur);
    }
    return cur;
  }

  private static List<Object> productSlots(Object value, int arity) {
    List<Object> slots = new ArrayList<>();
    Object cur = value;
    for (int index = 0; index < arity; index++) {
      if (index + 1 == arity) {
        slots.add(cur);
      } else {
        List<Object> pair = asList(cur);
        slots.add(pair.get(0));
        cur = pair.get(1);
      }
    }
    return slots;
  }

  private static Object productSlot(Object value, int index, int arity) {
    if (index < 0 || index >= arity) {
      throw new IndexOutOfBoundsException("product slot out of bounds");
    }
    return productSlots(value, arity).get(index);
  }

  private static Object sumInject(Object payload, int variant, int variants) {
    if (variants <= 0 || variant < 0 || variant >= variants) {
      throw new IndexOutOfBoundsException("sum variant out of bounds");
    }
    Object cur = payload;
    if (variant + 1 < variants) {
      cur = list(0, cur);
    }
    for (int i = 0; i < variant; i++) {
      cur = list(1, cur);
    }
    return cur;
  }

  private record SumPayload(int index, Object payload) {}

  private static SumPayload sumPayload(Object value, int variants) {
    if (variants <= 0) {
      throw new IndexOutOfBoundsException("sum arity out of bounds");
    }
    Object cur = value;
    for (int index = 0; index < variants; index++) {
      if (index + 1 == variants) {
        return new SumPayload(index, cur);
      }
      List<Object> pair = asList(cur);
      if (asInt(pair.get(0)) == 0) {
        return new SumPayload(index, pair.get(1));
      }
      cur = pair.get(1);
    }
    throw new IndexOutOfBoundsException("sum payload out of bounds");
  }

  private static boolean looksLikeProduct(Object value) {
    return value instanceof List<?> l && l.size() == 2;
  }

  private static final class KioFunction implements KioFn {
    private final int arity;
    private final FnInvoke invoke;
    private final String label;

    private KioFunction(int arity, FnInvoke invoke, String label) {
      this.arity = arity;
      this.invoke = invoke;
      this.label = label;
    }

    @Override
    public Object call(Object... rawArgs) {
      List<Object> args = Arrays.asList(rawArgs);
      if (args.size() == arity) {
        return invoke.invoke(args);
      }
      // Exact application is the norm: recover_to_low gives every value
      // group and every curried layer its own single-layer call node, so a
      // well-typed call arrives with exactly `arity` arguments. The one
      // exception is a positional-arity function value — a host-fn value or
      // a lambda whose value group has several parameters — reached through
      // a product-domain arrow slot, which passes the whole domain as one
      // product value (specs/backends/README.md § Function-type FFI
      // canonicalization). Spread that product across the parameter slots;
      // this is the runtime counterpart of the compiled backends' static
      // function-value slot adaptation.
      if (arity > 1 && args.size() == 1 && looksLikeProduct(args.get(0))) {
        return invoke.invoke(productSlots(args.get(0), arity));
      }
      throw new IllegalArgumentException(label + " expected " + arity + " args, got " + args.size());
    }
  }

  private static Object callValue(Object fn, Object... args) {
    if (fn instanceof KioFn kioFn) {
      return kioFn.call(args);
    }
    throw new IllegalArgumentException("value is not callable: " + fn);
  }

  // Type values erase, but their application boundaries do not: each type
  // binder is one nullary runtime stage, while each value group stays one call.
  private static List<SignatureStage> signatureRuntimeGroups(Map<String, Object> sig) {
    List<Object> params = listOrEmpty(sig.get("params"));
    List<Object> groups = listOrEmpty(sig.get("groups"));
    List<SignatureStage> out = new ArrayList<>();
    int offset = 0;
    boolean hasValueGroup = false;
    if (!groups.isEmpty()) {
      for (Object groupObj : groups) {
        Tag grouped = tag(groupObj);
        Map<String, Object> group = asMap(grouped.data());
        int len = asInt(group.get("len"));
        List<Object> chunk = params.subList(offset, offset + len);
        offset += len;
        if (grouped.tag().equals("Type")) {
          for (int i = 0; i < len; i++) {
            out.add(new SignatureStage(true, List.of()));
          }
        } else if (grouped.tag().equals("Value")) {
          List<String> names = new ArrayList<>();
          for (Object paramObj : chunk) {
            names.add(asString(asMap(tag(paramObj).data()).get("name")));
          }
          out.add(new SignatureStage(false, names));
          hasValueGroup = true;
        }
      }
    } else {
      List<String> names = new ArrayList<>();
      for (Object paramObj : params) {
        Tag param = tag(paramObj);
        if (param.tag().equals("Type")) {
          if (!names.isEmpty()) {
            out.add(new SignatureStage(false, names));
            hasValueGroup = true;
            names = new ArrayList<>();
          }
          out.add(new SignatureStage(true, List.of()));
        } else if (param.tag().equals("Value")) {
          names.add(asString(asMap(param.data()).get("name")));
        }
      }
      if (!names.isEmpty()) {
        out.add(new SignatureStage(false, names));
        hasValueGroup = true;
      }
    }
    if (!hasValueGroup) {
      out.add(new SignatureStage(false, List.of()));
    }
    return out;
  }

  private static Object applyTypeStages(Object fn, List<Object> typeArgs) {
    return applyTypeStages(fn, typeArgs.size());
  }

  private static Object applyTypeStages(Object fn, int count) {
    for (int i = 0; i < count; i++) {
      fn = callValue(fn);
    }
    return fn;
  }

  private static List<Object> signatureValueParamTypes(Map<String, Object> sig) {
    List<Object> out = new ArrayList<>();
    for (Object paramObj : listOrEmpty(sig.get("params"))) {
      Tag param = tag(paramObj);
      if (param.tag().equals("Value")) {
        out.add(asMap(param.data()).get("ty"));
      }
    }
    return out;
  }

  private static boolean isExported(Object vis) {
    return variantName(vis).equals("Public");
  }

  private static String itemName(Object itemObj) {
    Tag item = tag(itemObj);
    if (item.tag().equals("FnDef")
        || item.tag().equals("Newtype")
        || item.tag().equals("TypeAlias")
        || item.tag().equals("HostType")
        || item.tag().equals("HostFn")) {
      return asString(asMap(item.data()).get("name"));
    }
    return null;
  }

  private static String newtypeFfiKey(Map<String, Object> item) {
    return asString(item.get("name"));
  }

  private static List<String> pathSegments(Object ty) {
    Tag tagged = tag(ty);
    if (!tagged.tag().equals("Path")) {
      return null;
    }
    List<String> out = new ArrayList<>();
    for (Object segment : asList(asMap(tagged.data()).get("segments"))) {
      out.add(segName(segment));
    }
    return out;
  }

  private static List<Object> typeArgs(Object ty) {
    Tag tagged = tag(ty);
    if (!tagged.tag().equals("Path")) {
      return List.of();
    }
    return listOrEmpty(asMap(tagged.data()).get("args"));
  }

  private static boolean isComptimeTypeName(String name) {
    return (name.startsWith("__") && name.endsWith("__")) || name.startsWith("Comptime_");
  }

  private static List<String> assignSpineKeys(PackageRuntime runtime, List<Object> slots) {
    Set<String> taken = new LinkedHashSet<>();
    List<String> keys = new ArrayList<>();
    for (int index = 0; index < slots.size(); index++) {
      String[] info = runtime.labelInfo(slots.get(index));
      List<String> candidates = new ArrayList<>();
      if (info != null) {
        candidates.add(info[0]);
        candidates.add(info[1]);
      }
      candidates.add("_" + index);
      for (String candidate : candidates) {
        if (!taken.contains(candidate)) {
          keys.add(candidate);
          taken.add(candidate);
          break;
        }
      }
    }
    return keys;
  }

  private static final class CompiledPackage {
    private final Map<String, Object> ir;
    private final Map<String, Map<String, Object>> modules = new LinkedHashMap<>();
    private final Set<String> bridged = new LinkedHashSet<>();
    private final Map<String, Map<String, Object>> newtypes = new LinkedHashMap<>();

    private CompiledPackage(Map<String, Object> ir) {
      this.ir = ir;
      for (Object entryObj : asList(ir.get("modules"))) {
        Map<String, Object> entry = asMap(entryObj);
        modules.put(asString(entry.get("key")), asMap(entry.get("module")));
      }
      for (Object key : asList(ir.get("bridged"))) {
        bridged.add(asString(key));
      }
      for (Map.Entry<String, Map<String, Object>> moduleEntry : modules.entrySet()) {
        for (Object itemObj : listOrEmpty(moduleEntry.getValue().get("items"))) {
          Tag item = tag(itemObj);
          if (item.tag().equals("Newtype")) {
            Map<String, Object> data = asMap(item.data());
            newtypes.put(ntKey(moduleEntry.getKey(), asString(data.get("name"))), data);
          }
        }
      }
    }

    private KioObject create(Object host) {
      return new PackageRuntime(this, host).exports();
    }
  }

  private static String ntKey(String modulePath, String name) {
    return modulePath + "\u0000" + name;
  }

  private static final class ModuleContext {
    private final Map<String, Map<String, Object>> fns = new LinkedHashMap<>();
  }

  private record ResolvedNewtype(String modulePath, Map<String, Object> data) {}

  private static final class PackageRuntime {
    private final CompiledPackage compiled;
    private final Object host;
    private final Map<String, ModuleContext> contexts = new LinkedHashMap<>();
    private final Map<String, KioFunction> fnCache = new HashMap<>();

    private PackageRuntime(CompiledPackage compiled, Object host) {
      this.compiled = compiled;
      this.host = host == null ? new KioObject() : host;
      for (Map.Entry<String, Map<String, Object>> entry : compiled.modules.entrySet()) {
        contexts.put(entry.getKey(), buildContext(entry.getValue()));
      }
      validateHost();
    }

    private ModuleContext buildContext(Map<String, Object> module) {
      ModuleContext ctx = new ModuleContext();
      for (Object itemObj : listOrEmpty(module.get("items"))) {
        Tag item = tag(itemObj);
        Map<String, Object> data = asMap(item.data());
        if (item.tag().equals("FnDef")) {
          ctx.fns.put(asString(data.get("name")), data);
        }
      }
      return ctx;
    }

    private void validateHost() {
      List<String> missing = new ArrayList<>();
      for (String key : compiled.bridged) {
        Map<String, Object> module = compiled.modules.get(key);
        if (module == null) {
          continue;
        }
        for (Object itemObj : listOrEmpty(module.get("items"))) {
          Tag item = tag(itemObj);
          Map<String, Object> data = asMap(item.data());
          if (item.tag().equals("HostFn") && hostFn(asString(data.get("name")), key, false) == null) {
            missing.add(moduleNs(key) + "." + data.get("name"));
          }
        }
      }
      if (!missing.isEmpty()) {
        throw new IllegalStateException("missing host item: " + String.join(", ", missing));
      }
    }

    private Object hostFn(String name, String modulePath, boolean required) {
      String ns = moduleNs(modulePath);
      Object nested = getMember(host, ns);
      Object fn = nested == null ? null : getMember(nested, name);
      if (fn == null) {
        fn = getMember(host, name);
      }
      if (fn == null && required) {
        throw new IllegalStateException("missing host item: " + ns + "." + name);
      }
      return fn;
    }

    private KioObject exports() {
      KioObject root = new KioObject();
      List<String> keys = new ArrayList<>(compiled.bridged);
      Collections.sort(keys);
      for (String key : keys) {
        Map<String, Object> module = compiled.modules.get(key);
        if (module == null) {
          continue;
        }
        List<String> prefix = key.isEmpty()
            ? new ArrayList<>()
            : moduleFacadePath(key);
        for (Object itemObj : listOrEmpty(module.get("items"))) {
          Tag item = tag(itemObj);
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

    private KioObject exportNewtype(Map<String, Object> data) {
      KioObject ns = new KioObject();
      Map<String, Object> constructor = asMap(data.get("constructor"));
      Map<String, Object> projector = asMap(data.get("projector"));
      Object payload = data.get("payload");
      ns.set(asString(constructor.get("name")), (KioFn) args ->
          convertNewtypePayload(payload, args.length == 0 ? null : args[0], "in"));
      if (asBool(data.get("has_existentials"))) {
        ns.set(asString(projector.get("name")), (KioFn) args -> {
          Object value = args.length == 0 ? null : args[0];
          return (KioFn) continuationArgs -> {
            Object continuation = continuationArgs.length == 0 ? null : continuationArgs[0];
            if (variantName(payload).equals("Unit")) {
              return callValue(continuation);
            }
            return callValue(continuation, convertNewtypePayload(payload, value, "out"));
          };
        });
      } else {
        ns.set(asString(projector.get("name")), (KioFn) args ->
            convertNewtypePayload(payload, args.length == 0 ? null : args[0], "out"));
      }
      return ns;
    }

    private KioFunction exportFn(String moduleKey, Map<String, Object> fnDef) {
      List<Object> valueTys = signatureValueParamTypes(asMap(fnDef.get("sig")));
      List<SignatureStage> groups = signatureRuntimeGroups(asMap(fnDef.get("sig")));
      return new KioFunction(valueTys.size(), args -> {
        if (args.size() != valueTys.size()) {
          throw new IllegalArgumentException(
              fnDef.get("name") + " expected " + valueTys.size() + " args, got " + args.size());
        }
        List<Object> converted = new ArrayList<>();
        for (int i = 0; i < valueTys.size(); i++) {
          Object ty = valueTys.get(i);
          converted.add(ty == null ? args.get(i) : convert(ty, args.get(i), "in"));
        }
        // The host ABI is value-only and flat. Internally, each erased
        // type binder is a nullary stage and each value group is one
        // ordinary call layer.
        Object result = fn(moduleKey, asString(fnDef.get("name")));
        int offset = 0;
        for (SignatureStage group : groups) {
          if (group.type()) {
            result = callValue(result);
          } else {
            result = callValue(
                result,
                converted.subList(offset, offset + group.names().size()).toArray());
            offset += group.names().size();
          }
        }
        return convert(fnDef.get("ret"), result, "out");
      }, "export." + moduleKey + "." + fnDef.get("name"));
    }

    private KioFunction fn(String moduleKey, String name) {
      String cacheKey = moduleKey + "\u0000" + name;
      KioFunction cached = fnCache.get(cacheKey);
      if (cached != null) {
        return cached;
      }
      KioFunction fn = makeFn(moduleKey, contexts.get(moduleKey).fns.get(name), Map.of());
      fnCache.put(cacheKey, fn);
      return fn;
    }

    private KioFunction makeFn(String moduleKey, Map<String, Object> fnDef, Map<String, Object> baseEnv) {
      List<SignatureStage> groups = signatureRuntimeGroups(asMap(fnDef.get("sig")));
      return layer(moduleKey, fnDef, groups, 0, new LinkedHashMap<>(baseEnv));
    }

    private KioFunction layer(
        String moduleKey,
        Map<String, Object> fnDef,
        List<SignatureStage> groups,
        int index,
        Map<String, Object> env) {
      SignatureStage stage = groups.get(index);
      List<String> names = stage.names();
      int arity = stage.type() ? 0 : names.size();
      return new KioFunction(arity, args -> {
        Map<String, Object> local = new LinkedHashMap<>(env);
        if (!stage.type()) {
          for (int i = 0; i < names.size() && i < args.size(); i++) {
            local.put(names.get(i), args.get(i));
          }
        }
        if (index + 1 < groups.size()) {
          return layer(moduleKey, fnDef, groups, index + 1, local);
        }
        return eval(moduleKey, fnDef.get("body"), local);
      }, moduleKey + "." + fnDef.get("name"));
    }

    private Object eval(String moduleKey, Object expr, Map<String, Object> env) {
      Tag tagged = tag(expr);
      String tag = tagged.tag();
      Object rawData = tagged.data();
      Map<String, Object> data = rawData instanceof Map<?, ?> ? asMap(rawData) : Map.of();
      switch (tag) {
        case "Unit":
          return null;
        case "Let": {
          Object value = eval(moduleKey, data.get("value"), env);
          Map<String, Object> local = new LinkedHashMap<>(env);
          local.put(asString(data.get("name")), value);
          return eval(moduleKey, data.get("body"), local);
        }
        case "Seq":
          eval(moduleKey, data.get("value"), env);
          return eval(moduleKey, data.get("body"), env);
        case "FnExpr":
          return makeLambda(moduleKey, data, env);
        case "StrLit":
          return data.get("value");
        case "IntLit":
          return new BigInteger(asString(data.get("digits")).replace("_", ""));
        case "FloatLit":
          return Double.parseDouble(asString(data.get("digits")).replace("_", ""));
        case "BoolLit":
          return data.get("value");
        case "EnrichedTuple": {
          List<Object> values = new ArrayList<>();
          for (Object item : asList(data.get("items"))) {
            values.add(eval(moduleKey, item, env));
          }
          return productFromSlots(values);
        }
        case "EnrichedRecord": {
          List<Object> values = new ArrayList<>();
          for (Object fieldObj : asList(data.get("fields"))) {
            values.add(eval(moduleKey, asMap(fieldObj).get("value"), env));
          }
          return productFromSlots(values);
        }
        case "EnrichedProject":
        case "EnrichedFieldGet":
          return productSlot(
              eval(moduleKey, data.get("target"), env),
              asInt(data.get("index")),
              asInt(data.get("arity")));
        case "EnrichedInject":
          return sumInject(
              eval(moduleKey, data.get("payload"), env),
              asInt(data.get("variant")),
              asInt(data.get("variants")));
        case "EnrichedMatch": {
          SumPayload payload = sumPayload(eval(moduleKey, data.get("scrutinee"), env), asList(data.get("arms")).size());
          Map<String, Object> arm = asMap(asList(data.get("arms")).get(payload.index()));
          Map<String, Object> local = new LinkedHashMap<>(env);
          local.put(asString(arm.get("param")), payload.payload());
          return eval(moduleKey, arm.get("body"), local);
        }
        case "EnrichedConditional":
          return eval(
              moduleKey,
              asBool(eval(moduleKey, data.get("cond"), env)) ? data.get("then_branch") : data.get("else_branch"),
              env);
        case "LowBoundRef":
          return env.get(asString(data.get("name")));
        case "LowHostFnValueRef":
          return hostFnValue(
              asString(data.get("name")),
              asString(data.get("module_path")),
              asMap(data.get("sig")),
              data.get("ret_ty"));
        case "LowModuleFnValueRef":
          return fn(asString(data.get("module_path")), asString(data.get("mangled")));
        case "LowHostCall": {
          Object fn = hostFnValue(
              asString(data.get("name")),
              asString(data.get("module_path")),
              asMap(data.get("sig")),
              data.get("ret_ty"));
          fn = applyTypeStages(fn, listOrEmpty(data.get("type_args")));
          List<Object> args = new ArrayList<>();
          for (Object arg : asList(data.get("args"))) {
            args.add(eval(moduleKey, arg, env));
          }
          return callValue(fn, args.toArray());
        }
        case "LowModuleCall": {
          Object fn = fn(asString(data.get("module_path")), asString(data.get("mangled")));
          fn = applyTypeStages(fn, asList(data.get("type_args")));
          List<Object> args = new ArrayList<>();
          for (Object arg : asList(data.get("args"))) {
            args.add(eval(moduleKey, arg, env));
          }
          return callValue(fn, args.toArray());
        }
        case "LowQualifiedModuleCall": {
          String member = asString(data.get("mangled"));
          Object fn = fn(asString(data.get("module_path")), member);
          fn = applyTypeStages(fn, asList(data.get("type_args")));
          List<Object> args = new ArrayList<>();
          for (Object arg : asList(data.get("args"))) {
            args.add(eval(moduleKey, arg, env));
          }
          return callValue(fn, args.toArray());
        }
        case "LowQualifiedNewtypeMember":
        case "LowNewtypeCtor":
        case "LowNewtypeProj": {
          Object payload = data.containsKey("payload") ? data.get("payload") : data.get("target");
          return eval(moduleKey, payload, env);
        }
        case "LowClosureCall": {
          Object callee = applyTypeStages(
              env.get(asString(data.get("name"))), listOrEmpty(data.get("type_args")));
          List<Object> args = new ArrayList<>();
          for (Object arg : asList(data.get("args"))) {
            args.add(eval(moduleKey, arg, env));
          }
          return callValue(callee, args.toArray());
        }
        case "LowIndirectCall": {
          Object callee = eval(moduleKey, data.get("callee"), env);
          callee = applyTypeStages(callee, listOrEmpty(data.get("type_args")));
          List<Object> args = new ArrayList<>();
          for (Object arg : asList(data.get("args"))) {
            args.add(eval(moduleKey, arg, env));
          }
          return callValue(callee, args.toArray());
        }
        case "LowTypeApplication":
          return callValue(eval(moduleKey, data.get("callee"), env));
        case "LowAbsurdCall":
          eval(moduleKey, data.get("value_arg"), env);
          throw new IllegalStateException("Kio: __absurd__ on bottom value");
        case "LowCpsProjectorApply": {
          Object receiver = eval(moduleKey, data.get("receiver"), env);
          Object continuation = eval(moduleKey, data.get("continuation"), env);
          continuation = applyTypeStages(
              continuation, asInt(data.get("continuation_type_stages")));
          int continuationArity = asInt(data.get("continuation_abi_arity"));
          if (continuationArity == 0) {
            return callValue(continuation);
          }
          if (continuationArity == 1) {
            return callValue(continuation, receiver);
          }
          throw new IllegalStateException(
              "CPS projector continuation must have zero or one ABI argument, got "
                  + continuationArity);
        }
        default:
          throw new IllegalStateException("java target cannot evaluate routed expression " + tag);
      }
    }

    private KioFunction makeLambda(String moduleKey, Map<String, Object> data, Map<String, Object> env) {
      Map<String, Object> fnDef = new LinkedHashMap<>();
      fnDef.put("name", "<lambda>");
      fnDef.put("sig", data.get("sig"));
      fnDef.put("body", data.get("body"));
      return makeFn(moduleKey, fnDef, env);
    }

    private KioFunction hostFnValue(String name, String modulePath, Map<String, Object> sig, Object retTy) {
      List<SignatureStage> groups = signatureRuntimeGroups(sig);
      return hostFnLayer(name, modulePath, sig, retTy, groups, 0, List.of());
    }

    private KioFunction hostFnLayer(
        String name,
        String modulePath,
        Map<String, Object> sig,
        Object retTy,
        List<SignatureStage> groups,
        int index,
        List<Object> values) {
      SignatureStage stage = groups.get(index);
      int arity = stage.type() ? 0 : stage.names().size();
      return new KioFunction(arity, args -> {
        List<Object> accumulated = new ArrayList<>(values);
        if (!stage.type()) {
          accumulated.addAll(args);
        }
        if (index + 1 < groups.size()) {
          return hostFnLayer(name, modulePath, sig, retTy, groups, index + 1, accumulated);
        }
        return callHost(name, modulePath, sig, retTy, accumulated);
      }, "host." + modulePath + "." + name);
    }

    private Object callHost(String name, String modulePath, Map<String, Object> sig, Object retTy, List<Object> args) {
      Object fn = hostFn(name, modulePath, true);
      List<Object> valueTys = signatureValueParamTypes(sig);
      List<Object> converted = new ArrayList<>();
      for (int i = 0; i < valueTys.size(); i++) {
        Object ty = valueTys.get(i);
        converted.add(ty == null ? args.get(i) : convert(ty, args.get(i), "out"));
      }
      return convert(retTy, callValue(fn, converted.toArray()), "in");
    }

    private ResolvedNewtype resolveNewtype(Object ty) {
      List<String> segments = pathSegments(ty);
      if (segments == null || segments.isEmpty()) {
        return null;
      }
      if (segments.size() == 1) {
        String name = segments.get(0);
        if (!name.isEmpty() && Character.isLowerCase(name.charAt(0))) {
          return null;
        }
        for (Map.Entry<String, Map<String, Object>> entry : compiled.newtypes.entrySet()) {
          Map<String, Object> data = entry.getValue();
          if (asString(data.get("name")).equals(name)) {
            String key = entry.getKey();
            return new ResolvedNewtype(key.substring(0, key.indexOf('\u0000')), data);
          }
        }
        return null;
      }
      String modulePath = String.join("/", segments.subList(0, segments.size() - 1));
      Map<String, Object> data = compiled.newtypes.get(ntKey(modulePath, segments.get(segments.size() - 1)));
      return data == null ? null : new ResolvedNewtype(modulePath, data);
    }

    private boolean newtypeIsBoundaryAtomic(ResolvedNewtype resolved) {
      if (!newtypeHasPublicHostSurface(resolved)) {
        return false;
      }
      if (asBool(resolved.data().get("has_existentials"))) {
        return true;
      }
      Map<String, Object> constructor = asMap(resolved.data().get("constructor"));
      Map<String, Object> projector = asMap(resolved.data().get("projector"));
      boolean constructorPublic = asString(constructor.get("vis")).equals("Public");
      boolean projectorPublic = asString(projector.get("vis")).equals("Public");
      return !(constructorPublic && projectorPublic);
    }

    private boolean newtypeHasPublicHostSurface(ResolvedNewtype resolved) {
      return compiled.bridged.contains(resolved.modulePath())
          && asString(resolved.data().get("vis")).equals("Public");
    }

    private Object newtypeConversionPayload(ResolvedNewtype resolved, Object ty) {
      // A non-passthrough public host surface owns one Java carrier minted
      // from its declaration-erased payload. Substituting an occurrence's
      // type arguments here would change that carrier's structural keys.
      // Private newtypes have no stable host carrier and remain
      // occurrence-specialized.
      return newtypeHasPublicHostSurface(resolved)
          ? resolved.data().get("payload")
          : instantiateNewtypePayload(resolved.data(), ty);
    }

    private String[] labelInfo(Object ty) {
      ResolvedNewtype resolved = resolveNewtype(ty);
      if (resolved == null) {
        return null;
      }
      String bare = newtypeFfiKey(resolved.data());
      return new String[] { bare, resolved.modulePath() + "." + bare };
    }

    private Object instantiateNewtypePayload(Map<String, Object> data, Object ty) {
      List<Object> args = typeArgs(ty);
      List<Object> params = listOrEmpty(data.get("type_params"));
      if (args.isEmpty() || args.size() != params.size()) {
        return data.get("payload");
      }
      Map<String, Object> subst = new LinkedHashMap<>();
      for (int i = 0; i < params.size(); i++) {
        subst.put(asString(asMap(params.get(i)).get("id")), args.get(i));
      }
      return substType(data.get("payload"), subst);
    }

    private Object applyTypeArgs(Object ty, List<Object> args) {
      if (args.isEmpty()) {
        return ty;
      }
      Tag tagged = tag(ty);
      if (!tagged.tag().equals("TypeVar") && !tagged.tag().equals("Path")) {
        throw new IllegalStateException("cannot apply runtime type arguments to " + tagged.tag());
      }
      Map<String, Object> out = new LinkedHashMap<>(asMap(tagged.data()));
      List<Object> combined = new ArrayList<>(listOrEmpty(out.get("args")));
      combined.addAll(args);
      out.put("args", combined);
      return map(tagged.tag(), out);
    }

    private Object substType(Object ty, Map<String, Object> subst) {
      Tag tagged = tag(ty);
      Map<String, Object> data = tagged.data() instanceof Map<?, ?> ? asMap(tagged.data()) : Map.of();
      if (tagged.tag().equals("TypeVar")) {
        List<Object> args = new ArrayList<>();
        for (Object arg : listOrEmpty(data.get("args"))) {
          args.add(substType(arg, subst));
        }
        String id = asString(data.get("id"));
        if (subst.containsKey(id)) {
          return applyTypeArgs(subst.get(id), args);
        }
        Map<String, Object> out = new LinkedHashMap<>(data);
        out.put("args", args);
        return map("TypeVar", out);
      }
      if (tagged.tag().equals("Path")) {
        List<Object> args = new ArrayList<>();
        for (Object arg : listOrEmpty(data.get("args"))) {
          args.add(substType(arg, subst));
        }
        Map<String, Object> out = new LinkedHashMap<>(data);
        out.put("args", args);
        return map("Path", out);
      }
      if (tagged.tag().equals("Product") || tagged.tag().equals("Sum")) {
        Map<String, Object> out = new LinkedHashMap<>(data);
        out.put("left", substType(data.get("left"), subst));
        out.put("right", substType(data.get("right"), subst));
        return map(tagged.tag(), out);
      }
      if (tagged.tag().equals("Function")) {
        Map<String, Object> out = new LinkedHashMap<>(data);
        out.put("param", substType(data.get("param"), subst));
        out.put("ret", substType(data.get("ret"), subst));
        return map("Function", out);
      }
      if (tagged.tag().equals("Forall")) {
        Map<String, Object> out = new LinkedHashMap<>(data);
        out.put("body", substType(data.get("body"), subst));
        return map("Forall", out);
      }
      return ty;
    }

    private boolean typeInvolvesComptime(Object ty) {
      return typeInvolvesComptime(ty, new LinkedHashSet<>());
    }

    private boolean typeInvolvesComptime(Object ty, Set<String> visited) {
      Tag tagged = tag(ty);
      Map<String, Object> data = tagged.data() instanceof Map<?, ?> ? asMap(tagged.data()) : Map.of();
      if (tagged.tag().equals("Product") || tagged.tag().equals("Sum")) {
        return typeInvolvesComptime(data.get("left"), visited)
            || typeInvolvesComptime(data.get("right"), visited);
      }
      if (tagged.tag().equals("Function")) {
        return typeInvolvesComptime(data.get("param"), visited)
            || typeInvolvesComptime(data.get("ret"), visited);
      }
      if (tagged.tag().equals("Forall")) {
        return typeInvolvesComptime(data.get("body"), visited);
      }
      if (tagged.tag().equals("Bottom")) {
        return true;
      }
      if (tagged.tag().equals("TypeVar")) {
        for (Object arg : listOrEmpty(data.get("args"))) {
          if (typeInvolvesComptime(arg, visited)) {
            return true;
          }
        }
        return false;
      }
      if (tagged.tag().equals("Path")) {
        ResolvedNewtype resolved = resolveNewtype(ty);
        if (resolved != null) {
          if (newtypeIsBoundaryAtomic(resolved)) {
            return false;
          }
          String key = ntKey(resolved.modulePath(), asString(resolved.data().get("name")));
          if (visited.contains(key)) {
            return false;
          }
          visited.add(key);
          boolean result =
              typeInvolvesComptime(instantiateNewtypePayload(resolved.data(), ty), visited);
          visited.remove(key);
          return result;
        }
        List<String> segments = pathSegments(ty);
        if (segments != null && segments.size() == 1 && isComptimeTypeName(segments.get(0))) {
          return true;
        }
        for (Object arg : listOrEmpty(data.get("args"))) {
          if (typeInvolvesComptime(arg, visited)) {
            return true;
          }
        }
        return false;
      }
      return false;
    }

    private boolean newtypeIsRecursive(Map<String, Object> nt, String modulePath) {
      String root = ntKey(modulePath, asString(nt.get("name")));
      Set<String> visited = new LinkedHashSet<>();
      visited.add(root);
      return typeReachesNewtype(nt.get("payload"), root, visited);
    }

    private boolean typeReachesNewtype(Object ty, String root, Set<String> visited) {
      Tag tagged = tag(ty);
      Map<String, Object> data = tagged.data() instanceof Map<?, ?> ? asMap(tagged.data()) : Map.of();
      if (tagged.tag().equals("Product") || tagged.tag().equals("Sum")) {
        return typeReachesNewtype(data.get("left"), root, visited)
            || typeReachesNewtype(data.get("right"), root, visited);
      }
      if (tagged.tag().equals("Function")) {
        return typeReachesNewtype(data.get("param"), root, visited)
            || typeReachesNewtype(data.get("ret"), root, visited);
      }
      if (tagged.tag().equals("Forall")) {
        return typeReachesNewtype(data.get("body"), root, visited);
      }
      if (tagged.tag().equals("TypeVar")) {
        for (Object arg : listOrEmpty(data.get("args"))) {
          if (typeReachesNewtype(arg, root, visited)) {
            return true;
          }
        }
        return false;
      }
      if (tagged.tag().equals("Path")) {
        ResolvedNewtype resolved = resolveNewtype(ty);
        if (resolved == null) {
          for (Object arg : listOrEmpty(data.get("args"))) {
            if (typeReachesNewtype(arg, root, visited)) {
              return true;
            }
          }
          return false;
        }
        String key = ntKey(resolved.modulePath(), asString(resolved.data().get("name")));
        if (key.equals(root)) {
          return true;
        }
        if (newtypeIsBoundaryAtomic(resolved)) {
          return false;
        }
        if (!visited.add(key)) {
          return false;
        }
        // A resolved nominal application reaches through its instantiated
        // payload. Inspecting its arguments independently would make a
        // phantom argument look recursive even when the payload ignores it.
        try {
          return typeReachesNewtype(
              instantiateNewtypePayload(resolved.data(), ty), root, visited);
        } finally {
          visited.remove(key);
        }
      }
      return false;
    }

    private boolean isPassthrough(Object ty) {
      if (typeInvolvesComptime(ty)) {
        return true;
      }
      Tag tagged = tag(ty);
      Map<String, Object> data = tagged.data() instanceof Map<?, ?> ? asMap(tagged.data()) : Map.of();
      if (tagged.tag().equals("Unit") || tagged.tag().equals("Bottom")) {
        return true;
      }
      if (tagged.tag().equals("Function")) {
        for (Object slot : rightSpineTake(data.get("param"), asInt(data.get("abi_arity")))) {
          if (!isPassthrough(slot)) {
            return false;
          }
        }
        return isPassthrough(data.get("ret"));
      }
      if (tagged.tag().equals("TypeVar")) {
        return true;
      }
      if (tagged.tag().equals("Path")) {
        List<String> segments = pathSegments(ty);
        if (segments != null
            && segments.size() == 1
            && !segments.get(0).isEmpty()
            && Character.isLowerCase(segments.get(0).charAt(0))) {
          return true;
        }
        ResolvedNewtype resolved = resolveNewtype(ty);
        if (resolved == null) {
          return true;
        }
        if (newtypeIsBoundaryAtomic(resolved)) {
          return false;
        }
        return newtypeIsRecursive(resolved.data(), resolved.modulePath());
      }
      if (tagged.tag().equals("Product") || tagged.tag().equals("Sum") || tagged.tag().equals("Forall")) {
        return false;
      }
      return true;
    }

    private Object slotValueType(Object slot) {
      ResolvedNewtype resolved = resolveNewtype(slot);
      if (resolved != null) {
        if (newtypeIsBoundaryAtomic(resolved)) {
          return slot;
        }
        if (!isPassthrough(slot)) {
          return newtypeConversionPayload(resolved, slot);
        }
      }
      return slot;
    }

    private Object convert(Object ty, Object value, String direction) {
      if (ty == null || isPassthrough(ty)) {
        return value;
      }
      Tag tagged = tag(ty);
      Map<String, Object> data = tagged.data() instanceof Map<?, ?> ? asMap(tagged.data()) : Map.of();
      if (tagged.tag().equals("Forall")) {
        if (direction.equals("in")) {
          return new KioFunction(
              0,
              args -> convert(data.get("body"), value, direction),
              "ffi-type-stage");
        }
        return convert(data.get("body"), callValue(value), direction);
      }
      if (tagged.tag().equals("Path")) {
        ResolvedNewtype resolved = resolveNewtype(ty);
        if (resolved == null) {
          return value;
        }
        String key = newtypeFfiKey(resolved.data());
        if (newtypeIsBoundaryAtomic(resolved)) {
          return direction.equals("out") ? map(key, value) : getMember(value, key);
        }
        Object payload = newtypeConversionPayload(resolved, ty);
        if (direction.equals("out")) {
          return map(key, convertNewtypePayload(payload, value, "out"));
        }
        return convertNewtypePayload(payload, getMember(value, key), "in");
      }
      if (tagged.tag().equals("Product")) {
        List<Object> slots = rightSpineProduct(ty);
        List<String> keys = assignSpineKeys(this, slots);
        if (direction.equals("out")) {
          List<Object> values = productSlots(value, slots.size());
          Map<String, Object> out = new LinkedHashMap<>();
          for (int i = 0; i < slots.size(); i++) {
            out.put(keys.get(i), convert(slotValueType(slots.get(i)), values.get(i), "out"));
          }
          return out;
        }
        List<Object> converted = new ArrayList<>();
        for (int i = 0; i < slots.size(); i++) {
          converted.add(convert(slotValueType(slots.get(i)), getMember(value, keys.get(i)), "in"));
        }
        return productFromSlots(converted);
      }
      if (tagged.tag().equals("Sum")) {
        List<Object> slots = rightSpineSum(ty);
        List<String> keys = assignSpineKeys(this, slots);
        if (direction.equals("out")) {
          SumPayload payload = sumPayload(value, slots.size());
          return map(keys.get(payload.index()), convert(slotValueType(slots.get(payload.index())), payload.payload(), "out"));
        }
        for (int i = 0; i < slots.size(); i++) {
          if (hasMember(value, keys.get(i))) {
            Object payload = convert(slotValueType(slots.get(i)), getMember(value, keys.get(i)), "in");
            return sumInject(payload, i, slots.size());
          }
        }
        throw new IllegalStateException("sum value has no recognized variant key");
      }
      if (tagged.tag().equals("Function")) {
        return convertFunction(
            data.get("param"),
            data.get("ret"),
            asInt(data.get("abi_arity")),
            value,
            direction);
      }
      return value;
    }

    private Object convertNewtypePayload(Object payload, Object value, String direction) {
      int typeStages = 0;
      Object inner = payload;
      while (variantName(inner).equals("Forall")) {
        typeStages++;
        inner = asMap(tag(inner).data()).get("body");
      }
      Tag tagged = tag(inner);
      if (typeStages > 0 && tagged.tag().equals("Function") && !isPassthrough(payload)) {
        Map<String, Object> data = asMap(tagged.data());
        if (direction.equals("out")) {
          value = applyTypeStages(value, typeStages);
          return convertPolymorphicFunction(
              data.get("param"), data.get("ret"), asInt(data.get("abi_arity")), value, direction);
        }
        Object converted = convertPolymorphicFunction(
            data.get("param"), data.get("ret"), asInt(data.get("abi_arity")), value, direction);
        for (int i = 0; i < typeStages; i++) {
          Object nextStage = converted;
          converted = new KioFunction(0, args -> nextStage, "ffi-type-stage");
        }
        return converted;
      }
      return convert(payload, value, direction);
    }

    private KioFunction convertPolymorphicFunction(
        Object param, Object ret, int abiArity, Object fnValue, String direction) {
      List<Object> boundarySlots = polymorphicPayloadParamSlots(param, abiArity);
      List<Object> internalSlots = rightSpineTake(param, abiArity);
      int outerArity = direction.equals("out") ? boundarySlots.size() : internalSlots.size();
      return new KioFunction(outerArity, args -> {
        List<Object> callArgs = new ArrayList<>();
        if (direction.equals("out")) {
          List<Object> converted = new ArrayList<>();
          for (int i = 0; i < boundarySlots.size(); i++) {
            converted.add(convert(boundarySlots.get(i), args.get(i), "in"));
          }
          if (!internalSlots.isEmpty()) {
            int finalStart = internalSlots.size() - 1;
            callArgs.addAll(converted.subList(0, finalStart));
            callArgs.add(productFromSlots(converted.subList(finalStart, converted.size())));
          }
        } else {
          Object product = productFromSlots(args);
          List<Object> boundaryValues = boundarySlots.size() > 1
              ? productSlots(product, boundarySlots.size())
              : (boundarySlots.isEmpty()
                  ? List.of()
                  // Unit is the null runtime value, but a substituted Unit
                  // remains one real boundary slot.
                  : java.util.Collections.singletonList(product));
          for (int i = 0; i < boundarySlots.size(); i++) {
            callArgs.add(convert(boundarySlots.get(i), boundaryValues.get(i), "out"));
          }
        }
        Object result = callValue(fnValue, callArgs.toArray());
        return convert(ret, result, direction);
      }, "ffi-function");
    }

    private KioFunction convertFunction(Object param, Object ret, int abiArity, Object fnValue, String direction) {
      List<Object> slots = rightSpineTake(param, abiArity);
      String paramDir = direction.equals("out") ? "in" : "out";
      return new KioFunction(slots.size(), args -> {
        List<Object> convertedArgs = new ArrayList<>();
        for (int i = 0; i < slots.size(); i++) {
          convertedArgs.add(convert(slots.get(i), args.get(i), paramDir));
        }
        Object result = callValue(fnValue, convertedArgs.toArray());
        return convert(ret, result, direction);
      }, "ffi-function");
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
