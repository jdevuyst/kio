import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const source = readFileSync(new URL('./value_runtime.js', import.meta.url), 'utf8');
assert.doesNotMatch(source, /__kio(?:Protect|NeedsProtection|ValueRef|BoundaryValueRef|RunAdapted|Callable|Lift)|adapt\(/,
  'ordinary host values have neither an admission walker nor adapter-layer dispatch');
const runtime = vm.runInNewContext(source + '\n({ context: __kioNominalContext, poly: __kioPoly })');

{
  const events = [];
  const failure = new Error('type-stage failure');
  const poly = runtime.poly(() => { events.push('advance'); return value => value; });
  assert.deepEqual(events, []);
  const callable = poly.advance();
  assert.deepEqual(events.splice(0), ['advance']);
  assert.equal(callable(19), 19);
  assert.deepEqual(events, [], 'calling the result does not advance its producer again');
  poly.advance();
  assert.deepEqual(events.splice(0), ['advance']);
  assert.throws(() => runtime.poly(() => { throw failure; }).advance(),
    error => error === failure);
}

{
  const inventory = [
    { module: ['left'], name: 'Token', frame: 'left.Token' },
    { module: ['right'], name: 'Token', frame: 'right.Token' },
    { module: ['visible'], name: 'Box', frame: 'visible.Box' },
  ];
  const stores = new Map();
  const unwrap = (frame, handle) => {
    const store = stores.get(frame);
    if (store === undefined || !store.has(handle)) throw new TypeError('private storage owner');
    return store.get(handle);
  };
  const wrap = (frame, payload) => {
    if (!stores.has(frame)) stores.set(frame, new WeakMap());
    const handle = Object.freeze({});
    stores.get(frame).set(handle, payload);
    return handle;
  };
  const views = new Map([[JSON.stringify([['visible'], 'Box']), { key: 'Box', sealed: false }]]);
  const context = runtime.context(inventory, views, unwrap, wrap);
  const token = context.get('left.Token').wrap(17);
  assert.equal(context.get('left.Token').unwrap(token), 17);
  assert.throws(() => context.get('right.Token').unwrap(token), /private storage owner/);
  assert.throws(() => context.get('left.Token').unwrap({}), /private storage owner/);
  const visible = context.get('visible.Box').wrap(token);
  assert.equal(visible.Box, token);
  assert.equal(context.get('visible.Box').unwrap(visible), token);
}

console.log('JavaScript value runtime: type stages and nominal storage passed');
