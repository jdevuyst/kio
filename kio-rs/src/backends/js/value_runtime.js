function __kioRunAdapted(leaf, layers, argument) {
  const pending = [];
  let value = argument;
  for (let layer = layers; layer !== null; layer = layer.inner) {
    value = layer.before(value);
    pending.push(layer.after);
  }
  value = leaf(value);
  while (pending.length !== 0) value = pending.pop()(value);
  return value;
}

function __kioCallable(leaf, layers) {
  const callable = value => __kioRunAdapted(leaf, layers, value);
  return Object.freeze(Object.assign(callable, {
    adapt(before, after) {
      return __kioCallable(leaf, { before, after, inner: layers });
    }
  }));
}

function __kioLift(leaf) {
  return __kioCallable(leaf, null);
}

function __kioPoly(leaf, layers) {
  return Object.freeze({
    advance() {
      return __kioRunAdapted(leaf, layers, null);
    },
    adapt(after) {
      return __kioPoly(leaf, { before: value => value, after, inner: layers });
    }
  });
}

function __kioLiftPoly(leaf) {
  return __kioPoly(() => leaf.advance(), null);
}

function __kioValueRef(plan, id, context, bindings = null) {
  return { plan, id, context, bindings };
}

function __kioBoundaryValueRef(sites, key, context) {
  const site = sites.get(key);
  if (site === undefined) throw new Error("missing prepared callable boundary");
  const plan = site.callable.values;
  return __kioValueRef(plan, plan.root, context);
}

function __kioValueContext(inventory, boundary, views, unwrap, wrap) {
  const nominals = new Map();
  for (const item of inventory) {
    const name = item.name;
    if (item.kind === "host_type") {
      nominals.set(name.frame, { kind: "host_type" });
    } else {
      nominals.set(name.frame, {
        kind: "newtype", arity: item.arity, payload: null,
        check: value => { unwrap(name.frame, value); },
        wrap: value => wrap(name.frame, value),
        unwrap: value => unwrap(name.frame, value)
      });
    }
  }
  for (const site of boundary.sites) {
    for (const item of site.nominals) {
      const declaration = item.declaration;
      const current = nominals.get(item.name.frame);
      if (current === undefined) throw new Error("missing exact body nominal declaration");
      if (declaration.kind !== "newtype") continue;
      const view = views.get(JSON.stringify([item.name.module, item.name.name]));
      if (view === undefined) throw new Error("missing prepared nominal view");
      if (declaration.transparent_payload === null || view.sealed) continue;
      const payload = declaration.transparent_payload;
      const key = view.key;
      current.key = key;
      current.payload = {
        plan: payload.values, root: payload.values.source_uses[payload.payload_root],
        binders: payload.declaration_binders
      };
      current.wrap = value => ({ [key]: value });
      current.unwrap = value => value[key];
    }
  }
  return { nominals };
}

function __kioChildRef(reference, id) {
  return __kioValueRef(reference.plan, id, reference.context, reference.bindings);
}

function __kioApplication(reference) {
  const argumentChunks = [];
  let current = reference;
  for (;;) {
    const application = current.plan.uses[current.id].application;
    if (application === undefined) throw new Error("invalid prepared type constructor");
    argumentChunks.push(application.args.map(id => __kioChildRef(current, id)));
    if (application.head.kind === "nominal") {
      const args = [];
      while (argumentChunks.length !== 0) {
        for (const arg of argumentChunks.pop()) args.push(arg);
      }
      return { name: application.head.name, args };
    }
    const bound = current.bindings === null ? undefined : current.bindings.get(application.head.binder);
    if (bound === undefined) return null;
    current = bound;
  }
}

function __kioNominalPayload(current, declaration, args) {
  if (args.length !== declaration.arity) throw new Error("invalid prepared nominal application");
  const bindings = new Map();
  for (let index = 0; index < args.length; index++) {
    bindings.set(declaration.payload.binders[index], args[index]);
  }
  return __kioValueRef(declaration.payload.plan, declaration.payload.root, current.context, bindings);
}

