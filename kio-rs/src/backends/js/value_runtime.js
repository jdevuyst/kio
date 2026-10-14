function __kioPoly(advance) {
  return Object.freeze({ advance, kioStep: _transfer => advance() });
}

function __kioRun(target, domain, typeStage, initialResume) {
  let pending = true;
  const frames = initialResume === undefined ? [] : [initialResume];
  initialResume = undefined;
  let resume;
  const resumeCapability = Object.freeze({
    call(next, value, continuation) {
      frames.push(continuation);
      target = next;
      domain = value;
      typeStage = false;
      pending = true;
    },
    advance(next, continuation) {
      frames.push(continuation);
      target = next;
      domain = undefined;
      typeStage = true;
      pending = true;
    }
  });
  const transfer = Object.freeze({
    resume: resumeCapability,
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
    for (;;) {
      pending = false;
      if (resume === undefined) {
        result = typeStage
          ? target.kioStep(transfer)
          : target.kioStep(domain, transfer);
      } else {
        const continuation = resume;
        resume = undefined;
        result = continuation(domain, transfer);
      }
      if (pending) continue;
      if (frames.length === 0) return result;
      resume = frames.pop();
      target = undefined;
      domain = result;
      result = undefined;
      typeStage = false;
    }
  } finally {
    frames.length = 0;
    resume = undefined;
    result = undefined;
    target = undefined;
    domain = undefined;
  }
}

function __kioCallWithResume(cursor, target, domain, resume) {
  const capability = cursor.resume;
  if (capability !== undefined) return capability.call(target, domain, resume);
  return __kioRun(target, domain, false, resume);
}

function __kioAdvanceWithResume(cursor, target, resume) {
  const capability = cursor.resume;
  if (capability !== undefined) return capability.advance(target, resume);
  return __kioRun(target, undefined, true, resume);
}

function __kioResume(entry, block, saved) {
  return (result, cursor) => {
    const values = saved;
    saved = undefined;
    if (values === undefined) throw new Error("a pending resume is entered once");
    return entry(block, values, result, cursor);
  };
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
