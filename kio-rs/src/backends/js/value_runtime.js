function __kioPoly(advance) {
  return { advance };
}

// A weak identity lookup preserves host Proxy behavior.
const __kioTailEntries = new WeakMap();

function __kioGuest(entry) {
  const callable = domain => {
    const cursor = { target: null, domain: null, receiver: null };
    let result = entry(domain, cursor);
    while (result === cursor) {
      const target = cursor.target;
      const next = __kioTailEntries.get(target);
      if (next === undefined) {
        return cursor.receiver === null
          ? target(cursor.domain)
          : Reflect.apply(target, cursor.receiver, []);
      }
      result = next(cursor.domain, cursor);
    }
    return result;
  };
  __kioTailEntries.set(callable, entry);
  return callable;
}

function __kioTailCall(cursor, target, domain) {
  cursor.target = target;
  cursor.domain = domain;
  cursor.receiver = null;
  return cursor;
}

function __kioTailAdvance(cursor, value) {
  cursor.target = value.advance;
  cursor.domain = undefined;
  cursor.receiver = value;
  return cursor;
}

function __kioNominalContext(inventory, views, unwrap, wrap) {
  const nominals = new Map();
  for (const name of inventory) {
    const view = views.get(JSON.stringify([name.module, name.name]));
    if (view !== undefined && !view.sealed) {
      const key = view.key;
      nominals.set(name.frame, {
        wrap: value => ({ [key]: value }),
        unwrap: value => value[key]
      });
    } else {
      nominals.set(name.frame, {
        wrap: value => wrap(name.frame, value),
        unwrap: value => unwrap(name.frame, value)
      });
    }
  }
  return nominals;
}