function __kioNeedsProtection(reference, incoming) {
  const tasks = [{ reference, incoming }];
  const active = new Set();
  while (tasks.length !== 0) {
    const task = tasks.pop();
    if (task.leave !== undefined) {
      active.delete(task.leave);
      continue;
    }
    const current = task.reference;
    const node = current.plan.protection[current.id];
    const child = (id, direction = task.incoming) => ({ reference: __kioChildRef(current, id), incoming: direction });
    switch (node.kind) {
      case "identity":
        break;
      case "bound": {
        const bound = current.bindings === null ? undefined : current.bindings.get(node.binder);
        if (bound !== undefined) tasks.push({ reference: bound, incoming: task.incoming });
        break;
      }
      case "product":
      case "sum":
        if (node.snapshot) return true;
        tasks.push(child(node.right), child(node.left));
        break;
      case "function":
        tasks.push(child(node.result), child(node.domain, !task.incoming));
        break;
      case "forall":
        tasks.push(child(node.result));
        break;
      case "application": {
        const application = __kioApplication(current);
        if (application === null) break;
        const declaration = current.context.nominals.get(application.name.frame);
        if (declaration === undefined) throw new Error("missing prepared nominal declaration");
        if (declaration.kind === "host_type") break;
        if (declaration.payload === null) {
          if (task.incoming) return true;
          break;
        }
        if (node.snapshot) return true;
        const key = application.name.frame + (task.incoming ? ":in" : ":out");
        if (active.has(key)) return true;
        active.add(key);
        tasks.push({ leave: key }, { reference: __kioNominalPayload(current, declaration, application.args), incoming: task.incoming });
        break;
      }
      default:
        throw new Error("invalid prepared protection operation");
    }
  }
  return false;
}

function __kioProtect(reference, input, incoming) {
  if (!__kioNeedsProtection(reference, incoming)) return input;
  const result = [null];
  const tasks = [{ reference, value: input, target: result, key: 0 }];
  while (tasks.length !== 0) {
    const task = tasks.pop();
    if (task.read !== undefined) {
      task.value = task.read[task.readKey];
    }
    const current = task.reference;
    const node = current.plan.protection[current.id];
    const value = task.value;
    const assign = replacement => { task.target[task.key] = replacement; };
    const read = (id, source, key, target, targetKey) => ({
      reference: __kioChildRef(current, id), read: source, readKey: key,
      target, key: targetKey
    });
    switch (node.kind) {
      case "identity":
        assign(value);
        break;
      case "bound": {
        const bound = current.bindings === null ? undefined : current.bindings.get(node.binder);
        if (bound === undefined) assign(value);
        else tasks.push({ reference: bound, value, target: task.target, key: task.key });
        break;
      }
      case "product": {
        const copy = {};
        assign(copy);
        tasks.push(read(node.right, value, "_1", copy, "_1"));
        tasks.push(read(node.left, value, "_0", copy, "_0"));
        break;
      }
      case "sum": {
        const key = "_0" in value ? "_0" : "_1";
        const copy = {};
        assign(copy);
        tasks.push(read(key === "_0" ? node.left : node.right, value, key, copy, key));
        break;
      }
      case "function": {
        const domain = __kioChildRef(current, node.domain);
        const returned = __kioChildRef(current, node.result);
        assign(value.adapt(
          argument => __kioProtect(domain, argument, !incoming),
          output => __kioProtect(returned, output, incoming)
        ));
        break;
      }
      case "forall": {
        const returned = __kioChildRef(current, node.result);
        assign(value.adapt(output => __kioProtect(returned, output, incoming)));
        break;
      }
      case "application": {
        const application = __kioApplication(current);
        if (application === null) {
          assign(value);
          break;
        }
        const declaration = current.context.nominals.get(application.name.frame);
        if (declaration === undefined) throw new Error("missing prepared nominal declaration");
        if (declaration.kind === "host_type") {
          assign(value);
        } else if (declaration.payload === null) {
          if (incoming) declaration.check(value);
          assign(value);
        } else {
          const copy = {};
          assign(copy);
          tasks.push({
            reference: __kioNominalPayload(current, declaration, application.args),
            read: value, readKey: declaration.key, target: copy, key: declaration.key
          });
        }
        break;
      }
      default:
        throw new Error("invalid prepared value operation");
    }
  }
  return result[0];
}
