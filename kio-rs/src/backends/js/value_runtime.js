function __kioPoly(advance) {
  return { advance };
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
