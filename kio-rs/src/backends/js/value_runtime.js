function __kioPoly(advance) {
  return Object.freeze({ advance, kioStep: _transfer => advance() });
}

function __kioRun(target, domain, typeStage) {
  let pending = true;
  const transfer = Object.freeze({
    call(next, value) {
      target = next;
      domain = value;
      typeStage = false;
      pending = true;
    },
    advance(next) {
      target = next;
      domain = undefined;
      typeStage = true;
      pending = true;
    }
  });
  let result;
  try {
    while (pending) {
      pending = false;
      result = typeStage
        ? target.kioStep(transfer)
        : target.kioStep(domain, transfer);
    }
    return result;
  } finally {
    target = undefined;
    domain = undefined;
  }
}

function __kioLeaf(body) {
  const callable = domain => body(domain);
  callable.kioStep = (domain, _transfer) => body(domain);
  return Object.freeze(callable);
}

function __kioGuest(entry) {
  const callable = domain => __kioRun(callable, domain, false);
  callable.kioStep = entry;
  return Object.freeze(callable);
}

function __kioGuestPoly(entry) {
  const poly = {
    advance: () => __kioRun(poly, undefined, true),
    kioStep: transfer => entry(undefined, transfer)
  };
  return Object.freeze(poly);
}

function __kioLiftStage(source) {
  const advance = source.advance;
  return __kioPoly(() => Reflect.apply(advance, source, []));
}

function __kioTailCall(cursor, target, domain) {
  cursor.call(target, domain);
}

function __kioTailAdvance(cursor, value) {
  cursor.advance(value);
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
