import assert from 'node:assert/strict';
import { loadRuntime, runtime } from './runtime.mjs';

{
  const other = loadRuntime();
  const owners = [];
  const cursors = new Set();
  const leaf = value => { owners.push('leaf'); return value + 1; };
  let chain = leaf;
  for (let index = 0; index < 50000; index++) {
    const next = chain;
    const owner = index % 2 === 0 ? runtime : other;
    chain = owner.guest((value, cursor) => {
      cursors.add(cursor);
      return owner.call(cursor, next, value);
    });
  }
  assert.equal(typeof chain, 'function');
  assert.equal(chain(17), 18);
  assert.equal(cursors.size, 1, 'every transfer reuses its invocation cursor');
  assert.equal(chain(23), 24, 'retained entries are reusable across invocations');
  assert.equal(cursors.size, 2, 'separate calls own separate cursors');
  assert.deepEqual(owners, ['leaf', 'leaf']);
  assert.deepEqual(Object.getOwnPropertySymbols(chain), []);
}
