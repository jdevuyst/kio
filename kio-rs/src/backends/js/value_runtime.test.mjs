import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const source = readFileSync(new URL('./value_runtime.js', import.meta.url), 'utf8');
assert.doesNotMatch(source, /__kio(?:Protect|NeedsProtection|ValueRef|BoundaryValueRef|RunAdapted|Callable)|adapt\(/,
  'ordinary host values have neither an admission walker nor adapter-layer dispatch');
const realm = vm.createContext({});
const loadRuntime = (context = realm) => vm.runInContext('(() => {\n' + source + '\nreturn { context: __kioNominalContext, poly: __kioPoly, guest: __kioGuest, guestPoly: __kioGuestPoly, lift: __kioLeaf, liftStage: __kioLiftStage, call: __kioTailCall, advance: __kioTailAdvance }; })()', context);
const runtime = loadRuntime();

{
  const owners = [];
  const visits = [0, 0];
  const cursors = new Set();
  const leaf = runtime.lift(value => { owners.push('leaf'); return value + 1; });
  let chain = leaf;
  for (let index = 0; index < 50000; index++) {
    const next = chain;
    const owner = index % 2;
    chain = runtime.guest((value, cursor) => {
      visits[owner]++;
      cursors.add(cursor);
      return runtime.call(cursor, next, value);
    });
  }
  assert.equal(typeof chain, 'function');
  assert.equal(chain(17), 18);
  assert.equal(cursors.size, 1, 'every transfer reuses its invocation cursor');
  assert.equal(chain(23), 24, 'retained entries are reusable across invocations');
  assert.equal(cursors.size, 2, 'separate calls own separate cursors');
  assert.deepEqual(owners, ['leaf', 'leaf']);
  assert.deepEqual(visits, [50000, 50000], 'each captured owner survives both invocations');
  assert.deepEqual(Object.getOwnPropertySymbols(chain), []);
}

{
  const events = [];
  const target = new Proxy(value => value + 1, {
    get() { throw new Error('unexpected callable property read'); },
    getOwnPropertyDescriptor() { throw new Error('unexpected callable descriptor read'); },
    apply(target, receiver, args) {
      events.push(['value', receiver, ...args]);
      return Reflect.apply(target, receiver, args);
    },
  });
  const lifted = runtime.lift(target);
  const call = runtime.guest((value, cursor) => runtime.call(cursor, lifted, value));
  assert.equal(call(6), 7);
  assert.deepEqual(events.splice(0), [['value', undefined, 6]]);
  const poly = {
    advance: new Proxy(function () { return this; }, {
      get() { throw new Error('unexpected type callable property read'); },
      apply(target, receiver, args) {
        events.push(['type', args.length]);
        return Reflect.apply(target, receiver, args);
      },
    }),
  };
  const liftedPoly = runtime.liftStage(poly);
  const advance = runtime.guest((_, cursor) => runtime.advance(cursor, liftedPoly));
  assert.equal(advance(null), poly);
  assert.deepEqual(events, [['type', 0]]);
}

{
  const events = [];
  const makeOwner = label => {
    const leaf = runtime.lift(value => { events.push(label); return value; });
    return runtime.guest((value, cursor) => runtime.call(cursor, leaf, value));
  };
  const first = makeOwner('first');
  const second = makeOwner('second');
  const bridge = runtime.guest((value, cursor) => runtime.call(cursor, second, value));
  const host = runtime.lift(callback => {
    events.push('before');
    assert.equal(first(3), 3, 'host reentry owns a separate cursor');
    assert.equal(callback(7), 7);
    assert.equal(callback(8), 8);
    events.push('after');
    return callback;
  });
  const invoke = runtime.guest((callback, cursor) => runtime.call(cursor, host, callback));
  const returned = invoke(bridge);
  assert.equal(returned, bridge, 'a callable result is an ordinary value');
  assert.deepEqual(events.splice(0), ['before', 'first', 'second', 'second', 'after']);
  assert.equal(returned(9), 9);
  assert.deepEqual(events.splice(0), ['second']);
  const ignore = runtime.guest((callback, cursor) => runtime.call(cursor, runtime.lift(() => 31), callback));
  assert.equal(ignore(first), 31);
  assert.deepEqual(events, [], 'a host may ignore a callback');
  const failure = new Error('host failure');
  const throwing = runtime.guest((value, cursor) => runtime.call(cursor, runtime.lift(() => { throw failure; }), value));
  assert.throws(() => throwing(null), error => error === failure);
  assert.equal(first(11), 11, 'a failed invocation leaves later calls independent');
}

{
  const events = [];
  const visits = [0, 0];
  const leaf = {
    value: 41,
    advance() {
      assert.equal(arguments.length, 0);
      events.push('type');
      return runtime.lift(value => this.value + value);
    },
  };
  let chain = runtime.liftStage(leaf);
  for (let index = 0; index < 50000; index++) {
    const next = chain;
    const owner = index % 2;
    chain = runtime.guestPoly((_, cursor) => {
      visits[owner]++;
      return runtime.advance(cursor, next);
    });
  }
  assert.deepEqual(events, [], 'construction never enters a type stage');
  const result = chain.advance();
  assert.deepEqual(events.splice(0), ['type']);
  assert.equal(result(1), 42);
  assert.deepEqual(events, []);
  assert.equal(chain.advance()(2), 43);
  assert.deepEqual(events, ['type']);
  assert.deepEqual(visits, [50000, 50000]);
}

{
  const other = loadRuntime();
  const events = [];
  let value = runtime.lift(input => { events.push('value'); return input + 1; });
  let poly = runtime.liftStage({ advance() { events.push('type'); return 37; } });
  for (let index = 0; index < 50000; index++) {
    const owner = index % 2 === 0 ? runtime : other;
    const previousValue = value;
    const previousPoly = poly;
    value = owner.guest((input, cursor) => owner.call(cursor, previousValue, input));
    poly = owner.guestPoly((_, cursor) => owner.advance(cursor, previousPoly));
  }
  assert.equal(value(41), 42, 'independent artifacts retain ordinary callback invocation');
  assert.equal(poly.advance(), 37);
  assert.equal(value(43), 44);
  assert.equal(poly.advance(), 37);
  assert.deepEqual(events, ['value', 'type', 'value', 'type']);
}

{
  const events = [];
  const failure = new Error('type-stage failure');
  const poly = runtime.poly(() => { events.push('advance'); return runtime.lift(value => value); });
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
  const returnsControl = runtime.guest((_value, transfer) => transfer);
  const control = returnsControl(null);
  assert.equal(typeof control.call, 'function', 'a returned control object is ordinary data');
  assert.equal(typeof control.advance, 'function');
  const ordinary = runtime.lift(value => value);
  assert.equal(ordinary(control), control, 'native results are never control sentinels');
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

console.log('JavaScript value runtime: tail transfers, type stages and nominal storage passed');
