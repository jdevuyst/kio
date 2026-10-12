import test from 'node:test'
import { assertHistoryFixture, buildHistoryFixture } from './history-site.mjs'

test('production theme builds historical banners and ordinary current routes from inert fixture data', async t => {
  const fixture = await buildHistoryFixture()
  t.after(fixture.cleanup)
  assertHistoryFixture(fixture)
})
