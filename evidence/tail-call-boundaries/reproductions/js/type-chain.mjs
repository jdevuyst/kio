import assert from 'node:assert/strict';
import { loadRuntime, runtime } from './runtime.mjs';

{
  const other = loadRuntime();
  const events = [];
  const leaf = {
    value: 41,
    advance() {
      assert.equal(arguments.length, 0);
      events.push('type');
      return value => this.value + value;
    },
  };
  let chain = leaf;
  for (let index = 0; index < 50000; index++) {
    const next = chain;
    const owner = index % 2 === 0 ? runtime : other;
    chain = owner.poly(owner.guest((_, cursor) => owner.advance(cursor, next)));
  }
  assert.deepEqual(events, [], 'construction never enters a type stage');
  const result = chain.advance();
  assert.deepEqual(events.splice(0), ['type']);
  assert.equal(result(1), 42);
  assert.deepEqual(events, []);
  assert.equal(chain.advance()(2), 43);
  assert.deepEqual(events, ['type']);
}
