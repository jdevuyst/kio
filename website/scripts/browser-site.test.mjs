import assert from 'node:assert/strict'
import test from 'node:test'
import { browserToolchain, isCancelledPrefetch, withBuiltSite } from './browser-site.mjs'

test('all supported platforms resolve the same managed Firefox tool', () => {
  for (const arch of ['x64', 'arm64']) {
    assert.deepEqual(browserToolchain('linux', arch), { tool: 'http:kio-firefox', executable: 'firefox' })
    assert.deepEqual(browserToolchain('darwin', arch), { tool: 'http:kio-firefox', executable: 'Nightly.app/Contents/MacOS/firefox' })
  }
  assert.deepEqual(browserToolchain('win32', 'x64'), { tool: 'http:kio-firefox', executable: 'firefox.exe' })
  assert.throws(() => browserToolchain('unrecognized', 'x64'), /no managed browser route/)
  assert.throws(() => browserToolchain('win32', 'arm64'), /no managed browser route/)
})

test('only an explicitly marked non-navigation Firefox prefetch cancellation is harmless', () => {
  const request = (overrides = {}) => ({
    failure: () => ({ errorText: 'NS_BINDING_ABORTED' }),
    resourceType: () => 'other', isNavigationRequest: () => false,
    headers: () => ({ 'sec-purpose': 'prefetch' }), ...overrides
  })
  assert.equal(isCancelledPrefetch(request()), true)
  assert.equal(isCancelledPrefetch(request({ failure: () => ({ errorText: 'NS_ERROR_FAILURE' }) })), false)
  assert.equal(isCancelledPrefetch(request({ resourceType: () => 'script' })), false)
  assert.equal(isCancelledPrefetch(request({ isNavigationRequest: () => true })), false)
  assert.equal(isCancelledPrefetch(request({ headers: () => ({}) })), false)
})

test('browser tasks reject a sandbox-disable environment before serving or launching', async () => {
  const previous = process.env.MOZ_DISABLE_CONTENT_SANDBOX
  process.env.MOZ_DISABLE_CONTENT_SANDBOX = '1'
  try {
    await assert.rejects(withBuiltSite({ dist: 'deliberately-absent-dist' }, () => {
      assert.fail('sandbox override must not reach a browser callback')
    }), /refusing sandbox\/policy override|no managed browser route/)
  } finally {
    if (previous === undefined) delete process.env.MOZ_DISABLE_CONTENT_SANDBOX
    else process.env.MOZ_DISABLE_CONTENT_SANDBOX = previous
  }
})

test('an inherited custom Firefox policy is rejected before browser launch', async () => {
  const previous = process.env.PLAYWRIGHT_FIREFOX_POLICIES_JSON
  process.env.PLAYWRIGHT_FIREFOX_POLICIES_JSON = 'custom-policy.json'
  try {
    await assert.rejects(withBuiltSite({ dist: 'deliberately-absent-dist' }, () => {
      assert.fail('custom policy must not reach a browser callback')
    }), /refusing sandbox\/policy override|no managed browser route/)
  } finally {
    if (previous === undefined) delete process.env.PLAYWRIGHT_FIREFOX_POLICIES_JSON
    else process.env.PLAYWRIGHT_FIREFOX_POLICIES_JSON = previous
  }
})
